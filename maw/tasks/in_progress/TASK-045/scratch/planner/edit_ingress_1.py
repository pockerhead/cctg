import sys
p = sys.argv[1] + '/crates/cctg/src/hub/ingress.rs'
s = open(p, encoding='utf-8').read()


def rep(old, new, count=1):
    global s
    n = s.count(old)
    assert n == count, (old[:80], n)
    s = s.replace(old, new)


rep("""//! Both check the shared secret before anything else and hand what they
//! accept to the hub over bounded channels ([`AgentEvent`], [`HookPost`]).""",
"""//! Both check the secret before anything else ([`Devices`]: the shared
//! secret while it is on, and the secrets of enrolled devices, TASK-045)
//! and hand what they accept to the hub over bounded channels
//! ([`AgentEvent`], [`HookPost`]). A revoked device loses its agent links
//! and its waiting hooks at once, its other hooks from the next request on.
//! `POST /v1/join` is the one request without a secret: its join code is
//! the credential.""")
rep("""use crate::tls::{Acceptor, Incoming, ReadTask, Stream};
use crate::wire::{
    self, AgentMsg, Beat, Behavior, EventId, HOOK_PATH, Heartbeat, HookPost, HubMsg, Liveness,
    MAX_HOOK_BODY, PERMISSION_PATH, PING_PATH, PermissionAnswer, PermissionPost, QUESTION_PATH,
    QuestionAnswer, QuestionPost, Register, Rejection, Secret, WireError,
};""",
"""use super::devices::{Devices, JoinError, Who};
use crate::tls::{Acceptor, Incoming, ReadTask, Stream};
use crate::wire::{
    self, AgentMsg, Beat, Behavior, EventId, HOOK_PATH, Heartbeat, HookPost, HubMsg, JOIN_PATH,
    JoinAnswer, Liveness, MAX_HOOK_BODY, MAX_JOIN_BODY, PERMISSION_PATH, PING_PATH,
    PermissionAnswer, PermissionPost, QUESTION_PATH, QuestionAnswer, QuestionPost, Register,
    Rejection, WireError,
};""")
rep("use tokio::sync::{OwnedSemaphorePermit, Semaphore, mpsc, oneshot};",
    "use tokio::sync::{OwnedSemaphorePermit, Semaphore, mpsc, oneshot, watch};")
rep("""/// A wrong secret is answered only after this pause (both listeners).""",
"""/// A wrong secret or join code is answered only after this pause (both
/// listeners).""")
rep("""pub async fn serve_agents(
    listener: impl Into<Listener>,
    secret: Secret,
    events: mpsc::Sender<AgentEvent>,
) {
    serve_agents_with(listener, secret, events, Heartbeat::default()).await;
}""", """pub async fn serve_agents(
    listener: impl Into<Listener>,
    auth: impl Into<Devices>,
    events: mpsc::Sender<AgentEvent>,
) {
    serve_agents_with(listener, auth, events, Heartbeat::default()).await;
}""")
rep("""pub async fn serve_agents_with(
    listener: impl Into<Listener>,
    secret: Secret,
    events: mpsc::Sender<AgentEvent>,
    heartbeat: Heartbeat,
) {
    let Listener {
        tcp: listener,
        incoming,
    } = listener.into();
    let secret = Arc::new(secret);""", """pub async fn serve_agents_with(
    listener: impl Into<Listener>,
    auth: impl Into<Devices>,
    events: mpsc::Sender<AgentEvent>,
    heartbeat: Heartbeat,
) {
    let Listener {
        tcp: listener,
        incoming,
    } = listener.into();
    let devices = auth.into();""")
rep("""                let (secret, events, incoming, gate) =
                    (secret.clone(), events.clone(), incoming.clone(), gate.clone());""",
"""                let (devices, events, incoming, gate) =
                    (devices.clone(), events.clone(), incoming.clone(), gate.clone());""")
rep("""                        agent_session(stream, peer, conn, &secret, &events, handshake, heartbeat)
                            .await;""", """                        agent_session(stream, peer, conn, &devices, &events, handshake, heartbeat)
                            .await;""")
rep("""async fn agent_session(
    stream: Stream,
    peer: SocketAddr,
    conn: u64,
    secret: &Secret,
    events: &mpsc::Sender<AgentEvent>,
    pre_auth: Handshake,
    heartbeat: Heartbeat,
) {
    let (read, mut write) = tokio::io::split(stream);
    let mut reader = BufReader::new(read);
    let mut line = Vec::new();
""", """async fn agent_session(
    stream: Stream,
    peer: SocketAddr,
    conn: u64,
    devices: &Devices,
    events: &mpsc::Sender<AgentEvent>,
    pre_auth: Handshake,
    heartbeat: Heartbeat,
) {
    let (read, mut write) = tokio::io::split(stream);
    let mut reader = BufReader::new(read);
    let mut line = Vec::new();
    // Before the check: a revoke right after it is still seen.
    let mut changes = devices.subscribe();
""")
rep("""        let authed = match wire::decode::<AgentMsg>(&line) {
            Ok(AgentMsg::Hello { secret: offered }) => secret.matches(offered.expose().as_bytes()),
            Err(WireError::Version) => return Err(Rejection::Version),
            _ => false,
        };
        if !authed {
            return Err(Rejection::Auth);
        }""", """        let who = match wire::decode::<AgentMsg>(&line) {
            Ok(AgentMsg::Hello { secret: offered }) => devices.check(offered.expose().as_bytes()),
            Err(WireError::Version) => return Err(Rejection::Version),
            _ => None,
        };
        let Some(who) = who else {
            return Err(Rejection::Auth);
        };""")
rep("""        let result = match wire::decode::<AgentMsg>(&line) {
            Ok(AgentMsg::Register(register)) => Ok(Some(register)),""", """        let result = match wire::decode::<AgentMsg>(&line) {
            Ok(AgentMsg::Register(register)) => Ok(Some((who, register))),""")
rep("""    let register = match handshake {
        Ok(Ok(Some(register))) => register,""", """    let (who, register) = match handshake {
        Ok(Ok(Some(registered))) => registered,""")
rep("""                Beat::Dead => {
                    info!(conn, session, "agent silent past the heartbeat timeout; unbinding");
                    break;
                }
            },
        }
    }
    reader_task.stop().await;""", """                Beat::Dead => {
                    info!(conn, session, "agent silent past the heartbeat timeout; unbinding");
                    break;
                }
            },
            () = revoked(&mut changes, devices, &who) => {
                info!(conn, session, "agent of a revoked device; link closed");
                break;
            }
        }
    }
    reader_task.stop().await;""")
rep("""/// First 8 characters: enough to recognise a session in logs.""", """/// Completes when `who` no longer gets in (its device was revoked); never
/// for the shared secret.
async fn revoked(changes: &mut watch::Receiver<u64>, devices: &Devices, who: &Who) {
    loop {
        if changes.changed().await.is_err() {
            return std::future::pending().await;
        }
        if !devices.is_active(who) {
            return;
        }
    }
}

/// First 8 characters: enough to recognise a session in logs.""")
rep("""    Unauthorized,
    NotFound,
    MethodNotAllowed,""", """    Unauthorized,
    Forbidden,
    NotFound,
    MethodNotAllowed,""")
rep("""            Self::Unauthorized => 401,
""", """            Self::Unauthorized => 401,
            Self::Forbidden => 403,
""")
rep("""            Self::Unauthorized => "Unauthorized",
""", """            Self::Unauthorized => "Unauthorized",
            Self::Forbidden => "Forbidden",
""")
rep("""pub async fn serve_hooks(
    listener: impl Into<Listener>,
    secret: Secret,
    events: mpsc::Sender<HookPost>,
) {
    serve(listener.into(), secret, events, None).await;
}""", """pub async fn serve_hooks(
    listener: impl Into<Listener>,
    auth: impl Into<Devices>,
    events: mpsc::Sender<HookPost>,
) {
    serve(listener.into(), auth.into(), events, None).await;
}""")
rep("""pub async fn serve_hooks_and_permissions(
    listener: impl Into<Listener>,
    secret: Secret,""", """pub async fn serve_hooks_and_permissions(
    listener: impl Into<Listener>,
    auth: impl Into<Devices>,""")
rep("""    serve(listener.into(), secret, events, Some(waits)).await;""",
    """    serve(listener.into(), auth.into(), events, Some(waits)).await;""", 2)
rep("""pub async fn serve_hooks_and_asks(
    listener: impl Into<Listener>,
    secret: Secret,""", """pub async fn serve_hooks_and_asks(
    listener: impl Into<Listener>,
    auth: impl Into<Devices>,""")
rep("""async fn serve(
    listener: Listener,
    secret: Secret,
    events: mpsc::Sender<HookPost>,
    waits: Option<PermissionWaits>,
) {
    let Listener {
        tcp: listener,
        incoming,
    } = listener;
    let secret = Arc::new(secret);""", """async fn serve(
    listener: Listener,
    devices: Devices,
    events: mpsc::Sender<HookPost>,
    waits: Option<PermissionWaits>,
) {
    let Listener {
        tcp: listener,
        incoming,
    } = listener;""")
rep("""                let (secret, dedup, events, waits, incoming) = (
                    secret.clone(),""", """                let (devices, dedup, events, waits, incoming) = (
                    devices.clone(),""")
rep("""                    hook_request(
                        stream, peer, &secret, &dedup, &events, waits, permit, pre_auth, deadline,
                    )
                    .await;""", """                    hook_request(
                        stream, peer, &devices, &dedup, &events, waits, permit, pre_auth, deadline,
                    )
                    .await;""")

# hook_request: who, join, revocation of waiting hooks
rep("""async fn hook_request(
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
        Ok(Ok((Route::Question, body))) => match waits {
            Some(waits) if waits.questions.is_some() => {
                drop(permit);
                return question_request(stream, peer, &body, waits).await;
            }
            _ => Status::NotFound,
        },""", """async fn hook_request(
    mut stream: Stream,
    peer: SocketAddr,
    devices: &Devices,
    dedup: &Mutex<Dedup>,
    events: &mpsc::Sender<HookPost>,
    waits: Option<&PermissionWaits>,
    permit: OwnedSemaphorePermit,
    pre_auth: PreAuth,
    deadline: tokio::time::Instant,
) {
    // Before the check: a revoke while a hook waits is seen.
    let mut changes = devices.subscribe();
    let read = tokio::time::timeout_at(deadline, read_request(&mut stream, devices)).await;
    drop(pre_auth.peer_place);
    let gate = pre_auth.gate;
    let status = match read {
        Ok(Ok((Route::Hook, body, _))) => accept_hook(&body, dedup, events),
        // `cctg doctor`: the secret matched; nothing else happens.
        Ok(Ok((Route::Ping, _, _))) => {
            debug!(%peer, "ping answered");
            Status::NoContent
        }
        Ok(Ok((Route::Join, body, _))) => {
            drop(permit);
            return join_request(stream, peer, &body, devices, &gate).await;
        }
        Ok(Err(None)) => {
            debug!(%peer, "connection closed before its first byte");
            return;
        }
        Ok(Ok((Route::Permission, body, who))) => match (waits, who) {
            (Some(waits), Some(who)) => {
                // A waiting hook takes one of its own places, not a
                // hook request place.
                drop(permit);
                let revoked = revoked(&mut changes, devices, &who);
                return permission_request(stream, peer, &body, waits, revoked).await;
            }
            _ => Status::NotFound,
        },
        Ok(Ok((Route::Question, body, who))) => match (waits, who) {
            (Some(waits), Some(who)) if waits.questions.is_some() => {
                drop(permit);
                let revoked = revoked(&mut changes, devices, &who);
                return question_request(stream, peer, &body, waits, revoked).await;
            }
            _ => Status::NotFound,
        },""")
rep("""async fn permission_request(
    mut stream: Stream,
    peer: SocketAddr,
    body: &[u8],
    waits: &PermissionWaits,
) {""", """async fn permission_request(
    mut stream: Stream,
    peer: SocketAddr,
    body: &[u8],
    waits: &PermissionWaits,
    revoked: impl Future<Output = ()>,
) {""")
rep("""    let Some(behavior) = wait_for(&mut stream, decided, PERMISSION_WAIT_CAP).await else {
        debug!(session, "permission hook went away before an answer");
        return;
    };""", """    let Some(behavior) = wait_for(&mut stream, decided, PERMISSION_WAIT_CAP, revoked).await else {
        debug!(session, "permission hook went away before an answer");
        return;
    };""")
rep("""async fn question_request(
    mut stream: Stream,
    peer: SocketAddr,
    body: &[u8],
    waits: &PermissionWaits,
) {""", """async fn question_request(
    mut stream: Stream,
    peer: SocketAddr,
    body: &[u8],
    waits: &PermissionWaits,
    revoked: impl Future<Output = ()>,
) {""")
rep("""    let Some(answers) = wait_for(&mut stream, decided, QUESTION_WAIT_CAP).await else {""",
    """    let Some(answers) = wait_for(&mut stream, decided, QUESTION_WAIT_CAP, revoked).await else {""")
rep("""/// The hub's answer on `decided`, at most `cap` later. `None`: the hook went
/// away first; `Some(None)`: no decision (none, dropped, or time ran out).
async fn wait_for<T>(
    stream: &mut Stream,
    decided: oneshot::Receiver<Option<T>>,
    cap: Duration,
) -> Option<Option<T>> {
    tokio::select! {
        decided = decided => Some(decided.ok().flatten()),
        () = gone(stream) => None,
        () = tokio::time::sleep(cap) => Some(None),
    }
}""", """/// The hub's answer on `decided`, at most `cap` later. `None`: the hook went
/// away first, or its device was revoked (it then gets no answer at all);
/// `Some(None)`: no decision (none, dropped, or time ran out).
async fn wait_for<T>(
    stream: &mut Stream,
    decided: oneshot::Receiver<Option<T>>,
    cap: Duration,
    revoked: impl Future<Output = ()>,
) -> Option<Option<T>> {
    tokio::select! {
        decided = decided => Some(decided.ok().flatten()),
        () = gone(stream) => None,
        () = revoked => {
            info!("hook of a revoked device dropped while it waited");
            None
        }
        () = tokio::time::sleep(cap) => Some(None),
    }
}""")
# join_request after gone()
rep("""fn accept_hook(body: &[u8], dedup: &Mutex<Dedup>, events: &mpsc::Sender<HookPost>) -> Status {""",
"""/// `POST /v1/join` (TASK-045): spends the code and answers the new
/// device's secret. A refused code gets the same `403` whether it was
/// unknown, used or expired, after the pause of a wrong secret. Neither the
/// code nor the secret is logged.
async fn join_request(
    mut stream: Stream,
    peer: SocketAddr,
    body: &[u8],
    devices: &Devices,
    gate: &WarnGate,
) {
    let post = match wire::decode_join(body) {
        Ok(post) => post,
        Err(error) => {
            pre_auth!(gate, peer, %peer, %error, "join request rejected");
            return respond(&mut stream, Status::BadRequest, &[]).await;
        }
    };
    let joining = devices.clone();
    let joined =
        tokio::task::spawn_blocking(move || joining.join(&post.code, &post.name)).await;
    match joined {
        Ok(Ok(enrolled)) => {
            info!(%peer, device = enrolled.id, "device enrolled with a join code");
            let answer = JoinAnswer {
                device_id: enrolled.id,
                name: enrolled.name,
                secret: enrolled.secret,
            };
            let body = serde_json::to_vec(&answer).expect("a join answer always serializes");
            respond(&mut stream, Status::Ok, &body).await;
        }
        Ok(Err(JoinError::Refused)) => {
            pre_auth!(gate, peer, %peer, "join code refused");
            // No fast guessing, as for a wrong secret.
            tokio::time::sleep(AUTH_FAIL_DELAY).await;
            respond(&mut stream, Status::Forbidden, &[]).await;
        }
        Ok(Err(error)) => {
            warn!(%peer, %error, "join request not served");
            respond(&mut stream, Status::Unavailable, &[]).await;
        }
        Err(_) => {
            warn!(%peer, "join request failed");
            respond(&mut stream, Status::Unavailable, &[]).await;
        }
    }
}

fn accept_hook(body: &[u8], dedup: &Mutex<Dedup>, events: &mpsc::Sender<HookPost>) -> Status {""")
rep("""enum Route {
    Hook,
    Permission,
    Question,
    Ping,
}""", """enum Route {
    Hook,
    Permission,
    Question,
    Ping,
    /// The one route without a secret (TASK-045).
    Join,
}""")
rep("""/// Reads a `POST` with `Content-Length` (no chunked bodies, no keep-alive)
/// and returns its target and body. The secret is checked before the body
/// is read. `Err(None)`: closed before its first byte, nothing to answer.
async fn read_request<S: AsyncRead + Unpin>(
    stream: &mut S,
    secret: &Secret,
) -> Result<(Route, Vec<u8>), Option<Status>> {""", """/// Reads a `POST` with `Content-Length` (no chunked bodies, no keep-alive)
/// and returns its target, body and whose secret it carried. The secret is
/// checked before the body is read; a join request has none (`None`) and a
/// body of at most [`MAX_JOIN_BODY`]. `Err(None)`: closed before its first
/// byte, nothing to answer.
async fn read_request<S: AsyncRead + Unpin>(
    stream: &mut S,
    devices: &Devices,
) -> Result<(Route, Vec<u8>, Option<Who>), Option<Status>> {""")
rep("""        PING_PATH => Route::Ping,
        _ => return Err(Some(Status::NotFound)),""", """        PING_PATH => Route::Ping,
        JOIN_PATH => Route::Join,
        _ => return Err(Some(Status::NotFound)),""")
rep("""            let token = value
                .split_once(' ')
                .filter(|(scheme, _)| scheme.eq_ignore_ascii_case("bearer"))
                .map(|(_, token)| token.trim_matches([' ', '\\t']));
            authorized = Some(token.is_some_and(|token| secret.matches(token.as_bytes())));
        }
    }
    if authorized != Some(true) {
        return Err(Some(Status::Unauthorized));
    }
    let length = length.ok_or(Some(Status::LengthRequired))?;
    if length > MAX_HOOK_BODY {
        return Err(Some(Status::PayloadTooLarge));
    }""", """            // A join request needs none: its header is not looked at.
            let token = value
                .split_once(' ')
                .filter(|(scheme, _)| scheme.eq_ignore_ascii_case("bearer"))
                .map(|(_, token)| token.trim_matches([' ', '\\t']));
            authorized = Some(
                token
                    .filter(|_| route != Route::Join)
                    .and_then(|token| devices.check(token.as_bytes())),
            );
        }
    }
    let who = authorized.flatten();
    if route != Route::Join && who.is_none() {
        return Err(Some(Status::Unauthorized));
    }
    let length = length.ok_or(Some(Status::LengthRequired))?;
    let max = if route == Route::Join {
        MAX_JOIN_BODY
    } else {
        MAX_HOOK_BODY
    };
    if length > max {
        return Err(Some(Status::PayloadTooLarge));
    }""")
rep("""        .read_exact(&mut body[received..])
        .await
        .map_err(|_| Some(Status::BadRequest))?;
    Ok((route, body))
}""", """        .read_exact(&mut body[received..])
        .await
        .map_err(|_| Some(Status::BadRequest))?;
    Ok((route, body, who))
}""")
open(p, 'w', encoding='utf-8', newline='\n').write(s)
print("ok")
