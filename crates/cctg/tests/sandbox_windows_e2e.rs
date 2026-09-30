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
//! holds no secret or credential-looking name (checked over the whole env, not
//! one name); the exit code and stdin flow through; `.git`/`.claude` are
//! write-denied; a hard-linked body is write-denied; a bare-repo `HEAD` left in
//! the root is cleaned up; a junction inside the folder to the outside is not
//! followed by the token; a TLS network call works (no restricting SIDs); and
//! after `sandbox off` the folder no longer starts a sandboxed command. Then it
//! uninstalls and checks the accounts, the group and the mark are gone.
//!
//! Every check is recorded and the test fails ONCE at the end with the full
//! list, so one CI run shows every result. cctg is started only through
//! `tests/common` (no hub is ever contacted).

#![cfg(windows)]

use std::io::Write as _;
use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};

mod common;

fn skip(reason: &str) {
    println!("sandbox_windows_e2e: skipped ({reason})");
}

/// Recorded check results: every check runs, the test fails once at the end.
#[derive(Default)]
struct Checks {
    passed: Vec<String>,
    failed: Vec<String>,
}

impl Checks {
    fn check(&mut self, ok: bool, what: &str, detail: impl std::fmt::Display) {
        if ok {
            self.passed.push(what.to_owned());
        } else {
            self.failed.push(format!("{what}\n    {detail}"));
        }
    }

    fn report(&self) {
        println!("sandbox_windows_e2e: {} passed", self.passed.len());
        for what in &self.passed {
            println!("  PASS {what}");
        }
        for what in &self.failed {
            println!("  FAIL {what}");
        }
    }
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
    match common::output(&mut command) {
        Ok(out) => (
            out.status.code().unwrap_or(-1),
            String::from_utf8_lossy(&out.stdout).into_owned(),
            String::from_utf8_lossy(&out.stderr).into_owned(),
        ),
        Err(error) => (-1, String::new(), error.to_string()),
    }
}

/// One sandboxed command in `folder` (stdin null); returns (code, stdout).
fn broker(home: &Path, folder: &Path, bash: &Path, line: &str) -> (i32, String) {
    let mut command = common::cctg(home);
    command
        .args(["sandbox-exec", line])
        .current_dir(folder)
        .env("CCTG_SANDBOX_MARK", folder)
        .env("CCTG_SANDBOX_BASH", bash)
        .env("CCTG_SANDBOX", "1");
    match common::output(&mut command) {
        Ok(Output { status, stdout, .. }) => (
            status.code().unwrap_or(-1),
            String::from_utf8_lossy(&stdout).into_owned(),
        ),
        Err(error) => (-1, error.to_string()),
    }
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

/// An env name that must not reach a sandboxed command (an oracle independent
/// of the runner's scrub): `CCTG_*` / `CLAUDE*` / `ANTHROPIC*` and any
/// credential-looking name. Exactly the overlay's own non-credential names are
/// allowed: `CCTG_SANDBOX`, `CCTG_SANDBOX_COMMAND` (the command itself),
/// `CLAUDE_PROJECT_DIR` (the folder path) and `CLAUDECODE` (`1`).
fn sensitive(name: &str) -> bool {
    let up = name.to_ascii_uppercase();
    if matches!(
        up.as_str(),
        "CCTG_SANDBOX" | "CCTG_SANDBOX_COMMAND" | "CLAUDE_PROJECT_DIR" | "CLAUDECODE"
    ) {
        return false;
    }
    up.starts_with("CCTG_")
        || up.starts_with("CLAUDE")
        || up.starts_with("ANTHROPIC")
        || up.ends_with("_TOKEN")
        || up.ends_with("_SECRET")
        || up.ends_with("_KEY")
        || up.contains("PASSWORD")
        || up.contains("PASSWD")
        || up.contains("APIKEY")
        || up.contains("CREDENTIAL")
        || matches!(
            up.as_str(),
            "GH_TOKEN" | "GITHUB_TOKEN" | "SSH_AUTH_SOCK" | "NPM_TOKEN" | "OPENAI_API_KEY"
        )
}

fn settings_path(home: &Path) -> PathBuf {
    home.join(".cctg").join("claude").join("settings.json")
}

/// The scenario after install; records every result in `checks`.
fn scenario(home: &Path, tools: &Path, bash: &Path, claude: &Path, checks: &mut Checks) {
    let a = home.join("dev").join("A");
    let b = home.join("dev").join("B");
    std::fs::create_dir_all(&a).unwrap();
    std::fs::create_dir_all(&b).unwrap();
    write(&home.join("marker.txt"), b"top secret");

    // Setup BEFORE sandbox on (so grant/stamp see them): B has a git repo the
    // real user created; A has a hard link to an outside file and a junction
    // to the home directory.
    std::fs::create_dir_all(b.join(".git").join("hooks")).unwrap();
    std::fs::write(b.join(".git").join("config"), b"[core]\n").unwrap();
    let outside_file = home.join("linked-outside.txt");
    std::fs::write(&outside_file, b"shared-body").unwrap();
    let has_hardlink = std::fs::hard_link(&outside_file, a.join("shared.txt")).is_ok();
    let has_junction = junction(&a.join("linkhome"), home);

    for folder in [&a, &b] {
        let (code, _o, err) = cctg(
            home,
            claude,
            &["sandbox", "on", "--folder", &folder.to_string_lossy()],
        );
        checks.check(
            code == 0,
            &format!("sandbox on {}", folder.display()),
            format!("code {code}: {err}"),
        );
    }

    // (1) A command in A with three secrets planted on the broker.
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
    let (code, stdout) = match common::spawn(&mut command) {
        Ok(mut child) => {
            if let Some(mut stdin) = child.stdin.take() {
                let _ = stdin.write_all(b"STDIN_REACHED\n");
            }
            match child.wait_with_output() {
                Ok(out) => (
                    out.status.code().unwrap_or(-1),
                    String::from_utf8_lossy(&out.stdout).into_owned(),
                ),
                Err(error) => (-1, error.to_string()),
            }
        }
        Err(error) => (-1, error.to_string()),
    };
    // Print the command output once so the CI log shows it whatever fails.
    println!("--- command 1 output (exit {code})\n{stdout}--- end command 1");
    checks.check(
        stdout.contains("cctg-sandbox-"),
        "whoami is a slot account",
        "",
    );
    checks.check(stdout.contains("WROTE_OK"), "write inside A works", "");
    checks.check(a.join("wrote.txt").exists(), "the written file is in A", "");
    checks.check(
        stdout.contains("MARKER_DENIED") && !stdout.contains("top secret"),
        "reading the user's marker is denied",
        "",
    );
    checks.check(
        stdout.contains("B_DENIED") && !b.join("from_a").exists(),
        "writing the other marked folder is denied",
        "",
    );
    checks.check(
        stdout.contains("STDIN_REACHED"),
        "stdin reaches the command",
        "",
    );
    checks.check(
        code == 7,
        "the command's exit code flows through",
        format!("exit {code}"),
    );

    let names: Vec<String> = stdout
        .lines()
        .skip_while(|l| l.trim() != "--- ENVNAMES")
        .skip(1)
        .take_while(|l| l.trim() != "--- ENDENV")
        .map(|l| l.trim().to_owned())
        .filter(|l| !l.is_empty())
        .collect();
    checks.check(!names.is_empty(), "the sandboxed env was captured", "");
    let leaked: Vec<&String> = names.iter().filter(|n| sensitive(n)).collect();
    checks.check(
        leaked.is_empty(),
        "no secret or credential-looking env name in the sandbox",
        format!("leaked: {leaked:?}"),
    );
    for planted in ["CCTG_HUB_SECRET", "GH_TOKEN", "ANTHROPIC_API_KEY"] {
        checks.check(
            !names.iter().any(|n| n.eq_ignore_ascii_case(planted)),
            &format!("the broker's {planted} does not reach the sandbox"),
            "",
        );
    }
    for required in ["CCTG_SANDBOX", "PATH", "HOME", "TEMP", "CARGO_HOME"] {
        checks.check(
            names.iter().any(|n| n.eq_ignore_ascii_case(required)),
            &format!("the sandbox env has {required}"),
            format!("{names:?}"),
        );
    }
    checks.check(
        !a.join("HEAD").exists(),
        "a slot-created HEAD in the root is removed after the command",
        "",
    );

    // (2) Protected files, under the real token, in B.
    let (_c, s) = broker(
        home,
        &b,
        bash,
        "(echo x >> .git/config && echo WROTE_GITCONFIG) 2>/dev/null || echo GITCONFIG_DENIED; \
         (touch .git/hooks/x && echo WROTE_HOOK) 2>/dev/null || echo HOOK_DENIED; \
         (mkdir .claude/x && echo MADE_CLAUDE) 2>/dev/null || echo CLAUDE_DENIED",
    );
    checks.check(
        s.contains("GITCONFIG_DENIED"),
        ".git/config is write-denied",
        &s,
    );
    checks.check(s.contains("HOOK_DENIED"), ".git/hooks is write-denied", &s);
    checks.check(s.contains("CLAUDE_DENIED"), ".claude is write-denied", &s);

    // (3) The hard-linked body in A is write-denied.
    if has_hardlink {
        let (_c, s) = broker(
            home,
            &a,
            bash,
            "(printf x > shared.txt && echo WROTE_SHARED) 2>/dev/null || echo SHARED_DENIED",
        );
        let body = std::fs::read(&outside_file).unwrap_or_default();
        checks.check(
            s.contains("SHARED_DENIED") && body == b"shared-body",
            "a hard-linked body is write-denied and the outside file unchanged",
            &s,
        );
    }

    // (4) A junction inside A to the outside is not followed by the token.
    if has_junction {
        let (_c, s) = broker(
            home,
            &a,
            bash,
            "(cat linkhome/marker.txt && echo READ_VIA_JUNCTION) 2>/dev/null || echo JUNCTION_DENIED",
        );
        checks.check(
            s.contains("JUNCTION_DENIED") && !s.contains("top secret"),
            "a junction to the outside is not followed",
            &s,
        );
    }

    // (5) A TLS network call works (no restricting SIDs break Schannel).
    let (_c, s) = broker(
        home,
        &a,
        bash,
        "curl -sS -o /dev/null -w 'HTTP %{http_code}\\n' https://github.com 2>&1 || echo CURL_ERR",
    );
    checks.check(
        !s.contains("SEC_E_NO_CREDENTIALS") && !s.to_lowercase().contains("no credentials"),
        "a TLS call has no Schannel credentials error",
        &s,
    );
    checks.check(s.contains("HTTP "), "a TLS call to github.com answers", &s);

    // (6) read-dirs: a sandbox-check in A syncs the group grant, then the
    // program dir is readable but not writable.
    let check_code = {
        let mut c = common::cctg(home);
        c.args(["sandbox-check", "--settings"])
            .arg(settings_path(home))
            .current_dir(&a)
            .env("CCTG_CLAUDE", claude)
            .env("CLAUDE_CODE_GIT_BASH_PATH", bash);
        common::output(&mut c)
            .map(|o| o.status.code().unwrap_or(-1))
            .unwrap_or(-1)
    };
    checks.check(
        check_code == 0,
        "sandbox-check prepares the marked folder",
        format!("code {check_code}"),
    );
    if check_code == 0 {
        let probe = slash(&tools.join("probe.txt"));
        let (_c, s) = broker(
            home,
            &a,
            bash,
            &format!(
                "(cat '{probe}' >/dev/null && echo READ_TOOL) 2>/dev/null || echo TOOL_READ_DENIED; \
                 (echo x > '{probe}' && echo WROTE_TOOL) 2>/dev/null || echo TOOL_WRITE_DENIED"
            ),
        );
        checks.check(s.contains("READ_TOOL"), "a read-dirs entry is readable", &s);
        checks.check(
            s.contains("TOOL_WRITE_DENIED"),
            "a read-dirs entry is not writable",
            &s,
        );
    }

    // (7) After sandbox off, A no longer starts a sandboxed command.
    let (code, _o, err) = cctg(
        home,
        claude,
        &["sandbox", "off", "--folder", &a.to_string_lossy()],
    );
    checks.check(code == 0, "sandbox off", format!("code {code}: {err}"));
    let (code, _s) = broker(home, &a, bash, "echo should-not-run");
    checks.check(
        code == 125,
        "an unmarked folder is refused by the broker (125)",
        format!("exit {code}"),
    );
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

    let mut checks = Checks::default();
    // An unexpected panic in the scenario (setup I/O) is recorded too, so the
    // uninstall below always runs and the report still prints.
    let outcome = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        let mut inner = Checks::default();
        scenario(&home, &tools, &bash, &claude, &mut inner);
        inner
    }));
    match outcome {
        Ok(inner) => {
            checks.passed.extend(inner.passed);
            checks.failed.extend(inner.failed);
        }
        Err(_) => checks
            .failed
            .push("the scenario panicked (see the panic message above)".to_owned()),
    }

    let (code, _o, err) = cctg(&home, &claude, &["sandbox-uninstall"]);
    checks.check(
        code == 0,
        "sandbox-uninstall",
        format!("code {code}: {err}"),
    );
    let gone = |args: &[&str]| {
        let mut c = Command::new(args[0]);
        c.args(&args[1..]);
        common::output(&mut c)
            .map(|o| !o.status.success())
            .unwrap_or(true)
    };
    checks.check(
        gone(&["reg", "query", r"HKLM\SOFTWARE\cctg\sandbox"]),
        "the install mark is gone after uninstall",
        "",
    );
    checks.check(
        gone(&["net", "localgroup", "cctg-sandbox"]),
        "the sandbox group is gone after uninstall",
        "",
    );
    checks.check(
        gone(&["net", "user", "cctg-sandbox-1"]),
        "the slot account is gone after uninstall",
        "",
    );
    let _ = std::fs::remove_dir_all(&home);
    let _ = std::fs::remove_dir_all(&tools);

    checks.report();
    assert!(
        checks.failed.is_empty(),
        "{} check(s) failed:\n{}",
        checks.failed.len(),
        checks.failed.join("\n")
    );
}
