//! QA probes for the agent link (independent from the author tests).

use std::net::{Ipv4Addr, SocketAddr};
use std::time::{Duration, Instant};

use cctg::agent::{self, Backoff, LinkConfig, LinkEvent};
use cctg::hub::ingress::{self, AgentEvent};
use cctg::wire::{self, AgentMsg, HubMsg, Register, Secret};
use tokio::io::{AsyncWriteExt, BufReader};
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::mpsc;

const SECRET: &str = "qa-secret-0123456789abcdef";

fn secret() -> Secret {
    Secret::parse(SECRET).unwrap()
}

fn register() -> Register {
    Register {
        session_id: "11111111-2222-4333-8444-555555555555".into(),
        host: "qa".into(),
        cwd: "/qa".into(),
    }
}

fn cfg(addr: SocketAddr, backoff: Backoff) -> LinkConfig {
    LinkConfig {
        addr: addr.to_string(),
        secret: secret(),
        register: register(),
        backoff,
    }
}

fn lo() -> SocketAddr {
    SocketAddr::from((Ipv4Addr::LOCALHOST, 0))
}

async fn hub() -> (SocketAddr, mpsc::Receiver<AgentEvent>, tokio::task::JoinHandle<()>) {
    let l = ingress::bind(lo()).await.unwrap();
    let a = l.local_addr().unwrap();
    let (tx, rx) = mpsc::channel(256);
    (a, rx, tokio::spawn(ingress::serve_agents(l, secret(), tx)))
}

async fn registered(rx: &mut mpsc::Receiver<AgentEvent>) -> (u64, Register, mpsc::Sender<HubMsg>) {
    loop {
        match tokio::time::timeout(Duration::from_secs(10), rx.recv()).await.unwrap() {
            Some(AgentEvent::Registered { conn, register, to_agent }) => return (conn, register, to_agent),
            Some(_) => {}
            None => panic!("hub gone"),
        }
    }
}

async fn next(ev: &mut mpsc::Receiver<LinkEvent>, wait: Duration) -> Option<LinkEvent> {
    tokio::time::timeout(wait, ev.recv()).await.ok().flatten()
}

/// Disconfirmation probe: 64 x 256 KiB each way at the same time, consumers drain.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn qa_simultaneous_large_writes_both_ways_lose_nothing() {
    let (addr, mut hub_rx, _hub) = hub().await;
    let (outbox, mut events) = agent::spawn(cfg(addr, Backoff::default()));
    assert_eq!(next(&mut events, Duration::from_secs(10)).await, Some(LinkEvent::Up));
    let (_, _, to_agent) = registered(&mut hub_rx).await;

    const N: usize = 64;
    let big = |i: usize, c: char| format!("{i:04}-{}", c.to_string().repeat(256 * 1024));
    let started = Instant::now();
    let h2a = tokio::spawn(async move {
        for i in 0..N {
            to_agent
                .send(HubMsg::Inbound { content: big(i, 'h'), meta: Default::default() })
                .await
                .unwrap();
        }
        to_agent
    });
    let a2h = tokio::spawn(async move {
        for i in 0..N {
            outbox.send(AgentMsg::Reply { text: big(i, 'a') }).await.unwrap();
        }
        outbox
    });
    let hub_side = tokio::spawn(async move {
        let mut got = Vec::new();
        while got.len() < N {
            match tokio::time::timeout(Duration::from_secs(30), hub_rx.recv()).await {
                Ok(Some(AgentEvent::Message { msg: AgentMsg::Reply { text }, .. })) => got.push(text[..4].to_owned()),
                Ok(Some(AgentEvent::Disconnected { .. })) => panic!("hub saw disconnect after {}", got.len()),
                Ok(Some(other)) => panic!("unexpected {other:?}"),
                Ok(None) | Err(_) => panic!("hub stalled after {}", got.len()),
            }
        }
        got
    });
    let mut agent_got = Vec::new();
    while agent_got.len() < N {
        match next(&mut events, Duration::from_secs(30)).await {
            Some(LinkEvent::Message(HubMsg::Inbound { content, .. })) => agent_got.push(content[..4].to_owned()),
            other => panic!("agent got {other:?} after {}", agent_got.len()),
        }
    }
    let hub_got = hub_side.await.unwrap();
    let _keep = (h2a.await.unwrap(), a2h.await.unwrap());
    let want: Vec<String> = (0..N).map(|i| format!("{i:04}")).collect();
    assert_eq!(agent_got, want);
    assert_eq!(hub_got, want);
    eprintln!("QA both-ways 2x{N}x256KiB in {:?}", started.elapsed());
    assert!(started.elapsed() < Duration::from_secs(5), "{:?}", started.elapsed());
}

/// Characterisation: the hub consumer stalls for 7 s while the agent streams.
/// The agent cannot tell a busy hub from a dead one; record what happens.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn qa_stalled_hub_consumer_characterisation() {
    let l = ingress::bind(lo()).await.unwrap();
    let addr = l.local_addr().unwrap();
    let (tx, mut hub_rx) = mpsc::channel(4);
    let _hub = tokio::spawn(ingress::serve_agents(l, secret(), tx));
    let (outbox, mut events) = agent::spawn(cfg(addr, Backoff::default()));
    assert_eq!(next(&mut events, Duration::from_secs(10)).await, Some(LinkEvent::Up));
    let _reg = registered(&mut hub_rx).await;
    let sender = tokio::spawn(async move {
        for i in 0..200 {
            if outbox.send(AgentMsg::Reply { text: format!("{i:04}{}", "a".repeat(256 * 1024)) }).await.is_err() {
                break;
            }
        }
        outbox
    });
    tokio::time::sleep(Duration::from_secs(7)).await;
    let ev = next(&mut events, Duration::from_millis(100)).await;
    eprintln!("QA stalled consumer: agent event after 7 s stall = {ev:?}");
    let mut n = 0;
    let mut disc = false;
    while let Ok(Some(e)) = tokio::time::timeout(Duration::from_secs(3), hub_rx.recv()).await {
        match e {
            AgentEvent::Message { .. } => n += 1,
            AgentEvent::Disconnected { .. } => disc = true,
            AgentEvent::Registered { .. } => {}
        }
    }
    eprintln!("QA stalled consumer: hub then drained {n} messages, disconnect seen = {disc}");
    drop(sender);
}

/// A registered peer that never reads: the hub must drop it after the write timeout.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn qa_hub_drops_a_peer_that_never_reads() {
    let (addr, mut hub_rx, _hub) = hub().await;
    let s = TcpStream::connect(addr).await.unwrap();
    let (r, mut w) = s.into_split();
    w.write_all(&wire::encode(&AgentMsg::Hello { secret: secret() })).await.unwrap();
    w.write_all(&wire::encode(&AgentMsg::Register(register()))).await.unwrap();
    let (conn, _, to_agent) = registered(&mut hub_rx).await;
    let started = Instant::now();
    let pusher = tokio::spawn(async move {
        for _ in 0..64 {
            if to_agent
                .send(HubMsg::Inbound { content: "x".repeat(900 * 1024), meta: Default::default() })
                .await
                .is_err()
            {
                break;
            }
        }
    });
    match tokio::time::timeout(Duration::from_secs(20), hub_rx.recv()).await {
        Ok(Some(AgentEvent::Disconnected { conn: c })) => assert_eq!(c, conn),
        other => panic!("expected disconnect, got {other:?}"),
    }
    let took = started.elapsed();
    eprintln!("QA hub dropped a non-reading peer after {took:?}");
    assert!(took >= Duration::from_secs(4) && took < Duration::from_secs(12), "{took:?}");
    drop((r, w));
    let _ = pusher.await;
}

/// A hub that registers the agent and then never reads: the agent must go Down
/// after the write timeout and try again.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn qa_agent_drops_a_hub_that_never_reads_and_reconnects() {
    let listener = TcpListener::bind(lo()).await.unwrap();
    let addr = listener.local_addr().unwrap();
    let backoff = Backoff { initial: Duration::from_millis(50), max: Duration::from_millis(100) };
    let (outbox, mut events) = agent::spawn(cfg(addr, backoff));
    let (s, _) = listener.accept().await.unwrap();
    let (r, mut w) = s.into_split();
    let mut reader = BufReader::new(r);
    let mut line = Vec::new();
    wire::read_line(&mut reader, &mut line).await.unwrap();
    line.clear();
    wire::read_line(&mut reader, &mut line).await.unwrap();
    wire::write_msg(&mut w, &HubMsg::Registered).await.unwrap();
    assert_eq!(next(&mut events, Duration::from_secs(5)).await, Some(LinkEvent::Up));
    let started = Instant::now();
    let pusher = tokio::spawn(async move {
        for _ in 0..64 {
            if outbox.send(AgentMsg::Reply { text: "y".repeat(900 * 1024) }).await.is_err() {
                break;
            }
        }
        outbox
    });
    assert_eq!(next(&mut events, Duration::from_secs(20)).await, Some(LinkEvent::Down));
    let took = started.elapsed();
    eprintln!("QA agent went Down on a non-reading hub after {took:?}");
    assert!(took >= Duration::from_secs(4) && took < Duration::from_secs(12), "{took:?}");
    let again = tokio::time::timeout(Duration::from_secs(10), listener.accept()).await;
    assert!(matches!(again, Ok(Ok(_))), "agent reconnects after the write timeout");
    drop((reader, w));
    drop(pusher);
}

/// A 20 KiB line dribbled 1 byte at a time in each direction while the other
/// direction keeps sending: nothing is lost on either end.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn qa_byte_by_byte_lines_with_crossing_traffic() {
    // hub side: raw agent dribbles, hub sends a message every ms
    let (addr, mut hub_rx, _hub) = hub().await;
    let s = TcpStream::connect(addr).await.unwrap();
    s.set_nodelay(true).unwrap();
    let (r, mut w) = s.into_split();
    w.write_all(&wire::encode(&AgentMsg::Hello { secret: secret() })).await.unwrap();
    w.write_all(&wire::encode(&AgentMsg::Register(register()))).await.unwrap();
    let (_, _, to_agent) = registered(&mut hub_rx).await;
    let drain = tokio::spawn(async move {
        let mut reader = BufReader::new(r);
        let mut line = Vec::new();
        let mut n = 0usize;
        while wire::read_line(&mut reader, &mut line).await.is_ok() {
            line.clear();
            n += 1;
        }
        n
    });
    let noisy = tokio::spawn(async move {
        for i in 0..400 {
            let _ = to_agent.send(HubMsg::Inbound { content: format!("n{i}"), meta: Default::default() }).await;
            tokio::time::sleep(Duration::from_millis(1)).await;
        }
        to_agent
    });
    let texts: Vec<String> = (0..3).map(|i| format!("{i}{}", "z".repeat(20 * 1024))).collect();
    for t in &texts {
        for b in wire::encode(&AgentMsg::Reply { text: t.clone() }) {
            w.write_all(&[b]).await.unwrap();
            w.flush().await.unwrap();
            if b % 7 == 0 {
                tokio::task::yield_now().await;
            }
        }
    }
    for t in &texts {
        match tokio::time::timeout(Duration::from_secs(10), hub_rx.recv()).await.unwrap() {
            Some(AgentEvent::Message { msg: AgentMsg::Reply { text }, .. }) => assert_eq!(&text, t),
            other => panic!("hub side lost the dribbled line: {other:?}"),
        }
    }
    let _ = noisy.await;
    drop(w);
    let _ = drain.await;

    // agent side: raw hub dribbles, agent outbox sends every ms
    let listener = TcpListener::bind(lo()).await.unwrap();
    let addr = listener.local_addr().unwrap();
    let (outbox, mut events) = agent::spawn(cfg(addr, Backoff::default()));
    let (s, _) = listener.accept().await.unwrap();
    s.set_nodelay(true).unwrap();
    let (r, mut w) = s.into_split();
    let mut reader = BufReader::new(r);
    let mut line = Vec::new();
    wire::read_line(&mut reader, &mut line).await.unwrap();
    line.clear();
    wire::read_line(&mut reader, &mut line).await.unwrap();
    line.clear();
    wire::write_msg(&mut w, &HubMsg::Registered).await.unwrap();
    assert_eq!(next(&mut events, Duration::from_secs(5)).await, Some(LinkEvent::Up));
    let drain = tokio::spawn(async move {
        let mut n = 0usize;
        while wire::read_line(&mut reader, &mut line).await.is_ok() {
            line.clear();
            n += 1;
        }
        n
    });
    let noisy = tokio::spawn(async move {
        for i in 0..400 {
            let _ = outbox.send(AgentMsg::Reply { text: format!("n{i}") }).await;
            tokio::time::sleep(Duration::from_millis(1)).await;
        }
        outbox
    });
    for t in &texts {
        for b in wire::encode(&HubMsg::Inbound { content: t.clone(), meta: Default::default() }) {
            w.write_all(&[b]).await.unwrap();
            w.flush().await.unwrap();
            if b % 7 == 0 {
                tokio::task::yield_now().await;
            }
        }
    }
    for t in &texts {
        match next(&mut events, Duration::from_secs(10)).await {
            Some(LinkEvent::Message(HubMsg::Inbound { content, .. })) => assert_eq!(&content, t),
            other => panic!("agent side lost the dribbled line: {other:?}"),
        }
    }
    let outbox = noisy.await.unwrap();
    drop(outbox);
    drop(w);
    let n = drain.await.unwrap();
    assert!(n >= 400, "agent->hub noise delivered: {n}");
}

/// Hub restart on the same port: Down, then Up with a fresh Register carrying
/// the same session id, and traffic flows on the new connection.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn qa_hub_restart_reconnects_and_reregisters() {
    let (addr, mut hub_rx, hub_task) = hub().await;
    let backoff = Backoff { initial: Duration::from_millis(30), max: Duration::from_millis(300) };
    let (outbox, mut events) = agent::spawn(cfg(addr, backoff));
    assert_eq!(next(&mut events, Duration::from_secs(5)).await, Some(LinkEvent::Up));
    let (conn1, reg1, _t) = registered(&mut hub_rx).await;
    hub_task.abort();
    let _ = hub_task.await;
    assert_eq!(next(&mut events, Duration::from_secs(5)).await, Some(LinkEvent::Down));
    // stay down (refused) for ~1.2 s
    tokio::time::sleep(Duration::from_millis(1200)).await;
    let listener = {
        let mut l = None;
        for _ in 0..100 {
            if let Ok(x) = ingress::bind(addr).await {
                l = Some(x);
                break;
            }
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
        l.expect("rebind")
    };
    let (tx, mut hub_rx2) = mpsc::channel(16);
    let _hub2 = tokio::spawn(ingress::serve_agents(listener, secret(), tx));
    let started = Instant::now();
    assert_eq!(next(&mut events, Duration::from_secs(5)).await, Some(LinkEvent::Up));
    eprintln!("QA re-Up {:?} after the new hub bound", started.elapsed());
    let (_conn2, reg2, to_agent2) = registered(&mut hub_rx2).await;
    assert_eq!(reg1, reg2);
    let _ = conn1;
    outbox.send(AgentMsg::Reply { text: "after restart".into() }).await.unwrap();
    match tokio::time::timeout(Duration::from_secs(5), hub_rx2.recv()).await.unwrap() {
        Some(AgentEvent::Message { msg, .. }) => assert_eq!(msg, AgentMsg::Reply { text: "after restart".into() }),
        other => panic!("{other:?}"),
    }
    to_agent2.send(HubMsg::Inbound { content: "back".into(), meta: Default::default() }).await.unwrap();
    assert!(matches!(next(&mut events, Duration::from_secs(5)).await, Some(LinkEvent::Message(HubMsg::Inbound { .. }))));
}

/// Owner drops the outbox while connected: the link task ends, hub sees the
/// disconnect, no re-registration follows.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn qa_outbox_dropped_while_connected_stops_the_task() {
    let (addr, mut hub_rx, _hub) = hub().await;
    let (outbox, mut events) = agent::spawn(cfg(addr, Backoff { initial: Duration::from_millis(10), max: Duration::from_millis(20) }));
    assert_eq!(next(&mut events, Duration::from_secs(5)).await, Some(LinkEvent::Up));
    let _ = registered(&mut hub_rx).await;
    drop(outbox);
    match tokio::time::timeout(Duration::from_secs(5), hub_rx.recv()).await.unwrap() {
        Some(AgentEvent::Disconnected { .. }) => {}
        other => panic!("{other:?}"),
    }
    assert_eq!(next(&mut events, Duration::from_secs(2)).await, None, "events channel closed, task gone");
    assert!(tokio::time::timeout(Duration::from_millis(500), hub_rx.recv()).await.is_err(), "no re-register");
}

/// Owner drops the outbox during a long backoff sleep: how long until the task stops?
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn qa_outbox_dropped_during_backoff_stop_latency() {
    let listener = TcpListener::bind(lo()).await.unwrap();
    let addr = listener.local_addr().unwrap();
    drop(listener);
    let backoff = Backoff { initial: Duration::from_secs(4), max: Duration::from_secs(4) };
    let (outbox, mut events) = agent::spawn(cfg(addr, backoff));
    tokio::time::sleep(Duration::from_millis(300)).await;
    let started = Instant::now();
    drop(outbox);
    let r = tokio::time::timeout(Duration::from_secs(10), events.recv()).await;
    eprintln!("QA outbox dropped during backoff: task stopped after {:?} ({r:?})", started.elapsed());
    assert!(matches!(r, Ok(None)));
}

/// Events receiver dropped while the hub is unreachable: task ends (observed via
/// the outbox sender becoming closed).
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn qa_events_dropped_while_unreachable_stops_the_task() {
    let listener = TcpListener::bind(lo()).await.unwrap();
    let addr = listener.local_addr().unwrap();
    drop(listener);
    let backoff = Backoff { initial: Duration::from_millis(20), max: Duration::from_millis(40) };
    let (outbox, events) = agent::spawn(cfg(addr, backoff));
    tokio::time::sleep(Duration::from_millis(100)).await;
    drop(events);
    let started = Instant::now();
    while !outbox.is_closed() {
        assert!(started.elapsed() < Duration::from_secs(5), "task still alive");
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
}

/// Hub-to-agent traffic queued before the agent reads Registered, and
/// Registered+Inbound arriving in one segment, are not lost.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn qa_registered_and_inbound_in_one_segment() {
    let listener = TcpListener::bind(lo()).await.unwrap();
    let addr = listener.local_addr().unwrap();
    let (_outbox, mut events) = agent::spawn(cfg(addr, Backoff::default()));
    let (s, _) = listener.accept().await.unwrap();
    let (r, mut w) = s.into_split();
    let mut reader = BufReader::new(r);
    let mut line = Vec::new();
    wire::read_line(&mut reader, &mut line).await.unwrap();
    line.clear();
    wire::read_line(&mut reader, &mut line).await.unwrap();
    let mut both = wire::encode(&HubMsg::Registered);
    both.extend(wire::encode(&HubMsg::Inbound { content: "same segment".into(), meta: Default::default() }));
    w.write_all(&both).await.unwrap();
    assert_eq!(next(&mut events, Duration::from_secs(5)).await, Some(LinkEvent::Up));
    assert!(matches!(next(&mut events, Duration::from_secs(5)).await, Some(LinkEvent::Message(HubMsg::Inbound { .. }))));
    drop((reader, w));
}
