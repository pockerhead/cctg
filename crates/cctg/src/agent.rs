//! `cctg agent`: the channel MCP server Claude Code spawns over stdio, and
//! its end of the hub link.
//!
//! [`run_stdio`] reads JSON-RPC lines from stdin on a thread of its own, hands them
//! to [`channel::Server`] and writes every answer and notification to stdout
//! from one place, a line at a time. stdout carries JSON-RPC and nothing else;
//! logs go to stderr. The process ends when stdin closes.
//!
//! Hub link: connect, authenticate, register, and on any loss reconnect with
//! backoff and register again. Only the agent reconnects; the hub just
//! accepts. Messages queued while the link is down wait in the outbox and go
//! out after the next registration. A message whose write failed is lost.
//!
//! A headless run (`claude -p`, `CLAUDE_CODE_ENTRYPOINT=sdk-cli`) never gets a
//! channel from Claude Code (TASK-004), so its agent answers MCP but never
//! connects: a nested `claude -p` cannot show up as a routable channel even
//! when the process tree hides its parent.

use std::io::{BufRead, Read, Write};
use std::time::Duration;

use tokio::io::{AsyncWrite, AsyncWriteExt, BufReader};
use tokio::net::TcpStream;
use tokio::net::tcp::{OwnedReadHalf, OwnedWriteHalf};
use tokio::sync::mpsc;
use tracing::{debug, info, warn};

use crate::channel::{self, Hub, NoHub};
use crate::device::{self, DeviceConfig};
use crate::proctree;
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

/// Longest JSON-RPC line read from Claude Code; a longer one is skipped and
/// answered with a parse error.
pub const MAX_RPC_LINE: usize = 8 << 20;
const FRAMES: usize = 64;
pub const PANIC_MESSAGE: &str = "cctg agent: internal error";

/// Writes the fixed panic diagnostic without including the panic payload.
/// The binary's panic hook passes stderr here; taking a writer keeps the
/// security and stdout-purity behavior directly testable.
pub fn write_panic_message(mut stderr: impl Write) -> std::io::Result<()> {
    writeln!(stderr, "{PANIC_MESSAGE}")
}

/// One line from stdin.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Frame {
    Line(Vec<u8>),
    TooLong,
}

/// Runs `cctg agent` until stdin closes.
pub async fn run_stdio() {
    let config = DeviceConfig::load();
    let session_id = std::env::var("CLAUDE_CODE_SESSION_ID").ok();
    let entrypoint = std::env::var("CLAUDE_CODE_ENTRYPOINT").ok();
    let (hub, events) = match link_plan(session_id, entrypoint.as_deref(), &config) {
        Ok((secret, session_id)) => {
            let register = Register {
                session_id,
                host: config.host.clone(),
                cwd: device::canonical_cwd(&current_dir()),
                // Not env `CLAUDE_PID`: in an MCP server it is inherited
                // from an outer claude, or unset (TASK-004).
                claude_pid: proctree::current_lineage(None, None, "").claude_pid,
            };
            let (outbox, events) = spawn(LinkConfig {
                addr: config.agent_addr.clone(),
                secret,
                register,
                backoff: Backoff::default(),
            });
            (Hub::Link(outbox), Some(events))
        }
        Err(reason) => {
            info!(reason = reason.text(), "no hub link");
            (Hub::Off(reason), None)
        }
    };
    let frames = read_frames(std::io::BufReader::new(std::io::stdin()));
    if let Err(error) = serve_channel(frames, tokio::io::stdout(), hub, events).await {
        debug!(kind = ?error.kind(), "stdout closed");
    }
}

/// Whether this agent talks to the hub, and as which session.
pub fn link_plan(
    session_id: Option<String>,
    entrypoint: Option<&str>,
    config: &DeviceConfig,
) -> Result<(Secret, String), NoHub> {
    if entrypoint == Some("sdk-cli") {
        return Err(NoHub::Headless);
    }
    let session_id = session_id
        .map(|id| id.trim().to_owned())
        .filter(|id| !id.is_empty())
        .ok_or(NoHub::NoSession)?;
    let secret = config.secret.clone().map_err(|problem| {
        warn!(%problem, "hub link off");
        NoHub::NoConfig
    })?;
    Ok((secret, session_id))
}

fn current_dir() -> String {
    std::env::current_dir()
        .ok()
        .and_then(|dir| dir.into_os_string().into_string().ok())
        .unwrap_or_default()
}

/// Reads lines on a thread of its own: a blocking read cannot be cancelled,
/// and nobody waits for the thread once stdin is closed. The receiver ends
/// at end of input or on a read error.
pub fn read_frames<R: BufRead + Send + 'static>(mut reader: R) -> mpsc::Receiver<Frame> {
    let (tx, rx) = mpsc::channel(FRAMES);
    std::thread::spawn(move || {
        let mut line = Vec::new();
        loop {
            line.clear();
            match reader
                .by_ref()
                .take(MAX_RPC_LINE as u64)
                .read_until(b'\n', &mut line)
            {
                Ok(0) => return,
                Ok(_) => {}
                Err(error) if error.kind() == std::io::ErrorKind::Interrupted => continue,
                Err(_) => return,
            }
            let frame = if line.last() != Some(&b'\n') && line.len() >= MAX_RPC_LINE {
                if !skip_line(&mut reader) {
                    let _ = tx.blocking_send(Frame::TooLong);
                    return;
                }
                Frame::TooLong
            } else {
                Frame::Line(std::mem::take(&mut line))
            };
            if tx.blocking_send(frame).is_err() {
                return;
            }
        }
    });
    rx
}

/// Discards input up to and including the next newline; `false` when the
/// input ends first.
fn skip_line(reader: &mut impl BufRead) -> bool {
    loop {
        let (found, used) = match reader.fill_buf() {
            Ok([]) => return false,
            Ok(buf) => match buf.iter().position(|&byte| byte == b'\n') {
                Some(at) => (true, at + 1),
                None => (false, buf.len()),
            },
            Err(error) if error.kind() == std::io::ErrorKind::Interrupted => continue,
            Err(_) => return false,
        };
        reader.consume(used);
        if found {
            return true;
        }
    }
}

/// The MCP loop: stdin frames and hub events in, JSON-RPC lines out. Returns
/// when stdin ends (the hub link stops with it) or stdout fails.
pub async fn serve_channel<W: AsyncWrite + Unpin>(
    mut frames: mpsc::Receiver<Frame>,
    mut output: W,
    hub: Hub,
    mut events: Option<mpsc::Receiver<LinkEvent>>,
) -> std::io::Result<()> {
    let mut server = channel::Server::new(hub);
    loop {
        let lines = tokio::select! {
            frame = frames.recv() => match frame {
                Some(Frame::Line(line)) => server.on_line(&line),
                Some(Frame::TooLong) => server.on_oversized_line(),
                None => break,
            },
            event = recv_event(&mut events), if events.is_some() => match event {
                Some(event) => server.on_link(event),
                None => {
                    events = None;
                    Vec::new()
                }
            },
        };
        for line in lines {
            output.write_all(&line).await?;
        }
        output.flush().await?;
    }
    output.flush().await
}

async fn recv_event(events: &mut Option<mpsc::Receiver<LinkEvent>>) -> Option<LinkEvent> {
    match events {
        Some(events) => events.recv().await,
        None => None,
    }
}

/// The one-time registration command, with this executable's absolute path,
/// quoted for PowerShell on Windows and for a POSIX shell elsewhere.
pub fn install_command(exe: &str) -> String {
    format!(
        "claude mcp add --scope user cctg -- {} agent",
        quote_shell_arg(exe)
    )
}

/// PowerShell: a single-quoted string is literal, `'` is written twice.
#[cfg(windows)]
fn quote_shell_arg(arg: &str) -> String {
    format!("'{}'", arg.replace('\'', "''"))
}

#[cfg(not(windows))]
fn quote_shell_arg(arg: &str) -> String {
    format!("'{}'", arg.replace('\'', "'\\''"))
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
            claude_pid: None,
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

    fn device(secret: Option<&str>) -> DeviceConfig {
        DeviceConfig::from_vars(|name| {
            (name == crate::hub::config::SECRET_VAR)
                .then(|| secret.map(str::to_owned))
                .flatten()
        })
    }

    #[test]
    fn link_plan_rules() {
        let session = || Some(" 5e551017-0000-4000-8000-000000000001 ".to_owned());
        let (_, id) = link_plan(session(), Some("cli"), &device(Some(SECRET))).unwrap();
        assert_eq!(id, "5e551017-0000-4000-8000-000000000001");
        assert!(link_plan(session(), None, &device(Some(SECRET))).is_ok());
        assert_eq!(
            link_plan(session(), Some("sdk-cli"), &device(Some(SECRET))).unwrap_err(),
            NoHub::Headless
        );
        for missing in [None, Some(String::new()), Some("  ".to_owned())] {
            assert_eq!(
                link_plan(missing, Some("cli"), &device(Some(SECRET))).unwrap_err(),
                NoHub::NoSession
            );
        }
        assert_eq!(
            link_plan(session(), Some("cli"), &device(None)).unwrap_err(),
            NoHub::NoConfig
        );
        assert_eq!(
            link_plan(session(), Some("cli"), &device(Some("short"))).unwrap_err(),
            NoHub::NoConfig
        );
    }

    #[cfg(windows)]
    #[test]
    fn install_command_quotes_the_absolute_path_for_powershell() {
        assert_eq!(
            install_command(r"C:\Program Files\cctg\cctg.exe"),
            r"claude mcp add --scope user cctg -- 'C:\Program Files\cctg\cctg.exe' agent"
        );
        assert_eq!(
            install_command(r"C:\Users\o'$x`y\cctg.exe"),
            r"claude mcp add --scope user cctg -- 'C:\Users\o''$x`y\cctg.exe' agent"
        );
    }

    #[cfg(not(windows))]
    #[test]
    fn install_command_single_quotes_posix_shell_specials() {
        assert_eq!(
            install_command("/tmp/cctg $HOME `whoami` 'quoted'/cctg"),
            "claude mcp add --scope user cctg -- '/tmp/cctg $HOME `whoami` '\\''quoted'\\''/cctg' agent"
        );
    }

    #[test]
    fn panic_message_is_fixed_stderr_only() {
        let mut stderr = Vec::new();
        let stdout = Vec::<u8>::new();
        write_panic_message(&mut stderr).unwrap();
        assert_eq!(stderr, format!("{PANIC_MESSAGE}\n").as_bytes());
        assert!(stdout.is_empty());
        assert!(!String::from_utf8(stderr).unwrap().contains("payload"));
    }

    #[tokio::test]
    async fn frames_are_lines_and_an_oversized_line_is_skipped_whole() {
        let mut input = b"{\"a\":1}\r\n".to_vec();
        input.extend(vec![b'x'; MAX_RPC_LINE + 10]);
        input.extend(b"\n{\"b\":2}\nlast without newline");
        let mut frames = read_frames(std::io::Cursor::new(input));
        let mut got = Vec::new();
        while let Some(frame) = tokio::time::timeout(WAIT, frames.recv()).await.unwrap() {
            got.push(frame);
        }
        assert_eq!(
            got,
            [
                Frame::Line(b"{\"a\":1}\r\n".to_vec()),
                Frame::TooLong,
                Frame::Line(b"{\"b\":2}\n".to_vec()),
                Frame::Line(b"last without newline".to_vec()),
            ]
        );
        // An oversized line cut by the end of input.
        let mut frames = read_frames(std::io::Cursor::new(vec![b'y'; MAX_RPC_LINE + 1]));
        assert_eq!(frames.recv().await, Some(Frame::TooLong));
        assert_eq!(frames.recv().await, None);
    }

    /// Claude Code's side of the stdio pipes.
    struct Claude {
        frames: mpsc::Sender<Frame>,
        out: tokio::io::BufReader<tokio::io::DuplexStream>,
    }

    impl Claude {
        async fn send(&self, line: &str) {
            self.frames
                .send(Frame::Line(format!("{line}\n").into_bytes()))
                .await
                .unwrap();
        }

        async fn recv(&mut self) -> serde_json::Value {
            let mut line = Vec::new();
            tokio::time::timeout(WAIT, wire::read_line(&mut self.out, &mut line))
                .await
                .expect("a line in time")
                .expect("a whole line");
            serde_json::from_slice(&line).expect("one JSON object per line")
        }
    }

    fn claude(hub: Hub, events: Option<mpsc::Receiver<LinkEvent>>) -> Claude {
        let (frames, frames_rx) = mpsc::channel(16);
        let (ours, theirs) = tokio::io::duplex(1 << 16);
        tokio::spawn(serve_channel(frames_rx, ours, hub, events));
        Claude {
            frames,
            out: tokio::io::BufReader::new(theirs),
        }
    }

    async fn hub_message(hub: &mut mpsc::Receiver<AgentEvent>) -> AgentMsg {
        loop {
            match tokio::time::timeout(WAIT, hub.recv())
                .await
                .expect("in time")
            {
                Some(AgentEvent::Message { msg, .. }) => return msg,
                Some(_) => continue,
                None => panic!("hub stopped"),
            }
        }
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn the_channel_relays_both_ways_and_survives_a_hub_restart() {
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
        let (outbox, events) = spawn(config(addr, backoff));
        let mut claude = claude(Hub::Link(outbox), Some(events));

        claude
            .send(r#"{"jsonrpc":"2.0","id":0,"method":"initialize","params":{"protocolVersion":"2025-11-25"}}"#)
            .await;
        assert_eq!(claude.recv().await["id"], 0);
        claude
            .send(r#"{"jsonrpc":"2.0","method":"notifications/initialized"}"#)
            .await;
        let (got, to_agent) = registered(&mut hub_rx).await;
        assert_eq!(got, register());

        // Telegram -> Claude, invalid meta keys dropped.
        to_agent
            .send(HubMsg::Inbound {
                content: "hello from Telegram".into(),
                meta: [
                    ("chat_id".to_owned(), "-1001".to_owned()),
                    ("bad-key".to_owned(), "x".to_owned()),
                ]
                .into(),
            })
            .await
            .unwrap();
        let note = claude.recv().await;
        assert_eq!(note["method"], "notifications/claude/channel");
        assert_eq!(note["params"]["content"], "hello from Telegram");
        assert_eq!(
            note["params"]["meta"],
            serde_json::json!({ "chat_id": "-1001" })
        );

        // Claude -> Telegram.
        claude
            .send(r#"{"jsonrpc":"2.0","id":1,"method":"tools/call","params":{"name":"reply","arguments":{"text":"on it"}}}"#)
            .await;
        assert_eq!(claude.recv().await["result"]["isError"], false);
        assert_eq!(
            hub_message(&mut hub_rx).await,
            AgentMsg::Reply {
                text: "on it".into()
            }
        );

        // Permission relay.
        claude
            .send(r#"{"jsonrpc":"2.0","method":"notifications/claude/channel/permission_request","params":{"request_id":"fdqmc","tool_name":"Bash","description":"d","input_preview":"p"}}"#)
            .await;
        assert!(matches!(
            hub_message(&mut hub_rx).await,
            AgentMsg::PermissionRequest(request) if request.request_id == "fdqmc"
        ));
        to_agent
            .send(HubMsg::PermissionVerdict {
                request_id: "fdqmc".into(),
                behavior: wire::Behavior::Allow,
            })
            .await
            .unwrap();
        let verdict = claude.recv().await;
        assert_eq!(verdict["method"], "notifications/claude/channel/permission");
        assert_eq!(verdict["params"]["behavior"], "allow");

        // The hub goes away: MCP keeps answering, the reply waits.
        hub.abort();
        let _ = hub.await;
        drop(to_agent);
        tokio::time::sleep(Duration::from_millis(100)).await;
        claude
            .send(r#"{"jsonrpc":"2.0","id":2,"method":"tools/list"}"#)
            .await;
        assert_eq!(claude.recv().await["id"], 2);
        claude
            .send(r#"{"jsonrpc":"2.0","id":3,"method":"tools/call","params":{"name":"reply","arguments":{"text":"while down"}}}"#)
            .await;
        let answer = claude.recv().await;
        assert_eq!(answer["result"]["isError"], false);
        assert!(
            answer["result"]["content"][0]["text"]
                .as_str()
                .unwrap()
                .contains("queued")
        );

        // Back again: the same session registers and the reply arrives.
        let listener = rebind(addr).await;
        let (hub_tx, mut hub_rx) = mpsc::channel(16);
        let _hub = tokio::spawn(ingress::serve_agents(listener, secret, hub_tx));
        let (again, _to_agent) = registered(&mut hub_rx).await;
        assert_eq!(again, register());
        assert_eq!(
            hub_message(&mut hub_rx).await,
            AgentMsg::Reply {
                text: "while down".into()
            }
        );
    }

    #[tokio::test]
    async fn closing_stdin_ends_the_loop_and_the_link() {
        let listener = TcpListener::bind(SocketAddr::from((Ipv4Addr::LOCALHOST, 0)))
            .await
            .unwrap();
        let addr = listener.local_addr().unwrap();
        let (outbox, events) = spawn(config(addr, Backoff::default()));
        let (frames, frames_rx) = mpsc::channel(4);
        let (ours, _theirs) = tokio::io::duplex(1024);
        let serving = tokio::spawn(serve_channel(
            frames_rx,
            ours,
            Hub::Link(outbox),
            Some(events),
        ));
        let (_first, _) = listener.accept().await.unwrap();
        drop(frames);
        let done = tokio::time::timeout(WAIT, serving)
            .await
            .expect("loop ends");
        assert!(done.unwrap().is_ok());
    }
}
