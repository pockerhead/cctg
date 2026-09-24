//! QA round 2, TASK-016: independent end-to-end tests.
//! Real `cctg agent` process (binary from env CCTG_BIN) over real TCP into the
//! real `serve_agents`, real `Slots` actor and `Scheduler`, fake Telegram.
//! Transcripts live under a temp CLAUDE_CONFIG_DIR/projects/<folder>/<id>.jsonl.
//! Run with `--test-threads=1` (one global log buffer).

use std::collections::HashSet;
use std::io::{BufRead, BufReader, Write};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::atomic::{AtomicI64, Ordering};
use std::sync::{Arc, Mutex, OnceLock};
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

const SECRET: &str = "qa2-secret-0123456789abcdefXYZ";
const HOST: &str = "qa2box";

// ------------------------------------------------------------------ logs

static LOGS: OnceLock<Arc<Mutex<Vec<u8>>>> = OnceLock::new();

#[derive(Clone)]
struct LogWriter(Arc<Mutex<Vec<u8>>>);
impl Write for LogWriter {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        self.0.lock().unwrap().extend_from_slice(buf);
        Ok(buf.len())
    }
    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

fn logs() -> Arc<Mutex<Vec<u8>>> {
    LOGS.get_or_init(|| {
        let buf = Arc::new(Mutex::new(Vec::new()));
        let w = LogWriter(buf.clone());
        tracing_subscriber::fmt()
            .with_max_level(tracing::Level::DEBUG)
            .with_ansi(false)
            .without_time()
            .with_writer(move || w.clone())
            .init();
        buf
    })
    .clone()
}

fn log_text() -> String {
    String::from_utf8_lossy(&logs().lock().unwrap()).into_owned()
}

// ------------------------------------------------------------------ fake Telegram

#[derive(Debug, Clone)]
struct Rec {
    op: Op,
    accepted: bool,
}

#[derive(Default)]
struct Fake {
    ops: Mutex<Vec<Rec>>,
    next_id: AtomicI64,
    next_thread: AtomicI64,
    /// A stream message containing one of these is refused (502) once.
    refuse_once: Mutex<Vec<String>>,
    /// Reactions on these message ids fail with a 400.
    react_fail: Mutex<HashSet<i64>>,
}

impl Transport for Fake {
    async fn execute(&self, op: &Op) -> Delivery {
        let id = 5000 + self.next_id.fetch_add(1, Ordering::SeqCst);
        let result: Delivery = match op {
            Op::CreateTopic { name, .. } => Ok(Outcome::Topic(ForumTopic {
                message_thread_id: 100 + self.next_thread.fetch_add(1, Ordering::SeqCst),
                name: name.clone(),
                icon_custom_emoji_id: None,
            })),
            Op::Stream { text, .. } => {
                let mut refuse = self.refuse_once.lock().unwrap();
                if let Some(pos) = refuse.iter().position(|r| text.contains(r.as_str())) {
                    refuse.remove(pos);
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
            Op::React { message_id, .. } if self.react_fail.lock().unwrap().contains(message_id) => {
                Err(ApiError::Telegram {
                    code: 400,
                    description: "Bad Request: REACTION_INVALID".into(),
                })
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
    fn recs(&self) -> Vec<Rec> {
        self.ops.lock().unwrap().clone()
    }
    fn thread_of(&self, folder: &str) -> Option<i64> {
        // CreateTopic and its answer are recorded in order; thread ids are
        // handed out in call order.
        let mut n = 0;
        for rec in self.recs() {
            if let Op::CreateTopic { name, .. } = &rec.op {
                if name.contains(folder) {
                    return Some(100 + n);
                }
                n += 1;
            }
        }
        None
    }
    fn creates(&self) -> usize {
        self.recs()
            .iter()
            .filter(|r| matches!(r.op, Op::CreateTopic { .. }))
            .count()
    }
    /// Accepted stream lines (merged messages split) and plain sends
    /// (answers, separators) of one topic, in Telegram order.
    fn lines(&self, thread: i64) -> Vec<String> {
        let mut out = Vec::new();
        for rec in self.recs().into_iter().filter(|r| r.accepted) {
            match rec.op {
                Op::Stream {
                    thread_id, text, ..
                } if thread_id == thread => out.extend(text.lines().map(str::to_owned)),
                Op::Send {
                    thread_id, text, ..
                } if thread_id == Some(thread) => out.push(text),
                _ => {}
            }
        }
        out
    }
    fn stream_messages(&self, thread: i64) -> Vec<String> {
        self.recs()
            .into_iter()
            .filter(|r| r.accepted)
            .filter_map(|r| match r.op {
                Op::Stream {
                    thread_id, text, ..
                } if thread_id == thread => Some(text),
                _ => None,
            })
            .collect()
    }
    fn refused(&self) -> usize {
        self.recs().iter().filter(|r| !r.accepted).count()
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

// ------------------------------------------------------------------ hub

struct Hub {
    fake: Arc<Fake>,
    hooks: mpsc::Sender<HookPost>,
    control: mpsc::UnboundedSender<Control>,
    tasks: Vec<JoinHandle<()>>,
}

impl Hub {
    fn stop(self) {
        for t in self.tasks {
            t.abort();
        }
    }
}

fn fast() -> BucketConfig {
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
        stream_retry: Duration::from_millis(250),
        ..Options::default()
    }
}

async fn start_hub(state: &Path, l: TcpListener, bucket: BucketConfig, fake: Arc<Fake>) -> Hub {
    let _ = logs();
    let (scheduler, outbox) = Scheduler::new(fake.clone(), bucket);
    let sched = tokio::spawn(scheduler.run());
    let store = RegistryStore::open(state).expect("store");
    let registry = store.load().expect("registry");
    let (slots, _view) = Slots::new(registry, store, outbox, options());
    let (agents, agents_rx) = mpsc::channel(64);
    let (hooks, hooks_rx) = mpsc::channel(64);
    let (control, control_rx) = mpsc::unbounded_channel();
    let actor = tokio::spawn(slots.run(agents_rx, hooks_rx, control_rx));
    let ingress = tokio::spawn(serve_agents(l, Secret::parse(SECRET).unwrap(), agents));
    Hub {
        fake,
        hooks,
        control,
        tasks: vec![ingress, actor, sched],
    }
}

async fn listener() -> (TcpListener, u16) {
    let l = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let p = l.local_addr().unwrap().port();
    (l, p)
}

async fn rebind(port: u16) -> TcpListener {
    loop {
        match TcpListener::bind(("127.0.0.1", port)).await {
            Ok(l) => break l,
            Err(_) => tokio::time::sleep(Duration::from_millis(100)).await,
        }
    }
}

// ------------------------------------------------------------------ sessions

struct Root(PathBuf);
impl Drop for Root {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

struct Sess {
    id: String,
    transcript: PathBuf,
    workdir: PathBuf,
    config: PathBuf,
    home: PathBuf,
    pid: u32,
}

impl Sess {
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
    fn append(&self, text: &str) {
        self.append_bytes(text.as_bytes());
    }
    fn append_bytes(&self, bytes: &[u8]) {
        let mut f = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(&self.transcript)
            .unwrap();
        f.write_all(bytes).unwrap();
        f.flush().unwrap();
    }
    async fn start(&self, hub: &Hub, source: &str) {
        hub.hooks
            .send(self.post(HookEvent::SessionStart {
                source: Some(source.into()),
                claude_pid: Some(self.pid),
                parent_claude_pid: None,
            }))
            .await
            .unwrap();
    }
    async fn end(&self, hub: &Hub) {
        hub.hooks
            .send(self.post(HookEvent::SessionEnd {
                reason: Some("other".into()),
                claude_pid: Some(self.pid),
            }))
            .await
            .unwrap();
    }
    async fn stop(&self, hub: &Hub, answer: &str) {
        hub.hooks
            .send(self.post(HookEvent::Stop {
                prompt_id: None,
                last_assistant_message: Some(answer.into()),
            }))
            .await
            .unwrap();
    }
}

/// A test root: `<tmp>/qa2-<name>-<pid>/{cfg/projects/<project>, <folder>, home, state}`.
struct World {
    _root: Root,
    root: PathBuf,
    state: PathBuf,
}

fn world(name: &str) -> World {
    let root = std::env::temp_dir().join(format!("qa2-016-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    let state = root.join("state");
    std::fs::create_dir_all(&state).unwrap();
    World {
        _root: Root(root.clone()),
        root,
        state,
    }
}

impl World {
    fn session(&self, folder: &str, n: u32) -> Sess {
        let config = self.root.join("cfg");
        let project = config.join("projects").join(format!("C--qa2-{folder}"));
        std::fs::create_dir_all(&project).unwrap();
        let workdir = self.root.join(folder);
        std::fs::create_dir_all(&workdir).unwrap();
        let home = self.root.join("home");
        std::fs::create_dir_all(&home).unwrap();
        let id = format!("{n:08x}-0000-4000-8000-00000000a016");
        Sess {
            transcript: project.join(format!("{id}.jsonl")),
            id,
            workdir,
            config,
            home,
            pid: 7000 + n,
        }
    }
}

/// The real agent process; its stdout (JSON-RPC to "Claude") is captured.
struct Agent {
    child: Child,
    stdout: Arc<Mutex<Vec<String>>>,
}
impl Drop for Agent {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

fn agent(s: &Sess, port: u16) -> Agent {
    let bin = std::env::var("CCTG_BIN").expect("CCTG_BIN");
    let mut child = Command::new(bin)
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
    let mut stdin = child.stdin.take().unwrap();
    stdin
        .write_all(b"{\"jsonrpc\":\"2.0\",\"id\":0,\"method\":\"initialize\",\"params\":{\"protocolVersion\":\"2025-11-25\"}}\n{\"jsonrpc\":\"2.0\",\"method\":\"notifications/initialized\"}\n")
        .unwrap();
    std::mem::forget(stdin);
    let out = Arc::new(Mutex::new(Vec::new()));
    let sink = out.clone();
    let stdout = child.stdout.take().unwrap();
    std::thread::spawn(move || {
        for line in BufReader::new(stdout).lines().map_while(Result::ok) {
            sink.lock().unwrap().push(line);
        }
    });
    Agent { child, stdout: out }
}

impl Agent {
    fn channel_contents(&self) -> Vec<String> {
        self.stdout
            .lock()
            .unwrap()
            .iter()
            .filter_map(|l| serde_json::from_str::<serde_json::Value>(l).ok())
            .filter(|v| v["method"] == "notifications/claude/channel")
            .filter_map(|v| v["params"]["content"].as_str().map(str::to_owned))
            .collect()
    }
}

// ------------------------------------------------------------------ records

fn line(v: serde_json::Value) -> String {
    format!("{v}\n")
}
fn prompt(t: &str) -> String {
    line(serde_json::json!({"type":"user","isMeta":false,"message":{"role":"user","content":t}}))
}
fn call(id: &str, d: &str) -> String {
    line(serde_json::json!({"type":"assistant","message":{"role":"assistant","stop_reason":"tool_use","content":[
        {"type":"tool_use","id":id,"name":"Bash","input":{"command":"true","description":d}}]}}))
}
fn note(t: &str) -> String {
    line(serde_json::json!({"type":"assistant","message":{"role":"assistant","stop_reason":"tool_use","content":[
        {"type":"text","text":t}]}}))
}
fn result(id: &str, err: Option<&str>) -> String {
    line(serde_json::json!({"type":"user","message":{"role":"user","content":[
        {"type":"tool_result","tool_use_id":id,"content":err.unwrap_or("fine"),"is_error":err.is_some()}]}}))
}
fn answer(t: &str) -> String {
    line(serde_json::json!({"type":"assistant","message":{"role":"assistant","stop_reason":"end_turn","content":[
        {"type":"text","text":t}]}}))
}
fn channel_user(mid: i64, thread: i64) -> String {
    let tag = format!("<channel source=\"cctg\" chat_id=\"-100\" message_id=\"{mid}\" thread_id=\"{thread}\">\nbody\n</channel>");
    line(serde_json::json!({"type":"user","isMeta":true,"origin":{"kind":"channel"},"message":{"role":"user","content":tag}}))
}
/// The mid-turn queued shape (as peer messages are written): attachment
/// queued_command whose prompt starts with the tag.
fn channel_queued(mid: i64, thread: i64) -> String {
    let tag = format!("<channel source=\"cctg\" chat_id=\"-100\" message_id=\"{mid}\" thread_id=\"{thread}\">\nbody\n</channel>");
    line(serde_json::json!({"type":"attachment","attachment":{"type":"queued_command","prompt":tag,"commandMode":"prompt","origin":{"kind":"channel"}}}))
}
fn ok(d: &str) -> String {
    format!("• Bash: {d} ✓")
}

async fn wait_for(what: &str, secs: u64, mut cond: impl FnMut() -> bool) {
    let deadline = Instant::now() + Duration::from_secs(secs);
    while !cond() {
        assert!(Instant::now() < deadline, "timed out waiting for: {what}");
        tokio::time::sleep(Duration::from_millis(25)).await;
    }
}

fn committed(state: &Path, session: &str) -> serde_json::Value {
    std::fs::read_to_string(state.join("registry.json"))
        .ok()
        .and_then(|t| serde_json::from_str::<serde_json::Value>(&t).ok())
        .map(|v| v["sessions"][session]["stream"].clone())
        .unwrap_or(serde_json::Value::Null)
}

// ------------------------------------------------------------------ tests

/// AC1/AC2/AC8 + answer: two sessions in two folders stream into their own
/// topics; lines appended in pieces (one cut inside a multibyte character),
/// parallel calls whose results come in reverse order, a Stop whose turn end
/// was read first and a Stop that comes before its turn end. No duplicates.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn qa2_two_slots_pieces_order_and_answers() {
    let w = world("slots");
    let a = w.session("alpha", 1);
    let b = w.session("beta", 2);
    let (l, port) = listener().await;
    let fake = Arc::new(Fake::default());
    let hub = start_hub(&w.state, l, fast(), fake.clone()).await;
    a.start(&hub, "startup").await;
    b.start(&hub, "startup").await;
    let _aa = agent(&a, port);
    let _ab = agent(&b, port);
    wait_for("two topics", 20, || fake.creates() == 2).await;
    let ta = fake.thread_of("alpha").unwrap();
    let tb = fake.thread_of("beta").unwrap();
    assert_ne!(ta, tb);

    a.append(&prompt("alpha go"));
    b.append(&prompt("beta go"));
    a.append(&note("Запускаю два шага."));
    a.append(&call("a1", "шаг один ☃"));
    let c2 = call("a2", "шаг два ☃");
    // Cut inside the 3-byte snowman.
    let cut = c2.find('☃').unwrap() + 1;
    a.append_bytes(&c2.as_bytes()[..cut]);
    b.append(&call("b1", "beta one"));
    b.append(&result("b1", None));
    wait_for("beta line", 20, || fake.lines(tb).len() >= 2).await;
    tokio::time::sleep(Duration::from_millis(600)).await;
    assert_eq!(fake.lines(ta), ["> alpha go", "Запускаю два шага."]);
    a.append_bytes(&c2.as_bytes()[cut..]);
    a.append(&result("a2", Some("exit 1\nmore")));
    tokio::time::sleep(Duration::from_millis(600)).await;
    assert_eq!(
        fake.lines(ta),
        ["> alpha go", "Запускаю два шага."],
        "a2 is done but a1 was called first and still runs"
    );
    a.append(&result("a1", None));
    a.append(&answer("ALPHA ANSWER 1"));
    wait_for("a1 a2", 20, || fake.lines(ta).len() >= 4).await;
    // The turn end is read before Stop: the answer goes at once, after lines.
    tokio::time::sleep(Duration::from_millis(300)).await;
    let t0 = Instant::now();
    a.stop(&hub, "ALPHA ANSWER 1").await;
    wait_for("answer 1", 20, || fake.lines(ta).len() >= 5).await;
    assert!(t0.elapsed() < Duration::from_secs(3), "not held for its read turn end");

    // Turn 2: Stop first, then the lines and the turn end.
    a.append(&prompt("alpha again"));
    a.append(&call("a3", "three"));
    wait_for("prompt 2", 20, || fake.lines(ta).len() >= 6).await;
    a.stop(&hub, "ALPHA ANSWER 2").await;
    tokio::time::sleep(Duration::from_millis(500)).await;
    a.append(&result("a3", None));
    a.append(&answer("ALPHA ANSWER 2"));
    wait_for("answer 2", 20, || fake.lines(ta).len() >= 8).await;
    b.stop(&hub, "BETA ANSWER").await;
    b.append(&answer("BETA ANSWER"));
    wait_for("beta answer", 20, || fake.lines(tb).len() >= 3).await;
    tokio::time::sleep(Duration::from_millis(1200)).await;

    let la = fake.lines(ta);
    assert_eq!(
        la,
        [
            "> alpha go".to_owned(),
            "Запускаю два шага.".to_owned(),
            ok("шаг один ☃"),
            "• Bash: шаг два ☃ ✗ exit 1".to_owned(),
            "ALPHA ANSWER 1".to_owned(),
            "> alpha again".to_owned(),
            ok("three"),
            "ALPHA ANSWER 2".to_owned(),
        ]
    );
    assert_eq!(
        fake.lines(tb),
        ["> beta go".to_owned(), ok("beta one"), "BETA ANSWER".to_owned()]
    );
    hub.stop();
}

/// AC3/AC4/AC8 with refusals: a small bucket forces merging, two stream
/// messages are refused once (502). Nothing is lost, no line shows before an
/// earlier one was accepted, and the answer comes after every line.
async fn refusal_run(tag: &str, n: u32) -> (Arc<Fake>, i64, Vec<String>) {
    let w = world(tag);
    let s = w.session("gamma", n);
    let (l, port) = listener().await;
    let fake = Arc::new(Fake::default());
    let bucket = BucketConfig {
        capacity: 2,
        refill_every: Duration::from_millis(300),
        min_gap: Duration::ZERO,
    };
    let hub = start_hub(&w.state, l, bucket, fake.clone()).await;
    s.start(&hub, "startup").await;
    let _ag = agent(&s, port);
    wait_for("topic", 20, || fake.creates() == 1).await;
    let t = fake.thread_of("gamma").unwrap();
    // The agent is bound and the stream reads before the turn starts (as in a
    // real session, whose agent connects long before its first Stop).
    s.append(&prompt("gamma warm"));
    wait_for("stream up", 20, || fake.lines(t).len() == 1).await;
    *fake.refuse_once.lock().unwrap() = vec!["c05".into(), "c11".into()];
    let mut want = vec!["> gamma warm".to_owned(), "> gamma run".to_owned()];
    let mut batch = prompt("gamma run");
    for n in 0..16 {
        batch.push_str(&call(&format!("g{n}"), &format!("c{n:02}")));
        batch.push_str(&result(&format!("g{n}"), None));
        want.push(ok(&format!("c{n:02}")));
    }
    batch.push_str(&answer("GAMMA DONE"));
    // Written in three pieces, the middle one ending mid-line.
    let third = batch.len() / 3;
    let (p1, rest) = batch.split_at(third);
    let (p2, p3) = rest.split_at(third);
    s.append(p1);
    s.stop(&hub, "GAMMA DONE").await;
    tokio::time::sleep(Duration::from_millis(200)).await;
    s.append(p2);
    tokio::time::sleep(Duration::from_millis(200)).await;
    s.append(p3);
    want.push("GAMMA DONE".into());
    wait_for("everything once", 60, || {
        let got = fake.lines(t);
        want.iter().all(|w| got.contains(w))
    })
    .await;
    tokio::time::sleep(Duration::from_millis(1500)).await;
    let got = fake.lines(t);
    for rec in fake.recs() {
        match &rec.op {
            Op::Stream { text, restart, .. } => eprintln!(
                "QA2 op Stream accepted={} restart={restart} lines={:?}",
                rec.accepted,
                text.lines().collect::<Vec<_>>()
            ),
            Op::Send { text, .. } => eprintln!("QA2 op Send accepted={} {text:?}", rec.accepted),
            _ => {}
        }
    }
    assert_eq!(fake.refused(), 2, "both refusals happened");
    hub.stop();
    (fake, t, want)
}

fn firsts(got: &[String]) -> Vec<String> {
    let mut seen = HashSet::new();
    got.iter().filter(|l| seen.insert(l.as_str())).cloned().collect()
}

/// Stream lines only: nothing lost, first appearances in call order, merged.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn qa2_refusals_under_the_limit_keep_line_order_and_lose_nothing() {
    let (fake, t, want) = refusal_run("refuse-lines", 3).await;
    let got = fake.lines(t);
    let lines: Vec<String> = got.iter().filter(|l| *l != "GAMMA DONE").cloned().collect();
    let want_lines: Vec<String> = want.iter().filter(|l| *l != "GAMMA DONE").cloned().collect();
    assert_eq!(firsts(&lines), want_lines, "got {got:?}");
    assert_eq!(got.iter().filter(|l| *l == "GAMMA DONE").count(), 1, "one answer");
    eprintln!(
        "QA2 refusals: {} accepted lines, {} unique, {} repeats (at-least-once), {} stream messages",
        got.len(),
        firsts(&got).len(),
        got.len() - firsts(&got).len(),
        fake.stream_messages(t).len()
    );
    assert!(
        fake.stream_messages(t).len() < want.len(),
        "lines were merged under the limit"
    );
}

/// The turn answer after a refused line: it should still come after the
/// tool lines of its turn (first appearances). FAILS at 95a0f23 (QA2 bug).
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn qa2_answer_after_a_refused_line_still_follows_its_lines() {
    let (fake, t, want) = refusal_run("refuse-answer", 10).await;
    let got = fake.lines(t);
    assert_eq!(firsts(&got), want, "got {got:?}");
}

/// A single refused line with the rest queued right behind it (no merging):
/// no later line may show before the refused one is sent again.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn qa2_refused_single_line_is_not_overtaken() {
    let w = world("refuse-one");
    let s = w.session("iota", 11);
    let (l, port) = listener().await;
    let fake = Arc::new(Fake::default());
    let hub = start_hub(&w.state, l, fast(), fake.clone()).await;
    s.start(&hub, "startup").await;
    let _ag = agent(&s, port);
    wait_for("topic", 20, || fake.creates() == 1).await;
    let t = fake.thread_of("iota").unwrap();
    s.append(&prompt("iota warm"));
    wait_for("stream up", 20, || fake.lines(t).len() == 1).await;
    *fake.refuse_once.lock().unwrap() = vec!["i05".into()];
    let mut want = vec!["> iota warm".to_owned()];
    let mut batch = String::new();
    for n in 0..12 {
        batch.push_str(&call(&format!("i{n}"), &format!("i{n:02}")));
        batch.push_str(&result(&format!("i{n}"), None));
        want.push(ok(&format!("i{n:02}")));
    }
    s.append(&batch);
    wait_for("all", 30, || want.iter().all(|w| fake.lines(t).contains(w))).await;
    tokio::time::sleep(Duration::from_millis(1000)).await;
    let got = fake.lines(t);
    assert_eq!(fake.refused(), 1);
    assert_eq!(firsts(&got), want, "got {got:?}");
    hub.stop();
}

/// AC3: a hub restart from the saved registry while a call is open (its
/// result is written while the hub is down) neither repeats nor loses.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn qa2_restart_with_open_calls() {
    let w = world("restart");
    let s = w.session("delta", 4);
    let (l, port) = listener().await;
    let fake1 = Arc::new(Fake::default());
    let hub1 = start_hub(&w.state, l, fast(), fake1.clone()).await;
    s.start(&hub1, "startup").await;
    let _ag = agent(&s, port);
    wait_for("topic", 20, || fake1.creates() == 1).await;
    let t = fake1.thread_of("delta").unwrap();
    s.append(&prompt("delta p"));
    s.append(&call("x", "slow x"));
    s.append(&call("y", "fast y"));
    s.append(&result("y", None));
    let len = std::fs::metadata(&s.transcript).unwrap().len();
    wait_for("offset over open calls", 20, || {
        committed(&w.state, &s.id)["offset"].as_u64() == Some(len)
    })
    .await;
    let calls = committed(&w.state, &s.id)["calls"].clone();
    assert_eq!(calls.as_array().map(Vec::len), Some(2), "open calls persisted: {calls}");
    assert_eq!(fake1.lines(t), ["> delta p"]);
    hub1.stop();
    tokio::time::sleep(Duration::from_millis(300)).await;
    s.append(&result("x", None));
    s.append(&prompt("delta after"));
    let fake2 = Arc::new(Fake::default());
    let hub2 = start_hub(&w.state, rebind(port).await, fast(), fake2.clone()).await;
    wait_for("hub 2 lines", 60, || fake2.lines(t).len() >= 3).await;
    tokio::time::sleep(Duration::from_millis(1500)).await;
    assert_eq!(fake1.lines(t), ["> delta p"]);
    assert_eq!(
        fake2.lines(t),
        [ok("slow x"), ok("fast y"), "> delta after".to_owned()]
    );
    assert_eq!(fake2.creates(), 0, "the slot keeps its topic");
    hub2.stop();
}

/// AC5: a new session in the slot streams from its own start after exactly
/// one separator; lines the old session writes after its end are not streamed.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn qa2_rotation_one_separator_new_offset() {
    let w = world("rotate");
    let a = w.session("eps", 5);
    let b = w.session("eps", 6);
    let (l, port) = listener().await;
    let fake = Arc::new(Fake::default());
    let hub = start_hub(&w.state, l, fast(), fake.clone()).await;
    a.start(&hub, "startup").await;
    let ag_a = agent(&a, port);
    wait_for("topic", 20, || fake.creates() == 1).await;
    let t = fake.thread_of("eps").unwrap();
    a.append(&prompt("from A"));
    a.append(&call("a1", "a one"));
    a.append(&result("a1", None));
    wait_for("A lines", 20, || fake.lines(t).len() >= 2).await;
    a.end(&hub).await;
    drop(ag_a);
    tokio::time::sleep(Duration::from_millis(300)).await;
    a.append(&prompt("late A"));
    b.append(&prompt("from B"));
    b.append(&call("b1", "b one"));
    b.append(&result("b1", None));
    b.start(&hub, "startup").await;
    let _ag_b = agent(&b, port);
    wait_for("B lines", 30, || fake.lines(t).len() >= 5).await;
    tokio::time::sleep(Duration::from_millis(1200)).await;
    let got = fake.lines(t);
    let seps: Vec<&String> = got.iter().filter(|l| l.starts_with("──")).collect();
    assert_eq!(seps.len(), 1, "one separator: {got:?}");
    assert_eq!(
        got,
        [
            "> from A".to_owned(),
            ok("a one"),
            seps[0].clone(),
            "> from B".to_owned(),
            ok("b one"),
        ]
    );
    assert_eq!(fake.creates(), 1);
    assert_eq!(
        committed(&w.state, &b.id)["offset"].as_u64(),
        Some(std::fs::metadata(&b.transcript).unwrap().len())
    );
    hub.stop();
}

/// AC7: 👀 once handed to the agent (the agent really prints the channel
/// notification), ✍ only on the message's own cctg record (user meta or the
/// queued_command attachment shape), never on UserPromptSubmit or a terminal
/// prompt; a failing reaction does not stop routing.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn qa2_reactions() {
    let w = world("react");
    let s = w.session("zeta", 7);
    let (l, port) = listener().await;
    let fake = Arc::new(Fake::default());
    let hub = start_hub(&w.state, l, fast(), fake.clone()).await;
    s.start(&hub, "startup").await;
    let ag = agent(&s, port);
    wait_for("topic", 20, || fake.creates() == 1).await;
    let t = fake.thread_of("zeta").unwrap();
    s.append(&prompt("warm"));
    wait_for("stream up", 20, || !fake.lines(t).is_empty()).await;
    fake.react_fail.lock().unwrap().insert(903);
    for (mid, text) in [(901, "first msg"), (902, "second msg"), (903, "third msg"), (904, "fourth msg")] {
        hub.control
            .send(Control::Message(Inbound {
                message_id: mid,
                thread_id: Some(t),
                text: Some(text.into()),
                reply_to: None,
            }))
            .unwrap();
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    wait_for("delivered to Claude", 20, || ag.channel_contents().len() == 4).await;
    wait_for("eyes", 20, || fake.reactions().len() == 4).await;
    assert!(fake.reactions().iter().all(|(_, e)| e == "👀"));
    hub.hooks
        .send(s.post(HookEvent::UserPromptSubmit { prompt_id: None }))
        .await
        .unwrap();
    s.append(&prompt("typed locally"));
    s.append(&call("q1", "busy"));
    s.append(&channel_queued(902, t));
    s.append(&result("q1", None));
    s.append(&channel_user(904, t));
    wait_for("busy line", 20, || fake.lines(t).contains(&ok("busy"))).await;
    wait_for("writing x2", 20, || fake.reactions().len() == 6).await;
    tokio::time::sleep(Duration::from_millis(800)).await;
    let r = fake.reactions();
    assert_eq!(
        r[4..].iter().cloned().collect::<HashSet<_>>(),
        [(902, "✍".to_owned()), (904, "✍".to_owned())].into_iter().collect(),
        "all: {r:?}"
    );
    // 901 got no record: it keeps 👀. The failing 903 reaction did not stop
    // the 904 message from being routed (checked above: 4 notifications).
    assert!(!r.iter().any(|(m, e)| *m == 901 && e == "✍"));
    assert!(
        !fake.lines(t).iter().any(|l| l.contains("body") || l.contains("msg")),
        "Telegram text is never echoed"
    );
    hub.stop();
}

/// AC6 through ingress: no file at a resume = one WARN however many polls; a
/// new session without its file yet = no WARN; a file that was read and is
/// then deleted = one WARN; no path or text in the logs.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn qa2_missing_and_deleted_transcript_logs_once() {
    let w = world("missing");
    let r = w.session("eta", 8);
    let n = w.session("theta", 9);
    let (l, port) = listener().await;
    let fake = Arc::new(Fake::default());
    let hub = start_hub(&w.state, l, fast(), fake.clone()).await;
    logs().lock().unwrap().clear();
    r.start(&hub, "resume").await;
    let _ar = agent(&r, port);
    wait_for("topic r", 20, || fake.creates() == 1).await;
    tokio::time::sleep(Duration::from_millis(2000)).await; // ~40 polls
    let warn_missing = |t: &str| {
        t.lines()
            .filter(|l| l.contains("WARN") && l.contains("session transcript not found"))
            .count()
    };
    assert_eq!(warn_missing(&log_text()), 1, "one warn for the resume");

    n.start(&hub, "startup").await;
    let _an = agent(&n, port);
    wait_for("topic n", 20, || fake.creates() == 2).await;
    tokio::time::sleep(Duration::from_millis(1500)).await;
    assert_eq!(warn_missing(&log_text()), 1, "a new session's missing file is quiet");
    let tn = fake.thread_of("theta").unwrap();
    n.append(&prompt("theta secret prompt"));
    wait_for("theta streams", 20, || fake.lines(tn).len() == 1).await;
    std::fs::remove_file(&n.transcript).unwrap();
    tokio::time::sleep(Duration::from_millis(1500)).await;
    let text = log_text();
    assert_eq!(warn_missing(&text), 2, "one more warn after the delete");
    for leak in ["C--qa2", "theta secret prompt", "qa2-016-", &n.id, &r.id] {
        assert!(!text.contains(leak), "log leaks {leak:?}");
    }
    // The hub keeps polling: a new file is read from its start (reset).
    n.append(&prompt("theta back"));
    wait_for("theta back", 20, || fake.lines(tn).len() == 2).await;
    std::fs::write(
        Path::new(&w.root).join("qa2-logs.txt"),
        text.lines()
            .filter(|l| l.contains("WARN") || l.contains("transcript"))
            .collect::<Vec<_>>()
            .join("\n"),
    )
    .ok();
    eprintln!(
        "QA2 log lines about transcripts:\n{}",
        text.lines()
            .filter(|l| l.contains("transcript") || l.contains("WARN"))
            .collect::<Vec<_>>()
            .join("\n")
    );
    hub.stop();
}

/// Real transcripts of this machine (the newest 200 main jsonl files under
/// ~/.claude/projects): every line goes through `transcript::stream_events`
/// without a panic; prints counts only (no text, no paths). Checks that no
/// event carries thinking text and that each assistant message.id yields at
/// most one TurnEnd.
#[test]
fn qa2_real_transcripts_smoke() {
    use transcript::{StreamEvent, stream_events};
    let root = PathBuf::from(std::env::var("USERPROFILE").unwrap()).join(".claude").join("projects");
    let mut files: Vec<(std::time::SystemTime, PathBuf)> = Vec::new();
    for dir in std::fs::read_dir(&root).into_iter().flatten().flatten() {
        for f in std::fs::read_dir(dir.path()).into_iter().flatten().flatten() {
            let p = f.path();
            if p.extension().is_some_and(|e| e == "jsonl") {
                if let Ok(m) = f.metadata().and_then(|m| m.modified()) {
                    files.push((m, p));
                }
            }
        }
    }
    files.sort();
    files.reverse();
    files.truncate(200);
    let (mut lines, mut prompts, mut channels, mut notes, mut calls, mut results, mut errors, mut ends) =
        (0usize, 0, 0, 0, 0, 0, 0, 0);
    let mut multi_end_ids = 0;
    let mut overlaps = 0;
    for (_, p) in &files {
        let Ok(text) = std::fs::read_to_string(p) else { continue };
        let mut ends_per_id: std::collections::HashMap<String, usize> = Default::default();
        let mut thinking: Vec<String> = Vec::new();
        for l in text.lines() {
            lines += 1;
            let v: serde_json::Value = serde_json::from_str(l).unwrap_or_default();
            for b in v["message"]["content"].as_array().into_iter().flatten() {
                if b["type"] == "thinking" {
                    if let Some(t) = b["thinking"].as_str().filter(|t| t.len() > 40) {
                        thinking.push(t.chars().take(40).collect());
                    }
                }
            }
            for e in stream_events(l) {
                let shown = match &e {
                    StreamEvent::Prompt(t) => { prompts += 1; t.clone() }
                    StreamEvent::Channel { .. } => { channels += 1; String::new() }
                    StreamEvent::Note(t) => { notes += 1; t.clone() }
                    StreamEvent::Call { line, .. } => { calls += 1; line.clone() }
                    StreamEvent::Result { error, .. } => {
                        results += 1;
                        if error.is_some() { errors += 1; }
                        error.clone().unwrap_or_default()
                    }
                    StreamEvent::TurnEnd => {
                        ends += 1;
                        let id = v["message"]["id"].as_str().unwrap_or("").to_owned();
                        *ends_per_id.entry(id).or_default() += 1;
                        String::new()
                    }
                };
                if !shown.is_empty() && thinking.iter().any(|t| shown.contains(t.as_str())) {
                    let kind = match &e {
                        StreamEvent::Prompt(_) => "prompt",
                        StreamEvent::Note(_) => "note",
                        StreamEvent::Call { .. } => "call",
                        StreamEvent::Result { .. } => "result",
                        _ => "other",
                    };
                    let has_thinking_block = v["message"]["content"]
                        .as_array()
                        .is_some_and(|a| a.iter().any(|b| b["type"] == "thinking"));
                    eprintln!(
                        "QA2 thinking-overlap: event={kind} record_type={} same_record_has_thinking={has_thinking_block} isMeta={} isSidechain={}",
                        v["type"], v["isMeta"], v["isSidechain"]
                    );
                    overlaps += 1;
                }
            }
        }
        multi_end_ids += ends_per_id.iter().filter(|(id, n)| !id.is_empty() && **n > 1).count();
    }
    eprintln!(
        "QA2 real: files={} lines={lines} prompts={prompts} channel={channels} notes={notes} calls={calls} results={results} errors={errors} turn_ends={ends} ids_with_2plus_turn_ends={multi_end_ids}",
        files.len()
    );
    eprintln!("QA2 real: thinking overlaps={overlaps}");
    assert_eq!(multi_end_ids, 0);
}
