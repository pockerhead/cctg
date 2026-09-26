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
/// left by a failed run is replayed into the next one (TASK-060). Folders
/// of runs older than an hour are removed on the way.
#[allow(dead_code, reason = "not every test binary keeps homes there")]
pub fn own_tmp() -> PathBuf {
    let root = Path::new(env!("CARGO_TARGET_TMPDIR"));
    let own = std::process::id().to_string();
    if let Ok(entries) = std::fs::read_dir(root) {
        for entry in entries.flatten() {
            let name = entry.file_name();
            let name = name.to_string_lossy();
            let stale = entry
                .metadata()
                .and_then(|meta| meta.modified())
                .is_ok_and(|at| {
                    at.elapsed()
                        .is_ok_and(|age| age > Duration::from_secs(3600))
                });
            if name != own && name.bytes().all(|b| b.is_ascii_digit()) && stale {
                let _ = std::fs::remove_dir_all(entry.path());
            }
        }
    }
    root.join(own)
}
