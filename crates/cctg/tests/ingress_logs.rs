//! Log capture for the agent link and the hook endpoint: the shared secret,
//! a wrongly offered secret and message contents never reach the logs, on
//! any path, parse errors included. Its own test binary with a global
//! subscriber: connection tasks run on runtime workers, and parallel tests
//! would race on tracing callsite registration.

use std::io;
use std::net::{Ipv4Addr, SocketAddr};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use cctg::hook;
use cctg::hub::ingress::{self, AgentEvent};
use cctg::wire::{self, AgentMsg, HOOK_PATH, HookEvent, HookPost, Register, Secret};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpStream;
use tokio::sync::mpsc;

#[derive(Clone, Default)]
struct Captured(Arc<Mutex<Vec<u8>>>);

impl io::Write for Captured {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        if let Ok(mut out) = self.0.lock() {
            out.extend_from_slice(buf);
        }
        Ok(buf.len())
    }
    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

/// Sends raw bytes, half-closes, and waits until the hub closes too.
async fn send_and_wait(addr: SocketAddr, bytes: &[u8]) {
    let mut stream = TcpStream::connect(addr).await.expect("connect");
    let _ = stream.write_all(bytes).await;
    let _ = stream.shutdown().await;
    let mut sink = Vec::new();
    let _ = tokio::time::timeout(Duration::from_secs(10), stream.read_to_end(&mut sink)).await;
}

fn http(auth: &str, body: &[u8]) -> Vec<u8> {
    let head = format!(
        "POST {HOOK_PATH} HTTP/1.1\r\nAuthorization: Bearer {auth}\r\nContent-Length: {}\r\n\r\n",
        body.len()
    );
    [head.as_bytes(), body].concat()
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn ingress_logs_carry_no_secrets_or_contents() {
    let captured = Captured::default();
    let writer = captured.clone();
    let subscriber = tracing_subscriber::fmt()
        .without_time()
        .with_max_level(tracing::Level::TRACE)
        .with_writer(move || writer.clone())
        .finish();
    tracing::subscriber::set_global_default(subscriber).expect("first subscriber");

    let pid = std::process::id();
    let real = format!("real-secret-marker-{pid}");
    let wrong = format!("wrong-secret-marker-{pid}");
    let content = format!("content-marker-{pid}");
    let secret = Secret::parse(&real).expect("secret");

    let loopback = SocketAddr::from((Ipv4Addr::LOCALHOST, 0));
    let agents = ingress::bind(loopback).await.expect("bind agents");
    let agents_addr = agents.local_addr().expect("addr");
    let (agent_tx, mut agent_rx) = mpsc::channel(16);
    tokio::spawn(ingress::serve_agents(agents, secret.clone(), agent_tx));
    let hooks = ingress::bind(loopback).await.expect("bind hooks");
    let hooks_addr = hooks.local_addr().expect("addr");
    let (hook_tx, mut hook_rx) = mpsc::channel(16);
    tokio::spawn(ingress::serve_hooks(hooks, secret.clone(), hook_tx));

    // Agent link: wrong secret, broken hello carrying the secret, a version
    // mismatch carrying it, an overlong line carrying it.
    let wrong_hello = AgentMsg::Hello {
        secret: Secret::parse(&wrong).expect("secret"),
    };
    send_and_wait(agents_addr, &wire::encode(&wrong_hello)).await;
    send_and_wait(
        agents_addr,
        format!("{{\"v\":1,\"type\":\"hello\",\"secret\":[\"{real}\"]}}\n").as_bytes(),
    )
    .await;
    send_and_wait(
        agents_addr,
        format!("{{\"v\":9,\"type\":\"hello\",\"secret\":\"{real}\"}}\n").as_bytes(),
    )
    .await;
    send_and_wait(
        agents_addr,
        format!("{real}{}", "x".repeat(wire::MAX_LINE)).as_bytes(),
    )
    .await;

    // A registered agent sending junk that carries the secret and content.
    let register = Register {
        session_id: "5e551017-0000-4000-8000-000000000001".into(),
        host: format!("host-{content}"),
        cwd: format!("/home/{content}"),
    };
    let mut lines = wire::encode(&AgentMsg::Hello {
        secret: secret.clone(),
    });
    lines.extend(wire::encode(&AgentMsg::Register(register)));
    lines.extend(format!("{{\"v\":1,\"type\":\"{real}\"}}\n").as_bytes());
    lines
        .extend(format!("{{\"v\":1,\"type\":\"reply\",\"text\":7,\"x\":\"{real}\"}}\n").as_bytes());
    lines.extend(format!("{real} not json\n").as_bytes());
    lines.extend(wire::encode(&AgentMsg::Reply {
        text: content.clone(),
    }));
    send_and_wait(agents_addr, &lines).await;
    let mut registered = 0;
    while let Ok(event) = agent_rx.try_recv() {
        if matches!(event, AgentEvent::Registered { .. }) {
            registered += 1;
        }
    }
    assert_eq!(registered, 1, "only the good agent registers");

    // Hook endpoint: wrong bearer, malformed bodies carrying the secret, one
    // good post and its repeat.
    let post = HookPost::new(
        format!("host-{content}"),
        "5e551017-0000-4000-8000-000000000001".into(),
        format!("/home/{content}"),
        format!("/home/{content}/s.jsonl"),
        HookEvent::Stop {
            prompt_id: None,
            last_assistant_message: Some(content.clone()),
        },
    );
    let body = serde_json::to_vec(&post).expect("body");
    send_and_wait(hooks_addr, &http(&wrong, &body)).await;
    send_and_wait(
        hooks_addr,
        &http(&real, format!("{{\"v\":1,\"x\":\"{real}\"").as_bytes()),
    )
    .await;
    send_and_wait(
        hooks_addr,
        &http(
            &real,
            format!("{{\"v\":1,\"event\":{{\"type\":\"{real}\"}}}}").as_bytes(),
        ),
    )
    .await;
    send_and_wait(
        hooks_addr,
        format!("POST {HOOK_PATH} HTTP/1.x\r\nAuthorization: Bearer {real}\r\n\r\n").as_bytes(),
    )
    .await;
    send_and_wait(
        hooks_addr,
        format!("POST {HOOK_PATH} HTTP/1.1\r\nAuthorization: Bearer {real}\r\n{real}(: x\r\nX: {real}\n\r\n\r\n")
            .as_bytes(),
    )
    .await;
    let timeout = Duration::from_secs(5);
    hook::post(&hooks_addr.to_string(), &secret, &post, timeout)
        .await
        .expect("accepted");
    hook::post(&hooks_addr.to_string(), &secret, &post, timeout)
        .await
        .expect("repeat accepted");
    assert!(hook_rx.recv().await.is_some());
    assert!(hook_rx.try_recv().is_err(), "the repeat is dropped");

    let logs = String::from_utf8(captured.0.lock().map(|l| l.clone()).unwrap_or_default())
        .unwrap_or_default();
    for expected in [
        "agent rejected",
        "agent registered",
        "agent line ignored",
        "hook request rejected",
        "hook event accepted",
        "repeated hook event dropped",
    ] {
        assert!(
            logs.contains(expected),
            "expected {expected:?} in logs: {logs}"
        );
    }
    for leaked in [real.as_str(), wrong.as_str(), content.as_str()] {
        assert!(!logs.contains(leaked), "{leaked} in logs: {logs}");
    }
}
