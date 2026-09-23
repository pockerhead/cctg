//! Hub ingress: the agent TCP link and the hook HTTP endpoint.
//!
//! Both check the shared secret before anything else and hand what they
//! accept to the hub over bounded channels ([`AgentEvent`], [`HookPost`]).
//! Nothing here logs message contents, paths or the secret: log lines carry
//! the connection number, the peer address and fixed text only.

use std::collections::{HashSet, VecDeque};
use std::net::SocketAddr;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use tokio::io::{AsyncRead, AsyncReadExt, AsyncWriteExt, BufReader};
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::{Semaphore, mpsc};
use tokio::task::JoinSet;
use tracing::{debug, info, warn};

use crate::wire::{
    self, AgentMsg, EventId, HOOK_PATH, HookPost, HubMsg, MAX_HOOK_BODY, Register, Rejection,
    Secret, WireError,
};

/// Time an agent has to send `hello` and `register`.
const HANDSHAKE_TIMEOUT: Duration = Duration::from_secs(5);
/// Time a hook has to deliver its whole request.
const REQUEST_TIMEOUT: Duration = Duration::from_secs(2);
const MAX_AGENTS: usize = 256;
const MAX_HOOK_REQUESTS: usize = 64;
const MAX_HEAD: usize = 8 * 1024;
const TO_AGENT_QUEUE: usize = 64;
/// How long, and how much, a rejected peer's unread input is drained.
const LINGER: Duration = Duration::from_millis(250);
const LINGER_BYTES: usize = MAX_HEAD + MAX_HOOK_BODY;
/// A re-sent POST arrives within the hook's own lifetime (seconds, at most
/// its timeout), so ten minutes is ample; the size cap bounds memory.
pub const DEDUP_TTL: Duration = Duration::from_secs(10 * 60);
pub const DEDUP_MAX: usize = 4096;

/// Binds `addr`. A non-loopback address was configured explicitly; say so.
pub async fn bind(addr: SocketAddr) -> std::io::Result<TcpListener> {
    let listener = TcpListener::bind(addr).await?;
    let local = listener.local_addr()?;
    if !local.ip().is_loopback() {
        warn!(%local, "listening beyond loopback: plain TCP, keep it on a private network");
    }
    Ok(listener)
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
    /// A `reply` or `permission_request` after registration.
    Message {
        conn: u64,
        msg: AgentMsg,
    },
    Disconnected {
        conn: u64,
    },
}

/// Accepts agents until the task is dropped; dropping it also closes every
/// agent connection, which is what a hub restart looks like to an agent.
pub async fn serve_agents(listener: TcpListener, secret: Secret, events: mpsc::Sender<AgentEvent>) {
    let secret = Arc::new(secret);
    let slots = Arc::new(Semaphore::new(MAX_AGENTS));
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
                let Ok(permit) = slots.clone().try_acquire_owned() else {
                    warn!(%peer, "too many agent connections; refused");
                    continue;
                };
                let conn = next_conn.fetch_add(1, Ordering::Relaxed);
                let (secret, events) = (secret.clone(), events.clone());
                sessions.spawn(async move {
                    agent_session(stream, peer, conn, &secret, &events).await;
                    drop(permit);
                });
            }
            Some(_) = sessions.join_next() => {}
        }
    }
}

async fn reject(write: &mut (impl AsyncWriteExt + Unpin), reason: Rejection) {
    let _ = wire::write_msg(write, &HubMsg::Rejected { reason }).await;
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

async fn agent_session(
    stream: TcpStream,
    peer: SocketAddr,
    conn: u64,
    secret: &Secret,
    events: &mpsc::Sender<AgentEvent>,
) {
    let _ = stream.set_nodelay(true);
    let (read, mut write) = stream.into_split();
    let mut reader = BufReader::new(read);
    let mut line = Vec::new();

    let handshake = tokio::time::timeout(HANDSHAKE_TIMEOUT, async {
        wire::read_line(&mut reader, &mut line)
            .await
            .map_err(|_| Rejection::Protocol)?;
        let authed = match wire::decode::<AgentMsg>(&line) {
            Ok(AgentMsg::Hello { secret: offered }) => secret.matches(offered.expose().as_bytes()),
            Err(WireError::Version) => return Err(Rejection::Version),
            _ => false,
        };
        if !authed {
            return Err(Rejection::Auth);
        }
        wire::read_line(&mut reader, &mut line)
            .await
            .map_err(|_| Rejection::Protocol)?;
        match wire::decode::<AgentMsg>(&line) {
            Ok(AgentMsg::Register(register)) => Ok(register),
            Err(WireError::Version) => Err(Rejection::Version),
            _ => Err(Rejection::Protocol),
        }
    })
    .await;
    let register = match handshake {
        Ok(Ok(register)) => register,
        Ok(Err(reason)) => {
            warn!(conn, %peer, ?reason, "agent rejected");
            reject(&mut write, reason).await;
            linger(&mut reader).await;
            return;
        }
        Err(_) => {
            warn!(conn, %peer, "agent handshake timed out");
            return;
        }
    };

    let session = short(&register.session_id).to_owned();
    let (to_agent, mut outbound) = mpsc::channel(TO_AGENT_QUEUE);
    let registered = AgentEvent::Registered {
        conn,
        register,
        to_agent,
    };
    if events.send(registered).await.is_err()
        || wire::write_msg(&mut write, &HubMsg::Registered)
            .await
            .is_err()
    {
        return;
    }
    info!(conn, session, "agent registered");

    let mut outbound_open = true;
    loop {
        tokio::select! {
            read = wire::read_line(&mut reader, &mut line) => {
                if let Err(error) = read {
                    debug!(conn, %error, "agent link ended");
                    break;
                }
                match wire::decode::<AgentMsg>(&line) {
                    Ok(msg @ (AgentMsg::Reply { .. } | AgentMsg::PermissionRequest(_))) => {
                        if events.send(AgentEvent::Message { conn, msg }).await.is_err() {
                            break;
                        }
                    }
                    Ok(_) => warn!(conn, "agent repeated its handshake; ignored"),
                    Err(WireError::Version) => {
                        warn!(conn, "agent changed protocol version; closing");
                        reject(&mut write, Rejection::Version).await;
                        break;
                    }
                    Err(error) => warn!(conn, %error, "agent line ignored"),
                }
            }
            msg = outbound.recv(), if outbound_open => match msg {
                Some(msg) => {
                    if wire::write_msg(&mut write, &msg).await.is_err() {
                        break;
                    }
                }
                None => outbound_open = false,
            },
        }
    }
    let _ = events.send(AgentEvent::Disconnected { conn }).await;
    info!(conn, session, "agent disconnected");
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

/// Serves `POST /v1/hook` until the task is dropped. Each accepted, new
/// event id goes to `events` once; a repeat is answered 204 and dropped.
pub async fn serve_hooks(listener: TcpListener, secret: Secret, events: mpsc::Sender<HookPost>) {
    let secret = Arc::new(secret);
    let dedup = Arc::new(Mutex::new(Dedup::new(DEDUP_MAX, DEDUP_TTL)));
    let slots = Arc::new(Semaphore::new(MAX_HOOK_REQUESTS));
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
                let Ok(permit) = slots.clone().try_acquire_owned() else {
                    warn!(%peer, "too many hook requests; refused");
                    continue;
                };
                let (secret, dedup, events) = (secret.clone(), dedup.clone(), events.clone());
                requests.spawn(async move {
                    hook_request(stream, peer, &secret, &dedup, &events).await;
                    drop(permit);
                });
            }
            Some(_) = requests.join_next() => {}
        }
    }
}

async fn hook_request(
    mut stream: TcpStream,
    peer: SocketAddr,
    secret: &Secret,
    dedup: &Mutex<Dedup>,
    events: &mpsc::Sender<HookPost>,
) {
    let status =
        match tokio::time::timeout(REQUEST_TIMEOUT, read_request(&mut stream, secret)).await {
            Ok(Ok(body)) => accept_hook(&body, dedup, events),
            Ok(Err(status)) => status,
            Err(_) => {
                warn!(%peer, "hook request timed out");
                return;
            }
        };
    if status == Status::Unauthorized {
        warn!(%peer, "hook request rejected: bad or missing secret");
    } else if status != Status::NoContent {
        warn!(%peer, status = status.code(), "hook request rejected");
    }
    let response = format!(
        "HTTP/1.1 {} {}\r\nContent-Length: 0\r\nConnection: close\r\n\r\n",
        status.code(),
        status.reason()
    );
    let _ = stream.write_all(response.as_bytes()).await;
    let _ = stream.shutdown().await;
    linger(&mut stream).await;
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
    let (id, kind, session) = (
        post.event_id.clone(),
        post.event.kind(),
        short(&post.session_id).to_owned(),
    );
    match events.try_send(post) {
        Ok(()) => {
            // Remembered only once handed over, so a 503 can be re-sent.
            dedup.insert(id, now);
            info!(event = kind, session, "hook event accepted");
            Status::NoContent
        }
        Err(_) => Status::Unavailable,
    }
}

/// Reads a `POST` with `Content-Length` (no chunked bodies, no keep-alive)
/// and returns its body. The secret is checked before the body is read.
async fn read_request<S: AsyncRead + Unpin>(
    stream: &mut S,
    secret: &Secret,
) -> Result<Vec<u8>, Status> {
    let mut buf = Vec::with_capacity(1024);
    let head_end = loop {
        if let Some(end) = buf.windows(4).position(|window| window == b"\r\n\r\n") {
            break end;
        }
        if buf.len() >= MAX_HEAD {
            return Err(Status::HeaderTooLarge);
        }
        let mut chunk = [0u8; 1024];
        let read = stream
            .read(&mut chunk)
            .await
            .map_err(|_| Status::BadRequest)?;
        if read == 0 {
            return Err(Status::BadRequest);
        }
        buf.extend_from_slice(&chunk[..read]);
    };
    let head = std::str::from_utf8(&buf[..head_end]).map_err(|_| Status::BadRequest)?;
    let mut lines = head.split("\r\n");
    let mut request_line = lines.next().unwrap_or_default().split(' ');
    let (method, target, version) = (
        request_line.next().unwrap_or_default(),
        request_line.next().unwrap_or_default(),
        request_line.next().unwrap_or_default(),
    );
    if request_line.next().is_some() || version != "HTTP/1.1" {
        return Err(Status::BadRequest);
    }
    if method != "POST" {
        return Err(Status::MethodNotAllowed);
    }
    if target != HOOK_PATH {
        return Err(Status::NotFound);
    }

    let mut length = None;
    let mut authorized = false;
    for line in lines {
        let (name, value) = line.split_once(':').ok_or(Status::BadRequest)?;
        if name.is_empty() || !name.bytes().all(is_tchar) {
            return Err(Status::BadRequest);
        }
        // RFC 9110 5.5: a bare CR, LF or NUL in a value must be rejected.
        if value
            .bytes()
            .any(|byte| byte.is_ascii_control() && byte != b'\t')
        {
            return Err(Status::BadRequest);
        }
        let value = value.trim();
        if name.eq_ignore_ascii_case("content-length") {
            // RFC 9112 6.3: a repeated or invalid length is an error.
            if length.is_some() || value.is_empty() || !value.bytes().all(|b| b.is_ascii_digit()) {
                return Err(Status::BadRequest);
            }
            length = Some(value.parse::<usize>().unwrap_or(usize::MAX));
        } else if name.eq_ignore_ascii_case("transfer-encoding") {
            return Err(Status::BadRequest);
        } else if name.eq_ignore_ascii_case("authorization") {
            let token = value
                .split_once(' ')
                .filter(|(scheme, _)| scheme.eq_ignore_ascii_case("bearer"))
                .map(|(_, token)| token.trim());
            authorized = token.is_some_and(|token| secret.matches(token.as_bytes()));
        }
    }
    if !authorized {
        return Err(Status::Unauthorized);
    }
    let length = length.ok_or(Status::LengthRequired)?;
    if length > MAX_HOOK_BODY {
        return Err(Status::PayloadTooLarge);
    }
    let mut body = buf.split_off(head_end + 4);
    if body.len() > length {
        return Err(Status::BadRequest);
    }
    let received = body.len();
    body.resize(length, 0);
    stream
        .read_exact(&mut body[received..])
        .await
        .map_err(|_| Status::BadRequest)?;
    Ok(body)
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
            wire::decode(&self.line)
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
        assert_eq!(peer.recv().await, Ok(HubMsg::Registered));
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
            Some(AgentEvent::Message { conn: from, msg }) => {
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
    async fn an_overlong_line_closes_the_link() {
        let (addr, mut events, _hub) = agents_hub().await;
        let mut peer = Peer::connect(addr).await;
        peer.send(&AgentMsg::Hello { secret: secret() }).await;
        peer.send(&AgentMsg::Register(register())).await;
        assert_eq!(peer.recv().await, Ok(HubMsg::Registered));
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
        assert_eq!(peer.recv().await, Ok(HubMsg::Registered));
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
        let second = post(HookEvent::SessionEnd { reason: None });
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
}
