//! Shared by the integration tests that start the real `cctg` binary.
//!
//! The tests often run inside a live Claude Code session on a machine with a
//! live hub. A `cctg agent` that inherits that session's
//! `CLAUDE_CODE_SESSION_ID` and reads the real `~/.cctg/device.env` registers
//! with the live hub as the developer's session (TASK-042). Every `cctg`
//! process of the tests is therefore started through [`cctg`] or
//! [`isolate`]; `tests/isolation.rs` checks that.

use std::path::{Path, PathBuf};
use std::process::{Child, Command, ExitStatus, Output, Stdio};
use std::sync::{Mutex, OnceLock, PoisonError};
use std::time::Duration;

/// Held while a process of this test binary is started (TASK-066). On macOS
/// std makes a child's pipes with `pipe` and marks them close-on-exec only
/// afterwards, and its `posix_spawn` passes on every descriptor not so
/// marked: a child that another test thread starts in between inherits,
/// say, the write end of this child's stdin, and this child sees no end of
/// input while that one lives (`cctg hook`: "hook input unreadable, too
/// large or late"). Linux makes pipes with `pipe2(O_CLOEXEC)`. Every process
/// a test starts goes through [`spawn`], [`output`] or [`status`];
/// `tests/isolation.rs` checks that.
static SPAWN: Mutex<()> = Mutex::new(());

/// [`Command::spawn`], one at a time in this test binary (see [`SPAWN`]).
#[allow(dead_code, reason = "not every test binary starts processes")]
pub fn spawn(command: &mut Command) -> std::io::Result<Child> {
    let _alone = SPAWN.lock().unwrap_or_else(PoisonError::into_inner);
    command.spawn()
}

/// [`Command::output`] started through [`spawn`]: stdin null, stdout and
/// stderr captured, as there.
#[allow(dead_code, reason = "not every test binary starts processes")]
pub fn output(command: &mut Command) -> std::io::Result<Output> {
    command
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    spawn(command)?.wait_with_output()
}

/// [`Command::status`] started through [`spawn`]: stdio inherited unless
/// set, as there.
#[allow(dead_code, reason = "not every test binary starts processes")]
pub fn status(command: &mut Command) -> std::io::Result<ExitStatus> {
    spawn(command)?.wait()
}

/// A loopback port bound and held, not listened on: a hub that is down.
/// Connects to it are refused, and no other socket (another test, a
/// parallel run) can listen on it while it is held, as it could on a port
/// bound and closed again (TASK-066). `listen` on it brings the hub up.
#[allow(dead_code, reason = "not every test binary needs a down port")]
pub fn held_port() -> (tokio::net::TcpSocket, u16) {
    let socket = tokio::net::TcpSocket::new_v4().unwrap();
    socket
        .bind(std::net::SocketAddr::from((
            std::net::Ipv4Addr::LOCALHOST,
            0,
        )))
        .unwrap();
    let port = socket.local_addr().unwrap().port();
    (socket, port)
}

/// A loopback port free a moment ago, for another process to listen on
/// (a `cctg hub` the test starts): a socket cannot be handed to it, so the
/// port is closed again first. Another socket can take it meanwhile; then
/// that process fails to bind and the test fails loudly, never quietly
/// talks to the wrong listener. A port that must stay closed is a
/// [`held_port`].
#[allow(dead_code, reason = "not every test binary starts a hub")]
pub fn free_port() -> u16 {
    std::net::TcpListener::bind((std::net::Ipv4Addr::LOCALHOST, 0))
        .unwrap()
        .local_addr()
        .unwrap()
        .port()
}

/// `cctg` with `home` as its home directory and no `CCTG_*` or `CLAUDE*`
/// variable of the environment the tests run in. A test sets what it needs
/// afterwards.
#[allow(dead_code, reason = "not every test binary starts cctg directly")]
pub fn cctg(home: &Path) -> Command {
    let mut command = Command::new(env!("CARGO_BIN_EXE_cctg"));
    isolate(&mut command, home);
    command
}

/// Removes every `CCTG_*` and `CLAUDE*` variable of this process from
/// `command` and points its home at `home`, so the developer's device config
/// and Claude Code session never reach it.
pub fn isolate(command: &mut Command, home: &Path) {
    for (name, _) in std::env::vars_os() {
        let text = name.to_string_lossy();
        if text.starts_with("CCTG_") || text.starts_with("CLAUDE") {
            command.env_remove(&name);
        }
    }
    command.env("USERPROFILE", home).env("HOME", home);
}

/// Writes a program file that can be started: on Unix a written file has no
/// execute bit (TASK-035, the first Linux run).
#[allow(dead_code, reason = "not every test binary copies cctg")]
pub fn write_program(path: &Path, bytes: &[u8]) {
    std::fs::write(path, bytes).expect("write the program");
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o755))
            .expect("make the program executable");
    }
}

/// This test process's own directory under `CARGO_TARGET_TMPDIR`. The
/// target directory is shared (worktrees, a second `cargo test`), so homes
/// named only by test would be shared by concurrent runs: one run's
/// `device.env` then points the other's hooks at the wrong hub, and a spool
/// left by a failed run is replayed into the next one (TASK-060). The first
/// call empties the directory (a dead run with the same pid may have left
/// it) and removes the directories of runs whose process is gone and that
/// are older than an hour; a live run's directory is never touched.
#[allow(dead_code, reason = "not every test binary keeps homes there")]
pub fn own_tmp() -> PathBuf {
    static OWN: OnceLock<PathBuf> = OnceLock::new();
    OWN.get_or_init(|| {
        let root = Path::new(env!("CARGO_TARGET_TMPDIR"));
        let own = std::process::id();
        if let Ok(entries) = std::fs::read_dir(root) {
            for entry in entries.flatten() {
                let Ok(pid) = entry.file_name().to_string_lossy().parse::<u32>() else {
                    continue;
                };
                let stale = entry
                    .metadata()
                    .and_then(|meta| meta.modified())
                    .is_ok_and(|at| {
                        at.elapsed()
                            .is_ok_and(|age| age > Duration::from_secs(3600))
                    });
                if pid != own && stale && !alive(pid) {
                    let _ = std::fs::remove_dir_all(entry.path());
                }
            }
        }
        let dir = root.join(own.to_string());
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("create the test process's tmp directory");
        dir
    })
    .clone()
}

/// Whether process `pid` runs; `true` when that cannot be told, so its
/// directory stays.
#[allow(dead_code, reason = "used by own_tmp")]
fn alive(pid: u32) -> bool {
    #[cfg(windows)]
    let gone = output(Command::new("tasklist").args([
        "/FI",
        &format!("PID eq {pid}"),
        "/FO",
        "CSV",
        "/NH",
    ]))
    .map(|out| {
        out.status.success()
            && !String::from_utf8_lossy(&out.stdout).contains(&format!("\"{pid}\""))
    });
    #[cfg(not(windows))]
    let gone = output(Command::new("kill").args(["-0", &pid.to_string()])).map(|out| {
        !out.status.success() && String::from_utf8_lossy(&out.stderr).contains("No such process")
    });
    !gone.unwrap_or(false)
}
