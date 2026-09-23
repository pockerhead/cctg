//! `cctg agent` as Claude Code runs it: JSON-RPC over the real stdin and
//! stdout of the binary. stdout must carry one JSON object per line and
//! nothing else, also while the hub link fails and logs.

use std::io::Write;
use std::net::TcpListener;
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use serde_json::Value;

const SECRET: &str = "agent-stdio-secret-0123456789";

/// A hub address that accepts and hangs up at once: the agent's handshake
/// fails fast and it logs a warning.
fn slamming_hub() -> String {
    let listener = TcpListener::bind("127.0.0.1:0").expect("bind");
    let addr = listener.local_addr().expect("addr").to_string();
    std::thread::spawn(move || {
        for stream in listener.incoming() {
            drop(stream);
        }
    });
    addr
}

struct Run {
    status_ok: bool,
    stdout: Vec<Value>,
    stdout_raw: String,
    stderr: String,
}

fn run_agent(envs: &[(&str, &str)], lines: &[&str], linger: Duration) -> Run {
    let home = std::path::Path::new(env!("CARGO_TARGET_TMPDIR")).join("agent-stdio-home");
    std::fs::create_dir_all(&home).expect("home");
    let mut command = Command::new(env!("CARGO_BIN_EXE_cctg"));
    command
        .arg("agent")
        .current_dir(&home)
        // Never the developer's own device config or session.
        .env("USERPROFILE", &home)
        .env("HOME", &home)
        .env_remove("CCTG_HUB_SECRET")
        .env_remove("CCTG_HUB_AGENT_ADDR")
        .env_remove("CLAUDE_CODE_SESSION_ID")
        .env_remove("CLAUDE_CODE_ENTRYPOINT")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    for (key, value) in envs {
        command.env(key, value);
    }
    let mut child = command.spawn().expect("cctg starts");
    let mut stdin = child.stdin.take().expect("stdin");
    for line in lines {
        stdin.write_all(line.as_bytes()).expect("write");
        stdin.write_all(b"\n").expect("write");
    }
    stdin.flush().expect("flush");
    std::thread::sleep(linger);
    drop(stdin);
    let started = Instant::now();
    let output = child.wait_with_output().expect("cctg ends");
    assert!(
        started.elapsed() < Duration::from_secs(5),
        "closing stdin ends the agent"
    );
    let stdout_raw = String::from_utf8(output.stdout).expect("utf-8 stdout");
    let stdout = stdout_raw
        .lines()
        .map(|line| {
            let value: Value = serde_json::from_str(line)
                .unwrap_or_else(|_| panic!("stdout line is not JSON: {line:?}"));
            assert!(value.is_object(), "{line}");
            assert_eq!(value["jsonrpc"], "2.0", "{line}");
            value
        })
        .collect();
    Run {
        status_ok: output.status.success(),
        stdout,
        stdout_raw,
        stderr: String::from_utf8_lossy(&output.stderr).into_owned(),
    }
}

const SCRIPT: &[&str] = &[
    r#"{"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"2025-11-25","capabilities":{}}}"#,
    r#"{"jsonrpc":"2.0","method":"notifications/initialized"}"#,
    r#"{"jsonrpc":"2.0","id":6,"method":"ping"}"#,
    r#"{"jsonrpc":"2.0","id":2,"method":"tools/list"}"#,
    r#"{"jsonrpc":"2.0","id":3,"method":"tools/call","params":{"name":"reply","arguments":{"text":"hi"}}}"#,
    r#"{"jsonrpc":"2.0","id":4,"method":"prompts/list"}"#,
    "this is not json {",
    "",
    r#"{"id":7,"method":"tools/list"}"#,
    r#"{"jsonrpc":"2.0","id":8,"method":"tools/list","params":5}"#,
    r#"{"jsonrpc":"2.0","id":5,"method":"tools/list"}"#,
];

fn by_id(run: &Run, id: i64) -> &Value {
    run.stdout
        .iter()
        .find(|value| value["id"] == id)
        .unwrap_or_else(|| panic!("no answer for id {id}: {}", run.stdout_raw))
}

#[test]
fn a_full_session_with_a_failing_hub_keeps_stdout_pure() {
    let hub = slamming_hub();
    let run = run_agent(
        &[
            ("CCTG_HUB_SECRET", SECRET),
            ("CCTG_HUB_AGENT_ADDR", &hub),
            (
                "CLAUDE_CODE_SESSION_ID",
                "5e551017-0000-4000-8000-00000000abcd",
            ),
            ("CLAUDE_CODE_ENTRYPOINT", "cli"),
        ],
        SCRIPT,
        Duration::from_millis(800),
    );
    assert!(run.status_ok, "{}", run.stderr);
    // Nine answers (ids 1-6 and 8, one parse error, one invalid request
    // without an id), no notification.
    assert_eq!(run.stdout.len(), 9, "{}", run.stdout_raw);
    assert_eq!(
        by_id(&run, 1)["result"]["capabilities"]["experimental"]["claude/channel"],
        serde_json::json!({})
    );
    assert_eq!(by_id(&run, 2)["result"]["tools"][0]["name"], "reply");
    assert_eq!(by_id(&run, 3)["result"]["isError"], false);
    assert_eq!(by_id(&run, 4)["error"]["code"], -32601);
    assert_eq!(by_id(&run, 5)["result"]["tools"][0]["name"], "reply");
    assert_eq!(by_id(&run, 6)["result"], serde_json::json!({}));
    assert_eq!(by_id(&run, 8)["error"]["code"], -32600);
    for code in [-32700, -32600] {
        assert!(
            run.stdout
                .iter()
                .any(|value| value["id"].is_null() && value["error"]["code"] == code),
            "{}",
            run.stdout_raw
        );
    }
    // The failing link did log, and only to stderr.
    assert!(run.stderr.contains("hub link"), "{}", run.stderr);
    for leaked in ["hub link", "WARN", "INFO", SECRET] {
        assert!(!run.stdout_raw.contains(leaked), "{leaked} on stdout");
    }
    assert!(!run.stderr.contains(SECRET));
}

#[test]
fn headless_and_unconfigured_agents_answer_without_a_hub() {
    let hub = slamming_hub();
    for envs in [
        vec![
            ("CCTG_HUB_SECRET", SECRET),
            ("CCTG_HUB_AGENT_ADDR", hub.as_str()),
            (
                "CLAUDE_CODE_SESSION_ID",
                "5e551017-0000-4000-8000-00000000abcd",
            ),
            ("CLAUDE_CODE_ENTRYPOINT", "sdk-cli"),
        ],
        vec![(
            "CLAUDE_CODE_SESSION_ID",
            "5e551017-0000-4000-8000-00000000abcd",
        )],
        vec![("CCTG_HUB_SECRET", SECRET)],
    ] {
        let run = run_agent(&envs, SCRIPT, Duration::from_millis(200));
        assert!(run.status_ok, "{}", run.stderr);
        assert_eq!(run.stdout.len(), 9, "{}", run.stdout_raw);
        assert_eq!(by_id(&run, 3)["result"]["isError"], true);
        assert!(
            !run.stderr.contains("hub link not established"),
            "{}",
            run.stderr
        );
    }
}
