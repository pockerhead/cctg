//! Session lifecycle events the hub did not take, kept on this device until a
//! later hook or the agent of the same session delivers them (TASK-018).
//!
//! Without it a session whose `SessionStart` ran while the hub was down stays
//! unknown to the hub for good: the hub never adopts a session it did not see
//! start.
//!
//! Layout: `<state>/spool/<session id>/<nanos:020>-<event id>.json`, one
//! [`HookPost`] per file, exactly as it was to be sent. It keeps its event id,
//! so the hub drops a copy it already has. A file is written under a `.tmp`
//! name, flushed to disk and renamed (as `RegistryStore::save` does), so a
//! reader never sees half of one (the maildir scheme); several replays at
//! once only send an event twice, and the hub keeps one. A `.tmp` left by a
//! writer that died is deleted once it is [`TMP_GRACE`] old.
//!
//! Only `SessionStart` and `SessionEnd` are kept: they carry ids, pids, the
//! folder and the transcript path, never prompt or answer text. The secret is
//! never part of a [`HookPost`]. Bounds: [`MAX_PER_SESSION`] files per
//! session, [`MAX_FILES`] in all, [`MAX_FILE`] bytes each, [`MAX_AGE`] old.
//! The counts are checked without a lock: hooks that save at the same moment
//! may each add their one file, so with `k` of them the spool holds at most
//! `k - 1` files over a bound (a few KiB), and the next save sees it full.

use std::io::Write;
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use tokio::time::Instant;

use crate::hook::{PostError, post};
use crate::wire::{self, HookEvent, HookPost, Secret};

/// Files one session may keep; a later event of a full session is dropped
/// (the oldest, its start, is the one that matters).
pub const MAX_PER_SESSION: usize = 16;
/// Files of all sessions together.
pub const MAX_FILES: usize = 256;
/// Bytes of one file; a start or an end is well under 1 KiB plus two paths.
pub const MAX_FILE: usize = 16 << 10;
/// Older files are deleted unsent: that session is long over.
pub const MAX_AGE: Duration = Duration::from_secs(24 * 60 * 60);
/// A `.tmp` this old belongs to a writer that died; a live one renames it
/// within milliseconds.
pub const TMP_GRACE: Duration = Duration::from_secs(60);
const SPOOL_DIR: &str = "spool";
const MAX_SESSION_ID: usize = 128;

/// Why an event was not kept. Fixed text, safe to log.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum SpoolError {
    #[error("only session starts and ends are kept")]
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

/// The events worth keeping: the ones the hub needs to know a session at all.
pub fn keeps(event: &HookEvent) -> bool {
    matches!(
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

/// The write time of a spool file from its name `<nanos:020>-<32 hex>.json`.
fn stamp(name: &str) -> Option<u128> {
    name_stamp(name.strip_suffix(".json")?)
}

/// The same for a file being written, `<nanos:020>-<32 hex>.tmp`.
fn tmp_stamp(name: &str) -> Option<u128> {
    name_stamp(name.strip_suffix(".tmp")?)
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

fn expired(stamp: u128, now: SystemTime) -> bool {
    nanos(now).saturating_sub(stamp) > MAX_AGE.as_nanos()
}

/// Spool files of one session directory, oldest first. Files that are not
/// spool files (a `.tmp` being written) are left alone.
fn files(dir: &Path) -> Vec<(u128, PathBuf)> {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return Vec::new();
    };
    let mut files: Vec<(u128, PathBuf)> = entries
        .filter_map(Result::ok)
        .filter_map(|entry| {
            let name = entry.file_name().into_string().ok()?;
            Some((stamp(&name)?, entry.path()))
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
        for (stamp, file) in files(&path) {
            if expired(stamp, now) {
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

/// Keeps `post` for a later replay. `now` names and ages the file. The live
/// claude pids are dropped: replayed later, they would end sessions started
/// after the list was taken.
pub fn save(root: &Path, post: &HookPost, now: SystemTime) -> Result<(), SpoolError> {
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
    if prune(root, now) >= MAX_FILES || files(&dir).len() >= MAX_PER_SESSION {
        return Err(SpoolError::Full);
    }
    let io = |error: std::io::Error| SpoolError::Io(error.kind());
    std::fs::create_dir_all(&dir).map_err(io)?;
    let name = format!("{:020}-{}", nanos(now), post.event_id.as_str());
    let temp = dir.join(format!("{name}.tmp"));
    let mut file = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&temp)
        .map_err(io)?;
    // Flushed before the rename: a crash never leaves an empty `.json`.
    let written = file.write_all(&body).and_then(|()| file.sync_all());
    drop(file);
    written
        .and_then(|()| std::fs::rename(&temp, dir.join(format!("{name}.json"))))
        .map_err(|error| {
            let _ = std::fs::remove_file(&temp);
            io(error)
        })
}

/// The kept events of `session`, oldest first. Expired, unreadable, foreign
/// or oversized files and stale `.tmp` files are deleted on the way.
pub fn pending(root: &Path, session: &str, now: SystemTime) -> Vec<(PathBuf, HookPost)> {
    let Some(dir) = session_dir(root, session) else {
        return Vec::new();
    };
    drop_stale_tmp(&dir, now);
    let mut out = Vec::new();
    for (stamp, file) in files(&dir) {
        let post = std::fs::read(&file)
            .ok()
            .filter(|body| body.len() <= MAX_FILE)
            .and_then(|body| wire::decode_hook(&body).ok())
            .filter(|post| post.session_id == session && keeps(&post.event));
        match post {
            Some(post) if !expired(stamp, now) => out.push((file, post)),
            _ => {
                let _ = std::fs::remove_file(&file);
            }
        }
    }
    out
}

/// Sends the kept events of `session` in order, each deleted once the hub
/// has it, all before `deadline`. Stops at the first failure and keeps the
/// rest. `Ok(n)`: `n` events delivered, none left.
pub async fn replay(
    root: &Path,
    session: &str,
    addr: &str,
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
        post(addr, secret, &kept, left).await?;
        // Another replay of the same file may have removed it already.
        let _ = std::fs::remove_file(&file);
        sent += 1;
    }
    if let Some(dir) = session_dir(root, session) {
        let _ = std::fs::remove_dir(dir);
    }
    Ok(sent)
}

#[cfg(test)]
mod tests {
    use std::net::{Ipv4Addr, SocketAddr};

    use tokio::sync::mpsc;

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

    #[test]
    fn only_starts_and_ends_are_kept_and_nothing_else_is_written() {
        let dir = TempDir::new("spool-kinds");
        let root = dir.path().join("spool");
        let text = HookPost::new(
            "box".into(),
            SESSION.into(),
            "/w".into(),
            String::new(),
            HookEvent::Stop {
                prompt_id: None,
                last_assistant_message: Some("private answer text".into()),
            },
        );
        let prompt = HookPost::new(
            "box".into(),
            SESSION.into(),
            "/w".into(),
            String::new(),
            HookEvent::UserPromptSubmit {
                prompt_id: Some("p".into()),
            },
        );
        for post in [&text, &prompt] {
            assert_eq!(save(&root, post, at(0)), Err(SpoolError::NotKept));
        }
        assert!(!root.exists());
        save(&root, &start(SESSION), at(1)).unwrap();
        save(&root, &end(SESSION), at(2)).unwrap();
        let kept = pending(&root, SESSION, at(3));
        assert_eq!(kept.len(), 2);
        assert_eq!(kept[0].1.event.kind(), "session_start");
        assert_eq!(kept[1].1.event.kind(), "session_end");
        for (file, post) in &kept {
            let body = std::fs::read_to_string(file).unwrap();
            assert!(!body.contains(SECRET));
            let name = file.file_name().unwrap().to_string_lossy().into_owned();
            assert!(name.ends_with(&format!("-{}.json", post.event_id.as_str())));
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

    async fn hooks_hub(queue: usize) -> (String, mpsc::Receiver<HookPost>) {
        let listener = ingress::bind(SocketAddr::from((Ipv4Addr::LOCALHOST, 0)))
            .await
            .unwrap();
        let addr = listener.local_addr().unwrap().to_string();
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
        let gone = closed.local_addr().unwrap().to_string();
        drop(closed);
        let short = Instant::now() + Duration::from_millis(300);
        assert!(replay(&root, SESSION, &gone, &secret, short).await.is_err());
        assert_eq!(pending(&root, SESSION, SystemTime::now()).len(), 1);
    }
}
