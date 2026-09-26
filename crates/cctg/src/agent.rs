//! `cctg agent`: the channel MCP server Claude Code spawns over stdio, and
//! its end of the hub link.
//!
//! [`run_stdio`] reads JSON-RPC lines from stdin on a thread of its own, hands them
//! to [`channel::Server`] and writes every answer and notification to stdout
//! from one place, a line at a time. stdout carries JSON-RPC and nothing else;
//! logs go to stderr. The process ends when stdin closes.
//!
//! Hub link: connect (TLS to a hub on another machine, [`crate::tls`]),
//! authenticate, register, and on any loss reconnect with
//! backoff and register again. Only the agent reconnects; the hub just
//! accepts. Messages queued while the link is down wait in the outbox and go
//! out after the next registration. A message whose write failed is lost.
//! With a hub that keeps the heartbeat (TASK-049) the agent sends `ping`
//! when it wrote nothing for a while and reconnects when nothing came from
//! the hub for longer: a connection that a NAT dropped on the way never
//! reports an error by itself.
//!
//! A permission verdict with a `verdict_id` is acknowledged once it is queued
//! for the channel loop. The hub sends the same verdict again until the ack
//! arrives, so the agent remembers recent ids across reconnects and passes
//! each on only once.
//!
//! After every registration the agent sends the session starts and ends its
//! session's hooks could not deliver ([`crate::spool`]) to the hub's hook
//! endpoint: a session that started while the hub was down becomes known as
//! soon as the hub is back, without waiting for its next hook.
//!
//! The agent also presses Esc in its claude's terminal when the hub asks
//! (`console_key`, see [`crate::keys`]) and answers whether the key events
//! were written, and types a one-line command into its input box
//! (`console_command`, TASK-043) and answers what became of it: on Windows
//! in claude's console, elsewhere through the `cctg run` that holds claude's
//! terminal (TASK-044).
//!
//! Session reads (TASK-034): the hub never opens a file of this machine; it
//! asks with `session_read` and the agent answers from its session's files
//! ([`crate::reads`]), one read at a time, off the loop.
//!
//! Files (TASK-032): a hub that takes files says so in `registered`; the
//! `send_file` tool then offers the file, and once the hub accepts it goes
//! over in chunks, and the tool answers what Telegram did. A file from the
//! topic comes as `file_start` and chunks and is kept in memory until
//! complete, then saved ([`crate::files::save`]) and handed to Claude like a
//! message, with where it is. A lost link drops a transfer on both ends.
//! Logs carry kinds and sizes, never names, paths or contents.
//!
//! Update (TASK-040): the agent runs as `cctg agent-worker` under the
//! `cctg agent` shim ([`crate::shim`]) and registers with its build
//! ([`Client`]); on the hub's `update` it hands over to a newer binary or
//! restarts claude through `cctg run` ([`crate::update`]). An `update` that
//! names the hub's release tag first puts that release's binary in place of
//! the worker's file ([`crate::download`], TASK-050).
//!
//! A headless run (`claude -p`, `CLAUDE_CODE_ENTRYPOINT=sdk-cli`) never gets a
//! channel from Claude Code (TASK-004), so its agent answers MCP but never
//! connects: a nested `claude -p` cannot show up as a routable channel even
//! when the process tree hides its parent.

use std::collections::{BTreeMap, VecDeque};
use std::io::{BufRead, Read, Write};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, SystemTime};

use tokio::io::{AsyncWrite, AsyncWriteExt, BufReader, ReadHalf, WriteHalf};
use tokio::sync::mpsc;
use tokio::task::JoinHandle;
use tracing::{debug, info, warn};

use tokio::time::{Instant, sleep_until};

use crate::channel::{self, FileCall, Hub, NoHub};
use crate::client;
use crate::device::{self, DeviceConfig};
use crate::download;
use crate::files;
use crate::keys::{self, Typed};
use crate::proctree;
use crate::reads;
use crate::shim;
use crate::spool;
use crate::tail;
use crate::tls::{HubAddr, ReadTask, Stream};
use crate::update::{self, Plan, Worker};
use crate::wire::{
    self, AgentMsg, Beat, Client, CommandOutcome, ConsoleKey, FileChunk, FileKind, FileOutcome,
    Heartbeat, HubMsg, Liveness, Register, Rejection, Secret, SessionAsk, UpdateOutcome, WireError,
};

const CONNECT_TIMEOUT: Duration = Duration::from_secs(5);
const WRITE_TIMEOUT: Duration = Duration::from_secs(5);
const QUEUE: usize = 256;
/// Budget of one spool replay after a registration.
const REPLAY_TIMEOUT: Duration = Duration::from_secs(5);
/// Verdict ids remembered for dropping a verdict the hub sent again.
const RECENT_VERDICTS: usize = 256;

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
    /// The hub agent listener.
    pub addr: HubAddr,
    pub secret: Secret,
    pub register: Register,
    pub backoff: Backoff,
    /// Where the session's undelivered hook events are kept, and the hub
    /// hook endpoint to send them to after each registration.
    pub replay: Option<Replay>,
    /// Used when `register.heartbeat` is set and the hub keeps one too.
    pub heartbeat: Heartbeat,
}

#[derive(Debug, Clone)]
pub struct Replay {
    /// `<state>/spool` ([`spool::dir`]).
    pub spool: PathBuf,
    /// The hub hook endpoint.
    pub hook_addr: HubAddr,
}

/// Sends the kept hook events of the registered session. Its own task: the
/// link does not wait for it. At most one runs: a registration while the
/// last replay still runs starts none. A replay of a hook at the same time
/// only sends an event twice, and the hub keeps one.
fn spawn_replay(config: &LinkConfig, running: &mut Option<JoinHandle<()>>) {
    if running.as_ref().is_some_and(|task| !task.is_finished()) {
        return;
    }
    let Some(replay) = config.replay.clone() else {
        return;
    };
    let (session, secret) = (config.register.session_id.clone(), config.secret.clone());
    *running = Some(tokio::spawn(async move {
        let deadline = tokio::time::Instant::now() + REPLAY_TIMEOUT;
        match spool::replay(
            &replay.spool,
            &session,
            &replay.hook_addr,
            &secret,
            deadline,
        )
        .await
        {
            Ok(0) => {}
            Ok(sent) => info!(sent, "kept hook events delivered"),
            Err(error) => {
                warn!(%error, "kept hook events not delivered; the next hook tries again")
            }
        }
    }));
}

#[derive(Debug, Clone, PartialEq)]
pub enum LinkEvent {
    /// Registered with the hub (again, after a reconnect). `files`: the hub
    /// takes `file_offer` ([`HubMsg::Registered`]).
    Up {
        files: bool,
    },
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
    let mut verdicts = VecDeque::with_capacity(RECENT_VERDICTS);
    let mut replaying = None;
    loop {
        if outbox.is_closed() || events.is_closed() {
            return;
        }
        match connect(&config).await {
            Ok(Linked {
                reader,
                write,
                files,
                heartbeat,
            }) => {
                attempt = 0;
                last_error.clear();
                info!("registered with the hub");
                spawn_replay(&config, &mut replaying);
                if events.send(LinkEvent::Up { files }).await.is_err() {
                    return;
                }
                let heartbeat =
                    (config.register.heartbeat && heartbeat).then_some(config.heartbeat);
                let link = Link {
                    reader,
                    write,
                    heartbeat,
                };
                let stopped = serve(link, &mut outbox, &events, &mut verdicts).await;
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

type LinkRead = BufReader<ReadHalf<Stream>>;
type LinkWrite = WriteHalf<Stream>;

async fn write_agent_msg(write: &mut LinkWrite, msg: &AgentMsg) -> Result<(), WireError> {
    tokio::time::timeout(WRITE_TIMEOUT, wire::write_msg(write, msg))
        .await
        .unwrap_or(Err(WireError::Io(std::io::ErrorKind::TimedOut)))
}

/// A registered connection and what the hub said it does.
struct Linked {
    reader: LinkRead,
    write: LinkWrite,
    files: bool,
    heartbeat: bool,
}

async fn connect(config: &LinkConfig) -> Result<Linked, ConnectError> {
    let handshake = async {
        let stream = config
            .addr
            .connect()
            .await
            .map_err(|error| ConnectError::Io(error.kind()))?;
        let (read, mut write) = tokio::io::split(stream);
        let hello = AgentMsg::Hello {
            secret: config.secret.clone(),
        };
        write_agent_msg(&mut write, &hello).await?;
        write_agent_msg(&mut write, &AgentMsg::Register(config.register.clone())).await?;
        let mut reader = BufReader::new(read);
        let mut line = Vec::new();
        wire::read_line(&mut reader, &mut line).await?;
        match wire::decode::<HubMsg>(&line)? {
            HubMsg::Registered { files, heartbeat } => Ok(Linked {
                reader,
                write,
                files,
                heartbeat,
            }),
            HubMsg::Rejected { reason } => Err(ConnectError::Rejected(reason)),
            _ => Err(ConnectError::Wire(WireError::Malformed)),
        }
    };
    tokio::time::timeout(CONNECT_TIMEOUT, handshake)
        .await
        .unwrap_or(Err(ConnectError::Timeout))
}

/// One registered connection; `heartbeat` when both ends keep one.
struct Link {
    reader: LinkRead,
    write: LinkWrite,
    heartbeat: Option<Heartbeat>,
}

/// Runs one registered link. Returns `true` when the owner is gone (stop),
/// `false` when the link dropped (reconnect). `verdicts`: ids of verdicts
/// already passed on, newest last. The hub's pings end here and never
/// reach the owner, so a busy owner does not hold them up.
async fn serve(
    link: Link,
    outbox: &mut mpsc::Receiver<AgentMsg>,
    events: &mpsc::Sender<LinkEvent>,
    verdicts: &mut VecDeque<u64>,
) -> bool {
    let Link {
        reader,
        mut write,
        heartbeat,
    } = link;
    let mut liveness = Liveness::new(heartbeat);
    let (frames_tx, mut frames) = mpsc::channel(QUEUE);
    let reader_task = ReadTask::spawn(read_hub_frames(reader, frames_tx));
    let stopped = loop {
        let next_beat = liveness.next();
        tokio::select! {
            frame = frames.recv() => {
                if frame.is_some() {
                    liveness.heard();
                }
                match frame {
                    Some(Ok(HubMsg::Ping)) => {}
                    Some(Ok(msg)) => {
                        let ack = match &msg {
                            HubMsg::PermissionVerdict { verdict_id, .. } => *verdict_id,
                            _ => None,
                        };
                        let repeated = ack.is_some_and(|id| verdicts.contains(&id));
                        if !repeated && events.send(LinkEvent::Message(msg)).await.is_err() {
                            break true;
                        }
                        let Some(verdict_id) = ack else {
                            continue;
                        };
                        if !repeated {
                            if verdicts.len() == RECENT_VERDICTS {
                                verdicts.pop_front();
                            }
                            verdicts.push_back(verdict_id);
                        }
                        let ack = AgentMsg::PermissionAck { verdict_id };
                        if let Err(error) = write_agent_msg(&mut write, &ack).await {
                            debug!(%error, "write to hub failed");
                            break false;
                        }
                        liveness.said();
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
                    liveness.said();
                }
                None => break true,
            },
            beat = wire::beat(next_beat) => match beat {
                Beat::Ping => {
                    if let Err(error) = write_agent_msg(&mut write, &AgentMsg::Ping).await {
                        debug!(%error, "write to hub failed");
                        break false;
                    }
                    liveness.said();
                }
                // A frame read meanwhile goes first.
                Beat::Dead if !frames.is_empty() => {}
                Beat::Dead => {
                    info!("hub silent past the heartbeat timeout; reconnecting");
                    break false;
                }
            },
        }
    };
    reader_task.stop().await;
    stopped
}

async fn read_hub_frames(mut reader: LinkRead, frames: mpsc::Sender<Result<HubMsg, WireError>>) {
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

/// Writes one key into the claude console; `false` when it was not written.
pub type Presser = Arc<dyn Fn(ConsoleKey) -> bool + Send + Sync>;
/// Types one line into the input box of the claude console and returns the
/// text of a panel it opened ([`keys::type_command`]).
pub type Typist = Arc<dyn Fn(&str) -> (Typed, Option<String>) + Send + Sync>;

/// What the agent can do in its claude's console.
#[derive(Clone)]
pub struct Console {
    pub press: Presser,
    pub type_line: Typist,
}

/// Folders of the agent.
#[derive(Debug, Clone, Default)]
pub struct Dirs {
    /// The agent's own project folder ([`tail::OwnProject`]): transcript
    /// and session reads are answered only from it; without one, none are.
    pub project: Option<Arc<tail::OwnProject>>,
    /// The session's working folder: files from the topic are kept under
    /// it ([`files::save`]) and a relative `send_file` path starts there.
    pub work: Option<PathBuf>,
}

/// Runs the worker agent (`cctg agent-worker`, started by the shim) until
/// stdin closes. Returns the exit code: [`shim::HANDOVER`] after handing over
/// to a newer binary, else 0.
pub async fn run_stdio() -> i32 {
    let config = DeviceConfig::load();
    let session_id = std::env::var("CLAUDE_CODE_SESSION_ID").ok();
    let own_session = session_id.clone();
    let entrypoint = std::env::var("CLAUDE_CODE_ENTRYPOINT").ok();
    // Not env `CLAUDE_PID`: in an MCP server it is inherited from an outer
    // claude, or unset (TASK-004). The shim between claude and this worker
    // is no claude and the walk passes it.
    let claude_pid = proctree::current_lineage(None, None, "").claude_pid;
    let mut worker = Worker::from_env(
        |name| std::env::var(name).ok(),
        claude_pid,
        config.state_dir.clone(),
        None,
    );
    // Only the `cctg run` that started this claude restarts it (and, off
    // Windows, holds its terminal); an inherited `CCTG_RUN` of another
    // session's terminal does not count.
    if let (Some(run), Some(claude)) = (worker.run_pid, claude_pid) {
        let chain = tokio::task::spawn_blocking(move || proctree::ancestors(claude))
            .await
            .ok()
            .flatten()
            .unwrap_or_default();
        if !update::launched_by(&chain, run) {
            debug!("CCTG_RUN is not this claude's parent; no restarts");
            worker.run_pid = None;
        }
    }
    worker.console = console_target(claude_pid, worker.run_pid, worker.state_dir.clone()).await;
    let console = worker.console.clone().map(|target| {
        let pressed = target.clone();
        Console {
            press: Arc::new(move |key| keys::press(&pressed, key)),
            type_line: Arc::new(move |text: &str| keys::type_command(&target, text)),
        }
    });
    // The file hash is this process's own file (the shim's copy); a new
    // build shows up in the file the copy came from. The hub is told the
    // build's source (TASK-035), which needs the hash only without a clean
    // commit.
    let (exe, build) = tokio::task::spawn_blocking(|| {
        let own = std::env::current_exe().ok();
        let build = own.as_deref().and_then(|own| client::build_of(own).ok());
        let exe = std::env::var_os(shim::SOURCE_VAR)
            .map(PathBuf::from)
            .or(own);
        (exe, build)
    })
    .await
    .unwrap_or_default();
    worker.exe = exe;
    worker.build = build;
    let client = worker.build.clone().and_then(|hash| {
        Some(Client {
            version: client::VERSION.to_owned(),
            build: client::identity(client::SOURCE, || Some(hash))?,
            self_update: worker.self_update(),
        })
    });
    let (hub, events) = match link_plan(session_id, entrypoint.as_deref(), &config) {
        Ok(plan) => {
            let register = Register {
                session_id: plan.session_id,
                host: config.host.clone(),
                cwd: device::canonical_cwd(&current_dir()),
                claude_pid,
                verdict_ack: true,
                transcript_reads: true,
                console_keys: console.is_some(),
                console_commands: console.is_some(),
                client,
                files: true,
                session_reads: true,
                heartbeat: true,
            };
            let (outbox, events) = spawn(LinkConfig {
                addr: plan.agent,
                secret: plan.secret,
                register,
                backoff: Backoff::default(),
                replay: config.state_dir.as_deref().map(|state| Replay {
                    spool: spool::dir(state),
                    hook_addr: plan.hook,
                }),
                heartbeat: Heartbeat::default(),
            });
            (Hub::Link(outbox), Some(events))
        }
        Err(reason) => {
            info!(reason = reason.text(), "no hub link");
            (Hub::Off(reason), None)
        }
    };
    let frames = read_frames(std::io::BufReader::new(std::io::stdin()));
    // Found by the env session's transcript, looked for again on each read
    // until it is there: after `/clear` the env id is stale, the folder is
    // not (TASK-034 decision 12).
    let project = tokio::task::spawn_blocking(move || {
        let cwd = std::env::current_dir()
            .ok()
            .and_then(|dir| dir.into_os_string().into_string().ok());
        tail::projects_root()
            .and_then(|root| tail::OwnProject::new(root, own_session.as_deref(), cwd.as_deref()))
    })
    .await
    .ok()
    .flatten()
    .map(Arc::new);
    if project.is_none() {
        info!("no session id or config folder; session reads refused");
    }
    let dirs = Dirs {
        project,
        work: std::env::current_dir().ok(),
    };
    let worker = Some(Arc::new(worker));
    match serve_channel(
        frames,
        tokio::io::stdout(),
        hub,
        events,
        dirs,
        console,
        worker,
    )
    .await
    {
        Ok(Ended::Handover) => shim::HANDOVER,
        Ok(Ended::Input) => 0,
        Err(error) => {
            debug!(kind = ?error.kind(), "stdout closed");
            0
        }
    }
}

/// Where this agent presses and types: its claude's console on Windows;
/// elsewhere the terminal of the `cctg run` that started its claude, when
/// that one answers on its socket ([`crate::term`], TASK-044). `None`: no
/// console keys or commands.
async fn console_target(
    claude_pid: Option<u32>,
    run_pid: Option<u32>,
    state_dir: Option<PathBuf>,
) -> Option<keys::Target> {
    if cfg!(windows) {
        return claude_pid.map(keys::Target::Console);
    }
    let socket = crate::term::socket_path(&state_dir?, run_pid?);
    let probe = socket.clone();
    let answers = tokio::task::spawn_blocking(move || crate::term::screen(&probe).is_some())
        .await
        .unwrap_or(false);
    if !answers {
        info!("cctg run keeps no terminal for this claude; no console keys");
    }
    answers.then_some(keys::Target::Run(socket))
}

/// How this agent talks to the hub: as which session, and where.
#[derive(Debug)]
pub struct LinkPlan {
    pub secret: Secret,
    pub session_id: String,
    /// The agent listener and the hook endpoint (for the spool replay).
    pub agent: HubAddr,
    pub hook: HubAddr,
}

/// Whether this agent talks to the hub, and as which session.
pub fn link_plan(
    session_id: Option<String>,
    entrypoint: Option<&str>,
    config: &DeviceConfig,
) -> Result<LinkPlan, NoHub> {
    if entrypoint == Some("sdk-cli") {
        return Err(NoHub::Headless);
    }
    let session_id = session_id
        .map(|id| id.trim().to_owned())
        .filter(|id| !id.is_empty())
        .ok_or(NoHub::NoSession)?;
    let off = |problem| {
        warn!(%problem, "hub link off");
        NoHub::NoConfig
    };
    let secret = config.secret.clone().map_err(off)?;
    let agent = config.hub(&config.agent_addr).map_err(off)?;
    let hook = config.hub(&config.hook_addr).map_err(off)?;
    Ok(LinkPlan {
        secret,
        session_id,
        agent,
        hook,
    })
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
/// when stdin ends (the hub link stops with it) or stdout fails. Transcript
/// reads are answered only from `dirs.project` ([`tail::OwnProject`]);
/// files from the topic are kept under
/// `dirs.work`. Console keys are pressed and commands typed with `console`;
/// without one a `console_key` or `console_command` is answered as failed.
///
/// `worker` (TASK-040): the worker's place, for `update`. A resumed worker
/// starts with the channel initialized. On `update` see [`crate::update`]:
/// for a newer binary the loop writes [`shim::SWITCH`], answers every line
/// the shim still hands over, tells the hub it leaves and returns
/// [`Ended::Handover`] once the hub released it (or [`LEAVE_WAIT`] passed);
/// for a claude restart it leaves the hub, types `/exit` and runs on until
/// claude closes stdin. Without `worker` an `update` is ignored. A resumed
/// worker first tells Claude Code to list the tools again: it may offer
/// more than the worker Claude Code met.
pub async fn serve_channel<W: AsyncWrite + Unpin>(
    mut frames: mpsc::Receiver<Frame>,
    mut output: W,
    hub: Hub,
    mut events: Option<mpsc::Receiver<LinkEvent>>,
    dirs: Dirs,
    console: Option<Console>,
    worker: Option<Arc<Worker>>,
) -> std::io::Result<Ended> {
    let (reads, session_reads, console_jobs, outbox) = match &hub {
        Hub::Link(outbox) => (
            Some(spawn_reader(outbox.clone(), dirs.project.clone())),
            Some(spawn_session_reader(outbox.clone(), dirs.project.clone())),
            Some(spawn_console(outbox.clone(), console)),
            Some(outbox.clone()),
        ),
        Hub::Off(_) => (None, None, None, None),
    };
    let mut sender = outbox
        .clone()
        .map(|outbox| spawn_sender(outbox, dirs.work.clone()));
    let mut server = channel::Server::new(hub);
    if worker.as_ref().is_some_and(|worker| worker.resumed) {
        server = server.initialized();
        output.write_all(&channel::tools_changed()).await?;
        output.flush().await?;
    }
    // What the hub said at the last registration; nothing while it is away.
    let mut hub_files = false;
    let mut inbox: Option<Incoming> = None;
    // A complete file from the topic being written to disk, off the loop:
    // hub events wait until its message went to Claude, so the ones after it
    // stay after it; Claude Code's lines are answered meanwhile.
    let mut saving: Option<tokio::task::JoinHandle<HubMsg>> = None;
    let mut leaving: Option<Leaving> = None;
    // The hub's release being downloaded for an `update` (TASK-050).
    let mut downloading: Option<(u64, Download, Arc<download::Gate>)> = None;
    let mut frames_open = true;
    let answer = |update_id, outcome| {
        let outbox = outbox.clone();
        async move {
            info!(?outcome, "update answered");
            if let Some(outbox) = outbox {
                let _ = outbox
                    .send(AgentMsg::UpdateAnswer { update_id, outcome })
                    .await;
            }
        }
    };
    loop {
        let deadline = leaving.as_ref().and_then(Leaving::until);
        let lines = tokio::select! {
            frame = frames.recv(), if frames_open => match frame {
                Some(Frame::Line(line)) => {
                    let mut lines = server.on_line(&line);
                    for call in server.take_file_calls() {
                        lines.extend(start_upload(sender.as_ref(), hub_files, call));
                    }
                    lines
                }
                Some(Frame::TooLong) => server.on_oversized_line(),
                None => match leaving {
                    // The shim sends nothing more: every line was answered.
                    Some(Leaving::Draining { update_id }) => {
                        frames_open = false;
                        answer(update_id, UpdateOutcome::Reloading).await;
                        leaving = Some(Leaving::Released {
                            update_id,
                            restart: false,
                            until: Instant::now() + LEAVE_WAIT,
                        });
                        Vec::new()
                    }
                    _ => break,
                },
            },
            () = sleep_until(deadline.unwrap_or_else(Instant::now)), if deadline.is_some() => {
                match leaving.take() {
                    Some(Leaving::Released { restart: false, .. }) => {
                        warn!("hub did not release the agent in time; handing over anyway");
                        output.flush().await?;
                        return Ok(Ended::Handover);
                    }
                    Some(Leaving::Released { update_id, .. }) => {
                        warn!("hub did not release the agent in time; claude stays");
                        answer(update_id, UpdateOutcome::Failed).await;
                    }
                    Some(Leaving::Exiting { update_id, .. }) => {
                        warn!("claude did not exit after /exit; restart request withdrawn");
                        if let Some(worker) = &worker {
                            worker.withdraw_request();
                        }
                        answer(update_id, UpdateOutcome::Failed).await;
                    }
                    other => leaving = other,
                }
                Vec::new()
            }
            fetched = async {
                match downloading.as_mut() {
                    Some((_, task, _)) => task.await,
                    None => std::future::pending().await,
                }
            }, if downloading.is_some() => {
                let update_id = downloading.take().map_or(0, |(update_id, ..)| update_id);
                match (fetched, worker.clone()) {
                    (Ok(Ok(_)), Some(worker)) => {
                        follow_plan(&worker, update_id, &mut leaving, &answer, None).await
                    }
                    // The release did not come, but a build put in place
                    // otherwise (install.sh, cctg deploy) is still taken.
                    (
                        Ok(Err(failure @ (download::Failure::Missing | download::Failure::Download))),
                        Some(worker),
                    ) => {
                        let failed = Some(download_outcome(failure));
                        follow_plan(&worker, update_id, &mut leaving, &answer, failed).await
                    }
                    (Ok(Err(failure)), _) => {
                        answer(update_id, download_outcome(failure)).await;
                        Vec::new()
                    }
                    _ => {
                        answer(update_id, UpdateOutcome::Failed).await;
                        Vec::new()
                    }
                }
            }
            saved = async {
                match saving.as_mut() {
                    Some(save) => save.await,
                    None => std::future::pending().await,
                }
            }, if saving.is_some() => {
                saving = None;
                match saved {
                    Ok(inbound) => server.on_link(LinkEvent::Message(inbound)),
                    Err(_) => {
                        warn!("saving a file from the topic failed");
                        Vec::new()
                    }
                }
            }
            event = recv_event(&mut events), if events.is_some() && saving.is_none() => match event {
                Some(LinkEvent::Message(HubMsg::Update { update_id, release })) => {
                    let Some(worker) = worker
                        .clone()
                        .filter(|_| leaving.is_none() && downloading.is_none())
                    else {
                        debug!("update without a worker or during another one; ignored");
                        continue;
                    };
                    match (release, worker.exe.clone()) {
                        // The hub's release first (TASK-050), off the loop:
                        // Claude Code's lines are answered meanwhile.
                        (Some(tag), Some(exe)) if worker.self_update() => {
                            let base = download::base_from_env();
                            let gate = Arc::new(download::Gate::default());
                            let task_gate = gate.clone();
                            downloading = Some((
                                update_id,
                                tokio::spawn(async move {
                                    download::fetch(&base, &tag, &exe, &task_gate).await
                                }),
                                gate,
                            ));
                            Vec::new()
                        }
                        _ => follow_plan(&worker, update_id, &mut leaving, &answer, None).await,
                    }
                }
                Some(LinkEvent::Message(HubMsg::Released { update_id, session_id })) => {
                    match leaving.take() {
                        Some(Leaving::Released { update_id: id, restart: false, .. }) if id == update_id => {
                            output.flush().await?;
                            return Ok(Ended::Handover);
                        }
                        Some(Leaving::Released { update_id: id, restart: true, .. }) if id == update_id => {
                            let typed = match worker.clone() {
                                Some(worker) => tokio::task::spawn_blocking(move || worker.restart(&session_id))
                                    .await
                                    .unwrap_or(Typed::Failed),
                                None => Typed::Failed,
                            };
                            info!(?typed, "claude restart");
                            match typed {
                                Typed::Sent => {
                                    leaving = Some(Leaving::Exiting {
                                        update_id,
                                        until: Instant::now() + EXIT_WAIT,
                                    });
                                }
                                Typed::Draft => answer(update_id, UpdateOutcome::DraftInInput).await,
                                Typed::Agents => answer(update_id, UpdateOutcome::AgentsRunning).await,
                                Typed::Failed => answer(update_id, UpdateOutcome::Failed).await,
                            }
                        }
                        other => {
                            debug!("release of another update; ignored");
                            leaving = other;
                        }
                    }
                    Vec::new()
                }
                Some(LinkEvent::Message(HubMsg::TranscriptRead { session_id, from, .. })) => {
                    // One read at a time: while one runs, a request waits in
                    // the slot and a further one is dropped (the hub asks
                    // again after its timeout). Its `path` is never opened:
                    // the agent builds the path from the session id.
                    if let Some(reads) = &reads
                        && reads.try_send((session_id, from)).is_err()
                    {
                        debug!("transcript read busy; request dropped");
                    }
                    Vec::new()
                }
                Some(LinkEvent::Message(HubMsg::SessionRead { read_id, session_id, ask })) => {
                    // The hub has few reads out per session (TASK-034); one
                    // beyond the queue is dropped and the hub's wait runs out.
                    if let Some(session_reads) = &session_reads
                        && session_reads.try_send((read_id, session_id, ask)).is_err()
                    {
                        debug!("session reads busy; request dropped");
                    }
                    Vec::new()
                }
                Some(LinkEvent::Message(HubMsg::ConsoleKey { key_id, key })) => {
                    // Presses queue up; one beyond the queue is dropped (the
                    // hub takes a missing answer as nothing done).
                    if let Some(console_jobs) = &console_jobs
                        && console_jobs.try_send(ConsoleJob::Key(key_id, key)).is_err()
                    {
                        debug!("console key queue full; key dropped");
                    }
                    Vec::new()
                }
                Some(LinkEvent::Message(HubMsg::ConsoleCommand { command_id, text })) => {
                    // Like keys: one beyond the queue is dropped, the hub
                    // forgets an unanswered command.
                    if let Some(console_jobs) = &console_jobs
                        && console_jobs.try_send(ConsoleJob::Line(command_id, text)).is_err()
                    {
                        debug!("console queue full; command dropped");
                    }
                    Vec::new()
                }
                Some(LinkEvent::Message(HubMsg::FileAnswer { transfer_id, outcome })) => {
                    if let Some(sender) = &sender {
                        let _ = sender.events.send(Upload::Answer { transfer_id, outcome });
                    }
                    Vec::new()
                }
                Some(LinkEvent::Message(HubMsg::FileStart { transfer_id, name, size, kind, content, meta })) => {
                    // One file at a time: a new start drops an unfinished one.
                    if size > files::MAX_DOWNLOAD {
                        warn!(size, "file from the topic larger than the hub may send; dropped");
                        inbox = None;
                        Vec::new()
                    } else {
                        let incoming = Incoming {
                            transfer_id,
                            name,
                            kind,
                            content,
                            meta,
                            assembly: files::Assembly::new(size),
                        };
                        if incoming.assembly.is_complete() {
                            inbox = None;
                            saving = Some(tokio::spawn(deliver(incoming, dirs.work.clone())));
                            Vec::new()
                        } else {
                            inbox = Some(incoming);
                            Vec::new()
                        }
                    }
                }
                Some(LinkEvent::Message(HubMsg::FileChunk(chunk))) => {
                    match receive(&mut inbox, &chunk) {
                        Some(incoming) => {
                            saving = Some(tokio::spawn(deliver(incoming, dirs.work.clone())));
                            Vec::new()
                        }
                        None => Vec::new(),
                    }
                }
                Some(event @ LinkEvent::Up { files }) => {
                    hub_files = files;
                    server.on_link(event)
                }
                Some(LinkEvent::Down) => {
                    // A transfer of either way ends with its link.
                    hub_files = false;
                    if inbox.take().is_some() {
                        info!("hub link lost; the unfinished file from the topic is dropped");
                    }
                    if let Some(sender) = &sender {
                        let _ = sender.events.send(Upload::Lost);
                    }
                    server.on_link(LinkEvent::Down)
                }
                Some(event) => server.on_link(event),
                None => {
                    events = None;
                    Vec::new()
                }
            },
            answer = recv_answer(&mut sender), if sender.is_some() => match answer {
                Some(line) => vec![line],
                None => {
                    sender = None;
                    Vec::new()
                }
            },
        };
        for line in lines {
            output.write_all(&line).await?;
        }
        output.flush().await?;
    }
    output.flush().await?;
    // A download that has not begun its swap writes nothing now; a swap
    // cut off by the process exit could leave no binary in place, so one
    // that began is waited for (a write and two renames).
    if let Some((_, task, gate)) = downloading
        && !gate.close()
    {
        let _ = tokio::time::timeout(Duration::from_secs(30), task).await;
    }
    Ok(Ended::Input)
}

/// The hub's release on its way to the worker's file ([`download::fetch`]).
type Download = JoinHandle<Result<download::Fetched, download::Failure>>;

fn download_outcome(failure: download::Failure) -> UpdateOutcome {
    match failure {
        download::Failure::Download => UpdateOutcome::DownloadFailed,
        download::Failure::Missing => UpdateOutcome::NoReleaseBuild,
        download::Failure::Checksum => UpdateOutcome::ChecksumMismatch,
    }
}

/// Carries out what `update` leads to ([`Worker::plan`]), after the hub's
/// release was put in place when it sent one: a hand-over (the lines to
/// write, [`shim::SWITCH`]), a claude restart, or an answer. When the
/// release could not be put in place (`failed`), only a hand-over to a
/// newer file on disk goes on; anything else answers `failed`.
async fn follow_plan<F, Fut>(
    worker: &Arc<Worker>,
    update_id: u64,
    leaving: &mut Option<Leaving>,
    answer: &F,
    failed: Option<UpdateOutcome>,
) -> Vec<Vec<u8>>
where
    F: Fn(u64, UpdateOutcome) -> Fut,
    Fut: std::future::Future<Output = ()>,
{
    let plan = tokio::task::spawn_blocking({
        let worker = worker.clone();
        move || {
            if let Some(exe) = &worker.exe {
                download::recover_in(exe);
            }
            worker.plan()
        }
    })
    .await
    .unwrap_or(Plan::Failed);
    info!(?plan, "update asked");
    if let Some(outcome) = failed.filter(|_| plan != Plan::Reload) {
        answer(update_id, outcome).await;
        return Vec::new();
    }
    match plan {
        Plan::Reload => {
            *leaving = Some(Leaving::Draining { update_id });
            vec![shim::SWITCH.to_vec()]
        }
        Plan::Restart
            if tokio::task::spawn_blocking({
                let worker = worker.clone();
                move || worker.agents_on_screen()
            })
            .await
            .unwrap_or(false) =>
        {
            // Looked at before leaving too (and again before `/exit`):
            // waiting for background agents does not unbind the agent from
            // the hub.
            answer(update_id, UpdateOutcome::AgentsRunning).await;
            Vec::new()
        }
        Plan::Restart => {
            answer(update_id, UpdateOutcome::Restarting).await;
            *leaving = Some(Leaving::Released {
                update_id,
                restart: true,
                until: Instant::now() + LEAVE_WAIT,
            });
            Vec::new()
        }
        Plan::ManualRestart => {
            answer(update_id, UpdateOutcome::NeedsManualRestart).await;
            Vec::new()
        }
        Plan::UpToDate => {
            answer(update_id, UpdateOutcome::UpToDate).await;
            Vec::new()
        }
        Plan::Failed => {
            answer(update_id, UpdateOutcome::Failed).await;
            Vec::new()
        }
    }
}

/// How [`serve_channel`] ended.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Ended {
    /// stdin closed.
    Input,
    /// Handed over to a newer binary; exit with [`shim::HANDOVER`].
    Handover,
}

/// How long a leaving agent waits for the hub's `released`.
pub const LEAVE_WAIT: Duration = Duration::from_secs(5);
/// How long claude gets to exit after `/exit` went in.
pub const EXIT_WAIT: Duration = Duration::from_secs(20);

/// An update in progress.
enum Leaving {
    /// [`shim::SWITCH`] went out; the lines the shim still hands over are
    /// answered until its stdin ends.
    Draining { update_id: u64 },
    /// The leaving answer went to the hub; `released` is due before `until`.
    Released {
        update_id: u64,
        restart: bool,
        until: Instant,
    },
    /// `/exit` went in; claude closes stdin before `until`.
    Exiting { update_id: u64, until: Instant },
}

impl Leaving {
    fn until(&self) -> Option<Instant> {
        match self {
            Self::Draining { .. } => None,
            Self::Released { until, .. } | Self::Exiting { until, .. } => Some(*until),
        }
    }
}

/// A file from the topic being received.
struct Incoming {
    transfer_id: u64,
    name: String,
    kind: FileKind,
    content: String,
    meta: BTreeMap<String, String>,
    assembly: files::Assembly,
}

/// Takes a chunk of the file in `inbox`; returns the file once complete.
/// A chunk of another transfer is dropped; a broken one drops the file.
fn receive(inbox: &mut Option<Incoming>, chunk: &FileChunk) -> Option<Incoming> {
    let Some(incoming) = inbox
        .as_mut()
        .filter(|incoming| incoming.transfer_id == chunk.transfer_id)
    else {
        debug!("chunk of a file not being received; dropped");
        return None;
    };
    match incoming.assembly.push(chunk) {
        Ok(true) => inbox.take(),
        Ok(false) => None,
        Err(error) => {
            warn!(%error, "file from the topic broken; dropped");
            *inbox = None;
            None
        }
    }
}

/// Saves a complete file from the topic and makes the message Claude reads:
/// where the file is, then the words that came with it; meta `file_kind`,
/// `file_size` and, when it was saved, `file_path`.
async fn deliver(incoming: Incoming, work: Option<PathBuf>) -> HubMsg {
    let Incoming {
        name,
        kind,
        content,
        mut meta,
        assembly,
        ..
    } = incoming;
    let size = assembly.size();
    let name = files::clean_name(&name, kind.as_str());
    let bytes = assembly.into_bytes();
    let saved = tokio::task::spawn_blocking(move || {
        files::save(work.as_deref(), &name, &bytes, SystemTime::now()).ok()
    })
    .await
    .ok()
    .flatten();
    if saved.is_some() {
        info!(kind = kind.as_str(), size, "file from the topic saved");
    } else {
        warn!(
            kind = kind.as_str(),
            size, "file from the topic could not be saved"
        );
    }
    meta.insert("file_kind".to_owned(), kind.as_str().to_owned());
    meta.insert("file_size".to_owned(), size.to_string());
    if let Some(path) = &saved {
        meta.insert("file_path".to_owned(), path.to_string_lossy().into_owned());
    }
    HubMsg::Inbound {
        content: file_content(kind, saved.as_deref(), size, &content),
        meta,
    }
}

/// What Claude reads for a file from the topic.
fn file_content(kind: FileKind, saved: Option<&Path>, size: u64, words: &str) -> String {
    let kind = kind.as_str();
    let article = if kind.starts_with(['a', 'e', 'i', 'o', 'u']) {
        "An"
    } else {
        "A"
    };
    let mut content = match saved {
        Some(path) => format!(
            "{article} {kind} from the Telegram topic is saved at {} ({size} bytes).",
            path.display()
        ),
        None => format!(
            "{article} {kind} ({size} bytes) came from the Telegram topic but could not be saved on this machine."
        ),
    };
    if !words.is_empty() {
        content.push_str("\n\n");
        content.push_str(words);
    }
    content
}

/// How long the hub has to accept an offer: it answers at once when it
/// takes files, so silence means it does not.
pub const OFFER_WAIT: Duration = Duration::from_secs(10);
/// How long a `send_file` call waits for Telegram's answer before it says
/// the file is on its way; below Claude Code's two minutes after which a
/// call moves to the background.
pub const SENT_WAIT: Duration = Duration::from_secs(90);
/// `send_file` calls waiting behind the one being sent.
const UPLOADS: usize = 4;

/// The one worker that sends `send_file` files to the hub, one at a time.
struct Sender {
    calls: mpsc::Sender<FileCall>,
    /// Hub answers and link losses, from the loop.
    events: mpsc::UnboundedSender<Upload>,
    /// Finished tool answer lines.
    answers: mpsc::Receiver<Vec<u8>>,
}

#[derive(Debug)]
enum Upload {
    Answer {
        transfer_id: u64,
        outcome: FileOutcome,
    },
    Lost,
}

/// Hands a `send_file` call to the sender; the answer line comes later.
/// Without a hub on line that takes files it is answered at once.
fn start_upload(sender: Option<&Sender>, hub_files: bool, call: FileCall) -> Option<Vec<u8>> {
    let refuse = |call: &FileCall, text: &str| Some(channel::tool_answer(&call.id, text, true));
    let Some(sender) = sender else {
        return refuse(&call, "cctg: no hub link; nothing was sent");
    };
    if !hub_files {
        return refuse(
            &call,
            "The cctg hub is not reachable right now, or it is older than this agent and takes no files; nothing was sent.",
        );
    }
    match sender.calls.try_send(call) {
        Ok(()) => None,
        Err(error) => refuse(
            &error.into_inner(),
            "Too many files are being sent; try again when they are done.",
        ),
    }
}

fn spawn_sender(outbox: mpsc::Sender<AgentMsg>, work: Option<PathBuf>) -> Sender {
    let (calls, mut pending) = mpsc::channel::<FileCall>(UPLOADS);
    let (events, mut uploads) = mpsc::unbounded_channel();
    let (answer, answers) = mpsc::channel(UPLOADS + 1);
    tokio::spawn(async move {
        while let Some(call) = pending.recv().await {
            let (text, is_error) = upload(&call, &outbox, &mut uploads, work.as_deref()).await;
            if answer
                .send(channel::tool_answer(&call.id, &text, is_error))
                .await
                .is_err()
            {
                return;
            }
        }
    });
    Sender {
        calls,
        events,
        answers,
    }
}

async fn recv_answer(sender: &mut Option<Sender>) -> Option<Vec<u8>> {
    match sender {
        Some(sender) => sender.answers.recv().await,
        None => None,
    }
}

/// What came of waiting for the hub.
enum Heard {
    Outcome(FileOutcome),
    Lost,
    Silence,
}

/// Waits up to `limit` for the hub's answer to `transfer_id`; answers of
/// other transfers are skipped.
async fn hear(
    uploads: &mut mpsc::UnboundedReceiver<Upload>,
    transfer_id: u64,
    limit: Duration,
) -> Heard {
    let deadline = tokio::time::Instant::now() + limit;
    loop {
        match tokio::time::timeout_at(deadline, uploads.recv()).await {
            Ok(Some(Upload::Answer {
                transfer_id: id,
                outcome,
            })) if id == transfer_id => return Heard::Outcome(outcome),
            Ok(Some(Upload::Answer { .. })) => continue,
            Ok(Some(Upload::Lost)) | Ok(None) => return Heard::Lost,
            Err(_) => return Heard::Silence,
        }
    }
}

/// Waits for room in the link queue for the next chunk ([`files::room`]);
/// an answer to `transfer_id` or a lost link ends the wait: while the link
/// reconnects nothing drains the queue.
async fn room_or_heard(
    outbox: &mpsc::Sender<AgentMsg>,
    uploads: &mut mpsc::UnboundedReceiver<Upload>,
    transfer_id: u64,
) -> Option<Heard> {
    loop {
        tokio::select! {
            biased;
            event = uploads.recv() => match event {
                Some(Upload::Answer { transfer_id: id, outcome }) if id == transfer_id => {
                    return Some(Heard::Outcome(outcome));
                }
                Some(Upload::Answer { .. }) => {}
                Some(Upload::Lost) | None => return Some(Heard::Lost),
            },
            ready = files::room(outbox) => return (!ready).then_some(Heard::Lost),
        }
    }
}

const LOST: &str = "The link to the cctg hub dropped during the transfer; the file may not have reached Telegram. Try again.";

/// Sends one file; the tool answer text and whether it is an error.
async fn upload(
    call: &FileCall,
    outbox: &mpsc::Sender<AgentMsg>,
    uploads: &mut mpsc::UnboundedReceiver<Upload>,
    work: Option<&Path>,
) -> (String, bool) {
    let path = match (Path::new(&call.path), work) {
        (path, Some(work)) if path.is_relative() => work.join(path),
        (path, _) => path.to_owned(),
    };
    let name = files::clean_name(
        &path
            .file_name()
            .map(|name| name.to_string_lossy().into_owned())
            .unwrap_or_default(),
        "file",
    );
    let bytes = match tokio::task::spawn_blocking(move || files::read_upload(&path)).await {
        Ok(Ok(bytes)) => bytes,
        Ok(Err(error)) => return (format!("Nothing was sent: {error}."), true),
        Err(_) => {
            return (
                "Nothing was sent: the file could not be read.".to_owned(),
                true,
            );
        }
    };
    // Answers and losses of earlier transfers mean nothing now.
    while uploads.try_recv().is_ok() {}
    let transfer_id = wire::random_u64();
    let size = bytes.len() as u64;
    info!(size, "file offered to the hub");
    // Without a caption the topic still says what the file is (TASK-051):
    // a photo shows no name of its own.
    let caption = call.caption.clone().unwrap_or_else(|| name.clone());
    let offer = AgentMsg::FileOffer {
        transfer_id,
        name,
        size,
        caption: Some(caption),
    };
    if outbox.send(offer).await.is_err() {
        return (
            "The cctg hub link is gone; nothing was sent.".to_owned(),
            true,
        );
    }
    match hear(uploads, transfer_id, OFFER_WAIT).await {
        Heard::Outcome(FileOutcome::Accepted) => {}
        Heard::Outcome(outcome) => return refused(outcome),
        Heard::Lost => return (LOST.to_owned(), true),
        Heard::Silence => {
            return (
                "The cctg hub did not answer; nothing was sent.".to_owned(),
                true,
            );
        }
    }
    for chunk in files::chunks(transfer_id, &bytes) {
        match room_or_heard(outbox, uploads, transfer_id).await {
            Some(Heard::Outcome(outcome)) => return refused(outcome),
            Some(_) => return (LOST.to_owned(), true),
            None => {}
        }
        if outbox.send(AgentMsg::FileChunk(chunk)).await.is_err() {
            return (LOST.to_owned(), true);
        }
    }
    match hear(uploads, transfer_id, SENT_WAIT).await {
        Heard::Outcome(FileOutcome::Sent) => {
            info!(size, "file sent to the topic");
            (
                "Sent to the Telegram topic of this session.".to_owned(),
                false,
            )
        }
        Heard::Outcome(outcome) => refused(outcome),
        Heard::Lost => (LOST.to_owned(), true),
        Heard::Silence => (
            "The file is with the cctg hub; Telegram has not confirmed it yet.".to_owned(),
            false,
        ),
    }
}

/// The tool answer for a hub outcome that is not success.
fn refused(outcome: FileOutcome) -> (String, bool) {
    warn!(?outcome, "file not sent");
    let text = match outcome {
        FileOutcome::NoTopic => {
            "This session is not the live session of a Telegram topic right now; nothing was sent."
        }
        FileOutcome::Busy => {
            "The cctg hub has too many files waiting for Telegram; try again in a minute."
        }
        FileOutcome::Failed => "Telegram did not take the file; nothing was sent.",
        FileOutcome::Accepted | FileOutcome::Sent | FileOutcome::Other => {
            "The cctg hub gave an answer this agent does not know; the file may not have been sent."
        }
    };
    (text.to_owned(), true)
}

type ReadRequest = (String, Option<u64>);

/// The one worker that reads transcript chunks off the loop, one blocking
/// read at a time, and queues each answer for the hub; the hub asks again if
/// an answer gets lost with the link.
fn spawn_reader(
    outbox: mpsc::Sender<AgentMsg>,
    project: Option<Arc<tail::OwnProject>>,
) -> mpsc::Sender<ReadRequest> {
    let (requests, mut pending) = mpsc::channel::<ReadRequest>(1);
    tokio::spawn(async move {
        while let Some((session_id, from)) = pending.recv().await {
            let project = project.clone();
            let chunk = tokio::task::spawn_blocking(move || {
                tail::read_chunk(project.as_deref(), &session_id, from)
            })
            .await;
            if let Ok(chunk) = chunk
                && outbox.send(chunk).await.is_err()
            {
                debug!("hub link gone; transcript chunk dropped");
                return;
            }
        }
    });
    requests
}

type SessionRead = (u64, String, SessionAsk);
/// Session reads waiting for the reader.
const SESSION_READS: usize = 8;

/// The one worker that answers `session_read`s off the loop, one blocking
/// read at a time, apart from the transcript stream's reader (a `/full` of a
/// long session must not hold up the stream); each answer, and each piece of
/// a text, is queued for the hub in order.
fn spawn_session_reader(
    outbox: mpsc::Sender<AgentMsg>,
    project: Option<Arc<tail::OwnProject>>,
) -> mpsc::Sender<SessionRead> {
    let (requests, mut pending) = mpsc::channel::<SessionRead>(SESSION_READS);
    tokio::spawn(async move {
        while let Some((read_id, session_id, ask)) = pending.recv().await {
            let project = project.clone();
            let answers = tokio::task::spawn_blocking(move || {
                reads::answer(project.as_deref(), &session_id, ask)
            })
            .await
            .unwrap_or_default();
            for answer in answers {
                if outbox
                    .send(AgentMsg::SessionAnswer { read_id, answer })
                    .await
                    .is_err()
                {
                    debug!("hub link gone; session answer dropped");
                    return;
                }
            }
        }
    });
    requests
}

/// A key to press or a line to type, with the hub's id for the answer.
enum ConsoleJob {
    Key(u64, ConsoleKey),
    Line(u64, String),
}

/// The one worker that presses console keys and types commands off the
/// loop, one at a time (a console is attached per process), and answers
/// each with `console_key_written` or `console_command_typed`. A line that
/// is not [`keys::typable`] is answered as failed without typing.
fn spawn_console(
    outbox: mpsc::Sender<AgentMsg>,
    console: Option<Console>,
) -> mpsc::Sender<ConsoleJob> {
    let (requests, mut pending) = mpsc::channel::<ConsoleJob>(4);
    tokio::spawn(async move {
        while let Some(job) = pending.recv().await {
            let answer = match job {
                ConsoleJob::Key(key_id, key) => {
                    let written = match console.clone() {
                        Some(console) => tokio::task::spawn_blocking(move || (console.press)(key))
                            .await
                            .unwrap_or(false),
                        None => false,
                    };
                    if written {
                        info!(?key, "console key written");
                    } else {
                        warn!(?key, "console key not written");
                    }
                    AgentMsg::ConsoleKeyWritten { key_id, written }
                }
                ConsoleJob::Line(command_id, text) => {
                    let (typed, panel) = match console.clone().filter(|_| keys::typable(&text)) {
                        Some(console) => {
                            tokio::task::spawn_blocking(move || (console.type_line)(&text))
                                .await
                                .unwrap_or((Typed::Failed, None))
                        }
                        None => (Typed::Failed, None),
                    };
                    info!(?typed, panel = panel.is_some(), "console command");
                    let outcome = match typed {
                        Typed::Sent => CommandOutcome::Sent,
                        Typed::Draft => CommandOutcome::Draft,
                        Typed::Agents => CommandOutcome::AgentsRunning,
                        Typed::Failed => CommandOutcome::Failed,
                    };
                    AgentMsg::ConsoleCommandTyped {
                        command_id,
                        outcome,
                        panel,
                    }
                }
            };
            if outbox.send(answer).await.is_err() {
                return;
            }
        }
    });
    requests
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
    use tokio::net::tcp::{OwnedReadHalf, OwnedWriteHalf};

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
            verdict_ack: true,
            transcript_reads: true,
            console_keys: false,
            console_commands: false,
            client: None,
            files: true,
            session_reads: false,
            heartbeat: false,
        }
    }

    fn config(addr: SocketAddr, backoff: Backoff) -> LinkConfig {
        LinkConfig {
            addr: HubAddr::plain(addr.to_string()),
            secret: Secret::parse(SECRET).unwrap(),
            register: register(),
            backoff,
            replay: None,
            heartbeat: Heartbeat::default(),
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
        // The hub says it takes files.
        assert_eq!(next(&mut events).await, LinkEvent::Up { files: true });
        let (got, to_agent) = registered(&mut hub_rx).await;
        assert_eq!(got, register());
        to_agent
            .send(HubMsg::Registered {
                files: true,
                heartbeat: false,
            })
            .await
            .unwrap();
        assert_eq!(
            next(&mut events).await,
            LinkEvent::Message(HubMsg::Registered {
                files: true,
                heartbeat: false
            })
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
                LinkEvent::Up { .. } => break,
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

    /// A registration while the last replay still waits on a silent hub
    /// starts no second replay of the same files.
    #[tokio::test]
    async fn one_replay_runs_at_a_time() {
        let dir = crate::hub::testdir::TempDir::new("agent-replay");
        let spool_dir = dir.path().join("spool");
        let kept = crate::wire::HookPost::new(
            "box".into(),
            register().session_id,
            "/w".into(),
            "/w/s.jsonl".into(),
            crate::wire::HookEvent::SessionStart {
                source: Some("startup".into()),
                claude_pid: None,
                parent_claude_pid: None,
            },
        );
        spool::save(&spool_dir, &kept, std::time::SystemTime::now()).unwrap();
        // The hook endpoint accepts, counts and never answers.
        let silent = TcpListener::bind(SocketAddr::from((Ipv4Addr::LOCALHOST, 0)))
            .await
            .unwrap();
        let hook_addr = HubAddr::plain(silent.local_addr().unwrap().to_string());
        let accepted = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let counter = accepted.clone();
        tokio::spawn(async move {
            let mut open = Vec::new();
            while let Ok((stream, _)) = silent.accept().await {
                counter.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                open.push(stream);
            }
        });
        let mut link = config(
            SocketAddr::from((Ipv4Addr::LOCALHOST, 9)),
            Backoff::default(),
        );
        link.replay = Some(Replay {
            spool: spool_dir,
            hook_addr,
        });
        let mut running = None;
        spawn_replay(&link, &mut running);
        tokio::time::sleep(Duration::from_millis(200)).await;
        spawn_replay(&link, &mut running);
        tokio::time::sleep(Duration::from_millis(200)).await;
        assert_eq!(accepted.load(std::sync::atomic::Ordering::SeqCst), 1);
        running.take().unwrap().abort();
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
        wire::write_msg(
            &mut write,
            &HubMsg::Registered {
                files: false,
                heartbeat: false,
            },
        )
        .await
        .unwrap();
        assert_eq!(next(&mut events).await, LinkEvent::Up { files: false });

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

    /// Short heartbeat for the TASK-049 tests: pings every 100 ms, a link
    /// silent for 600 ms is dead.
    const BEAT: Heartbeat = Heartbeat {
        interval: Duration::from_millis(100),
        timeout: Duration::from_millis(600),
    };

    fn beating(addr: SocketAddr) -> LinkConfig {
        let backoff = Backoff {
            initial: Duration::from_millis(20),
            max: Duration::from_millis(40),
        };
        let mut link = config(addr, backoff);
        link.register.heartbeat = true;
        link.heartbeat = BEAT;
        link
    }

    /// A hub stand-in: takes `hello` and `register`, answers `registered`
    /// with `heartbeat` as given, and returns the open connection.
    async fn fake_hub(
        listener: &TcpListener,
        heartbeat: bool,
    ) -> (BufReader<OwnedReadHalf>, OwnedWriteHalf) {
        let (stream, _) = tokio::time::timeout(WAIT, listener.accept())
            .await
            .expect("agent connected")
            .unwrap();
        let (read, mut write) = stream.into_split();
        let mut reader = BufReader::new(read);
        let mut line = Vec::new();
        for _ in 0..2 {
            wire::read_line(&mut reader, &mut line).await.unwrap();
            line.clear();
        }
        let registered = HubMsg::Registered {
            files: false,
            heartbeat,
        };
        wire::write_msg(&mut write, &registered).await.unwrap();
        (reader, write)
    }

    /// TASK-049: a hub that stops answering but never closes (a NAT on the
    /// way forgot the connection) is left after the timeout, and the agent
    /// connects again.
    #[tokio::test]
    async fn a_frozen_hub_is_left_after_the_heartbeat_timeout() {
        let listener = TcpListener::bind(SocketAddr::from((Ipv4Addr::LOCALHOST, 0)))
            .await
            .unwrap();
        let (_outbox, mut events) = spawn(beating(listener.local_addr().unwrap()));
        let (mut reader, _write) = fake_hub(&listener, true).await;
        assert_eq!(next(&mut events).await, LinkEvent::Up { files: false });
        let up = tokio::time::Instant::now();
        // The agent pings the quiet hub.
        let mut line = Vec::new();
        tokio::time::timeout(WAIT, wire::read_line(&mut reader, &mut line))
            .await
            .unwrap()
            .unwrap();
        assert_eq!(wire::decode::<AgentMsg>(&line), Ok(AgentMsg::Ping));
        // The hub never writes: the link is given up once the timeout passed.
        assert_eq!(next(&mut events).await, LinkEvent::Down);
        assert!(up.elapsed() >= BEAT.timeout, "{:?}", up.elapsed());
        let (_reader, _write) = fake_hub(&listener, true).await;
        assert_eq!(next(&mut events).await, LinkEvent::Up { files: false });
    }

    /// A hub before TASK-049 gets no ping and is waited for as long as the
    /// connection stays open; so is any hub when the agent announced none.
    #[tokio::test]
    async fn without_a_heartbeat_on_both_ends_nothing_is_sent_or_timed() {
        for (agent, hub) in [(true, false), (false, true)] {
            let listener = TcpListener::bind(SocketAddr::from((Ipv4Addr::LOCALHOST, 0)))
                .await
                .unwrap();
            let mut link = beating(listener.local_addr().unwrap());
            link.register.heartbeat = agent;
            let (_outbox, mut events) = spawn(link);
            let (mut reader, _write) = fake_hub(&listener, hub).await;
            assert_eq!(next(&mut events).await, LinkEvent::Up { files: false });
            let mut line = Vec::new();
            let quiet = BEAT.timeout * 2;
            let read = tokio::time::timeout(quiet, wire::read_line(&mut reader, &mut line)).await;
            assert!(read.is_err(), "agent {agent}, hub {hub}: {line:?}");
            assert!(events.try_recv().is_err(), "agent {agent}, hub {hub}");
        }
    }

    /// Pings keep a quiet link up while the owner reads nothing (a worker
    /// that hands over or waits for claude to exit, TASK-040), and one-way
    /// traffic either way (file chunks, TASK-032) does not trip the side
    /// that only reads.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn the_heartbeat_holds_a_quiet_or_one_way_link() {
        let listener = ingress::bind(SocketAddr::from((Ipv4Addr::LOCALHOST, 0)))
            .await
            .unwrap();
        let addr = listener.local_addr().unwrap();
        let (hub_tx, mut hub_rx) = mpsc::channel(16);
        let _hub = tokio::spawn(ingress::serve_agents_with(
            listener,
            Secret::parse(SECRET).unwrap(),
            hub_tx,
            BEAT,
        ));
        let (outbox, mut events) = spawn(beating(addr));
        let (_, to_agent) = registered(&mut hub_rx).await;
        assert_eq!(next(&mut events).await, LinkEvent::Up { files: true });

        // Quiet, and nobody reads the link events.
        let quiet = tokio::time::timeout(BEAT.timeout * 3, hub_rx.recv()).await;
        assert!(quiet.is_err(), "{quiet:?}");
        assert!(events.try_recv().is_err());

        // The agent writes, the hub only reads.
        let span = BEAT.timeout * 2;
        let started = tokio::time::Instant::now();
        let mut replies = 0;
        while started.elapsed() < span {
            let reply = AgentMsg::Reply {
                text: "chunk".into(),
            };
            outbox.send(reply).await.unwrap();
            match within(hub_rx.recv()).await {
                Some(AgentEvent::Message { .. }) => replies += 1,
                other => panic!("expected the reply, got {other:?}"),
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
        assert!(replies > 10, "{replies}");

        // The hub writes, the agent only reads.
        let started = tokio::time::Instant::now();
        while started.elapsed() < span {
            let inbound = HubMsg::Inbound {
                content: "chunk".into(),
                meta: Default::default(),
            };
            to_agent.send(inbound.clone()).await.unwrap();
            assert_eq!(next(&mut events).await, LinkEvent::Message(inbound));
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
        assert!(hub_rx.try_recv().is_err(), "the hub kept the agent");
        assert!(events.try_recv().is_err(), "the agent kept the hub");
    }

    async fn within<T>(future: impl std::future::Future<Output = T>) -> T {
        tokio::time::timeout(WAIT, future).await.expect("in time")
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
        let plan = link_plan(session(), Some("cli"), &device(Some(SECRET))).unwrap();
        assert_eq!(plan.session_id, "5e551017-0000-4000-8000-000000000001");
        assert!(!plan.agent.is_tls() && !plan.hook.is_tls());
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
        // A hub elsewhere without a pin: no link, the secret stays here.
        let remote = DeviceConfig::from_vars(|name| match name {
            crate::hub::config::SECRET_VAR => Some(SECRET.to_owned()),
            crate::device::AGENT_ADDR_VAR => Some("hub.example.org:47291".to_owned()),
            _ => None,
        });
        assert_eq!(
            link_plan(session(), Some("cli"), &remote).unwrap_err(),
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
        claude_reading(hub, events, None)
    }

    fn claude_reading(
        hub: Hub,
        events: Option<mpsc::Receiver<LinkEvent>>,
        project: Option<PathBuf>,
    ) -> Claude {
        let (frames, frames_rx) = mpsc::channel(16);
        let (ours, theirs) = tokio::io::duplex(1 << 16);
        let dirs = Dirs {
            project: project.map(|folder| Arc::new(tail::OwnProject::at(folder))),
            work: None,
        };
        tokio::spawn(serve_channel(
            frames_rx, ours, hub, events, dirs, None, None,
        ));
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
                verdict_id: None,
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

    /// Accepts one agent on `listener` and answers its handshake as a hub
    /// that takes no files.
    async fn raw_hub(listener: &TcpListener) -> (BufReader<OwnedReadHalf>, OwnedWriteHalf) {
        raw_hub_files(listener, false).await
    }

    async fn agent_line(reader: &mut BufReader<OwnedReadHalf>) -> AgentMsg {
        let mut line = Vec::new();
        tokio::time::timeout(WAIT, wire::read_line(reader, &mut line))
            .await
            .expect("a line in time")
            .unwrap();
        wire::decode(&line).unwrap()
    }

    #[tokio::test]
    async fn a_verdict_sent_again_after_a_reconnect_is_passed_on_once_and_acked_again() {
        let listener = TcpListener::bind(SocketAddr::from((Ipv4Addr::LOCALHOST, 0)))
            .await
            .unwrap();
        let addr = listener.local_addr().unwrap();
        let backoff = Backoff {
            initial: Duration::from_millis(10),
            max: Duration::from_millis(20),
        };
        let (_outbox, mut events) = spawn(config(addr, backoff));
        let verdict = |verdict_id| HubMsg::PermissionVerdict {
            request_id: "fdqmc".into(),
            behavior: wire::Behavior::Allow,
            verdict_id,
        };

        let (mut reader, mut write) = raw_hub(&listener).await;
        assert_eq!(next(&mut events).await, LinkEvent::Up { files: false });
        wire::write_msg(&mut write, &verdict(Some(7)))
            .await
            .unwrap();
        assert_eq!(
            next(&mut events).await,
            LinkEvent::Message(verdict(Some(7)))
        );
        assert_eq!(
            agent_line(&mut reader).await,
            AgentMsg::PermissionAck { verdict_id: 7 }
        );
        // The ack is lost with the link; the hub sends the same verdict again.
        drop((reader, write));
        assert_eq!(next(&mut events).await, LinkEvent::Down);
        let (mut reader, mut write) = raw_hub(&listener).await;
        assert_eq!(next(&mut events).await, LinkEvent::Up { files: false });
        wire::write_msg(&mut write, &verdict(Some(7)))
            .await
            .unwrap();
        assert_eq!(
            agent_line(&mut reader).await,
            AgentMsg::PermissionAck { verdict_id: 7 }
        );
        // A verdict without an id (a hub before TASK-014) gets no ack; a new
        // id is passed on.
        wire::write_msg(&mut write, &verdict(None)).await.unwrap();
        wire::write_msg(&mut write, &verdict(Some(8)))
            .await
            .unwrap();
        assert_eq!(next(&mut events).await, LinkEvent::Message(verdict(None)));
        assert_eq!(
            next(&mut events).await,
            LinkEvent::Message(verdict(Some(8)))
        );
        assert_eq!(
            agent_line(&mut reader).await,
            AgentMsg::PermissionAck { verdict_id: 8 }
        );
        assert!(events.try_recv().is_err(), "verdict 7 was passed on once");
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
            Dirs::default(),
            None,
            None,
        ));
        let (_first, _) = listener.accept().await.unwrap();
        drop(frames);
        let done = tokio::time::timeout(WAIT, serving)
            .await
            .expect("loop ends");
        assert!(done.unwrap().is_ok());
    }

    #[tokio::test]
    async fn a_transcript_read_is_answered_over_the_link_and_never_reaches_claude() {
        let dir = crate::hub::testdir::TempDir::new("agent-transcript-read");
        let session = "5e551017-0000-4000-8000-000000000001";
        let project = dir.path().join("projects").join("C--w");
        std::fs::create_dir_all(&project).unwrap();
        let path = project.join(format!("{session}.jsonl"));
        std::fs::write(
            &path,
            "{\"type\":\"user\",\"message\":{\"role\":\"user\",\"content\":\"hello\"}}\n",
        )
        .unwrap();
        let listener = TcpListener::bind(SocketAddr::from((Ipv4Addr::LOCALHOST, 0)))
            .await
            .unwrap();
        let addr = listener.local_addr().unwrap();
        let (outbox, events) = spawn(config(addr, Backoff::default()));
        let mut claude = claude_reading(Hub::Link(outbox), Some(events), Some(project.clone()));
        claude
            .send(r#"{"jsonrpc":"2.0","id":0,"method":"initialize","params":{"protocolVersion":"2025-11-25"}}"#)
            .await;
        assert_eq!(claude.recv().await["id"], 0);
        claude
            .send(r#"{"jsonrpc":"2.0","method":"notifications/initialized"}"#)
            .await;
        let (mut reader, mut write) = raw_hub(&listener).await;
        // The hub's `path` (still sent for older agents) names something
        // else entirely: it is never opened.
        let (bait, touched) = bait("transcript-read").await;
        wire::write_msg(
            &mut write,
            &HubMsg::TranscriptRead {
                session_id: session.into(),
                path: bait,
                from: Some(0),
            },
        )
        .await
        .unwrap();
        match agent_line(&mut reader).await {
            AgentMsg::TranscriptChunk {
                from: 0,
                to,
                lines,
                missing: false,
                ..
            } => {
                assert_eq!(to, std::fs::metadata(&path).unwrap().len());
                assert_eq!(
                    lines[0].items,
                    [wire::StreamItem::Prompt {
                        text: "hello".into()
                    }]
                );
            }
            other => panic!("{other:?}"),
        }
        assert!(!touched().await, "the hub's path was opened");
        // Claude Code saw nothing of it: the next line it gets is the answer
        // to its own request.
        claude
            .send(r#"{"jsonrpc":"2.0","id":5,"method":"ping"}"#)
            .await;
        assert_eq!(claude.recv().await["id"], 5);
    }

    /// A path a hub might name to make the agent open something: on Windows
    /// a named pipe whose server sees any client, elsewhere a missing file.
    /// The closure answers whether anything opened it.
    #[cfg(windows)]
    async fn bait(
        name: &str,
    ) -> (
        String,
        impl FnOnce() -> std::pin::Pin<Box<dyn std::future::Future<Output = bool>>>,
    ) {
        use tokio::net::windows::named_pipe::ServerOptions;
        let pipe = format!(r"\\.\pipe\cctg-bait-{name}-{}", std::process::id());
        let server = ServerOptions::new()
            .first_pipe_instance(true)
            .create(&pipe)
            .unwrap();
        let touched = move || -> std::pin::Pin<Box<dyn std::future::Future<Output = bool>>> {
            Box::pin(async move {
                tokio::time::timeout(Duration::from_millis(300), server.connect())
                    .await
                    .is_ok()
            })
        };
        (pipe, touched)
    }

    #[cfg(not(windows))]
    async fn bait(
        name: &str,
    ) -> (
        String,
        impl FnOnce() -> std::pin::Pin<Box<dyn std::future::Future<Output = bool>>>,
    ) {
        let path = std::env::temp_dir().join(format!("cctg-bait-{name}-{}", std::process::id()));
        let touched = || -> std::pin::Pin<Box<dyn std::future::Future<Output = bool>>> {
            Box::pin(async { false })
        };
        (path.to_string_lossy().into_owned(), touched)
    }

    #[tokio::test]
    async fn session_reads_are_answered_in_pieces_over_the_link_and_never_reach_claude() {
        let dir = crate::hub::testdir::TempDir::new("agent-session-read");
        let session = "5e551017-0000-4000-8000-000000000001";
        let project = dir.path().join("projects").join("C--w");
        std::fs::create_dir_all(&project).unwrap();
        let path = project.join(format!("{session}.jsonl"));
        // A prompt longer than one piece of an answer.
        let long = "я".repeat(reads::PIECE);
        std::fs::write(
            &path,
            format!("{{\"type\":\"user\",\"message\":{{\"role\":\"user\",\"content\":\"{long}\"}}}}\n{{\"type\":\"ai-title\",\"aiTitle\":\"T\"}}\n"),
        )
        .unwrap();
        let listener = TcpListener::bind(SocketAddr::from((Ipv4Addr::LOCALHOST, 0)))
            .await
            .unwrap();
        let addr = listener.local_addr().unwrap();
        let (outbox, events) = spawn(config(addr, Backoff::default()));
        let mut claude = claude_reading(Hub::Link(outbox), Some(events), Some(project.clone()));
        claude
            .send(r#"{"jsonrpc":"2.0","id":0,"method":"initialize","params":{"protocolVersion":"2025-11-25"}}"#)
            .await;
        assert_eq!(claude.recv().await["id"], 0);
        claude
            .send(r#"{"jsonrpc":"2.0","method":"notifications/initialized"}"#)
            .await;
        let (mut reader, mut write) = raw_hub(&listener).await;
        let read = HubMsg::SessionRead {
            read_id: 7,
            session_id: session.into(),
            ask: SessionAsk::Render {
                view: wire::TranscriptView::Brief,
                prompts: 1,
            },
        };
        wire::write_msg(&mut write, &read).await.unwrap();
        // A hub of the first TASK-034 build still names a path: ignored,
        // never opened.
        let (bait, touched) = bait("session-read").await;
        let earlier = serde_json::json!({
            "v": wire::VERSION, "type": "session_read", "read_id": 8,
            "session_id": session, "path": bait, "ask": { "kind": "title", "from": 0 },
        });
        write
            .write_all(format!("{earlier}\n").as_bytes())
            .await
            .unwrap();
        let mut text = String::new();
        loop {
            match agent_line(&mut reader).await {
                AgentMsg::SessionAnswer {
                    read_id: 7,
                    answer: wire::SessionAnswer::Text { text: piece, more },
                } => {
                    text.push_str(&piece);
                    if !more {
                        break;
                    }
                }
                other => panic!("{other:?}"),
            }
        }
        assert!(
            text.starts_with("> я") && text.len() > reads::PIECE,
            "{}",
            text.len()
        );
        match agent_line(&mut reader).await {
            AgentMsg::SessionAnswer {
                read_id: 8,
                answer: wire::SessionAnswer::Title { title, .. },
            } => assert_eq!(title.as_deref(), Some("T")),
            other => panic!("{other:?}"),
        }
        assert!(!touched().await, "the hub's path was opened");
        // Claude Code saw nothing of it: the next line it gets is the answer
        // to its own request.
        claude
            .send(r#"{"jsonrpc":"2.0","id":5,"method":"ping"}"#)
            .await;
        assert_eq!(claude.recv().await["id"], 5);
    }

    #[tokio::test]
    async fn console_keys_and_commands_are_answered_and_never_reach_claude() {
        let listener = TcpListener::bind(SocketAddr::from((Ipv4Addr::LOCALHOST, 0)))
            .await
            .unwrap();
        let addr = listener.local_addr().unwrap();
        let (outbox, events) = spawn(config(addr, Backoff::default()));
        let pressed = Arc::new(std::sync::Mutex::new(Vec::new()));
        let seen = pressed.clone();
        // The first write works, the second does not.
        let presser: Presser = Arc::new(move |key| {
            let mut seen = seen.lock().unwrap();
            seen.push(key);
            seen.len() == 1
        });
        let typed = Arc::new(std::sync::Mutex::new(Vec::new()));
        let seen = typed.clone();
        // The first line goes in, the second finds a draft.
        let typist: Typist = Arc::new(move |text: &str| {
            let mut seen = seen.lock().unwrap();
            seen.push(text.to_owned());
            if seen.len() == 1 {
                (Typed::Sent, Some("Total cost: $0.01".to_owned()))
            } else {
                (Typed::Draft, None)
            }
        });
        let console = Console {
            press: presser,
            type_line: typist,
        };
        let (frames, frames_rx) = mpsc::channel(16);
        let (ours, theirs) = tokio::io::duplex(1 << 16);
        tokio::spawn(serve_channel(
            frames_rx,
            ours,
            Hub::Link(outbox),
            Some(events),
            Dirs::default(),
            Some(console),
            None,
        ));
        let mut claude = Claude {
            frames,
            out: tokio::io::BufReader::new(theirs),
        };
        let (mut reader, mut write) = raw_hub(&listener).await;
        for key_id in [7, 8] {
            let key = ConsoleKey::Interrupt;
            wire::write_msg(&mut write, &HubMsg::ConsoleKey { key_id, key })
                .await
                .unwrap();
        }
        assert_eq!(
            agent_line(&mut reader).await,
            AgentMsg::ConsoleKeyWritten {
                key_id: 7,
                written: true
            }
        );
        assert_eq!(
            agent_line(&mut reader).await,
            AgentMsg::ConsoleKeyWritten {
                key_id: 8,
                written: false
            }
        );
        assert_eq!(
            *pressed.lock().unwrap(),
            [ConsoleKey::Interrupt, ConsoleKey::Interrupt]
        );
        for (command_id, text) in [(9, "!echo hi"), (10, "/compact"), (11, "!echo a\nb")] {
            let command = HubMsg::ConsoleCommand {
                command_id,
                text: text.into(),
            };
            wire::write_msg(&mut write, &command).await.unwrap();
        }
        for (command_id, outcome, panel) in [
            (
                9,
                CommandOutcome::Sent,
                Some("Total cost: $0.01".to_owned()),
            ),
            (10, CommandOutcome::Draft, None),
            // Two lines are never typed.
            (11, CommandOutcome::Failed, None),
        ] {
            assert_eq!(
                agent_line(&mut reader).await,
                AgentMsg::ConsoleCommandTyped {
                    command_id,
                    outcome,
                    panel,
                }
            );
        }
        assert_eq!(*typed.lock().unwrap(), ["!echo hi", "/compact"]);
        // Claude Code saw nothing of it.
        claude
            .send(r#"{"jsonrpc":"2.0","id":5,"method":"ping"}"#)
            .await;
        assert_eq!(claude.recv().await["id"], 5);
    }

    #[tokio::test]
    async fn without_a_presser_a_console_key_is_answered_as_failed() {
        let listener = TcpListener::bind(SocketAddr::from((Ipv4Addr::LOCALHOST, 0)))
            .await
            .unwrap();
        let addr = listener.local_addr().unwrap();
        let (outbox, events) = spawn(config(addr, Backoff::default()));
        let _claude = claude(Hub::Link(outbox), Some(events));
        let (mut reader, mut write) = raw_hub(&listener).await;
        wire::write_msg(
            &mut write,
            &HubMsg::ConsoleKey {
                key_id: 1,
                key: ConsoleKey::Interrupt,
            },
        )
        .await
        .unwrap();
        assert_eq!(
            agent_line(&mut reader).await,
            AgentMsg::ConsoleKeyWritten {
                key_id: 1,
                written: false
            }
        );
        let command = HubMsg::ConsoleCommand {
            command_id: 2,
            text: "!echo hi".into(),
        };
        wire::write_msg(&mut write, &command).await.unwrap();
        assert_eq!(
            agent_line(&mut reader).await,
            AgentMsg::ConsoleCommandTyped {
                command_id: 2,
                outcome: CommandOutcome::Failed,
                panel: None,
            }
        );
    }

    /// A raw hub like [`raw_hub`] that says whether it takes files.
    async fn raw_hub_files(
        listener: &TcpListener,
        files: bool,
    ) -> (BufReader<OwnedReadHalf>, OwnedWriteHalf) {
        let (stream, _) = tokio::time::timeout(WAIT, listener.accept())
            .await
            .expect("agent connects")
            .unwrap();
        let (read, mut write) = stream.into_split();
        let mut reader = BufReader::new(read);
        let mut line = Vec::new();
        for _ in 0..2 {
            wire::read_line(&mut reader, &mut line).await.unwrap();
            line.clear();
        }
        wire::write_msg(
            &mut write,
            &HubMsg::Registered {
                files,
                heartbeat: false,
            },
        )
        .await
        .unwrap();
        (reader, write)
    }

    /// Claude Code with the channel initialized, over a link to `addr`,
    /// keeping files in `work`.
    async fn claude_in(addr: SocketAddr, work: &Path) -> Claude {
        let backoff = Backoff {
            initial: Duration::from_millis(10),
            max: Duration::from_millis(20),
        };
        let (outbox, events) = spawn(config(addr, backoff));
        let (frames, frames_rx) = mpsc::channel(16);
        let (ours, theirs) = tokio::io::duplex(1 << 20);
        let dirs = Dirs {
            project: None,
            work: Some(work.to_owned()),
        };
        tokio::spawn(serve_channel(
            frames_rx,
            ours,
            Hub::Link(outbox),
            Some(events),
            dirs,
            None,
            None,
        ));
        let mut claude = Claude {
            frames,
            out: tokio::io::BufReader::new(theirs),
        };
        claude
            .send(r#"{"jsonrpc":"2.0","id":0,"method":"initialize","params":{"protocolVersion":"2025-11-25"}}"#)
            .await;
        assert_eq!(claude.recv().await["id"], 0);
        claude
            .send(r#"{"jsonrpc":"2.0","method":"notifications/initialized"}"#)
            .await;
        claude
    }

    /// Sends an inbound and waits until Claude has it: the link events
    /// before it (the registration) are handled then.
    async fn settle(claude: &mut Claude, write: &mut OwnedWriteHalf) {
        let inbound = HubMsg::Inbound {
            content: "settled".into(),
            meta: Default::default(),
        };
        wire::write_msg(write, &inbound).await.unwrap();
        assert_eq!(claude.recv().await["params"]["content"], "settled");
    }

    fn file_start(transfer_id: u64, name: &str, size: usize) -> HubMsg {
        HubMsg::FileStart {
            transfer_id,
            name: name.into(),
            size: size as u64,
            kind: FileKind::Photo,
            content: "> quoted\n\nlook".into(),
            meta: [("message_id".to_owned(), "9".to_owned())].into(),
        }
    }

    #[tokio::test]
    async fn a_file_from_the_topic_is_saved_and_handed_to_claude_with_its_path() {
        let dir = crate::hub::testdir::TempDir::new("agent-inbox");
        let listener = TcpListener::bind(SocketAddr::from((Ipv4Addr::LOCALHOST, 0)))
            .await
            .unwrap();
        let mut claude = claude_in(listener.local_addr().unwrap(), dir.path()).await;
        let (_reader, mut write) = raw_hub(&listener).await;
        let bytes: Vec<u8> = (0..files::CHUNK + 100).map(|n| (n % 251) as u8).collect();
        let pieces: Vec<FileChunk> = files::chunks(2, &bytes).collect();
        // A transfer with a piece out of order gives Claude nothing.
        wire::write_msg(&mut write, &file_start(1, "x.png", bytes.len()))
            .await
            .unwrap();
        let wrong = FileChunk {
            transfer_id: 1,
            ..pieces[1].clone()
        };
        wire::write_msg(&mut write, &HubMsg::FileChunk(wrong))
            .await
            .unwrap();
        // A whole one, with a name that tries to leave the inbox.
        wire::write_msg(
            &mut write,
            &file_start(2, "../../evil name.png", bytes.len()),
        )
        .await
        .unwrap();
        for piece in &pieces {
            wire::write_msg(&mut write, &HubMsg::FileChunk(piece.clone()))
                .await
                .unwrap();
        }
        let note = claude.recv().await;
        assert_eq!(note["method"], "notifications/claude/channel");
        let meta = &note["params"]["meta"];
        assert_eq!(meta["message_id"], "9");
        assert_eq!(meta["file_kind"], "photo");
        assert_eq!(meta["file_size"], bytes.len().to_string());
        let path = PathBuf::from(meta["file_path"].as_str().unwrap());
        let inbox = dir.path().join(".cctg").join("inbox");
        assert_eq!(
            path,
            inbox.join(format!("{}-evil name.png", files::date(SystemTime::now())))
        );
        assert_eq!(std::fs::read(&path).unwrap(), bytes);
        let content = note["params"]["content"].as_str().unwrap();
        assert_eq!(
            content,
            format!(
                "A photo from the Telegram topic is saved at {} ({} bytes).\n\n> quoted\n\nlook",
                path.display(),
                bytes.len()
            )
        );
        // A link lost in the middle of a file drops it: its rest on the
        // next link is nobody's.
        wire::write_msg(&mut write, &file_start(3, "y.png", bytes.len()))
            .await
            .unwrap();
        let first = FileChunk {
            transfer_id: 3,
            ..pieces[0].clone()
        };
        wire::write_msg(&mut write, &HubMsg::FileChunk(first))
            .await
            .unwrap();
        drop((_reader, write));
        let (_reader, mut write) = raw_hub(&listener).await;
        let rest = FileChunk {
            transfer_id: 3,
            ..pieces[1].clone()
        };
        wire::write_msg(&mut write, &HubMsg::FileChunk(rest))
            .await
            .unwrap();
        settle(&mut claude, &mut write).await;
        // The one saved file and the inbox's own `.gitignore`.
        let kept: Vec<_> = std::fs::read_dir(&inbox).unwrap().flatten().collect();
        assert_eq!(kept.len(), 2, "{kept:?}");
    }

    #[tokio::test]
    async fn a_file_saved_off_the_loop_still_reaches_claude_before_the_messages_after_it() {
        let dir = crate::hub::testdir::TempDir::new("agent-inbox-order");
        let listener = TcpListener::bind(SocketAddr::from((Ipv4Addr::LOCALHOST, 0)))
            .await
            .unwrap();
        let mut claude = claude_in(listener.local_addr().unwrap(), dir.path()).await;
        let (_reader, mut write) = raw_hub(&listener).await;
        let bytes = vec![3u8; 3 * files::CHUNK + 1];
        wire::write_msg(&mut write, &file_start(1, "a.png", bytes.len()))
            .await
            .unwrap();
        for piece in files::chunks(1, &bytes) {
            wire::write_msg(&mut write, &HubMsg::FileChunk(piece))
                .await
                .unwrap();
        }
        // An empty file is complete at its start.
        wire::write_msg(&mut write, &file_start(2, "b.png", 0))
            .await
            .unwrap();
        let after = HubMsg::Inbound {
            content: "after".into(),
            meta: Default::default(),
        };
        wire::write_msg(&mut write, &after).await.unwrap();
        let mut got = Vec::new();
        for _ in 0..3 {
            let note = claude.recv().await;
            let params = &note["params"];
            got.push(
                params["meta"]["file_path"]
                    .as_str()
                    .map(|path| {
                        Path::new(path)
                            .file_name()
                            .unwrap()
                            .to_string_lossy()
                            .into_owned()
                    })
                    .unwrap_or_else(|| params["content"].as_str().unwrap().to_owned()),
            );
        }
        let date = files::date(SystemTime::now());
        assert_eq!(
            got,
            [
                format!("{date}-a.png"),
                format!("{date}-b.png"),
                "after".to_owned()
            ]
        );
    }

    #[test]
    fn claude_reads_where_a_file_is_or_that_it_could_not_be_kept() {
        let path = Path::new("/w/.cctg/inbox/2026-09-25-voice.oga");
        assert_eq!(
            file_content(FileKind::Voice, Some(path), 7, ""),
            format!(
                "A voice from the Telegram topic is saved at {} (7 bytes).",
                path.display()
            )
        );
        assert_eq!(
            file_content(FileKind::Audio, None, 9, "words"),
            "An audio (9 bytes) came from the Telegram topic but could not be saved on this machine.\n\nwords"
        );
    }

    #[tokio::test]
    async fn send_file_offers_the_file_and_answers_what_the_hub_did() {
        let dir = crate::hub::testdir::TempDir::new("agent-send-file");
        let png = [b"\x89PNG\r\n\x1a\n".to_vec(), vec![3; files::CHUNK]].concat();
        std::fs::write(dir.path().join("shot.png"), &png).unwrap();
        std::fs::create_dir(dir.path().join("sub")).unwrap();
        let listener = TcpListener::bind(SocketAddr::from((Ipv4Addr::LOCALHOST, 0)))
            .await
            .unwrap();
        let mut claude = claude_in(listener.local_addr().unwrap(), dir.path()).await;
        let (mut reader, mut write) = raw_hub_files(&listener, true).await;
        settle(&mut claude, &mut write).await;
        let call = |id: u32, path: &str| {
            serde_json::json!({"jsonrpc":"2.0","id":id,"method":"tools/call",
                "params":{"name":"send_file","arguments":{"path":path,"caption":"look"}}})
            .to_string()
        };
        let text = |answer: &serde_json::Value| {
            answer["result"]["content"][0]["text"]
                .as_str()
                .unwrap()
                .to_owned()
        };
        // A relative path starts in the session's folder.
        claude.send(&call(10, "shot.png")).await;
        let AgentMsg::FileOffer {
            transfer_id,
            name,
            size,
            caption,
        } = agent_line(&mut reader).await
        else {
            panic!("an offer first");
        };
        assert_eq!(
            (name.as_str(), size, caption.as_deref()),
            ("shot.png", png.len() as u64, Some("look"))
        );
        let accepted = HubMsg::FileAnswer {
            transfer_id,
            outcome: FileOutcome::Accepted,
        };
        wire::write_msg(&mut write, &accepted).await.unwrap();
        let mut assembly = files::Assembly::new(size);
        while !assembly.is_complete() {
            match agent_line(&mut reader).await {
                AgentMsg::FileChunk(chunk) => {
                    assembly.push(&chunk).unwrap();
                }
                other => panic!("{other:?}"),
            }
        }
        assert_eq!(assembly.into_bytes(), png);
        let sent = HubMsg::FileAnswer {
            transfer_id,
            outcome: FileOutcome::Sent,
        };
        wire::write_msg(&mut write, &sent).await.unwrap();
        let answer = claude.recv().await;
        assert_eq!(
            (answer["id"].clone(), answer["result"]["isError"].clone()),
            (10.into(), false.into())
        );
        assert_eq!(text(&answer), "Sent to the Telegram topic of this session.");
        // The hub's refusal is a tool error that says why.
        let absolute = dir.path().join("shot.png");
        claude.send(&call(11, &absolute.to_string_lossy())).await;
        let AgentMsg::FileOffer { transfer_id, .. } = agent_line(&mut reader).await else {
            panic!("an offer");
        };
        let refused = HubMsg::FileAnswer {
            transfer_id,
            outcome: FileOutcome::NoTopic,
        };
        wire::write_msg(&mut write, &refused).await.unwrap();
        let answer = claude.recv().await;
        assert_eq!(answer["result"]["isError"], true);
        assert!(text(&answer).contains("not the live session"), "{answer}");
        // Not a file: answered without an offer.
        claude.send(&call(12, "sub")).await;
        let answer = claude.recv().await;
        assert_eq!(
            (answer["id"].clone(), answer["result"]["isError"].clone()),
            (12.into(), true.into())
        );
        assert!(text(&answer).contains("not a regular file"), "{answer}");
        // The link drops during a transfer: the tool says so.
        claude.send(&call(13, "shot.png")).await;
        let AgentMsg::FileOffer { transfer_id, .. } = agent_line(&mut reader).await else {
            panic!("an offer");
        };
        let accepted = HubMsg::FileAnswer {
            transfer_id,
            outcome: FileOutcome::Accepted,
        };
        wire::write_msg(&mut write, &accepted).await.unwrap();
        drop((reader, write));
        let answer = claude.recv().await;
        assert_eq!(
            (answer["id"].clone(), answer["result"]["isError"].clone()),
            (13.into(), true.into())
        );
        assert!(text(&answer).contains("dropped"), "{answer}");
        // A hub that takes no files is not offered one.
        let (mut reader, mut write) = raw_hub_files(&listener, false).await;
        settle(&mut claude, &mut write).await;
        claude.send(&call(14, "shot.png")).await;
        let answer = claude.recv().await;
        assert_eq!(
            (answer["id"].clone(), answer["result"]["isError"].clone()),
            (14.into(), true.into())
        );
        assert!(text(&answer).contains("takes no files"), "{answer}");
        let offered = async {
            loop {
                if let AgentMsg::FileOffer { .. } = agent_line(&mut reader).await {
                    return;
                }
            }
        };
        assert!(
            tokio::time::timeout(Duration::from_millis(300), offered)
                .await
                .is_err(),
            "no offer to a hub without files"
        );
    }

    #[tokio::test]
    async fn a_link_lost_while_chunks_wait_for_room_ends_the_upload_at_once() {
        let dir = crate::hub::testdir::TempDir::new("agent-upload-lost");
        let file = dir.path().join("big.bin");
        std::fs::write(&file, vec![5u8; files::CHUNK * 8]).unwrap();
        let call = FileCall {
            id: serde_json::json!(1),
            path: file.to_string_lossy().into_owned(),
            caption: None,
        };
        // The link queue: the link task stops draining it while it
        // reconnects, so chunks pile up to the sender's share.
        let (outbox, mut link) = mpsc::channel(QUEUE);
        let (events, mut uploads) = mpsc::unbounded_channel();
        let sending = tokio::spawn(async move { upload(&call, &outbox, &mut uploads, None).await });
        let Some(AgentMsg::FileOffer { transfer_id, .. }) = link.recv().await else {
            panic!("an offer first");
        };
        events
            .send(Upload::Answer {
                transfer_id,
                outcome: FileOutcome::Accepted,
            })
            .unwrap();
        tokio::time::sleep(Duration::from_millis(200)).await;
        events.send(Upload::Lost).unwrap();
        let (text, is_error) = tokio::time::timeout(Duration::from_secs(3), sending)
            .await
            .expect("the tool answers without waiting for the link to come back")
            .unwrap();
        assert_eq!((text.as_str(), is_error), (LOST, true));
        drop(link);
    }
}
