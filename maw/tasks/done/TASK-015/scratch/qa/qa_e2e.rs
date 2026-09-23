//! QA TASK-015 end-to-end. Real hook ingress: `hook::build` on Claude Code
//! style stdin JSON, `hook::post` over HTTP into `ingress::serve_hooks`. Real
//! agent link: TCP into `ingress::serve_agents` (hello + register). Real
//! `Slots` actor and `Scheduler`, fake Telegram transport. Anonymized parent
//! and subagent jsonl files in a temp dir. Then the actor is stopped and a new
//! one is started from the saved `registry.json`.
//!
//! Lives under maw/tasks/in_progress/TASK-015/scratch/qa/; copied into
//! crates/cctg/tests/ only for the run (see run_e2e.sh).

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use cctg::hook::{self, Probe};
use cctg::hub::api::{ForumTopic, Message};
use cctg::hub::ingress;
use cctg::hub::registry::RegistryStore;
use cctg::hub::scheduler::{BucketConfig, Delivery, Op, Outcome, Scheduler, Transport};
use cctg::hub::slots::{Control, Options, Slots};
use cctg::hub::updates::Inbound;
use cctg::proctree::Lineage;
use cctg::wire::{self, AgentMsg, HookEvent, HookPost, HubMsg, Register, Secret};
use serde_json::{Value, json};
use tokio::io::BufReader;
use tokio::net::TcpStream;
use tokio::net::tcp::OwnedWriteHalf;
use tokio::sync::mpsc;
use tokio::task::JoinHandle;

const SECRET: &str = "qa-secret-0123456789abcdef";
const HOST: &str = "qa-box";
const P: &str = "5e551017-0000-4000-8000-0000000000a1";
const N: &str = "5e551017-0000-4000-8000-0000000000b2";
const Q: &str = "5e551017-0000-4000-8000-0000000000c3";
const A1: &str = "a1000000000000001";
const A2: &str = "a2000000000000002";
const A3: &str = "a3000000000000003";
const A4: &str = "a4000000000000004";
const GHOST: &str = "a9000000000000009";
const INTERNAL: &str = "a8000000000000008";

fn secret() -> Secret {
    Secret::parse(SECRET).expect("secret")
}

/// Telegram as the hub sees it: numbered topics and messages; `shown` is what
/// each message displays after sends and edits.
struct Fake {
    ops: Mutex<Vec<Op>>,
    shown: Mutex<BTreeMap<i64, (Option<i64>, String)>>,
    next_message: Mutex<i64>,
    next_topic: Mutex<i64>,
}

impl Fake {
    fn new(first_topic: i64, first_message: i64) -> Arc<Self> {
        Arc::new(Self {
            ops: Mutex::new(Vec::new()),
            shown: Mutex::new(BTreeMap::new()),
            next_message: Mutex::new(first_message),
            next_topic: Mutex::new(first_topic),
        })
    }
    fn ops(&self) -> Vec<Op> {
        self.ops.lock().unwrap().clone()
    }
    fn creates(&self) -> usize {
        self.ops()
            .iter()
            .filter(|op| matches!(op, Op::CreateTopic { .. }))
            .count()
    }
    fn sends(&self) -> Vec<(Option<i64>, String)> {
        self.ops()
            .into_iter()
            .filter_map(|op| match op {
                Op::Send {
                    thread_id, text, ..
                } => Some((thread_id, text)),
                _ => None,
            })
            .collect()
    }
    fn block_sends(&self) -> Vec<(Option<i64>, String)> {
        self.sends()
            .into_iter()
            .filter(|(_, text)| text.starts_with('↳') || text.starts_with('⇣'))
            .collect()
    }
    /// Message id and current text of the message whose text starts with `prefix`.
    fn message(&self, prefix: &str) -> Option<(i64, Option<i64>, String)> {
        self.shown
            .lock()
            .unwrap()
            .iter()
            .find(|(_, (_, text))| text.starts_with(prefix))
            .map(|(id, (thread, text))| (*id, *thread, text.clone()))
    }
}

impl Transport for Fake {
    async fn execute(&self, op: &Op) -> Delivery {
        self.ops.lock().unwrap().push(op.clone());
        match op {
            Op::CreateTopic { name, .. } => {
                let mut next = self.next_topic.lock().unwrap();
                *next += 1;
                Ok(Outcome::Topic(ForumTopic {
                    message_thread_id: *next,
                    name: name.clone(),
                    icon_custom_emoji_id: None,
                }))
            }
            Op::Send {
                thread_id, text, ..
            } => {
                let id = {
                    let mut next = self.next_message.lock().unwrap();
                    *next += 1;
                    *next
                };
                self.shown
                    .lock()
                    .unwrap()
                    .insert(id, (*thread_id, text.clone()));
                Ok(Outcome::Sent(Message {
                    message_id: id,
                    ..Message::default()
                }))
            }
            Op::SendDocument { .. } => {
                let mut next = self.next_message.lock().unwrap();
                *next += 1;
                Ok(Outcome::Sent(Message {
                    message_id: *next,
                    ..Message::default()
                }))
            }
            Op::Edit {
                message_id, text, ..
            } => {
                if let Some(entry) = self.shown.lock().unwrap().get_mut(message_id) {
                    entry.1 = text.clone();
                }
                Ok(Outcome::Done)
            }
            _ => Ok(Outcome::Done),
        }
    }
}

struct Hub {
    hook_addr: String,
    agent_addr: String,
    control: mpsc::UnboundedSender<Control>,
    tasks: Vec<JoinHandle<()>>,
}

impl Hub {
    async fn start(state: &Path, fake: Arc<Fake>) -> Self {
        let store = RegistryStore::open(state).expect("store");
        let registry = store.load().expect("registry loads");
        let bucket = BucketConfig {
            capacity: 1000,
            refill_every: Duration::from_millis(1),
            min_gap: Duration::ZERO,
        };
        let (scheduler, outbox) = Scheduler::new(fake, bucket);
        let mut tasks = vec![tokio::spawn(scheduler.run())];
        let options = Options {
            chat_id: -1000000000001,
            grace: Duration::ZERO,
            correlate_for: Duration::from_secs(3),
            recheck_after: Duration::from_millis(50),
            ..Options::default()
        };
        let (slots, _view) = Slots::new(registry, store, outbox, options);
        let hooks_l = ingress::bind("127.0.0.1:0".parse().unwrap())
            .await
            .unwrap();
        let agents_l = ingress::bind("127.0.0.1:0".parse().unwrap())
            .await
            .unwrap();
        let hook_addr = hooks_l.local_addr().unwrap().to_string();
        let agent_addr = agents_l.local_addr().unwrap().to_string();
        let (agents_tx, agents_rx) = mpsc::channel(256);
        let (hooks_tx, hooks_rx) = mpsc::channel(256);
        let (control, control_rx) = mpsc::unbounded_channel();
        tasks.push(tokio::spawn(ingress::serve_agents(
            agents_l,
            secret(),
            agents_tx,
        )));
        tasks.push(tokio::spawn(ingress::serve_hooks(hooks_l, secret(), hooks_tx)));
        tasks.push(tokio::spawn(slots.run(agents_rx, hooks_rx, control_rx)));
        Self {
            hook_addr,
            agent_addr,
            control,
            tasks,
        }
    }

    fn stop(self) {
        for task in self.tasks {
            task.abort();
        }
    }

    /// Runs the real hook builder on `stdin`; `false` when the hook skipped it.
    async fn fire(&self, event: &str, stdin: Value, lineage: Lineage) -> bool {
        let cwd = |cwd: &str| cwd.to_owned();
        let lin = move |_: &str| lineage;
        let exists = |path: &Path| path.exists();
        let probe = Probe {
            host: HOST,
            cwd: &cwd,
            lineage: &lin,
            exists: &exists,
        };
        let bytes = serde_json::to_vec(&stdin).unwrap();
        match hook::build(event, &bytes, &probe) {
            Ok(post) => {
                self.post(&post).await;
                true
            }
            Err(_) => false,
        }
    }

    async fn post(&self, post: &HookPost) {
        hook::post(&self.hook_addr, &secret(), post, Duration::from_secs(3))
            .await
            .expect("hub took the hook");
    }

    fn message(&self, message_id: i64, thread_id: i64, text: &str, reply_to: Option<i64>) {
        self.control
            .send(Control::Message(Inbound {
                message_id,
                thread_id: Some(thread_id),
                text: Some(text.to_owned()),
                reply_to,
            }))
            .unwrap();
    }
}

struct AgentLink {
    rx: mpsc::UnboundedReceiver<HubMsg>,
    _writer: OwnedWriteHalf,
}

impl AgentLink {
    async fn connect(addr: &str, session: &str, cwd: &str, pid: u32) -> Self {
        let stream = TcpStream::connect(addr).await.unwrap();
        let (read, mut writer) = stream.into_split();
        wire::write_msg(&mut writer, &AgentMsg::Hello { secret: secret() })
            .await
            .unwrap();
        wire::write_msg(
            &mut writer,
            &AgentMsg::Register(Register {
                session_id: session.to_owned(),
                host: HOST.to_owned(),
                cwd: cwd.to_owned(),
                claude_pid: Some(pid),
                verdict_ack: false,
            }),
        )
        .await
        .unwrap();
        let (tx, rx) = mpsc::unbounded_channel();
        tokio::spawn(async move {
            let mut reader = BufReader::new(read);
            let mut buf = Vec::new();
            loop {
                buf.clear();
                if wire::read_line(&mut reader, &mut buf).await.is_err() {
                    return;
                }
                if let Ok(msg) = wire::decode::<HubMsg>(&buf) {
                    let _ = tx.send(msg);
                }
            }
        });
        Self {
            rx,
            _writer: writer,
        }
    }

    /// Inbound messages received so far: (content, meta).
    fn inbound(&mut self) -> Vec<(String, BTreeMap<String, String>)> {
        let mut out = Vec::new();
        while let Ok(msg) = self.rx.try_recv() {
            if let HubMsg::Inbound { content, meta } = msg {
                out.push((content, meta));
            }
        }
        out
    }
}

async fn wait_for(what: &str, mut cond: impl FnMut() -> bool) {
    for _ in 0..200 {
        if cond() {
            return;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    panic!("timed out waiting for: {what}");
}

fn stdin(event: &str, session: &str, cwd: &str, transcript: &Path, extra: Value) -> Value {
    let mut base = json!({
        "session_id": session,
        "transcript_path": transcript.to_string_lossy(),
        "cwd": cwd,
        "hook_event_name": event,
    });
    base.as_object_mut()
        .unwrap()
        .extend(extra.as_object().unwrap().clone());
    base
}

fn top(pid: u32) -> Lineage {
    Lineage {
        claude_pid: Some(pid),
        parent_claude_pid: None,
    }
}

fn line(value: Value) -> String {
    format!("{value}\n")
}

fn call_line(tool: &str, description: &str) -> String {
    line(json!({
        "type": "assistant", "isSidechain": false,
        "message": { "role": "assistant", "id": format!("msg_{tool}"), "stop_reason": "tool_use", "content": [{
            "type": "tool_use", "id": tool, "name": "Agent",
            "input": { "description": description, "prompt": "anonymized prompt", "subagent_type": "Explore" },
        }]},
    }))
}

fn result_line(tool: &str, agent: &str) -> String {
    line(json!({
        "type": "user", "isSidechain": false,
        "message": { "role": "user", "content": [{
            "type": "tool_result", "tool_use_id": tool,
            "content": [{ "type": "text", "text": "Async agent launched." }],
        }]},
        "toolUseResult": { "isAsync": true, "status": "async_launched", "agentId": agent },
    }))
}

fn append(path: &Path, text: &str) {
    use std::io::Write;
    let mut file = std::fs::OpenOptions::new()
        .append(true)
        .create(true)
        .open(path)
        .unwrap();
    file.write_all(text.as_bytes()).unwrap();
}

/// Subagent files: `finished` adds a Bash call and a final `end_turn` text.
fn agent_files(dir: &Path, agent: &str, description: &str, finished: Option<&str>) -> PathBuf {
    let path = dir.join(format!("agent-{agent}.jsonl"));
    let mut text = line(json!({
        "type": "user", "isSidechain": true, "agentId": agent,
        "message": { "role": "user", "content": "anonymized spawn prompt" },
    }));
    text += &line(json!({
        "type": "assistant", "isSidechain": true, "agentId": agent,
        "message": { "role": "assistant", "id": format!("m1{agent}"), "stop_reason": null, "content": [{
            "type": "tool_use", "id": format!("tb{agent}"), "name": "Bash",
            "input": { "command": "ls", "description": format!("List files for {agent}") },
        }]},
    }));
    if let Some(final_text) = finished {
        text += &line(json!({
            "type": "user", "isSidechain": true, "agentId": agent,
            "message": { "role": "user", "content": [{
                "type": "tool_result", "tool_use_id": format!("tb{agent}"), "content": "a b c",
            }]},
        }));
        text += &line(json!({
            "type": "assistant", "isSidechain": true, "agentId": agent,
            "message": { "role": "assistant", "id": format!("m2{agent}"), "stop_reason": "end_turn", "content": [{
                "type": "text", "text": final_text,
            }]},
        }));
    }
    std::fs::write(&path, text).unwrap();
    std::fs::write(
        dir.join(format!("agent-{agent}.meta.json")),
        json!({ "agentType": "Explore", "description": description, "toolUseId": format!("toolu_{agent}"), "spawnDepth": 1 }).to_string(),
    )
    .unwrap();
    path
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn qa_subagents_nested_reply_and_restart_end_to_end() {
    let root = std::env::temp_dir().join(format!("cctg-qa-e2e-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    let state = root.join("state");
    let project = root.join("projects").join("C--qa-work-demo");
    let subagents = project.join(P).join("subagents");
    std::fs::create_dir_all(&state).unwrap();
    std::fs::create_dir_all(&subagents).unwrap();
    let cwd_p = r"C:\qa\work\demo";
    let cwd_q = r"C:\qa\work\other";
    let tp = project.join(format!("{P}.jsonl"));
    let tn = project.join(format!("{N}.jsonl"));
    let tq = root.join("projects").join(format!("{Q}.jsonl"));
    std::fs::write(
        &tp,
        line(json!({"type":"user","message":{"role":"user","content":"anonymized prompt"}})),
    )
    .unwrap();

    // ---------- hub run 1 ----------
    let fake = Fake::new(100, 1000);
    let hub = Hub::start(&state, fake.clone()).await;

    assert!(
        hub.fire(
            "SessionStart",
            stdin("SessionStart", P, cwd_p, &tp, json!({"source":"startup"})),
            top(10)
        )
        .await
    );
    let mut agent_p = AgentLink::connect(&hub.agent_addr, P, cwd_p, 10).await;
    wait_for("parent topic", || fake.creates() == 1).await;
    let topic_p = 101;
    assert!(
        hub.fire(
            "SessionStart",
            stdin("SessionStart", Q, cwd_q, &tq, json!({"source":"startup"})),
            top(30)
        )
        .await
    );
    let mut agent_q = AgentLink::connect(&hub.agent_addr, Q, cwd_q, 30).await;
    wait_for("unrelated topic", || fake.creates() == 2).await;
    let topic_q = 102;

    // Three explicit subagents start before the parent transcript has their calls.
    for agent in [A1, A2, A3] {
        let body = json!({"agent_id": agent, "agent_type": "Explore"});
        assert!(
            hub.fire(
                "SubagentStart",
                stdin("SubagentStart", P, cwd_p, &tp, body),
                top(10)
            )
            .await
        );
    }
    // An `--agent` session's typed main agent: files exist, no Agent call.
    agent_files(&subagents, GHOST, "ghost", Some("ghost final"));
    let ghost_path = subagents.join(format!("agent-{GHOST}.jsonl"));
    assert!(
        hub.fire(
            "SubagentStart",
            stdin(
                "SubagentStart",
                P,
                cwd_p,
                &tp,
                json!({"agent_id": GHOST, "agent_type": "my-agent"})
            ),
            top(10)
        )
        .await
    );
    assert!(
        hub.fire(
            "SubagentStop",
            stdin(
                "SubagentStop",
                P,
                cwd_p,
                &tp,
                json!({"agent_id": GHOST, "agent_type": "my-agent",
                "agent_transcript_path": ghost_path.to_string_lossy(), "last_assistant_message": "ghost final"})
            ),
            top(10)
        )
        .await,
        "a typed stop with files passes the hook; the hub must drop it"
    );
    // Internal agent: the hook skips it; posted raw anyway, the hub must drop it too.
    let internal_stdin = stdin(
        "SubagentStop",
        P,
        cwd_p,
        &tp,
        json!({"agent_id": INTERNAL, "agent_type": "",
        "agent_transcript_path": subagents.join(format!("agent-{INTERNAL}.jsonl")).to_string_lossy(),
        "last_assistant_message": "Running a command"}),
    );
    assert!(!hub.fire("SubagentStop", internal_stdin, top(10)).await);
    hub.post(&HookPost::new(
        HOST.into(),
        P.into(),
        cwd_p.into(),
        tp.to_string_lossy().into_owned(),
        HookEvent::SubagentStop {
            agent_id: INTERNAL.into(),
            agent_type: String::new(),
            agent_transcript_path: Some(ghost_path.to_string_lossy().into_owned()),
            last_assistant_message: Some("Running a command".into()),
        },
    ))
    .await;

    tokio::time::sleep(Duration::from_millis(400)).await;
    assert!(
        fake.block_sends().is_empty(),
        "no block before the transcript shows the calls: {:?}",
        fake.block_sends()
    );

    // The parent transcript catches up; A3's result line is still being written.
    append(
        &tp,
        &(call_line("toolu_a1", "desc one")
            + &result_line("toolu_a1", A1)
            + &call_line("toolu_a2", "desc two")
            + &result_line("toolu_a2", A2)
            + &call_line("toolu_a3", "desc three")),
    );
    let a3_result = result_line("toolu_a3", A3);
    append(&tp, a3_result.trim_end());
    wait_for("two blocks", || fake.block_sends().len() == 2).await;
    tokio::time::sleep(Duration::from_millis(300)).await;
    assert_eq!(
        fake.block_sends().len(),
        2,
        "a partial line is not read: {:?}",
        fake.block_sends()
    );
    append(&tp, "\n");
    wait_for("three blocks", || fake.block_sends().len() == 3).await;
    for (thread, text) in fake.block_sends() {
        assert_eq!(thread, Some(topic_p), "block in the parent's topic: {text}");
        assert!(text.ends_with("в работе…"), "{text}");
    }
    let (a1_msg, _, a1_text) = fake.message(&format!("↳ Explore {A1}")).expect("A1 block");
    assert!(a1_text.contains("desc one"), "{a1_text}");

    // A1: report handed back, then stop -> report wins over last message.
    agent_files(&subagents, A1, "desc one", Some("A1 transcript final"));
    assert!(
        hub.fire(
            "PostToolUse",
            stdin(
                "PostToolUse",
                P,
                cwd_p,
                &tp,
                json!({"agent_id": A1, "agent_type": "Explore", "tool_name": "SubagentHandback",
                "tool_input": {"message": "REPORT-A1 delivered"}})
            ),
            top(10)
        )
        .await
    );
    let a1_path = subagents.join(format!("agent-{A1}.jsonl"));
    let stop = |agent: &str, path: &Path, last: &str| {
        stdin(
            "SubagentStop",
            P,
            cwd_p,
            &tp,
            json!({"agent_id": agent, "agent_type": "Explore",
            "agent_transcript_path": path.to_string_lossy(), "last_assistant_message": last}),
        )
    };
    assert!(
        hub.fire("SubagentStop", stop(A1, &a1_path, "LAST-A1"), top(10))
            .await
    );
    // A2: finished transcript whose final text equals the last message -> brief.
    let a2_path = agent_files(&subagents, A2, "desc two", Some("FINAL-A2"));
    assert!(
        hub.fire("SubagentStop", stop(A2, &a2_path, "FINAL-A2"), top(10))
            .await
    );
    // A3: lagging transcript (no final text yet) -> last message.
    let a3_path = agent_files(&subagents, A3, "desc three", None);
    assert!(
        hub.fire("SubagentStop", stop(A3, &a3_path, "LAST-A3"), top(10))
            .await
    );
    wait_for("finished blocks", || {
        [A1, A2, A3].iter().all(|agent| {
            fake.message(&format!("↳ Explore {agent}"))
                .is_some_and(|(_, _, text)| !text.ends_with("в работе…"))
        })
    })
    .await;
    let text = |agent: &str| fake.message(&format!("↳ Explore {agent}")).unwrap().2;
    let (t1, t2, t3) = (text(A1), text(A2), text(A3));
    eprintln!("A1 block:\n{t1}\n---\nA2 block:\n{t2}\n---\nA3 block:\n{t3}");
    assert!(
        t1.contains("REPORT-A1 delivered") && !t1.contains("LAST-A1"),
        "{t1}"
    );
    assert!(
        t2.contains("FINAL-A2") && t2.contains(&format!("List files for {A2}")),
        "brief of the finished transcript: {t2}"
    );
    assert!(t3.contains("LAST-A3"), "lagging transcript -> last message: {t3}");
    assert_eq!(
        fake.message(&format!("↳ Explore {A1}")).unwrap().0,
        a1_msg,
        "edited in place"
    );

    // Nested claude -p: no topic, one block in the parent's topic.
    let nested_lineage = Lineage {
        claude_pid: Some(20),
        parent_claude_pid: Some(10),
    };
    assert!(
        hub.fire(
            "SessionStart",
            stdin("SessionStart", N, cwd_p, &tn, json!({"source":"startup"})),
            nested_lineage
        )
        .await
    );
    let mut agent_n = AgentLink::connect(&hub.agent_addr, N, cwd_p, 20).await;
    wait_for("nested block", || fake.message("⇣ nested").is_some()).await;
    // A subagent of the nested run gets no block.
    agent_files(&subagents, "a7000000000000007", "nested sub", Some("x"));
    assert!(
        hub.fire(
            "SubagentStart",
            stdin(
                "SubagentStart",
                N,
                cwd_p,
                &tn,
                json!({"agent_id": "a7000000000000007", "agent_type": "Explore"})
            ),
            nested_lineage
        )
        .await
    );
    assert!(
        hub.fire(
            "Stop",
            stdin(
                "Stop",
                N,
                cwd_p,
                &tn,
                json!({"last_assistant_message": "NESTED-ANSWER"})
            ),
            nested_lineage
        )
        .await
    );
    assert!(
        hub.fire(
            "SessionEnd",
            stdin("SessionEnd", N, cwd_p, &tn, json!({"reason":"other"})),
            nested_lineage
        )
        .await
    );
    wait_for("nested answer", || {
        fake.message("⇣ nested")
            .is_some_and(|(_, _, text)| text.contains("NESTED-ANSWER"))
    })
    .await;
    let (nested_msg, nested_thread, nested_text) = fake.message("⇣ nested").unwrap();
    assert_eq!(nested_thread, Some(topic_p));
    assert!(nested_text.starts_with("⇣ nested 5e551017"), "{nested_text}");

    // Replies.
    let (a2_msg, _, _) = fake.message(&format!("↳ Explore {A2}")).unwrap();
    hub.message(7001, topic_p, "to A2", Some(a2_msg));
    hub.message(7002, topic_p, "to nested", Some(nested_msg));
    hub.message(7003, topic_p, "plain", None);
    hub.message(7004, topic_q, "other topic, same message id", Some(a2_msg));
    tokio::time::sleep(Duration::from_millis(500)).await;
    let got_p = agent_p.inbound();
    let got_q = agent_q.inbound();
    let got_n = agent_n.inbound();
    eprintln!("P inbound: {got_p:?}\nQ inbound: {got_q:?}\nN inbound: {got_n:?}");
    assert_eq!(got_p.len(), 3, "{got_p:?}");
    assert_eq!(
        got_p[0].1.get("target_agent").map(String::as_str),
        Some(A2)
    );
    assert!(got_p[1..].iter().all(|(_, meta)| !meta.contains_key("target_agent")));
    assert_eq!(got_q.len(), 1);
    assert!(!got_q[0].1.contains_key("target_agent"));
    assert!(got_n.is_empty(), "nested agent gets nothing: {got_n:?}");

    // Wait out the correlation window: still no ghost, internal or nested-sub block.
    tokio::time::sleep(Duration::from_millis(3500)).await;
    let blocks = fake.block_sends();
    eprintln!("block sends after the window: {blocks:?}");
    assert_eq!(
        blocks.iter().filter(|(_, t)| t.starts_with('↳')).count(),
        3,
        "exactly three subagent blocks"
    );
    assert_eq!(blocks.iter().filter(|(_, t)| t.starts_with('⇣')).count(), 1);
    for id in [GHOST, INTERNAL, "a7000000000000007"] {
        assert!(
            !fake.ops().iter().any(|op| format!("{op:?}").contains(id)),
            "{id} reached Telegram"
        );
    }
    assert!(
        !fake
            .sends()
            .iter()
            .any(|(_, t)| t.contains("NESTED-ANSWER") && !t.starts_with('⇣')),
        "nested answer only in its block"
    );
    assert_eq!(fake.creates(), 2, "no topic for nested or subagents");

    // A4 still running at restart.
    assert!(
        hub.fire(
            "SubagentStart",
            stdin(
                "SubagentStart",
                P,
                cwd_p,
                &tp,
                json!({"agent_id": A4, "agent_type": "Explore"})
            ),
            top(10)
        )
        .await
    );
    append(
        &tp,
        &(call_line("toolu_a4", "desc four") + &result_line("toolu_a4", A4)),
    );
    wait_for("A4 block", || fake.message(&format!("↳ Explore {A4}")).is_some()).await;
    let (a4_msg, _, _) = fake.message(&format!("↳ Explore {A4}")).unwrap();
    tokio::time::sleep(Duration::from_millis(800)).await;
    let saved = std::fs::read_to_string(state.join("registry.json")).expect("registry saved");
    assert!(saved.contains(A4) && !saved.contains(GHOST) && !saved.contains(INTERNAL));

    // ---------- restart ----------
    hub.stop();
    drop((agent_p, agent_q, agent_n));
    tokio::time::sleep(Duration::from_millis(300)).await;
    let fake2 = Fake::new(900, 5000);
    let hub = Hub::start(&state, fake2.clone()).await;
    let mut agent_p = AgentLink::connect(&hub.agent_addr, P, cwd_p, 10).await;
    tokio::time::sleep(Duration::from_millis(1000)).await;
    assert_eq!(fake2.creates(), 0, "no topic after restart: {:?}", fake2.ops());
    assert!(fake2.sends().is_empty(), "no send after restart: {:?}", fake2.ops());
    // Duplicate stop of A1 after restart: an edit at most, never a new message.
    assert!(
        hub.fire("SubagentStop", stop(A1, &a1_path, "LAST-A1"), top(10))
            .await
    );
    hub.message(7101, topic_p, "to A2 again", Some(a2_msg));
    tokio::time::sleep(Duration::from_millis(500)).await;
    let got = agent_p.inbound();
    assert_eq!(got.len(), 1, "{got:?}");
    assert_eq!(got[0].1.get("target_agent").map(String::as_str), Some(A2));
    // Parent ends with A4 still running -> deterministic lost mark by edit.
    assert!(
        hub.fire(
            "SessionEnd",
            stdin("SessionEnd", P, cwd_p, &tp, json!({"reason":"other"})),
            top(10)
        )
        .await
    );
    wait_for("A4 marked lost", || {
        fake2.ops().iter().any(|op| {
            matches!(op, Op::Edit { message_id, text, .. }
                if *message_id == a4_msg && text.ends_with("итог не получен"))
        })
    })
    .await;
    tokio::time::sleep(Duration::from_millis(300)).await;
    let block_sends2: Vec<_> = fake2
        .sends()
        .into_iter()
        .filter(|(_, t)| t.starts_with('↳') || t.starts_with('⇣'))
        .collect();
    assert!(block_sends2.is_empty(), "no block sent twice: {block_sends2:?}");
    assert_eq!(fake2.creates(), 0);
    eprintln!("run 2 ops: {:?}", fake2.ops());
    hub.stop();
    let _ = std::fs::remove_dir_all(&root);
}
