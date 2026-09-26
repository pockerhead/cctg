//! `cctg deploy` (TASK-026; since TASK-040 it installs by itself).
//!
//! `cctg deploy <exe>` checks the new binary (`--version` answers `cctg ...`,
//! its bytes differ from the running `cctg(.exe)`), renames the running
//! binary to `cctg.old` (Windows lets a running exe be renamed, not
//! replaced), puts the new one in under a `.part` name first and then as
//! `cctg(.exe)`, and asks the supervisor ([`crate::supervise`]) for a hub
//! restart with `cctg.restart`. It then watches `cctg.hub-started`: a new
//! stamp means the new hub runs; another new stamp within the trial means it
//! exited and was restarted, and the deploy rolls back (the new binary to
//! `cctg.bad`, `cctg.old` back) and asks for one more restart. No new stamp
//! in time (no supervisor) rolls the files back too. One deploy at a time: an
//! OS lock on `cctg.deploy-lock`.
//!
//! The rollback needs this process to wait out the trial (it does, and says
//! how it ended). An old or bad binary that is still running (the
//! supervisor, the hub, agents started before) cannot be deleted; it is
//! renamed aside with a timestamp and deleted by a later deploy once nothing
//! runs it.

use std::io;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use anyhow::{Context, bail};
use tokio::process::Command;

use crate::supervise;

/// How long a new hub must keep running before a deploy counts as done.
pub const DEFAULT_TRIAL: Duration = Duration::from_secs(10);
/// How long `cctg deploy` waits for the supervisor to start a hub.
pub const DEFAULT_DEPLOY_TIMEOUT: Duration = Duration::from_secs(120);
/// A hub that exits in its trial is started again after the supervisor's
/// 1 s backoff; the trial looks this much longer.
const RESTART_SLACK: Duration = Duration::from_secs(2);
const VERSION_WAIT: Duration = Duration::from_secs(10);
const STAMP_POLL: Duration = Duration::from_millis(200);
/// A scanner or indexer can hold a fresh file for a moment on Windows.
const RENAME_WAITS: [Duration; 3] = [
    Duration::from_millis(100),
    Duration::from_millis(300),
    Duration::from_millis(1000),
];

/// The files of one bin directory, named after the running binary.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Files {
    /// The binary the hub runs from: `cctg(.exe)`.
    pub current: PathBuf,
    /// Where the new binary is copied before it is renamed to `current`.
    pub part: PathBuf,
    /// The binary before the last deploy: `cctg.old(.exe)`.
    pub old: PathBuf,
    /// A binary that was rolled back: `cctg.bad(.exe)`.
    pub bad: PathBuf,
    /// Held locked by a running `cctg deploy`: `cctg.deploy-lock`.
    pub lock: PathBuf,
    /// The supervisor's restart request and start stamp.
    pub restart: PathBuf,
    pub started: PathBuf,
}

impl Files {
    /// `exe_name` is the running binary's file name, such as `cctg.exe`.
    pub fn new(dir: &Path, exe_name: &str) -> Self {
        let (stem, ext) = match exe_name.rsplit_once('.') {
            Some((stem, ext)) if !stem.is_empty() => (stem, format!(".{ext}")),
            _ => (exe_name, String::new()),
        };
        let named = |tag: &str| dir.join(format!("{stem}.{tag}{ext}"));
        let current = dir.join(exe_name);
        Self {
            part: dir.join(format!("{stem}.next{ext}.part")),
            old: named("old"),
            bad: named("bad"),
            lock: dir.join(format!("{stem}.deploy-lock")),
            restart: supervise::restart_path(&current),
            started: supervise::started_path(&current),
            current,
        }
    }

    /// The files next to this executable.
    pub fn of_current_exe() -> anyhow::Result<Self> {
        let exe = std::env::current_exe().context("cannot find this executable's path")?;
        let dir = exe.parent().context("this executable has no directory")?;
        let name = exe
            .file_name()
            .and_then(|name| name.to_str())
            .context("this executable's file name is not UTF-8")?;
        Ok(Self::new(dir, name))
    }

    fn name(path: &Path) -> String {
        path.file_name()
            .map(|name| name.to_string_lossy().into_owned())
            .unwrap_or_default()
    }
}

/// What a deploy did.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Outcome {
    /// The new hub passed its trial; detail: its version line.
    Deployed(String),
    /// The new binary has the running binary's bytes; nothing changed.
    Unchanged,
    /// The new binary failed its check; nothing changed.
    Rejected(String),
    /// The new hub exited during its trial; the old binary runs again.
    RolledBack(String),
    /// The swap or the restart failed; the detail says what runs now.
    Failed(String),
}

impl Outcome {
    pub fn is_success(&self) -> bool {
        matches!(self, Self::Deployed(_) | Self::Unchanged)
    }
}

impl std::fmt::Display for Outcome {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Deployed(version) => write!(f, "deployed: {version}"),
            Self::Unchanged => f.write_str("unchanged: the candidate is the running binary"),
            Self::Rejected(detail) => write!(f, "rejected: {detail}"),
            Self::RolledBack(detail) => write!(f, "rolled back: {detail}"),
            Self::Failed(detail) => write!(f, "failed: {detail}"),
        }
    }
}

/// Installs `exe` and waits out its trial.
pub async fn deploy(
    exe: &Path,
    files: &Files,
    timeout: Duration,
    trial: Duration,
) -> anyhow::Result<Outcome> {
    // An OS lock: released when this process ends, however it ends.
    let lock = std::fs::OpenOptions::new()
        .create(true)
        .truncate(false)
        .write(true)
        .open(&files.lock)
        .context("cannot open the deploy lock")?;
    match lock.try_lock() {
        Ok(()) => {}
        Err(std::fs::TryLockError::WouldBlock) => {
            bail!("another cctg deploy is running from this bin directory")
        }
        Err(std::fs::TryLockError::Error(error)) => {
            return Err(error).context("cannot lock the deploy lock");
        }
    }
    if !exe.is_file() {
        bail!("{} is not a file", exe.display());
    }
    let version = match check(exe, files).await {
        Ok(Some(version)) => version,
        Ok(None) => return Ok(Outcome::Unchanged),
        Err(reason) => return Ok(Outcome::Rejected(reason)),
    };
    sweep(files);
    std::fs::copy(exe, &files.part).context("cannot copy the binary into the bin directory")?;
    if let Err(reason) = swap_in(files) {
        let _ = std::fs::remove_file(&files.part);
        return Ok(Outcome::Failed(format!("{reason}; the old binary runs")));
    }
    let before = stamp(files);
    if !restarted(files, before.as_deref(), timeout).await {
        let _ = std::fs::remove_file(&files.restart);
        let back = restore(files);
        return Ok(Outcome::Failed(format!(
            "no supervisor started the hub within {} s (is `cctg supervise` running from this bin directory?); {back}",
            timeout.as_secs()
        )));
    }
    let first = stamp(files);
    tokio::time::sleep(trial + RESTART_SLACK).await;
    if stamp(files) == first {
        return Ok(Outcome::Deployed(version));
    }
    let reason = format!("the new hub exited within its {} s trial", trial.as_secs());
    Ok(match roll_back(files) {
        Ok(()) => {
            let now = stamp(files);
            if restarted(files, now.as_deref(), timeout).await {
                Outcome::RolledBack(format!(
                    "{reason}; the old binary runs again, the new one is kept as {}",
                    Files::name(&files.bad)
                ))
            } else {
                Outcome::Failed(format!(
                    "{reason}; the old binary is back but no hub restarted"
                ))
            }
        }
        Err(error) => Outcome::Failed(format!("{reason}; rollback failed: {error}")),
    })
}

/// `Ok(Some(version line))` for a runnable binary with other bytes than the
/// running one, `Ok(None)` for the running binary's bytes.
async fn check(exe: &Path, files: &Files) -> Result<Option<String>, String> {
    let run = Command::new(exe)
        .arg("--version")
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .kill_on_drop(true)
        .output();
    let output = tokio::time::timeout(VERSION_WAIT, run)
        .await
        .map_err(|_| format!("--version did not answer in {} s", VERSION_WAIT.as_secs()))?
        .map_err(|error| format!("cannot run it: {error}"))?;
    if !output.status.success() {
        return Err(format!(
            "--version exited ({})",
            supervise::describe(Some(output.status))
        ));
    }
    let text = String::from_utf8_lossy(&output.stdout);
    let version = text.lines().next().unwrap_or_default().trim();
    if !version.starts_with("cctg ") {
        return Err("--version printed no cctg version".to_owned());
    }
    let version: String = version.chars().take(80).collect();
    let (new, current) = (exe.to_owned(), files.current.clone());
    let same = tokio::task::spawn_blocking(move || {
        let current = match std::fs::read(current) {
            Ok(bytes) => bytes,
            // A half-failed rollback: any runnable binary is better.
            Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(false),
            Err(error) => return Err(error),
        };
        Ok::<_, io::Error>(std::fs::read(new)? == current)
    })
    .await
    .map_err(|_| "compare worker failed".to_owned())?
    .map_err(|error| format!("cannot compare with the running binary: {error}"))?;
    Ok((!same).then_some(version))
}

/// The supervisor's last start stamp.
fn stamp(files: &Files) -> Option<String> {
    std::fs::read_to_string(&files.started).ok()
}

/// Asks the supervisor for a restart and waits for a start stamp other than
/// `before`.
async fn restarted(files: &Files, before: Option<&str>, timeout: Duration) -> bool {
    if std::fs::write(&files.restart, b"").is_err() {
        return false;
    }
    let deadline = Instant::now() + timeout;
    while Instant::now() < deadline {
        if stamp(files).as_deref() != before && stamp(files).is_some() {
            return true;
        }
        tokio::time::sleep(STAMP_POLL).await;
    }
    false
}

/// `current` -> `old`, `part` -> `current`; on failure `current` is back.
/// Without a `current` (a half-failed rollback) `old` is the binary to keep
/// and is left alone.
pub(crate) fn swap_in(files: &Files) -> Result<(), String> {
    let had_current = files.current.exists();
    if had_current {
        set_aside(&files.old)?;
        rename(&files.current, &files.old)
            .map_err(|error| format!("cannot move the running binary aside: {error}"))?;
    }
    if let Err(error) = rename(&files.part, &files.current) {
        if had_current {
            let _ = rename(&files.old, &files.current);
        }
        return Err(format!("cannot move the new binary in: {error}"));
    }
    Ok(())
}

/// Undoes [`swap_in`] when no hub ever ran the new binary.
fn restore(files: &Files) -> String {
    let back = set_aside(&files.bad)
        .and_then(|()| rename(&files.current, &files.bad).map_err(|error| error.to_string()))
        .and_then(|()| rename(&files.old, &files.current).map_err(|error| error.to_string()));
    match back {
        Ok(()) => "the old binary is back".to_owned(),
        Err(error) => format!("putting the old binary back failed: {error}"),
    }
}

/// `current` -> `bad`, `old` -> `current`.
fn roll_back(files: &Files) -> Result<(), String> {
    set_aside(&files.bad)?;
    rename(&files.current, &files.bad)
        .map_err(|error| format!("cannot move the new binary aside: {error}"))?;
    rename(&files.old, &files.current)
        .map_err(|error| format!("cannot restore the old binary: {error}"))
}

/// Deletes `path`, or renames it to `<name>.<nanos>` when it cannot be
/// deleted (a running binary). Nothing to do when it does not exist.
fn set_aside(path: &Path) -> Result<(), String> {
    if !path.exists() || std::fs::remove_file(path).is_ok() {
        return Ok(());
    }
    rename(path, &aside_name(path)).map_err(|error| {
        format!(
            "cannot delete or rename the previous {}: {error}",
            Files::name(path)
        )
    })
}

fn aside_name(path: &Path) -> PathBuf {
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |since| since.as_nanos());
    path.with_file_name(format!("{}.{nanos}", Files::name(path)))
}

/// Deletes binaries set aside by earlier deploys that nothing runs any more.
pub(crate) fn sweep(files: &Files) {
    let Some(dir) = files.current.parent() else {
        return;
    };
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    let prefixes = [&files.old, &files.bad].map(|path| format!("{}.", Files::name(path)));
    for entry in entries.flatten() {
        let name = entry.file_name().to_string_lossy().into_owned();
        let aside = prefixes.iter().any(|prefix| {
            name.strip_prefix(prefix.as_str())
                .is_some_and(|nanos| !nanos.is_empty() && nanos.bytes().all(|b| b.is_ascii_digit()))
        });
        if aside {
            let _ = std::fs::remove_file(entry.path());
        }
    }
}

pub(crate) fn rename(from: &Path, to: &Path) -> io::Result<()> {
    let mut result = std::fs::rename(from, to);
    for wait in RENAME_WAITS {
        match &result {
            Ok(()) => return Ok(()),
            Err(error) if error.kind() == io::ErrorKind::NotFound => break,
            Err(_) => {}
        }
        std::thread::sleep(wait);
        result = std::fs::rename(from, to);
    }
    result
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::hub::testdir::TempDir;

    #[test]
    fn files_are_named_after_the_binary() {
        let dir = Path::new("bin");
        let files = Files::new(dir, "cctg.exe");
        assert_eq!(files.current, dir.join("cctg.exe"));
        assert_eq!(files.part, dir.join("cctg.next.exe.part"));
        assert_eq!(files.old, dir.join("cctg.old.exe"));
        assert_eq!(files.bad, dir.join("cctg.bad.exe"));
        assert_eq!(files.lock, dir.join("cctg.deploy-lock"));
        assert_eq!(files.restart, dir.join("cctg.restart"));
        assert_eq!(files.started, dir.join("cctg.hub-started"));
        let files = Files::new(dir, "cctg");
        assert_eq!(files.part, dir.join("cctg.next.part"));
        assert_eq!(files.old, dir.join("cctg.old"));
    }

    #[test]
    fn swap_and_roll_back_move_the_right_files() {
        let dir = TempDir::new("deploy-swap");
        let files = Files::new(dir.path(), "cctg.exe");
        std::fs::write(&files.current, "one").unwrap();
        std::fs::write(&files.old, "zero").unwrap();
        std::fs::write(&files.part, "two").unwrap();
        swap_in(&files).unwrap();
        let read = |path: &Path| std::fs::read_to_string(path).ok();
        assert_eq!(read(&files.current).as_deref(), Some("two"));
        assert_eq!(read(&files.old).as_deref(), Some("one"));
        assert!(!files.part.exists());

        roll_back(&files).unwrap();
        assert_eq!(read(&files.current).as_deref(), Some("one"));
        assert_eq!(read(&files.bad).as_deref(), Some("two"));
        assert!(!files.old.exists());

        // Nothing to move in: the running binary stays.
        assert!(swap_in(&files).is_err());
        assert_eq!(read(&files.current).as_deref(), Some("one"));
    }

    #[test]
    fn without_a_current_binary_the_old_one_is_kept() {
        let dir = TempDir::new("deploy-no-current");
        let files = Files::new(dir.path(), "cctg.exe");
        std::fs::write(&files.old, "good").unwrap();
        std::fs::write(&files.part, "fix").unwrap();
        swap_in(&files).unwrap();
        let read = |path: &Path| std::fs::read_to_string(path).ok();
        assert_eq!(read(&files.current).as_deref(), Some("fix"));
        assert_eq!(read(&files.old).as_deref(), Some("good"));
        assert_eq!(restore(&files), "the old binary is back");
        assert_eq!(read(&files.current).as_deref(), Some("good"));
    }

    #[test]
    fn set_aside_files_are_swept_and_others_are_kept() {
        let dir = TempDir::new("deploy-sweep");
        let files = Files::new(dir.path(), "cctg.exe");
        let aside = aside_name(&files.old);
        assert!(Files::name(&aside).starts_with("cctg.old.exe."));
        for path in [&aside, &aside_name(&files.bad), &files.current, &files.old] {
            std::fs::write(path, "x").unwrap();
        }
        let keep = dir.path().join("cctg.old.exe.notes");
        std::fs::write(&keep, "x").unwrap();
        sweep(&files);
        let mut left: Vec<String> = std::fs::read_dir(dir.path())
            .unwrap()
            .map(|entry| entry.unwrap().file_name().to_string_lossy().into_owned())
            .collect();
        left.sort();
        assert_eq!(left, ["cctg.exe", "cctg.old.exe", "cctg.old.exe.notes"]);
    }

    #[tokio::test]
    async fn a_restart_counts_only_with_a_new_stamp() {
        let dir = TempDir::new("deploy-stamp");
        let files = Files::new(dir.path(), "cctg.exe");
        std::fs::write(&files.started, "1 1\n").unwrap();
        assert!(!restarted(&files, Some("1 1\n"), Duration::from_millis(300)).await);
        assert!(files.restart.exists(), "the request waits for a supervisor");
        let fake = files.clone();
        let supervisor = tokio::spawn(async move {
            while !fake.restart.exists() {
                tokio::time::sleep(Duration::from_millis(20)).await;
            }
            std::fs::remove_file(&fake.restart).unwrap();
            std::fs::write(&fake.started, "2 2\n").unwrap();
        });
        std::fs::remove_file(&files.restart).unwrap();
        assert!(restarted(&files, Some("1 1\n"), Duration::from_secs(10)).await);
        supervisor.await.unwrap();
    }

    #[tokio::test]
    async fn deploy_runs_alone_and_refuses_what_is_no_file() {
        let dir = TempDir::new("deploy-lock");
        let files = Files::new(dir.path(), "cctg.exe");
        let missing = dir.path().join("missing.exe");
        let error = deploy(&missing, &files, Duration::from_millis(300), Duration::ZERO)
            .await
            .unwrap_err();
        assert!(error.to_string().contains("is not a file"), "{error}");
        let lock = std::fs::File::open(&files.lock).unwrap();
        lock.lock().unwrap();
        let error = deploy(&missing, &files, Duration::from_millis(300), Duration::ZERO)
            .await
            .unwrap_err();
        assert!(error.to_string().contains("another cctg deploy"), "{error}");
    }
}
