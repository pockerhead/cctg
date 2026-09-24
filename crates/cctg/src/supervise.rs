//! `cctg supervise` and `cctg deploy` (TASK-026).
//!
//! The supervisor keeps `cctg hub` running from `<bin dir>/cctg(.exe)`, the
//! file it was started from: it restarts a hub that exits (backoff 1 s up to
//! 60 s, reset after a minute of uptime) and stops it with itself on Ctrl+C.
//! The hub runs with `--stop-on-stdin` in its own process group: Ctrl+C in
//! the console reaches only the supervisor, which stops the hub by closing
//! its stdin; the hub then writes its registry and exits. If the supervisor
//! dies, the pipe closes too.
//!
//! Deploy: `cctg deploy <exe>` copies the new binary to `cctg.next(.exe)`
//! next to the running one (under a `.part` name first, then renamed, so the
//! supervisor never sees half a file) and waits for `cctg.deploy-result`
//! carrying the id it wrote to `cctg.deploy-id`. One deploy at a time: it
//! holds an OS lock on `cctg.deploy-lock`. A candidate that is not installed
//! (unchanged, rejected, swap failed) leaves the `next` name at once; one
//! that cannot be removed is not taken again until the file changes.
//! The supervisor checks the candidate (`--version` answers `cctg ...`, the
//! bytes differ from the running binary), stops the hub, renames the running
//! binary to `cctg.old` (Windows lets a running exe be renamed, not
//! replaced), moves the candidate in, starts the hub and watches it for the
//! trial period. A hub that exits within it is rolled back: its binary goes
//! to `cctg.bad`, `cctg.old` comes back and the hub restarts from it.
//!
//! An old or bad binary that is still running (the supervisor itself, agents
//! started before a deploy) cannot be deleted; it is renamed aside with a
//! timestamp and deleted by a later deploy once nothing runs it. The
//! supervisor's own code changes only when it is restarted.
//!
//! Logs name files, statuses and versions only; the supervisor never reads
//! the hub's env file.

use std::ffi::OsString;
use std::io;
use std::path::{Path, PathBuf};
use std::pin::pin;
use std::process::{ExitStatus, Stdio};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use anyhow::{Context, bail};
use tokio::process::{Child, ChildStdin, Command};
use tracing::{info, warn};

/// How long a new hub must keep running before a deploy counts as done.
pub const DEFAULT_TRIAL: Duration = Duration::from_secs(10);
/// How long `cctg deploy` waits for the supervisor's result.
pub const DEFAULT_DEPLOY_TIMEOUT: Duration = Duration::from_secs(120);
const CANDIDATE_POLL: Duration = Duration::from_secs(1);
const MIN_BACKOFF: Duration = Duration::from_secs(1);
const MAX_BACKOFF: Duration = Duration::from_secs(60);
/// A hub that ran this long counts as healthy: the backoff starts over.
const HEALTHY: Duration = Duration::from_secs(60);
/// A stopping hub gets this long to write its registry before it is killed.
const STOP_WAIT: Duration = Duration::from_secs(30);
const VERSION_WAIT: Duration = Duration::from_secs(10);
const RESULT_POLL: Duration = Duration::from_millis(200);
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
    /// The candidate a deploy puts down: `cctg.next(.exe)`.
    pub next: PathBuf,
    /// Where `cctg deploy` writes the candidate before renaming it to `next`.
    pub part: PathBuf,
    /// The binary before the last deploy: `cctg.old(.exe)`.
    pub old: PathBuf,
    /// A candidate that was rolled back: `cctg.bad(.exe)`.
    pub bad: PathBuf,
    /// The supervisor's answer to a deploy: `cctg.deploy-result`.
    pub result: PathBuf,
    /// The waiting candidate's deploy id, echoed in its result: `cctg.deploy-id`.
    pub id: PathBuf,
    /// Held locked by a running `cctg deploy`: `cctg.deploy-lock`.
    pub lock: PathBuf,
}

impl Files {
    /// `exe_name` is the running binary's file name, such as `cctg.exe`.
    pub fn new(dir: &Path, exe_name: &str) -> Self {
        let (stem, ext) = match exe_name.rsplit_once('.') {
            Some((stem, ext)) if !stem.is_empty() => (stem, format!(".{ext}")),
            _ => (exe_name, String::new()),
        };
        let named = |tag: &str| dir.join(format!("{stem}.{tag}{ext}"));
        Self {
            current: dir.join(exe_name),
            next: named("next"),
            part: dir.join(format!("{stem}.next{ext}.part")),
            old: named("old"),
            bad: named("bad"),
            result: dir.join(format!("{stem}.deploy-result")),
            id: dir.join(format!("{stem}.deploy-id")),
            lock: dir.join(format!("{stem}.deploy-lock")),
        }
    }

    /// The files next to this executable. Read once at start: after a deploy
    /// the running image has another name.
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

/// What the supervisor did with a candidate.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Outcome {
    /// The new hub passed its trial; detail: its version line.
    Deployed(String),
    /// The candidate has the running binary's bytes; nothing changed.
    Unchanged,
    /// The candidate failed its check; the running hub was not touched.
    Rejected(String),
    /// The new hub exited during its trial; the old binary runs again.
    RolledBack(String),
    /// The swap itself failed; the detail says what runs now.
    Failed(String),
}

impl Outcome {
    pub fn is_success(&self) -> bool {
        matches!(self, Self::Deployed(_) | Self::Unchanged)
    }

    /// Three lines: a tag, the deploy id and a detail.
    pub fn encode(&self, id: &str) -> String {
        let (tag, detail) = match self {
            Self::Deployed(detail) => ("deployed", detail.as_str()),
            Self::Unchanged => ("unchanged", ""),
            Self::Rejected(detail) => ("rejected", detail.as_str()),
            Self::RolledBack(detail) => ("rolled-back", detail.as_str()),
            Self::Failed(detail) => ("failed", detail.as_str()),
        };
        format!("{tag}\n{id}\n{}\n", detail.replace(['\r', '\n'], " "))
    }

    /// The deploy id and the outcome.
    pub fn decode(text: &str) -> Option<(String, Self)> {
        let mut lines = text.splitn(3, '\n');
        let (tag, id) = (lines.next()?, lines.next()?);
        let detail = lines.next().unwrap_or_default().trim().to_owned();
        let outcome = match tag.trim() {
            "deployed" => Self::Deployed(detail),
            "unchanged" => Self::Unchanged,
            "rejected" => Self::Rejected(detail),
            "rolled-back" => Self::RolledBack(detail),
            "failed" => Self::Failed(detail),
            _ => return None,
        };
        Some((id.trim().to_owned(), outcome))
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

/// Supervisor settings.
#[derive(Debug, Clone)]
pub struct Settings {
    pub files: Files,
    /// Extra `cctg hub` arguments (`--env-file <path>`).
    pub hub_args: Vec<OsString>,
    pub trial: Duration,
}

struct Running {
    child: Child,
    /// Kept apart from `child`: `Child::wait` closes the stdin it holds,
    /// which would stop the hub.
    stdin: Option<ChildStdin>,
    since: Instant,
}

enum Event {
    Stop,
    Exited(io::Result<ExitStatus>),
    Tick,
}

/// Runs the hub until Ctrl+C (Ctrl+Break on Windows, SIGTERM on Unix).
pub async fn supervise(settings: Settings) -> anyhow::Result<()> {
    let mut stop = pin!(async {
        let signal = stop_signal().await;
        info!(signal, "stopping");
    });
    let mut backoff = MIN_BACKOFF;
    let mut hub: Option<Running> = None;
    let mut stuck: Stamp = None;
    info!(
        exe = Files::name(&settings.files.current),
        "supervisor started"
    );
    loop {
        let Some(running) = hub.as_mut() else {
            // A hub that fails at once never lives to a tick: a candidate
            // (maybe its fix) is taken before each start too.
            if waiting(&settings.files, stuck) {
                hub = take_candidate(&settings, None, &mut backoff, &mut stuck).await;
                if hub.is_some() {
                    continue;
                }
            }
            match start_hub(&settings) {
                Ok(running) => hub = Some(running),
                Err(error) => {
                    warn!(%error, wait = ?backoff, "cannot start the hub");
                    if stopped_during(stop.as_mut(), backoff).await {
                        return Ok(());
                    }
                    backoff = (backoff * 2).min(MAX_BACKOFF);
                }
            }
            continue;
        };
        let event = tokio::select! {
            biased;
            () = stop.as_mut() => Event::Stop,
            status = running.child.wait() => Event::Exited(status),
            () = tokio::time::sleep(CANDIDATE_POLL) => Event::Tick,
        };
        match event {
            Event::Stop => {
                info!("stopping the hub");
                let status = stop_hub(running).await;
                info!(status = %describe(status), "supervisor stopped");
                return Ok(());
            }
            Event::Exited(status) => {
                if running.since.elapsed() >= HEALTHY {
                    backoff = MIN_BACKOFF;
                }
                warn!(status = %describe(status.ok()), wait = ?backoff, "hub exited; restarting it");
                hub = None;
                if stopped_during(stop.as_mut(), backoff).await {
                    return Ok(());
                }
                backoff = (backoff * 2).min(MAX_BACKOFF);
            }
            Event::Tick if waiting(&settings.files, stuck) => {
                hub = take_candidate(&settings, hub.take(), &mut backoff, &mut stuck).await;
            }
            Event::Tick => {}
        }
    }
}

/// Size and modification time of a candidate that could not be moved away.
type Stamp = Option<(u64, SystemTime)>;

fn stamp(path: &Path) -> Stamp {
    let meta = std::fs::metadata(path).ok()?;
    Some((meta.len(), meta.modified().ok()?))
}

/// A candidate waits, and it is not the one already refused (`stuck`).
fn waiting(files: &Files, stuck: Stamp) -> bool {
    files.next.is_file() && (stuck.is_none() || stamp(&files.next) != stuck)
}

/// [`install`], then logs and reports the outcome under the candidate's
/// deploy id.
async fn take_candidate(
    settings: &Settings,
    hub: Option<Running>,
    backoff: &mut Duration,
    stuck: &mut Stamp,
) -> Option<Running> {
    let files = &settings.files;
    let id: String = std::fs::read_to_string(&files.id)
        .unwrap_or_default()
        .trim()
        .chars()
        .take(64)
        .collect();
    let _ = std::fs::remove_file(&files.id);
    let (after, outcome) = install(settings, hub).await;
    if outcome.is_success() {
        info!(%outcome, "deploy finished");
        *backoff = MIN_BACKOFF;
    } else {
        warn!(%outcome, "deploy finished");
    }
    *stuck = None;
    if files.next.exists() {
        // Never taken again as it is: no restart loop, no late install.
        *stuck = stamp(&files.next);
        warn!(
            candidate = Files::name(&files.next),
            "cannot remove the refused candidate; delete it by hand"
        );
    }
    report(files, &id, &outcome);
    after
}

/// Takes the candidate. Returns the hub that runs afterwards (`None`: the
/// supervisor starts one from `current`) and what happened.
async fn install(settings: &Settings, hub: Option<Running>) -> (Option<Running>, Outcome) {
    let files = &settings.files;
    sweep(files);
    let version = match check_candidate(files).await {
        Ok(Some(version)) => version,
        Ok(None) => {
            withdraw(files);
            return (hub, Outcome::Unchanged);
        }
        Err(reason) => {
            withdraw(files);
            return (hub, Outcome::Rejected(reason));
        }
    };
    info!(%version, "installing a new hub binary");
    if let Some(mut running) = hub {
        let status = stop_hub(&mut running).await;
        info!(status = %describe(status), "hub stopped for the deploy");
    }
    if let Err(reason) = swap_in(files) {
        withdraw(files);
        return (
            None,
            Outcome::Failed(format!("{reason}; the old binary runs")),
        );
    }
    let mut new = match start_hub(settings) {
        Ok(new) => new,
        Err(error) => return (None, roll_back(files, &format!("cannot start: {error}"))),
    };
    tokio::select! {
        status = new.child.wait() => {
            let reason = format!(
                "the new hub exited ({}) within its {} s trial",
                describe(status.ok()),
                settings.trial.as_secs()
            );
            (None, roll_back(files, &reason))
        }
        () = tokio::time::sleep(settings.trial) => (Some(new), Outcome::Deployed(version)),
    }
}

/// `Ok(Some(version line))` for a runnable candidate with other bytes,
/// `Ok(None)` for the running binary's bytes.
async fn check_candidate(files: &Files) -> Result<Option<String>, String> {
    let run = Command::new(&files.next)
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
            describe(Some(output.status))
        ));
    }
    let text = String::from_utf8_lossy(&output.stdout);
    let version = text.lines().next().unwrap_or_default().trim();
    if !version.starts_with("cctg ") {
        return Err("--version printed no cctg version".to_owned());
    }
    let version: String = version.chars().take(80).collect();
    let (next, current) = (files.next.clone(), files.current.clone());
    let same = tokio::task::spawn_blocking(move || {
        let current = match std::fs::read(current) {
            Ok(bytes) => bytes,
            // A half-failed rollback: any runnable candidate is better.
            Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(false),
            Err(error) => return Err(error),
        };
        Ok::<_, io::Error>(std::fs::read(next)? == current)
    })
    .await
    .map_err(|_| "compare worker failed".to_owned())?
    .map_err(|error| format!("cannot compare with the running binary: {error}"))?;
    Ok((!same).then_some(version))
}

/// `current` -> `old`, `next` -> `current`; on failure `current` is back.
/// Without a `current` (a half-failed rollback) `old` is the binary to keep
/// and is left alone.
fn swap_in(files: &Files) -> Result<(), String> {
    let had_current = files.current.exists();
    if had_current {
        set_aside(&files.old)?;
        rename(&files.current, &files.old)
            .map_err(|error| format!("cannot move the running binary aside: {error}"))?;
    }
    if let Err(error) = rename(&files.next, &files.current) {
        if had_current {
            let _ = rename(&files.old, &files.current);
        }
        return Err(format!("cannot move the candidate in: {error}"));
    }
    Ok(())
}

/// Moves a candidate that was not installed out of the `next` name: deleted,
/// or renamed aside like a bad binary (a later sweep deletes it).
fn withdraw(files: &Files) {
    if files.next.exists() && std::fs::remove_file(&files.next).is_err() {
        let _ = rename(&files.next, &aside_name(&files.bad));
    }
}

/// `current` -> `bad`, `old` -> `current`.
fn roll_back(files: &Files, reason: &str) -> Outcome {
    let back = set_aside(&files.bad)
        .and_then(|()| {
            rename(&files.current, &files.bad)
                .map_err(|error| format!("cannot move the new binary aside: {error}"))
        })
        .and_then(|()| {
            rename(&files.old, &files.current)
                .map_err(|error| format!("cannot restore the old binary: {error}"))
        });
    match back {
        Ok(()) => Outcome::RolledBack(format!(
            "{reason}; the old binary runs again, the new one is kept as {}",
            Files::name(&files.bad)
        )),
        Err(error) => Outcome::Failed(format!("{reason}; rollback failed: {error}")),
    }
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
    path.with_file_name(format!("{}.{}", Files::name(path), aside_stamp()))
}

/// Nanoseconds since the epoch.
fn aside_stamp() -> u128 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |since| since.as_nanos())
}

/// Deletes binaries set aside by earlier deploys that nothing runs any more.
fn sweep(files: &Files) {
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

fn rename(from: &Path, to: &Path) -> io::Result<()> {
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

fn report(files: &Files, id: &str, outcome: &Outcome) {
    let tmp = files.result.with_extension("deploy-result.tmp");
    let written =
        std::fs::write(&tmp, outcome.encode(id)).and_then(|()| rename(&tmp, &files.result));
    if let Err(error) = written {
        warn!(%error, "cannot write the deploy result");
    }
}

fn start_hub(settings: &Settings) -> io::Result<Running> {
    let mut command = Command::new(&settings.files.current);
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
    info!(pid = child.id().unwrap_or_default(), "hub started");
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

fn describe(status: Option<ExitStatus>) -> String {
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

/// `cctg deploy`: puts `exe` down as the candidate and waits for the
/// supervisor's answer.
pub async fn deploy(exe: &Path, files: &Files, timeout: Duration) -> anyhow::Result<Outcome> {
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
    if files.next.exists() {
        bail!(
            "{} already waits in the bin directory: another deploy is running, or no supervisor took it (delete it to retry)",
            Files::name(&files.next)
        );
    }
    if !exe.is_file() {
        bail!("{} is not a file", exe.display());
    }
    match std::fs::remove_file(&files.result) {
        Err(error) if error.kind() != io::ErrorKind::NotFound => {
            return Err(error).context("cannot remove the previous deploy result");
        }
        _ => {}
    }
    // Echoed in the result: a late answer to an earlier deploy is not ours.
    let id = format!("{}-{}", std::process::id(), aside_stamp());
    std::fs::write(&files.id, &id).context("cannot write the deploy id")?;
    std::fs::copy(exe, &files.part).context("cannot copy the binary into the bin directory")?;
    rename(&files.part, &files.next).context("cannot put the candidate in place")?;
    let deadline = Instant::now() + timeout;
    loop {
        if let Ok(text) = std::fs::read_to_string(&files.result)
            && let Some((answered, outcome)) = Outcome::decode(&text)
            && answered == id
        {
            let _ = std::fs::remove_file(&files.result);
            return Ok(outcome);
        }
        if Instant::now() >= deadline {
            if std::fs::remove_file(&files.next).is_ok() {
                let _ = std::fs::remove_file(&files.id);
                bail!(
                    "no supervisor took the candidate within {} s; is `cctg supervise` running from this bin directory?",
                    timeout.as_secs()
                );
            }
            bail!(
                "the supervisor took the candidate but gave no result within {} s",
                timeout.as_secs()
            );
        }
        tokio::time::sleep(RESULT_POLL).await;
    }
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
        assert_eq!(files.next, dir.join("cctg.next.exe"));
        assert_eq!(files.part, dir.join("cctg.next.exe.part"));
        assert_eq!(files.old, dir.join("cctg.old.exe"));
        assert_eq!(files.bad, dir.join("cctg.bad.exe"));
        assert_eq!(files.result, dir.join("cctg.deploy-result"));
        assert_eq!(files.id, dir.join("cctg.deploy-id"));
        assert_eq!(files.lock, dir.join("cctg.deploy-lock"));
        let files = Files::new(dir, "cctg");
        assert_eq!(files.next, dir.join("cctg.next"));
        assert_eq!(files.part, dir.join("cctg.next.part"));
        assert_eq!(files.old, dir.join("cctg.old"));
    }

    #[test]
    fn outcomes_round_trip() {
        for outcome in [
            Outcome::Deployed("cctg 0.1.0".to_owned()),
            Outcome::Unchanged,
            Outcome::Rejected("cannot run it".to_owned()),
            Outcome::RolledBack("exited\nearly".to_owned()),
            Outcome::Failed(String::new()),
        ] {
            let (id, decoded) = Outcome::decode(&outcome.encode("42-7")).unwrap();
            assert_eq!(id, "42-7");
            match (&outcome, &decoded) {
                (Outcome::RolledBack(_), Outcome::RolledBack(detail)) => {
                    assert_eq!(detail, "exited early");
                }
                _ => assert_eq!(decoded, outcome),
            }
        }
        assert_eq!(Outcome::decode("what\n1\n"), None);
        assert_eq!(Outcome::decode("deployed"), None);
        assert!(Outcome::Unchanged.is_success());
        assert!(!Outcome::Rejected(String::new()).is_success());
    }

    #[test]
    fn swap_and_roll_back_move_the_right_files() {
        let dir = TempDir::new("supervise-swap");
        let files = Files::new(dir.path(), "cctg.exe");
        std::fs::write(&files.current, "one").unwrap();
        std::fs::write(&files.old, "zero").unwrap();
        std::fs::write(&files.next, "two").unwrap();
        swap_in(&files).unwrap();
        let read = |path: &Path| std::fs::read_to_string(path).ok();
        assert_eq!(read(&files.current).as_deref(), Some("two"));
        assert_eq!(read(&files.old).as_deref(), Some("one"));
        assert!(!files.next.exists());

        let outcome = roll_back(&files, "exited");
        assert!(matches!(outcome, Outcome::RolledBack(_)), "{outcome:?}");
        assert_eq!(read(&files.current).as_deref(), Some("one"));
        assert_eq!(read(&files.bad).as_deref(), Some("two"));
        assert!(!files.old.exists());

        // A missing candidate leaves the running binary where it was.
        assert!(swap_in(&files).is_err());
        assert_eq!(read(&files.current).as_deref(), Some("one"));
    }

    #[test]
    fn without_a_current_binary_the_old_one_is_kept() {
        // A rollback that failed halfway: `current` gone, `old` is the good one.
        let dir = TempDir::new("supervise-no-current");
        let files = Files::new(dir.path(), "cctg.exe");
        std::fs::write(&files.old, "good").unwrap();
        std::fs::write(&files.next, "fix").unwrap();
        swap_in(&files).unwrap();
        let read = |path: &Path| std::fs::read_to_string(path).ok();
        assert_eq!(read(&files.current).as_deref(), Some("fix"));
        assert_eq!(read(&files.old).as_deref(), Some("good"));
        let outcome = roll_back(&files, "exited");
        assert!(matches!(outcome, Outcome::RolledBack(_)), "{outcome:?}");
        assert_eq!(read(&files.current).as_deref(), Some("good"));
    }

    #[test]
    fn a_refused_candidate_leaves_the_next_name() {
        let dir = TempDir::new("supervise-withdraw");
        let files = Files::new(dir.path(), "cctg.exe");
        std::fs::write(&files.next, "refused").unwrap();
        assert!(waiting(&files, None));
        withdraw(&files);
        assert!(!files.next.exists());
        assert!(!waiting(&files, None));

        // One that could not be removed is not taken again until it changes.
        std::fs::write(&files.next, "stuck").unwrap();
        let stuck = stamp(&files.next);
        assert!(stuck.is_some());
        assert!(!waiting(&files, stuck));
        std::fs::write(&files.next, "a new build").unwrap();
        assert!(waiting(&files, stuck));
    }

    /// Holds `path` open so that it can be neither deleted nor renamed.
    #[cfg(windows)]
    fn hold(path: &Path) -> std::fs::File {
        use std::os::windows::fs::OpenOptionsExt;
        std::fs::OpenOptions::new()
            .read(true)
            .share_mode(0)
            .open(path)
            .unwrap()
    }

    #[cfg(windows)]
    #[test]
    fn a_failed_swap_keeps_the_running_binary() {
        let dir = TempDir::new("supervise-swap-fails");
        let files = Files::new(dir.path(), "cctg.exe");
        std::fs::write(&files.current, "running").unwrap();
        std::fs::write(&files.old, "held").unwrap();
        std::fs::write(&files.next, "candidate").unwrap();
        let held = hold(&files.old);
        let error = swap_in(&files).unwrap_err();
        assert!(error.contains("cctg.old.exe"), "{error}");
        let read = |path: &Path| std::fs::read_to_string(path).ok();
        assert_eq!(read(&files.current).as_deref(), Some("running"));
        withdraw(&files);
        assert!(!files.next.exists(), "the candidate is never taken again");
        drop(held);
    }

    #[test]
    fn set_aside_files_are_swept_and_others_are_kept() {
        let dir = TempDir::new("supervise-sweep");
        let files = Files::new(dir.path(), "cctg.exe");
        let aside = aside_name(&files.old);
        let name = Files::name(&aside);
        assert!(name.starts_with("cctg.old.exe."), "{name}");
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
    async fn deploy_without_a_supervisor_withdraws_its_candidate() {
        let dir = TempDir::new("supervise-deploy-alone");
        let files = Files::new(dir.path(), "cctg.exe");
        let exe = dir.path().join("built.exe");
        std::fs::write(&exe, "new").unwrap();
        let error = deploy(&exe, &files, Duration::from_millis(300))
            .await
            .unwrap_err();
        assert!(error.to_string().contains("no supervisor"), "{error}");
        assert!(!files.next.exists());
        assert!(!files.part.exists());

        std::fs::write(&files.next, "pending").unwrap();
        let error = deploy(&exe, &files, Duration::from_millis(300))
            .await
            .unwrap_err();
        assert!(error.to_string().contains("already waits"), "{error}");
    }

    #[tokio::test]
    async fn deploy_takes_only_its_own_result_and_runs_alone() {
        let dir = TempDir::new("supervise-deploy-id");
        let files = Files::new(dir.path(), "cctg.exe");
        let exe = dir.path().join("built.exe");
        std::fs::write(&exe, "new").unwrap();
        // A stand-in supervisor: a late answer to an earlier deploy first,
        // then the answer under the id this deploy wrote.
        let fake = files.clone();
        let supervisor = tokio::spawn(async move {
            while !fake.next.exists() {
                tokio::time::sleep(Duration::from_millis(20)).await;
            }
            let id = std::fs::read_to_string(&fake.id).unwrap();
            std::fs::remove_file(&fake.id).unwrap();
            std::fs::write(&fake.result, Outcome::Unchanged.encode("earlier")).unwrap();
            tokio::time::sleep(Duration::from_millis(600)).await;
            std::fs::remove_file(&fake.next).unwrap();
            std::fs::write(
                &fake.result,
                Outcome::Deployed("cctg 9".to_owned()).encode(&id),
            )
            .unwrap();
        });
        let outcome = deploy(&exe, &files, Duration::from_secs(10)).await.unwrap();
        assert_eq!(outcome, Outcome::Deployed("cctg 9".to_owned()));
        supervisor.await.unwrap();

        let lock = std::fs::File::open(&files.lock).unwrap();
        lock.lock().unwrap();
        let error = deploy(&exe, &files, Duration::from_millis(300))
            .await
            .unwrap_err();
        assert!(error.to_string().contains("another cctg deploy"), "{error}");
        assert!(!files.next.exists());
    }
}
