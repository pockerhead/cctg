import sys; sys.path.insert(0, 'maw/tasks/in_progress/TASK-049/scratch')
from sub import Sub
f = Sub('crates/cctg/src/agent.rs'); rep = f.rep
rep("""//! out after the next registration. A message whose write failed is lost.
""","""//! out after the next registration. A message whose write failed is lost.
//! With a hub that keeps the heartbeat (TASK-049) the agent sends `ping`
//! when it wrote nothing for a while and reconnects when nothing came from
//! the hub for longer: a connection that a NAT dropped on the way never
//! reports an error by itself.
""")
rep("""use crate::wire::{
    self, AgentMsg, Client, CommandOutcome, ConsoleKey, FileChunk, FileKind, FileOutcome, HubMsg,
    Register, Rejection, Secret, SessionAsk, UpdateOutcome, WireError,
};""","""use crate::wire::{
    self, AgentMsg, Beat, Client, CommandOutcome, ConsoleKey, FileChunk, FileKind, FileOutcome,
    Heartbeat, HubMsg, Liveness, Register, Rejection, Secret, SessionAsk, UpdateOutcome, WireError,
};""")
rep("""    /// hook endpoint to send them to after each registration.
    pub replay: Option<Replay>,
}
""","""    /// hook endpoint to send them to after each registration.
    pub replay: Option<Replay>,
    /// Used when `register.heartbeat` is set and the hub keeps one too.
    pub heartbeat: Heartbeat,
}
""")
rep("""        match connect(&config).await {
            Ok((reader, write, files)) => {""","""        match connect(&config).await {
            Ok(Linked {
                reader,
                write,
                files,
                heartbeat,
            }) => {""")
rep("""                let stopped = serve(reader, write, &mut outbox, &events, &mut verdicts).await;""","""                let heartbeat = (config.register.heartbeat && heartbeat).then_some(config.heartbeat);
                let link = Link {
                    reader,
                    write,
                    heartbeat,
                };
                let stopped = serve(link, &mut outbox, &events, &mut verdicts).await;""")
rep("""async fn connect(config: &LinkConfig) -> Result<(LinkRead, LinkWrite, bool), ConnectError> {""","""/// A registered connection and what the hub said it does.
struct Linked {
    reader: LinkRead,
    write: LinkWrite,
    files: bool,
    heartbeat: bool,
}

async fn connect(config: &LinkConfig) -> Result<Linked, ConnectError> {""")
rep("""            HubMsg::Registered { files } => Ok((reader, write, files)),""","""            HubMsg::Registered { files, heartbeat } => Ok(Linked {
                reader,
                write,
                files,
                heartbeat,
            }),""")
rep("""/// Runs one registered link. Returns `true` when the owner is gone (stop),
/// `false` when the link dropped (reconnect). `verdicts`: ids of verdicts
/// already passed on, newest last.
async fn serve(
    reader: LinkRead,
    mut write: LinkWrite,
    outbox: &mut mpsc::Receiver<AgentMsg>,
    events: &mpsc::Sender<LinkEvent>,
    verdicts: &mut VecDeque<u64>,
) -> bool {
    let (frames_tx, mut frames) = mpsc::channel(QUEUE);
    let reader_task = ReadTask::spawn(read_hub_frames(reader, frames_tx));
    let stopped = loop {
        tokio::select! {
            frame = frames.recv() => {
                match frame {
                    Some(Ok(msg)) => {""","""/// One registered connection; `heartbeat` when both ends keep one.
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
                    Some(Ok(msg)) => {""")
rep("""                        let ack = AgentMsg::PermissionAck { verdict_id };
                        if let Err(error) = write_agent_msg(&mut write, &ack).await {
                            debug!(%error, "write to hub failed");
                            break false;
                        }
                    }""","""                        let ack = AgentMsg::PermissionAck { verdict_id };
                        if let Err(error) = write_agent_msg(&mut write, &ack).await {
                            debug!(%error, "write to hub failed");
                            break false;
                        }
                        liveness.said();
                    }""")
rep("""            msg = outbox.recv() => match msg {
                Some(msg) => {
                    if let Err(error) = write_agent_msg(&mut write, &msg).await {
                        debug!(%error, "write to hub failed");
                        break false;
                    }
                }
                None => break true,
            },
        }
    };""","""            msg = outbox.recv() => match msg {
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
    };""")
rep("""                files: true,
                session_reads: true,
            };
            let (outbox, events) = spawn(LinkConfig {""","""                files: true,
                session_reads: true,
                heartbeat: true,
            };
            let (outbox, events) = spawn(LinkConfig {""")
rep("""                    hook_addr: plan.hook,
                }),
            });""","""                    hook_addr: plan.hook,
                }),
                heartbeat: Heartbeat::default(),
            });""")
# tests
rep("""            files: true,
            session_reads: false,
        }
    }

    fn config(addr: SocketAddr, backoff: Backoff) -> LinkConfig {
        LinkConfig {
            addr: HubAddr::plain(addr.to_string()),
            secret: Secret::parse(SECRET).unwrap(),
            register: register(),
            backoff,
            replay: None,
        }
    }""","""            files: true,
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
    }""")
f.save()
