//! `cctg supervise` (TASK-026, cut down in TASK-040).
//!
//! Keeps `cctg hub` running from `<bin dir>/cctg(.exe)`, the file the
//! supervisor was started from: restarts a hub that exits (backoff 1 s up to
//! 60 s, reset after a minute of uptime), restarts it at once when
//! `cctg.restart` appears (`cctg deploy` asks so; the file is removed), and
//! stops it with itself on Ctrl+C. Each start writes `cctg.hub-started` (pid
//! and time), which is how `cctg deploy` sees that a new hub runs and whether
//! it keeps running. The hub runs with `--stop-on-stdin` in its own process
//! group: Ctrl+C in the console reaches only the supervisor, which stops the
//! hub by closing its stdin; the hub then writes its registry and exits. If
//! the supervisor dies, the pipe closes too.
//!
//! The supervisor is not updated while it runs, so it only starts, waits and
//! restarts: checking, swapping and rolling back binaries is `cctg deploy`'s
//! ([`crate::deploy`]). It never reads the hub's env file.
//!
//! For a hub started at logon without a console (TASK-046, `install.sh --hub
//! --local`): one supervisor per binary folder (`cctg.supervise-lock`, an OS
//! lock; a second one leaves at once), `cctg.stop` stops
//! hub and supervisor like Ctrl+C (the file goes once both stopped), and
//! `--log-file` takes the supervisor's and the hub's log. A restart request
//! also ends the wait between restarts.

use std::ffi::OsString;
use std::fs::File;
use std::io;
use std::path::{Path, PathBuf};
use std::pin::pin;
use std::process::{ExitStatus, Stdio};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use tokio::process::{Child, ChildStdin, Command};
use tracing::{info, warn};

const MIN_BACKOFF: Duration = Duration::from_secs(1);
const MAX_BACKOFF: Duration = Duration::from_secs(60);
/// A hub that ran this long counts as healthy: the backoff starts over.
const HEALTHY: Duration = Duration::from_secs(60);
/// A stopping hub gets this long to write its registry before it is killed.
const STOP_WAIT: Duration = Duration::from_secs(30);
const REQUEST_POLL: Duration = Duration::from_secs(1);
/// A log file larger than this is moved to `<log>.prev` at start.
const LOG_KEEP: u64 = 5 << 20;

/// Supervisor settings.
#[derive(Debug)]
pub struct Settings {
    /// The hub binary: `<bin dir>/cctg(.exe)`.
    pub exe: PathBuf,
    /// Extra `cctg hub` arguments (`--env-file <path>`).
    pub hub_args: Vec<OsString>,
    /// The hub's stderr goes here too ([`open_log`]); `None`: inherited.
    pub log: Option<File>,
}

/// `cctg.restart` next to `exe`: a request to restart the hub now.
pub fn restart_path(exe: &Path) -> PathBuf {
    sibling(exe, "restart")
}

/// `cctg.hub-started` next to `exe`: `<pid> <unix nanos>` of the last start.
pub fn started_path(exe: &Path) -> PathBuf {
    sibling(exe, "hub-started")
}

/// `cctg.stop` next to `exe`: a request to stop the hub and the supervisor.
pub fn stop_path(exe: &Path) -> PathBuf {
    sibling(exe, "stop")
}

/// `cctg.supervise-lock` next to `exe`: held by the running supervisor.
pub fn lock_path(exe: &Path) -> PathBuf {
    sibling(exe, "supervise-lock")
}

/// The supervisor lock of `exe`'s folder; `None`: another supervisor holds
/// it. Never waits: a second supervisor that waited would take over when
/// the first is stopped for good (an uninstall). Released when this
/// process ends, however it ends.
pub fn lock(exe: &Path) -> io::Result<Option<File>> {
    let file = std::fs::OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(false)
        .open(lock_path(exe))?;
    match file.try_lock() {
        Ok(()) => Ok(Some(file)),
        Err(std::fs::TryLockError::WouldBlock) => Ok(None),
        Err(std::fs::TryLockError::Error(error)) => Err(error),
    }
}

/// Opens `path` for appending, after moving a file larger than
/// [`LOG_KEEP`] to `<path>.prev`. Call it holding [`lock`]: only then does
/// nobody else write to the file.
pub fn open_log(path: &Path) -> io::Result<File> {
    if std::fs::metadata(path).is_ok_and(|meta| meta.len() > LOG_KEEP) {
        let mut prev = path.as_os_str().to_owned();
        prev.push(".prev");
        let _ = std::fs::remove_file(&prev);
        let _ = std::fs::rename(path, &prev);
    }
    std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)
}

fn sibling(exe: &Path, tag: &str) -> PathBuf {
    let stem = exe
        .file_stem()
        .map_or_else(|| "cctg".into(), |stem| stem.to_string_lossy().into_owned());
    exe.with_file_name(format!("{stem}.{tag}"))
}

struct Running {
    child: Child,
    /// Kept apart from `child`: `Child::wait` closes the stdin it holds,
    /// which would stop the hub.
    stdin: Option<ChildStdin>,
    since: Instant,
}

/// Runs the hub until Ctrl+C (Ctrl+Break on Windows, SIGTERM on Unix) or
/// `cctg.stop`, which is removed once the hub stopped.
pub async fn supervise(settings: Settings) -> anyhow::Result<()> {
    let stop_file = stop_path(&settings.exe);
    // An old request never stops a new supervisor.
    let _ = std::fs::remove_file(&stop_file);
    let result = run(&settings, &stop_file).await;
    let _ = std::fs::remove_file(&stop_file);
    result
}

async fn run(settings: &Settings, stop_file: &Path) -> anyhow::Result<()> {
    let mut stop = pin!(async {
        tokio::select! {
            signal = stop_signal() => info!(signal, "stopping"),
            () = appears(stop_file) => info!("stopping: cctg.stop"),
        }
    });
    let restart = restart_path(&settings.exe);
    let mut backoff = MIN_BACKOFF;
    info!("supervisor started");
    loop {
        let _ = std::fs::remove_file(&restart);
        let mut hub = match start_hub(settings) {
            Ok(hub) => hub,
            Err(error) => {
                warn!(%error, wait = ?backoff, "cannot start the hub");
                match wait_backoff(stop.as_mut(), backoff, &restart).await {
                    Waited::Stop => {
                        info!("supervisor stopped");
                        return Ok(());
                    }
                    Waited::Restart => backoff = MIN_BACKOFF,
                    Waited::Elapsed => backoff = (backoff * 2).min(MAX_BACKOFF),
                }
                continue;
            }
        };
        let exited = loop {
            tokio::select! {
                biased;
                () = stop.as_mut() => {
                    let status = stop_hub(&mut hub).await;
                    info!(status = %describe(status), "supervisor stopped");
                    return Ok(());
                }
                status = hub.child.wait() => break Some(status),
                () = tokio::time::sleep(REQUEST_POLL) => {
                    if std::fs::remove_file(&restart).is_ok() {
                        let status = stop_hub(&mut hub).await;
                        info!(status = %describe(status), "hub restarted on request");
                        break None;
                    }
                }
            }
        };
        let Some(status) = exited else {
            backoff = MIN_BACKOFF;
            continue;
        };
        if hub.since.elapsed() >= HEALTHY {
            backoff = MIN_BACKOFF;
        }
        warn!(status = %describe(status.ok()), wait = ?backoff, "hub exited; restarting it");
        match wait_backoff(stop.as_mut(), backoff, &restart).await {
            Waited::Stop => {
                info!("supervisor stopped");
                return Ok(());
            }
            Waited::Restart => backoff = MIN_BACKOFF,
            Waited::Elapsed => backoff = (backoff * 2).min(MAX_BACKOFF),
        }
    }
}

/// Completes once `path` exists; checks every [`REQUEST_POLL`].
async fn appears(path: &Path) {
    while !path.exists() {
        tokio::time::sleep(REQUEST_POLL).await;
    }
}

fn start_hub(settings: &Settings) -> io::Result<Running> {
    let mut command = Command::new(&settings.exe);
    command
        .arg("hub")
        .args(&settings.hub_args)
        .arg("--stop-on-stdin")
        .stdin(Stdio::piped())
        .kill_on_drop(true);
    if let Some(log) = &settings.log {
        // A file, not a terminal: no colour codes in it.
        command.stderr(log.try_clone()?).env("NO_COLOR", "1");
    }
    #[cfg(windows)]
    {
        const CREATE_NEW_PROCESS_GROUP: u32 = 0x0000_0200;
        command.creation_flags(CREATE_NEW_PROCESS_GROUP);
    }
    #[cfg(unix)]
    command.process_group(0);
    let mut child = command.spawn()?;
    let pid = child.id().unwrap_or_default();
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |since| since.as_nanos());
    if let Err(error) = std::fs::write(started_path(&settings.exe), format!("{pid} {nanos}\n")) {
        warn!(%error, "cannot write the hub start stamp");
    }
    info!(pid, "hub started");
    Ok(Running {
        stdin: child.stdin.take(),
        child,
        since: Instant::now(),
    })
}

/// Closes the hub's stdin and waits for it; kills it after [`STOP_WAIT`].
async fn stop_hub(hub: &mut Running) -> Option<ExitStatus> {
    drop(hub.stdin.take());
    let child = &mut hub.child;
    match tokio::time::timeout(STOP_WAIT, child.wait()).await {
        Ok(Ok(status)) => Some(status),
        Ok(Err(_)) | Err(_) => {
            warn!("the hub did not stop in time; killing it");
            let _ = child.kill().await;
            child.wait().await.ok()
        }
    }
}

pub(crate) fn describe(status: Option<ExitStatus>) -> String {
    match status {
        Some(status) => match status.code() {
            Some(code) => format!("exit code {code}"),
            None => "killed by a signal".to_owned(),
        },
        None => "status unknown".to_owned(),
    }
}

/// How a wait between hub starts ended.
#[derive(Debug, PartialEq)]
enum Waited {
    Stop,
    /// `cctg.restart` appeared: start the hub now (the loop removes the file).
    Restart,
    Elapsed,
}

async fn wait_backoff(
    stop: std::pin::Pin<&mut impl Future<Output = ()>>,
    wait: Duration,
    restart: &Path,
) -> Waited {
    tokio::select! {
        biased;
        () = stop => Waited::Stop,
        () = appears(restart) => Waited::Restart,
        () = tokio::time::sleep(wait) => Waited::Elapsed,
    }
}

/// Ctrl+C, and Ctrl+Break on Windows or SIGTERM on Unix; returns which one.
/// A handler that cannot be installed never fires. The hub uses it too:
/// a Ctrl+Break typed in the console reaches every process attached to it.
pub(crate) async fn stop_signal() -> &'static str {
    let ctrl_c = async {
        if tokio::signal::ctrl_c().await.is_err() {
            std::future::pending::<()>().await;
        }
    };
    #[cfg(windows)]
    let other = async {
        match tokio::signal::windows::ctrl_break() {
            Ok(mut signal) => {
                signal.recv().await;
            }
            Err(_) => std::future::pending().await,
        }
    };
    #[cfg(unix)]
    let other = async {
        use tokio::signal::unix::{SignalKind, signal};
        match signal(SignalKind::terminate()) {
            Ok(mut signal) => {
                signal.recv().await;
            }
            Err(_) => std::future::pending().await,
        }
    };
    #[cfg(not(any(windows, unix)))]
    let other = std::future::pending::<()>();
    tokio::select! {
        () = ctrl_c => "Ctrl+C",
        () = other => "stop signal",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn one_supervisor_per_folder_and_a_second_one_leaves() {
        let dir = crate::hub::testdir::TempDir::new("supervise-lock");
        let exe = dir.path().join("cctg.exe");
        let first = lock(&exe).unwrap();
        assert!(first.is_some());
        assert!(lock(&exe).unwrap().is_none());
        drop(first);
        assert!(lock(&exe).unwrap().is_some());
    }

    #[test]
    fn a_large_log_moves_aside_at_start() {
        let dir = crate::hub::testdir::TempDir::new("supervise-log");
        let log = dir.path().join("hub.log");
        std::fs::write(&log, "small\n").unwrap();
        drop(open_log(&log).unwrap());
        assert_eq!(std::fs::read_to_string(&log).unwrap(), "small\n");
        std::fs::write(&log, vec![b'x'; LOG_KEEP as usize + 1]).unwrap();
        let mut file = open_log(&log).unwrap();
        io::Write::write_all(&mut file, b"new\n").unwrap();
        drop(file);
        assert_eq!(std::fs::read_to_string(&log).unwrap(), "new\n");
        let prev = dir.path().join("hub.log.prev");
        assert_eq!(std::fs::metadata(prev).unwrap().len(), LOG_KEEP + 1);
    }

    #[tokio::test(start_paused = true)]
    async fn a_restart_request_or_a_stop_ends_the_backoff() {
        let dir = crate::hub::testdir::TempDir::new("supervise-backoff");
        let restart = dir.path().join("cctg.restart");
        let wait = Duration::from_secs(8);

        std::fs::write(&restart, "").unwrap();
        let begin = tokio::time::Instant::now();
        let waited = wait_backoff(pin!(std::future::pending::<()>()), wait, &restart).await;
        assert_eq!(waited, Waited::Restart);
        assert!(begin.elapsed() < REQUEST_POLL);

        assert_eq!(
            wait_backoff(pin!(async {}), wait, &restart).await,
            Waited::Stop,
            "a stop wins over a restart request"
        );

        std::fs::remove_file(&restart).unwrap();
        let begin = tokio::time::Instant::now();
        let waited = wait_backoff(pin!(std::future::pending::<()>()), wait, &restart).await;
        assert_eq!(waited, Waited::Elapsed);
        assert_eq!(begin.elapsed(), wait);
    }

    #[test]
    fn request_and_stamp_files_sit_next_to_the_binary() {
        let exe = Path::new("bin").join("cctg.exe");
        assert_eq!(restart_path(&exe), Path::new("bin").join("cctg.restart"));
        assert_eq!(
            started_path(&exe),
            Path::new("bin").join("cctg.hub-started")
        );
        let bare = Path::new("bin").join("cctg");
        assert_eq!(restart_path(&bare), Path::new("bin").join("cctg.restart"));
        assert_eq!(stop_path(&exe), Path::new("bin").join("cctg.stop"));
        assert_eq!(
            lock_path(&exe),
            Path::new("bin").join("cctg.supervise-lock")
        );
    }
}
