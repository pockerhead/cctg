import sys; sys.path.insert(0, 'maw/tasks/in_progress/TASK-049/scratch')
from sub import Sub
f = Sub('crates/cctg/src/hub/ingress.rs'); rep = f.rep
rep("""//! Nothing here logs message contents, paths or the secret: log lines carry
//! the connection number, the peer address and fixed text only.
""","""//! Nothing here logs message contents, paths or the secret: log lines carry
//! the connection number, the peer address and fixed text only.
//!
//! An agent that registered with `heartbeat` (TASK-049) gets a `ping` when
//! the hub wrote nothing to it for [`Heartbeat::interval`], and is unbound
//! like a closed connection when nothing came from it for
//! [`Heartbeat::timeout`].
""")
rep("""use crate::wire::{
    self, AgentMsg, Behavior, EventId, HOOK_PATH, HookPost, HubMsg, MAX_HOOK_BODY, PERMISSION_PATH,
    PING_PATH, PermissionAnswer, PermissionPost, Register, Rejection, Secret, WireError,
};""","""use crate::wire::{
    self, AgentMsg, Beat, Behavior, EventId, HOOK_PATH, Heartbeat, HookPost, HubMsg, Liveness,
    MAX_HOOK_BODY, PERMISSION_PATH, PING_PATH, PermissionAnswer, PermissionPost, Register,
    Rejection, Secret, WireError,
};""")
rep("""pub async fn serve_agents(
    listener: impl Into<Listener>,
    secret: Secret,
    events: mpsc::Sender<AgentEvent>,
) {
    let Listener {""","""pub async fn serve_agents(
    listener: impl Into<Listener>,
    secret: Secret,
    events: mpsc::Sender<AgentEvent>,
) {
    serve_agents_with(listener, secret, events, Heartbeat::default()).await;
}

/// [`serve_agents`] with another [`Heartbeat`] (tests use short ones).
pub async fn serve_agents_with(
    listener: impl Into<Listener>,
    secret: Secret,
    events: mpsc::Sender<AgentEvent>,
    heartbeat: Heartbeat,
) {
    let Listener {""")
rep("""                        agent_session(stream, peer, conn, &secret, &events, handshake).await;""","""                        agent_session(stream, peer, conn, &secret, &events, handshake, heartbeat)
                            .await;""")
rep("""    events: &mpsc::Sender<AgentEvent>,
    pre_auth: Handshake,
) {
    let (read, mut write) = tokio::io::split(stream);""","""    events: &mpsc::Sender<AgentEvent>,
    pre_auth: Handshake,
    heartbeat: Heartbeat,
) {
    let (read, mut write) = tokio::io::split(stream);""")
rep("""    let session = short(&register.session_id).to_owned();
    let (to_agent, mut outbound) = mpsc::channel(TO_AGENT_QUEUE);""","""    let session = short(&register.session_id).to_owned();
    let mut liveness = Liveness::new(register.heartbeat.then_some(heartbeat));
    let (to_agent, mut outbound) = mpsc::channel(TO_AGENT_QUEUE);""")
rep("""    // This hub takes files from agents (TASK-032).
    if events.send(registered).await.is_err()
        || write_hub_msg(&mut write, &HubMsg::Registered { files: true })
            .await
            .is_err()
    {
        return;
    }
    info!(conn, session, "agent registered");
""","""    // This hub takes files from agents (TASK-032) and keeps a heartbeat
    // with those that want one (TASK-049).
    let answer = HubMsg::Registered {
        files: true,
        heartbeat: true,
    };
    if events.send(registered).await.is_err()
        || write_hub_msg(&mut write, &answer).await.is_err()
    {
        return;
    }
    liveness.said();
    info!(conn, session, "agent registered");
""")
rep("""    loop {
        tokio::select! {
            frame = frames.recv() => {
                match frame {
                    Some((received_at, Ok(""","""    loop {
        let next_beat = liveness.next();
        tokio::select! {
            frame = frames.recv() => {
                if frame.is_some() {
                    liveness.heard();
                }
                match frame {
                    Some((_, Ok(AgentMsg::Ping))) => {}
                    Some((received_at, Ok(""")
rep("""            msg = outbound.recv(), if outbound_open => match msg {
                Some(msg) => {
                    if write_hub_msg(&mut write, &msg).await.is_err() {
                        break;
                    }
                }
                None => outbound_open = false,
            },
        }
    }""","""            msg = outbound.recv(), if outbound_open => match msg {
                Some(msg) => {
                    if write_hub_msg(&mut write, &msg).await.is_err() {
                        break;
                    }
                    liveness.said();
                }
                None => outbound_open = false,
            },
            beat = wire::beat(next_beat) => match beat {
                Beat::Ping => {
                    if write_hub_msg(&mut write, &HubMsg::Ping).await.is_err() {
                        break;
                    }
                    liveness.said();
                }
                // A frame read meanwhile goes first.
                Beat::Dead if !frames.is_empty() => {}
                Beat::Dead => {
                    info!(conn, session, "agent silent past the heartbeat timeout; unbinding");
                    break;
                }
            },
        }
    }""")
f.save()
