//! `cctg run` with real processes (TASK-040): this test binary is also the
//! claude stand-in (`RUN_E2E_ROLE=claude`, started through `CCTG_CLAUDE`).
//! The first claude asks for a restart the way the worker agent does (a
//! request file named after `CCTG_RUN` with `relaunch_args` of its
//! `CCTG_RUN_ARGS`) and exits; `cctg run` starts it again with exactly those
//! arguments (`--resume <session>`, no prompt); the second exits with code 7
//! and no request, which `cctg run` returns. No console, no window, temp home
//! and state.

use std::io::Write;
use std::process::Stdio;
use std::time::{Duration, Instant};

use serde_json::{Value, json};

mod common;

const SESSION: &str = "5e550000-0000-4000-8000-00000000run1";

fn main() {
    if std::env::var("RUN_E2E_ROLE").as_deref() == Ok("claude") {
        fake_claude();
    }
    if std::env::args().any(|arg| arg == "--list") {
        println!("run_e2e: test");
        return;
    }
    scenario();
    println!("run_e2e: ok");
}

/// Logs its arguments and env, then asks for a restart once.
fn fake_claude() -> ! {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let run = std::env::var("CCTG_RUN").unwrap_or_default();
    let line = json!({
        "args": args,
        "run": run,
        "run_args": std::env::var("CCTG_RUN_ARGS").unwrap_or_default(),
    });
    let mut log = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(std::env::var("RUN_E2E_LOG").unwrap())
        .unwrap();
    writeln!(log, "{line}").unwrap();
    if args.iter().any(|arg| arg == "--resume") {
        std::process::exit(7);
    }
    let state = std::path::PathBuf::from(std::env::var("CCTG_STATE_DIR").unwrap());
    let request = cctg::update::request_path(&state, run.parse().unwrap());
    std::fs::create_dir_all(request.parent().unwrap()).unwrap();
    let first: Vec<String> =
        serde_json::from_str(&std::env::var("CCTG_RUN_ARGS").unwrap()).unwrap();
    let request_args = cctg::update::relaunch_args(&first, SESSION);
    std::fs::write(&request, json!({ "args": request_args }).to_string()).unwrap();
    std::process::exit(0);
}

fn scenario() {
    let root = std::env::temp_dir().join(format!("cctg-run-e2e-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    let (state, home) = (root.join("state"), root.join("home"));
    std::fs::create_dir_all(&state).unwrap();
    std::fs::create_dir_all(&home).unwrap();
    let log = root.join("claude.log");
    let given = [
        "--settings",
        "s.json",
        "--continue",
        "--model",
        "haiku",
        "the first prompt",
    ];
    let mut child = common::cctg(&home)
        .arg("run")
        .arg("--")
        .args(given)
        .env("CCTG_CLAUDE", std::env::current_exe().unwrap())
        .env("RUN_E2E_ROLE", "claude")
        .env("RUN_E2E_LOG", &log)
        .env("CCTG_STATE_DIR", &state)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .spawn()
        .expect("cctg run starts");
    let pid = child.id().to_string();
    let deadline = Instant::now() + Duration::from_secs(30);
    let status = loop {
        if let Some(status) = child.try_wait().unwrap() {
            break status;
        }
        assert!(Instant::now() < deadline, "cctg run ends");
        std::thread::sleep(Duration::from_millis(50));
    };
    assert_eq!(status.code(), Some(7), "claude's own exit code");
    let runs: Vec<Value> = std::fs::read_to_string(&log)
        .unwrap()
        .lines()
        .map(|line| serde_json::from_str(line).unwrap())
        .collect();
    assert_eq!(runs.len(), 2, "{runs:?}");
    assert_eq!(runs[0]["args"], json!(given));
    assert_eq!(
        runs[1]["args"],
        json!([
            "--settings",
            "s.json",
            "--model",
            "haiku",
            "--resume",
            SESSION
        ])
    );
    for run in &runs {
        assert_eq!(run["run"], pid, "CCTG_RUN is the pid of cctg run");
        let run_args: Value = serde_json::from_str(run["run_args"].as_str().unwrap()).unwrap();
        assert_eq!(
            run_args,
            json!(given),
            "CCTG_RUN_ARGS are the first arguments"
        );
    }
    let left = std::fs::read_dir(state.join("restart")).unwrap().count();
    assert_eq!(left, 0, "the request was taken");
    let _ = std::fs::remove_dir_all(&root);
}
