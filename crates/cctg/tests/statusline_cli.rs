//! `cctg statusline` as Claude Code runs it (TASK-029): a process with the
//! status line JSON on stdin, configured through `<home>/.cctg/device.env`,
//! chaining the `statusLine.command` of `<home>/.claude/settings.json`.
//! What it prints is what the terminal shows, so a configured command's
//! output must pass byte for byte.

use std::io::Write;
use std::net::{Ipv4Addr, SocketAddr};
use std::path::{Path, PathBuf};
use std::process::{Output, Stdio};
use std::time::{Duration, Instant};

use cctg::hub::ingress;
use cctg::wire::{HookEvent, HookPost, Secret};
use tokio::sync::mpsc;

mod common;

const SECRET: &str = "statusline-cli-secret-0123456789";

/// A status line input with bytes a lossy pass would change: no final
/// newline, non-ASCII text, CRLF inside a string.
const INPUT: &str = "{\"session_id\":\"5e551017-0000-4000-8000-000000000031\",\"model\":{\"display_name\":\"Опус 5.5\"},\"effort\":{\"level\":\"high\"},\"context_window\":{\"used_percentage\":50.4},\"rate_limits\":{\"five_hour\":{\"used_percentage\":3},\"seven_day\":{\"used_percentage\":91.6}},\"note\":\"a\\r\\nb\"}";

/// A home whose `.cctg/device.env` points at `addr` and whose Claude Code
/// user settings carry `command` as the status line.
fn home(test: &str, addr: &str, command: Option<&str>) -> PathBuf {
    let home = Path::new(env!("CARGO_TARGET_TMPDIR")).join(format!("statusline-cli-{test}"));
    let _ = std::fs::remove_dir_all(&home);
    std::fs::create_dir_all(home.join(".cctg")).unwrap();
    std::fs::create_dir_all(home.join(".claude")).unwrap();
    std::fs::write(
        home.join(".cctg").join("device.env"),
        format!("CCTG_HUB_SECRET={SECRET}\nCCTG_HUB_HOOK_ADDR={addr}\nCCTG_HOST=box\n"),
    )
    .unwrap();
    if let Some(command) = command {
        let settings =
            serde_json::json!({ "statusLine": { "type": "command", "command": command } });
        std::fs::write(
            home.join(".claude").join("settings.json"),
            settings.to_string(),
        )
        .unwrap();
    }
    home
}

/// Runs `cctg statusline`; the exit code is left to the caller.
fn run(home: &Path, extra_env: &[(&str, &str)]) -> (Output, Duration) {
    let started = Instant::now();
    let mut command = common::cctg(home);
    command
        .arg("statusline")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    for (name, value) in extra_env {
        command.env(name, value);
    }
    let mut child = command.spawn().expect("cctg starts");
    let mut input = child.stdin.take().unwrap();
    input.write_all(INPUT.as_bytes()).unwrap();
    drop(input);
    let output = child.wait_with_output().unwrap();
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(!stderr.contains(SECRET), "{stderr}");
    (output, started.elapsed())
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

/// Claude Code runs status line commands through Git Bash on Windows; these
/// tests need it (as Claude Code on Windows does).
fn has_shell() -> bool {
    !cfg!(windows)
        || cctg::statusline::git_bash(&|name| std::env::var(name).ok(), &|path| path.is_file())
            .is_some()
}

async fn in_blocking(home: PathBuf, extra_env: Vec<(&'static str, &'static str)>) -> Output {
    tokio::task::spawn_blocking(move || run(&home, &extra_env).0)
        .await
        .unwrap()
}

#[tokio::test(flavor = "multi_thread")]
async fn the_users_command_gets_the_same_stdin_and_its_output_passes_byte_for_byte() {
    if !has_shell() {
        eprintln!("no Git Bash: skipped");
        return;
    }
    let (addr, mut events) = hub().await;
    let home = home("cat", &addr, Some("cat"));
    let output = in_blocking(home, Vec::new()).await;
    assert_eq!(output.stdout, INPUT.as_bytes());
    assert_eq!(output.status.code(), Some(0));
    // The numbers went to the hub, rounded, without the input's other text.
    let post = tokio::time::timeout(Duration::from_secs(5), events.recv())
        .await
        .expect("hub got the numbers")
        .unwrap();
    assert_eq!(post.session_id, "5e551017-0000-4000-8000-000000000031");
    assert_eq!(
        post.event,
        HookEvent::StatusLine {
            model: Some("Опус 5.5".into()),
            effort: Some("high".into()),
            context: Some(50),
            five_hour: Some(3),
            seven_day: Some(92),
        }
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn colours_lines_a_failing_exit_and_empty_output_pass_unchanged() {
    if !has_shell() {
        eprintln!("no Git Bash: skipped");
        return;
    }
    let (addr, _events) = hub().await;
    let home = home("ansi", &addr, Some(r"printf 'a\033[31mb\nc'; exit 3"));
    let output = in_blocking(home, Vec::new()).await;
    assert_eq!(output.stdout, b"a\x1b[31mb\nc");
    // Its exit code too: Claude Code blanks the line on a non-zero one.
    assert_eq!(output.status.code(), Some(3));
    // A command that prints nothing shows nothing, not cctg's line.
    let home = self::home("empty", &addr, Some("true"));
    let output = in_blocking(home, Vec::new()).await;
    assert_eq!(output.stdout, b"");
    assert_eq!(output.status.code(), Some(0));
}

#[tokio::test(flavor = "multi_thread")]
async fn without_a_command_or_inside_one_cctg_prints_its_own_line() {
    let (addr, mut events) = hub().await;
    let home = home("own", &addr, None);
    let output = in_blocking(home, Vec::new()).await;
    assert_eq!(output.status.code(), Some(0));
    assert_eq!(
        String::from_utf8(output.stdout).unwrap(),
        "Опус 5.5 · ctx 50% · 5h 3% · 7d 92%"
    );
    tokio::time::timeout(Duration::from_secs(5), events.recv())
        .await
        .expect("hub got the numbers");
    // Run from inside the user's command: no chain again, no second POST.
    let home = self::home("nested", &addr, Some("cat"));
    let output = in_blocking(home, vec![("CCTG_STATUSLINE", "1")]).await;
    assert_eq!(
        String::from_utf8(output.stdout).unwrap(),
        "Опус 5.5 · ctx 50% · 5h 3% · 7d 92%"
    );
    assert!(
        tokio::time::timeout(Duration::from_millis(500), events.recv())
            .await
            .is_err()
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn a_stopped_hub_adds_well_under_150_ms() {
    let (addr, _events) = hub().await;
    // A port nobody listens on: on Windows a connect there lasts until the
    // client gives up.
    let closed = {
        let listener = std::net::TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).unwrap();
        listener.local_addr().unwrap().to_string()
    };
    let up = home("up", &addr, None);
    let down = home("down", &closed, None);
    let best = |home: PathBuf| (0..5).map(|_| run(&home, &[]).1).min().unwrap();
    let (up, down) = tokio::task::spawn_blocking(move || (best(up), best(down)))
        .await
        .unwrap();
    let extra = down.saturating_sub(up);
    assert!(
        extra < Duration::from_millis(150),
        "hub up {up:?}, down {down:?}"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn a_command_that_prints_too_much_is_cut_without_waiting_for_the_timeout() {
    if !has_shell() {
        eprintln!("no Git Bash: skipped");
        return;
    }
    let (addr, _events) = hub().await;
    let home = home(
        "large",
        &addr,
        Some(r"head -c 300000 /dev/zero | tr '\0' a"),
    );
    let (output, took) = tokio::task::spawn_blocking(move || run(&home, &[]))
        .await
        .unwrap();
    assert_eq!(output.status.code(), Some(0));
    assert_eq!(output.stdout.len(), 64 << 10);
    assert!(output.stdout.iter().all(|byte| *byte == b'a'));
    assert!(took < Duration::from_secs(5), "{took:?}");
}
