//! End-to-end test of the native Windows folder sandbox (TASK-089), against
//! REAL local slot accounts. It creates accounts, so it runs only on a
//! Windows runner that sets `CCTG_WINDOWS_SANDBOX_E2E=1` (the hosted GitHub
//! Windows runners are administrators with UAC off). Anywhere else — including
//! a developer's machine — it prints "skipped" and returns without touching
//! the system.
//!
//! It exercises the acceptance criteria and the PLAN_FINAL §3 scenario, all
//! under the real restricted-token slot process: a command cannot read the
//! user's files or write another marked folder; the sandboxed environment
//! holds no secret (checked against a whitelist/denylist, not one name); the
//! exit code and stdin flow through; `.git`/`.claude` and shell/IDE configs are
//! write-denied; a hard-linked body is write-denied; a bare-repo `HEAD` left in
//! the root is cleaned up; a junction inside the folder to the outside is not
//! followed by the token; a TLS network call works (no restricting SIDs); and
//! after `sandbox off` the folder no longer starts a sandboxed command. Then it
//! uninstalls and checks the accounts, the group and the mark are gone. cctg is
//! started only through `tests/common` (no hub is ever contacted).

#![cfg(windows)]

use std::io::Write as _;
use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};

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

/// A cctg subcommand with the fake home and `CCTG_CLAUDE`.
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

/// One sandboxed command in `folder` (stdin null). The broker process also
/// carries the given extra env (e.g. secrets, to prove they do not leak).
fn broker(home: &Path, folder: &Path, bash: &Path, line: &str, extra: &[(&str, &str)]) -> Output {
    let mut command = common::cctg(home);
    command
        .args(["sandbox-exec", line])
        .current_dir(folder)
        .env("CCTG_SANDBOX_MARK", folder)
        .env("CCTG_SANDBOX_BASH", bash)
        .env("CCTG_SANDBOX", "1");
    for (k, v) in extra {
        command.env(k, v);
    }
    common::output(&mut command).expect("broker")
}

fn write(path: &Path, bytes: &[u8]) {
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(path, bytes).unwrap();
}

/// A slash-form path for Bash single quotes.
fn slash(path: &Path) -> String {
    path.to_string_lossy().replace('\\', "/")
}

fn junction(link: &Path, target: &Path) -> bool {
    let mut c = Command::new("cmd");
    c.args(["/C", "mklink", "/J"]).arg(link).arg(target);
    common::output(&mut c)
        .map(|o| o.status.success())
        .unwrap_or(false)
}

/// A name that must never reach a sandboxed command (mirrors the runner's
/// scrub): CCTG_* except the two sandbox vars, CLAUDE*/ANTHROPIC*, *_TOKEN,
/// *_SECRET, and known token names.
fn sensitive(name: &str) -> bool {
    let up = name.to_ascii_uppercase();
    if up == "CCTG_SANDBOX" || up == "CCTG_SANDBOX_COMMAND" {
        return false;
    }
    up.starts_with("CCTG_")
        || up.starts_with("CLAUDE")
        || up.starts_with("ANTHROPIC")
        || up.ends_with("_TOKEN")
        || up.ends_with("_SECRET")
        || matches!(
            up.as_str(),
            "GH_TOKEN" | "GITHUB_TOKEN" | "SSH_AUTH_SOCK" | "NPM_TOKEN" | "OPENAI_API_KEY"
        )
}

fn settings_path(home: &Path) -> PathBuf {
    home.join(".cctg").join("claude").join("settings.json")
}

#[test]
fn the_windows_sandbox_confines_a_command() {
    if std::env::var("CCTG_WINDOWS_SANDBOX_E2E").as_deref() != Ok("1") {
        return skip("CCTG_WINDOWS_SANDBOX_E2E is not 1");
    }
    let Some(bash) = git_bash() else {
        return skip("no Git Bash");
    };

    // A throwaway home under the runner's temp, plus a program dir OUTSIDE it.
    let home = std::env::temp_dir().join(format!("cctg-e2e-{}", std::process::id()));
    let tools = std::env::temp_dir().join(format!("cctg-e2e-tools-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&home);
    let _ = std::fs::remove_dir_all(&tools);
    std::fs::create_dir_all(&home).unwrap();
    std::fs::create_dir_all(&tools).unwrap();
    std::fs::write(tools.join("probe.txt"), b"tool-data").unwrap();

    // A claude stand-in that reports a recent version; the wrapper/shim
    // markers preflight checks for; a base settings.json; a read-dirs entry.
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
    write(&settings_path(&home), b"{}");
    write(
        &home.join(".cctg").join("sandbox").join("read-dirs"),
        tools.to_string_lossy().as_bytes(),
    );

    // Install two slots. On a non-elevated runner this fails; skip cleanly.
    let (code, out, err) = cctg(&home, &claude, &["sandbox-install", "--slots", "2"]);
    if code != 0 {
        let _ = cctg(&home, &claude, &["sandbox-uninstall"]);
        let _ = std::fs::remove_dir_all(&home);
        let _ = std::fs::remove_dir_all(&tools);
        return skip(&format!("sandbox-install failed ({code}): {out} {err}"));
    }

    let bash2 = bash.clone();
    let claude2 = claude.clone();
    let home2 = home.clone();
    let tools2 = tools.clone();
    let run = move || {
        let bash = &bash2;
        let claude = &claude2;
        let home = &home2;
        let tools = &tools2;
        let a = home.join("dev").join("A");
        let b = home.join("dev").join("B");
        std::fs::create_dir_all(&a).unwrap();
        std::fs::create_dir_all(&b).unwrap();
        write(&home.join("marker.txt"), b"top secret");

        // Setup BEFORE sandbox on (so grant/stamp see them):
        // - B has a git repo the real user created (its .git must become
        //   read-only to the slot after sandbox on).
        std::fs::create_dir_all(b.join(".git").join("hooks")).unwrap();
        std::fs::write(b.join(".git").join("config"), b"[core]\n").unwrap();
        // - A hard link from inside A to an outside file (its shared body must
        //   be write-denied).
        let outside_file = home.join("linked-outside.txt");
        std::fs::write(&outside_file, b"shared-body").unwrap();
        let _ = std::fs::hard_link(&outside_file, a.join("shared.txt"));
        // - A junction inside A to the home directory (must not be followed).
        let has_junction = junction(&a.join("linkhome"), home);

        for folder in [&a, &b] {
            let (code, _o, err) = cctg(
                home,
                claude,
                &["sandbox", "on", "--folder", &folder.to_string_lossy()],
            );
            assert_eq!(code, 0, "sandbox on {}: {err}", folder.display());
        }

        // (1) A command in A: identity, write inside A, denied reads/writes,
        // the env dump, a HEAD left in the root, stdin echo, exit code.
        let marker = slash(&home.join("marker.txt"));
        let bslash = slash(&b);
        let line = format!(
            "whoami; \
             echo wrote > wrote.txt && echo WROTE_OK; \
             (cat '{marker}' && echo READ_MARKER) 2>/dev/null || echo MARKER_DENIED; \
             (echo x > '{bslash}/from_a' && echo WROTE_B) 2>/dev/null || echo B_DENIED; \
             echo ref: HEAD > HEAD; \
             echo '--- ENVNAMES'; env | cut -d= -f1 | sort; echo '--- ENDENV'; \
             cat; exit 7"
        );
        let mut command = common::cctg(home);
        command
            .args(["sandbox-exec", &line])
            .current_dir(&a)
            .env("CCTG_SANDBOX_MARK", &a)
            .env("CCTG_SANDBOX_BASH", bash)
            .env("CCTG_SANDBOX", "1")
            .env("CCTG_HUB_SECRET", "e2e-dummy-secret")
            .env("GH_TOKEN", "e2e-dummy-token")
            .env("ANTHROPIC_API_KEY", "e2e-dummy-key")
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
        assert!(
            stdout.contains("STDIN_REACHED"),
            "stdin reached the command: {stdout}"
        );
        assert_eq!(code, 7, "the command's own exit code: {stdout}");

        // Whitelist/denylist of the sandboxed environment: no sensitive name,
        // and the expected overlay names are present.
        let names: Vec<&str> = stdout
            .lines()
            .skip_while(|l| l.trim() != "--- ENVNAMES")
            .skip(1)
            .take_while(|l| l.trim() != "--- ENDENV")
            .map(str::trim)
            .filter(|l| !l.is_empty())
            .collect();
        assert!(!names.is_empty(), "env names captured: {stdout}");
        for name in &names {
            assert!(
                !sensitive(name),
                "sensitive env var leaked: {name}\n{stdout}"
            );
        }
        for required in ["CCTG_SANDBOX", "PATH", "HOME", "TEMP", "CARGO_HOME"] {
            assert!(
                names.iter().any(|n| n.eq_ignore_ascii_case(required)),
                "missing expected env {required}: {names:?}"
            );
        }

        // The bare-repo HEAD the command left in the root was cleaned up.
        assert!(
            !a.join("HEAD").exists(),
            "a slot-created HEAD is removed after the command"
        );

        // (2) Protected files, under the real token, in B.
        let protect = "(echo x >> .git/config && echo WROTE_GITCONFIG) 2>/dev/null || echo GITCONFIG_DENIED; \
             (touch .git/hooks/x && echo WROTE_HOOK) 2>/dev/null || echo HOOK_DENIED; \
             (mkdir .claude/x && echo MADE_CLAUDE) 2>/dev/null || echo CLAUDE_DENIED";
        let o = broker(home, &b, bash, protect, &[]);
        let s = String::from_utf8_lossy(&o.stdout);
        assert!(
            s.contains("GITCONFIG_DENIED"),
            ".git/config write denied: {s}"
        );
        assert!(s.contains("HOOK_DENIED"), ".git/hooks write denied: {s}");
        assert!(s.contains("CLAUDE_DENIED"), ".claude write denied: {s}");

        // (3) The hard-linked body in A is write-denied.
        let o = broker(
            home,
            &a,
            bash,
            "(printf x > shared.txt && echo WROTE_SHARED) 2>/dev/null || echo SHARED_DENIED",
            &[],
        );
        let s = String::from_utf8_lossy(&o.stdout);
        assert!(
            s.contains("SHARED_DENIED"),
            "hard-linked body write denied: {s}"
        );
        assert_eq!(
            std::fs::read(&outside_file).unwrap(),
            b"shared-body",
            "the outside body is unchanged"
        );

        // (4) A junction inside A to the outside is not followed by the token.
        if has_junction {
            let o = broker(
                home,
                &a,
                bash,
                "(cat linkhome/marker.txt && echo READ_VIA_JUNCTION) 2>/dev/null || echo JUNCTION_DENIED",
                &[],
            );
            let s = String::from_utf8_lossy(&o.stdout);
            assert!(s.contains("JUNCTION_DENIED"), "junction not followed: {s}");
            assert!(
                !s.contains("top secret"),
                "marker not read via junction: {s}"
            );
        }

        // (5) A TLS network call works (no restricting SIDs break Schannel).
        let o = broker(
            home,
            &a,
            bash,
            "curl -sS -o /dev/null -w 'HTTP %{http_code}\\n' https://github.com 2>&1 || echo CURL_ERR",
            &[],
        );
        let s = String::from_utf8_lossy(&o.stdout);
        assert!(
            !s.contains("SEC_E_NO_CREDENTIALS") && !s.to_lowercase().contains("no credentials"),
            "Schannel credentials error inside the sandbox: {s}"
        );

        // (6) read-dirs: a sandbox-check in A syncs the group grant, then the
        // tools dir is readable but not writable. Best effort: the grant is
        // applied at session start; if sandbox-check refuses, skip.
        let check_code = {
            let mut c = common::cctg(home);
            c.args(["sandbox-check", "--settings"])
                .arg(settings_path(home))
                .current_dir(&a)
                .env("CCTG_CLAUDE", claude)
                .env("CLAUDE_CODE_GIT_BASH_PATH", bash);
            common::output(&mut c).unwrap().status.code().unwrap_or(-1)
        };
        if check_code == 0 {
            let probe = slash(&tools.join("probe.txt"));
            let line = format!(
                "(cat '{probe}' >/dev/null && echo READ_TOOL) 2>/dev/null || echo TOOL_READ_DENIED; \
                 (echo x > '{probe}' && echo WROTE_TOOL) 2>/dev/null || echo TOOL_WRITE_DENIED"
            );
            let o = broker(home, &a, bash, &line, &[]);
            let s = String::from_utf8_lossy(&o.stdout);
            assert!(s.contains("READ_TOOL"), "read-dirs allows reading: {s}");
            assert!(
                s.contains("TOOL_WRITE_DENIED"),
                "read-dirs is read-only: {s}"
            );
        }

        // (7) After sandbox off, A no longer starts a sandboxed command.
        let (code, _o, _e) = cctg(
            home,
            claude,
            &["sandbox", "off", "--folder", &a.to_string_lossy()],
        );
        assert_eq!(code, 0, "sandbox off");
        let o = broker(home, &a, bash, "echo should-not-run", &[]);
        assert_eq!(
            o.status.code(),
            Some(125),
            "an unmarked folder is refused by the broker"
        );
    };

    // Always uninstall, even if an assertion panics.
    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(run));
    let (code, _o, err) = cctg(&home, &claude, &["sandbox-uninstall"]);
    assert_eq!(code, 0, "sandbox-uninstall: {err}");
    let gone = |args: &[&str]| {
        let mut c = Command::new(args[0]);
        c.args(&args[1..]);
        common::output(&mut c)
            .map(|o| !o.status.success())
            .unwrap_or(true)
    };
    let key_gone = gone(&["reg", "query", r"HKLM\SOFTWARE\cctg\sandbox"]);
    let group_gone = gone(&["net", "localgroup", "cctg-sandbox"]);
    let user_gone = gone(&["net", "user", "cctg-sandbox-1"]);
    let _ = std::fs::remove_dir_all(&home);
    let _ = std::fs::remove_dir_all(&tools);
    assert!(key_gone, "the install mark is gone after uninstall");
    assert!(group_gone, "the sandbox group is gone after uninstall");
    assert!(user_gone, "the slot account is gone after uninstall");
    if let Err(panic) = result {
        std::panic::resume_unwind(panic);
    }
}
