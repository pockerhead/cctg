//! Hook events the hub did not take, kept on this device until a later hook
//! or the agent of the same session delivers them (TASK-018, TASK-088).
//!
//! Without it a session whose `SessionStart` ran while the hub was down stays
//! unknown to the hub for good (the hub never adopts a session it did not see
//! start), and a `Stop` whose POST timed out loses the turn's answer.
//!
//! Layout: `<state>/spool/<session id>/<nanos:020>-<event id>.json` for a
//! lifecycle event and `<nanos:020>-<event id>.turn.json` for a turn event,
//! one [`HookPost`] per file, exactly as it was to be sent. It keeps its event
//! id, so the hub drops a copy it already has. A file is written under a
//! `.tmp` (`.turn.tmp`) name, flushed to disk and renamed (as
//! `RegistryStore::save` does), so a reader never sees half of one (the
//! maildir scheme); several replays at once only send an event twice, and the
//! hub keeps one. A `.tmp` left by a writer that died is deleted once it is
//! [`TMP_GRACE`] old. Builds before TASK-088 do not parse turn file names and
//! leave them alone.
//!
//! Kept: lifecycle events (`SessionStart`, `SessionEnd`: ids, pids, the
//! folder and the transcript path) and turn events (`UserPromptSubmit`,
//! `Stop`, `SubagentStart`, `SubagentStop`, `SubagentHandback`,
//! `PreCompact`). Turn events carry answer and report texts: they stay only
//! on this device, in files only this user may read on Unix (`0700`
//! directories, `0600` files), and are deleted once delivered or at most
//! [`TURN_MAX_AGE`] after they were kept (at the latest at the next sweep: a
//! save, a replay of the session, or the agent's minute sweep). Status-only
//! events (`ToolStart`, `ToolEnd`, `StatusLine`) are never kept. The secret
//! is never part of a [`HookPost`]. Bounds: [`MAX_PER_SESSION`] files per
//! session for a lifecycle event, [`MAX_TURN_FILES`] turn files per session
//! (a new one evicts the oldest), [`MAX_FILES`] in all, [`MAX_FILE`] bytes
//! each, [`MAX_AGE`] old. The counts are checked without a lock: hooks that
//! save at the same moment may each add their one file, so with `k` of them
//! the spool holds at most `k - 1` files over a bound, and the next save sees
//! it full.

use std::io::Write;
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use tokio::time::Instant;

use crate::hook::{PostError, post};
use crate::wire::{self, HookEvent, HookPost, Secret};

/// Files one session may keep; a later lifecycle event of a full session is
/// dropped (the oldest, its start, is the one that matters).
pub const MAX_PER_SESSION: usize = 16;
/// Turn files one session may keep; the rest of [`MAX_PER_SESSION`] stays
/// for its start and end. A new turn event evicts the oldest: the newest
/// answer matters most.
pub const MAX_TURN_FILES: usize = MAX_PER_SESSION / 2;
/// Files of all sessions together.
pub const MAX_FILES: usize = 256;
/// Bytes of one file. A `Stop` carries up to `hook::MAX_TEXT` (128 KiB) of
/// answer, up to 6 times that when escaped; the hub refuses larger bodies
/// anyway.
pub const MAX_FILE: usize = wire::MAX_HOOK_BODY;
/// Older files are deleted unsent: that session is long over.
pub const MAX_AGE: Duration = Duration::from_secs(24 * 60 * 60);
/// Older turn files are deleted unsent. Must stay below
/// `hub::ingress::DEDUP_TTL`, so a copy of an event the hub took (only its
/// answer was late) is still in the hub's dedup when it is sent again.
pub const TURN_MAX_AGE: Duration = Duration::from_secs(5 * 60);
/// A `.tmp` this old belongs to a writer that died; a live one renames it
/// within milliseconds.
pub const TMP_GRACE: Duration = Duration::from_secs(60);
const SPOOL_DIR: &str = "spool";
const MAX_SESSION_ID: usize = 128;
/// Name ends of turn files and of turn files being written.
const TURN_SUFFIX: &str = ".turn.json";
const TURN_TMP_SUFFIX: &str = ".turn.tmp";

/// Why an event was not kept. Fixed text, safe to log.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum SpoolError {
    #[error("status events are not kept")]
    NotKept,
    #[error("session id is not usable as a file name")]
    BadSession,
    #[error("the spool is full")]
    Full,
    #[error("event is too large to keep")]
    TooLarge,
    #[error("spool write failed: {0:?}")]
    Io(std::io::ErrorKind),
}

/// `<state>/spool`.
pub fn dir(state_dir: &Path) -> PathBuf {
    state_dir.join(SPOOL_DIR)
}

/// The events worth keeping: the ones the hub needs to know a session, and
/// the events of a turn.
pub fn keeps(event: &HookEvent) -> bool {
    matches!(
        event,
        HookEvent::SessionStart { .. }
            | HookEvent::SessionEnd { .. }
            | HookEvent::UserPromptSubmit { .. }
            | HookEvent::Stop { .. }
            | HookEvent::SubagentStart { .. }
            | HookEvent::SubagentStop { .. }
            | HookEvent::SubagentHandback { .. }
            | HookEvent::PreCompact { .. }
    )
}

/// A kept event of a turn (not a session start or end): kept under a turn
/// file name, for at most [`TURN_MAX_AGE`].
pub fn is_turn(event: &HookEvent) -> bool {
    keeps(event)
        && !matches!(
            event,
            HookEvent::SessionStart { .. } | HookEvent::SessionEnd { .. }
        )
}

fn session_dir(root: &Path, session: &str) -> Option<PathBuf> {
    let usable = !session.is_empty()
        && session.len() <= MAX_SESSION_ID
        && session
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || byte == b'-' || byte == b'_');
    usable.then(|| root.join(session))
}

fn nanos(at: SystemTime) -> u128 {
    at.duration_since(UNIX_EPOCH)
        .map(|elapsed| elapsed.as_nanos())
        .unwrap_or_default()
}

/// The write time of a spool file from its name `<nanos:020>-<32 hex>.json`
/// or `<nanos:020>-<32 hex>.turn.json`, and whether it is a turn file. The
/// turn end is tried first: stripped of `.json` only, the stem ends in
/// `.turn` and is no stamp (which is why older builds ignore turn files).
fn stamp(name: &str) -> Option<(u128, bool)> {
    if let Some(stem) = name.strip_suffix(TURN_SUFFIX) {
        return name_stamp(stem).map(|stamp| (stamp, true));
    }
    name_stamp(name.strip_suffix(".json")?).map(|stamp| (stamp, false))
}

/// The same for a file being written, `<nanos:020>-<32 hex>.tmp` or
/// `.turn.tmp`.
fn tmp_stamp(name: &str) -> Option<u128> {
    match name.strip_suffix(TURN_TMP_SUFFIX) {
        Some(stem) => name_stamp(stem),
        None => name_stamp(name.strip_suffix(".tmp")?),
    }
}

fn name_stamp(stem: &str) -> Option<u128> {
    let (time, rest) = stem.split_once('-')?;
    let valid = time.len() == 20
        && time.bytes().all(|byte| byte.is_ascii_digit())
        && rest.len() == 32
        && rest
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte));
    valid.then(|| time.parse().ok()).flatten()
}

/// `turn`: the file is a turn file ([`TURN_MAX_AGE`], else [`MAX_AGE`]).
fn expired(stamp: u128, turn: bool, now: SystemTime) -> bool {
    let age = if turn { TURN_MAX_AGE } else { MAX_AGE };
    nanos(now).saturating_sub(stamp) > age.as_nanos()
}

/// Spool files of one session directory, oldest first: stamp, whether it is
/// a turn file, path. Files that are not spool files (a `.tmp` being
/// written) are left alone.
fn files(dir: &Path) -> Vec<(u128, bool, PathBuf)> {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return Vec::new();
    };
    let mut files: Vec<(u128, bool, PathBuf)> = entries
        .filter_map(Result::ok)
        .filter_map(|entry| {
            let name = entry.file_name().into_string().ok()?;
            let (stamp, turn) = stamp(&name)?;
            Some((stamp, turn, entry.path()))
        })
        .collect();
    files.sort();
    files
}

/// Deletes the `.tmp` files of `dir` that a dead writer left.
fn drop_stale_tmp(dir: &Path, now: SystemTime) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for entry in entries.filter_map(Result::ok) {
        let stale = entry
            .file_name()
            .to_str()
            .and_then(tmp_stamp)
            .is_some_and(|stamp| nanos(now).saturating_sub(stamp) > TMP_GRACE.as_nanos());
        if stale {
            let _ = std::fs::remove_file(entry.path());
        }
    }
}

/// Deletes expired files and stale `.tmp` files of every session and counts
/// the rest.
fn prune(root: &Path, now: SystemTime) -> usize {
    let Ok(sessions) = std::fs::read_dir(root) else {
        return 0;
    };
    let mut kept = 0;
    for session in sessions.filter_map(Result::ok) {
        let path = session.path();
        if !path.is_dir() {
            continue;
        }
        drop_stale_tmp(&path, now);
        for (stamp, turn, file) in files(&path) {
            if expired(stamp, turn, now) {
                let _ = std::fs::remove_file(file);
            } else {
                kept += 1;
            }
        }
        // Only an empty directory goes.
        let _ = std::fs::remove_dir(&path);
    }
    kept
}

/// Deletes expired files and stale `.tmp` files of every session, by name
/// only (the agent's minute sweep): turn texts of sessions nobody replays
/// any more go after [`TURN_MAX_AGE`].
pub fn sweep(root: &Path, now: SystemTime) {
    let _ = prune(root, now);
}

/// Creates `dir` (`0700` on Unix when this call created it).
fn make_dir(dir: &Path) -> std::io::Result<()> {
    if !dir.is_dir() {
        std::fs::create_dir_all(dir)?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(dir, std::fs::Permissions::from_mode(0o700))?;
        }
    }
    Ok(())
}

/// Makes `root` and the session directory `dir`, then creates `temp` in it
/// with `open`. Another hook's `prune` or the end of a replay removes every
/// empty session directory, also one just made here: when `open` finds it
/// gone, both go once more (the file keeps a later `remove_dir` off).
fn create_temp(
    root: &Path,
    dir: &Path,
    temp: &Path,
    mut open: impl FnMut(&Path) -> std::io::Result<std::fs::File>,
) -> std::io::Result<std::fs::File> {
    make_dir(root)?;
    make_dir(dir)?;
    match open(temp) {
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            make_dir(root)?;
            make_dir(dir)?;
            open(temp)
        }
        opened => opened,
    }
}

/// Keeps `post` for a later replay. `now` names and ages the file. The live
/// claude pids are dropped: replayed later, they would end sessions started
/// after the list was taken. `Ok(n)`: `n` older turn files of the session
/// were deleted to make room.
pub fn save(root: &Path, post: &HookPost, now: SystemTime) -> Result<usize, SpoolError> {
    if !keeps(&post.event) {
        return Err(SpoolError::NotKept);
    }
    let dir = session_dir(root, &post.session_id).ok_or(SpoolError::BadSession)?;
    let post = HookPost {
        live_claude_pids: None,
        ..post.clone()
    };
    let body = serde_json::to_vec(&post).map_err(|_| SpoolError::TooLarge)?;
    if body.len() > MAX_FILE {
        return Err(SpoolError::TooLarge);
    }
    if prune(root, now) >= MAX_FILES {
        return Err(SpoolError::Full);
    }
    let turn = is_turn(&post.event);
    let mut evicted = 0;
    if turn {
        let turns: Vec<PathBuf> = files(&dir)
            .into_iter()
            .filter_map(|(_, turn, file)| turn.then_some(file))
            .collect();
        // Oldest first: the newest answer stays.
        while turns.len() - evicted >= MAX_TURN_FILES {
            let _ = std::fs::remove_file(&turns[evicted]);
            evicted += 1;
        }
    } else if files(&dir).len() >= MAX_PER_SESSION {
        return Err(SpoolError::Full);
    }
    let io = |error: std::io::Error| SpoolError::Io(error.kind());
    let name = format!("{:020}-{}", nanos(now), post.event_id.as_str());
    let (temp, done) = if turn {
        (TURN_TMP_SUFFIX, TURN_SUFFIX)
    } else {
        (".tmp", ".json")
    };
    let temp = dir.join(format!("{name}{temp}"));
    let mut options = std::fs::OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    let mut file = create_temp(root, &dir, &temp, |path| options.open(path)).map_err(io)?;
    // Flushed before the rename: a crash never leaves an empty `.json`.
    let written = file.write_all(&body).and_then(|()| file.sync_all());
    drop(file);
    written
        .and_then(|()| std::fs::rename(&temp, dir.join(format!("{name}{done}"))))
        .map(|()| evicted)
        .map_err(|error| {
            let _ = std::fs::remove_file(&temp);
            io(error)
        })
}

/// The kept events of `session`, oldest first. Expired, unreadable, foreign
/// or oversized files and stale `.tmp` files are deleted on the way; a file
/// whose name and event disagree on being a turn event is foreign.
pub fn pending(root: &Path, session: &str, now: SystemTime) -> Vec<(PathBuf, HookPost)> {
    let Some(dir) = session_dir(root, session) else {
        return Vec::new();
    };
    drop_stale_tmp(&dir, now);
    let mut out = Vec::new();
    for (stamp, turn, file) in files(&dir) {
        let post = std::fs::read(&file)
            .ok()
            .filter(|body| body.len() <= MAX_FILE)
            .and_then(|body| wire::decode_hook(&body).ok())
            .filter(|post| {
                post.session_id == session && keeps(&post.event) && is_turn(&post.event) == turn
            });
        match post {
            Some(post) if !expired(stamp, turn, now) => out.push((file, post)),
            _ => {
                let _ = std::fs::remove_file(&file);
            }
        }
    }
    out
}

/// Sends the kept events of `session` in order, each deleted once the hub
/// has it, all before `deadline`. Stops at the first failure and keeps the
/// rest; a turn event the hub refused (4xx) is final: deleted, and the next
/// one goes. `Ok(n)`: `n` events delivered, none left.
pub async fn replay(
    root: &Path,
    session: &str,
    addr: &crate::tls::HubAddr,
    secret: &Secret,
    deadline: Instant,
) -> Result<usize, PostError> {
    let pending = pending(root, session, SystemTime::now());
    let mut sent = 0;
    for (file, kept) in pending {
        let left = deadline.saturating_duration_since(Instant::now());
        if left.is_zero() {
            return Err(PostError::Timeout(Duration::ZERO));
        }
        match post(addr, secret, &kept, left).await {
            Ok(()) => sent += 1,
            Err(PostError::Status(code @ 400..=499)) if is_turn(&kept.event) => {
                tracing::debug!(
                    event = kept.event.kind(),
                    status = code,
                    "kept turn event refused; dropped"
                );
            }
            Err(error) => return Err(error),
        }
        // Another replay of the same file may have removed it already.
        let _ = std::fs::remove_file(&file);
    }
    if let Some(dir) = session_dir(root, session) {
        let _ = std::fs::remove_dir(dir);
    }
    Ok(sent)
}

/// Whether `session` has kept files, by name only (no reads, no decodes).
pub fn has_kept(root: &Path, session: &str) -> bool {
    session_dir(root, session).is_some_and(|dir| !files(&dir).is_empty())
}

#[cfg(test)]
mod tests {
    use std::net::{Ipv4Addr, SocketAddr};

    use tokio::sync::mpsc;

    use crate::tls::HubAddr;

    use super::*;
    use crate::hub::ingress;
    use crate::hub::testdir::TempDir;

    const SECRET: &str = "0123456789abcdef-spool";
    const SESSION: &str = "5e551017-0000-4000-8000-000000000001";

    fn start(session: &str) -> HookPost {
        HookPost::new(
            "box".into(),
            session.into(),
            "/w".into(),
            "/w/s.jsonl".into(),
            HookEvent::SessionStart {
                source: Some("startup".into()),
                claude_pid: Some(7),
                parent_claude_pid: None,
            },
        )
    }

    fn end(session: &str) -> HookPost {
        HookPost::new(
            "box".into(),
            session.into(),
            "/w".into(),
            "/w/s.jsonl".into(),
            HookEvent::SessionEnd {
                reason: Some("other".into()),
                claude_pid: Some(7),
            },
        )
    }

    fn at(secs: u64) -> SystemTime {
        UNIX_EPOCH + Duration::from_secs(1_800_000_000 + secs)
    }

    fn event(session: &str, event: HookEvent) -> HookPost {
        HookPost::new(
            "box".into(),
            session.into(),
            "/w".into(),
            String::new(),
            event,
        )
    }

    fn stop(session: &str, text: &str) -> HookPost {
        event(
            session,
            HookEvent::Stop {
                prompt_id: None,
                last_assistant_message: Some(text.into()),
            },
        )
    }

    fn turn_events(session: &str) -> Vec<HookPost> {
        vec![
            event(
                session,
                HookEvent::UserPromptSubmit {
                    prompt_id: Some("p".into()),
                },
            ),
            stop(session, "private answer text"),
            event(
                session,
                HookEvent::SubagentStart {
                    agent_id: "a1".into(),
                    agent_type: "Explore".into(),
                },
            ),
            event(
                session,
                HookEvent::SubagentStop {
                    agent_id: "a1".into(),
                    agent_type: "Explore".into(),
                    agent_transcript_path: None,
                    last_assistant_message: Some("report".into()),
                },
            ),
            event(
                session,
                HookEvent::SubagentHandback {
                    agent_id: "a1".into(),
                    message: "report".into(),
                },
            ),
            event(
                session,
                HookEvent::PreCompact {
                    trigger: Some("auto".into()),
                },
            ),
        ]
    }

    fn name_of(file: &Path) -> String {
        file.file_name().unwrap().to_string_lossy().into_owned()
    }

    #[test]
    fn turn_events_are_kept_and_status_events_are_not() {
        let dir = TempDir::new("spool-kinds");
        let root = dir.path().join("spool");
        let status = [
            event(
                SESSION,
                HookEvent::ToolStart {
                    tool_use_id: "t".into(),
                    line: "Bash: x".into(),
                },
            ),
            event(
                SESSION,
                HookEvent::ToolEnd {
                    tool_use_id: "t".into(),
                },
            ),
            event(
                SESSION,
                HookEvent::StatusLine {
                    model: None,
                    effort: None,
                    context: Some(1),
                    five_hour: None,
                    seven_day: None,
                },
            ),
        ];
        for post in &status {
            assert_eq!(save(&root, post, at(0)), Err(SpoolError::NotKept));
        }
        assert!(!root.exists());
        let turns = turn_events(SESSION);
        save(&root, &start(SESSION), at(1)).unwrap();
        for (n, post) in turns.iter().enumerate() {
            assert_eq!(save(&root, post, at(2 + n as u64)), Ok(0));
        }
        save(&root, &end(SESSION), at(20)).unwrap();
        let kept = pending(&root, SESSION, at(21));
        let back: Vec<HookPost> = kept.iter().map(|(_, post)| post.clone()).collect();
        assert_eq!(back.len(), turns.len() + 2);
        assert_eq!(back[0].event.kind(), "session_start");
        assert_eq!(back[1..=turns.len()], turns[..], "in order, with their ids");
        assert_eq!(back[turns.len() + 1].event.kind(), "session_end");
        for (file, post) in &kept {
            let body = std::fs::read_to_string(file).unwrap();
            assert!(!body.contains(SECRET));
            let end = if is_turn(&post.event) {
                ".turn.json"
            } else {
                ".json"
            };
            assert!(
                name_of(file).ends_with(&format!("-{}{end}", post.event_id.as_str())),
                "{}",
                name_of(file)
            );
        }
        assert_eq!(
            kept.iter()
                .filter(|(file, _)| name_of(file).ends_with(TURN_SUFFIX))
                .count(),
            6
        );
    }

    #[test]
    fn the_newest_turn_events_are_kept() {
        let dir = TempDir::new("spool-turn-evict");
        let root = dir.path().join("spool");
        let stops: Vec<HookPost> = (0..MAX_TURN_FILES + 2)
            .map(|n| stop(SESSION, &format!("answer {n}")))
            .collect();
        let mut last = Ok(0);
        for (n, post) in stops.iter().enumerate() {
            last = save(&root, post, at(n as u64));
        }
        assert_eq!(last, Ok(1));
        let back: Vec<HookPost> = pending(&root, SESSION, at(20))
            .into_iter()
            .map(|(_, post)| post)
            .collect();
        assert_eq!(back, stops[2..]);
        // Lifecycle events still find room.
        assert_eq!(save(&root, &end(SESSION), at(21)), Ok(0));
        assert_eq!(pending(&root, SESSION, at(22)).len(), MAX_TURN_FILES + 1);
    }

    #[test]
    fn an_old_turn_file_is_dropped_by_any_sweep() {
        let dir = TempDir::new("spool-turn-age");
        let root = dir.path().join("spool");
        const X: &str = "5e551017-0000-4000-8000-00000000000x";
        save(&root, &stop(X, "private answer text"), at(0)).unwrap();
        save(&root, &start(X), at(0)).unwrap();
        let later = at(0) + TURN_MAX_AGE + Duration::from_secs(1);
        // A save of another session deletes X's old turn file by name.
        save(&root, &start(SESSION), later).unwrap();
        let left: Vec<String> = files(&root.join(X))
            .into_iter()
            .map(|(_, _, file)| name_of(&file))
            .collect();
        assert_eq!(left.len(), 1, "{left:?}");
        assert!(!left[0].ends_with(TURN_SUFFIX), "{left:?}");
        let kept = pending(&root, X, later);
        assert_eq!(kept.len(), 1);
        assert_eq!(kept[0].1.event.kind(), "session_start");
        // The agent's sweep does the same.
        save(&root, &stop(X, "another answer"), later).unwrap();
        sweep(&root, later + TURN_MAX_AGE + Duration::from_secs(1));
        let left = files(&root.join(X));
        assert_eq!(left.len(), 1);
        assert!(!left[0].1, "only the start stays");
    }

    #[test]
    fn a_mismatched_name_is_foreign() {
        let dir = TempDir::new("spool-mismatch");
        let root = dir.path().join("spool");
        let session = root.join(SESSION);
        std::fs::create_dir_all(&session).unwrap();
        let answer = stop(SESSION, "x");
        let as_lifecycle = session.join(format!(
            "{:020}-{}.json",
            nanos(at(1)),
            answer.event_id.as_str()
        ));
        std::fs::write(&as_lifecycle, serde_json::to_vec(&answer).unwrap()).unwrap();
        let begin = start(SESSION);
        let as_turn = session.join(format!(
            "{:020}-{}{TURN_SUFFIX}",
            nanos(at(2)),
            begin.event_id.as_str()
        ));
        std::fs::write(&as_turn, serde_json::to_vec(&begin).unwrap()).unwrap();
        assert!(has_kept(&root, SESSION));
        assert!(pending(&root, SESSION, at(3)).is_empty());
        assert!(!as_lifecycle.exists() && !as_turn.exists());
    }

    #[test]
    fn older_builds_do_not_see_turn_files() {
        let name = format!(
            "{:020}-{}{TURN_SUFFIX}",
            7, "0123456789abcdef0123456789abcdef"
        );
        assert_eq!(stamp(&name), Some((7, true)));
        // What a build before TASK-088 does with the name.
        assert_eq!(name_stamp(name.strip_suffix(".json").unwrap()), None);
        let tmp = format!(
            "{:020}-{}{TURN_TMP_SUFFIX}",
            7, "0123456789abcdef0123456789abcdef"
        );
        assert_eq!(tmp_stamp(&tmp), Some(7));
    }

    #[test]
    fn a_long_answer_fits_a_file() {
        let dir = TempDir::new("spool-long");
        let root = dir.path().join("spool");
        // Control characters: the worst escaping (`\u0001`, 6 bytes each).
        let long = stop(SESSION, &"\u{1}".repeat(crate::hook::MAX_TEXT));
        assert_eq!(save(&root, &long, at(0)), Ok(0));
        let kept = pending(&root, SESSION, at(1));
        assert_eq!(kept.len(), 1);
        assert_eq!(kept[0].1, long);
    }

    /// TASK-088 review, finding 1: a `prune` or a replay of another process
    /// removes the empty session directory between its creation and the
    /// file's; the save makes it again once instead of losing the event.
    #[test]
    fn a_session_dir_removed_before_the_file_is_made_again() {
        let dir = TempDir::new("spool-dir-race");
        let root = dir.path().join("spool");
        let session = root.join(SESSION);
        let temp = session.join("x.tmp");
        let open = |path: &Path| {
            std::fs::OpenOptions::new()
                .write(true)
                .create_new(true)
                .open(path)
        };
        let mut calls = 0;
        let made = create_temp(&root, &session, &temp, |path| {
            calls += 1;
            if calls == 1 {
                // The racing `remove_dir` of the empty directory.
                std::fs::remove_dir(&session).unwrap();
            }
            open(path)
        });
        assert!(made.is_ok(), "{made:?}");
        assert_eq!(calls, 2);
        assert!(temp.is_file());
        // Only once: a directory that keeps vanishing is an error.
        let again = session.join("y.tmp");
        let mut calls = 0;
        let made = create_temp(&root, &session, &again, |path| {
            calls += 1;
            let _ = std::fs::remove_file(&temp);
            std::fs::remove_dir(&session).unwrap();
            open(path)
        });
        assert_eq!(
            made.map_err(|error| error.kind()).err(),
            Some(std::io::ErrorKind::NotFound)
        );
        assert_eq!(calls, 2);
    }

    #[test]
    fn the_turn_age_stays_inside_the_hub_dedup() {
        assert!(TURN_MAX_AGE < ingress::DEDUP_TTL);
    }

    #[cfg(unix)]
    #[test]
    fn spool_files_are_private() {
        use std::os::unix::fs::PermissionsExt;
        let dir = TempDir::new("spool-private");
        let root = dir.path().join("spool");
        save(&root, &stop(SESSION, "private answer text"), at(0)).unwrap();
        save(&root, &start(SESSION), at(1)).unwrap();
        let mode = |path: &Path| std::fs::metadata(path).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode(&root), 0o700);
        assert_eq!(mode(&root.join(SESSION)), 0o700);
        for (_, _, file) in files(&root.join(SESSION)) {
            assert_eq!(mode(&file), 0o600);
        }
    }

    #[test]
    fn a_kept_event_loses_its_live_claude_pids() {
        let dir = TempDir::new("spool-live-pids");
        let root = dir.path().join("spool");
        let mut post = start(SESSION);
        post.live_claude_pids = Some(vec![7, 9]);
        save(&root, &post, at(1)).unwrap();
        let kept = pending(&root, SESSION, at(2));
        assert_eq!(kept.len(), 1);
        assert_eq!(kept[0].1.event_id, post.event_id);
        assert_eq!(kept[0].1.live_claude_pids, None);
        assert_eq!(kept[0].1.event, post.event);
    }

    #[test]
    fn events_come_back_in_order_with_their_ids() {
        let dir = TempDir::new("spool-order");
        let root = dir.path().join("spool");
        let posts: Vec<HookPost> = (0..5)
            .map(|n| {
                if n % 2 == 0 {
                    start(SESSION)
                } else {
                    end(SESSION)
                }
            })
            .collect();
        // Written out of order in time: read back by time.
        for (n, post) in posts.iter().enumerate().rev() {
            save(&root, post, at(n as u64)).unwrap();
        }
        let back: Vec<HookPost> = pending(&root, SESSION, at(10))
            .into_iter()
            .map(|(_, post)| post)
            .collect();
        assert_eq!(back, posts);
        assert!(pending(&root, "another-session", at(10)).is_empty());
    }

    #[test]
    fn the_spool_is_bounded_per_session_in_all_and_per_file() {
        let dir = TempDir::new("spool-bounds");
        let root = dir.path().join("spool");
        for n in 0..MAX_PER_SESSION {
            save(&root, &start(SESSION), at(n as u64)).unwrap();
        }
        assert_eq!(save(&root, &start(SESSION), at(100)), Err(SpoolError::Full));
        for s in 1..MAX_FILES / MAX_PER_SESSION {
            let session = format!("session-{s}");
            for n in 0..MAX_PER_SESSION {
                save(&root, &start(&session), at(n as u64)).unwrap();
            }
        }
        assert_eq!(
            save(&root, &start("one-more"), at(100)),
            Err(SpoolError::Full)
        );
        let mut big = start("big");
        big.cwd = "x".repeat(MAX_FILE);
        assert_eq!(save(&root, &big, at(100)), Err(SpoolError::TooLarge));
        // Once the old ones expire there is room again, and they are gone.
        let later = at(0) + MAX_AGE + Duration::from_secs(MAX_PER_SESSION as u64 + 1);
        save(&root, &start("one-more"), later).unwrap();
        assert_eq!(prune(&root, later), 1);
        assert!(pending(&root, SESSION, later).is_empty());
    }

    /// Savers that race each add at most their own file past a bound, and the
    /// next save after them sees the spool full; no file is half written.
    #[test]
    fn concurrent_saves_overshoot_a_bound_by_at_most_one_file_each() {
        const SAVERS: usize = 8;
        let dir = TempDir::new("spool-race");
        let root = dir.path().join("spool");
        for n in 0..MAX_PER_SESSION - 1 {
            save(&root, &start(SESSION), at(n as u64)).unwrap();
        }
        let barrier = std::sync::Barrier::new(SAVERS);
        std::thread::scope(|scope| {
            for _ in 0..SAVERS {
                scope.spawn(|| {
                    barrier.wait();
                    let _ = save(&root, &end(SESSION), at(50));
                });
            }
        });
        let kept = pending(&root, SESSION, at(60));
        assert!(
            (MAX_PER_SESSION..MAX_PER_SESSION + SAVERS).contains(&kept.len()),
            "{}",
            kept.len()
        );
        assert!(kept.iter().all(|(_, post)| keeps(&post.event)));
        assert_eq!(save(&root, &end(SESSION), at(61)), Err(SpoolError::Full));
    }

    #[test]
    fn unusable_session_ids_are_not_written() {
        let dir = TempDir::new("spool-ids");
        let root = dir.path().join("spool");
        for bad in ["", "..", "../x", "a/b", r"a\b", "a:b", &"x".repeat(129)] {
            assert_eq!(
                save(&root, &start(bad), at(0)),
                Err(SpoolError::BadSession),
                "{bad}"
            );
            assert!(pending(&root, bad, at(0)).is_empty());
        }
        assert!(!root.exists());
    }

    #[test]
    fn broken_foreign_and_old_files_are_dropped_on_read() {
        let dir = TempDir::new("spool-broken");
        let root = dir.path().join("spool");
        save(&root, &start(SESSION), at(5)).unwrap();
        let session = root.join(SESSION);
        let id = "0123456789abcdef0123456789abcdef";
        let garbage = session.join(format!("{:020}-{id}.json", nanos(at(4))));
        std::fs::write(&garbage, b"{not json").unwrap();
        let mut foreign = serde_json::to_value(start("other-session")).unwrap();
        foreign["event_id"] = id.into();
        let foreign_file = session.join(format!("{:020}-{id}.json", nanos(at(4)) + 1));
        std::fs::write(&foreign_file, foreign.to_string()).unwrap();
        // A `.tmp` being written now is left alone; one a dead writer left
        // a minute ago is deleted.
        let writing = session.join(format!("{:020}-{id}.tmp", nanos(at(6))));
        std::fs::write(&writing, b"half").unwrap();
        let dead = session.join(format!(
            "{:020}-{id}.tmp",
            nanos(at(6) - TMP_GRACE - Duration::from_secs(1))
        ));
        std::fs::write(&dead, b"half").unwrap();
        let kept = pending(&root, SESSION, at(6));
        assert_eq!(kept.len(), 1);
        assert!(!garbage.exists() && !foreign_file.exists());
        assert!(writing.exists(), "a file being written is left alone");
        assert!(!dead.exists(), "a dead writer's file is deleted");
        assert!(pending(&root, SESSION, at(5) + MAX_AGE + Duration::from_secs(1)).is_empty());
        assert!(!kept[0].0.exists(), "an expired file is deleted unsent");
    }

    async fn hooks_hub(queue: usize) -> (HubAddr, mpsc::Receiver<HookPost>) {
        let listener = ingress::bind(SocketAddr::from((Ipv4Addr::LOCALHOST, 0)))
            .await
            .unwrap();
        let addr = HubAddr::plain(listener.local_addr().unwrap().to_string());
        let (tx, rx) = mpsc::channel(queue);
        tokio::spawn(ingress::serve_hooks(
            listener,
            Secret::parse(SECRET).unwrap(),
            tx,
        ));
        (addr, rx)
    }

    fn deadline() -> Instant {
        Instant::now() + Duration::from_secs(5)
    }

    #[tokio::test]
    async fn a_replay_delivers_in_order_once_and_empties_the_spool() {
        let dir = TempDir::new("spool-replay");
        let root = dir.path().join("spool");
        let (first, second) = (start(SESSION), end(SESSION));
        save(&root, &first, SystemTime::now() - Duration::from_secs(2)).unwrap();
        save(&root, &second, SystemTime::now() - Duration::from_secs(1)).unwrap();
        let (addr, mut events) = hooks_hub(8).await;
        let secret = Secret::parse(SECRET).unwrap();
        assert_eq!(
            replay(&root, SESSION, &addr, &secret, deadline()).await,
            Ok(2)
        );
        assert_eq!(events.recv().await, Some(first.clone()));
        assert_eq!(events.recv().await, Some(second));
        assert!(!root.join(SESSION).exists());
        assert_eq!(
            replay(&root, SESSION, &addr, &secret, deadline()).await,
            Ok(0)
        );
        // A file whose delivery was not recorded (a crash between the
        // answer and the delete) goes again and the hub keeps one copy.
        save(&root, &first, SystemTime::now()).unwrap();
        assert_eq!(
            replay(&root, SESSION, &addr, &secret, deadline()).await,
            Ok(1)
        );
        tokio::time::sleep(Duration::from_millis(100)).await;
        assert!(events.try_recv().is_err());
    }

    /// A hook endpoint that answers request `n` with `codes[n]` (204 past
    /// the end), each after reading the whole request.
    async fn answering_hub(codes: Vec<u16>) -> HubAddr {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};
        let listener = tokio::net::TcpListener::bind(SocketAddr::from((Ipv4Addr::LOCALHOST, 0)))
            .await
            .unwrap();
        let addr = HubAddr::plain(listener.local_addr().unwrap().to_string());
        tokio::spawn(async move {
            let mut n = 0;
            while let Ok((mut stream, _)) = listener.accept().await {
                let code = codes.get(n).copied().unwrap_or(204);
                n += 1;
                let mut request = Vec::new();
                let mut buf = [0u8; 4096];
                while let Ok(read) = stream.read(&mut buf).await {
                    if read == 0 {
                        break;
                    }
                    request.extend_from_slice(&buf[..read]);
                    let text = String::from_utf8_lossy(&request);
                    let Some(head) = text.find("\r\n\r\n") else {
                        continue;
                    };
                    let length: usize = text[..head]
                        .lines()
                        .find_map(|line| line.strip_prefix("Content-Length: "))
                        .and_then(|value| value.trim().parse().ok())
                        .unwrap_or(0);
                    if request.len() >= head + 4 + length {
                        break;
                    }
                }
                let answer = format!("HTTP/1.1 {code} X\r\nContent-Length: 0\r\n\r\n");
                let _ = stream.write_all(answer.as_bytes()).await;
                let _ = stream.shutdown().await;
            }
        });
        addr
    }

    #[tokio::test]
    async fn a_refused_turn_event_does_not_block_the_session() {
        let dir = TempDir::new("spool-refused-turn");
        let root = dir.path().join("spool");
        let secret = Secret::parse(SECRET).unwrap();
        save(
            &root,
            &stop(SESSION, "x"),
            SystemTime::now() - Duration::from_secs(2),
        )
        .unwrap();
        save(
            &root,
            &end(SESSION),
            SystemTime::now() - Duration::from_secs(1),
        )
        .unwrap();
        let addr = answering_hub(vec![400, 204]).await;
        assert_eq!(
            replay(&root, SESSION, &addr, &secret, deadline()).await,
            Ok(1)
        );
        assert!(pending(&root, SESSION, SystemTime::now()).is_empty());
        // A refused start still stops the replay and stays (TASK-018).
        save(&root, &start(SESSION), SystemTime::now()).unwrap();
        let addr = answering_hub(vec![400]).await;
        assert_eq!(
            replay(&root, SESSION, &addr, &secret, deadline()).await,
            Err(PostError::Status(400))
        );
        assert_eq!(pending(&root, SESSION, SystemTime::now()).len(), 1);
    }

    #[tokio::test]
    async fn a_failed_replay_keeps_what_was_not_delivered() {
        let dir = TempDir::new("spool-fail");
        let root = dir.path().join("spool");
        let (first, second) = (start(SESSION), end(SESSION));
        save(&root, &first, SystemTime::now() - Duration::from_secs(2)).unwrap();
        save(&root, &second, SystemTime::now() - Duration::from_secs(1)).unwrap();
        // Room for one event: the second is answered 503.
        let (addr, mut events) = hooks_hub(1).await;
        let secret = Secret::parse(SECRET).unwrap();
        assert_eq!(
            replay(&root, SESSION, &addr, &secret, deadline()).await,
            Err(PostError::Status(503))
        );
        let left = pending(&root, SESSION, SystemTime::now());
        assert_eq!(left.len(), 1);
        assert_eq!(left[0].1, second);
        assert_eq!(events.recv().await, Some(first));
        assert_eq!(
            replay(&root, SESSION, &addr, &secret, deadline()).await,
            Ok(1)
        );
        assert_eq!(events.recv().await, Some(second));
        // No hub at all: nothing is lost.
        save(&root, &start(SESSION), SystemTime::now()).unwrap();
        let closed = std::net::TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).unwrap();
        let gone = HubAddr::plain(closed.local_addr().unwrap().to_string());
        drop(closed);
        let short = Instant::now() + Duration::from_millis(300);
        assert!(replay(&root, SESSION, &gone, &secret, short).await.is_err());
        assert_eq!(pending(&root, SESSION, SystemTime::now()).len(), 1);
    }
}
