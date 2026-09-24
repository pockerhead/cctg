//! TASK-042: a `cctg` process started by the tests never reaches the
//! developer's live hub. The tests run inside a live Claude Code session
//! (`CLAUDE_CODE_SESSION_ID` set) on a machine whose `~/.cctg/device.env`
//! names the live hub; a `cctg agent` that inherits both registers with that
//! hub as the developer's session and unbinds its real agent.

use std::net::TcpListener;
use std::path::Path;
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

mod common;

const SECRET: &str = "isolation-secret-0123456789";
/// Set for the inner run: `isolated` or `raw`.
const INNER: &str = "ISOLATION_INNER";

/// Every test file that starts `cctg` goes through `tests/common`.
#[test]
fn every_test_starts_cctg_through_the_isolating_helper() {
    let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests");
    let mut checked = 0;
    for entry in std::fs::read_dir(&dir).expect("tests dir") {
        let path = entry.expect("dir entry").path();
        let name = path.file_name().unwrap().to_string_lossy().into_owned();
        if !name.ends_with(".rs") || name == "isolation.rs" {
            continue;
        }
        let text = std::fs::read_to_string(&path).expect("test source");
        if !text.contains("CARGO_BIN_EXE_cctg") && !text.contains("common::") {
            continue;
        }
        checked += 1;
        assert!(
            text.contains("common::cctg(") || text.contains("common::isolate("),
            "{name} starts cctg without tests/common"
        );
        for line in text.lines() {
            assert!(
                !(line.contains("CARGO_BIN_EXE_cctg") && line.contains("Command::new(")),
                "{name}: cctg started without tests/common: {line}"
            );
        }
    }
    assert!(checked >= 10, "only {checked} files checked");
}

/// With a session id, a hub secret and a hub address in the environment the
/// tests run in, the agent started through the helper stays off the hub; the
/// same agent started plainly connects (the TASK-042 leak).
#[test]
fn an_inherited_session_and_hub_config_never_reach_the_helpers_agent() {
    assert!(!connects("isolated"), "the isolated agent reached the hub");
    assert!(connects("raw"), "control: a plain agent reaches the hub");
}

/// Runs the inner test in `mode` with a session and hub config in its
/// environment; whether an agent connected to the hub address.
fn connects(mode: &str) -> bool {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    listener.set_nonblocking(true).unwrap();
    let addr = listener.local_addr().unwrap();
    let mut inner = Command::new(std::env::current_exe().unwrap())
        .args(["--exact", "inner_agent", "--ignored", "--nocapture"])
        .env(INNER, mode)
        .env(
            "CLAUDE_CODE_SESSION_ID",
            "15015015-0000-4000-8000-000000000042",
        )
        .env_remove("CLAUDE_CODE_ENTRYPOINT")
        .env("CCTG_HUB_SECRET", SECRET)
        .env("CCTG_HUB_AGENT_ADDR", addr.to_string())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .unwrap();
    let deadline = Instant::now() + Duration::from_secs(30);
    let mut connected = false;
    loop {
        if listener.accept().is_ok() {
            connected = true;
        }
        if let Some(status) = inner.try_wait().unwrap() {
            assert!(status.success(), "inner {mode} run failed");
            break;
        }
        assert!(Instant::now() < deadline, "inner {mode} run hangs");
        std::thread::sleep(Duration::from_millis(20));
    }
    connected || listener.accept().is_ok()
}

/// The inner run: starts `cctg agent` (through the helper, or plainly for
/// the control) and keeps it alive long enough to link.
#[test]
#[ignore = "run by an_inherited_session_and_hub_config_never_reach_the_helpers_agent"]
fn inner_agent() {
    let Ok(mode) = std::env::var(INNER) else {
        return;
    };
    let home = Path::new(env!("CARGO_TARGET_TMPDIR")).join(format!("isolation-{mode}"));
    std::fs::create_dir_all(&home).unwrap();
    let mut command = if mode == "isolated" {
        common::cctg(&home)
    } else {
        let mut plain = Command::new(env!("CARGO_BIN_EXE_cctg"));
        plain.env("USERPROFILE", &home).env("HOME", &home);
        plain
    };
    let mut agent = command
        .arg("agent")
        .current_dir(&home)
        .stdin(Stdio::piped())
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    std::thread::sleep(Duration::from_secs(2));
    drop(agent.stdin.take());
    let output = agent.wait_with_output().unwrap();
    if mode == "isolated" {
        let stderr = String::from_utf8_lossy(&output.stderr);
        assert!(stderr.contains("no Claude Code session id"), "{stderr}");
    }
}
