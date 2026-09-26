//! Shared by the integration tests that start the real `cctg` binary.
//!
//! The tests often run inside a live Claude Code session on a machine with a
//! live hub. A `cctg agent` that inherits that session's
//! `CLAUDE_CODE_SESSION_ID` and reads the real `~/.cctg/device.env` registers
//! with the live hub as the developer's session (TASK-042). Every `cctg`
//! process of the tests is therefore started through [`cctg`] or
//! [`isolate`]; `tests/isolation.rs` checks that.

use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::OnceLock;
use std::time::Duration;

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
    let gone = Command::new("tasklist")
        .args(["/FI", &format!("PID eq {pid}"), "/FO", "CSV", "/NH"])
        .output()
        .map(|out| {
            out.status.success()
                && !String::from_utf8_lossy(&out.stdout).contains(&format!("\"{pid}\""))
        });
    #[cfg(not(windows))]
    let gone = Command::new("kill")
        .args(["-0", &pid.to_string()])
        .output()
        .map(|out| {
            !out.status.success()
                && String::from_utf8_lossy(&out.stderr).contains("No such process")
        });
    !gone.unwrap_or(false)
}
