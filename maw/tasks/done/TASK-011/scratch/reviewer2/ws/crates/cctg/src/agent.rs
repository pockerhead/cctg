//! Agent end of the hub link: connect, authenticate, register, and on any
//! loss reconnect with backoff and register again. Only the agent reconnects;
//! the hub just accepts.
//!
//! Messages queued while the link is down wait in the outbox and go out after
//! the next registration. A message whose write failed is lost.

use std::time::Duration;

use tokio::io::BufReader;
use tokio::net::TcpStream;
use tokio::net::tcp::{OwnedReadHalf, OwnedWriteHalf};
use tokio::sync::mpsc;
use tracing::{debug, info, warn};

use crate::wire::{self, AgentMsg, HubMsg, Register, Rejection, Secret, WireError};

const CONNECT_TIMEOUT: Duration = Duration::from_secs(5);
const WRITE_TIMEOUT: Duration = Duration::from_secs(5);
const QUEUE: usize = 256;

/// Exponential backoff with "equal jitter": the delay for attempt `n` is
/// uniform in `[d/2, d]` where `d = min(max, initial * 2^n)`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Backoff {
    pub initial: Duration,
    pub max: Duration,
}

impl Default for Backoff {
    fn default() -> Self {
        Self {
            initial: Duration::from_millis(250),
            max: Duration::from_secs(30),
        }
    }
}

impl Backoff {
    pub fn delay(&self, attempt: u32) -> Duration {
        let factor = 1u32.checked_shl(attempt.min(31)).unwrap_or(u32::MAX);
        let capped = self.initial.saturating_mul(factor).min(self.max);
        let half = capped / 2;
        let span = u64::try_from((capped - half).as_nanos()).unwrap_or(u64::MAX);
        half + Duration::from_nanos(wire::random_u64() % span.saturating_add(1))
    }
}

#[derive(Debug, Clone)]
pub struct LinkConfig {
    /// `host:port` of the hub agent listener.
    pub addr: String,
    pub secret: Secret,
    pub register: Register,
    pub backoff: Backoff,
}

#[derive(Debug, Clone, PartialEq)]
pub enum LinkEvent {
    /// Registered with the hub (again, after a reconnect).
    Up,
    /// The link dropped; reconnecting.
    Down,
    Message(HubMsg),
}

#[derive(Debug, thiserror::Error)]
enum ConnectError {
    #[error("cannot reach the hub: {0:?}")]
    Io(std::io::ErrorKind),
    #[error("hub handshake timed out")]
    Timeout,
    #[error("hub rejected the agent: {0:?}")]
    Rejected(Rejection),
    #[error("hub handshake failed: {0}")]
    Wire(#[from] WireError),
}

/// Starts the link task. Send to the returned sender; read link state and hub
/// messages from the receiver. Dropping either one stops the task.
pub fn spawn(config: LinkConfig) -> (mpsc::Sender<AgentMsg>, mpsc::Receiver<LinkEvent>) {
    let (outbox_tx, outbox) = mpsc::channel(QUEUE);
    let (events, events_rx) = mpsc::channel(QUEUE);
    tokio::spawn(run(config, outbox, events));
    (outbox_tx, events_rx)
}

async fn run(
    config: LinkConfig,
    mut outbox: mpsc::Receiver<AgentMsg>,
    events: mpsc::Sender<LinkEvent>,
) {
    let mut attempt = 0u32;
    let mut last_error = String::new();
    loop {
        if outbox.is_closed() || events.is_closed() {
            return;
        }
        match connect(&config).await {
            Ok((reader, write)) => {
                attempt = 0;
                last_error.clear();
                info!("registered with the hub");
                if events.send(LinkEvent::Up).await.is_err() {
                    return;
                }
                let stopped = serve(reader, write, &mut outbox, &events).await;
                if stopped || events.send(LinkEvent::Down).await.is_err() {
                    return;
                }
                info!("hub link lost; reconnecting");
            }
            Err(error) => {
                // One warning per kind of failure, not one per attempt.
                let text = error.to_string();
                if text != last_error {
                    warn!(%error, "hub link not established; retrying");
                    last_error = text;
                } else {
                    debug!(%error, attempt, "hub link retry failed");
                }
            }
        }
        if outbox.is_closed() || events.is_closed() {
            return;
        }
        tokio::time::sleep(config.backoff.delay(attempt)).await;
        attempt = attempt.saturating_add(1);
    }
}

async fn write_agent_msg(write: &mut OwnedWriteHalf, msg: &AgentMsg) -> Result<(), WireError> {
    tokio::time::timeout(WRITE_TIMEOUT, wire::write_msg(write, msg))
        .await
        .unwrap_or(Err(WireError::Io(std::io::ErrorKind::TimedOut)))
}

async fn connect(
    config: &LinkConfig,
) -> Result<(BufReader<OwnedReadHalf>, OwnedWriteHalf), ConnectError> {
    let handshake = async {
        let stream = TcpStream::connect(config.addr.as_str())
            .await
            .map_err(|error| ConnectError::Io(error.kind()))?;
        let _ = stream.set_nodelay(true);
        let (read, mut write) = stream.into_split();
        let hello = AgentMsg::Hello {
            secret: config.secret.clone(),
        };
        write_agent_msg(&mut write, &hello).await?;
        write_agent_msg(&mut write, &AgentMsg::Register(config.register.clone())).await?;
        let mut reader = BufReader::new(read);
        let mut line = Vec::new();
        wire::read_line(&mut reader, &mut line).await?;
        match wire::decode::<HubMsg>(&line)? {
            HubMsg::Registered => Ok((reader, write)),
            HubMsg::Rejected { reason } => Err(ConnectError::Rejected(reason)),
            _ => Err(ConnectError::Wire(WireError::Malformed)),
        }
    };
    tokio::time::timeout(CONNECT_TIMEOUT, handshake)
        .await
        .unwrap_or(Err(ConnectError::Timeout))
}

/// Runs one registered link. Returns `true` when the owner is gone (stop),
/// `false` when the link dropped (reconnect).
async fn serve(
    reader: BufReader<OwnedReadHalf>,
    mut write: OwnedWriteHalf,
    outbox: &mut mpsc::Receiver<AgentMsg>,
    events: &mpsc::Sender<LinkEvent>,
) -> bool {
    let (frames_tx, mut frames) = mpsc::channel(QUEUE);
    let reader_task = tokio::spawn(read_hub_frames(reader, frames_tx));
    let stopped = loop {
        tokio::select! {
            frame = frames.recv() => {
                match frame {
                    Some(Ok(msg)) => {
                        if events.send(LinkEvent::Message(msg)).await.is_err() {
                            break true;
                        }
                    }
                    Some(Err(WireError::Version)) => {
                        warn!("hub changed protocol version; reconnecting");
                        break false;
                    }
                    Some(Err(error @ (WireError::Closed | WireError::TooLong | WireError::Io(_)))) => {
                        debug!(%error, "hub link ended");
                        break false;
                    }
                    Some(Err(error)) => warn!(%error, "hub line ignored"),
                    None => break false,
                }
            }
            msg = outbox.recv() => match msg {
                Some(msg) => {
                    if let Err(error) = write_agent_msg(&mut write, &msg).await {
                        debug!(%error, "write to hub failed");
                        break false;
                    }
                }
                None => break true,
            },
        }
    };
    reader_task.abort();
    let _ = reader_task.await;
    stopped
}

async fn read_hub_frames(
    mut reader: BufReader<OwnedReadHalf>,
    frames: mpsc::Sender<Result<HubMsg, WireError>>,
) {
    let mut line = Vec::new();
    loop {
        let frame = match wire::read_line(&mut reader, &mut line).await {
            Ok(()) => wire::decode::<HubMsg>(&line),
            Err(error) => {
                let _ = frames.send(Err(error)).await;
                return;
            }
        };
        line.clear();
        if frames.send(frame).await.is_err() {
            return;
        }
    }
}

#[cfg(test)]
mod tests {
    use std::net::{Ipv4Addr, SocketAddr};

    use tokio::io::AsyncWriteExt;
    use tokio::net::TcpListener;

    use super::*;
    use crate::hub::ingress::{self, AgentEvent};

    const SECRET: &str = "0123456789abcdef-secret";
    const WAIT: Duration = Duration::from_secs(10);

    fn register() -> Register {
        Register {
            session_id: "5e551017-0000-4000-8000-000000000001".into(),
            host: "box".into(),
            cwd: "/w".into(),
        }
    }

    fn config(addr: SocketAddr, backoff: Backoff) -> LinkConfig {
        LinkConfig {
            addr: addr.to_string(),
            secret: Secret::parse(SECRET).unwrap(),
            register: register(),
            backoff,
        }
    }

    /// Rebinding the port of a just-closed listener can fail briefly.
    async fn rebind(addr: SocketAddr) -> TcpListener {
        for _ in 0..100 {
            if let Ok(listener) = TcpListener::bind(addr).await {
                return listener;
            }
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
        panic!("cannot rebind {addr}");
    }

    async fn next(events: &mut mpsc::Receiver<LinkEvent>) -> LinkEvent {
        tokio::time::timeout(WAIT, events.recv())
            .await
            .expect("link event in time")
            .expect("link task alive")
    }

    async fn registered(hub: &mut mpsc::Receiver<AgentEvent>) -> (Register, mpsc::Sender<HubMsg>) {
        loop {
            match tokio::time::timeout(WAIT, hub.recv())
                .await
                .expect("hub event in time")
            {
                Some(AgentEvent::Registered {
                    register, to_agent, ..
                }) => return (register, to_agent),
                Some(_) => continue,
                None => panic!("hub stopped"),
            }
        }
    }

    #[test]
    fn backoff_grows_to_the_cap_with_jitter() {
        let backoff = Backoff {
            initial: Duration::from_millis(100),
            max: Duration::from_secs(2),
        };
        for attempt in 0..40 {
            let capped = (backoff.initial * 2u32.saturating_pow(attempt.min(20))).min(backoff.max);
            for _ in 0..50 {
                let delay = backoff.delay(attempt);
                assert!(
                    delay >= capped / 2 && delay <= capped,
                    "{attempt}: {delay:?}"
                );
            }
        }
        assert_eq!(backoff.delay(u32::MAX).max(backoff.max), backoff.max);
        let spread: std::collections::HashSet<Duration> =
            (0..20).map(|_| backoff.delay(5)).collect();
        assert!(spread.len() > 1, "jitter expected");
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn the_agent_reconnects_and_registers_again_after_a_hub_restart() {
        let secret = Secret::parse(SECRET).unwrap();
        let listener = ingress::bind(SocketAddr::from((Ipv4Addr::LOCALHOST, 0)))
            .await
            .unwrap();
        let addr = listener.local_addr().unwrap();
        let (hub_tx, mut hub_rx) = mpsc::channel(16);
        let hub = tokio::spawn(ingress::serve_agents(listener, secret.clone(), hub_tx));

        let backoff = Backoff {
            initial: Duration::from_millis(20),
            max: Duration::from_millis(160),
        };
        let (outbox, mut events) = spawn(config(addr, backoff));
        assert_eq!(next(&mut events).await, LinkEvent::Up);
        let (got, to_agent) = registered(&mut hub_rx).await;
        assert_eq!(got, register());
        to_agent.send(HubMsg::Registered).await.unwrap();
        assert_eq!(
            next(&mut events).await,
            LinkEvent::Message(HubMsg::Registered)
        );

        // Hub goes away: its listener and every connection close.
        hub.abort();
        let _ = hub.await;
        drop(to_agent);
        assert_eq!(next(&mut events).await, LinkEvent::Down);

        // While it is down, the port accepts and hangs up at once: the agent
        // must retry with growing pauses, not in a tight loop.
        let refusing = rebind(addr).await;
        let queued = AgentMsg::Reply {
            text: "queued while down".into(),
        };
        outbox.send(queued.clone()).await.unwrap();
        let mut attempts = Vec::new();
        while attempts.len() < 5 {
            let accepted = tokio::time::timeout(WAIT, refusing.accept()).await;
            assert!(matches!(accepted, Ok(Ok(_))), "agent retried");
            attempts.push(tokio::time::Instant::now());
        }
        drop(refusing);
        let gaps: Vec<Duration> = attempts.windows(2).map(|w| w[1] - w[0]).collect();
        // Attempt n waits at least min(160, 20 * 2^n) / 2 ms; the fifth
        // accept follows at least the fourth failure.
        assert!(
            gaps.iter().all(|gap| *gap >= Duration::from_millis(5)),
            "{gaps:?}"
        );
        assert!(gaps[3] >= Duration::from_millis(60), "{gaps:?}");

        let listener = rebind(addr).await;
        let (hub_tx, mut hub_rx) = mpsc::channel(16);
        let _hub = tokio::spawn(ingress::serve_agents(listener, secret, hub_tx));
        loop {
            match next(&mut events).await {
                LinkEvent::Up => break,
                LinkEvent::Down => continue,
                other => panic!("unexpected {other:?}"),
            }
        }
        let (again, _to_agent) = registered(&mut hub_rx).await;
        assert_eq!(again, register());
        match tokio::time::timeout(WAIT, hub_rx.recv()).await.unwrap() {
            Some(AgentEvent::Message { msg, .. }) => assert_eq!(msg, queued),
            other => panic!("expected the queued reply, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn a_rejected_secret_keeps_retrying_quietly() {
        let listener = ingress::bind(SocketAddr::from((Ipv4Addr::LOCALHOST, 0)))
            .await
            .unwrap();
        let addr = listener.local_addr().unwrap();
        let (hub_tx, mut hub_rx) = mpsc::channel(16);
        let other = Secret::parse("another-secret-0123456789").unwrap();
        let _hub = tokio::spawn(ingress::serve_agents(listener, other, hub_tx));
        let backoff = Backoff {
            initial: Duration::from_millis(10),
            max: Duration::from_millis(40),
        };
        let (_outbox, mut events) = spawn(config(addr, backoff));
        tokio::time::sleep(Duration::from_millis(300)).await;
        assert!(events.try_recv().is_err(), "never up");
        assert!(hub_rx.try_recv().is_err(), "never registered");
    }

    #[tokio::test]
    async fn dropping_the_receiver_stops_the_link() {
        let listener = ingress::bind(SocketAddr::from((Ipv4Addr::LOCALHOST, 0)))
            .await
            .unwrap();
        let addr = listener.local_addr().unwrap();
        let (hub_tx, mut hub_rx) = mpsc::channel(16);
        let _hub = tokio::spawn(ingress::serve_agents(
            listener,
            Secret::parse(SECRET).unwrap(),
            hub_tx,
        ));
        let (outbox, events) = spawn(config(addr, Backoff::default()));
        let _ = registered(&mut hub_rx).await;
        drop(events);
        drop(outbox);
        match tokio::time::timeout(WAIT, hub_rx.recv()).await.unwrap() {
            Some(AgentEvent::Disconnected { .. }) => {}
            other => panic!("expected disconnect, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn a_split_hub_line_survives_concurrent_outbox_traffic() {
        let listener = TcpListener::bind(SocketAddr::from((Ipv4Addr::LOCALHOST, 0)))
            .await
            .unwrap();
        let addr = listener.local_addr().unwrap();
        let (outbox, mut events) = spawn(config(addr, Backoff::default()));
        let (stream, _) = listener.accept().await.unwrap();
        let (read, mut write) = stream.into_split();
        let mut reader = BufReader::new(read);
        let mut line = Vec::new();
        wire::read_line(&mut reader, &mut line).await.unwrap();
        line.clear();
        wire::read_line(&mut reader, &mut line).await.unwrap();
        line.clear();
        wire::write_msg(&mut write, &HubMsg::Registered)
            .await
            .unwrap();
        assert_eq!(next(&mut events).await, LinkEvent::Up);

        let inbound = HubMsg::Inbound {
            content: "split inbound".repeat(100),
            meta: Default::default(),
        };
        let encoded = wire::encode(&inbound);
        let (first, second) = encoded.split_at(encoded.len() / 2);
        write.write_all(first).await.unwrap();
        tokio::time::sleep(Duration::from_millis(20)).await;

        outbox
            .send(AgentMsg::Reply {
                text: "outbound".into(),
            })
            .await
            .unwrap();
        wire::read_line(&mut reader, &mut line).await.unwrap();
        line.clear();
        write.write_all(second).await.unwrap();

        assert_eq!(next(&mut events).await, LinkEvent::Message(inbound));
    }

    #[tokio::test]
    async fn dropping_the_outbox_stops_reconnects_while_the_hub_is_unreachable() {
        let listener = TcpListener::bind(SocketAddr::from((Ipv4Addr::LOCALHOST, 0)))
            .await
            .unwrap();
        let addr = listener.local_addr().unwrap();
        let backoff = Backoff {
            initial: Duration::from_millis(100),
            max: Duration::from_millis(100),
        };
        let (outbox, mut events) = spawn(config(addr, backoff));
        let (first, _) = listener.accept().await.unwrap();
        drop(outbox);
        drop(first);

        assert_eq!(
            tokio::time::timeout(WAIT, events.recv()).await.unwrap(),
            None
        );
        assert!(
            tokio::time::timeout(Duration::from_millis(300), listener.accept())
                .await
                .is_err(),
            "the closed outbox must prevent another connection attempt"
        );
    }
}
