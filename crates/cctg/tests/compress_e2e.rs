//! The helper claude of a group history compression (TASK-077) with real
//! processes: this test binary is also the claude stand-in. `Compressor`
//! starts it with `--safe-mode`, which the test runner never passes; the
//! stand-in logs its arguments, `MAX_THINKING_TOKENS`, its working folder
//! and what came on stdin into `log.json` of that folder, then answers by
//! the role in `role.txt` there. No real claude, no console window
//! (`Compressor` starts it without one), everything in a temp folder.

use std::io::{BufRead, BufReader, Read, Write};
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::time::{Duration, Instant};

use cctg::compress::Compressor;
use cctg::wire::{AgentMsg, HubMsg, SessionAnswer, SessionAsk, encode};
use serde_json::{Value, json};
use tokio::sync::mpsc;

mod common;

/// What `Compressor` passes before the system prompt.
const FLAGS: [&str; 11] = [
    "-p",
    "--safe-mode",
    "--no-session-persistence",
    "--strict-mcp-config",
    "--tools",
    "",
    "--model",
    "haiku",
    "--output-format",
    "json",
    "--system-prompt",
];
/// The `slow` role's sleep; the test's timeout is shorter.
const SLOW: Duration = Duration::from_secs(4);

fn main() {
    if std::env::args().any(|arg| arg == "--safe-mode") {
        stand_in();
    }
    if std::env::args().any(|arg| arg == "--list") {
        println!("compress_e2e: test");
        return;
    }
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    runtime.block_on(scenario());
    println!("compress_e2e: ok");
}

/// FNV-1a of `bytes`: stdin arrived whole.
fn fnv(bytes: &[u8]) -> u64 {
    bytes.iter().fold(0xcbf2_9ce4_8422_2325, |hash, byte| {
        (hash ^ u64::from(*byte)).wrapping_mul(0x0100_0000_01b3)
    })
}

fn stand_in() -> ! {
    let work = std::env::current_dir().unwrap();
    let role = std::fs::read_to_string(work.join("role.txt")).unwrap_or_default();
    let mut input = Vec::new();
    std::io::stdin().read_to_end(&mut input).unwrap();
    let log = json!({
        "args": std::env::args().skip(1).collect::<Vec<_>>(),
        "thinking": std::env::var("MAX_THINKING_TOKENS").ok(),
        "cwd": work,
        "stdin_len": input.len(),
        "stdin_fnv": fnv(&input).to_string(),
    });
    std::fs::write(work.join("log.json"), log.to_string()).unwrap();
    let answer = |result: &str, is_error: bool| {
        let line = json!({ "type": "result", "result": result, "is_error": is_error });
        let mut out = std::io::stdout();
        writeln!(out, "{line}").unwrap();
        out.flush().unwrap();
    };
    match role.trim() {
        "ok" => answer("сводка", false),
        "heading" => answer("**Итог:**\nсводка", false),
        "error" => answer("сводка", true),
        "empty" => answer("", false),
        "garbage" => println!("not json"),
        "exit1" => {
            answer("сводка", false);
            std::process::exit(1);
        }
        "slow" => {
            std::thread::sleep(SLOW);
            std::fs::write(work.join("survived"), "").unwrap();
            answer("сводка", false);
        }
        other => panic!("unknown role {other:?}"),
    }
    std::process::exit(0);
}

/// A folder of its own for `role`.
fn case(root: &Path, role: &str) -> PathBuf {
    let work = root.join(role);
    std::fs::create_dir_all(&work).unwrap();
    std::fs::write(work.join("role.txt"), role).unwrap();
    work
}

fn compressor(work: &Path, timeout: Duration) -> Compressor {
    Compressor {
        program: std::env::current_exe().unwrap(),
        work: Some(work.to_owned()),
        timeout,
    }
}

fn logged(work: &Path) -> Value {
    serde_json::from_str(&std::fs::read_to_string(work.join("log.json")).unwrap()).unwrap()
}

async fn scenario() {
    let root =
        Path::new(env!("CARGO_TARGET_TMPDIR")).join(format!("compress-e2e-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    std::fs::create_dir_all(&root).unwrap();
    let timeout = Duration::from_secs(30);

    // The exact flags, thinking off, the session's folder, and 64 KiB of
    // Cyrillic on stdin, whole.
    let work = case(&root, "ok");
    let history = "я".repeat(32 * 1024);
    let summary = compressor(&work, timeout).run(&history, 4000).await;
    assert_eq!(summary.as_deref(), Some("сводка"));
    let log = logged(&work);
    let args: Vec<&str> = log["args"]
        .as_array()
        .unwrap()
        .iter()
        .map(|arg| arg.as_str().unwrap())
        .collect();
    assert_eq!(args.len(), FLAGS.len() + 1, "{args:?}");
    assert_eq!(args[..FLAGS.len()], FLAGS, "{args:?}");
    assert!(
        args[FLAGS.len()].contains("At most 4000 characters"),
        "{args:?}"
    );
    assert_eq!(log["thinking"], "0");
    assert_eq!(
        std::fs::canonicalize(log["cwd"].as_str().unwrap()).unwrap(),
        std::fs::canonicalize(&work).unwrap()
    );
    assert_eq!(log["stdin_len"], history.len());
    assert_eq!(log["stdin_fnv"], fnv(history.as_bytes()).to_string());

    // A heading goes; every failure is `None`.
    let work = case(&root, "heading");
    assert_eq!(
        compressor(&work, timeout).run("x", 5).await.as_deref(),
        Some("сводка")
    );
    for role in ["error", "empty", "garbage", "exit1"] {
        let work = case(&root, role);
        assert_eq!(compressor(&work, timeout).run("x", 5).await, None, "{role}");
        assert!(work.join("log.json").is_file(), "{role} ran");
    }

    // Too slow: killed at the timeout, it never finishes its work.
    let work = case(&root, "slow");
    let started = std::time::Instant::now();
    assert_eq!(
        compressor(&work, Duration::from_secs(1)).run("x", 5).await,
        None
    );
    assert!(started.elapsed() < SLOW, "{:?}", started.elapsed());
    tokio::time::sleep(SLOW + Duration::from_secs(2)).await;
    assert!(
        !work.join("survived").exists(),
        "the slow run was not killed"
    );

    // The agent's worker: the summary as text pieces for the read id, or
    // `unreadable` without a compressor or on a failure.
    let (outbox, mut hub) = mpsc::channel(8);
    let jobs = cctg::agent::spawn_compressor(outbox, Some(compressor(&root.join("ok"), timeout)));
    jobs.requests.send((7, "Анна: да".into(), 5)).await.unwrap();
    let answer = tokio::time::timeout(timeout, hub.recv()).await.unwrap();
    assert_eq!(
        answer,
        Some(AgentMsg::SessionAnswer {
            read_id: 7,
            answer: SessionAnswer::Text {
                text: "сводка".into(),
                more: false
            }
        })
    );
    let (outbox, mut hub) = mpsc::channel(8);
    let jobs =
        cctg::agent::spawn_compressor(outbox, Some(compressor(&root.join("error"), timeout)));
    jobs.requests.send((8, "x".into(), 5)).await.unwrap();
    let answer = tokio::time::timeout(timeout, hub.recv()).await.unwrap();
    assert_eq!(
        answer,
        Some(AgentMsg::SessionAnswer {
            read_id: 8,
            answer: SessionAnswer::Unreadable
        })
    );
    let (outbox, mut hub) = mpsc::channel(8);
    let jobs = cctg::agent::spawn_compressor(outbox, None);
    jobs.requests.send((9, "x".into(), 5)).await.unwrap();
    let answer = tokio::time::timeout(timeout, hub.recv()).await.unwrap();
    assert_eq!(
        answer,
        Some(AgentMsg::SessionAnswer {
            read_id: 9,
            answer: SessionAnswer::Unreadable
        })
    );

    // The worker agent ends (Claude Code closed its stdin) while its helper
    // runs: the helper goes with it, though the worker leaves by
    // `std::process::exit` (TASK-077 review F4).
    let work = root.join("orphan");
    std::fs::create_dir_all(&work).unwrap();
    std::fs::write(work.join("role.txt"), "slow").unwrap();
    let home = root.join("home");
    std::fs::create_dir_all(&home).unwrap();
    let hub = asking_hub();
    let mut command = common::cctg(&home);
    command
        .arg("agent-worker")
        .current_dir(&work)
        .env("CCTG_CLAUDE", std::env::current_exe().unwrap())
        .env("CCTG_HUB_SECRET", SECRET)
        .env("CCTG_HUB_AGENT_ADDR", &hub)
        .env("CLAUDE_CODE_SESSION_ID", SESSION)
        .env("CLAUDE_CODE_ENTRYPOINT", "cli")
        .stdin(Stdio::piped())
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    let mut agent = common::spawn(&mut command).unwrap();
    let asked = Instant::now();
    while !work.join("log.json").is_file() {
        assert!(asked.elapsed() < timeout, "the helper never started");
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    let started = Instant::now();
    drop(agent.stdin.take());
    assert!(agent.wait().unwrap().success());
    tokio::time::sleep((SLOW + Duration::from_secs(2)).saturating_sub(started.elapsed())).await;
    assert!(
        !work.join("survived").exists(),
        "the helper outlived its agent"
    );

    let _ = std::fs::remove_dir_all(&root);
}

const SECRET: &str = "compress-e2e-secret-0123456789";
const SESSION: &str = "5e551077-0000-4000-8000-0000000000f4";

/// A hub on loopback that registers the agent and asks it at once to
/// compress a history; the link stays open until the agent goes.
fn asking_hub() -> String {
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let addr = listener.local_addr().unwrap().to_string();
    std::thread::spawn(move || {
        for stream in listener.incoming().flatten() {
            let Ok(read) = stream.try_clone() else {
                continue;
            };
            let mut reader = BufReader::new(read);
            // `hello`, then `register`.
            let mut line = String::new();
            for _ in 0..2 {
                line.clear();
                let _ = reader.read_line(&mut line);
            }
            let mut stream = stream;
            let registered = HubMsg::Registered {
                files: false,
                albums: false,
                heartbeat: false,
            };
            let ask = HubMsg::SessionRead {
                read_id: 1,
                session_id: SESSION.into(),
                ask: SessionAsk::Compress {
                    text: "Анна: да".into(),
                    limit: 5,
                },
            };
            let _ = stream.write_all(&encode(&registered));
            let _ = stream.write_all(&encode(&ask));
            let _ = std::io::copy(&mut reader, &mut std::io::sink());
        }
    });
    addr
}
