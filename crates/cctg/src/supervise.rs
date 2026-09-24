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

use std::ffi::OsString;
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

/// Supervisor settings.
#[derive(Debug, Clone)]
pub struct Settings {
    /// The hub binary: `<bin dir>/cctg(.exe)`.
    pub exe: PathBuf,
    /// Extra `cctg hub` arguments (`--env-file <path>`).
    pub hub_args: Vec<OsString>,
}

/// `cctg.restart` next to `exe`: a request to restart the hub now.
pub fn restart_path(exe: &Path) -> PathBuf {
    sibling(exe, "restart")
}

/// `cctg.hub-started` next to `exe`: `<pid> <unix nanos>` of the last start.
pub fn started_path(exe: &Path) -> PathBuf {
    sibling(exe, "hub-started")
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

/// Runs the hub until Ctrl+C (Ctrl+Break on Windows, SIGTERM on Unix).
pub async fn supervise(settings: Settings) -> anyhow::Result<()> {
    let mut stop = pin!(async {
        let signal = stop_signal().await;
        info!(signal, "stopping");
    });
    let restart = restart_path(&settings.exe);
    let mut backoff = MIN_BACKOFF;
    info!("supervisor started");
    loop {
        let _ = std::fs::remove_file(&restart);
        let mut hub = match start_hub(&settings) {
            Ok(hub) => hub,
            Err(error) => {
                warn!(%error, wait = ?backoff, "cannot start the hub");
                if stopped_during(stop.as_mut(), backoff).await {
                    return Ok(());
                }
                backoff = (backoff * 2).min(MAX_BACKOFF);
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
        if stopped_during(stop.as_mut(), backoff).await {
            return Ok(());
        }
        backoff = (backoff * 2).min(MAX_BACKOFF);
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

async fn stopped_during(
    stop: std::pin::Pin<&mut impl Future<Output = ()>>,
    wait: Duration,
) -> bool {
    tokio::select! {
        biased;
        () = stop => true,
        () = tokio::time::sleep(wait) => false,
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
    fn request_and_stamp_files_sit_next_to_the_binary() {
        let exe = Path::new("bin").join("cctg.exe");
        assert_eq!(restart_path(&exe), Path::new("bin").join("cctg.restart"));
        assert_eq!(
            started_path(&exe),
            Path::new("bin").join("cctg.hub-started")
        );
        let bare = Path::new("bin").join("cctg");
        assert_eq!(restart_path(&bare), Path::new("bin").join("cctg.restart"));
    }
}
