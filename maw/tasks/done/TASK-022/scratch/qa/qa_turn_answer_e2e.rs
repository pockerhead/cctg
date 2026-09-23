//! QA TASK-022: real hook ingress (HTTP POST, including the real `cctg hook
//! Stop` binary) into the real Slots actor and Scheduler with a fake
//! Telegram transport. Copied into crates/cctg/tests/ only for the QA run.

use std::io::Write;
use std::net::{Ipv4Addr, SocketAddr};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use cctg::hub::api::{ForumTopic, Message};
use cctg::hub::ingress::{self, AgentEvent};
use cctg::hub::registry::RegistryStore;
use cctg::hub::scheduler::{BucketConfig, Delivery, Op, Outcome, Scheduler, Transport};
use cctg::hub::slots::{Options, Slots};
use transcript::{SplitOptions, split_for_telegram};
use cctg::wire::{AgentMsg, HookEvent, HookPost, Register, Secret};
use tokio::sync::mpsc;

const SECRET: &str = "qa-turn-answer-secret-0123456789";
const CHAT: i64 = -1000000000001;
const A: &str = "aaaaaaaa-0000-4000-8000-000000000001";
const B: &str = "bbbbbbbb-0000-4000-8000-000000000002";
const N: &str = "nnnnnnnn-0000-4000-8000-000000000003";
const CWD: &str = r"C:\qa\proj";

#[derive(Default)]
struct Fake(Mutex<Vec<Op>>, Mutex<i64>);

impl Transport for Fake {
    async fn execute(&self, op: &Op) -> Delivery {
        self.0.lock().unwrap().push(op.clone());
        match op {
            Op::CreateTopic { name, .. } => {
                let mut next = self.1.lock().unwrap();
                *next += 1;
                Ok(Outcome::Topic(ForumTopic {
                    message_thread_id: 99 + *next,
                    name: name.clone(),
                    icon_custom_emoji_id: None,
                }))
            }
            Op::Send { .. } | Op::SendDocument { .. } => Ok(Outcome::Sent(Message::default())),
            _ => Ok(Outcome::Done),
        }
    }
}

impl Fake {
    fn to_topic(&self, thread: i64) -> Vec<String> {
        self.0
            .lock()
            .unwrap()
            .iter()
            .filter_map(|op| match op {
                Op::Send {
                    thread_id: Some(t),
                    text,
                    ..
                } if *t == thread => Some(text.clone()),
                Op::SendDocument {
                    thread_id: Some(t),
                    document,
                } if *t == thread => Some(format!("DOC:{}:{}", document.file_name, document.bytes.len())),
                _ => None,
            })
            .collect()
    }
}

struct Rig {
    fake: Arc<Fake>,
    addr: String,
    home: PathBuf,
    agents: mpsc::Sender<AgentEvent>,
    _to_agent: Vec<mpsc::Receiver<cctg::wire::HubMsg>>,
    _control: mpsc::UnboundedSender<cctg::hub::slots::Control>,
}

async fn rig(test: &str) -> Rig {
    let state = std::env::temp_dir().join(format!("cctg-qa022-{test}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&state);
    std::fs::create_dir_all(&state).unwrap();
    let fake = Arc::new(Fake::default());
    let (scheduler, outbox) = Scheduler::new(
        fake.clone(),
        BucketConfig {
            capacity: 1000,
            refill_every: Duration::from_millis(1),
            min_gap: Duration::ZERO,
        },
    );
    tokio::spawn(scheduler.run());
    let store = RegistryStore::open(&state).unwrap();
    let options = Options {
        grace: Duration::ZERO,
        chat_id: CHAT,
        ..Options::default()
    };
    let (slots, _view) = Slots::new(store.load().unwrap(), store, outbox, options);
    let (agents, agents_rx) = mpsc::channel(16);
    let (hooks, hooks_rx) = mpsc::channel(16);
    let (control, control_rx) = mpsc::unbounded_channel();
    tokio::spawn(slots.run(agents_rx, hooks_rx, control_rx));
    let listener = ingress::bind(SocketAddr::from((Ipv4Addr::LOCALHOST, 0)))
        .await
        .unwrap();
    let addr = listener.local_addr().unwrap().to_string();
    tokio::spawn(ingress::serve_hooks(
        listener,
        Secret::parse(SECRET).unwrap(),
        hooks,
    ));
    // Isolated home: the hook binary must not read the real ~/.cctg/device.env.
    let home = Path::new(env!("CARGO_TARGET_TMPDIR")).join(format!("qa022-{test}"));
    std::fs::create_dir_all(home.join(".cctg")).unwrap();
    std::fs::write(home.join(".cctg").join("device.env"), "").unwrap();
    Rig {
        fake,
        addr,
        home,
        agents,
        _to_agent: Vec::new(),
        _control: control,
    }
}

impl Rig {
    async fn post(&self, session: &str, event: HookEvent) {
        let post = HookPost::new(
            "box".into(),
            session.into(),
            CWD.into(),
            String::new(),
            event,
        );
        cctg::hook::post(
            &self.addr,
            &Secret::parse(SECRET).unwrap(),
            &post,
            Duration::from_secs(5),
        )
        .await
        .expect("hub accepted the hook POST");
    }

    async fn start(&self, session: &str, source: &str, pid: u32, parent: Option<u32>) {
        self.post(
            session,
            HookEvent::SessionStart {
                source: Some(source.into()),
                claude_pid: Some(pid),
                parent_claude_pid: parent,
            },
        )
        .await;
    }

    async fn end(&self, session: &str, reason: &str, pid: u32) {
        self.post(
            session,
            HookEvent::SessionEnd {
                reason: Some(reason.into()),
                claude_pid: Some(pid),
            },
        )
        .await;
    }

    /// The real `cctg hook Stop` binary, real Claude Code Stop payload shape.
    async fn stop_via_binary(&self, session: &str, answer: Option<&str>) {
        let mut input = serde_json::json!({
            "session_id": session,
            "transcript_path": "",
            "cwd": CWD,
            "prompt_id": "0bb27012-0d04-43a9-9fa9-128e91ee7e34",
            "permission_mode": "default",
            "hook_event_name": "Stop",
            "stop_hook_active": false,
            "background_tasks": [],
            "session_crons": []
        });
        if let Some(answer) = answer {
            input["last_assistant_message"] = answer.into();
        }
        let (home, addr) = (self.home.clone(), self.addr.clone());
        let output = tokio::task::spawn_blocking(move || {
            let mut child = Command::new(env!("CARGO_BIN_EXE_cctg"))
                .args(["hook", "Stop"])
                .env("USERPROFILE", &home)
                .env("HOME", &home)
                .env("CCTG_HUB_SECRET", SECRET)
                .env("CCTG_HUB_HOOK_ADDR", &addr)
                .env("CCTG_HOST", "box")
                .stdin(Stdio::piped())
                .stdout(Stdio::piped())
                .stderr(Stdio::piped())
                .spawn()
                .unwrap();
            let mut stdin = child.stdin.take().unwrap();
            stdin.write_all(input.to_string().as_bytes()).unwrap();
            drop(stdin);
            child.wait_with_output().unwrap()
        })
        .await
        .unwrap();
        assert!(output.status.success());
        assert!(output.stdout.is_empty());
        let stderr = String::from_utf8_lossy(&output.stderr);
        assert!(!stderr.contains(SECRET), "{stderr}");
        if let Some(answer) = answer.filter(|a| !a.trim().is_empty()) {
            assert!(!stderr.contains(answer), "answer in hook stderr: {stderr}");
        }
    }

    async fn settle<F: Fn(&[String]) -> bool>(&self, thread: i64, done: F) -> Vec<String> {
        let waited = async {
            loop {
                let got = self.fake.to_topic(thread);
                if done(&got) {
                    return got;
                }
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        };
        tokio::time::timeout(Duration::from_secs(20), waited)
            .await
            .unwrap_or_else(|_| panic!("timeout: {:?}", self.fake.to_topic(thread)))
    }

    /// Lets in-flight work drain, then returns the topic's messages.
    async fn quiet(&self, thread: i64) -> Vec<String> {
        tokio::time::sleep(Duration::from_millis(400)).await;
        self.fake.to_topic(thread)
    }

    async fn connect(&mut self, conn: u64, session: &str, pid: u32) {
        let (to_agent, rx) = mpsc::channel(16);
        self._to_agent.push(rx);
        self.agents
            .send(AgentEvent::Registered {
                conn,
                register: Register {
                    session_id: session.into(),
                    host: "box".into(),
                    cwd: CWD.into(),
                    claude_pid: Some(pid),
                    verdict_ack: false,
                },
                to_agent,
            })
            .await
            .unwrap();
    }

    async fn reply(&self, conn: u64, text: &str) {
        self.agents
            .send(AgentEvent::Message {
                conn,
                received_at: std::time::Instant::now(),
                msg: AgentMsg::Reply { text: text.into() },
            })
            .await
            .unwrap();
    }
}

fn is_separator(s: &str) -> bool {
    s.starts_with("── session")
}

fn content(msgs: &[String]) -> Vec<String> {
    msgs.iter().filter(|m| !is_separator(m)).cloned().collect()
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn stop_answer_reaches_topic_in_order_via_real_hook_binary() {
    let r = rig("order").await;
    r.start(A, "startup", 10, None).await;
    r.settle(100, |_| r.fake.0.lock().unwrap().iter().any(|op| matches!(op, Op::CreateTopic { .. })))
        .await;
    // Wait for the topic to be recorded in the registry.
    tokio::time::sleep(Duration::from_millis(300)).await;

    // Short answer.
    r.stop_via_binary(A, Some("первый ответ")).await;
    // Multi-chunk answer: must arrive as split_for_telegram chunks, in order.
    let para = |c: char| format!("{}\n\n", c.to_string().repeat(3000));
    let long: String = ['a', 'b', 'c'].into_iter().map(para).collect();
    let want = split_for_telegram(&long, SplitOptions::default());
    assert!(want.chunks.len() > 1 && !want.prefer_file);
    r.stop_via_binary(A, Some(&long)).await;
    // Emoji / surrogate-heavy answer.
    let emoji = "😀".repeat(2100); // 4200 UTF-16 units -> 2 chunks
    let want_emoji = split_for_telegram(&emoji, SplitOptions::default());
    r.stop_via_binary(A, Some(&emoji)).await;
    // > max chunks: one document.
    let huge = "x".repeat(5 * 4096);
    r.stop_via_binary(A, Some(&huge)).await;
    // Empty / absent / blank: nothing.
    r.stop_via_binary(A, None).await;
    r.stop_via_binary(A, Some("")).await;
    r.stop_via_binary(A, Some("  \n\t ")).await;

    let mut expected: Vec<String> = vec!["первый ответ".into()];
    expected.extend(want.chunks.iter().cloned());
    expected.extend(want_emoji.chunks.iter().cloned());
    expected.push(format!("DOC:answer-aaaaaaaa.txt:{}", huge.len()));
    let got = r.settle(100, |g| content(g).len() >= expected.len()).await;
    let got = {
        let _ = got;
        content(&r.quiet(100).await)
    };
    assert_eq!(got, expected);
    for chunk in &got {
        if !chunk.starts_with("DOC:") {
            assert!(transcript::telegram_len(chunk) <= 4096);
        }
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn nested_ended_unknown_and_late_clear_stop_send_nothing() {
    let mut r = rig("negative").await;
    r.start(A, "startup", 10, None).await;
    r.settle(100, |_| r.fake.0.lock().unwrap().iter().any(|op| matches!(op, Op::CreateTopic { .. })))
        .await;
    tokio::time::sleep(Duration::from_millis(300)).await;
    r.connect(1, A, 10).await;

    // Nested claude -p inside A.
    r.start(N, "startup", 20, Some(10)).await;
    r.stop_via_binary(N, Some("nested answer")).await;
    // A session never announced by SessionStart.
    r.stop_via_binary("cccccccc-0000-4000-8000-00000000000c", Some("unknown answer"))
        .await;
    // Reply then Stop in one turn: two messages, reply first.
    r.reply(1, "progress reply").await;
    r.settle(100, |g| g.iter().any(|m| m == "progress reply")).await;
    r.stop_via_binary(A, Some("final A")).await;
    r.settle(100, |g| g.iter().any(|m| m == "final A")).await;

    // /clear: A ends (reason clear), B starts (source clear) in the same pid.
    r.end(A, "clear", 10).await;
    r.start(B, "clear", 10, None).await;
    r.settle(100, |g| g.iter().any(|m| is_separator(m))).await;
    // Late Stop of the old session A after the slot moved to B.
    r.stop_via_binary(A, Some("late A after clear")).await;
    // B's own turn goes to the same topic.
    r.stop_via_binary(B, Some("answer B")).await;
    r.settle(100, |g| g.iter().any(|m| m == "answer B")).await;
    // B ends for good; a late Stop after that sends nothing.
    r.end(B, "prompt_input_exit", 10).await;
    tokio::time::sleep(Duration::from_millis(200)).await;
    r.stop_via_binary(B, Some("after end B")).await;

    let got = r.quiet(100).await;
    let other_topics: Vec<Op> = r
        .fake
        .0
        .lock()
        .unwrap()
        .iter()
        .filter(|op| matches!(op, Op::CreateTopic { .. }))
        .cloned()
        .collect();
    println!("topic 100: {got:?}");
    assert_eq!(other_topics.len(), 1, "nested run must not get a topic");
    let pos = |t: &str| got.iter().position(|m| m == t);
    assert!(pos("nested answer").is_none());
    assert!(pos("unknown answer").is_none());
    assert!(pos("late A after clear").is_none());
    assert!(pos("after end B").is_none());
    assert!(pos("progress reply").unwrap() < pos("final A").unwrap());
    let sep = got.iter().position(|m| is_separator(m)).unwrap();
    assert!(pos("final A").unwrap() < sep);
    assert!(sep < pos("answer B").unwrap(), "separator before B's answer");
    assert_eq!(
        content(&got),
        vec!["progress reply", "final A", "answer B"]
    );
}
