//! `cctg agent` hands over to a newer binary without Claude Code noticing
//! (TASK-040), with real processes: a copy of the built `cctg` in a temp bin
//! directory runs as the shim, the hub end is the real `serve_agents` over
//! TCP and this test plays the slots actor and Claude Code.
//!
//! The copy is replaced by a different build (the same bytes plus a tail,
//! which Windows still runs) while its worker runs; `update` makes the worker
//! write the switch marker, answer every line Claude Code sent meanwhile,
//! leave (`update_answer reloading`), take what was queued before `released`
//! and exit; the shim starts the new file, whose worker registers with the
//! new build and needs no second `initialize`; it asks Claude Code to list
//! the tools again, and its `send_file` works through the shim (TASK-032).
//! The real `~/.cctg` is never touched: home, state and config are temp dirs.

use std::io::{BufRead, BufReader, Write};
use std::net::{Ipv4Addr, SocketAddr};
use std::path::Path;
use std::process::{Child, Command, Stdio};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use cctg::hub::ingress::{self, AgentEvent};
use cctg::wire::{AgentMsg, FileOutcome, HubMsg, Register, Secret, UpdateOutcome};
use serde_json::Value;
use tokio::sync::mpsc;

mod common;

const SECRET: &str = "update-e2e-secret-0123456789";
const SESSION: &str = "5e550000-0000-4000-8000-000000000040";
const WAIT: Duration = Duration::from_secs(30);
const EXE: &str = std::env::consts::EXE_SUFFIX;

struct Shim(Child);
impl Drop for Shim {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

struct Root(std::path::PathBuf);
impl Drop for Root {
    fn drop(&mut self) {
        // The replaced binary may still be held for a moment.
        for _ in 0..20 {
            if std::fs::remove_dir_all(&self.0).is_ok() {
                return;
            }
            std::thread::sleep(Duration::from_millis(100));
        }
    }
}

async fn registered(
    events: &mut mpsc::Receiver<AgentEvent>,
) -> (u64, Register, mpsc::Sender<HubMsg>) {
    loop {
        match tokio::time::timeout(WAIT, events.recv())
            .await
            .expect("a registration in time")
        {
            Some(AgentEvent::Registered {
                conn,
                register,
                to_agent,
            }) => return (conn, register, to_agent),
            Some(_) => continue,
            None => panic!("ingress stopped"),
        }
    }
}

async fn message(events: &mut mpsc::Receiver<AgentEvent>, from: u64) -> AgentMsg {
    loop {
        match tokio::time::timeout(WAIT, events.recv())
            .await
            .expect("a message in time")
        {
            Some(AgentEvent::Message { conn, msg, .. }) if conn == from => return msg,
            Some(_) => continue,
            None => panic!("ingress stopped"),
        }
    }
}

async fn wait_for(what: &str, check: impl Fn() -> bool) {
    let deadline = Instant::now() + WAIT;
    while !check() {
        assert!(Instant::now() < deadline, "waited too long for {what}");
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
}

fn send(claude: &mut std::process::ChildStdin, line: &str) {
    claude.write_all(line.as_bytes()).unwrap();
    claude
        .write_all(
            b"
",
        )
        .unwrap();
    claude.flush().unwrap();
}

fn build_of(path: &Path) -> String {
    cctg::client::build_of(path).unwrap()
}

#[tokio::test]
async fn a_new_binary_is_taken_without_losing_a_line() {
    let root = Root(std::env::temp_dir().join(format!("cctg-update-e2e-{}", std::process::id())));
    let _ = std::fs::remove_dir_all(&root.0);
    let [bin, home, work] = ["bin", "home", "work"].map(|name| root.0.join(name));
    for dir in [&bin, &home, &work] {
        std::fs::create_dir_all(dir).unwrap();
    }
    let exe = bin.join(format!("cctg{EXE}"));
    let original = std::fs::read(env!("CARGO_BIN_EXE_cctg")).unwrap();
    std::fs::write(&exe, &original).unwrap();

    let listener = ingress::bind(SocketAddr::from((Ipv4Addr::LOCALHOST, 0)))
        .await
        .unwrap();
    let port = listener.local_addr().unwrap().port();
    let (events_tx, mut events) = mpsc::channel(64);
    tokio::spawn(ingress::serve_agents(
        listener,
        Secret::parse(SECRET).unwrap(),
        events_tx,
    ));

    let mut command = Command::new(&exe);
    common::isolate(&mut command, &home);
    let mut child = command
        .arg("agent")
        .current_dir(&work)
        .env("CCTG_HUB_SECRET", SECRET)
        .env("CCTG_HUB_AGENT_ADDR", format!("127.0.0.1:{port}"))
        .env("CCTG_HOST", "box")
        .env("CCTG_STATE_DIR", root.0.join("state"))
        .env("CLAUDE_CODE_SESSION_ID", SESSION)
        .env("CLAUDE_CONFIG_DIR", home.join("claude"))
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .expect("cctg agent starts");
    let mut claude = child.stdin.take().unwrap();
    let out = Arc::new(Mutex::new(Vec::<String>::new()));
    {
        let out = out.clone();
        let stdout = child.stdout.take().unwrap();
        std::thread::spawn(move || {
            for line in BufReader::new(stdout).lines() {
                let Ok(line) = line else { break };
                out.lock().unwrap().push(line);
            }
        });
    }
    let shim = Shim(child);
    send(
        &mut claude,
        r#"{"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"2025-11-25"}}"#,
    );
    send(
        &mut claude,
        r#"{"jsonrpc":"2.0","method":"notifications/initialized"}"#,
    );

    let (first, register, to_first) = registered(&mut events).await;
    let client = register.client.expect("a client");
    assert!(client.self_update, "under the shim");
    assert_eq!(client.build, build_of(&exe));

    // A different build takes the file's place; the old one keeps running.
    std::fs::rename(&exe, bin.join(format!("cctg.old{EXE}"))).unwrap();
    let mut newer = original.clone();
    newer.extend_from_slice(b"\0update-e2e newer build");
    std::fs::write(&exe, &newer).unwrap();

    to_first
        .send(HubMsg::Update { update_id: 7 })
        .await
        .unwrap();
    // Claude Code keeps talking while the worker changes: a reply and a
    // permission prompt in flight, then pings.
    send(
        &mut claude,
        r#"{"jsonrpc":"2.0","id":107,"method":"tools/call","params":{"name":"reply","arguments":{"text":"in-flight reply"}}}"#,
    );
    send(
        &mut claude,
        r#"{"jsonrpc":"2.0","method":"notifications/claude/channel/permission_request","params":{"request_id":"qwert","tool_name":"Bash","description":"in flight","input_preview":"echo"}}"#,
    );
    for id in 100..106 {
        send(
            &mut claude,
            &format!(r#"{{"jsonrpc":"2.0","id":{id},"method":"ping"}}"#),
        );
    }
    // What the old worker got before its stdin ended reaches the hub ahead
    // of its `reloading`, on its own link; the rest goes to the new worker.
    let mut relayed = Vec::new();
    let reloading = loop {
        match message(&mut events, first).await {
            answer @ AgentMsg::UpdateAnswer { .. } => break answer,
            msg => relayed.push(msg),
        }
    };
    assert_eq!(
        reloading,
        AgentMsg::UpdateAnswer {
            update_id: 7,
            outcome: UpdateOutcome::Reloading
        }
    );
    // The worker let go of its stdin: these wait in the shim for the next.
    for id in 110..113 {
        send(
            &mut claude,
            &format!(r#"{{"jsonrpc":"2.0","id":{id},"method":"ping"}}"#),
        );
    }
    // Queued before `released`: still reaches Claude Code.
    to_first
        .send(HubMsg::Inbound {
            content: "before-release".into(),
            meta: Default::default(),
        })
        .await
        .unwrap();
    to_first
        .send(HubMsg::Released {
            update_id: 7,
            session_id: SESSION.into(),
        })
        .await
        .unwrap();

    let (second, register, to_second) = registered(&mut events).await;
    assert_ne!(second, first);
    assert_eq!(register.session_id, SESSION);
    assert_eq!(register.client.expect("a client").build, build_of(&exe));
    to_second
        .send(HubMsg::Inbound {
            content: "after-handover".into(),
            meta: Default::default(),
        })
        .await
        .unwrap();
    send(&mut claude, r#"{"jsonrpc":"2.0","id":106,"method":"ping"}"#);
    let answered = |id: i64| {
        let out = out.clone();
        move || {
            out.lock().unwrap().iter().any(|line| {
                serde_json::from_str::<Value>(line).is_ok_and(|value| value["id"] == id)
            })
        }
    };
    wait_for("the ping after the hand-over", answered(106)).await;
    let delivered = |text: &'static str| {
        let out = out.clone();
        move || out.lock().unwrap().iter().any(|line| line.contains(text))
    };
    wait_for(
        "the message queued before the release",
        delivered("before-release"),
    )
    .await;
    wait_for(
        "a message to the new worker, without a second initialize",
        delivered("after-handover"),
    )
    .await;

    // The new worker asked Claude Code to list the tools again, and lists
    // `send_file`, which works through the shim.
    send(
        &mut claude,
        r#"{"jsonrpc":"2.0","id":120,"method":"tools/list"}"#,
    );
    wait_for("the tools of the new worker", answered(120)).await;
    let png = [b"\x89PNG\r\n\x1a\n".as_slice(), &[5; 300_000]].concat();
    std::fs::write(work.join("shot.png"), &png).unwrap();
    send(
        &mut claude,
        r#"{"jsonrpc":"2.0","id":121,"method":"tools/call","params":{"name":"send_file","arguments":{"path":"shot.png"}}}"#,
    );
    let (transfer_id, size) = loop {
        match message(&mut events, second).await {
            AgentMsg::FileOffer {
                transfer_id, size, ..
            } => break (transfer_id, size),
            msg => relayed.push(msg),
        }
    };
    assert_eq!(size, png.len() as u64);
    let accepted = HubMsg::FileAnswer {
        transfer_id,
        outcome: FileOutcome::Accepted,
    };
    to_second.send(accepted).await.unwrap();
    let mut received = 0;
    while received < size {
        match message(&mut events, second).await {
            AgentMsg::FileChunk(chunk) => {
                assert_eq!(chunk.offset, received);
                received += cctg::files::CHUNK.min((size - received) as usize) as u64;
            }
            msg => relayed.push(msg),
        }
    }
    let sent = HubMsg::FileAnswer {
        transfer_id,
        outcome: FileOutcome::Sent,
    };
    to_second.send(sent).await.unwrap();
    wait_for("the send_file answer", answered(121)).await;

    // Nothing up to date asks for nothing.
    to_second
        .send(HubMsg::Update { update_id: 8 })
        .await
        .unwrap();
    let up_to_date = loop {
        match message(&mut events, second).await {
            answer @ AgentMsg::UpdateAnswer { .. } => break answer,
            msg => relayed.push(msg),
        }
    };
    assert_eq!(
        up_to_date,
        AgentMsg::UpdateAnswer {
            update_id: 8,
            outcome: UpdateOutcome::UpToDate
        }
    );
    let replies = relayed
        .iter()
        .filter(|msg| matches!(msg, AgentMsg::Reply { text } if text == "in-flight reply"))
        .count();
    let prompts = relayed
        .iter()
        .filter(|msg| matches!(msg, AgentMsg::PermissionRequest(request) if request.request_id == "qwert"))
        .count();
    assert_eq!((replies, prompts), (1, 1), "each relayed once: {relayed:?}");

    let lines = out.lock().unwrap().clone();
    for id in [
        1, 107, 100, 101, 102, 103, 104, 105, 110, 111, 112, 106, 120, 121,
    ] {
        let answers = lines
            .iter()
            .filter(|line| serde_json::from_str::<Value>(line).is_ok_and(|value| value["id"] == id))
            .count();
        assert_eq!(answers, 1, "one answer for id {id}:\n{lines:#?}");
    }
    for line in &lines {
        let value: Value = serde_json::from_str(line).expect("only JSON-RPC on stdout");
        assert_eq!(value["jsonrpc"], "2.0", "{line}");
    }
    assert!(!lines.iter().any(|line| line.contains("cctg_shim")));
    let changed = lines
        .iter()
        .filter(|line| line.contains("notifications/tools/list_changed"))
        .count();
    assert_eq!(changed, 1, "only the new worker asks:\n{lines:#?}");
    let answer = |id: i64| {
        lines
            .iter()
            .filter_map(|line| serde_json::from_str::<Value>(line).ok())
            .find(|value| value["id"] == id)
            .unwrap()
    };
    let tools: Vec<Value> = answer(120)["result"]["tools"]
        .as_array()
        .unwrap()
        .iter()
        .map(|tool| tool["name"].clone())
        .collect();
    assert_eq!(tools, ["reply", "send_file"]);
    assert_eq!(answer(121)["result"]["isError"], false, "{lines:#?}");

    // Claude Code closes stdin: shim and worker end.
    drop(claude);
    let mut shim = shim;
    let deadline = Instant::now() + Duration::from_secs(10);
    let status = loop {
        if let Some(status) = shim.0.try_wait().unwrap() {
            break status;
        }
        assert!(Instant::now() < deadline, "the shim ends with its stdin");
        tokio::time::sleep(Duration::from_millis(50)).await;
    };
    assert!(status.success(), "{status:?}");
    let links = cctg::shim::links_dir(&exe);
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        let left: Vec<_> = std::fs::read_dir(&links).unwrap().flatten().collect();
        if left.is_empty() {
            break;
        }
        assert!(
            Instant::now() < deadline,
            "the shim removes its worker links: {left:?}"
        );
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
}
