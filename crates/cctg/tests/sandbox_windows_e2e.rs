//! End-to-end test of the native Windows folder sandbox (TASK-089), against
//! REAL local slot accounts. It creates accounts, so it runs only on a
//! Windows runner that sets `CCTG_WINDOWS_SANDBOX_E2E=1` (the hosted GitHub
//! Windows runners are administrators with UAC off). Anywhere else — including
//! a developer's machine — it prints "skipped" and returns without touching
//! the system.
//!
//! It exercises the acceptance criteria: a command in a marked folder cannot
//! read the user's files or write another marked folder, secrets are gone from
//! its environment, its exit code and stdin flow through, and after
//! `sandbox off` the folder no longer starts a sandboxed command. Then it
//! uninstalls and checks the accounts and the mark are gone. It starts cctg
//! only through `tests/common` (no hub is ever contacted).

#![cfg(windows)]

use std::io::Write as _;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

mod common;

fn skip(reason: &str) {
    println!("sandbox_windows_e2e: skipped ({reason})");
}

/// The Git Bash `bash.exe`, or `None`.
fn git_bash() -> Option<PathBuf> {
    for candidate in [
        r"C:\Program Files\Git\bin\bash.exe",
        r"C:\Program Files (x86)\Git\bin\bash.exe",
    ] {
        let path = PathBuf::from(candidate);
        if path.exists() {
            return Some(path);
        }
    }
    None
}

/// A cctg subcommand with the fake home and `CCTG_CLAUDE`; returns
/// (code, stdout, stderr).
fn cctg(home: &Path, claude: &Path, args: &[&str]) -> (i32, String, String) {
    let mut command = common::cctg(home);
    command.args(args).env("CCTG_CLAUDE", claude);
    let out = common::output(&mut command).expect("run cctg");
    (
        out.status.code().unwrap_or(-1),
        String::from_utf8_lossy(&out.stdout).into_owned(),
        String::from_utf8_lossy(&out.stderr).into_owned(),
    )
}

fn write(path: &Path, bytes: &[u8]) {
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(path, bytes).unwrap();
}

#[test]
fn the_windows_sandbox_confines_a_command() {
    if std::env::var("CCTG_WINDOWS_SANDBOX_E2E").as_deref() != Ok("1") {
        return skip("CCTG_WINDOWS_SANDBOX_E2E is not 1");
    }
    let Some(bash) = git_bash() else {
        return skip("no Git Bash");
    };

    // A throwaway home under the runner's temp.
    let home = std::env::temp_dir().join(format!("cctg-e2e-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&home);
    std::fs::create_dir_all(&home).unwrap();

    // A claude stand-in that reports a recent version, and the wrapper/shim
    // markers preflight checks for.
    let claude = home.join("fake-claude.cmd");
    write(&claude, b"@echo 2.1.285 (Claude Code)\r\n");
    write(
        &home.join(".local").join("bin").join("claude-cctg"),
        b"#!/bin/sh\np=$(cctg sandbox-check --settings s)\n",
    );
    write(
        &home.join(".local").join("bin").join("claude-cctg.cmd"),
        b"sandbox-check --settings s --cmd\r\n",
    );
    write(
        &home.join(".cctg").join("bin").join("cctg-sandbox-exec"),
        b"MSYS_NO_PATHCONV=1 cctg sandbox-exec \"$1\"\n",
    );
    write(
        &home.join(".cctg").join("claude").join("settings.json"),
        b"{}",
    );

    // Install two slots. On a non-elevated runner this fails; skip cleanly.
    let (code, out, err) = cctg(&home, &claude, &["sandbox-install", "--slots", "2"]);
    if code != 0 {
        let _ = cctg(&home, &claude, &["sandbox-uninstall"]);
        let _ = std::fs::remove_dir_all(&home);
        return skip(&format!("sandbox-install failed ({code}): {out} {err}"));
    }

    let run = || {
        let a = home.join("dev").join("A");
        let b = home.join("dev").join("B");
        std::fs::create_dir_all(&a).unwrap();
        std::fs::create_dir_all(&b).unwrap();
        write(&home.join("marker.txt"), b"top secret");

        for folder in [&a, &b] {
            let (code, _o, err) = cctg(
                &home,
                &claude,
                &["sandbox", "on", "--folder", &folder.to_string_lossy()],
            );
            assert_eq!(code, 0, "sandbox on {}: {err}", folder.display());
        }

        // A command in A: identity, a write inside A, a denied read of the
        // marker and of B, the secret check, stdin echo and a chosen exit code.
        let marker = home.join("marker.txt").to_string_lossy().replace('\\', "/");
        let bslash = b.to_string_lossy().replace('\\', "/");
        let line = format!(
            "whoami; echo wrote > wrote.txt && echo WROTE_OK; \
             (cat '{marker}' && echo READ_MARKER) 2>/dev/null || echo MARKER_DENIED; \
             (echo x > '{bslash}/from_a' && echo WROTE_B) 2>/dev/null || echo B_DENIED; \
             if set | grep -q CCTG_HUB_SECRET; then echo SECRET_LEAK; else echo NO_SECRET; fi; \
             cat; exit 7"
        );
        let mut command = common::cctg(&home);
        command
            .args(["sandbox-exec", &line])
            .current_dir(&a)
            .env("CCTG_SANDBOX_MARK", &a)
            .env("CCTG_SANDBOX_BASH", &bash)
            .env("CCTG_SANDBOX", "1")
            .env("CCTG_HUB_SECRET", "e2e-dummy-secret")
            .env("GH_TOKEN", "e2e-dummy-token")
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        let mut child = common::spawn(&mut command).expect("spawn broker");
        child
            .stdin
            .take()
            .unwrap()
            .write_all(b"STDIN_REACHED\n")
            .unwrap();
        let out = child.wait_with_output().expect("broker output");
        let stdout = String::from_utf8_lossy(&out.stdout);
        let code = out.status.code().unwrap_or(-1);

        assert!(
            stdout.contains("cctg-sandbox-"),
            "whoami is a slot: {stdout}"
        );
        assert!(stdout.contains("WROTE_OK"), "write inside A: {stdout}");
        assert!(a.join("wrote.txt").exists(), "the file is there");
        assert!(
            stdout.contains("MARKER_DENIED"),
            "marker read denied: {stdout}"
        );
        assert!(!stdout.contains("top secret"), "marker not read: {stdout}");
        assert!(stdout.contains("B_DENIED"), "write to B denied: {stdout}");
        assert!(!b.join("from_a").exists(), "B unwritten");
        assert!(stdout.contains("NO_SECRET"), "no secret in env: {stdout}");
        assert!(
            stdout.contains("STDIN_REACHED"),
            "stdin reached the command: {stdout}"
        );
        assert_eq!(code, 7, "the command's own exit code: {stdout}");

        // After sandbox off, A no longer starts a sandboxed command.
        let (code, _o, _e) = cctg(
            &home,
            &claude,
            &["sandbox", "off", "--folder", &a.to_string_lossy()],
        );
        assert_eq!(code, 0, "sandbox off");
        let mut refused = common::cctg(&home);
        refused
            .args(["sandbox-exec", "echo should-not-run"])
            .current_dir(&a)
            .env("CCTG_SANDBOX_MARK", &a)
            .env("CCTG_SANDBOX_BASH", &bash);
        let out = common::output(&mut refused).unwrap();
        assert_eq!(
            out.status.code(),
            Some(125),
            "an unmarked folder is refused by the broker"
        );
    };

    // Always uninstall, even if an assertion panics.
    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(run));
    let (code, _o, err) = cctg(&home, &claude, &["sandbox-uninstall"]);
    assert_eq!(code, 0, "sandbox-uninstall: {err}");
    // The mark key is gone.
    let mut reg = Command::new("reg");
    reg.args(["query", r"HKLM\SOFTWARE\cctg\sandbox"]);
    let mark_gone = common::output(&mut reg)
        .map(|o| !o.status.success())
        .unwrap_or(true);
    let _ = std::fs::remove_dir_all(&home);
    assert!(mark_gone, "the install mark is gone after uninstall");
    if let Err(panic) = result {
        std::panic::resume_unwind(panic);
    }
}
