p='crates/cctg/src/agent.rs'
s=open(p,encoding='utf-8').read()
def rep(old,new,count=1):
    global s
    assert s.count(old)==count, (old, s.count(old))
    s=s.replace(old,new)
rep("""//! Session reads (TASK-034):""","""//! Status line numbers (TASK-058): with a hub that tells the agent which
//! session it is bound to (`bound`), the link passes on the numbers `cctg
//! statusline` keeps for that session ([`crate::statusfile`]), looking once
//! a second, and keeps the file's mark fresh so `cctg statusline` does not
//! post them itself.
//!
//! Session reads (TASK-034):""")
rep("""use std::sync::Arc;
use std::time::{Duration, SystemTime};""","""use std::sync::{Arc, Mutex};
use std::time::{Duration, SystemTime};""")
rep("""use crate::spool;
use crate::tail;""","""use crate::spool;
use crate::statusfile;
use crate::tail;""")
rep("""/// Verdict ids remembered for dropping a verdict the hub sent again.
const RECENT_VERDICTS: usize = 256;""","""/// Verdict ids remembered for dropping a verdict the hub sent again.
const RECENT_VERDICTS: usize = 256;
/// How often the link looks at its session's status line numbers.
const STATUS_POLL: Duration = Duration::from_secs(1);""")
rep("""    /// Used when `register.heartbeat` is set and the hub keeps one too.
    pub heartbeat: Heartbeat,
}
""","""    /// Used when `register.heartbeat` is set and the hub keeps one too.
    pub heartbeat: Heartbeat,
    /// Where the status line numbers wait ([`crate::statusfile`]); `None`:
    /// none are passed on. Set together with `register.status_lines`.
    pub status: Option<StatusWatch>,
}

/// The status line numbers the link passes on (TASK-058).
#[derive(Debug, Clone)]
pub struct StatusWatch {
    /// `<state>`, holding `status/`.
    pub state_dir: PathBuf,
    /// The session of the hub's last `bound`, kept across reconnects; its
    /// files go when the agent leaves ([`StatusWatch::clean`]).
    pub bound: Arc<Mutex<Option<String>>>,
}

impl StatusWatch {
    pub fn new(state_dir: PathBuf) -> Self {
        Self {
            state_dir,
            bound: Arc::default(),
        }
    }

    /// The hub bound the link to `session`: the files of the session it was
    /// bound to before (ended by `/clear`) go.
    fn bind(&self, session: &str) {
        let Ok(mut bound) = self.bound.lock() else {
            return;
        };
        if let Some(old) = bound.replace(session.to_owned())
            && old != session
        {
            statusfile::remove(&self.state_dir, &old);
        }
    }

    /// Removes the files of the bound session: the agent leaves with it.
    pub fn clean(&self) {
        let session = self.bound.lock().ok().and_then(|mut bound| bound.take());
        if let Some(session) = session {
            statusfile::remove(&self.state_dir, &session);
        }
    }
}

/// What one connection passes on of its bound session's numbers.
struct StatusState<'a> {
    watch: &'a StatusWatch,
    session: String,
    /// The change time of the numbers last sent on this connection.
    sent: Option<SystemTime>,
    marked: Option<Instant>,
}

impl StatusState<'_> {
    /// The numbers to send when they changed since the last ones sent on
    /// this connection; renews the mark when due. Small local files, read
    /// inline at most once a second.
    fn due(&mut self) -> Option<AgentMsg> {
        if self
            .marked
            .is_none_or(|at| at.elapsed() >= statusfile::MARK_EVERY)
        {
            self.marked = Some(Instant::now());
            if let Err(error) = statusfile::mark(&self.watch.state_dir, &self.session) {
                debug!(kind = ?error.kind(), "status line mark not renewed");
            }
        }
        let changed = statusfile::changed(&self.watch.state_dir, &self.session)?;
        if self.sent == Some(changed) {
            return None;
        }
        let (changed, numbers) = statusfile::read(&self.watch.state_dir, &self.session)?;
        self.sent = Some(changed);
        let HookEvent::StatusLine {
            model,
            effort,
            context,
            five_hour,
            seven_day,
        } = numbers
        else {
            return None;
        };
        Some(AgentMsg::StatusLine {
            session_id: self.session.clone(),
            model,
            effort,
            context,
            five_hour,
            seven_day,
        })
    }
}
""")
rep("""                let stopped = serve(link, &mut outbox, &events, &mut verdicts).await;""","""                let stopped = serve(
                    link,
                    &mut outbox,
                    &events,
                    &mut verdicts,
                    config.status.as_ref(),
                )
                .await;""")
rep("""/// Runs one registered link. Returns `true` when the owner is gone (stop),
/// `false` when the link dropped (reconnect). `verdicts`: ids of verdicts
/// already passed on, newest last. The hub's pings end here and never
/// reach the owner, so a busy owner does not hold them up.
async fn serve(
    link: Link,
    outbox: &mut mpsc::Receiver<AgentMsg>,
    events: &mpsc::Sender<LinkEvent>,
    verdicts: &mut VecDeque<u64>,
) -> bool {""","""/// Runs one registered link. Returns `true` when the owner is gone (stop),
/// `false` when the link dropped (reconnect). `verdicts`: ids of verdicts
/// already passed on, newest last. The hub's pings end here and never
/// reach the owner, so a busy owner does not hold them up; so do its
/// `bound`s, after which the link sends the numbers of `status` itself.
async fn serve(
    link: Link,
    outbox: &mut mpsc::Receiver<AgentMsg>,
    events: &mpsc::Sender<LinkEvent>,
    verdicts: &mut VecDeque<u64>,
    status: Option<&StatusWatch>,
) -> bool {""")
rep("""    let reader_task = ReadTask::spawn(read_hub_frames(reader, frames_tx));
    let stopped = loop {
        let next_beat = liveness.next();
        tokio::select! {
            frame = frames.recv() => {
                if frame.is_some() {
                    liveness.heard();
                }
                match frame {
                    Some(Ok(HubMsg::Ping)) => {}
                    Some(Ok(msg)) => {""","""    let reader_task = ReadTask::spawn(read_hub_frames(reader, frames_tx));
    // Nothing is sent before this connection's `bound`.
    let mut numbers: Option<StatusState> = None;
    let mut poll = tokio::time::interval(STATUS_POLL);
    poll.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    let stopped = loop {
        let next_beat = liveness.next();
        // What to write to the hub now.
        let due = tokio::select! {
            frame = frames.recv() => {
                if frame.is_some() {
                    liveness.heard();
                }
                match frame {
                    Some(Ok(HubMsg::Ping)) => None,
                    Some(Ok(HubMsg::Bound { session_id })) => match status {
                        Some(watch) => {
                            watch.bind(&session_id);
                            let mut state = StatusState {
                                watch,
                                session: session_id,
                                sent: None,
                                marked: None,
                            };
                            let due = state.due();
                            numbers = Some(state);
                            due
                        }
                        None => None,
                    },
                    Some(Ok(msg)) => {""")
rep("""                        let ack = AgentMsg::PermissionAck { verdict_id };
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
    };""","""                        Some(AgentMsg::PermissionAck { verdict_id })
                    }
                    Some(Err(WireError::Version)) => {
                        warn!("hub changed protocol version; reconnecting");
                        break false;
                    }
                    Some(Err(error @ (WireError::Closed | WireError::TooLong | WireError::Io(_)))) => {
                        debug!(%error, "hub link ended");
                        break false;
                    }
                    Some(Err(error)) => {
                        warn!(%error, "hub line ignored");
                        None
                    }
                    None => break false,
                }
            }
            msg = outbox.recv() => match msg {
                Some(msg) => Some(msg),
                None => break true,
            },
            _ = poll.tick(), if numbers.is_some() => numbers.as_mut().and_then(StatusState::due),
            beat = wire::beat(next_beat) => match beat {
                Beat::Ping => Some(AgentMsg::Ping),
                // A frame read meanwhile goes first.
                Beat::Dead if !frames.is_empty() => None,
                Beat::Dead => {
                    info!("hub silent past the heartbeat timeout; reconnecting");
                    break false;
                }
            },
        };
        if let Some(msg) = due {
            if let Err(error) = write_agent_msg(&mut write, &msg).await {
                debug!(%error, "write to hub failed");
                break false;
            }
            liveness.said();
        }
    };""")
# imports: HookEvent
rep("""    Heartbeat, HubMsg, Liveness, Register, Rejection, Secret, SessionAsk, UpdateOutcome, WireError,
};""","""    Heartbeat, HookEvent, HubMsg, Liveness, Register, Rejection, Secret, SessionAsk, UpdateOutcome,
    WireError,
};""")
# run_stdio: register + link config + cleanup
rep("""                session_reads: true,
                status_lines: false,
                heartbeat: true,
            };""","""                session_reads: true,
                status_lines: status.is_some(),
                heartbeat: true,
            };""")
rep("""                heartbeat: Heartbeat::default(),
            });
            (Hub::Link(outbox), Some(events))""","""                heartbeat: Heartbeat::default(),
                status: status.clone(),
            });
            (Hub::Link(outbox), Some(events))""")
rep("""    let (hub, events) = match link_plan(session_id, entrypoint.as_deref(), &config) {""","""    // Status line numbers (TASK-058); files of sessions that ended without
    // their agent go first.
    let status = config.state_dir.clone().map(StatusWatch::new);
    if let Some(state) = config.state_dir.clone() {
        let _ = tokio::task::spawn_blocking(move || statusfile::prune(&state, SystemTime::now()))
            .await;
    }
    let (hub, events) = match link_plan(session_id, entrypoint.as_deref(), &config) {""")
rep("""    let worker = Some(Arc::new(worker));
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
    }""","""    let worker = Some(Arc::new(worker));
    let ended = serve_channel(
        frames,
        tokio::io::stdout(),
        hub,
        events,
        dirs,
        console,
        worker,
    )
    .await;
    // A newer worker takes the session and its numbers over.
    if !matches!(ended, Ok(Ended::Handover))
        && let Some(status) = status
    {
        let _ = tokio::task::spawn_blocking(move || status.clean()).await;
    }
    match ended {
        Ok(Ended::Handover) => shim::HANDOVER,
        Ok(Ended::Input) => 0,
        Err(error) => {
            debug!(kind = ?error.kind(), "stdout closed");
            0
        }
    }""")
open(p,'w',encoding='utf-8',newline='').write(s)
print("ok")
