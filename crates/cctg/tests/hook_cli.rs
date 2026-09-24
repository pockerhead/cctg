//! `cctg hook <event>` as Claude Code runs it: a process with JSON on stdin,
//! configured through `<home>/.cctg/device.env`, talking to a real hub hook
//! endpoint. Exit code, stdout and stderr are what Claude Code sees.

use std::io::Write;
use std::net::{Ipv4Addr, SocketAddr};
use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};
use std::time::{Duration, Instant};

use cctg::hub::ingress;
use cctg::wire::{HookEvent, HookPost, Secret};
use tokio::net::TcpListener;
use tokio::sync::mpsc;

const SECRET: &str = "hook-cli-secret-0123456789";

fn fixture(name: &str) -> Vec<u8> {
    std::fs::read(
        Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("tests/fixtures/hook")
            .join(format!("{name}.json")),
    )
    .expect("hook fixture")
}

/// A home directory whose `.cctg/device.env` points at `addr`.
fn home(test: &str, addr: Option<&str>) -> PathBuf {
    let home = Path::new(env!("CARGO_TARGET_TMPDIR")).join(format!("hook-cli-{test}"));
    let dir = home.join(".cctg");
    std::fs::create_dir_all(&dir).unwrap();
    let mut env = String::new();
    if let Some(addr) = addr {
        env.push_str(&format!(
            "CCTG_HUB_SECRET={SECRET}\nCCTG_HUB_HOOK_ADDR={addr}\nCCTG_HOST=box\n"
        ));
    }
    std::fs::write(dir.join("device.env"), env).unwrap();
    home
}

fn run_hook(home: &Path, event: &str, stdin: &[u8]) -> (Output, Duration) {
    let started = Instant::now();
    let mut child = Command::new(env!("CARGO_BIN_EXE_cctg"))
        .args(["hook", event])
        .env("USERPROFILE", home)
        .env("HOME", home)
        .env_remove("CCTG_HUB_SECRET")
        .env_remove("CCTG_HUB_HOOK_ADDR")
        .env_remove("CCTG_HOST")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("cctg starts");
    let mut input = child.stdin.take().unwrap();
    let _ = input.write_all(stdin);
    drop(input);
    let output = child.wait_with_output().unwrap();
    (output, started.elapsed())
}

fn assert_quiet(output: &Output, secret_free_of: &[&str]) {
    assert!(output.status.success(), "{:?}", output.status);
    assert!(
        output.stdout.is_empty(),
        "stdout: {}",
        String::from_utf8_lossy(&output.stdout)
    );
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(!stderr.contains(SECRET), "{stderr}");
    for needle in secret_free_of {
        assert!(!stderr.contains(needle), "{needle} in {stderr}");
    }
}

async fn hub() -> (String, mpsc::Receiver<HookPost>) {
    let listener = ingress::bind(SocketAddr::from((Ipv4Addr::LOCALHOST, 0)))
        .await
        .unwrap();
    let addr = listener.local_addr().unwrap().to_string();
    let (tx, rx) = mpsc::channel(16);
    tokio::spawn(ingress::serve_hooks(
        listener,
        Secret::parse(SECRET).unwrap(),
        tx,
    ));
    (addr, rx)
}

#[tokio::test(flavor = "multi_thread")]
async fn every_event_reaches_the_hub() {
    let (addr, mut events) = hub().await;
    let home = home("deliver", Some(&addr));
    // The fixture's subagent files live on another machine: point at real ones.
    let agents = home.join("subagents");
    std::fs::create_dir_all(&agents).unwrap();
    let transcript = agents.join("agent-a8c1bff86acd31609.jsonl");
    std::fs::write(&transcript, "").unwrap();
    let mut stop: serde_json::Value = serde_json::from_slice(&fixture("subagent_stop")).unwrap();
    stop["agent_transcript_path"] = transcript.to_string_lossy().into_owned().into();
    let stop = stop.to_string().into_bytes();

    let cases: Vec<(&str, Vec<u8>, &str)> = vec![
        ("SessionStart", fixture("session_start"), "session_start"),
        ("SessionEnd", fixture("session_end"), "session_end"),
        (
            "UserPromptSubmit",
            fixture("user_prompt_submit"),
            "user_prompt_submit",
        ),
        ("Stop", fixture("stop"), "stop"),
        ("SubagentStart", fixture("subagent_start"), "subagent_start"),
        ("SubagentStop", stop, "subagent_stop"),
        (
            "PostToolUse",
            fixture("post_tool_use_handback"),
            "subagent_handback",
        ),
    ];
    for (event, input, kind) in cases {
        let (output, _) = tokio::task::spawn_blocking({
            let home = home.clone();
            move || run_hook(&home, event, &input)
        })
        .await
        .unwrap();
        assert_quiet(&output, &[]);
        let post = tokio::time::timeout(Duration::from_secs(5), events.recv())
            .await
            .expect("hub got the event")
            .unwrap();
        assert_eq!(post.event.kind(), kind, "{event}");
        assert_eq!(post.host, "box");
        assert!(!post.session_id.is_empty());
        if let HookEvent::SessionStart {
            claude_pid,
            parent_claude_pid,
            ..
        } = post.event
        {
            // The test runner may itself run under claude; whatever the tree,
            // a parent is never the session's own process.
            assert!(parent_claude_pid.is_none() || parent_claude_pid != claude_pid);
        }
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn a_silent_hub_keeps_session_end_well_inside_its_budget() {
    // Accepts and never answers: the POST runs into its timeout.
    let listener = TcpListener::bind(SocketAddr::from((Ipv4Addr::LOCALHOST, 0)))
        .await
        .unwrap();
    let addr = listener.local_addr().unwrap().to_string();
    tokio::spawn(async move {
        let mut open = Vec::new();
        while let Ok((stream, _)) = listener.accept().await {
            open.push(stream);
        }
    });
    let home = home("silent", Some(&addr));
    let (output, elapsed) =
        tokio::task::spawn_blocking(move || run_hook(&home, "SessionEnd", &fixture("session_end")))
            .await
            .unwrap();
    assert_quiet(&output, &["745465f7", "\"reason\""]);
    // Claude Code allows 1.5 s for all SessionEnd hooks together.
    assert!(elapsed < Duration::from_millis(1200), "{elapsed:?}");
    assert!(!output.stderr.is_empty(), "a failed delivery is reported");
}

#[test]
fn no_hub_listening_is_quiet_and_fast() {
    let port = std::net::TcpListener::bind((Ipv4Addr::LOCALHOST, 0))
        .unwrap()
        .local_addr()
        .unwrap();
    let home = home("refused", Some(&port.to_string()));
    for (event, name) in [
        ("SessionStart", "session_start"),
        ("SessionEnd", "session_end"),
    ] {
        let (output, elapsed) = run_hook(&home, event, &fixture(name));
        assert_quiet(&output, &["1e087ca8", "745465f7", "~\\"]);
        assert!(
            elapsed < Duration::from_millis(1200),
            "{event}: {elapsed:?}"
        );
    }
}

#[test]
fn an_undelivered_stop_without_a_state_dir_is_not_a_spool_failure() {
    let port = std::net::TcpListener::bind((Ipv4Addr::LOCALHOST, 0))
        .unwrap()
        .local_addr()
        .unwrap();
    // No home and no CCTG_STATE_DIR: the device has no spool.
    let mut child = Command::new(env!("CARGO_BIN_EXE_cctg"))
        .args(["hook", "Stop"])
        .env_remove("USERPROFILE")
        .env_remove("HOME")
        .env_remove("CCTG_STATE_DIR")
        .env("CCTG_HUB_SECRET", SECRET)
        .env("CCTG_HUB_HOOK_ADDR", port.to_string())
        .env("CCTG_HOST", "box")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("cctg starts");
    let mut input = child.stdin.take().unwrap();
    let _ = input.write_all(&fixture("stop"));
    drop(input);
    let output = child.wait_with_output().unwrap();
    assert_quiet(&output, &[]);
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("hook event not delivered"), "{stderr}");
    assert!(!stderr.contains("not kept"), "{stderr}");
}

#[test]
fn broken_input_and_missing_config_exit_zero_quietly() {
    let home_ok = home("broken", Some("127.0.0.1:9"));
    let whole = fixture("session_start");
    for input in [
        &b""[..],
        b"{",
        b"\xff\xfe",
        &whole[..whole.len() / 2],
        b"[1,2]",
    ] {
        let (output, _) = run_hook(&home_ok, "SessionStart", input);
        assert_quiet(&output, &["1e087ca8"]);
    }
    let home_empty = home("unconfigured", None);
    let (output, _) = run_hook(&home_empty, "Stop", &fixture("stop"));
    assert_quiet(&output, &["Ok.", "745465f7"]);
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("CCTG_HUB_SECRET is not set"));
    assert!(!stderr.contains("\u{1b}["), "{stderr}");
    assert!(
        !stderr
            .trim_start()
            .starts_with(|c: char| c.is_ascii_digit()),
        "{stderr}"
    );
}

#[test]
fn bad_hook_arguments_still_exit_zero() {
    for args in [&["hook"][..], &["hook", "Stop", "extra"][..]] {
        let output = Command::new(env!("CARGO_BIN_EXE_cctg"))
            .args(args)
            .stdin(Stdio::null())
            .output()
            .unwrap();
        assert_eq!(output.status.code(), Some(0), "{args:?}");
        assert!(output.stdout.is_empty(), "{args:?}");
    }
}

#[test]
fn an_open_silent_stdin_does_not_hold_the_hook() {
    let home = home("hung-stdin", Some("127.0.0.1:9"));
    let started = Instant::now();
    let mut child = Command::new(env!("CARGO_BIN_EXE_cctg"))
        .args(["hook", "SessionEnd"])
        .env("USERPROFILE", &home)
        .env("HOME", &home)
        .env_remove("CCTG_HUB_SECRET")
        .env_remove("CCTG_HUB_HOOK_ADDR")
        .env_remove("CCTG_HOST")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("cctg starts");
    // Held open, never written, until the hook has exited.
    let stdin = child.stdin.take().unwrap();
    let output = child.wait_with_output().unwrap();
    let elapsed = started.elapsed();
    drop(stdin);
    assert_quiet(&output, &[]);
    assert!(
        String::from_utf8_lossy(&output.stderr).contains("hook input unreadable"),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(elapsed < Duration::from_millis(1200), "{elapsed:?}");
}

#[test]
fn settings_snippet_registers_every_event_without_secrets_or_paths() {
    let path = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../docs/hook-settings.json");
    let text = std::fs::read_to_string(path).expect("docs/hook-settings.json");
    let settings: serde_json::Value = serde_json::from_str(&text).unwrap();
    let hooks = settings["hooks"].as_object().unwrap();
    let mut events: Vec<&str> = hooks.keys().map(String::as_str).collect();
    events.sort_unstable();
    assert_eq!(
        events,
        [
            "PermissionRequest",
            "PostToolUse",
            "SessionEnd",
            "SessionStart",
            "Stop",
            "SubagentStart",
            "SubagentStop",
            "UserPromptSubmit"
        ]
    );
    for (event, groups) in hooks {
        let groups = groups.as_array().unwrap();
        assert_eq!(groups.len(), 1, "{event}");
        let matcher = groups[0].get("matcher").and_then(|m| m.as_str());
        assert_eq!(
            matcher,
            (event == "PostToolUse").then_some("SubagentHandback"),
            "{event}"
        );
        let commands = groups[0]["hooks"].as_array().unwrap();
        assert_eq!(commands.len(), 1, "{event}");
        assert_eq!(commands[0]["type"], "command");
        assert_eq!(commands[0]["command"], format!("cctg hook {event}"));
        // Only the waiting hook needs more than Claude Code's default time.
        let timeout = commands[0].get("timeout").and_then(|t| t.as_u64());
        assert_eq!(
            timeout,
            (event == "PermissionRequest").then_some(100),
            "{event}"
        );
    }
    for needle in [
        "CCTG_",
        "SECRET",
        "secret",
        ":\\",
        "/",
        "\\\\",
        "~",
        "127.0.0.1",
    ] {
        assert!(!text.contains(needle), "{needle}");
    }
}
