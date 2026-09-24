//! Live transcript stream end to end (TASK-016): the real `cctg agent`
//! binary (its own process, `CLAUDE_CONFIG_DIR` and home in a temp dir, so no
//! real config or `~/.cctg/device.env` is read) linked over real TCP to the
//! real `serve_agents`, the real `Slots` actor and `Scheduler`, and a fake
//! Telegram transport. The transcript is a temp file under
//! `<CLAUDE_CONFIG_DIR>/projects/<folder>/<session>.jsonl` written in pieces.
//! Adopted from the QA harness of TASK-016.

use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::atomic::{AtomicI64, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use cctg::device::canonical_cwd;
use cctg::hub::api::{ApiError, ForumTopic, Message};
use cctg::hub::ingress::serve_agents;
use cctg::hub::registry::RegistryStore;
use cctg::hub::scheduler::{BucketConfig, Delivery, Op, Outcome, Scheduler, Transport};
use cctg::hub::slots::{Control, Options, Slots};
use cctg::hub::updates::Inbound;
use cctg::wire::{HookEvent, HookPost, Secret};
use tokio::net::TcpListener;
use tokio::sync::mpsc;
use tokio::task::JoinHandle;

const SECRET: &str = "e2e-secret-0123456789abcdef";
const HOST: &str = "e2ebox";
const THREAD: i64 = 100;

// ---------------------------------------------------------------- fake Telegram

#[derive(Debug, Clone)]
struct Rec {
    op: Op,
    accepted: bool,
}

#[derive(Default)]
struct Fake {
    ops: Mutex<Vec<Rec>>,
    /// The next this many stream sends fail with a 502.
    refuse_streams: AtomicUsize,
    /// A stream send containing one of these fails once with a 502 (the
    /// entry is used up).
    refuse_once: Mutex<Vec<String>>,
    next_id: AtomicI64,
}

impl Transport for Fake {
    async fn execute(&self, op: &Op) -> Delivery {
        let id = 1000 + self.next_id.fetch_add(1, Ordering::SeqCst);
        let result: Delivery = match op {
            Op::CreateTopic { name, .. } => Ok(Outcome::Topic(ForumTopic {
                message_thread_id: THREAD,
                name: name.clone(),
                icon_custom_emoji_id: None,
            })),
            Op::Stream { text, .. } => {
                let refuse = self.take_refusal(text)
                    || self
                        .refuse_streams
                        .fetch_update(Ordering::SeqCst, Ordering::SeqCst, |n| n.checked_sub(1))
                        .is_ok();
                if refuse {
                    Err(ApiError::Telegram {
                        code: 502,
                        description: "Bad Gateway".into(),
                    })
                } else {
                    Ok(Outcome::Sent(Message {
                        message_id: id,
                        ..Message::default()
                    }))
                }
            }
            Op::Send { .. } | Op::SendDocument { .. } => Ok(Outcome::Sent(Message {
                message_id: id,
                ..Message::default()
            })),
            _ => Ok(Outcome::Done),
        };
        self.ops.lock().unwrap().push(Rec {
            op: op.clone(),
            accepted: result.is_ok(),
        });
        result
    }
}

impl Fake {
    fn take_refusal(&self, text: &str) -> bool {
        let mut once = self.refuse_once.lock().unwrap();
        let hit = once
            .iter()
            .position(|needle| text.contains(needle.as_str()));
        hit.map(|at| once.remove(at)).is_some()
    }
    fn recs(&self) -> Vec<Rec> {
        self.ops.lock().unwrap().clone()
    }
    /// Accepted stream messages, each split into its lines (merged messages
    /// hold several), plus accepted plain sends of the topic (answers) and
    /// documents (as `[document]`), in Telegram order.
    fn topic_lines(&self) -> Vec<String> {
        let mut out = Vec::new();
        for rec in self.recs() {
            if !rec.accepted {
                continue;
            }
            match rec.op {
                Op::Stream { text, .. } => out.extend(text.lines().map(str::to_owned)),
                Op::SendDocument { .. } => out.push("[document]".into()),
                Op::Send {
                    text, thread_id, ..
                } if thread_id == Some(THREAD) && !text.starts_with("──") => out.push(text),
                _ => {}
            }
        }
        out
    }
    fn stream_messages(&self) -> Vec<String> {
        self.recs()
            .into_iter()
            .filter(|r| r.accepted)
            .filter_map(|r| match r.op {
                Op::Stream { text, .. } => Some(text),
                _ => None,
            })
            .collect()
    }
    fn reactions(&self) -> Vec<(i64, String)> {
        self.recs()
            .into_iter()
            .filter_map(|r| match r.op {
                Op::React { message_id, emoji } => Some((message_id, emoji)),
                _ => None,
            })
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

impl Hub {
    fn stop(self) {
        for task in self.tasks {
            task.abort();
        }
    }
}

fn fast_bucket() -> BucketConfig {
    BucketConfig {
        capacity: 1000,
        refill_every: Duration::from_millis(1),
        min_gap: Duration::ZERO,
    }
}

fn options() -> Options {
    Options {
        grace: Duration::ZERO,
        stream_every: Duration::from_millis(50),
        stream_retry: Duration::from_millis(200),
        ..Options::default()
    }
}

async fn start_hub(
    state: &Path,
    listener: TcpListener,
    bucket: BucketConfig,
    fake: Arc<Fake>,
) -> Hub {
    let (scheduler, outbox) = Scheduler::new(fake.clone(), bucket);
    let sched = tokio::spawn(scheduler.run());
    let store = RegistryStore::open(state).expect("store");
    let registry = store.load().expect("load registry");
    let (slots, _view) = Slots::new(registry, store, outbox, options());
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

// ---------------------------------------------------------------- session fixture

struct Session {
    _root: TempRoot,
    id: String,
    transcript: PathBuf,
    workdir: PathBuf,
    config: PathBuf,
    home: PathBuf,
    state: PathBuf,
}

struct TempRoot(PathBuf);
impl Drop for TempRoot {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

fn session(name: &str, n: u32) -> Session {
    let root = std::env::temp_dir().join(format!("cctg-stream-e2e-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    let config = root.join("cfg");
    let project = config.join("projects").join("C--qa-w");
    std::fs::create_dir_all(&project).unwrap();
    let workdir = root.join("w");
    std::fs::create_dir_all(&workdir).unwrap();
    let home = root.join("home");
    std::fs::create_dir_all(&home).unwrap();
    let state = root.join("state");
    std::fs::create_dir_all(&state).unwrap();
    let id = format!("0a16e2e0-0000-4000-8000-{n:012}");
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
    fn cwd(&self) -> String {
        canonical_cwd(&self.workdir.to_string_lossy())
    }
    fn post(&self, event: HookEvent) -> HookPost {
        HookPost::new(
            HOST.into(),
            self.id.clone(),
            self.cwd(),
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
        file.flush().unwrap();
    }
}

async fn start_session(hub: &Hub, s: &Session, source: &str) {
    hub.hooks
        .send(s.post(HookEvent::SessionStart {
            source: Some(source.into()),
            claude_pid: Some(4242),
            parent_claude_pid: None,
        }))
        .await
        .unwrap();
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
    let mut child = Command::new(env!("CARGO_BIN_EXE_cctg"))
        .arg("agent")
        .current_dir(&s.workdir)
        .env_remove("CLAUDE_CODE_ENTRYPOINT")
        .env_remove("CCTG_HUB_HOOK_ADDR")
        .env("CCTG_HUB_SECRET", SECRET)
        .env("CCTG_HUB_AGENT_ADDR", format!("127.0.0.1:{port}"))
        .env("CCTG_HOST", HOST)
        .env("CLAUDE_CODE_SESSION_ID", &s.id)
        .env("CLAUDE_CONFIG_DIR", &s.config)
        .env("USERPROFILE", &s.home)
        .env("HOME", &s.home)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
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

// ---------------------------------------------------------------- records

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
fn result(id: &str, error: bool) -> String {
    format!(
        "{}\n",
        serde_json::json!({"type":"user","message":{"role":"user","content":[
            {"type":"tool_result","tool_use_id":id,"content": if error {"boom failed"} else {"ok"},"is_error":error}]}})
    )
}
fn answer(text: &str) -> String {
    format!(
        "{}\n",
        serde_json::json!({"type":"assistant","message":{"role":"assistant","stop_reason":"end_turn","content":[
            {"type":"text","text":text}]}})
    )
}
fn channel(source: &str, message_id: i64) -> String {
    let tag = format!(
        "<channel source=\"{source}\" chat_id=\"-100\" message_id=\"{message_id}\" thread_id=\"{THREAD}\">\nhello\n</channel>"
    );
    format!(
        "{}\n",
        serde_json::json!({"type":"user","isMeta":true,"origin":{"kind":"channel"},"message":{"role":"user","content":tag}})
    )
}

async fn wait_for(what: &str, secs: u64, mut cond: impl FnMut() -> bool) {
    let deadline = Instant::now() + Duration::from_secs(secs);
    while !cond() {
        assert!(Instant::now() < deadline, "timed out waiting for: {what}");
        tokio::time::sleep(Duration::from_millis(25)).await;
    }
}

async fn listener() -> (TcpListener, u16) {
    let l = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = l.local_addr().unwrap().port();
    (l, port)
}

fn assert_no_dup(lines: &[String]) {
    let mut seen = std::collections::HashSet::new();
    for line in lines {
        assert!(
            seen.insert(line),
            "duplicate topic line {line:?} in {lines:?}"
        );
    }
}

fn ok_line(desc: &str) -> String {
    format!("• Bash: {desc} ✓")
}

// ---------------------------------------------------------------- tests

/// AC1/AC2/AC8 + answer ordering: pieces, a partial last line, results out
/// of call order, a Stop that comes before its transcript turn end.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn e2e_order_partial_line_and_stop_after_lines() {
    let s = session("order", 1);
    let (l, port) = listener().await;
    let hub = start_hub(&s.state, l, fast_bucket(), Arc::new(Fake::default())).await;
    start_session(&hub, &s, "startup").await;
    let _agent = start_agent(&s, port);
    let fake = hub.fake.clone();
    // The slot topic exists first.
    wait_for("topic", 20, || {
        fake.recs()
            .iter()
            .any(|r| matches!(&r.op, Op::CreateTopic { .. }))
    })
    .await;

    s.append(&prompt("go"));
    s.append(&call("ta", "step A"));
    s.append(&call("tb", "step B"));
    s.append(&result("tb", false));
    let ra = result("ta", false);
    let (head, tail) = ra.split_at(ra.len() / 2);
    s.append(head); // partial last line
    wait_for("prompt", 20, || !fake.topic_lines().is_empty()).await;
    tokio::time::sleep(Duration::from_millis(1500)).await;
    assert_eq!(
        fake.topic_lines(),
        ["> go"],
        "B waits for A; the partial line is not sent"
    );
    s.append(tail);
    wait_for("A and B", 20, || fake.topic_lines().len() >= 3).await;
    assert_eq!(
        fake.topic_lines(),
        ["> go", &ok_line("step A"), &ok_line("step B")]
    );

    // Stop arrives before its last lines reach the file.
    hub.hooks
        .send(s.post(HookEvent::Stop {
            prompt_id: None,
            last_assistant_message: Some("FINAL ANSWER one".into()),
        }))
        .await
        .unwrap();
    tokio::time::sleep(Duration::from_millis(400)).await;
    s.append(&call("tc", "step C"));
    s.append(&result("tc", true));
    s.append(&answer("FINAL ANSWER one"));
    wait_for("answer", 20, || {
        fake.topic_lines().iter().any(|l| l == "FINAL ANSWER one")
    })
    .await;
    tokio::time::sleep(Duration::from_millis(1500)).await;
    let lines = fake.topic_lines();
    assert_eq!(
        lines,
        [
            "> go".to_owned(),
            ok_line("step A"),
            ok_line("step B"),
            "• Bash: step C ✗ boom failed".to_owned(),
            "FINAL ANSWER one".to_owned(),
        ]
    );
    assert_no_dup(&lines);
    hub.stop();
}

fn thinking(text: &str) -> String {
    format!(
        "{}\n",
        serde_json::json!({"type":"assistant","message":{"role":"assistant","stop_reason":"tool_use","content":[
            {"type":"thinking","thinking":text,"signature":"sig"}]}})
    )
}
fn reply_call(id: &str) -> String {
    format!(
        "{}\n",
        serde_json::json!({"type":"assistant","message":{"role":"assistant","stop_reason":"tool_use","content":[
            {"type":"tool_use","id":id,"name":"mcp__cctg__reply","input":{"text":"status"}}]}})
    )
}

/// TASK-025 over a real agent link: visible thinking goes as its own 💭
/// message in turn order, signature-only thinking and the `reply` call give
/// nothing, and the answer still follows the turn's lines.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn e2e_thinking_shows_and_the_reply_call_does_not() {
    let s = session("think", 12);
    let (l, port) = listener().await;
    let hub = start_hub(&s.state, l, fast_bucket(), Arc::new(Fake::default())).await;
    start_session(&hub, &s, "startup").await;
    let _agent = start_agent(&s, port);
    let fake = hub.fake.clone();
    wait_for("topic", 20, || {
        fake.recs()
            .iter()
            .any(|r| matches!(&r.op, Op::CreateTopic { .. }))
    })
    .await;

    s.append(&prompt("go"));
    // The stream runs before the turn goes on (as in the order test).
    wait_for("prompt", 20, || !fake.topic_lines().is_empty()).await;
    s.append(&thinking("Checking cargo."));
    s.append(&reply_call("tr"));
    s.append(&result("tr", false));
    s.append(&thinking(""));
    s.append(&call("ta", "step A"));
    s.append(&result("ta", false));
    s.append(&thinking("Both done."));
    s.append(&answer("DONE"));
    hub.hooks
        .send(s.post(HookEvent::Stop {
            prompt_id: None,
            last_assistant_message: Some("DONE".into()),
        }))
        .await
        .unwrap();
    wait_for("answer", 20, || {
        fake.topic_lines().iter().any(|l| l == "DONE")
    })
    .await;
    tokio::time::sleep(Duration::from_millis(1500)).await;
    let lines = fake.topic_lines();
    assert_eq!(
        lines,
        [
            "> go".to_owned(),
            "\u{1F4AD} Checking cargo.".to_owned(),
            ok_line("step A"),
            "\u{1F4AD} Both done.".to_owned(),
            "DONE".to_owned(),
        ]
    );
    assert_no_dup(&lines);
    hub.stop();
}

/// Stream messages Telegram refuses (502) are neither lost nor overtaken.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn e2e_refused_sends_lose_nothing() {
    let s = session("refuse", 2);
    let (l, port) = listener().await;
    let fake = Arc::new(Fake::default());
    let hub = start_hub(&s.state, l, fast_bucket(), fake.clone()).await;
    start_session(&hub, &s, "startup").await;
    let _agent = start_agent(&s, port);
    wait_for("topic", 20, || {
        fake.recs()
            .iter()
            .any(|r| matches!(&r.op, Op::CreateTopic { .. }))
    })
    .await;
    fake.refuse_streams.store(3, Ordering::SeqCst);
    s.append(&prompt("refuse run"));
    let mut want = vec!["> refuse run".to_owned()];
    for n in 0..4 {
        s.append(&call(&format!("r{n}"), &format!("refused {n}")));
        s.append(&result(&format!("r{n}"), false));
        want.push(ok_line(&format!("refused {n}")));
    }
    wait_for("every line accepted once", 30, || {
        let got = fake.topic_lines();
        want.iter().all(|w| got.contains(w))
    })
    .await;
    tokio::time::sleep(Duration::from_millis(800)).await;
    let got = fake.topic_lines();
    let refused = fake.recs().iter().filter(|r| !r.accepted).count();
    assert!(refused >= 3, "the fake refused {refused}");
    // Every line once, in call order: nothing queued behind a refused line
    // shows before it.
    assert_eq!(got, want);
    hub.stop();
}

fn committed_offset(state: &Path, session: &str) -> Option<u64> {
    let text = std::fs::read_to_string(state.join("registry.json")).ok()?;
    let value: serde_json::Value = serde_json::from_str(&text).ok()?;
    value["sessions"][session]["stream"]["offset"].as_u64()
}

/// AC3: a hub restart from the saved registry neither repeats accepted lines
/// nor loses lines written while it was down.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn e2e_hub_restart_neither_repeats_nor_loses() {
    let s = session("restart", 3);
    let (l, port) = listener().await;
    let first = start_hub(&s.state, l, fast_bucket(), Arc::new(Fake::default())).await;
    start_session(&first, &s, "startup").await;
    let _agent = start_agent(&s, port);
    let fake1 = first.fake.clone();
    s.append(&prompt("before restart"));
    s.append(&call("k1", "one"));
    s.append(&result("k1", false));
    wait_for("hub 1 lines", 20, || fake1.topic_lines().len() >= 2).await;
    let len = std::fs::metadata(&s.transcript).unwrap().len();
    wait_for("offset committed", 20, || {
        committed_offset(&s.state, &s.id) == Some(len)
    })
    .await;
    first.stop();
    tokio::time::sleep(Duration::from_millis(300)).await;
    // Written while the hub is down.
    s.append(&call("k2", "two"));
    s.append(&result("k2", false));
    s.append(&prompt("while down"));
    let fake2 = Arc::new(Fake::default());
    let l2 = loop {
        match TcpListener::bind(("127.0.0.1", port)).await {
            Ok(l) => break l,
            Err(_) => tokio::time::sleep(Duration::from_millis(100)).await,
        }
    };
    let second = start_hub(&s.state, l2, fast_bucket(), fake2.clone()).await;
    wait_for("hub 2 lines", 60, || fake2.topic_lines().len() >= 2).await;
    tokio::time::sleep(Duration::from_millis(1500)).await;
    assert_eq!(
        fake1.topic_lines(),
        ["> before restart".to_owned(), ok_line("one")]
    );
    assert_eq!(
        fake2
            .stream_messages()
            .iter()
            .flat_map(|m| m.lines().map(str::to_owned))
            .collect::<Vec<_>>(),
        [ok_line("two"), "> while down".to_owned()],
        "hub 2 streams only what hub 1 did not"
    );
    second.stop();
}

/// AC7: 👀 on hand-off; ✍ only for the cctg channel record of that id.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn e2e_reactions() {
    let s = session("react", 4);
    let (l, port) = listener().await;
    let fake = Arc::new(Fake::default());
    let hub = start_hub(&s.state, l, fast_bucket(), fake.clone()).await;
    start_session(&hub, &s, "startup").await;
    let _agent = start_agent(&s, port);
    wait_for("topic", 20, || {
        fake.recs()
            .iter()
            .any(|r| matches!(&r.op, Op::CreateTopic { .. }))
    })
    .await;
    // The agent must be bound before the message is routed.
    s.append(&prompt("warm up"));
    wait_for("stream up", 20, || !fake.topic_lines().is_empty()).await;
    hub.control
        .send(Control::Message(Inbound {
            message_id: 555,
            thread_id: Some(THREAD),
            text: Some("from telegram".into()),
            reply_to: None,
            quote: None,
            forwarded: false,
        }))
        .unwrap();
    wait_for("eyes", 10, || fake.reactions() == [(555, "👀".to_owned())]).await;
    hub.hooks
        .send(s.post(HookEvent::UserPromptSubmit { prompt_id: None }))
        .await
        .unwrap();
    s.append(&prompt("typed in terminal"));
    s.append(&channel("webhook", 555));
    s.append(&channel("cctg", 777));
    wait_for("terminal prompt", 10, || {
        fake.topic_lines()
            .iter()
            .any(|l| l == "> typed in terminal")
    })
    .await;
    tokio::time::sleep(Duration::from_millis(800)).await;
    assert_eq!(
        fake.reactions(),
        [(555, "👀".to_owned())],
        "nothing but its own record turns it"
    );
    s.append(&channel("cctg", 555));
    wait_for("writing", 10, || fake.reactions().len() == 2).await;
    s.append(&channel("cctg", 555));
    s.append(&prompt("after"));
    wait_for("after", 10, || {
        fake.topic_lines().iter().any(|l| l == "> after")
    })
    .await;
    tokio::time::sleep(Duration::from_millis(500)).await;
    assert_eq!(
        fake.reactions(),
        [(555, "👀".to_owned()), (555, "✍".to_owned())]
    );
    hub.stop();
}

/// AC4/AC8: under the group limit consecutive tool lines of the topic merge,
/// in order, without loss.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn e2e_merge_under_the_limit() {
    let s = session("merge", 5);
    let (l, port) = listener().await;
    let fake = Arc::new(Fake::default());
    let bucket = BucketConfig {
        capacity: 3,
        refill_every: Duration::from_millis(700),
        min_gap: Duration::ZERO,
    };
    let hub = start_hub(&s.state, l, bucket, fake.clone()).await;
    start_session(&hub, &s, "startup").await;
    let _agent = start_agent(&s, port);
    wait_for("topic", 30, || {
        fake.recs()
            .iter()
            .any(|r| matches!(&r.op, Op::CreateTopic { .. }))
    })
    .await;
    let mut want = vec!["> merge run".to_owned()];
    let mut batch = prompt("merge run");
    for n in 0..15 {
        batch.push_str(&call(&format!("m{n}"), &format!("merged {n:02}")));
        batch.push_str(&result(&format!("m{n}"), false));
        want.push(ok_line(&format!("merged {n:02}")));
    }
    s.append(&batch);
    wait_for("all merged lines", 60, || {
        fake.topic_lines().len() >= want.len()
    })
    .await;
    tokio::time::sleep(Duration::from_millis(1500)).await;
    let got = fake.topic_lines();
    assert_eq!(got, want);
    let messages = fake.stream_messages();
    eprintln!(
        "{} stream messages for {} lines: {messages:?}",
        messages.len(),
        want.len()
    );
    assert!(messages.len() < want.len(), "lines merged under the limit");
    hub.stop();
}

/// A resumed session starts at the end of the file
/// (offset None). If that end is not a line end (a record being written, or a
/// torn last line), the history must still not be streamed again.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn e2e_resume_at_a_torn_end_does_not_replay_history() {
    let s = session("resume", 6);
    for n in 0..5 {
        s.append(&prompt(&format!("history {n}")));
    }
    let last = prompt("being written");
    let (head, tail) = last.split_at(last.len() / 2);
    s.append(head);
    let (l, port) = listener().await;
    let fake = Arc::new(Fake::default());
    let hub = start_hub(&s.state, l, fast_bucket(), fake.clone()).await;
    start_session(&hub, &s, "resume").await;
    let _agent = start_agent(&s, port);
    wait_for("offset set", 20, || {
        committed_offset(&s.state, &s.id).is_some()
    })
    .await;
    s.append(tail);
    s.append(&prompt("new after resume"));
    wait_for("new line", 20, || {
        fake.topic_lines().iter().any(|l| l == "> new after resume")
    })
    .await;
    tokio::time::sleep(Duration::from_millis(1000)).await;
    let got = fake.topic_lines();
    assert!(
        !got.iter().any(|l| l.starts_with("> history")),
        "history replayed after resume: {got:?}"
    );
    hub.stop();
}

/// The first appearance of each line, in order (a refused message is sent
/// again, so accepted lines may repeat).
fn firsts(lines: &[String]) -> Vec<String> {
    let mut seen = std::collections::HashSet::new();
    lines
        .iter()
        .filter(|line| seen.insert(line.as_str()))
        .cloned()
        .collect()
}

/// TASK-023 (QA of TASK-016, BUG-1): Telegram refuses stream messages of a
/// turn (502) while its answer is held; the answer still shows only after
/// every tool line of its turn, in time, and nothing is lost.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn e2e_answer_after_a_refused_line_still_follows_its_lines() {
    let s = session("refuse-answer", 7);
    let (l, port) = listener().await;
    let fake = Arc::new(Fake::default());
    // Under the group limit, so tool lines merge and a refusal takes several.
    let bucket = BucketConfig {
        capacity: 2,
        refill_every: Duration::from_millis(300),
        min_gap: Duration::ZERO,
    };
    let hub = start_hub(&s.state, l, bucket, fake.clone()).await;
    start_session(&hub, &s, "startup").await;
    let _agent = start_agent(&s, port);
    wait_for("topic", 20, || {
        fake.recs()
            .iter()
            .any(|r| matches!(&r.op, Op::CreateTopic { .. }))
    })
    .await;
    // The agent is bound and the stream reads before the turn starts.
    s.append(&prompt("gamma warm"));
    wait_for("stream up", 20, || fake.topic_lines().len() == 1).await;
    *fake.refuse_once.lock().unwrap() = vec!["c05".into(), "c11".into()];
    let mut want = vec!["> gamma warm".to_owned(), "> gamma run".to_owned()];
    let mut batch = prompt("gamma run");
    for n in 0..16 {
        batch.push_str(&call(&format!("g{n}"), &format!("c{n:02}")));
        batch.push_str(&result(&format!("g{n}"), false));
        want.push(ok_line(&format!("c{n:02}")));
    }
    batch.push_str(&answer("GAMMA DONE"));
    want.push("GAMMA DONE".into());
    // Written in three pieces, the middle one ending mid-line; the Stop
    // comes after the first.
    let third = batch.len() / 3;
    let (p1, rest) = batch.split_at(third);
    let (p2, p3) = rest.split_at(third);
    s.append(p1);
    hub.hooks
        .send(s.post(HookEvent::Stop {
            prompt_id: None,
            last_assistant_message: Some("GAMMA DONE".into()),
        }))
        .await
        .unwrap();
    let stopped = Instant::now();
    tokio::time::sleep(Duration::from_millis(200)).await;
    s.append(p2);
    tokio::time::sleep(Duration::from_millis(200)).await;
    s.append(p3);
    wait_for("everything once", 60, || {
        let got = fake.topic_lines();
        want.iter().all(|w| got.contains(w))
    })
    .await;
    // Not later than the hold plus one rewind cycle (and a loaded host).
    let bound = Options::default().hold_answer + options().stream_retry + Duration::from_secs(3);
    assert!(
        stopped.elapsed() < bound,
        "answer after {:?}",
        stopped.elapsed()
    );
    tokio::time::sleep(Duration::from_millis(1500)).await;
    let got = fake.topic_lines();
    for rec in fake.recs() {
        match &rec.op {
            Op::Stream { text, restart, .. } => eprintln!(
                "stream accepted={} restart={restart} {:?}",
                rec.accepted,
                text.lines().collect::<Vec<_>>()
            ),
            Op::Send { text, .. } => eprintln!("send accepted={} {text:?}", rec.accepted),
            _ => {}
        }
    }
    let refused = fake.recs().iter().filter(|r| !r.accepted).count();
    assert_eq!(refused, 2, "both refusals happened");
    assert_eq!(firsts(&got), want, "got {got:?}");
    assert_eq!(
        got.iter().filter(|line| *line == "GAMMA DONE").count(),
        1,
        "the answer once: {got:?}"
    );
    hub.stop();
}

/// Every stream and plain send, for a failing test's output.
fn dump(fake: &Fake) {
    for rec in fake.recs() {
        match &rec.op {
            Op::Stream { text, restart, .. } => eprintln!(
                "stream accepted={} restart={restart} {:?}",
                rec.accepted,
                text.lines()
                    .map(|l| l.chars().take(40).collect::<String>())
                    .collect::<Vec<_>>()
            ),
            Op::Send { text, .. } => eprintln!("send accepted={} {text:?}", rec.accepted),
            Op::SendDocument { .. } => eprintln!("document accepted={}", rec.accepted),
            _ => {}
        }
    }
}

/// Accepted stream messages containing `needle`.
fn accepted_count(fake: &Fake, needle: &str) -> usize {
    fake.recs()
        .iter()
        .filter(|r| r.accepted)
        .filter(|r| matches!(&r.op, Op::Stream { text, .. } if text.contains(needle)))
        .count()
}

async fn stop(hub: &Hub, s: &Session, text: &str) {
    hub.hooks
        .send(s.post(HookEvent::Stop {
            prompt_id: None,
            last_assistant_message: Some(text.into()),
        }))
        .await
        .unwrap();
}

/// One turn: its transcript bytes and its topic lines (answer last).
fn turn(name: &str, calls: usize, answer_text: &str) -> (String, Vec<String>) {
    let mut bytes = prompt(name);
    let mut lines = vec![format!("> {name}")];
    for n in 0..calls {
        let id = format!("{name}c{n}");
        bytes.push_str(&call(&id, &id));
        bytes.push_str(&result(&id, false));
        lines.push(ok_line(&id));
    }
    bytes.push_str(&answer(answer_text));
    lines.push(answer_text.to_owned());
    (bytes, lines)
}

/// TASK-023 (QA, adopted): several turns under a slow Telegram (turn ends of
/// two turns land in one read chunk), refusals of a tool line, of an answer
/// and of a prompt line at different points, Stops both before and after
/// their transcript turn ends. Every answer shows exactly once, after all
/// lines of its own turn and before the next turn's prompt.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn e2e_turns_with_refusals_under_a_slow_telegram_keep_their_answers_in_place() {
    let s = session("multi-refuse", 8);
    let (l, port) = listener().await;
    let fake = Arc::new(Fake::default());
    let bucket = BucketConfig {
        capacity: 1,
        refill_every: Duration::from_millis(250),
        min_gap: Duration::ZERO,
    };
    let hub = start_hub(&s.state, l, bucket, fake.clone()).await;
    start_session(&hub, &s, "startup").await;
    let _agent = start_agent(&s, port);
    wait_for("topic", 20, || {
        fake.recs()
            .iter()
            .any(|r| matches!(&r.op, Op::CreateTopic { .. }))
    })
    .await;
    s.append(&prompt("warm"));
    wait_for("stream up", 20, || fake.topic_lines().len() == 1).await;
    *fake.refuse_once.lock().unwrap() = vec![
        "t1c1".into(),  // a tool line of turn 1
        "ANS-2".into(), // the answer of turn 2
        "> t3".into(),  // the prompt line of turn 3
        "t4c0".into(),  // a tool line of turn 4 (its answer is claimed)
        "ANS-5".into(), // the answer of turn 5
    ];
    let mut want = vec!["> warm".to_owned()];
    let (t1, w1) = turn("t1", 3, "ANS-1");
    let (t2, w2) = turn("t2", 2, "ANS-2");
    let (t3, w3) = turn("t3", 0, "ANS-3");
    let (t4, w4) = turn("t4", 2, "ANS-4");
    let (t5, w5) = turn("t5", 1, "ANS-5");
    for w in [&w1, &w2, &w3, &w4, &w5] {
        want.extend(w.iter().cloned());
    }
    // Both Stops held before their turns reach the file, then both turns in
    // one write (two turn ends in one read chunk).
    stop(&hub, &s, "ANS-1").await;
    stop(&hub, &s, "ANS-2").await;
    tokio::time::sleep(Duration::from_millis(100)).await;
    s.append(&(t1 + &t2));
    // Turns 3 and 4 in one piece while Telegram is still behind; their Stops
    // come after the file (claim path).
    tokio::time::sleep(Duration::from_millis(300)).await;
    s.append(&(t3 + &t4));
    tokio::time::sleep(Duration::from_millis(200)).await;
    stop(&hub, &s, "ANS-3").await;
    stop(&hub, &s, "ANS-4").await;
    // Turn 5: Stop first, file 300 ms later.
    tokio::time::sleep(Duration::from_millis(300)).await;
    stop(&hub, &s, "ANS-5").await;
    tokio::time::sleep(Duration::from_millis(300)).await;
    s.append(&t5);
    wait_for("everything", 90, || {
        let got = fake.topic_lines();
        want.iter().all(|w| got.contains(w))
    })
    .await;
    tokio::time::sleep(Duration::from_millis(2000)).await;
    dump(&fake);
    let got = fake.topic_lines();
    let refused = fake.recs().iter().filter(|r| !r.accepted).count();
    assert_eq!(refused, 5, "every planned refusal happened");
    assert_eq!(firsts(&got), want, "got {got:?}");
    for n in 1..=5 {
        let a = format!("ANS-{n}");
        assert_eq!(accepted_count(&fake, &a), 1, "{a} once: {got:?}");
    }
    // Every appearance of an answer comes after at least one appearance of
    // each line of its turn.
    for w in [&w1, &w2, &w3, &w4, &w5] {
        let ans = w.last().unwrap();
        let at = got.iter().position(|l| l == ans).unwrap();
        for line in &w[..w.len() - 1] {
            assert!(got[..at].contains(line), "{ans} before {line}: {got:?}");
        }
    }
    hub.stop();
}

/// TASK-023 (QA BUG-1): turn 1's answer is long and goes as a document,
/// outside the stream; turn 2's prompt line is refused, so the rewind reads
/// turn 1's end again. That end is answered and must not be claimable: turn
/// 3's Stop comes before its lines reach the file (the usual order) and its
/// answer still waits for its own lines.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn e2e_a_document_answer_read_again_does_not_let_the_next_answer_go_early() {
    let s = session("doc-answer", 9);
    let (l, port) = listener().await;
    let fake = Arc::new(Fake::default());
    let hub = start_hub(&s.state, l, fast_bucket(), fake.clone()).await;
    start_session(&hub, &s, "startup").await;
    let _agent = start_agent(&s, port);
    wait_for("topic", 20, || {
        fake.recs()
            .iter()
            .any(|r| matches!(&r.op, Op::CreateTopic { .. }))
    })
    .await;
    s.append(&prompt("warm"));
    wait_for("stream up", 20, || fake.topic_lines().len() == 1).await;
    *fake.refuse_once.lock().unwrap() = vec!["> t2".into()];
    let big: String = (0..600)
        .map(|n| format!("big answer line {n:04} ....................\n"))
        .collect();
    let (t1, _) = turn("t1", 1, &big);
    let (t2, w2) = turn("t2", 1, "ANS-2");
    let (t3, w3) = turn("t3", 1, "ANS-3");
    stop(&hub, &s, &big).await;
    stop(&hub, &s, "ANS-2").await;
    tokio::time::sleep(Duration::from_millis(100)).await;
    s.append(&(t1 + &t2));
    wait_for("ANS-2", 30, || {
        fake.topic_lines().iter().any(|l| l == "ANS-2")
    })
    .await;
    // Turn 3: its Stop first, its lines 500 ms later.
    stop(&hub, &s, "ANS-3").await;
    tokio::time::sleep(Duration::from_millis(500)).await;
    s.append(&t3);
    wait_for("ANS-3", 30, || {
        fake.topic_lines().iter().any(|l| l == "ANS-3")
    })
    .await;
    tokio::time::sleep(Duration::from_millis(1000)).await;
    dump(&fake);
    let got = fake.topic_lines();
    let mut want = vec![
        "> warm".to_owned(),
        "> t1".into(),
        ok_line("t1c0"),
        "[document]".into(),
    ];
    want.extend(w2.iter().cloned());
    want.extend(w3.iter().cloned());
    assert_eq!(firsts(&got), want, "got {got:?}");
    hub.stop();
}

/// Asserts the topic lines (first appearances) equals `want` and every answer was
/// accepted exactly once.
fn check_answers(fake: &Fake, want: &[String], answers: &[&str]) {
    dump(fake);
    let got = fake.topic_lines();
    assert_eq!(firsts(&got), want, "got {got:?}");
    for a in answers {
        assert_eq!(accepted_count(fake, a), 1, "{a} accepted once: {got:?}");
    }
}

/// TASK-023 (QA BUG-2): turn 2's prompt line is refused twice, so two
/// rewinds read turn 1's end again while its answer is in the topic. Both
/// reads must leave that end answered, not claimable: turn 3's Stop comes
/// before its lines and its answer still waits for them.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn e2e_a_turn_end_read_again_by_two_rewinds_does_not_let_the_next_answer_go_early() {
    let s = session("double-rewind", 10);
    let (l, port) = listener().await;
    let fake = Arc::new(Fake::default());
    let hub = start_hub(&s.state, l, fast_bucket(), fake.clone()).await;
    start_session(&hub, &s, "startup").await;
    let _agent = start_agent(&s, port);
    wait_for("topic", 20, || {
        fake.recs()
            .iter()
            .any(|r| matches!(&r.op, Op::CreateTopic { .. }))
    })
    .await;
    s.append(&prompt("warm"));
    wait_for("stream up", 20, || fake.topic_lines().len() == 1).await;
    *fake.refuse_once.lock().unwrap() = vec!["> t2".into(), "> t2".into()];
    let (t1, w1) = turn("t1", 1, "ANS-1");
    let (t2, w2) = turn("t2", 1, "ANS-2");
    let (t3, w3) = turn("t3", 1, "ANS-3");
    stop(&hub, &s, "ANS-1").await;
    stop(&hub, &s, "ANS-2").await;
    tokio::time::sleep(Duration::from_millis(100)).await;
    s.append(&(t1 + &t2));
    wait_for("ANS-2", 30, || {
        fake.topic_lines().iter().any(|l| l == "ANS-2")
    })
    .await;
    // Turn 3: its Stop first (the usual order), its lines 500 ms later.
    stop(&hub, &s, "ANS-3").await;
    tokio::time::sleep(Duration::from_millis(500)).await;
    s.append(&t3);
    wait_for("ANS-3", 30, || {
        fake.topic_lines().iter().any(|l| l == "ANS-3")
    })
    .await;
    tokio::time::sleep(Duration::from_millis(1000)).await;
    let refused = fake.recs().iter().filter(|r| !r.accepted).count();
    let mut want = vec!["> warm".to_owned()];
    for w in [&w1, &w2, &w3] {
        want.extend(w.iter().cloned());
    }
    check_answers(&fake, &want, &["ANS-1", "ANS-2", "ANS-3"]);
    assert_eq!(refused, 2);
    hub.stop();
}

/// Broad acceptance run: six turns under a slow Telegram (two turn ends in
/// one read chunk twice), refusals of tool lines, of answers and of a prompt
/// line at different points, Stops before and after their turn ends.
/// Every answer exactly once, right after its own turn's lines, none lost.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn e2e_six_turns_with_refusals_keep_every_answer_in_place() {
    let s = session("six-turns", 11);
    let (l, port) = listener().await;
    let fake = Arc::new(Fake::default());
    let bucket = BucketConfig {
        capacity: 1,
        refill_every: Duration::from_millis(200),
        min_gap: Duration::ZERO,
    };
    let hub = start_hub(&s.state, l, bucket, fake.clone()).await;
    start_session(&hub, &s, "startup").await;
    let _agent = start_agent(&s, port);
    wait_for("topic", 20, || {
        fake.recs()
            .iter()
            .any(|r| matches!(&r.op, Op::CreateTopic { .. }))
    })
    .await;
    s.append(&prompt("warm"));
    wait_for("stream up", 20, || fake.topic_lines().len() == 1).await;
    *fake.refuse_once.lock().unwrap() = vec![
        "t1c0".into(),  // a tool line of turn 1
        "ANS-1".into(), // the answer of turn 1
        "t2c1".into(),  // a tool line of turn 2
        "ANS-4".into(), // the claimed answer of turn 4
        "> t5".into(),  // the prompt line of turn 5
        "t6c0".into(),  // a tool line of turn 6
    ];
    let (t1, w1) = turn("t1", 2, "ANS-1");
    let (t2, w2) = turn("t2", 2, "ANS-2");
    let (t3, w3) = turn("t3", 1, "ANS-3");
    let (t4, w4) = turn("t4", 1, "ANS-4");
    let (t5, w5) = turn("t5", 2, "ANS-5");
    let (t6, w6) = turn("t6", 1, "ANS-6");
    let mut want = vec!["> warm".to_owned()];
    for w in [&w1, &w2, &w3, &w4, &w5, &w6] {
        want.extend(w.iter().cloned());
    }
    // Turns 1+2: Stops held first, both turns in one write.
    stop(&hub, &s, "ANS-1").await;
    stop(&hub, &s, "ANS-2").await;
    tokio::time::sleep(Duration::from_millis(100)).await;
    s.append(&(t1 + &t2));
    // Turns 3+4 in one write while Telegram is behind; Stops after (claim).
    tokio::time::sleep(Duration::from_millis(400)).await;
    s.append(&(t3 + &t4));
    tokio::time::sleep(Duration::from_millis(250)).await;
    stop(&hub, &s, "ANS-3").await;
    stop(&hub, &s, "ANS-4").await;
    wait_for("ANS-4", 60, || {
        fake.topic_lines().iter().any(|l| l == "ANS-4")
    })
    .await;
    // Turn 5: Stop first, file 300 ms later.
    stop(&hub, &s, "ANS-5").await;
    tokio::time::sleep(Duration::from_millis(300)).await;
    s.append(&t5);
    wait_for("ANS-5", 60, || {
        fake.topic_lines().iter().any(|l| l == "ANS-5")
    })
    .await;
    // Turn 6: file first, Stop 300 ms later.
    s.append(&t6);
    tokio::time::sleep(Duration::from_millis(300)).await;
    stop(&hub, &s, "ANS-6").await;
    wait_for("everything", 90, || {
        let got = fake.topic_lines();
        want.iter().all(|w| got.contains(w))
    })
    .await;
    tokio::time::sleep(Duration::from_millis(2000)).await;
    let refused = fake.recs().iter().filter(|r| !r.accepted).count();
    check_answers(
        &fake,
        &want,
        &["ANS-1", "ANS-2", "ANS-3", "ANS-4", "ANS-5", "ANS-6"],
    );
    assert_eq!(refused, 6, "every planned refusal happened");
    hub.stop();
}
