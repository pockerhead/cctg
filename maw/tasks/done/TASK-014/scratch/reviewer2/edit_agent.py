import sys
p = sys.argv[1]
s = open(p, encoding='utf-8').read()


def rep(old, new, count=1):
    global s
    assert s.count(old) == count, (old, s.count(old))
    s = s.replace(old, new)


rep('''//! accepts. Messages queued while the link is down wait in the outbox and go
//! out after the next registration. A message whose write failed is lost.
''', '''//! accepts. Messages queued while the link is down wait in the outbox and go
//! out after the next registration. A message whose write failed is lost.
//!
//! A permission verdict with a `verdict_id` is acknowledged once it is queued
//! for the channel loop. The hub sends the same verdict again until the ack
//! arrives, so the agent remembers recent ids across reconnects and passes
//! each on only once.
''')
rep('''use std::io::{BufRead, Read, Write};
use std::time::Duration;
''', '''use std::collections::VecDeque;
use std::io::{BufRead, Read, Write};
use std::time::Duration;
''')
rep('''const QUEUE: usize = 256;
''', '''const QUEUE: usize = 256;
/// Verdict ids remembered for dropping a verdict the hub sent again.
const RECENT_VERDICTS: usize = 256;
''')
rep('''    let mut attempt = 0u32;
    let mut last_error = String::new();
    loop {''', '''    let mut attempt = 0u32;
    let mut last_error = String::new();
    let mut verdicts = VecDeque::with_capacity(RECENT_VERDICTS);
    loop {''')
rep('''                let stopped = serve(reader, write, &mut outbox, &events).await;''',
    '''                let stopped = serve(reader, write, &mut outbox, &events, &mut verdicts).await;''')
rep('''/// Runs one registered link. Returns `true` when the owner is gone (stop),
/// `false` when the link dropped (reconnect).
async fn serve(
    reader: BufReader<OwnedReadHalf>,
    mut write: OwnedWriteHalf,
    outbox: &mut mpsc::Receiver<AgentMsg>,
    events: &mpsc::Sender<LinkEvent>,
) -> bool {''', '''/// Runs one registered link. Returns `true` when the owner is gone (stop),
/// `false` when the link dropped (reconnect). `verdicts`: ids of verdicts
/// already passed on, newest last.
async fn serve(
    reader: BufReader<OwnedReadHalf>,
    mut write: OwnedWriteHalf,
    outbox: &mut mpsc::Receiver<AgentMsg>,
    events: &mpsc::Sender<LinkEvent>,
    verdicts: &mut VecDeque<u64>,
) -> bool {''')
rep('''                    Some(Ok(msg)) => {
                        if events.send(LinkEvent::Message(msg)).await.is_err() {
                            break true;
                        }
                    }''', '''                    Some(Ok(msg)) => {
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
                    }''')
rep('''                // Not env `CLAUDE_PID`: in an MCP server it is inherited
                // from an outer claude, or unset (TASK-004).
                claude_pid: proctree::current_lineage(None, None, "").claude_pid,
            };''', '''                // Not env `CLAUDE_PID`: in an MCP server it is inherited
                // from an outer claude, or unset (TASK-004).
                claude_pid: proctree::current_lineage(None, None, "").claude_pid,
                verdict_ack: true,
            };''')
rep('''            cwd: "/w".into(),
            claude_pid: None,
        }
    }''', '''            cwd: "/w".into(),
            claude_pid: None,
            verdict_ack: true,
        }
    }''')
rep('''            .send(HubMsg::PermissionVerdict {
                request_id: "fdqmc".into(),
                behavior: wire::Behavior::Allow,
            })''', '''            .send(HubMsg::PermissionVerdict {
                request_id: "fdqmc".into(),
                behavior: wire::Behavior::Allow,
                verdict_id: None,
            })''')
# new test before closing_stdin test
rep('''    #[tokio::test]
    async fn closing_stdin_ends_the_loop_and_the_link() {''', '''    /// Accepts one agent on `listener` and answers its handshake.
    async fn raw_hub(
        listener: &TcpListener,
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
        wire::write_msg(&mut write, &HubMsg::Registered)
            .await
            .unwrap();
        (reader, write)
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
        assert_eq!(next(&mut events).await, LinkEvent::Up);
        wire::write_msg(&mut write, &verdict(Some(7))).await.unwrap();
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
        assert_eq!(next(&mut events).await, LinkEvent::Up);
        wire::write_msg(&mut write, &verdict(Some(7))).await.unwrap();
        assert_eq!(
            agent_line(&mut reader).await,
            AgentMsg::PermissionAck { verdict_id: 7 }
        );
        // A verdict without an id (a hub before TASK-014) gets no ack; a new
        // id is passed on.
        wire::write_msg(&mut write, &verdict(None)).await.unwrap();
        wire::write_msg(&mut write, &verdict(Some(8))).await.unwrap();
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
    async fn closing_stdin_ends_the_loop_and_the_link() {''')
open(p, 'w', encoding='utf-8', newline='\n').write(s)
print('ok')
