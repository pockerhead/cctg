//! Rolling status end to end (TASK-062): the real `cctg agent` binary
//! reading a transcript file, linked over real TCP to the real
//! `serve_agents`, the real `Slots` actor and `Scheduler`, and a fake Bot API
//! that keeps what the topic shows: messages get ids in the order they come
//! (a user's messages too), edits change them, deletes remove them. The
//! status message is the last message of the topic at every quiet point;
//! nothing is pinned. Harness adopted from `stream_e2e.rs`.

use std::io::Write;
use std::path::PathBuf;
use std::process::{Child, Stdio};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use cctg::device::canonical_cwd;
use cctg::hub::api::{ForumTopic, Message};
use cctg::hub::chat::{Chat, GroupChat};
use cctg::hub::ingress::serve_agents;
use cctg::hub::registry::RegistryStore;
use cctg::hub::scheduler::{BucketConfig, Delivery, Op, Outcome, Scheduler, Transport};
use cctg::hub::slots::{Control, Options, Slots};
use cctg::hub::updates::Inbound;
use cctg::wire::{HookEvent, HookPost, Secret};
use tokio::net::TcpListener;
use tokio::sync::mpsc;
use tokio::task::JoinHandle;

/// The default group of this test (TASK-069): the chat its Bot API fake
/// and its registry name.
const GROUP_ID: i64 = -1001;
const GROUP: Chat = Chat::Group(GroupChat::of(GROUP_ID));

mod common;

const SECRET: &str = "e2e-secret-0123456789abcdef";
const HOST: &str = "e2ebox";
const THREAD: i64 = 100;

// ---------------------------------------------------------------- fake Telegram

/// A message the topic shows.
#[derive(Debug, Clone, PartialEq)]
struct Shown {
    id: i64,
    text: String,
    /// Sent as a status message (buttons, no prompt) and not turned into
    /// anything else since.
    status: bool,
    /// Sent with a sound.
    loud: bool,
    /// Written by the user.
    user: bool,
}

#[derive(Default)]
struct Topic {
    next: i64,
    shown: Vec<Shown>,
    ops: Vec<Op>,
}

impl Topic {
    fn id(&mut self) -> i64 {
        self.next = self.next.max(1000) + 1;
        self.next
    }
}

#[derive(Default)]
struct Fake(Mutex<Topic>);

impl Transport for Fake {
    async fn execute(&self, op: &Op) -> Delivery {
        let mut topic = self.0.lock().unwrap();
        topic.ops.push(op.clone());
        let sent = |id| {
            Ok(Outcome::Sent(Message {
                message_id: id,
                ..Message::default()
            }))
        };
        match op {
            Op::CreateTopic { name, .. } => Ok(Outcome::Topic(ForumTopic {
                message_thread_id: THREAD,
                name: name.clone(),
                icon_custom_emoji_id: None,
            })),
            Op::Send {
                text,
                reply_markup,
                permission,
                notify,
                ..
            } => {
                let id = topic.id();
                topic.shown.push(Shown {
                    id,
                    text: text.clone(),
                    status: reply_markup.is_some() && !permission,
                    loud: *notify,
                    user: false,
                });
                sent(id)
            }
            Op::Stream {
                text,
                notify,
                into: None,
                ..
            } => {
                let id = topic.id();
                topic.shown.push(Shown {
                    id,
                    text: text.clone(),
                    status: false,
                    loud: *notify,
                    user: false,
                });
                sent(id)
            }
            Op::Stream {
                text,
                into: Some(id),
                ..
            } => {
                let id = *id;
                let Some(message) = topic.shown.iter_mut().find(|m| m.id == id) else {
                    return Err(cctg::hub::api::ApiError::Telegram {
                        code: 400,
                        description: "Bad Request: message to edit not found".into(),
                    });
                };
                message.text.clone_from(text);
                message.status = false;
                sent(id)
            }
            Op::Edit {
                message_id, text, ..
            } => {
                if let Some(message) = topic.shown.iter_mut().find(|m| m.id == *message_id) {
                    message.text.clone_from(text);
                }
                Ok(Outcome::Done)
            }
            Op::Delete { message_id, .. } => {
                topic.shown.retain(|m| m.id != *message_id);
                Ok(Outcome::Done)
            }
            _ => Ok(Outcome::Done),
        }
    }
}

impl Fake {
    /// A message of the user in the topic: its id.
    fn user(&self, text: &str) -> i64 {
        let mut topic = self.0.lock().unwrap();
        let id = topic.id();
        topic.shown.push(Shown {
            id,
            text: text.into(),
            status: false,
            loud: false,
            user: true,
        });
        id
    }

    fn shown(&self) -> Vec<Shown> {
        self.0.lock().unwrap().shown.clone()
    }

    fn ops(&self) -> Vec<Op> {
        self.0.lock().unwrap().ops.clone()
    }

    /// The texts the topic shows, `STATUS` for the status message.
    fn layout(&self) -> Vec<String> {
        self.shown()
            .into_iter()
            .map(|m| if m.status { "STATUS".into() } else { m.text })
            .collect()
    }
}

// ---------------------------------------------------------------- hub

struct Hub {
    fake: Arc<Fake>,
    hooks: mpsc::Sender<HookPost>,
    control: mpsc::UnboundedSender<Control>,
    tasks: Vec<JoinHandle<()>>,
}

impl Drop for Hub {
    fn drop(&mut self) {
        for task in &self.tasks {
            task.abort();
        }
    }
}

async fn start_hub(state: &std::path::Path, listener: TcpListener) -> Hub {
    let fake = Arc::new(Fake::default());
    let bucket = BucketConfig {
        capacity: 1000,
        refill_every: Duration::from_millis(1),
        min_gap: Duration::ZERO,
    };
    let (scheduler, outbox) = Scheduler::new(fake.clone(), bucket);
    let sched = tokio::spawn(scheduler.run());
    let store = RegistryStore::open(state).expect("store");
    let registry = store.load(GroupChat::of(GROUP_ID)).expect("load registry");
    let options = Options {
        grace: Duration::ZERO,
        stream_every: Duration::from_millis(50),
        stream_retry: Duration::from_millis(200),
        hold_answer: Duration::from_millis(500),
        status_every: Some(Duration::from_millis(50)),
        ..Options::default()
    };
    let slots = Slots::new(registry, store, outbox, options);
    let (agents, agents_rx) = mpsc::channel(64);
    let (hooks, hooks_rx) = mpsc::channel(64);
    let (control, control_rx) = mpsc::unbounded_channel();
    let actor = tokio::spawn(slots.run(agents_rx, hooks_rx, control_rx));
    let ingress = tokio::spawn(serve_agents(
        listener,
        Secret::parse(SECRET).expect("secret"),
        agents,
    ));
    Hub {
        fake,
        hooks,
        control,
        tasks: vec![ingress, actor, sched],
    }
}

// ---------------------------------------------------------------- session

struct TempRoot(PathBuf);
impl Drop for TempRoot {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

struct Session {
    _root: TempRoot,
    id: String,
    transcript: PathBuf,
    workdir: PathBuf,
    config: PathBuf,
    home: PathBuf,
    state: PathBuf,
}

fn session(name: &str) -> Session {
    let root = std::env::temp_dir().join(format!("cctg-rolling-e2e-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    let config = root.join("cfg");
    let project = config.join("projects").join("C--qa-w");
    std::fs::create_dir_all(&project).unwrap();
    let workdir = root.join("w");
    let home = root.join("home");
    let state = root.join("state");
    for dir in [&workdir, &home, &state] {
        std::fs::create_dir_all(dir).unwrap();
    }
    let id = "0a16e2e0-0000-4000-8000-000000000621".to_owned();
    let transcript = project.join(format!("{id}.jsonl"));
    Session {
        _root: TempRoot(root),
        id,
        transcript,
        workdir,
        config,
        home,
        state,
    }
}

impl Session {
    fn post(&self, event: HookEvent) -> HookPost {
        HookPost::new(
            HOST.into(),
            self.id.clone(),
            canonical_cwd(&self.workdir.to_string_lossy()),
            self.transcript.to_string_lossy().into_owned(),
            event,
        )
    }

    fn append(&self, bytes: &str) {
        let mut file = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(&self.transcript)
            .unwrap();
        file.write_all(bytes.as_bytes()).unwrap();
    }
}

/// The real `cctg agent` process of the session.
struct Agent(Child);
impl Drop for Agent {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

fn start_agent(s: &Session, port: u16) -> Agent {
    let mut child = common::spawn(
        common::cctg(&s.home)
            .arg("agent")
            .current_dir(&s.workdir)
            .env("CCTG_HUB_SECRET", SECRET)
            .env("CCTG_HUB_AGENT_ADDR", format!("127.0.0.1:{port}"))
            .env("CCTG_HOST", HOST)
            .env("CLAUDE_CODE_SESSION_ID", &s.id)
            .env("CLAUDE_CONFIG_DIR", &s.config)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null()),
    )
    .expect("spawn cctg agent");
    // Keep stdin open (the agent stops at EOF) and drain stdout.
    let mut stdin = child.stdin.take().unwrap();
    let _ = stdin.write_all(
        b"{\"jsonrpc\":\"2.0\",\"id\":0,\"method\":\"initialize\",\"params\":{\"protocolVersion\":\"2025-11-25\"}}\n{\"jsonrpc\":\"2.0\",\"method\":\"notifications/initialized\"}\n",
    );
    std::mem::forget(stdin);
    let mut stdout = child.stdout.take().unwrap();
    std::thread::spawn(move || {
        let _ = std::io::copy(&mut stdout, &mut std::io::sink());
    });
    Agent(child)
}

fn prompt(text: &str) -> String {
    format!(
        "{}\n",
        serde_json::json!({"type":"user","isMeta":false,"message":{"role":"user","content":text}})
    )
}

fn call(id: &str, desc: &str) -> String {
    format!(
        "{}\n",
        serde_json::json!({"type":"assistant","message":{"role":"assistant","stop_reason":"tool_use","content":[
            {"type":"tool_use","id":id,"name":"Bash","input":{"command":"true","description":desc}}]}})
    )
}

fn result(id: &str) -> String {
    format!(
        "{}\n",
        serde_json::json!({"type":"user","message":{"role":"user","content":[
            {"type":"tool_result","tool_use_id":id,"content":"ok","is_error":false}]}})
    )
}

fn answer(text: &str) -> String {
    format!(
        "{}\n",
        serde_json::json!({"type":"assistant","message":{"role":"assistant","stop_reason":"end_turn","content":[
            {"type":"text","text":text}]}})
    )
}

/// Waits until the topic shows `want` (`STATUS` for the status message).
async fn layout(hub: &Hub, what: &str, want: &[&str]) {
    let deadline = Instant::now() + Duration::from_secs(30);
    loop {
        if hub.fake.layout() == want {
            return;
        }
        assert!(
            Instant::now() < deadline,
            "{what}: the topic shows {:#?}",
            hub.fake.shown()
        );
        tokio::time::sleep(Duration::from_millis(25)).await;
    }
}

/// What the user asked for (2026-09-27): no pin; the status message is the
/// last message of the topic; a turn's prompt and lines go into the status
/// message, which moves below them, and the lines of one turn share one
/// message; the answer is a message of its own with a sound; a user's
/// message moves the status message below it.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn e2e_the_status_message_rolls_below_the_turn() {
    let s = session("turn");
    let l = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = l.local_addr().unwrap().port();
    let hub = start_hub(&s.state, l).await;
    hub.hooks
        .send(s.post(HookEvent::SessionStart {
            source: Some("startup".into()),
            claude_pid: Some(4242),
            parent_claude_pid: None,
        }))
        .await
        .unwrap();
    let _agent = start_agent(&s, port);
    layout(&hub, "the status message", &["STATUS"]).await;

    // A turn typed in the terminal.
    s.append(&prompt("go"));
    s.append(&call("t1", "one"));
    s.append(&result("t1"));
    s.append(&call("t2", "two"));
    s.append(&result("t2"));
    layout(
        &hub,
        "prompt and lines above the status",
        &["> go", "• Bash: one ✓\n• Bash: two ✓", "STATUS"],
    )
    .await;

    // Its answer: a loud message of its own, the status below it.
    s.append(&answer("Готово"));
    hub.hooks
        .send(s.post(HookEvent::Stop {
            prompt_id: None,
            last_assistant_message: Some("Готово".into()),
        }))
        .await
        .unwrap();
    layout(
        &hub,
        "the answer above the status",
        &["> go", "• Bash: one ✓\n• Bash: two ✓", "Готово", "STATUS"],
    )
    .await;
    let answer_message = hub
        .fake
        .shown()
        .into_iter()
        .find(|m| m.text == "Готово")
        .unwrap();
    assert!(answer_message.loud, "the answer rings");

    // The user writes: the status goes below the message, the next lines
    // below it too, in a message of their own.
    let user = hub.fake.user("ещё");
    hub.control
        .send(Control::Message(Inbound {
            display_name: None,
            chat: GROUP,
            sender: cctg::hub::chat::PrivateChat::of_user(1001),
            message_id: user,
            thread_id: Some(THREAD),
            text: Some("ещё".into()),
            reply_to: None,
            quote: None,
            forwarded: false,
            media: None,
            from_name: None,
            author: None,
            reply_from: None,
        }))
        .unwrap();
    layout(
        &hub,
        "the status below the user's message",
        &[
            "> go",
            "• Bash: one ✓\n• Bash: two ✓",
            "Готово",
            "ещё",
            "STATUS",
        ],
    )
    .await;
    s.append(&call("t3", "three"));
    s.append(&result("t3"));
    layout(
        &hub,
        "the next turn's lines below the user's message",
        &[
            "> go",
            "• Bash: one ✓\n• Bash: two ✓",
            "Готово",
            "ещё",
            "• Bash: three ✓",
            "STATUS",
        ],
    )
    .await;

    let ops = hub.fake.ops();
    assert!(
        !ops.iter().any(|op| matches!(op, Op::Unpin { .. })),
        "nothing pinned or unpinned"
    );
    // Only the answer rang.
    let loud: Vec<String> = hub
        .fake
        .shown()
        .into_iter()
        .filter(|m| m.loud)
        .map(|m| m.text)
        .collect();
    assert_eq!(loud, ["Готово"]);
}
