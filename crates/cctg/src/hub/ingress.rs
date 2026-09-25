//! Hub ingress: the agent TCP link and the hook HTTP endpoint.
//!
//! Both check the shared secret before anything else and hand what they
//! accept to the hub over bounded channels ([`AgentEvent`], [`HookPost`]).
//! Either can take its connections over TLS ([`Listener::tls`], TASK-035):
//! the handshake runs in the connection's own task, within its time limit,
//! so a slow client never holds up accepting others. A connection that
//! closes before its first byte (a health probe, a port scan) or fails its
//! TLS handshake is logged at debug level only.
//! Nothing here logs message contents, paths or the secret: log lines carry
//! the connection number, the peer address and fixed text only.

use std::collections::{HashMap, HashSet, VecDeque};
use std::net::{IpAddr, SocketAddr};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt, BufReader, ReadHalf};
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::{OwnedSemaphorePermit, Semaphore, mpsc, oneshot};
use tokio::task::JoinSet;
use tracing::{debug, info, warn};

use crate::tls::{Acceptor, Incoming, ReadTask, Stream};
use crate::wire::{
    self, AgentMsg, Behavior, EventId, HOOK_PATH, HookPost, HubMsg, MAX_HOOK_BODY, PERMISSION_PATH,
    PING_PATH, PermissionAnswer, PermissionPost, Register, Rejection, Secret, WireError,
};

/// Time an agent has from its TCP connect to finish the TLS handshake (when
/// there is one), `hello` and `register`: one deadline for all of it.
const HANDSHAKE_TIMEOUT: Duration = Duration::from_secs(5);
/// Time a hook has from its TCP connect to deliver its whole request, TLS
/// handshake included.
const REQUEST_TIMEOUT: Duration = Duration::from_secs(2);
const WRITE_TIMEOUT: Duration = Duration::from_secs(5);
const MAX_AGENTS: usize = 256;
/// Agent connections that have not authenticated yet; one more is closed at
/// once (the listeners may face the internet, TASK-035).
const MAX_PENDING_AGENTS: usize = 16;
/// Of those, from one peer address: one source cannot take them all.
const MAX_PENDING_AGENTS_PER_PEER: usize = 4;
/// Longest `hello` line: read before the secret is checked. A secret longer
/// than about 4000 characters cannot authenticate.
const MAX_HELLO_LINE: usize = 4 * 1024;
/// A wrong secret is answered only after this pause (both listeners).
const AUTH_FAIL_DELAY: Duration = Duration::from_millis(250);
const MAX_HOOK_REQUESTS: usize = 64;
/// Hook requests of one peer address still before their secret (the
/// request is read whole by then: a few hundred milliseconds at most).
const MAX_PENDING_HOOKS_PER_PEER: usize = 16;
/// What arrives before the secret (scanners, a wrong version, a wrong
/// secret) warns at most once per peer address in this time; the rest of
/// it is logged at debug level.
const NOISE_WARN_EVERY: Duration = Duration::from_secs(60);
/// Peer addresses remembered for that; beyond them, debug only.
const NOISE_PEERS: usize = 1024;
/// `PermissionRequest` hooks waiting for an answer at a time; one more gets
/// no decision at once. They do not count against [`MAX_HOOK_REQUESTS`].
pub const MAX_PERMISSION_WAITS: usize = 16;
/// Longest wait for the hub's answer to a `PermissionRequest` hook. The slot
/// actor gives up earlier (`slots::HOOK_ANSWER_WAIT`); this only bounds a
/// request the actor never answers.
pub const PERMISSION_WAIT_CAP: Duration = Duration::from_secs(95);
const MAX_HEAD: usize = 8 * 1024;
const TO_AGENT_QUEUE: usize = 64;
/// How long, and how much, a rejected peer's unread input is drained.
const LINGER: Duration = Duration::from_millis(250);
const LINGER_BYTES: usize = MAX_HEAD + MAX_HOOK_BODY;
/// A re-sent POST arrives within the hook's own lifetime (seconds, at most
/// its timeout), so ten minutes is ample; the size cap bounds memory.
pub const DEDUP_TTL: Duration = Duration::from_secs(10 * 60);
pub const DEDUP_MAX: usize = 4096;

/// A warning for the first pre-authentication failure of a peer address in
/// [`NOISE_WARN_EVERY`], debug for the rest (TASK-035: listeners may face
/// the internet, and scanners must not drown real warnings).
macro_rules! pre_auth {
    ($gate:expr, $peer:expr, $($arg:tt)+) => {
        if $gate.warns($peer.ip()) {
            warn!($($arg)+)
        } else {
            debug!($($arg)+)
        }
    };
}

/// When each peer address last got a pre-authentication warning.
#[derive(Clone, Default)]
struct WarnGate(Arc<Mutex<HashMap<IpAddr, Instant>>>);

impl WarnGate {
    fn warns(&self, ip: IpAddr) -> bool {
        let Ok(mut last) = self.0.lock() else {
            return false;
        };
        let now = Instant::now();
        let quiet = |at: &Instant| now.duration_since(*at) < NOISE_WARN_EVERY;
        if last.len() >= NOISE_PEERS {
            last.retain(|_, at| quiet(at));
        }
        match last.get(&ip.to_canonical()) {
            Some(at) if quiet(at) => false,
            _ if last.len() >= NOISE_PEERS => false,
            _ => {
                last.insert(ip.to_canonical(), now);
                true
            }
        }
    }
}

/// Places for connections before their secret, counted per peer address.
#[derive(Clone)]
struct PerPeer {
    limit: usize,
    taken: Arc<Mutex<HashMap<IpAddr, usize>>>,
}

impl PerPeer {
    fn new(limit: usize) -> Self {
        Self {
            limit,
            taken: Arc::default(),
        }
    }

    /// A place for `ip`; `None` when it holds `limit` already.
    fn take(&self, ip: IpAddr) -> Option<PeerPlace> {
        let ip = ip.to_canonical();
        let mut taken = self.taken.lock().ok()?;
        let count = taken.entry(ip).or_insert(0);
        if *count >= self.limit {
            return None;
        }
        *count += 1;
        Some(PeerPlace {
            ip,
            taken: self.taken.clone(),
        })
    }
}

/// Given back when dropped.
struct PeerPlace {
    ip: IpAddr,
    taken: Arc<Mutex<HashMap<IpAddr, usize>>>,
}

impl Drop for PeerPlace {
    fn drop(&mut self) {
        let Ok(mut taken) = self.taken.lock() else {
            return;
        };
        if let Some(count) = taken.get_mut(&self.ip) {
            *count -= 1;
            if *count == 0 {
                taken.remove(&self.ip);
            }
        }
    }
}

/// Binds `addr`.
pub async fn bind(addr: SocketAddr) -> std::io::Result<TcpListener> {
    TcpListener::bind(addr).await
}

/// A bound listener and how it takes connections: plain TCP (a
/// `TcpListener` converts into that) or TLS.
#[derive(Debug)]
pub struct Listener {
    tcp: TcpListener,
    incoming: Incoming,
}

impl From<TcpListener> for Listener {
    fn from(tcp: TcpListener) -> Self {
        Self {
            tcp,
            incoming: Incoming::plain(),
        }
    }
}

impl Listener {
    pub fn tls(tcp: TcpListener, acceptor: Acceptor) -> Self {
        Self {
            tcp,
            incoming: Incoming::tls(acceptor),
        }
    }

    /// With `acceptor`: TLS; without: plain. A non-loopback plain listener
    /// was configured explicitly; say so.
    pub fn new(tcp: TcpListener, acceptor: Option<Acceptor>) -> Self {
        let local = tcp.local_addr().ok();
        match acceptor {
            Some(acceptor) => {
                if let Some(local) = local {
                    info!(%local, "listening with TLS");
                }
                Self::tls(tcp, acceptor)
            }
            None => {
                if let Some(local) = local.filter(|local| !local.ip().is_loopback()) {
                    warn!(%local, "listening beyond loopback: plain TCP, keep it on a private network");
                }
                Self::from(tcp)
            }
        }
    }
}

/// The TLS handshake of an accepted connection (nothing for plain), before
/// `deadline`. `None`: it failed; logged at debug level only (port scans).
async fn open(
    incoming: &Incoming,
    tcp: TcpStream,
    peer: SocketAddr,
    deadline: tokio::time::Instant,
) -> Option<Stream> {
    match tokio::time::timeout_at(deadline, incoming.accept(tcp)).await {
        Ok(Ok(stream)) => Some(stream),
        Ok(Err(error)) => {
            debug!(%peer, kind = ?error.kind(), "TLS handshake failed");
            None
        }
        Err(_) => {
            debug!(%peer, "TLS handshake timed out");
            None
        }
    }
}

/// What the agent link hands to the hub. `conn` numbers are unique per hub run.
#[derive(Debug)]
pub enum AgentEvent {
    Registered {
        conn: u64,
        register: Register,
        /// Dropping it stops outbound traffic, not the connection.
        to_agent: mpsc::Sender<HubMsg>,
    },
    /// A `reply`, `permission_request` or `permission_ack` after registration.
    Message {
        conn: u64,
        /// Captured as soon as the complete frame was read, before delivery
        /// can race with a `/clear` hook on the slots actor's other channel.
        received_at: Instant,
        msg: AgentMsg,
    },
    Disconnected {
        conn: u64,
    },
}

/// Accepts agents until the task is dropped; dropping it also closes every
/// agent connection, which is what a hub restart looks like to an agent.
pub async fn serve_agents(
    listener: impl Into<Listener>,
    secret: Secret,
    events: mpsc::Sender<AgentEvent>,
) {
    let Listener {
        tcp: listener,
        incoming,
    } = listener.into();
    let secret = Arc::new(secret);
    let slots = Arc::new(Semaphore::new(MAX_AGENTS));
    let pending = Arc::new(Semaphore::new(MAX_PENDING_AGENTS));
    let per_peer = PerPeer::new(MAX_PENDING_AGENTS_PER_PEER);
    let gate = WarnGate::default();
    let next_conn = AtomicU64::new(1);
    let mut sessions = JoinSet::new();
    loop {
        tokio::select! {
            accepted = listener.accept() => {
                let (stream, peer) = match accepted {
                    Ok(accepted) => accepted,
                    Err(error) => {
                        warn!(kind = ?error.kind(), "agent accept failed");
                        tokio::time::sleep(Duration::from_millis(100)).await;
                        continue;
                    }
                };
                // Closed at once: not yet authenticated connections are few,
                // and fewer from one address.
                let Some(peer_place) = per_peer.take(peer.ip()) else {
                    debug!(%peer, "too many unauthenticated agent connections from one address; closed");
                    continue;
                };
                let Ok(unauthenticated) = pending.clone().try_acquire_owned() else {
                    debug!(%peer, "too many unauthenticated agent connections; closed");
                    continue;
                };
                let Ok(permit) = slots.clone().try_acquire_owned() else {
                    pre_auth!(gate, peer, %peer, "too many agent connections; refused");
                    continue;
                };
                let conn = next_conn.fetch_add(1, Ordering::Relaxed);
                let (secret, events, incoming, gate) =
                    (secret.clone(), events.clone(), incoming.clone(), gate.clone());
                let deadline = tokio::time::Instant::now() + HANDSHAKE_TIMEOUT;
                sessions.spawn(async move {
                    if let Some(stream) = open(&incoming, stream, peer, deadline).await {
                        let handshake = Handshake {
                            deadline,
                            unauthenticated,
                            peer_place,
                            gate,
                        };
                        agent_session(stream, peer, conn, &secret, &events, handshake).await;
                    }
                    drop(permit);
                });
            }
            Some(_) = sessions.join_next() => {}
        }
    }
}

async fn write_hub_msg<W: AsyncWrite + Unpin>(
    write: &mut W,
    msg: &HubMsg,
) -> Result<(), WireError> {
    tokio::time::timeout(WRITE_TIMEOUT, wire::write_msg(write, msg))
        .await
        .unwrap_or(Err(WireError::Io(std::io::ErrorKind::TimedOut)))
}

async fn reject(write: &mut (impl AsyncWrite + Unpin), reason: Rejection) {
    let _ = write_hub_msg(write, &HubMsg::Rejected { reason }).await;
    let _ = write.shutdown().await;
}

/// Closing a socket with unread input sends a reset, which can destroy the
/// answer just written. Discard a bounded amount of input first.
async fn linger(read: &mut (impl AsyncRead + Unpin)) {
    let drain = async {
        let mut chunk = [0u8; 4096];
        let mut total = 0;
        while total < LINGER_BYTES {
            match read.read(&mut chunk).await {
                Ok(0) | Err(_) => break,
                Ok(n) => total += n,
            }
        }
    };
    let _ = tokio::time::timeout(LINGER, drain).await;
}

/// The pre-authentication part of an agent connection: its deadline and its
/// places among the [`MAX_PENDING_AGENTS`] and those of its peer address,
/// given back when it ends.
struct Handshake {
    deadline: tokio::time::Instant,
    unauthenticated: OwnedSemaphorePermit,
    peer_place: PeerPlace,
    gate: WarnGate,
}

async fn agent_session(
    stream: Stream,
    peer: SocketAddr,
    conn: u64,
    secret: &Secret,
    events: &mpsc::Sender<AgentEvent>,
    pre_auth: Handshake,
) {
    let (read, mut write) = tokio::io::split(stream);
    let mut reader = BufReader::new(read);
    let mut line = Vec::new();

    let handshake = tokio::time::timeout_at(pre_auth.deadline, async {
        // Read before the secret is checked: a short line only.
        match wire::read_line_max(&mut reader, &mut line, MAX_HELLO_LINE).await {
            Ok(()) => {}
            // Closed before its first byte: a probe, not an agent.
            Err(WireError::Closed) if line.is_empty() => return Ok(None),
            Err(_) => return Err(Rejection::Protocol),
        }
        let authed = match wire::decode::<AgentMsg>(&line) {
            Ok(AgentMsg::Hello { secret: offered }) => secret.matches(offered.expose().as_bytes()),
            Err(WireError::Version) => return Err(Rejection::Version),
            _ => false,
        };
        if !authed {
            return Err(Rejection::Auth);
        }
        line.clear();
        wire::read_line(&mut reader, &mut line)
            .await
            .map_err(|_| Rejection::Protocol)?;
        let result = match wire::decode::<AgentMsg>(&line) {
            Ok(AgentMsg::Register(register)) => Ok(Some(register)),
            Err(WireError::Version) => Err(Rejection::Version),
            _ => Err(Rejection::Protocol),
        };
        line.clear();
        result
    })
    .await;
    let register = match handshake {
        Ok(Ok(Some(register))) => register,
        Ok(Ok(None)) => {
            debug!(conn, %peer, "connection closed before its first byte");
            return;
        }
        Ok(Err(reason)) => {
            pre_auth!(pre_auth.gate, peer, conn, %peer, ?reason, "agent rejected");
            if reason == Rejection::Auth {
                // No fast guessing; the place stays taken meanwhile.
                tokio::time::sleep(AUTH_FAIL_DELAY).await;
            }
            reject(&mut write, reason).await;
            linger(&mut reader).await;
            return;
        }
        Err(_) => {
            pre_auth!(pre_auth.gate, peer, conn, %peer, "agent handshake timed out");
            return;
        }
    };
    drop(pre_auth.unauthenticated);
    drop(pre_auth.peer_place);

    let session = short(&register.session_id).to_owned();
    let (to_agent, mut outbound) = mpsc::channel(TO_AGENT_QUEUE);
    let registered = AgentEvent::Registered {
        conn,
        register,
        to_agent,
    };
    // This hub takes files from agents (TASK-032).
    if events.send(registered).await.is_err()
        || write_hub_msg(&mut write, &HubMsg::Registered { files: true })
            .await
            .is_err()
    {
        return;
    }
    info!(conn, session, "agent registered");

    let (frames_tx, mut frames) = mpsc::channel(TO_AGENT_QUEUE);
    let reader_task = ReadTask::spawn(read_agent_frames(reader, frames_tx));
    let mut outbound_open = true;
    loop {
        tokio::select! {
            frame = frames.recv() => {
                match frame {
                    Some((received_at, Ok(
                        msg @ (AgentMsg::Reply { .. }
                        | AgentMsg::PermissionRequest(_)
                        | AgentMsg::PermissionAck { .. }
                        | AgentMsg::TranscriptChunk { .. }
                        | AgentMsg::ConsoleKeyWritten { .. }
                        | AgentMsg::ConsoleCommandTyped { .. }
                        | AgentMsg::UpdateAnswer { .. }
                        | AgentMsg::FileOffer { .. }
                        | AgentMsg::FileChunk(_)
                        | AgentMsg::SessionAnswer { .. }),
                    ))) => {
                        if events.send(AgentEvent::Message { conn, received_at, msg }).await.is_err() {
                            break;
                        }
                    }
                    Some((_, Ok(_))) => warn!(conn, "agent repeated its handshake; ignored"),
                    Some((_, Err(WireError::Version))) => {
                        warn!(conn, "agent changed protocol version; closing");
                        reject(&mut write, Rejection::Version).await;
                        break;
                    }
                    Some((_, Err(error @ (WireError::Closed | WireError::TooLong | WireError::Io(_))))) => {
                        debug!(conn, %error, "agent link ended");
                        break;
                    }
                    Some((_, Err(error))) => warn!(conn, %error, "agent line ignored"),
                    None => break,
                }
            }
            msg = outbound.recv(), if outbound_open => match msg {
                Some(msg) => {
                    if write_hub_msg(&mut write, &msg).await.is_err() {
                        break;
                    }
                }
                None => outbound_open = false,
            },
        }
    }
    reader_task.stop().await;
    let _ = events.send(AgentEvent::Disconnected { conn }).await;
    info!(conn, session, "agent disconnected");
}

async fn read_agent_frames(
    mut reader: BufReader<ReadHalf<Stream>>,
    frames: mpsc::Sender<(Instant, Result<AgentMsg, WireError>)>,
) {
    let mut line = Vec::new();
    loop {
        let frame = match wire::read_line(&mut reader, &mut line).await {
            Ok(()) => (Instant::now(), wire::decode::<AgentMsg>(&line)),
            Err(error) => {
                let _ = frames.send((Instant::now(), Err(error))).await;
                return;
            }
        };
        line.clear();
        if frames.send(frame).await.is_err() {
            return;
        }
    }
}

/// First 8 characters: enough to recognise a session in logs.
fn short(session_id: &str) -> &str {
    session_id
        .char_indices()
        .nth(8)
        .map_or(session_id, |(end, _)| &session_id[..end])
}

/// Recently accepted hook event ids, bounded by count and age.
#[derive(Debug)]
pub struct Dedup {
    ttl: Duration,
    max: usize,
    seen: HashSet<EventId>,
    order: VecDeque<(Instant, EventId)>,
}

impl Dedup {
    pub fn new(max: usize, ttl: Duration) -> Self {
        Self {
            ttl,
            max,
            seen: HashSet::new(),
            order: VecDeque::new(),
        }
    }

    pub fn contains(&mut self, id: &EventId, now: Instant) -> bool {
        self.expire(now);
        self.seen.contains(id)
    }

    pub fn insert(&mut self, id: EventId, now: Instant) {
        if self.seen.insert(id.clone()) {
            self.order.push_back((now, id));
        }
        self.expire(now);
    }

    pub fn len(&self) -> usize {
        self.order.len()
    }

    pub fn is_empty(&self) -> bool {
        self.order.is_empty()
    }

    fn expire(&mut self, now: Instant) {
        while let Some((at, _)) = self.order.front() {
            let stale = now.saturating_duration_since(*at) >= self.ttl;
            if !stale && self.order.len() <= self.max {
                break;
            }
            if let Some((_, id)) = self.order.pop_front() {
                self.seen.remove(&id);
            }
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Status {
    Ok,
    NoContent,
    BadRequest,
    Unauthorized,
    NotFound,
    MethodNotAllowed,
    LengthRequired,
    PayloadTooLarge,
    HeaderTooLarge,
    Unavailable,
}

impl Status {
    pub fn code(self) -> u16 {
        match self {
            Self::Ok => 200,
            Self::NoContent => 204,
            Self::BadRequest => 400,
            Self::Unauthorized => 401,
            Self::NotFound => 404,
            Self::MethodNotAllowed => 405,
            Self::LengthRequired => 411,
            Self::PayloadTooLarge => 413,
            Self::HeaderTooLarge => 431,
            Self::Unavailable => 503,
        }
    }

    fn reason(self) -> &'static str {
        match self {
            Self::Ok => "OK",
            Self::NoContent => "No Content",
            Self::BadRequest => "Bad Request",
            Self::Unauthorized => "Unauthorized",
            Self::NotFound => "Not Found",
            Self::MethodNotAllowed => "Method Not Allowed",
            Self::LengthRequired => "Length Required",
            Self::PayloadTooLarge => "Content Too Large",
            Self::HeaderTooLarge => "Request Header Fields Too Large",
            Self::Unavailable => "Service Unavailable",
        }
    }
}

/// A `PermissionRequest` hook waiting for the hub's answer.
#[derive(Debug)]
pub struct PermissionAsk {
    pub post: PermissionPost,
    /// `Some`: the answer chosen in Telegram. `None` or dropped: no decision.
    pub answer: oneshot::Sender<Option<Behavior>>,
}

/// Where `PermissionRequest` hooks go, and how many may wait at a time.
struct PermissionWaits {
    asks: mpsc::Sender<PermissionAsk>,
    waiting: Arc<Semaphore>,
}

/// Serves `POST /v1/hook` until the task is dropped. Each accepted, new
/// event id goes to `events` once; a repeat is answered 204 and dropped.
/// `POST /v1/permission` is not served (404): see
/// [`serve_hooks_and_permissions`].
pub async fn serve_hooks(
    listener: impl Into<Listener>,
    secret: Secret,
    events: mpsc::Sender<HookPost>,
) {
    serve(listener.into(), secret, events, None).await;
}

/// [`serve_hooks`] plus `POST /v1/permission`: each such request goes to
/// `asks` and is held open until the hub answers (at most
/// [`PERMISSION_WAIT_CAP`], [`MAX_PERMISSION_WAITS`] at a time). A waiting
/// request never holds up other hook requests.
pub async fn serve_hooks_and_permissions(
    listener: impl Into<Listener>,
    secret: Secret,
    events: mpsc::Sender<HookPost>,
    asks: mpsc::Sender<PermissionAsk>,
) {
    let waits = PermissionWaits {
        asks,
        waiting: Arc::new(Semaphore::new(MAX_PERMISSION_WAITS)),
    };
    serve(listener.into(), secret, events, Some(waits)).await;
}

async fn serve(
    listener: Listener,
    secret: Secret,
    events: mpsc::Sender<HookPost>,
    waits: Option<PermissionWaits>,
) {
    let Listener {
        tcp: listener,
        incoming,
    } = listener;
    let secret = Arc::new(secret);
    let dedup = Arc::new(Mutex::new(Dedup::new(DEDUP_MAX, DEDUP_TTL)));
    let waits = waits.map(Arc::new);
    let slots = Arc::new(Semaphore::new(MAX_HOOK_REQUESTS));
    let per_peer = PerPeer::new(MAX_PENDING_HOOKS_PER_PEER);
    let gate = WarnGate::default();
    let mut requests = JoinSet::new();
    loop {
        tokio::select! {
            accepted = listener.accept() => {
                let (stream, peer) = match accepted {
                    Ok(accepted) => accepted,
                    Err(error) => {
                        warn!(kind = ?error.kind(), "hook accept failed");
                        tokio::time::sleep(Duration::from_millis(100)).await;
                        continue;
                    }
                };
                let Some(peer_place) = per_peer.take(peer.ip()) else {
                    debug!(%peer, "too many hook requests from one address; closed");
                    continue;
                };
                let Ok(permit) = slots.clone().try_acquire_owned() else {
                    pre_auth!(gate, peer, %peer, "too many hook requests; refused");
                    continue;
                };
                let (secret, dedup, events, waits, incoming) = (
                    secret.clone(),
                    dedup.clone(),
                    events.clone(),
                    waits.clone(),
                    incoming.clone(),
                );
                let pre_auth = PreAuth {
                    peer_place,
                    gate: gate.clone(),
                };
                let deadline = tokio::time::Instant::now() + REQUEST_TIMEOUT;
                requests.spawn(async move {
                    let Some(stream) = open(&incoming, stream, peer, deadline).await else {
                        return;
                    };
                    let waits = waits.as_deref();
                    hook_request(
                        stream, peer, &secret, &dedup, &events, waits, permit, pre_auth, deadline,
                    )
                    .await;
                });
            }
            Some(_) = requests.join_next() => {}
        }
    }
}

/// A hook request's place among those of its peer address, held until the
/// request is read, and where its early failures are logged.
struct PreAuth {
    peer_place: PeerPlace,
    gate: WarnGate,
}

/// `deadline`: the whole request is in by then, TLS handshake included.
#[allow(clippy::too_many_arguments)]
async fn hook_request(
    mut stream: Stream,
    peer: SocketAddr,
    secret: &Secret,
    dedup: &Mutex<Dedup>,
    events: &mpsc::Sender<HookPost>,
    waits: Option<&PermissionWaits>,
    permit: OwnedSemaphorePermit,
    pre_auth: PreAuth,
    deadline: tokio::time::Instant,
) {
    let read = tokio::time::timeout_at(deadline, read_request(&mut stream, secret)).await;
    drop(pre_auth.peer_place);
    let gate = pre_auth.gate;
    let status = match read {
        Ok(Ok((Route::Hook, body))) => accept_hook(&body, dedup, events),
        // `cctg doctor`: the secret matched; nothing else happens.
        Ok(Ok((Route::Ping, _))) => {
            debug!(%peer, "ping answered");
            Status::NoContent
        }
        Ok(Err(None)) => {
            debug!(%peer, "connection closed before its first byte");
            return;
        }
        Ok(Ok((Route::Permission, body))) => match waits {
            Some(waits) => {
                // A waiting hook takes one of its own places, not a
                // hook request place.
                drop(permit);
                return permission_request(stream, peer, &body, waits).await;
            }
            None => Status::NotFound,
        },
        Ok(Err(Some(status))) => {
            if status == Status::Unauthorized {
                pre_auth!(gate, peer, %peer, "hook request rejected: bad or missing secret");
                // No fast guessing; the request place stays taken meanwhile.
                tokio::time::sleep(AUTH_FAIL_DELAY).await;
            } else {
                pre_auth!(gate, peer, %peer, status = status.code(), "hook request rejected");
            }
            respond(&mut stream, status, &[]).await;
            return;
        }
        Err(_) => {
            pre_auth!(gate, peer, %peer, "hook request timed out");
            return;
        }
    };
    if status != Status::NoContent {
        warn!(%peer, status = status.code(), "hook request rejected");
    }
    respond(&mut stream, status, &[]).await;
}

/// Writes the whole answer and closes the connection.
async fn respond(stream: &mut Stream, status: Status, body: &[u8]) {
    let content_type = if body.is_empty() {
        ""
    } else {
        "Content-Type: application/json\r\n"
    };
    let head = format!(
        "HTTP/1.1 {} {}\r\n{content_type}Content-Length: {}\r\nConnection: close\r\n\r\n",
        status.code(),
        status.reason(),
        body.len()
    );
    let _ = stream.write_all(&[head.as_bytes(), body].concat()).await;
    let _ = stream.shutdown().await;
    linger(stream).await;
}

/// Hands a `PermissionRequest` hook to the hub and holds the connection
/// until the answer: `200` with the decision, `204` without one (also when
/// the hub gave up, stopped or too many hooks wait). A hook that goes away
/// while waiting drops its ask, which the hub notices.
async fn permission_request(
    mut stream: Stream,
    peer: SocketAddr,
    body: &[u8],
    waits: &PermissionWaits,
) {
    let post = match wire::decode_permission(body) {
        Ok(post) => post,
        Err(error) => {
            debug!(%error, "permission hook body rejected");
            warn!(%peer, status = Status::BadRequest.code(), "hook request rejected");
            return respond(&mut stream, Status::BadRequest, &[]).await;
        }
    };
    let session = short(&post.session_id).to_owned();
    let Ok(_waiting) = waits.waiting.clone().try_acquire_owned() else {
        warn!(
            session,
            "too many permission hooks wait; answered without a decision"
        );
        return respond(&mut stream, Status::NoContent, &[]).await;
    };
    let (answer, decided) = oneshot::channel();
    if waits.asks.try_send(PermissionAsk { post, answer }).is_err() {
        warn!(
            session,
            "hub cannot take a permission hook now; answered without a decision"
        );
        return respond(&mut stream, Status::NoContent, &[]).await;
    }
    let behavior = tokio::select! {
        decided = decided => decided.ok().flatten(),
        () = gone(&mut stream) => {
            debug!(session, "permission hook went away before an answer");
            return;
        }
        () = tokio::time::sleep(PERMISSION_WAIT_CAP) => None,
    };
    match behavior {
        Some(behavior) => {
            info!(session, ?behavior, "permission hook answered from Telegram");
            let body = serde_json::to_vec(&PermissionAnswer { behavior })
                .expect("a permission answer always serializes");
            respond(&mut stream, Status::Ok, &body).await;
        }
        None => {
            debug!(session, "permission hook answered without a decision");
            respond(&mut stream, Status::NoContent, &[]).await;
        }
    }
}

/// Completes when the peer closed its side (or the connection broke). A
/// waiting hook has sent its whole request, so any read end means it left.
async fn gone(stream: &mut Stream) {
    let mut byte = [0u8; 1];
    loop {
        match stream.read(&mut byte).await {
            Ok(0) | Err(_) => return,
            Ok(_) => {}
        }
    }
}

fn accept_hook(body: &[u8], dedup: &Mutex<Dedup>, events: &mpsc::Sender<HookPost>) -> Status {
    let post = match wire::decode_hook(body) {
        Ok(post) => post,
        Err(error) => {
            debug!(%error, "hook body rejected");
            return Status::BadRequest;
        }
    };
    let Ok(mut dedup) = dedup.lock() else {
        return Status::Unavailable;
    };
    let now = Instant::now();
    if dedup.contains(&post.event_id, now) {
        debug!(event = post.event.kind(), "repeated hook event dropped");
        return Status::NoContent;
    }
    note_hook_version(post.client_version.as_deref());
    let (id, kind, session, frequent) = (
        post.event_id.clone(),
        post.event.kind(),
        short(&post.session_id).to_owned(),
        post.event.is_frequent(),
    );
    match events.try_send(post) {
        Ok(()) => {
            // Remembered only once handed over, so a 503 can be re-sent.
            dedup.insert(id, now);
            if frequent {
                debug!(event = kind, session, "hook event accepted");
            } else {
                info!(event = kind, session, "hook event accepted");
            }
            Status::NoContent
        }
        Err(_) => Status::Unavailable,
    }
}

/// Logs once per hub run that a hook runs another cctg version than the hub
/// (TASK-040). Builds are compared on the agent link; hooks carry only the
/// version, which changes rarely, so one line is enough.
fn note_hook_version(version: Option<&str>) {
    static NOTED: AtomicBool = AtomicBool::new(false);
    if let Some(version) = other_version(version)
        && !NOTED.swap(true, Ordering::Relaxed)
    {
        info!(
            hook = version,
            hub = crate::client::VERSION,
            "a hook runs another cctg version"
        );
    }
}

/// `version` when it is not this hub's, cut to 32 characters for the log.
fn other_version(version: Option<&str>) -> Option<&str> {
    let version = version.filter(|version| *version != crate::client::VERSION)?;
    Some(
        version
            .char_indices()
            .nth(32)
            .map_or(version, |(at, _)| &version[..at]),
    )
}

/// The endpoints.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Route {
    Hook,
    Permission,
    Ping,
}

/// Reads a `POST` with `Content-Length` (no chunked bodies, no keep-alive)
/// and returns its target and body. The secret is checked before the body
/// is read. `Err(None)`: closed before its first byte, nothing to answer.
async fn read_request<S: AsyncRead + Unpin>(
    stream: &mut S,
    secret: &Secret,
) -> Result<(Route, Vec<u8>), Option<Status>> {
    let mut buf = Vec::with_capacity(1024);
    let head_end = loop {
        if let Some(end) = buf.windows(4).position(|window| window == b"\r\n\r\n") {
            if end > MAX_HEAD {
                return Err(Some(Status::HeaderTooLarge));
            }
            break end;
        }
        if buf.len() >= MAX_HEAD + 4 {
            return Err(Some(Status::HeaderTooLarge));
        }
        let mut chunk = [0u8; 1024];
        let remaining = MAX_HEAD + 4 - buf.len();
        let chunk_len = remaining.min(chunk.len());
        let read = stream
            .read(&mut chunk[..chunk_len])
            .await
            .map_err(|_| Some(Status::BadRequest))?;
        if read == 0 {
            // Nothing at all: a probe, not a hook.
            return Err((!buf.is_empty()).then_some(Status::BadRequest));
        }
        buf.extend_from_slice(&chunk[..read]);
    };
    let head = std::str::from_utf8(&buf[..head_end]).map_err(|_| Some(Status::BadRequest))?;
    let mut lines = head.split("\r\n");
    let mut request_line = lines.next().unwrap_or_default().split(' ');
    let (method, target, version) = (
        request_line.next().unwrap_or_default(),
        request_line.next().unwrap_or_default(),
        request_line.next().unwrap_or_default(),
    );
    if request_line.next().is_some() || version != "HTTP/1.1" {
        return Err(Some(Status::BadRequest));
    }
    if method != "POST" {
        return Err(Some(Status::MethodNotAllowed));
    }
    let route = match target {
        HOOK_PATH => Route::Hook,
        PERMISSION_PATH => Route::Permission,
        PING_PATH => Route::Ping,
        _ => return Err(Some(Status::NotFound)),
    };

    let mut length = None;
    let mut authorized = None;
    for line in lines {
        let (name, value) = line.split_once(':').ok_or(Some(Status::BadRequest))?;
        if name.is_empty() || !name.bytes().all(is_tchar) {
            return Err(Some(Status::BadRequest));
        }
        // RFC 9110 5.5: a bare CR, LF or NUL in a value must be rejected.
        if value
            .bytes()
            .any(|byte| byte.is_ascii_control() && byte != b'\t')
        {
            return Err(Some(Status::BadRequest));
        }
        // RFC 9110 5.6.3: optional whitespace is SP / HTAB only.
        let value = value.trim_matches([' ', '\t']);
        if name.eq_ignore_ascii_case("content-length") {
            // RFC 9112 6.3: a repeated or invalid length is an error.
            if length.is_some() || value.is_empty() || !value.bytes().all(|b| b.is_ascii_digit()) {
                return Err(Some(Status::BadRequest));
            }
            length = Some(value.parse::<usize>().unwrap_or(usize::MAX));
        } else if name.eq_ignore_ascii_case("transfer-encoding") {
            return Err(Some(Status::BadRequest));
        } else if name.eq_ignore_ascii_case("authorization") {
            if authorized.is_some() {
                return Err(Some(Status::BadRequest));
            }
            let token = value
                .split_once(' ')
                .filter(|(scheme, _)| scheme.eq_ignore_ascii_case("bearer"))
                .map(|(_, token)| token.trim_matches([' ', '\t']));
            authorized = Some(token.is_some_and(|token| secret.matches(token.as_bytes())));
        }
    }
    if authorized != Some(true) {
        return Err(Some(Status::Unauthorized));
    }
    let length = length.ok_or(Some(Status::LengthRequired))?;
    if length > MAX_HOOK_BODY {
        return Err(Some(Status::PayloadTooLarge));
    }
    let mut body = buf.split_off(head_end + 4);
    if body.len() > length {
        return Err(Some(Status::BadRequest));
    }
    let received = body.len();
    body.resize(length, 0);
    stream
        .read_exact(&mut body[received..])
        .await
        .map_err(|_| Some(Status::BadRequest))?;
    Ok((route, body))
}

fn is_tchar(byte: u8) -> bool {
    byte.is_ascii_alphanumeric()
        || matches!(
            byte,
            b'!' | b'#'
                | b'$'
                | b'%'
                | b'&'
                | b'\''
                | b'*'
                | b'+'
                | b'-'
                | b'.'
                | b'^'
                | b'_'
                | b'`'
                | b'|'
                | b'~'
        )
}

#[cfg(test)]
mod tests {
    #[test]
    fn only_another_hook_version_is_noted() {
        assert_eq!(other_version(None), None);
        assert_eq!(other_version(Some(crate::client::VERSION)), None);
        assert_eq!(other_version(Some("9.9.9")), Some("9.9.9"));
        let long = "ü".repeat(40);
        assert_eq!(other_version(Some(&long)), Some("ü".repeat(32).as_str()));
    }

    use std::net::{Ipv4Addr, SocketAddrV4};

    use tokio::io::AsyncBufReadExt;

    use super::*;
    use crate::wire::HookEvent;

    const SECRET: &str = "0123456789abcdef-secret";
    const WAIT: Duration = Duration::from_secs(10);

    fn secret() -> Secret {
        Secret::parse(SECRET).unwrap()
    }

    async fn within<T>(future: impl std::future::Future<Output = T>) -> T {
        tokio::time::timeout(WAIT, future).await.expect("in time")
    }

    fn loopback() -> SocketAddr {
        SocketAddr::V4(SocketAddrV4::new(Ipv4Addr::LOCALHOST, 0))
    }

    async fn agents_hub() -> (
        SocketAddr,
        mpsc::Receiver<AgentEvent>,
        tokio::task::JoinHandle<()>,
    ) {
        let listener = bind(loopback()).await.unwrap();
        let addr = listener.local_addr().unwrap();
        let (tx, rx) = mpsc::channel(16);
        let task = tokio::spawn(serve_agents(listener, secret(), tx));
        (addr, rx, task)
    }

    fn register() -> Register {
        Register {
            session_id: "5e551017-0000-4000-8000-000000000001".into(),
            host: "box".into(),
            cwd: "/w".into(),
            claude_pid: None,
            verdict_ack: false,
            transcript_reads: false,
            console_keys: false,
            console_commands: false,
            client: None,
            files: false,
            session_reads: false,
        }
    }

    struct Peer {
        reader: BufReader<tokio::net::tcp::OwnedReadHalf>,
        write: tokio::net::tcp::OwnedWriteHalf,
        line: Vec<u8>,
    }

    impl Peer {
        async fn connect(addr: SocketAddr) -> Self {
            let (read, write) = TcpStream::connect(addr).await.unwrap().into_split();
            Self {
                reader: BufReader::new(read),
                write,
                line: Vec::new(),
            }
        }

        async fn send(&mut self, msg: &AgentMsg) {
            wire::write_msg(&mut self.write, msg).await.unwrap();
        }

        async fn raw(&mut self, bytes: &[u8]) {
            self.write.write_all(bytes).await.unwrap();
        }

        async fn recv(&mut self) -> Result<HubMsg, WireError> {
            tokio::time::timeout(WAIT, wire::read_line(&mut self.reader, &mut self.line))
                .await
                .expect("hub answered in time")?;
            let result = wire::decode(&self.line);
            self.line.clear();
            result
        }
    }

    #[tokio::test]
    async fn a_wrong_secret_is_rejected_before_register() {
        let (addr, mut events, _hub) = agents_hub().await;
        let mut peer = Peer::connect(addr).await;
        let wrong = Secret::parse("0123456789abcdef-secreT").unwrap();
        peer.send(&AgentMsg::Hello { secret: wrong }).await;
        peer.send(&AgentMsg::Register(register())).await;
        assert_eq!(
            peer.recv().await,
            Ok(HubMsg::Rejected {
                reason: Rejection::Auth
            })
        );
        assert_eq!(peer.recv().await, Err(WireError::Closed));

        // Register without hello is not processed either.
        let mut peer = Peer::connect(addr).await;
        peer.send(&AgentMsg::Register(register())).await;
        assert_eq!(
            peer.recv().await,
            Ok(HubMsg::Rejected {
                reason: Rejection::Auth
            })
        );
        assert!(events.try_recv().is_err(), "no event for rejected agents");
    }

    #[tokio::test]
    async fn another_version_is_rejected_as_such() {
        let (addr, _events, _hub) = agents_hub().await;
        let mut peer = Peer::connect(addr).await;
        peer.raw(format!("{{\"v\":2,\"type\":\"hello\",\"secret\":\"{SECRET}\"}}\n").as_bytes())
            .await;
        assert_eq!(
            peer.recv().await,
            Ok(HubMsg::Rejected {
                reason: Rejection::Version
            })
        );
    }

    #[tokio::test]
    async fn a_registered_agent_talks_both_ways() {
        let (addr, mut events, _hub) = agents_hub().await;
        let mut peer = Peer::connect(addr).await;
        peer.send(&AgentMsg::Hello { secret: secret() }).await;
        peer.send(&AgentMsg::Register(register())).await;
        assert_eq!(peer.recv().await, Ok(HubMsg::Registered { files: true }));
        let Some(AgentEvent::Registered {
            conn,
            register: got,
            to_agent,
        }) = within(events.recv()).await
        else {
            panic!("expected registration");
        };
        assert_eq!(got, register());

        // Unknown types and bad lines are skipped, the link stays up.
        peer.raw(b"{\"v\":1,\"type\":\"teleport\"}\nnot json\n")
            .await;
        let reply = AgentMsg::Reply {
            text: "done".into(),
        };
        peer.send(&reply).await;
        match within(events.recv()).await {
            Some(AgentEvent::Message {
                conn: from, msg, ..
            }) => {
                assert_eq!((from, msg), (conn, reply));
            }
            other => panic!("expected the reply, got {other:?}"),
        }

        let inbound = HubMsg::Inbound {
            content: "hi".into(),
            meta: Default::default(),
        };
        to_agent.send(inbound.clone()).await.unwrap();
        assert_eq!(peer.recv().await, Ok(inbound));

        drop(peer);
        match tokio::time::timeout(WAIT, events.recv()).await.unwrap() {
            Some(AgentEvent::Disconnected { conn: gone }) => assert_eq!(gone, conn),
            other => panic!("expected disconnect, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn a_split_agent_line_survives_concurrent_outbound_traffic() {
        let (addr, mut events, _hub) = agents_hub().await;
        let mut peer = Peer::connect(addr).await;
        peer.send(&AgentMsg::Hello { secret: secret() }).await;
        peer.send(&AgentMsg::Register(register())).await;
        assert_eq!(peer.recv().await, Ok(HubMsg::Registered { files: true }));
        let Some(AgentEvent::Registered { to_agent, .. }) = within(events.recv()).await else {
            panic!("expected registration");
        };

        let reply = AgentMsg::Reply {
            text: "split reply".repeat(100),
        };
        let encoded = wire::encode(&reply);
        let (first, second) = encoded.split_at(encoded.len() / 2);
        peer.raw(first).await;
        tokio::time::sleep(Duration::from_millis(20)).await;

        to_agent
            .send(HubMsg::Registered { files: true })
            .await
            .unwrap();
        assert_eq!(peer.recv().await, Ok(HubMsg::Registered { files: true }));
        peer.raw(second).await;

        match within(events.recv()).await {
            Some(AgentEvent::Message { msg, .. }) => assert_eq!(msg, reply),
            other => panic!("expected the complete split reply, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn an_overlong_line_closes_the_link() {
        let (addr, mut events, _hub) = agents_hub().await;
        let mut peer = Peer::connect(addr).await;
        peer.send(&AgentMsg::Hello { secret: secret() }).await;
        peer.send(&AgentMsg::Register(register())).await;
        assert_eq!(peer.recv().await, Ok(HubMsg::Registered { files: true }));
        let _ = within(events.recv()).await;

        // No newline ever: the hub must stop reading at the limit and close.
        let Peer {
            mut reader,
            mut write,
            ..
        } = peer;
        let flood = tokio::spawn(async move {
            let chunk = vec![b'a'; 64 * 1024];
            for _ in 0..(wire::MAX_LINE / chunk.len() + 64) {
                if write.write_all(&chunk).await.is_err() {
                    break;
                }
            }
            write
        });
        match tokio::time::timeout(WAIT, events.recv()).await.unwrap() {
            Some(AgentEvent::Disconnected { .. }) => {}
            other => panic!("expected disconnect, got {other:?}"),
        }
        let mut rest = Vec::new();
        let read = tokio::time::timeout(WAIT, reader.read_until(b'\n', &mut rest)).await;
        assert!(matches!(read, Ok(Ok(0)) | Ok(Err(_))), "{read:?}");
        let _ = flood.await;
    }

    #[tokio::test]
    async fn listeners_bind_loopback_and_explicit_addresses() {
        let listener = bind(loopback()).await.unwrap();
        assert!(listener.local_addr().unwrap().ip().is_loopback());
        let any = SocketAddr::V4(SocketAddrV4::new(Ipv4Addr::UNSPECIFIED, 0));
        let listener = bind(any).await.unwrap();
        let local = listener.local_addr().unwrap();
        assert!(!local.ip().is_loopback());
        let (tx, mut rx) = mpsc::channel(4);
        let _hub = tokio::spawn(serve_agents(listener, secret(), tx));
        let mut peer = Peer::connect(SocketAddr::from((Ipv4Addr::LOCALHOST, local.port()))).await;
        peer.send(&AgentMsg::Hello { secret: secret() }).await;
        peer.send(&AgentMsg::Register(register())).await;
        assert_eq!(peer.recv().await, Ok(HubMsg::Registered { files: true }));
        assert!(matches!(
            rx.recv().await,
            Some(AgentEvent::Registered { .. })
        ));
    }

    async fn hooks_hub(queue: usize) -> (SocketAddr, mpsc::Receiver<HookPost>) {
        let listener = bind(loopback()).await.unwrap();
        let addr = listener.local_addr().unwrap();
        let (tx, rx) = mpsc::channel(queue);
        tokio::spawn(serve_hooks(listener, secret(), tx));
        (addr, rx)
    }

    fn post(event: HookEvent) -> HookPost {
        HookPost::new(
            "box".into(),
            "5e551017-0000-4000-8000-000000000001".into(),
            "/w".into(),
            "/w/s.jsonl".into(),
            event,
        )
    }

    fn start() -> HookEvent {
        HookEvent::SessionStart {
            source: Some("resume".into()),
            claude_pid: None,
            parent_claude_pid: None,
        }
    }

    fn request(auth: Option<&str>, body: &[u8]) -> Vec<u8> {
        let mut head = format!(
            "POST {HOOK_PATH} HTTP/1.1\r\nHost: x\r\nContent-Length: {}\r\n",
            body.len()
        );
        if let Some(auth) = auth {
            head.push_str(&format!("Authorization: {auth}\r\n"));
        }
        head.push_str("\r\n");
        [head.as_bytes(), body].concat()
    }

    async fn exchange(addr: SocketAddr, raw: &[u8]) -> u16 {
        let mut stream = TcpStream::connect(addr).await.unwrap();
        stream.write_all(raw).await.unwrap();
        let mut response = Vec::new();
        tokio::time::timeout(WAIT, stream.read_to_end(&mut response))
            .await
            .unwrap()
            .unwrap();
        let text = String::from_utf8(response).unwrap();
        assert!(
            text.ends_with("\r\n\r\n") && text.contains("Connection: close"),
            "{text}"
        );
        text.get(9..12).unwrap().parse().unwrap()
    }

    fn body(post: &HookPost) -> Vec<u8> {
        serde_json::to_vec(post).unwrap()
    }

    #[tokio::test]
    async fn a_ping_checks_the_secret_and_is_no_event() {
        let (addr, mut events) = hooks_hub(8).await;
        let ping = |auth: &str| {
            format!(
                "POST {PING_PATH} HTTP/1.1\r\nAuthorization: {auth}\r\nContent-Length: 0\r\n\r\n"
            )
            .into_bytes()
        };
        assert_eq!(
            exchange(addr, &ping(&format!("Bearer {SECRET}"))).await,
            204
        );
        assert_eq!(
            exchange(addr, &ping("Bearer 0123456789abcdef-wrong")).await,
            401
        );
        let hook = crate::tls::HubAddr::plain(addr.to_string());
        assert_eq!(crate::hook::ping(&hook, &secret(), WAIT).await, Ok(()));
        assert!(events.try_recv().is_err());
    }

    #[tokio::test]
    async fn a_repeated_post_is_delivered_once() {
        let (addr, mut events) = hooks_hub(8).await;
        let bearer = format!("Bearer {SECRET}");
        let first = post(start());
        assert_eq!(
            exchange(addr, &request(Some(&bearer), &body(&first))).await,
            204
        );
        assert_eq!(
            exchange(addr, &request(Some(&bearer), &body(&first))).await,
            204
        );
        // A resumed session repeats session_id and source: a new event, not a repeat.
        let resumed = post(start());
        assert_eq!(
            exchange(addr, &request(Some(&bearer), &body(&resumed))).await,
            204
        );
        assert_eq!(within(events.recv()).await, Some(first));
        assert_eq!(within(events.recv()).await, Some(resumed));
        assert!(events.try_recv().is_err());
    }

    #[tokio::test]
    async fn two_stops_of_one_prompt_are_two_events() {
        let (addr, mut events) = hooks_hub(8).await;
        let bearer = format!("Bearer {SECRET}");
        let stop = || HookEvent::Stop {
            prompt_id: Some("p1".into()),
            last_assistant_message: Some("same".into()),
        };
        for _ in 0..2 {
            assert_eq!(
                exchange(addr, &request(Some(&bearer), &body(&post(stop())))).await,
                204
            );
        }
        assert!(within(events.recv()).await.is_some() && within(events.recv()).await.is_some());
    }

    #[tokio::test]
    async fn bad_requests_get_errors_and_no_event() {
        let (addr, mut events) = hooks_hub(8).await;
        let bearer = format!("Bearer {SECRET}");
        let good = body(&post(start()));
        let mut cases: Vec<(Vec<u8>, u16)> = vec![
            (request(None, &good), 401),
            (request(Some("Bearer 0123456789abcdef-secreT"), &good), 401),
            (request(Some(SECRET), &good), 401),
            (request(Some(&bearer), b"{\"v\":1}"), 400),
            (request(Some(&bearer), b"not json"), 400),
            (
                request(
                    Some(&bearer),
                    &good.replace_type("session_start", "pre_compact"),
                ),
                400,
            ),
            (
                format!("GET {HOOK_PATH} HTTP/1.1\r\nAuthorization: {bearer}\r\n\r\n").into_bytes(),
                405,
            ),
            (
                format!("POST /other HTTP/1.1\r\nAuthorization: {bearer}\r\nContent-Length: 0\r\n\r\n")
                    .into_bytes(),
                404,
            ),
            (
                format!("POST {HOOK_PATH} HTTP/1.1\r\nAuthorization: {bearer}\r\n\r\n").into_bytes(),
                411,
            ),
            (
                format!(
                    "POST {HOOK_PATH} HTTP/1.1\r\nAuthorization: {bearer}\r\nContent-Length: {}\r\n\r\n",
                    MAX_HOOK_BODY + 1
                )
                .into_bytes(),
                413,
            ),
            // Both carry a valid body: only the framing headers make them bad.
            (
                [
                    format!(
                        "POST {HOOK_PATH} HTTP/1.1\r\nAuthorization: {bearer}\r\nTransfer-Encoding: chunked\r\nContent-Length: {}\r\n\r\n",
                        good.len()
                    )
                    .as_bytes(),
                    &good[..],
                ]
                .concat(),
                400,
            ),
            (
                [
                    format!(
                        "POST {HOOK_PATH} HTTP/1.1\r\nAuthorization: {bearer}\r\nContent-Length: {0}\r\nContent-Length: {0}\r\n\r\n",
                        good.len()
                    )
                    .as_bytes(),
                    &good[..],
                ]
                .concat(),
                400,
            ),
            (
                [
                    format!(
                        "POST {HOOK_PATH} HTTP/1.1\r\nAuthorization: Bearer wrong-wrong-wrong\r\nAuthorization: {bearer}\r\nContent-Length: {}\r\n\r\n",
                        good.len()
                    )
                    .as_bytes(),
                    &good[..],
                ]
                .concat(),
                400,
            ),
            (
                format!(
                    "POST {HOOK_PATH} HTTP/1.1\r\nAuthorization: {bearer}\r\nX-Pad: {}\r\n\r\n",
                    "a".repeat(MAX_HEAD)
                )
                .into_bytes(),
                431,
            ),
        ];
        // Framing that is almost right: every one carries the valid bearer
        // and a valid body, so only the flaw can explain the status.
        let framed = |version: &str, headers: &str| {
            [
                format!("POST {HOOK_PATH} {version}\r\nAuthorization: {bearer}\r\n{headers}\r\n")
                    .as_bytes(),
                &good[..],
            ]
            .concat()
        };
        let n = good.len();
        cases.extend([
            (
                framed(
                    "HTTP/1.1",
                    &format!("Authorization: Bearer wrong-wrong-wrong\r\nContent-Length: {n}\r\n"),
                ),
                400,
            ),
            (framed("HTTP/1.x", &format!("Content-Length: {n}\r\n")), 400),
            (framed("HTTP/1.0", &format!("Content-Length: {n}\r\n")), 400),
            (
                framed("HTTP/1.10", &format!("Content-Length: {n}\r\n")),
                400,
            ),
            (framed("HTTP/1.1", "Transfer-Encoding: chunked\r\n"), 400),
            (
                framed("HTTP/1.1", &format!("Content-Length: +{n}\r\n")),
                400,
            ),
            (
                framed("HTTP/1.1", &format!("Content-Length: {n}x\r\n")),
                400,
            ),
            (
                framed("HTTP/1.1", &format!("Content-Length: {n},{n}\r\n")),
                400,
            ),
            (
                framed("HTTP/1.1", &format!("Content-Length: -{n}\r\n")),
                400,
            ),
            (
                framed(
                    "HTTP/1.1",
                    &format!("Content-Length: {n}\r\nContent-Length: {}\r\n", n + 1),
                ),
                400,
            ),
            (
                framed("HTTP/1.1", "Content-Length: 99999999999999999999999999\r\n"),
                413,
            ),
            (
                framed("HTTP/1.1", &format!("Content-Length : {n}\r\n")),
                400,
            ),
            (
                framed(
                    "HTTP/1.1",
                    &format!("Content-Length: {n}\r\nBad(Name: x\r\n"),
                ),
                400,
            ),
            (
                framed(
                    "HTTP/1.1",
                    &format!("Content-Length: {n}\r\nX-Fold: a\r\n b\r\n"),
                ),
                400,
            ),
            (
                framed(
                    "HTTP/1.1",
                    &format!("Content-Length: {n}\r\nX-Lf: a\nb\r\n"),
                ),
                400,
            ),
            (
                framed(
                    "HTTP/1.1",
                    &format!("Content-Length: {n}\r\nX-Nul: a\0b\r\n"),
                ),
                400,
            ),
            (
                framed(
                    "HTTP/1.1",
                    &format!("Content-Length: {n}\r\nX-Cr: a\rb\r\n"),
                ),
                400,
            ),
        ]);
        for (raw, want) in cases {
            let got = exchange(addr, &raw).await;
            assert_eq!(
                got,
                want,
                "{}",
                String::from_utf8_lossy(&raw[..raw.len().min(120)])
            );
        }
        assert!(events.try_recv().is_err());
    }

    #[tokio::test]
    async fn the_header_limit_is_exact() {
        let (addr, mut events) = hooks_hub(4).await;
        let bearer = format!("Bearer {SECRET}");
        let sent = post(start());
        let body = body(&sent);
        let prefix = format!(
            "POST {HOOK_PATH} HTTP/1.1\r\nAuthorization: {bearer}\r\nContent-Length: {}\r\nX-Pad: ",
            body.len()
        );
        let at_limit = [
            prefix.as_bytes(),
            &vec![b'a'; MAX_HEAD - prefix.len()],
            b"\r\n\r\n",
            &body,
        ]
        .concat();
        assert_eq!(exchange(addr, &at_limit).await, 204);
        assert_eq!(within(events.recv()).await, Some(sent));

        let over_limit = [
            prefix.as_bytes(),
            &vec![b'a'; MAX_HEAD + 1 - prefix.len()],
            b"\r\n\r\n",
            &body,
        ]
        .concat();
        assert_eq!(exchange(addr, &over_limit).await, 431);
        assert!(events.try_recv().is_err());
    }

    trait ReplaceType {
        fn replace_type(&self, from: &str, to: &str) -> Vec<u8>;
    }

    impl ReplaceType for Vec<u8> {
        fn replace_type(&self, from: &str, to: &str) -> Vec<u8> {
            String::from_utf8_lossy(self).replace(from, to).into_bytes()
        }
    }

    #[tokio::test]
    async fn a_full_queue_answers_503_and_the_resend_is_delivered() {
        let (addr, mut events) = hooks_hub(1).await;
        let bearer = format!("Bearer {SECRET}");
        let first = post(start());
        let second = post(HookEvent::SessionEnd {
            reason: None,
            claude_pid: None,
        });
        assert_eq!(
            exchange(addr, &request(Some(&bearer), &body(&first))).await,
            204
        );
        assert_eq!(
            exchange(addr, &request(Some(&bearer), &body(&second))).await,
            503
        );
        assert_eq!(within(events.recv()).await, Some(first));
        assert_eq!(
            exchange(addr, &request(Some(&bearer), &body(&second))).await,
            204
        );
        assert_eq!(within(events.recv()).await, Some(second));
    }

    #[tokio::test]
    async fn a_body_split_across_writes_is_read_whole() {
        let (addr, mut events) = hooks_hub(4).await;
        let sent = post(start());
        let raw = request(Some(&format!("Bearer {SECRET}")), &body(&sent));
        let mut stream = TcpStream::connect(addr).await.unwrap();
        for piece in raw.chunks(7) {
            stream.write_all(piece).await.unwrap();
            stream.flush().await.unwrap();
        }
        let mut response = Vec::new();
        stream.read_to_end(&mut response).await.unwrap();
        assert!(response.starts_with(b"HTTP/1.1 204"));
        assert_eq!(within(events.recv()).await, Some(sent));
    }

    #[tokio::test]
    async fn a_body_of_exactly_the_limit_is_accepted() {
        let (addr, mut events) = hooks_hub(4).await;
        let mut sent = post(HookEvent::Stop {
            prompt_id: None,
            last_assistant_message: Some(String::new()),
        });
        let pad = MAX_HOOK_BODY - body(&sent).len();
        sent.event = HookEvent::Stop {
            prompt_id: None,
            last_assistant_message: Some("x".repeat(pad)),
        };
        assert_eq!(body(&sent).len(), MAX_HOOK_BODY);
        let raw = request(Some(&format!("Bearer {SECRET}")), &body(&sent));
        assert_eq!(exchange(addr, &raw).await, 204);
        assert_eq!(within(events.recv()).await, Some(sent));
    }

    /// The 401 goes out after the head; the unread body must be drained so
    /// that closing does not reset the connection and destroy the answer.
    #[tokio::test]
    async fn an_early_401_survives_a_body_of_the_full_limit() {
        let (addr, mut events) = hooks_hub(4).await;
        let raw = request(
            Some("Bearer 0123456789abcdef-secreT"),
            &vec![b'x'; MAX_HOOK_BODY],
        );
        assert_eq!(exchange(addr, &raw).await, 401);
        assert!(events.try_recv().is_err());
    }

    #[tokio::test]
    async fn one_connection_carries_at_most_one_event() {
        let (addr, mut events) = hooks_hub(4).await;
        let bearer = format!("Bearer {SECRET}");
        // Two requests in one write: bytes read past Content-Length are
        // refused (400); if the read happened to stop at the first body, the
        // second request is never read. Never two events.
        let mut raw = request(Some(&bearer), &body(&post(start())));
        raw.extend(request(Some(&bearer), &body(&post(start()))));
        let status = exchange(addr, &raw).await;
        assert!(status == 400 || status == 204, "{status}");
        tokio::time::sleep(Duration::from_millis(50)).await;
        let delivered = std::iter::from_fn(|| events.try_recv().ok()).count();
        assert_eq!(delivered, usize::from(status == 204));

        // A second request written after the answer is never read.
        let first = post(start());
        let mut stream = TcpStream::connect(addr).await.unwrap();
        stream
            .write_all(&request(Some(&bearer), &body(&first)))
            .await
            .unwrap();
        let mut status = [0u8; 12];
        within(stream.read_exact(&mut status)).await.unwrap();
        assert_eq!(&status, b"HTTP/1.1 204");
        let _ = stream
            .write_all(&request(Some(&bearer), &body(&post(start()))))
            .await;
        let mut rest = Vec::new();
        let _ = within(stream.read_to_end(&mut rest)).await;
        assert_eq!(within(events.recv()).await, Some(first));
        tokio::time::sleep(Duration::from_millis(50)).await;
        assert!(events.try_recv().is_err());
    }

    #[tokio::test]
    async fn a_slow_request_is_closed_at_the_deadline_without_an_event() {
        let (addr, mut events) = hooks_hub(4).await;
        let mut stream = TcpStream::connect(addr).await.unwrap();
        let started = Instant::now();
        stream
            .write_all(format!("POST {HOOK_PATH} HTTP/1.1\r\nX:").as_bytes())
            .await
            .unwrap();
        let mut response = Vec::new();
        let _ = within(stream.read_to_end(&mut response)).await;
        let waited = started.elapsed();
        assert!(
            waited >= REQUEST_TIMEOUT - Duration::from_millis(100) && waited < WAIT,
            "{waited:?}"
        );
        assert!(response.is_empty());
        assert!(events.try_recv().is_err());
    }

    #[test]
    fn dedup_forgets_by_age_and_by_count() {
        let t0 = Instant::now();
        let ttl = Duration::from_secs(60);
        let mut dedup = Dedup::new(3, ttl);
        let ids: Vec<EventId> = (0..5).map(|_| EventId::new()).collect();
        dedup.insert(ids[0].clone(), t0);
        assert!(dedup.contains(&ids[0], t0 + ttl - Duration::from_millis(1)));
        assert!(!dedup.contains(&ids[0], t0 + ttl));
        assert!(dedup.is_empty());

        for id in &ids {
            dedup.insert(id.clone(), t0);
        }
        assert_eq!(dedup.len(), 3);
        assert!(!dedup.contains(&ids[1], t0));
        assert!(dedup.contains(&ids[2], t0) && dedup.contains(&ids[4], t0));
        dedup.insert(ids[4].clone(), t0);
        assert_eq!(dedup.len(), 3);
    }

    #[test]
    fn short_ids_cut_at_eight_chars() {
        assert_eq!(short("5e551017-0000"), "5e551017");
        assert_eq!(short("abc"), "abc");
        assert_eq!(short("ééééééééé"), "éééééééé");
    }

    // ---- Before authentication (TASK-035 review: the listeners may face the
    // internet). One deadline from the TCP connect, TLS handshake included;
    // few unauthenticated agent connections; a short first line; a pause
    // after a wrong secret; garbage never stops a listener.

    fn tls_acceptor() -> (crate::tls::Acceptor, crate::tls::CertPin) {
        use rustls::pki_types::pem::PemObject;
        let rcgen::CertifiedKey { cert, signing_key } =
            rcgen::generate_simple_self_signed(vec!["localhost".to_owned()]).unwrap();
        let key = rustls::pki_types::PrivateKeyDer::from_pem_slice(
            signing_key.serialize_pem().as_bytes(),
        )
        .unwrap();
        crate::tls::Acceptor::new(vec![cert.der().clone()], key).unwrap()
    }

    /// TCP connect now, the TLS handshake after `late`, then one byte of
    /// the request every 100 ms. Returns how long after the TCP connect the
    /// hub closed the connection.
    async fn late_and_slow(addr: SocketAddr, pin: crate::tls::CertPin, late: Duration) -> Duration {
        let tcp = TcpStream::connect(addr).await.unwrap();
        let connected = tokio::time::Instant::now();
        tokio::time::sleep(late).await;
        let name = rustls::pki_types::ServerName::try_from("localhost").unwrap();
        let connector = tokio_rustls::TlsConnector::from(crate::tls::pinned_config(pin).unwrap());
        let stream = connector.connect(name, tcp).await.expect("in time");
        let (mut read, mut write) = tokio::io::split(stream);
        let trickle = async {
            loop {
                tokio::time::sleep(Duration::from_millis(100)).await;
                if write.write_all(b"P").await.is_err() || write.flush().await.is_err() {
                    return;
                }
            }
        };
        let closed = async {
            let mut buf = [0u8; 256];
            while let Ok(n) = read.read(&mut buf).await {
                if n == 0 {
                    return;
                }
            }
        };
        within(async {
            tokio::select! {
                () = trickle => {}
                () = closed => {}
            }
        })
        .await;
        connected.elapsed()
    }

    #[tokio::test]
    async fn the_tls_handshake_counts_against_the_pre_auth_deadline() {
        let (acceptor, pin) = tls_acceptor();
        let agents = bind(loopback()).await.unwrap();
        let hooks = bind(loopback()).await.unwrap();
        let (agents_addr, hooks_addr) = (agents.local_addr().unwrap(), hooks.local_addr().unwrap());
        let (agent_tx, _agent_rx) = mpsc::channel(4);
        let (hook_tx, _hook_rx) = mpsc::channel(4);
        let agents = Listener::tls(agents, acceptor.clone());
        let hooks = Listener::tls(hooks, acceptor);
        tokio::spawn(serve_agents(agents, secret(), agent_tx));
        tokio::spawn(serve_hooks(hooks, secret(), hook_tx));

        let late = HANDSHAKE_TIMEOUT - Duration::from_secs(2);
        let agent = late_and_slow(agents_addr, pin, late).await;
        assert!(
            agent < HANDSHAKE_TIMEOUT + Duration::from_millis(700),
            "agent closed {agent:?} after its connect"
        );
        let late = REQUEST_TIMEOUT - Duration::from_secs(1);
        let hook = late_and_slow(hooks_addr, pin, late).await;
        assert!(
            hook < REQUEST_TIMEOUT + Duration::from_millis(500),
            "hook closed {hook:?} after its connect"
        );
    }

    /// Connects from `source` (port 0) to `addr`.
    async fn connect_from(source: IpAddr, addr: SocketAddr) -> TcpStream {
        let socket = tokio::net::TcpSocket::new_v4().unwrap();
        socket.bind(SocketAddr::new(source, 0)).unwrap();
        socket.connect(addr).await.unwrap()
    }

    /// Closed by the hub at once, not at a deadline, without an answer.
    async fn closed_at_once(mut stream: TcpStream) {
        let mut buf = [0u8; 16];
        let read = tokio::time::timeout(Duration::from_secs(1), stream.read(&mut buf))
            .await
            .expect("closed at once, not at the deadline");
        assert!(matches!(read, Ok(0) | Err(_)), "{read:?}");
    }

    async fn registers(addr: SocketAddr, events: &mut mpsc::Receiver<AgentEvent>) {
        let mut peer = Peer::connect(addr).await;
        peer.send(&AgentMsg::Hello { secret: secret() }).await;
        peer.send(&AgentMsg::Register(register())).await;
        assert_eq!(peer.recv().await, Ok(HubMsg::Registered { files: true }));
        assert!(matches!(
            within(events.recv()).await,
            Some(AgentEvent::Registered { .. })
        ));
    }

    #[tokio::test]
    async fn unauthenticated_agents_of_one_address_beyond_its_cap_are_closed_at_once() {
        let (addr, mut events, _hub) = agents_hub().await;
        let mut idle = Vec::new();
        for _ in 0..MAX_PENDING_AGENTS_PER_PEER {
            idle.push(TcpStream::connect(addr).await.unwrap());
        }
        tokio::time::sleep(Duration::from_millis(200)).await;
        closed_at_once(TcpStream::connect(addr).await.unwrap()).await;

        // The places come back when those connections go.
        drop(idle);
        tokio::time::sleep(Duration::from_millis(200)).await;
        registers(addr, &mut events).await;
    }

    /// Several loopback source addresses: macOS has only 127.0.0.1.
    #[cfg(not(target_os = "macos"))]
    #[tokio::test]
    async fn unauthenticated_agents_beyond_the_cap_are_closed_at_once() {
        let (addr, mut events, _hub) = agents_hub().await;
        let sources = (2..).map(|last| IpAddr::V4(Ipv4Addr::new(127, 0, 0, last)));
        let mut idle = Vec::new();
        for source in sources.take(MAX_PENDING_AGENTS / MAX_PENDING_AGENTS_PER_PEER) {
            for _ in 0..MAX_PENDING_AGENTS_PER_PEER {
                idle.push(connect_from(source, addr).await);
            }
        }
        tokio::time::sleep(Duration::from_millis(200)).await;
        // A fresh address, still under its own cap: the global one closes it.
        closed_at_once(connect_from(Ipv4Addr::new(127, 0, 0, 99).into(), addr).await).await;

        drop(idle);
        tokio::time::sleep(Duration::from_millis(200)).await;
        registers(addr, &mut events).await;
    }

    #[tokio::test]
    async fn pending_hook_requests_of_one_address_beyond_its_cap_are_closed_at_once() {
        let (addr, mut events) = hooks_hub(4).await;
        let mut idle = Vec::new();
        for _ in 0..MAX_PENDING_HOOKS_PER_PEER {
            idle.push(TcpStream::connect(addr).await.unwrap());
        }
        tokio::time::sleep(Duration::from_millis(200)).await;
        closed_at_once(TcpStream::connect(addr).await.unwrap()).await;

        drop(idle);
        tokio::time::sleep(Duration::from_millis(200)).await;
        let body = serde_json::to_vec(&post(start())).unwrap();
        let auth = format!("Bearer {SECRET}");
        assert_eq!(exchange(addr, &request(Some(&auth), &body)).await, 204);
        assert!(within(events.recv()).await.is_some());
    }

    #[test]
    fn places_per_address_are_counted_and_given_back() {
        let places = PerPeer::new(2);
        let a: IpAddr = Ipv4Addr::new(192, 0, 2, 1).into();
        let b: IpAddr = Ipv4Addr::new(192, 0, 2, 2).into();
        let first = places.take(a).unwrap();
        let _second = places.take(a).unwrap();
        assert!(places.take(a).is_none(), "a third from one address");
        // The same address written as IPv4-mapped IPv6 counts as the same.
        let mapped: IpAddr = Ipv4Addr::new(192, 0, 2, 1).to_ipv6_mapped().into();
        assert!(places.take(mapped).is_none());
        assert!(places.take(b).is_some(), "another address");
        drop(first);
        assert!(places.take(a).is_some(), "given back");
    }

    #[test]
    fn an_address_warns_at_most_once_a_minute() {
        let gate = WarnGate::default();
        let a: IpAddr = Ipv4Addr::new(192, 0, 2, 1).into();
        assert!(gate.warns(a));
        assert!(!gate.warns(a), "the second within a minute is debug");
        assert!(
            gate.warns(Ipv4Addr::new(192, 0, 2, 2).into()),
            "another address"
        );
        // Too many addresses at once: the rest stays at debug.
        let gate = WarnGate::default();
        for n in 0..NOISE_PEERS as u32 {
            assert!(gate.warns(IpAddr::V4((0x0a00_0000 + n).into())));
        }
        assert!(!gate.warns(Ipv4Addr::new(192, 0, 2, 3).into()));
    }

    #[tokio::test]
    async fn a_long_first_line_is_refused_before_the_secret_is_read() {
        let (addr, mut events, _hub) = agents_hub().await;
        let mut peer = Peer::connect(addr).await;
        let started = Instant::now();
        peer.raw(&vec![b'a'; MAX_HELLO_LINE + 1]).await;
        assert_eq!(
            peer.recv().await,
            Ok(HubMsg::Rejected {
                reason: Rejection::Protocol
            })
        );
        assert!(started.elapsed() < HANDSHAKE_TIMEOUT, "not at the deadline");
        assert!(events.try_recv().is_err());
    }

    #[tokio::test]
    async fn a_wrong_secret_is_answered_after_a_fixed_pause() {
        let (addr, _events, _hub) = agents_hub().await;
        let wrong = Secret::parse("0123456789abcdef-secreT").unwrap();
        let mut peer = Peer::connect(addr).await;
        let started = Instant::now();
        peer.send(&AgentMsg::Hello { secret: wrong }).await;
        assert_eq!(
            peer.recv().await,
            Ok(HubMsg::Rejected {
                reason: Rejection::Auth
            })
        );
        assert!(
            started.elapsed() >= AUTH_FAIL_DELAY,
            "{:?}",
            started.elapsed()
        );

        let (hooks, _rx) = hooks_hub(4).await;
        let started = Instant::now();
        let raw = request(Some("Bearer 0123456789abcdef-secreT"), b"{}");
        assert_eq!(exchange(hooks, &raw).await, 401);
        assert!(
            started.elapsed() >= AUTH_FAIL_DELAY,
            "{:?}",
            started.elapsed()
        );
        let started = Instant::now();
        assert_eq!(exchange(hooks, &request(None, b"{}")).await, 401);
        assert!(
            started.elapsed() >= AUTH_FAIL_DELAY,
            "{:?}",
            started.elapsed()
        );
    }

    /// xorshift64*: repeatable noise without a new dependency.
    struct Noise(u64);

    impl Noise {
        fn next(&mut self) -> u64 {
            self.0 ^= self.0 >> 12;
            self.0 ^= self.0 << 25;
            self.0 ^= self.0 >> 27;
            self.0.wrapping_mul(0x2545_f491_4f6c_dd1d)
        }

        fn bytes(&mut self, max: usize) -> Vec<u8> {
            let len = (self.next() as usize) % (max + 1);
            (0..len).map(|_| self.next() as u8).collect()
        }
    }

    #[tokio::test]
    async fn garbage_before_auth_never_panics_or_grows() {
        let mut noise = Noise(0x0123_4567_89ab_cdef);
        let prefixes: [&[u8]; 5] = [
            b"",
            b"POST /v1/hook HTTP/1.1\r\n",
            b"POST /v1/hook HTTP/1.1\r\nAuthorization: Bearer ",
            b"{\"v\":1,\"type\":\"hello\",\"secret\":",
            b"\x16\x03\x01\x02\x00\x01\x00\x01\xfc\x03\x03",
        ];
        for round in 0..2000 {
            let prefix = prefixes[round % prefixes.len()];
            let input = [prefix, &noise.bytes(3 * 1024)].concat();
            // Parsers of both listeners on arbitrary input: an answer, never
            // a panic.
            let mut reader = &input[..];
            let _ = read_request(&mut reader, &secret()).await;
            let _ = wire::decode::<AgentMsg>(&input);
            let mut line = Vec::new();
            let mut reader = BufReader::new(&input[..]);
            let _ = wire::read_line_max(&mut reader, &mut line, MAX_HELLO_LINE).await;
            assert!(line.len() <= MAX_HELLO_LINE);
        }
    }

    #[tokio::test]
    async fn garbage_and_foreign_handshakes_never_stop_a_listener() {
        let (acceptor, pin) = tls_acceptor();
        let (plain_agents, mut agent_events, _hub) = agents_hub().await;
        let (plain_hooks, mut hook_events) = hooks_hub(8).await;
        let tls_hooks = bind(loopback()).await.unwrap();
        let tls_hooks_addr = tls_hooks.local_addr().unwrap();
        let (hook_tx, mut tls_hook_events) = mpsc::channel(8);
        tokio::spawn(serve_hooks(
            Listener::tls(tls_hooks, acceptor),
            secret(),
            hook_tx,
        ));

        let mut noise = Noise(0xfeed_f00d_dead_beef);
        for addr in [plain_agents, plain_hooks, tls_hooks_addr] {
            for _ in 0..12 {
                let payload = noise.bytes(16 * 1024);
                let mut stream = TcpStream::connect(addr).await.unwrap();
                let _ = stream.write_all(&payload).await;
                let _ = stream.shutdown().await;
                let mut sink = Vec::new();
                let _ = within(stream.read_to_end(&mut sink)).await;
                assert!(sink.len() < 4096, "a short answer at most");
            }
        }
        // A TLS ClientHello to the plain listeners, plain text to TLS.
        for addr in [plain_agents, plain_hooks] {
            let tls = crate::tls::HubAddr::pinned(&addr.to_string(), pin).unwrap();
            assert!(within(tls.connect()).await.is_err());
        }
        let answer = exchange_raw(
            tls_hooks_addr,
            &request(Some(&format!("Bearer {SECRET}")), b"{}"),
        )
        .await;
        assert!(
            !answer.starts_with(b"HTTP/"),
            "plain text gets no HTTP answer from a TLS listener"
        );

        // Everyone real still gets through.
        let mut peer = Peer::connect(plain_agents).await;
        peer.send(&AgentMsg::Hello { secret: secret() }).await;
        peer.send(&AgentMsg::Register(register())).await;
        assert_eq!(peer.recv().await, Ok(HubMsg::Registered { files: true }));
        assert!(matches!(
            within(agent_events.recv()).await,
            Some(AgentEvent::Registered { .. })
        ));
        let sent = post(start());
        let plain = crate::tls::HubAddr::plain(plain_hooks.to_string());
        crate::hook::post(&plain, &secret(), &sent, WAIT)
            .await
            .unwrap();
        assert_eq!(within(hook_events.recv()).await, Some(sent));
        let sent = post(start());
        let tls = crate::tls::HubAddr::pinned(&tls_hooks_addr.to_string(), pin).unwrap();
        crate::hook::post(&tls, &secret(), &sent, WAIT)
            .await
            .unwrap();
        assert_eq!(within(tls_hook_events.recv()).await, Some(sent));
    }

    async fn exchange_raw(addr: SocketAddr, raw: &[u8]) -> Vec<u8> {
        let mut stream = TcpStream::connect(addr).await.unwrap();
        let _ = stream.write_all(raw).await;
        let mut response = Vec::new();
        let _ = within(stream.read_to_end(&mut response)).await;
        response
    }
}
