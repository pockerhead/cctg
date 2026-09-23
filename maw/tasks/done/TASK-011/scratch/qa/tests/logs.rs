//! QA (TASK-011): log capture. (1) no private text in any slot-actor log line
//! across failure paths; (2) a stopped scheduler is retried on the tick, not
//! in a hot loop. Own binary: global subscriber.

use std::io;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use cctg::hub::api::{ApiError, ForumTopic, Message};
use cctg::hub::ingress::AgentEvent;
use cctg::hub::registry::RegistryStore;
use cctg::hub::scheduler::{BucketConfig, Delivery, Op, Outcome, Scheduler, Transport};
use cctg::hub::slots::{Control, Options, Slots};
use cctg::wire::{AgentMsg, HookEvent, HookPost, PermissionRequest, Register};
use tokio::sync::mpsc;

#[derive(Clone, Default)]
struct Captured(Arc<Mutex<Vec<u8>>>);
impl io::Write for Captured {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        self.0.lock().unwrap().extend_from_slice(buf);
        Ok(buf.len())
    }
    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}
impl Captured {
    fn text(&self) -> String {
        String::from_utf8_lossy(&self.0.lock().unwrap()).into_owned()
    }
}

/// First create fails (network-ish), then works; every edit, send and delete fails.
#[derive(Default)]
struct Flaky {
    creates: Mutex<u32>,
}
impl Transport for Flaky {
    async fn execute(&self, op: &Op) -> Delivery {
        let bad = |d: &str| {
            Err(ApiError::Telegram {
                code: 400,
                description: d.to_owned(),
            })
        };
        match op {
            Op::CreateTopic { name, .. } => {
                let mut n = self.creates.lock().unwrap();
                *n += 1;
                if *n == 1 {
                    return bad("Bad Request: first create refused");
                }
                Ok(Outcome::Topic(ForumTopic {
                    message_thread_id: 100 + i64::from(*n),
                    name: name.clone(),
                    icon_custom_emoji_id: None,
                }))
            }
            Op::EditTopic { .. } => bad("Bad Request: edit refused"),
            Op::Send { .. } => bad("Bad Request: send refused"),
            Op::Delete { .. } => bad("Bad Request: message can't be deleted"),
            _ => Ok(Outcome::Sent(Message::default())),
        }
    }
}

fn post(host: &str, session: &str, cwd: &str, path: &str, event: HookEvent) -> HookPost {
    HookPost::new(host.into(), session.into(), cwd.into(), path.into(), event)
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn logs_are_private_and_scheduler_loss_is_not_a_hot_loop() {
    let captured = Captured::default();
    let w = captured.clone();
    tracing::subscriber::set_global_default(
        tracing_subscriber::fmt()
            .without_time()
            .with_max_level(tracing::Level::TRACE)
            .with_writer(move || w.clone())
            .finish(),
    )
    .unwrap();

    // ---- part 1: privacy ----
    let state = std::env::temp_dir().join(format!("qa011-logs-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&state);
    std::fs::create_dir_all(&state).unwrap();
    let host = "PRIVHOSTzz";
    let cwd = r"C:\Users\PRIVUSERzz\PRIVFOLDERzz";
    let transcript = state.join("PRIVPATHzz.jsonl");
    std::fs::write(
        &transcript,
        "{\"type\":\"ai-title\",\"aiTitle\":\"PRIVTITLEzz\"}\n",
    )
    .unwrap();
    let tpath = transcript.to_str().unwrap().to_owned();
    let a = "aaaaaaaa-PRIVTAIL-4000-8000-000000000001";
    let b = "bbbbbbbb-PRIVTAIL-4000-8000-000000000002";
    // Make the save fail: the temp name is a directory.
    std::fs::create_dir_all(state.join("registry.json.tmp")).unwrap();

    let fake = Arc::new(Flaky::default());
    let store = RegistryStore::open(&state).unwrap();
    let registry = store.load().unwrap();
    let fast = BucketConfig {
        capacity: 1000,
        refill_every: Duration::from_millis(1),
        min_gap: Duration::ZERO,
    };
    let (scheduler, outbox) = Scheduler::new(fake.clone(), fast);
    tokio::spawn(scheduler.run());
    let options = Options {
        grace: Duration::ZERO,
        retry_every: Duration::from_millis(200),
        ..Options::default()
    };
    let (slots, _view) = Slots::new(registry, store, outbox, options);
    let (agents, agents_rx) = mpsc::channel(16);
    let (hooks, hooks_rx) = mpsc::channel(16);
    let (control, control_rx) = mpsc::unbounded_channel();
    tokio::spawn(slots.run(agents_rx, hooks_rx, control_rx));

    let (to_agent, _keep) = mpsc::channel(4);
    agents
        .send(AgentEvent::Registered {
            conn: 1,
            register: Register {
                session_id: a.into(),
                host: host.into(),
                cwd: cwd.into(),
            },
            to_agent,
        })
        .await
        .unwrap();
    for ev in [
        HookEvent::SessionStart {
            source: Some("startup".into()),
            claude_pid: Some(7),
            parent_claude_pid: None,
        },
        HookEvent::UserPromptSubmit { prompt_id: None },
        HookEvent::SubagentStart {
            agent_id: "PRIVAGENTzz".into(),
            agent_type: "Explore".into(),
        },
        HookEvent::Stop {
            prompt_id: None,
            last_assistant_message: Some("PRIVMSGzz".into()),
        },
    ] {
        hooks.send(post(host, a, cwd, &tpath, ev)).await.unwrap();
    }
    agents
        .send(AgentEvent::Message {
            conn: 1,
            msg: AgentMsg::PermissionRequest(PermissionRequest {
                request_id: "abcde".into(),
                tool_name: "Bash".into(),
                description: "PRIVDESCzz".into(),
                input_preview: "PRIVPREVzz".into(),
            }),
        })
        .await
        .unwrap();
    tokio::time::sleep(Duration::from_millis(600)).await;
    hooks
        .send(post(host, a, cwd, &tpath, HookEvent::SessionEnd { reason: Some("clear".into()), claude_pid: None }))
        .await
        .unwrap();
    hooks
        .send(post(
            host,
            b,
            cwd,
            &tpath,
            HookEvent::SessionStart {
                source: Some("clear".into()),
                claude_pid: Some(7),
                parent_claude_pid: None,
            },
        ))
        .await
        .unwrap();
    for t in 100..104 {
        control
            .send(Control::TopicEdited {
                thread_id: Some(t),
                message_id: t * 10,
            })
            .unwrap();
    }
    tokio::time::sleep(Duration::from_millis(1500)).await;
    let text = captured.text();
    for needle in [
        "PRIVHOST", "PRIVUSER", "PRIVFOLDER", "PRIVPATH", "PRIVTITLE", "PRIVTAIL", "PRIVAGENT",
        "PRIVMSG", "PRIVDESC", "PRIVPREV", "Users",
    ] {
        assert!(!text.contains(needle), "{needle} leaked:\n{text}");
    }
    eprintln!("---- part1 log ----\n{text}");
    let part1_len = text.len();

    // ---- part 2: scheduler gone ----
    let state2 = std::env::temp_dir().join(format!("qa011-logs2-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&state2);
    let store2 = RegistryStore::open(&state2).unwrap();
    let (scheduler2, outbox2) = Scheduler::new(Arc::new(Flaky::default()), fast);
    drop(scheduler2);
    let options2 = Options {
        grace: Duration::ZERO,
        retry_every: Duration::from_millis(300),
        ..Options::default()
    };
    let (slots2, _v2) = Slots::new(store2.load().unwrap(), store2, outbox2, options2);
    let (_agents2, agents_rx2) = mpsc::channel(16);
    let (hooks2, hooks_rx2) = mpsc::channel(16);
    let (_control2, control_rx2) = mpsc::unbounded_channel();
    tokio::spawn(slots2.run(agents_rx2, hooks_rx2, control_rx2));
    hooks2
        .send(post(
            "h2",
            "cccccccc-0000-4000-8000-000000000003",
            r"C:\x",
            "",
            HookEvent::SessionStart {
                source: Some("startup".into()),
                claude_pid: Some(9),
                parent_claude_pid: None,
            },
        ))
        .await
        .unwrap();
    tokio::time::sleep(Duration::from_millis(2000)).await;
    let text = captured.text();
    let part2 = &text[part1_len..];
    let attempts = part2.matches("topic call got no answer").count();
    eprintln!("scheduler-gone attempts in 2 s with retry_every 300 ms: {attempts}");
    assert!(
        (3..=10).contains(&attempts),
        "attempts {attempts}: expected ~7 (tick-driven), not a hot loop"
    );
}
