//! Agent-side and hook-client log/error hygiene (not covered by tests/ingress_logs.rs,
//! which captures only the hub side). Own binary: global TRACE subscriber.

use std::io;
use std::net::{Ipv4Addr, SocketAddr};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use cctg::agent::{self, Backoff, LinkConfig, LinkEvent};
use cctg::hook;
use cctg::hub::ingress;
use cctg::wire::{self, HookEvent, HookPost, HubMsg, Register, Secret};
use tokio::io::{AsyncWriteExt, BufReader};
use tokio::net::TcpListener;
use tokio::sync::mpsc;

#[derive(Clone, Default)]
struct Cap(Arc<Mutex<Vec<u8>>>);
impl io::Write for Cap {
    fn write(&mut self, b: &[u8]) -> io::Result<usize> {
        self.0.lock().unwrap().extend_from_slice(b);
        Ok(b.len())
    }
    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn qa_agent_and_hook_client_never_log_the_secret() {
    let cap = Cap::default();
    let w = cap.clone();
    tracing::subscriber::set_global_default(
        tracing_subscriber::fmt().without_time().with_max_level(tracing::Level::TRACE).with_writer(move || w.clone()).finish(),
    )
    .unwrap();
    let pid = std::process::id();
    let real = format!("qa-real-secret-{pid}-xyz");
    let other = format!("qa-other-secret-{pid}-xyz");
    let secret = Secret::parse(&real).unwrap();
    let lo = SocketAddr::from((Ipv4Addr::LOCALHOST, 0));
    let register = Register { session_id: "11111111-2222".into(), host: "h".into(), cwd: "/c".into() };
    let backoff = Backoff { initial: Duration::from_millis(10), max: Duration::from_millis(30) };

    // 1. agent with a secret the hub rejects
    let l = ingress::bind(lo).await.unwrap();
    let a = l.local_addr().unwrap();
    let (tx, _rx) = mpsc::channel(16);
    tokio::spawn(ingress::serve_agents(l, Secret::parse(&other).unwrap(), tx));
    let (_o1, _e1) = agent::spawn(LinkConfig { addr: a.to_string(), secret: secret.clone(), register: register.clone(), backoff });

    // 2. fake hub that echoes the secret back in junk lines after Registered
    let fake = TcpListener::bind(lo).await.unwrap();
    let fa = fake.local_addr().unwrap();
    let real2 = real.clone();
    tokio::spawn(async move {
        let (s, _) = fake.accept().await.unwrap();
        let (r, mut w) = s.into_split();
        let mut rd = BufReader::new(r);
        let mut line = Vec::new();
        wire::read_line(&mut rd, &mut line).await.unwrap();
        line.clear();
        wire::read_line(&mut rd, &mut line).await.unwrap();
        wire::write_msg(&mut w, &HubMsg::Registered).await.unwrap();
        w.write_all(format!("{{\"v\":1,\"type\":\"{real2}\"}}\n").as_bytes()).await.unwrap();
        w.write_all(format!("{{\"v\":1,\"type\":\"inbound\",\"content\":7,\"x\":\"{real2}\"}}\n").as_bytes()).await.unwrap();
        w.write_all(format!("{real2}\n").as_bytes()).await.unwrap();
        w.write_all(format!("{{\"v\":3,\"type\":\"registered\",\"x\":\"{real2}\"}}\n").as_bytes()).await.unwrap();
        tokio::time::sleep(Duration::from_millis(200)).await;
    });
    let (_o2, mut e2) = agent::spawn(LinkConfig { addr: fa.to_string(), secret: secret.clone(), register: register.clone(), backoff });
    assert_eq!(tokio::time::timeout(Duration::from_secs(5), e2.recv()).await.unwrap(), Some(LinkEvent::Up));
    assert_eq!(tokio::time::timeout(Duration::from_secs(5), e2.recv()).await.unwrap(), Some(LinkEvent::Down));

    // 3. hook client errors: wrong secret, refused port, junk answer
    let hl = ingress::bind(lo).await.unwrap();
    let ha = hl.local_addr().unwrap().to_string();
    let (htx, _hrx) = mpsc::channel(4);
    tokio::spawn(ingress::serve_hooks(hl, Secret::parse(&other).unwrap(), htx));
    let post = HookPost::new("h".into(), "s".into(), "/c".into(), "/c/t".into(), HookEvent::SessionEnd { reason: None });
    let e = hook::post(&ha, &secret, &post, Duration::from_secs(2)).await.unwrap_err();
    let closed = TcpListener::bind(lo).await.unwrap();
    let ca = closed.local_addr().unwrap().to_string();
    drop(closed);
    let e2x = hook::post(&ca, &secret, &post, Duration::from_secs(2)).await.unwrap_err();
    tokio::time::sleep(Duration::from_millis(400)).await;

    let cfg = LinkConfig { addr: "x:1".into(), secret: secret.clone(), register, backoff };
    let texts = format!("{e} {e:?} {e2x} {e2x:?} {cfg:?} {:?}", wire::AgentMsg::Hello { secret: secret.clone() });
    let logs = String::from_utf8(cap.0.lock().unwrap().clone()).unwrap();
    eprintln!("QA agent-side logs:\n{logs}");
    for must in ["hub link not established", "registered with the hub", "hub line ignored"] {
        assert!(logs.contains(must), "missing {must:?}");
    }
    assert!(!logs.contains(&real), "secret in logs");
    assert!(!logs.contains(&other), "other secret in logs");
    assert!(!texts.contains(&real), "secret in error/debug text: {texts}");
}
