//! Status line numbers on their way from `cctg statusline` to the session's
//! agent (TASK-058).
//!
//! `cctg statusline` runs after every assistant message and must return
//! quickly, so it never waits for a hub on another machine: it replaces
//! `<state>/status/<session>.json` with the latest numbers (temp file and
//! rename, `0600` on Unix) and returns. The agent of that session, which
//! keeps its link to the hub open anyway, looks at the file once a second
//! and sends what changed as `status_line` ([`crate::wire::AgentMsg`]).
//!
//! An agent that sends numbers for a session keeps `<session>.agent`
//! fresh ([`MARK_EVERY`]). Only without a fresh mark does `cctg statusline`
//! post the numbers itself, and only to a hub on this machine: a session
//! without a channel, or an agent built before TASK-058.
//!
//! The files hold the session id and the numbers only. The agent removes
//! its session's files when it leaves or moves on after `/clear`; any file
//! older than [`MAX_AGE`] goes when an agent starts.

use std::io::Write;
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime};

use serde::{Deserialize, Serialize};

use crate::wire::HookEvent;

/// How often an agent that passes numbers on renews its mark.
pub const MARK_EVERY: Duration = Duration::from_secs(30);
/// A mark older than this belongs to an agent that is gone or cut off.
pub const MARK_FRESH: Duration = Duration::from_secs(120);
/// Files older than this are removed when an agent starts.
pub const MAX_AGE: Duration = Duration::from_secs(24 * 60 * 60);
/// Longest status file read.
const MAX_FILE: u64 = 4 << 10;
const MAX_SESSION_ID: usize = 128;

/// One status file.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct StatusFile {
    pub session_id: String,
    /// Always [`HookEvent::StatusLine`].
    pub numbers: HookEvent,
}

/// `<state>/status`.
pub fn dir(state_dir: &Path) -> PathBuf {
    state_dir.join("status")
}

/// A session id usable as a file name; anything else has no files.
fn usable(session: &str) -> bool {
    !session.is_empty()
        && session.len() <= MAX_SESSION_ID
        && session
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || byte == b'-' || byte == b'_')
}

fn numbers_path(state_dir: &Path, session: &str) -> Option<PathBuf> {
    usable(session).then(|| dir(state_dir).join(format!("{session}.json")))
}

fn mark_path(state_dir: &Path, session: &str) -> Option<PathBuf> {
    usable(session).then(|| dir(state_dir).join(format!("{session}.agent")))
}

/// Creates `<state>/status` (`0700` on Unix).
fn make_dir(state_dir: &Path) -> std::io::Result<PathBuf> {
    let dir = dir(state_dir);
    if !dir.is_dir() {
        std::fs::create_dir_all(&dir)?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o700))?;
        }
    }
    Ok(dir)
}

/// A new file only this user may read (`0600` on Unix).
fn create_private(path: &Path) -> std::io::Result<std::fs::File> {
    let mut options = std::fs::OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    options.open(path)
}

/// Replaces the numbers of `session` with `numbers` (a
/// [`HookEvent::StatusLine`]). Leaves the file alone when it holds the same
/// numbers already, so the agent sends nothing new. Not flushed to disk:
/// the numbers are only worth something while the session runs.
pub fn write(state_dir: &Path, session: &str, numbers: &HookEvent) -> std::io::Result<()> {
    let Some(path) = numbers_path(state_dir, session) else {
        return Err(std::io::ErrorKind::InvalidInput.into());
    };
    let body = serde_json::to_vec(&StatusFile {
        session_id: session.to_owned(),
        numbers: numbers.clone(),
    })
    .map_err(|_| std::io::Error::from(std::io::ErrorKind::InvalidData))?;
    if std::fs::read(&path).is_ok_and(|old| old == body) {
        return Ok(());
    }
    let dir = make_dir(state_dir)?;
    let temp = dir.join(format!(
        "{session}.{}-{:016x}.tmp",
        std::process::id(),
        crate::wire::random_u64()
    ));
    let written = create_private(&temp).and_then(|mut file| file.write_all(&body));
    // `rename` replaces an existing file on every OS (on Windows too).
    written
        .and_then(|()| std::fs::rename(&temp, &path))
        .inspect_err(|_| {
            let _ = std::fs::remove_file(&temp);
        })
}

/// When the numbers of `session` last changed, and the numbers; `None`
/// without a readable file of that session.
pub fn read(state_dir: &Path, session: &str) -> Option<(SystemTime, HookEvent)> {
    let path = numbers_path(state_dir, session)?;
    let changed = std::fs::metadata(&path).ok()?.modified().ok()?;
    let mut body = Vec::new();
    std::io::Read::read_to_end(
        &mut std::io::Read::take(std::fs::File::open(&path).ok()?, MAX_FILE + 1),
        &mut body,
    )
    .ok()?;
    if body.len() as u64 > MAX_FILE {
        return None;
    }
    let file: StatusFile = serde_json::from_slice(&body).ok()?;
    (file.session_id == session && matches!(file.numbers, HookEvent::StatusLine { .. }))
        .then_some((changed, file.numbers))
}

/// When the numbers file of `session` last changed; `None` without one.
pub fn changed(state_dir: &Path, session: &str) -> Option<SystemTime> {
    let path = numbers_path(state_dir, session)?;
    std::fs::metadata(path).ok()?.modified().ok()
}

/// Renews the agent's mark for `session`.
pub fn mark(state_dir: &Path, session: &str) -> std::io::Result<()> {
    let Some(path) = mark_path(state_dir, session) else {
        return Err(std::io::ErrorKind::InvalidInput.into());
    };
    make_dir(state_dir)?;
    let file = match create_private(&path) {
        Ok(file) => file,
        Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {
            std::fs::OpenOptions::new().write(true).open(&path)?
        }
        Err(error) => return Err(error),
    };
    file.set_modified(SystemTime::now())
}

/// Whether an agent renewed its mark for `session` within [`MARK_FRESH`]
/// of `now`.
pub fn agent_present(state_dir: &Path, session: &str, now: SystemTime) -> bool {
    mark_path(state_dir, session)
        .and_then(|path| std::fs::metadata(path).ok())
        .and_then(|meta| meta.modified().ok())
        .is_some_and(|marked| {
            now.duration_since(marked)
                .map_or(true, |age| age < MARK_FRESH)
        })
}

/// Removes the numbers and the mark of `session`.
pub fn remove(state_dir: &Path, session: &str) {
    for path in [
        numbers_path(state_dir, session),
        mark_path(state_dir, session),
    ]
    .into_iter()
    .flatten()
    {
        let _ = std::fs::remove_file(path);
    }
}

/// Removes files of `<state>/status` older than [`MAX_AGE`] (sessions that
/// ended without their agent cleaning up) and temp files older than a
/// minute (a writer that died).
pub fn prune(state_dir: &Path, now: SystemTime) {
    let Ok(entries) = std::fs::read_dir(dir(state_dir)) else {
        return;
    };
    for entry in entries.filter_map(Result::ok) {
        let path = entry.path();
        let Some(changed) = entry.metadata().ok().and_then(|meta| meta.modified().ok()) else {
            continue;
        };
        let age = now.duration_since(changed).unwrap_or_default();
        let temp = path.extension().is_some_and(|ext| ext == "tmp");
        if age > MAX_AGE || (temp && age > Duration::from_secs(60)) {
            let _ = std::fs::remove_file(path);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn numbers(context: u32) -> HookEvent {
        HookEvent::StatusLine {
            model: Some("Opus".into()),
            effort: None,
            context: Some(context),
            five_hour: None,
            seven_day: Some(92),
        }
    }

    fn state(test: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "cctg-test-statusfile-{test}-{}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&dir);
        dir
    }

    const S: &str = "5e551017-0000-4000-8000-000000000001";

    #[test]
    fn numbers_are_replaced_and_read_back_for_their_session_only() {
        let state = state("replace");
        assert_eq!(read(&state, S), None);
        write(&state, S, &numbers(10)).unwrap();
        let (first, got) = read(&state, S).unwrap();
        assert_eq!(got, numbers(10));
        write(&state, S, &numbers(20)).unwrap();
        assert_eq!(read(&state, S).unwrap().1, numbers(20));
        assert!(changed(&state, S).unwrap() >= first);
        // The same numbers leave the file as it is.
        let before = changed(&state, S).unwrap();
        std::thread::sleep(Duration::from_millis(20));
        write(&state, S, &numbers(20)).unwrap();
        assert_eq!(changed(&state, S).unwrap(), before);
        // Another session's name, a file of another session, bad names.
        assert_eq!(read(&state, "other"), None);
        std::fs::copy(
            dir(&state).join(format!("{S}.json")),
            dir(&state).join("other.json"),
        )
        .unwrap();
        assert_eq!(read(&state, "other"), None);
        for bad in ["", "../x", "a/b", "a.b"] {
            assert!(write(&state, bad, &numbers(1)).is_err(), "{bad}");
            assert_eq!(read(&state, bad), None);
        }
        // No temp file is left behind.
        let names: Vec<String> = std::fs::read_dir(dir(&state))
            .unwrap()
            .map(|entry| entry.unwrap().file_name().into_string().unwrap())
            .collect();
        assert!(
            names.iter().all(|name| !name.ends_with(".tmp")),
            "{names:?}"
        );
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode =
                |path: PathBuf| std::fs::metadata(path).unwrap().permissions().mode() & 0o777;
            assert_eq!(mode(dir(&state).join(format!("{S}.json"))), 0o600);
            assert_eq!(mode(dir(&state)), 0o700);
        }
        let _ = std::fs::remove_dir_all(&state);
    }

    #[test]
    fn the_mark_says_whether_an_agent_passes_numbers_on() {
        let state = state("mark");
        let now = SystemTime::now();
        assert!(!agent_present(&state, S, now));
        mark(&state, S).unwrap();
        assert!(agent_present(&state, S, SystemTime::now()));
        assert!(!agent_present(&state, "other", SystemTime::now()));
        // Renewed in place; stale past MARK_FRESH.
        mark(&state, S).unwrap();
        assert!(!agent_present(
            &state,
            S,
            SystemTime::now() + MARK_FRESH + Duration::from_secs(1)
        ));
        write(&state, S, &numbers(1)).unwrap();
        remove(&state, S);
        assert!(!agent_present(&state, S, SystemTime::now()));
        assert_eq!(read(&state, S), None);
        let _ = std::fs::remove_dir_all(&state);
    }

    #[test]
    fn prune_removes_old_files_only() {
        let state = state("prune");
        write(&state, S, &numbers(1)).unwrap();
        mark(&state, S).unwrap();
        std::fs::write(dir(&state).join("x.1-2.tmp"), b"").unwrap();
        prune(&state, SystemTime::now());
        assert!(read(&state, S).is_some());
        assert!(dir(&state).join("x.1-2.tmp").exists());
        prune(&state, SystemTime::now() + Duration::from_secs(120));
        assert!(read(&state, S).is_some());
        assert!(!dir(&state).join("x.1-2.tmp").exists());
        prune(&state, SystemTime::now() + MAX_AGE + Duration::from_secs(1));
        assert_eq!(read(&state, S), None);
        assert!(!agent_present(&state, S, SystemTime::now()));
        let _ = std::fs::remove_dir_all(&state);
    }
}
