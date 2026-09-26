//! `cctg hook <event>` as Claude Code runs it: a process with JSON on stdin,
//! configured through `<home>/.cctg/device.env`, talking to a real hub hook
//! endpoint. Exit code, stdout and stderr are what Claude Code sees.

use std::io::Write;
use std::net::{Ipv4Addr, SocketAddr};
use std::path::{Path, PathBuf};
use std::process::{Output, Stdio};
use std::time::{Duration, Instant};

use cctg::hub::ingress;
use cctg::wire::{HookEvent, HookPost, Secret};
use tokio::net::TcpListener;
use tokio::sync::mpsc;

mod common;

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
    let home = common::own_tmp().join(format!("hook-cli-{test}"));
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
    let mut child = common::cctg(home)
        .args(["hook", event])
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

/// A `PreCompact` input of `/compact` with the user's own text.
fn pre_compact() -> Vec<u8> {
    serde_json::json!({
        "session_id": "5e551017-0000-4000-8000-000000000053",
        "transcript_path": "/p/s.jsonl",
        "cwd": "/w",
        "hook_event_name": "PreCompact",
        "trigger": "manual",
        "custom_instructions": "private compact focus",
    })
    .to_string()
    .into_bytes()
}

/// A hub before TASK-053 does not know the event and answers 400: the hook
/// still exits 0 with nothing on stdout (the compaction goes on) and keeps
/// nothing.
#[tokio::test(flavor = "multi_thread")]
async fn a_hub_without_compactions_leaves_the_hook_quiet() {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    let listener = TcpListener::bind(SocketAddr::from((Ipv4Addr::LOCALHOST, 0)))
        .await
        .unwrap();
    let addr = listener.local_addr().unwrap().to_string();
    tokio::spawn(async move {
        while let Ok((mut stream, _)) = listener.accept().await {
            // The whole request is read first: closing with unread bytes
            // resets the connection on Windows and the answer is lost.
            let mut request = Vec::new();
            let mut chunk = vec![0u8; 64 * 1024];
            let mut want = usize::MAX;
            while request.len() < want {
                let Ok(n) = stream.read(&mut chunk).await else {
                    break;
                };
                if n == 0 {
                    break;
                }
                request.extend_from_slice(&chunk[..n]);
                if let Some(end) = request.windows(4).position(|w| w == b"\r\n\r\n") {
                    let head = String::from_utf8_lossy(&request[..end]).into_owned();
                    let length: usize = head
                        .lines()
                        .find_map(|line| line.strip_prefix("Content-Length: "))
                        .and_then(|value| value.parse().ok())
                        .unwrap_or(0);
                    want = end + 4 + length;
                }
            }
            let _ = stream
                .write_all(b"HTTP/1.1 400 Bad Request\r\nContent-Length: 0\r\n\r\n")
                .await;
            let _ = stream.shutdown().await;
        }
    });
    let home = home("old-hub-compact", Some(&addr));
    let (output, elapsed) =
        tokio::task::spawn_blocking(move || run_hook(&home, "PreCompact", &pre_compact()))
            .await
            .unwrap();
    assert_quiet(&output, &["private compact focus"]);
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("HTTP 400"), "{stderr}");
    assert!(!stderr.contains("kept"), "{stderr}");
    assert!(elapsed < Duration::from_secs(3), "{elapsed:?}");
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
        ("PreCompact", pre_compact(), "pre_compact"),
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
    let mut child = common::cctg(Path::new(env!("CARGO_TARGET_TMPDIR")))
        .args(["hook", "Stop"])
        .env_remove("USERPROFILE")
        .env_remove("HOME")
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
        let output = common::cctg(&home("bad-args", None))
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
    let mut child = common::cctg(&home)
        .args(["hook", "SessionEnd"])
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
            "PostToolUseFailure",
            "PreCompact",
            "PreToolUse",
            "SessionEnd",
            "SessionStart",
            "Stop",
            "SubagentStart",
            "SubagentStop",
            "UserPromptSubmit"
        ]
    );
    // The status message (TASK-029): the status line command and the tool
    // status hook, which runs in the background for every tool.
    assert_eq!(
        settings["statusLine"],
        serde_json::json!({ "type": "command", "command": "cctg statusline" })
    );
    let mut tool_status = 0;
    for (event, groups) in hooks {
        for group in groups.as_array().unwrap() {
            let matcher = group.get("matcher").and_then(|m| m.as_str());
            let commands = group["hooks"].as_array().unwrap();
            assert_eq!(commands.len(), 1, "{event}");
            assert_eq!(commands[0]["type"], "command");
            let timeout = commands[0].get("timeout").and_then(|t| t.as_u64());
            if commands[0]["command"] == "cctg hook ToolStatus" {
                tool_status += 1;
                assert!(
                    ["PreToolUse", "PostToolUse", "PostToolUseFailure"].contains(&event.as_str()),
                    "{event}"
                );
                assert_eq!(matcher, None, "{event}");
                assert_eq!(commands[0]["async"], true, "{event}");
                assert_eq!(timeout, None, "{event}");
                continue;
            }
            let wanted = match event.as_str() {
                "PostToolUse" => Some("SubagentHandback"),
                "PreToolUse" => Some("AskUserQuestion"),
                _ => None,
            };
            assert_eq!(matcher, wanted, "{event}");
            assert_eq!(commands[0]["command"], format!("cctg hook {event}"));
            assert_eq!(commands[0].get("async"), None, "{event}");
            // Only the waiting hooks need more than Claude Code's default
            // time; the compaction waits for its hook, so that one gets less.
            let wait = match event.as_str() {
                "PermissionRequest" => Some(100),
                "PreToolUse" => Some(330),
                "PreCompact" => Some(5),
                _ => None,
            };
            assert_eq!(timeout, wait, "{event}");
            assert_eq!(
                commands[0].get("statusMessage").is_some(),
                event == "PreToolUse",
                "{event}"
            );
        }
    }
    assert_eq!(tool_status, 3);
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
