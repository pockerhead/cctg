//! `cctg hook PermissionRequest` as Claude Code runs it (a process with JSON
//! on stdin, configured through `<home>/.cctg/device.env`) against the real
//! `serve_hooks_and_permissions`, slot actor and scheduler with a fake
//! Telegram: a press in the topic comes back as the hook's decision JSON; a
//! request the channel relays, a session end and a stopped hub give no
//! decision, exit 0 and an empty stdout.

use std::io::Write;
use std::net::{Ipv4Addr, SocketAddr};
use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use cctg::hub::api::{ForumTopic, Message};
use cctg::hub::ingress::{self, AgentEvent};
use cctg::hub::registry::RegistryStore;
use cctg::hub::scheduler::{BucketConfig, Delivery, Op, Outcome, Scheduler, Transport};
use cctg::hub::slots::{Control, Options, Slots};
use cctg::hub::updates::CallbackInput;
use cctg::wire::{AgentMsg, HookEvent, HookPost, HubMsg, PermissionRequest, Register, Secret};
use serde_json::Value;
use tokio::sync::mpsc;

const SECRET: &str = "permission-hook-secret-0123456789";
const SESSION: &str = "5e551017-0000-4000-8000-00000000c0de";
const COMMAND: &str = "rm -rf \"$UNSET_PRIVATE_DIR\"/";

/// Topic 100; sent messages get ids from 1000 up.
#[derive(Default)]
struct Fake {
    ops: Mutex<Vec<Op>>,
    next: Mutex<i64>,
}

impl Fake {
    fn ops(&self) -> Vec<Op> {
        self.ops.lock().unwrap().clone()
    }

    /// (message id, request id) of every permission prompt sent.
    fn prompts(&self) -> Vec<(i64, String)> {
        let mut message_id = 1000;
        let mut found = Vec::new();
        for op in self.ops() {
            if let Op::Send {
                permission,
                reply_markup,
                ..
            } = &op
            {
                if *permission {
                    let data =
                        reply_markup.as_ref().unwrap()["inline_keyboard"][0][0]["callback_data"]
                            .as_str()
                            .unwrap()
                            .to_owned();
                    let id = data.strip_prefix("allow:").unwrap().to_owned();
                    found.push((message_id, id));
                }
                message_id += 1;
            }
        }
        found
    }

    fn edits_of(&self, message: i64) -> Vec<String> {
        self.ops()
            .into_iter()
            .filter_map(|op| match op {
                Op::Edit {
                    message_id, text, ..
                } if message_id == message => Some(text),
                _ => None,
            })
            .collect()
    }
}

impl Transport for Fake {
    async fn execute(&self, op: &Op) -> Delivery {
        self.ops.lock().unwrap().push(op.clone());
        match op {
            Op::CreateTopic { name, .. } => Ok(Outcome::Topic(ForumTopic {
                message_thread_id: 100,
                name: name.clone(),
                icon_custom_emoji_id: None,
            })),
            Op::Send { .. } | Op::SendDocument { .. } => {
                let mut next = self.next.lock().unwrap();
                *next = (*next).max(1000);
                let message_id = *next;
                *next += 1;
                Ok(Outcome::Sent(Message {
                    message_id,
                    ..Message::default()
                }))
            }
            _ => Ok(Outcome::Done),
        }
    }
}

struct Hub {
    addr: String,
    fake: Arc<Fake>,
    hooks: mpsc::Sender<HookPost>,
    agents: mpsc::Sender<AgentEvent>,
    control: mpsc::UnboundedSender<Control>,
    _to_agent: mpsc::Receiver<HubMsg>,
    _state: TempState,
}

struct TempState(PathBuf);

impl Drop for TempState {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

async fn until(what: &str, mut ready: impl FnMut() -> bool) {
    let wait = async {
        while !ready() {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    };
    tokio::time::timeout(Duration::from_secs(30), wait)
        .await
        .unwrap_or_else(|_| panic!("{what} in time"));
}

/// A hub with one live session (topic 100) whose agent is linked.
async fn hub(test: &str) -> Hub {
    let state = std::env::temp_dir().join(format!(
        "cctg-permission-hook-{test}-{}",
        std::process::id()
    ));
    let _ = std::fs::remove_dir_all(&state);
    std::fs::create_dir_all(&state).unwrap();
    let fake = Arc::new(Fake::default());
    let (scheduler, outbox) = Scheduler::new(
        fake.clone(),
        BucketConfig {
            capacity: 100,
            refill_every: Duration::from_millis(10),
            min_gap: Duration::ZERO,
        },
    );
    tokio::spawn(scheduler.run());
    let store = RegistryStore::open(&state).unwrap();
    let options = Options {
        grace: Duration::ZERO,
        chat_id: -1000000000001,
        ..Options::default()
    };
    let (mut slots, _view) = Slots::new(store.load().unwrap(), store, outbox, options);
    let asks = slots.permission_asks();
    let (agents, agents_rx) = mpsc::channel(16);
    let (hooks, hooks_rx) = mpsc::channel(16);
    let (control, control_rx) = mpsc::unbounded_channel();
    tokio::spawn(slots.run(agents_rx, hooks_rx, control_rx));
    let (hook_tx, mut hook_rx) = mpsc::channel(16);
    let listener = ingress::bind(SocketAddr::from((Ipv4Addr::LOCALHOST, 0)))
        .await
        .unwrap();
    let addr = listener.local_addr().unwrap().to_string();
    tokio::spawn(ingress::serve_hooks_and_permissions(
        listener,
        Secret::parse(SECRET).unwrap(),
        hook_tx,
        asks,
    ));
    // Hook posts over HTTP go on to the actor.
    let forward = hooks.clone();
    tokio::spawn(async move {
        while let Some(post) = hook_rx.recv().await {
            let _ = forward.send(post).await;
        }
    });
    hooks
        .send(HookPost::new(
            "box".into(),
            SESSION.into(),
            r"C:\w\p".into(),
            String::new(),
            HookEvent::SessionStart {
                source: Some("startup".into()),
                claude_pid: Some(10),
                parent_claude_pid: None,
            },
        ))
        .await
        .unwrap();
    until("topic", || {
        fake.ops()
            .iter()
            .any(|op| matches!(op, Op::CreateTopic { .. }))
    })
    .await;
    let (to_agent, to_agent_rx) = mpsc::channel(8);
    agents
        .send(AgentEvent::Registered {
            conn: 1,
            register: Register {
                session_id: SESSION.into(),
                host: "box".into(),
                cwd: r"C:\w\p".into(),
                claude_pid: Some(10),
                verdict_ack: true,
                transcript_reads: false,
                console_keys: false,
            },
            to_agent,
        })
        .await
        .unwrap();
    Hub {
        addr,
        fake,
        hooks,
        agents,
        control,
        _to_agent: to_agent_rx,
        _state: TempState(state),
    }
}

/// A home directory whose `.cctg/device.env` points at `addr`.
fn home(test: &str, addr: &str) -> PathBuf {
    let home = Path::new(env!("CARGO_TARGET_TMPDIR")).join(format!("permission-hook-{test}"));
    let dir = home.join(".cctg");
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(
        dir.join("device.env"),
        format!("CCTG_HUB_SECRET={SECRET}\nCCTG_HUB_HOOK_ADDR={addr}\nCCTG_HOST=box\n"),
    )
    .unwrap();
    home
}

fn input() -> Vec<u8> {
    serde_json::json!({
        "session_id": SESSION,
        "transcript_path": "/p/s.jsonl",
        "cwd": r"C:\w\p",
        "hook_event_name": "PermissionRequest",
        "permission_mode": "auto",
        "tool_name": "Bash",
        "tool_input": { "command": COMMAND, "description": "Clean the build folder" },
        "permission_suggestions": [],
    })
    .to_string()
    .into_bytes()
}

/// Runs the real hook binary on a blocking thread.
async fn run_hook(home: PathBuf) -> (Output, Duration) {
    tokio::task::spawn_blocking(move || {
        let started = Instant::now();
        let mut child = Command::new(env!("CARGO_BIN_EXE_cctg"))
            .args(["hook", "PermissionRequest"])
            .env("USERPROFILE", &home)
            .env("HOME", &home)
            .env("RUST_LOG", "trace")
            .env_remove("CCTG_HUB_SECRET")
            .env_remove("CCTG_HUB_HOOK_ADDR")
            .env_remove("CCTG_HOST")
            .env_remove("CCTG_STATE_DIR")
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .expect("cctg starts");
        let mut stdin = child.stdin.take().unwrap();
        let _ = stdin.write_all(&input());
        drop(stdin);
        let output = child.wait_with_output().unwrap();
        (output, started.elapsed())
    })
    .await
    .unwrap()
}

fn assert_clean(output: &Output) {
    assert!(output.status.success(), "{:?}", output.status);
    let stderr = String::from_utf8_lossy(&output.stderr);
    for private in [SECRET, "UNSET_PRIVATE_DIR", "Clean the build folder"] {
        assert!(!stderr.contains(private), "{private} in {stderr}");
    }
}

fn press(hub: &Hub, message_id: i64, data: String) {
    hub.control
        .send(Control::Callback(CallbackInput {
            query_id: "q".into(),
            data: Some(data),
            message_id: Some(message_id),
        }))
        .unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_press_in_the_topic_is_the_hooks_decision() {
    for (action, behavior) in [("allow", "allow"), ("deny", "deny")] {
        let hub = hub(&format!("press-{action}")).await;
        let running = tokio::spawn(run_hook(home(&format!("press-{action}"), &hub.addr)));
        until("hook prompt", || hub.fake.prompts().len() == 1).await;
        let (message_id, id) = hub.fake.prompts().remove(0);
        let sent = hub.fake.ops();
        let text = sent
            .iter()
            .find_map(|op| match op {
                Op::Send {
                    permission: true,
                    text,
                    ..
                } => Some(text.clone()),
                _ => None,
            })
            .unwrap();
        assert!(text.contains("Clean the build folder") && text.contains("UNSET_PRIVATE_DIR"));
        press(&hub, message_id, format!("{action}:{id}"));
        let (output, _) = running.await.unwrap();
        assert_clean(&output);
        let decision: Value = serde_json::from_slice(&output.stdout).expect("decision JSON");
        assert_eq!(
            decision["hookSpecificOutput"]["hookEventName"],
            "PermissionRequest"
        );
        assert_eq!(
            decision["hookSpecificOutput"]["decision"]["behavior"],
            behavior
        );
        until("buttons removed", || {
            hub.fake.edits_of(message_id).len() == 1
        })
        .await;
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_request_the_channel_relays_gets_no_second_prompt() {
    let hub = hub("twin").await;
    hub.agents
        .send(AgentEvent::Message {
            conn: 1,
            received_at: std::time::Instant::now(),
            msg: AgentMsg::PermissionRequest(PermissionRequest {
                request_id: "qzxwv".into(),
                tool_name: "Bash".into(),
                description: "d".into(),
                input_preview: "p".into(),
            }),
        })
        .await
        .unwrap();
    until("channel prompt", || hub.fake.prompts().len() == 1).await;
    let (output, elapsed) = run_hook(home("twin", &hub.addr)).await;
    assert_clean(&output);
    assert!(output.stdout.is_empty());
    assert!(elapsed < Duration::from_millis(1500), "{elapsed:?}");
    tokio::time::sleep(Duration::from_secs(2)).await;
    assert_eq!(hub.fake.prompts().len(), 1, "no second set of buttons");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_session_end_answers_the_waiting_hook_without_a_decision() {
    let hub = hub("end").await;
    let running = tokio::spawn(run_hook(home("end", &hub.addr)));
    until("hook prompt", || hub.fake.prompts().len() == 1).await;
    let (message_id, _) = hub.fake.prompts().remove(0);
    hub.hooks
        .send(HookPost::new(
            "box".into(),
            SESSION.into(),
            r"C:\w\p".into(),
            String::new(),
            HookEvent::SessionEnd {
                reason: Some("other".into()),
                claude_pid: Some(10),
            },
        ))
        .await
        .unwrap();
    let (output, _) = running.await.unwrap();
    assert_clean(&output);
    assert!(output.stdout.is_empty());
    until("prompt closed", || hub.fake.edits_of(message_id).len() == 1).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_hub_that_stops_answers_the_waiting_hook_without_a_decision() {
    let hub = hub("stop").await;
    let running = tokio::spawn(run_hook(home("stop", &hub.addr)));
    until("hook prompt", || hub.fake.prompts().len() == 1).await;
    hub.control.send(Control::Stop).unwrap();
    let (output, elapsed) = running.await.unwrap();
    assert_clean(&output);
    assert!(output.stdout.is_empty());
    assert!(elapsed < Duration::from_secs(30), "{elapsed:?}");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_stopped_hub_means_no_decision_and_a_quick_exit() {
    let port = std::net::TcpListener::bind((Ipv4Addr::LOCALHOST, 0))
        .unwrap()
        .local_addr()
        .unwrap();
    let (output, elapsed) = run_hook(home("down", &port.to_string())).await;
    assert_clean(&output);
    assert!(output.stdout.is_empty());
    assert!(elapsed < Duration::from_secs(4), "{elapsed:?}");
}
