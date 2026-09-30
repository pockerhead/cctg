//! TASK-087: the real `cctg sandbox-gate`, `cctg sandbox-check` and
//! `cctg sandbox on` binaries: exit codes, stdout and stderr.

use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};

mod common;

/// A home with a project folder, under this run's own tmp directory.
fn home(name: &str) -> (PathBuf, PathBuf) {
    let home = common::own_tmp().join(format!("sandbox-cli-{name}"));
    let folder = home.join("proj");
    std::fs::create_dir_all(&folder).unwrap();
    let home = std::fs::canonicalize(&home).unwrap();
    let folder = std::fs::canonicalize(&folder).unwrap();
    (plain(home), plain(folder))
}

/// Without Windows' `\\?\`.
fn plain(path: PathBuf) -> PathBuf {
    let text = path.to_string_lossy().into_owned();
    PathBuf::from(text.strip_prefix(r"\\?\").unwrap_or(&text))
}

fn with_stdin(command: &mut Command, input: &[u8]) -> Output {
    command
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    let mut child = common::spawn(command).unwrap();
    child.stdin.take().unwrap().write_all(input).unwrap();
    child.wait_with_output().unwrap()
}

fn gate(home: &Path, folder: &Path, input: &str) -> Output {
    let mut command = common::cctg(home);
    command
        .arg("sandbox-gate")
        .env("CCTG_SANDBOX", "1")
        .env("CLAUDE_PROJECT_DIR", folder);
    with_stdin(&mut command, input.as_bytes())
}

#[test]
fn the_gate_blocks_with_exit_2_and_names_no_path() {
    let (home, folder) = home("gate");
    let write = |path: &Path| {
        serde_json::json!({
            "tool_name": "Write",
            "tool_input": { "file_path": path.display().to_string() }
        })
        .to_string()
    };
    let inside = gate(&home, &folder, &write(&folder.join("a.txt")));
    assert_eq!(inside.status.code(), Some(0), "{inside:?}");
    assert!(
        inside.stdout.is_empty() && inside.stderr.is_empty(),
        "{inside:?}"
    );

    let secret_place = home.join("secret-place").join("x");
    for denied in [secret_place.clone(), folder.join(".git").join("config")] {
        let out = gate(&home, &folder, &write(&denied));
        assert_eq!(out.status.code(), Some(2), "{out:?}");
        assert!(out.stdout.is_empty());
        let stderr = String::from_utf8_lossy(&out.stderr);
        assert!(stderr.contains("cctg sandbox:"), "{stderr}");
        assert!(
            !stderr.contains("secret-place") && !stderr.contains(".git/"),
            "{stderr}"
        );
    }
    let broken = gate(&home, &folder, "{not json");
    assert_eq!(broken.status.code(), Some(2));

    let mut extra = common::cctg(&home);
    extra.args(["sandbox-gate", "extra"]);
    let out = with_stdin(&mut extra, write(&folder.join("a.txt")).as_bytes());
    assert_eq!(out.status.code(), Some(2), "bad arguments block too");

    let mut no_profile = common::cctg(&home);
    no_profile.arg("sandbox-gate");
    let out = with_stdin(&mut no_profile, write(&secret_place).as_bytes());
    assert_eq!(out.status.code(), Some(0), "not a sandbox session");
}

fn check(home: &Path, cwd: &Path) -> Output {
    let settings = home.join(".cctg").join("claude").join("settings.json");
    std::fs::create_dir_all(settings.parent().unwrap()).unwrap();
    if !settings.exists() {
        std::fs::write(&settings, b"{}").unwrap();
    }
    let mut command = common::cctg(home);
    command
        .arg("sandbox-check")
        .arg("--settings")
        .arg(&settings)
        .current_dir(cwd);
    common::output(&mut command).unwrap()
}

fn mark(home: &Path, folders: &[&Path]) {
    let file = home.join(".cctg").join("sandbox").join("folders.json");
    std::fs::create_dir_all(file.parent().unwrap()).unwrap();
    let json = serde_json::json!({ "version": 1, "folders": folders });
    std::fs::write(file, json.to_string()).unwrap();
}

#[test]
fn sandbox_check_answers_the_wrapper() {
    let (home, folder) = home("check");
    let out = check(&home, &folder);
    assert_eq!(out.status.code(), Some(10), "{out:?}");
    assert!(out.stdout.is_empty(), "{out:?}");

    // A marked folder this device cannot lock: nothing starts.
    let odd = folder.join("a[b");
    std::fs::create_dir_all(&odd).unwrap();
    mark(&home, &[&odd]);
    let out = check(&home, &odd);
    assert_eq!(out.status.code(), Some(3), "{out:?}");
    assert!(out.stdout.is_empty());
    assert!(!out.stderr.is_empty());
    let out = check(&home, &folder);
    assert_eq!(out.status.code(), Some(10), "a sibling mark does not cover");

    std::fs::write(
        home.join(".cctg").join("sandbox").join("folders.json"),
        b"{broken",
    )
    .unwrap();
    let out = check(&home, &folder);
    assert_eq!(out.status.code(), Some(3), "unreadable marks fail closed");

    let mut bad = common::cctg(&home);
    bad.arg("sandbox-check");
    let out = common::output(&mut bad).unwrap();
    assert_eq!(out.status.code(), Some(3), "no --settings");
}

#[cfg(not(target_os = "macos"))]
#[test]
fn sandbox_on_refuses_what_cannot_be_locked() {
    let (home, folder) = home("on-refused");
    let mut on = common::cctg(&home);
    on.args(["sandbox", "on", "--folder"])
        .arg(&folder)
        .env("CCTG_CLAUDE", home.join("no-such-claude"));
    let out = common::output(&mut on).unwrap();
    assert_eq!(out.status.code(), Some(1), "{out:?}");
    let stderr = String::from_utf8_lossy(&out.stderr);
    if cfg!(windows) {
        assert!(stderr.contains("Windows"), "{stderr}");
    }
    assert!(
        !home
            .join(".cctg")
            .join("sandbox")
            .join("folders.json")
            .exists()
    );
}

#[cfg(target_os = "macos")]
#[test]
fn sandbox_on_then_check_gives_a_profile_on_macos() {
    let (home, folder) = home("on-mac");
    let claude = home.join("fake-claude");
    common::write_program(&claude, b"#!/bin/sh\necho '2.1.284 (Claude Code)'\n");
    let wrapper = home.join(".local").join("bin").join("claude-cctg");
    std::fs::create_dir_all(wrapper.parent().unwrap()).unwrap();
    std::fs::write(
        &wrapper,
        b"#!/bin/sh\np=$(cctg sandbox-check --settings s)\n",
    )
    .unwrap();
    let mut on = common::cctg(&home);
    on.args(["sandbox", "on", "--folder"])
        .arg(&folder)
        .env("CCTG_CLAUDE", &claude);
    let out = common::output(&mut on).unwrap();
    assert_eq!(out.status.code(), Some(0), "{out:?}");

    let settings = home.join(".cctg").join("claude").join("settings.json");
    std::fs::create_dir_all(settings.parent().unwrap()).unwrap();
    std::fs::write(&settings, b"{\"hooks\":{}}").unwrap();
    let mut command = common::cctg(&home);
    command
        .args(["sandbox-check", "--settings"])
        .arg(&settings)
        .env("CCTG_CLAUDE", &claude)
        .current_dir(&folder);
    let out = common::output(&mut command).unwrap();
    assert_eq!(out.status.code(), Some(0), "{out:?}");
    let path = String::from_utf8(out.stdout).unwrap();
    let profile: serde_json::Value =
        serde_json::from_slice(&std::fs::read(path.trim_end()).unwrap()).unwrap();
    assert_eq!(profile["sandbox"]["enabled"], true);
    assert_eq!(profile["env"]["CCTG_SANDBOX"], "1");
}
