//! TASK-058: the status line numbers reach a hub that is slower than the
//! status line may wait, through the real `cctg agent` of the session. The
//! real `cctg statusline` keeps them in `<state>/status/<session>.json`; the
//! agent sends them over its link after the hub told it its session
//! (`bound`). The hub here is a hand-written one that answers the handshake
//! only after [`DELAY`], well past the 80 ms the status line gives a POST.
//! With a hub that never says `bound` (built before TASK-058) the status
//! line posts the numbers itself, as before, to a hub on this machine.

use std::io::Write;
use std::net::{Ipv4Addr, SocketAddr, TcpListener as StdListener};
use std::path::{Path, PathBuf};
use std::process::{Child, Stdio};
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::{Duration, Instant};

use cctg::wire::{self, AgentMsg, HubMsg};
use tokio::io::BufReader;
use tokio::net::TcpListener;
use tokio::sync::mpsc;

mod common;

const SECRET: &str = "statusline-agent-secret-0123456789";
const SESSION: &str = "5e551017-0000-4000-8000-000000000058";
/// The hub's answer to the handshake comes this late.
const DELAY: Duration = Duration::from_millis(300);
const WAIT: Duration = Duration::from_secs(20);
const INPUT: &str = "{\"session_id\":\"5e551017-0000-4000-8000-000000000058\",\"model\":{\"display_name\":\"Opus 5.5\"},\"effort\":{\"level\":\"high\"},\"context_window\":{\"used_percentage\":50.4},\"rate_limits\":{\"five_hour\":{\"used_percentage\":3},\"seven_day\":{\"used_percentage\":91.6}}}";

/// A hub that accepts one agent, answers its handshake after [`DELAY`],
/// says `bound` for [`SESSION`] when `bound`, and hands every agent message
/// to the returned receiver.
async fn slow_hub(bound: bool) -> (String, mpsc::Receiver<AgentMsg>) {
    let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).await.unwrap();
    let addr = listener.local_addr().unwrap().to_string();
    let (tx, rx) = mpsc::channel(64);
    tokio::spawn(async move {
        let (stream, _) = listener.accept().await.unwrap();
        let (read, mut write) = stream.into_split();
        let mut reader = BufReader::new(read);
        let mut line = Vec::new();
        loop {
            line.clear();
            if wire::read_line(&mut reader, &mut line).await.is_err() {
                return;
            }
            let Ok(msg) = wire::decode::<AgentMsg>(&line) else {
                continue;
            };
            let register = matches!(msg, AgentMsg::Register(_));
            if tx.send(msg).await.is_err() {
                return;
            }
            if register {
                tokio::time::sleep(DELAY).await;
                let registered = HubMsg::Registered {
                    files: false,
                    heartbeat: false,
                };
                wire::write_msg(&mut write, &registered).await.unwrap();
                if bound {
                    tokio::time::sleep(DELAY).await;
                    let bound = HubMsg::Bound {
                        session_id: SESSION.into(),
                    };
                    wire::write_msg(&mut write, &bound).await.unwrap();
                }
            }
        }
    });
    (addr, rx)
}

/// A hook endpoint on this machine that counts the connections made to it.
fn counting_hook() -> (String, Arc<AtomicUsize>) {
    let listener = StdListener::bind(SocketAddr::from((Ipv4Addr::LOCALHOST, 0))).unwrap();
    let addr = listener.local_addr().unwrap().to_string();
    let count = Arc::new(AtomicUsize::new(0));
    let counted = count.clone();
    std::thread::spawn(move || {
        for stream in listener.incoming() {
            counted.fetch_add(1, Ordering::SeqCst);
            drop(stream);
        }
    });
    (addr, count)
}

fn home(test: &str, agent_addr: &str, hook_addr: &str) -> PathBuf {
    let home = Path::new(env!("CARGO_TARGET_TMPDIR")).join(format!("statusline-agent-{test}"));
    let _ = std::fs::remove_dir_all(&home);
    std::fs::create_dir_all(home.join(".cctg")).unwrap();
    std::fs::write(
        home.join(".cctg").join("device.env"),
        format!(
            "CCTG_HUB_SECRET={SECRET}\nCCTG_HUB_AGENT_ADDR={agent_addr}\n\
             CCTG_HUB_HOOK_ADDR={hook_addr}\nCCTG_HOST=box\n"
        ),
    )
    .unwrap();
    home
}

fn status_file(home: &Path, ext: &str) -> PathBuf {
    home.join(".cctg")
        .join("status")
        .join(format!("{SESSION}.{ext}"))
}

/// The session's `cctg agent`; its stdin stays open until the caller drops it.
fn agent(home: &Path) -> Child {
    common::cctg(home)
        .arg("agent")
        .current_dir(home)
        .env("CLAUDE_CODE_SESSION_ID", SESSION)
        .stdin(Stdio::piped())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .expect("cctg agent starts")
}

/// Runs `cctg statusline` with [`INPUT`]: its exit code and how long it took.
fn statusline(home: &Path) -> (Option<i32>, Duration) {
    let started = Instant::now();
    let mut child = common::cctg(home)
        .current_dir(home)
        .env("GIT_CEILING_DIRECTORIES", home.parent().unwrap())
        .arg("statusline")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("cctg statusline starts");
    let mut stdin = child.stdin.take().unwrap();
    stdin.write_all(INPUT.as_bytes()).unwrap();
    drop(stdin);
    let output = child.wait_with_output().unwrap();
    assert!(!String::from_utf8_lossy(&output.stderr).contains(SECRET));
    (output.status.code(), started.elapsed())
}

/// Ends the agent the way Claude Code does: closes its stdin.
fn stop(mut agent: Child) {
    drop(agent.stdin.take());
    let started = Instant::now();
    while agent.try_wait().unwrap().is_none() {
        assert!(started.elapsed() < WAIT, "the agent did not end");
        std::thread::sleep(Duration::from_millis(20));
    }
}

async fn wait_for(what: &str, ready: impl Fn() -> bool) {
    let started = Instant::now();
    while !ready() {
        assert!(started.elapsed() < WAIT, "{what}");
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
}

/// The next message of the agent that is not a ping.
async fn next(messages: &mut mpsc::Receiver<AgentMsg>, wait: Duration) -> Option<AgentMsg> {
    let deadline = tokio::time::Instant::now() + wait;
    loop {
        match tokio::time::timeout_at(deadline, messages.recv()).await {
            Ok(Some(AgentMsg::Ping)) => {}
            Ok(msg) => return msg,
            Err(_) => return None,
        }
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn a_slow_hub_gets_the_numbers_through_the_agent_and_no_post_is_made() {
    let (agent_addr, mut messages) = slow_hub(true).await;
    let (hook_addr, posts) = counting_hook();
    let home = home("slow", &agent_addr, &hook_addr);
    let agent = agent(&home);
    assert!(matches!(
        next(&mut messages, WAIT).await,
        Some(AgentMsg::Hello { .. })
    ));
    match next(&mut messages, WAIT).await {
        Some(AgentMsg::Register(register)) => {
            assert_eq!(register.session_id, SESSION);
            assert!(register.status_lines);
        }
        other => panic!("no register: {other:?}"),
    }
    // The agent marks the session once the hub said `bound`.
    wait_for("the agent's mark", || status_file(&home, "agent").is_file()).await;

    let (code, took) = {
        let home = home.clone();
        tokio::task::spawn_blocking(move || statusline(&home))
            .await
            .unwrap()
    };
    assert_eq!(code, Some(0));
    // No hub is waited for: only the process and its own line.
    assert!(took < Duration::from_secs(2), "{took:?}");
    match next(&mut messages, WAIT).await {
        Some(AgentMsg::StatusLine {
            session_id,
            model,
            effort,
            context,
            five_hour,
            seven_day,
        }) => {
            assert_eq!(session_id, SESSION);
            assert_eq!(model.as_deref(), Some("Opus 5.5"));
            assert_eq!(effort.as_deref(), Some("high"));
            assert_eq!(
                (context, five_hour, seven_day),
                (Some(50), Some(3), Some(92))
            );
        }
        other => panic!("no numbers: {other:?}"),
    }
    // The same numbers again are not sent twice, and nothing was posted.
    let (code, _) = {
        let home = home.clone();
        tokio::task::spawn_blocking(move || statusline(&home))
            .await
            .unwrap()
    };
    assert_eq!(code, Some(0));
    assert_eq!(next(&mut messages, Duration::from_millis(2500)).await, None);
    assert_eq!(posts.load(Ordering::SeqCst), 0);
    // The file holds the numbers and the session id only.
    let kept = std::fs::read_to_string(status_file(&home, "json")).unwrap();
    assert!(!kept.contains(SECRET) && !kept.contains('@'), "{kept}");

    // The agent leaves with its session's files.
    tokio::task::spawn_blocking(move || stop(agent))
        .await
        .unwrap();
    assert!(!status_file(&home, "json").exists());
    assert!(!status_file(&home, "agent").exists());
}

#[tokio::test(flavor = "multi_thread")]
async fn with_a_hub_before_task_058_the_status_line_posts_as_before() {
    let (agent_addr, mut messages) = slow_hub(false).await;
    let (hook_addr, posts) = counting_hook();
    let home = home("old-hub", &agent_addr, &hook_addr);
    let agent = agent(&home);
    assert!(matches!(
        next(&mut messages, WAIT).await,
        Some(AgentMsg::Hello { .. })
    ));
    assert!(matches!(
        next(&mut messages, WAIT).await,
        Some(AgentMsg::Register(_))
    ));
    let (code, _) = {
        let home = home.clone();
        tokio::task::spawn_blocking(move || statusline(&home))
            .await
            .unwrap()
    };
    assert_eq!(code, Some(0));
    // No `bound`, no mark: the status line posts to the hub on this machine,
    // and the agent sends nothing the old hub would not know.
    wait_for("the status line's post", || {
        posts.load(Ordering::SeqCst) > 0
    })
    .await;
    assert!(!status_file(&home, "agent").exists());
    assert_eq!(next(&mut messages, Duration::from_millis(2500)).await, None);
    tokio::task::spawn_blocking(move || stop(agent))
        .await
        .unwrap();
}
