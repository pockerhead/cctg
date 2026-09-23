//! Reviewer repro: a line split across two TCP writes, with the peer's
//! outbound branch completing in between, must still be delivered.
use std::net::{Ipv4Addr, SocketAddr};
use std::time::Duration;

use cctg::agent::{self, Backoff, LinkConfig, LinkEvent};
use cctg::hub::ingress::{self, AgentEvent};
use cctg::wire::{self, AgentMsg, HubMsg, Register, Secret};
use tokio::io::{AsyncWriteExt, BufReader};
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::mpsc;

const SECRET: &str = "0123456789abcdef-secret";

fn register() -> Register {
    Register { session_id: "s".into(), host: "h".into(), cwd: "/w".into() }
}

#[tokio::test]
async fn hub_side_split_line_survives_outbound() {
    let listener = ingress::bind(SocketAddr::from((Ipv4Addr::LOCALHOST, 0))).await.unwrap();
    let addr = listener.local_addr().unwrap();
    let (tx, mut rx) = mpsc::channel(16);
    tokio::spawn(ingress::serve_agents(listener, Secret::parse(SECRET).unwrap(), tx));
    let (read, mut write) = TcpStream::connect(addr).await.unwrap().into_split();
    let mut reader = BufReader::new(read);
    wire::write_msg(&mut write, &AgentMsg::Hello { secret: Secret::parse(SECRET).unwrap() }).await.unwrap();
    wire::write_msg(&mut write, &AgentMsg::Register(register())).await.unwrap();
    let mut line = Vec::new();
    wire::read_line(&mut reader, &mut line).await.unwrap();
    let Some(AgentEvent::Registered { to_agent, .. }) = rx.recv().await else { panic!() };

    let reply = AgentMsg::Reply { text: "x".repeat(1000) };
    let bytes = wire::encode(&reply);
    let (a, b) = bytes.split_at(bytes.len() / 2);
    write.write_all(a).await.unwrap();
    write.flush().await.unwrap();
    tokio::time::sleep(Duration::from_millis(100)).await;
    // Hub sends to the agent while the agent's line is half-received.
    to_agent.send(HubMsg::Registered).await.unwrap();
    wire::read_line(&mut reader, &mut line).await.unwrap();
    write.write_all(b).await.unwrap();
    write.flush().await.unwrap();
    let got = tokio::time::timeout(Duration::from_secs(2), rx.recv()).await;
    match got {
        Ok(Some(AgentEvent::Message { msg, .. })) => assert_eq!(msg, reply),
        other => panic!("reply lost: {:?}", other.map(|o| o.map(|e| format!("{e:?}").len()))),
    }
}

#[tokio::test]
async fn agent_side_split_line_survives_outbox() {
    let listener = TcpListener::bind(SocketAddr::from((Ipv4Addr::LOCALHOST, 0))).await.unwrap();
    let addr = listener.local_addr().unwrap();
    let (outbox, mut events) = agent::spawn(LinkConfig {
        addr: addr.to_string(),
        secret: Secret::parse(SECRET).unwrap(),
        register: register(),
        backoff: Backoff::default(),
    });
    let (stream, _) = listener.accept().await.unwrap();
    let (read, mut write) = stream.into_split();
    let mut reader = BufReader::new(read);
    let mut line = Vec::new();
    wire::read_line(&mut reader, &mut line).await.unwrap(); // hello
    wire::read_line(&mut reader, &mut line).await.unwrap(); // register
    wire::write_msg(&mut write, &HubMsg::Registered).await.unwrap();
    assert_eq!(events.recv().await, Some(LinkEvent::Up));

    let inbound = HubMsg::Inbound { content: "y".repeat(1000), meta: Default::default() };
    let bytes = wire::encode(&inbound);
    let (a, b) = bytes.split_at(bytes.len() / 2);
    write.write_all(a).await.unwrap();
    tokio::time::sleep(Duration::from_millis(100)).await;
    outbox.send(AgentMsg::Reply { text: "r".into() }).await.unwrap();
    wire::read_line(&mut reader, &mut line).await.unwrap();
    write.write_all(b).await.unwrap();
    let got = tokio::time::timeout(Duration::from_secs(2), events.recv()).await;
    assert_eq!(got.ok().flatten(), Some(LinkEvent::Message(inbound)), "inbound lost");
}
