//! QA TASK-021: scripted routing sequence against the real `Slots` actor
//! (`Slots::run`), the real `Scheduler` (fast bucket) and fake agent links.
//! Updates are real Bot API JSON through `updates::route_batch`, then split
//! exactly like `hub::route_inbound` (commands vs `Control::Message`).
//! Runs on a paused current-thread runtime so the 60 s notice window is exact.

use std::io;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use cctg::hub::api::{ForumTopic, Message};
use cctg::hub::commands::is_command;
use cctg::hub::config::Allowlist;
use cctg::hub::ingress::AgentEvent;
use cctg::hub::registry::RegistryStore;
use cctg::hub::scheduler::{BucketConfig, Delivery, Op, Outcome, Scheduler, Transport};
use cctg::hub::slots::{Control, OFFLINE_NOTICE, Options, Slots, TEXT_ONLY_NOTICE};
use cctg::hub::updates::{Routed, route_batch};
use cctg::wire::{AgentMsg, HookEvent, HookPost, HubMsg, Register};
use serde_json::{Value, json};
use tokio::sync::mpsc;
use transcript::{SplitOptions, split_for_telegram};

const CHAT: i64 = -1000000000001;
const USER: i64 = 7_294_163_805;
const STRANGER: i64 = 6_111_222_333;
const A: &str = "aaaaaaaa-0000-4000-8000-000000000001";
const B: &str = "bbbbbbbb-0000-4000-8000-000000000002";
const C: &str = "cccccccc-0000-4000-8000-000000000003";
const CWD: &str = r"C:\w\proj";

static FAILS: AtomicUsize = AtomicUsize::new(0);

macro_rules! check {
    ($cond:expr, $($arg:tt)*) => {{
        let ok = $cond;
        if !ok { FAILS.fetch_add(1, Ordering::SeqCst); }
        println!("[{}] {}", if ok { "PASS" } else { "FAIL" }, format!($($arg)*));
    }};
}

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

#[derive(Default)]
struct Fake {
    ops: Mutex<Vec<Op>>,
    creates: AtomicUsize,
    stall_sends: AtomicBool,
}
impl Transport for Fake {
    async fn execute(&self, op: &Op) -> Delivery {
        self.ops.lock().unwrap().push(op.clone());
        match op {
            Op::CreateTopic { name, .. } => {
                let n = self.creates.fetch_add(1, Ordering::SeqCst) as i64;
                Ok(Outcome::Topic(ForumTopic {
                    message_thread_id: 100 + n,
                    name: name.clone(),
                    icon_custom_emoji_id: None,
                }))
            }
            Op::Send { .. } | Op::SendDocument { .. } => {
                if self.stall_sends.load(Ordering::SeqCst) {
                    std::future::pending::<()>().await;
                }
                Ok(Outcome::Sent(Message::default()))
            }
            _ => Ok(Outcome::Done),
        }
    }
}
impl Fake {
    fn ops(&self) -> Vec<Op> {
        self.ops.lock().unwrap().clone()
    }
    fn sends_to(&self, thread: i64) -> Vec<String> {
        self.ops()
            .into_iter()
            .filter_map(|op| match op {
                Op::Send { thread_id: Some(t), text, .. } if t == thread => Some(text),
                _ => None,
            })
            .collect()
    }
    fn docs_to(&self, thread: i64) -> Vec<Vec<u8>> {
        self.ops()
            .into_iter()
            .filter_map(|op| match op {
                Op::SendDocument { thread_id: Some(t), document } if t == thread => {
                    Some(document.bytes)
                }
                _ => None,
            })
            .collect()
    }
}

fn post(session: &str, event: HookEvent) -> HookPost {
    HookPost::new("box".into(), session.into(), CWD.into(), String::new(), event)
}
fn start(session: &str, pid: u32, source: &str) -> HookPost {
    post(
        session,
        HookEvent::SessionStart {
            source: Some(source.into()),
            claude_pid: Some(pid),
            parent_claude_pid: None,
        },
    )
}
fn end(session: &str, pid: u32, reason: Option<&str>) -> HookPost {
    post(
        session,
        HookEvent::SessionEnd {
            reason: reason.map(Into::into),
            claude_pid: Some(pid),
        },
    )
}
fn register(conn: u64, session: &str, pid: u32) -> (AgentEvent, mpsc::Receiver<HubMsg>) {
    let (to_agent, rx) = mpsc::channel(64);
    (
        AgentEvent::Registered {
            conn,
            register: Register {
                session_id: session.into(),
                host: "box".into(),
                cwd: CWD.into(),
                claude_pid: Some(pid),
            },
            to_agent,
        },
        rx,
    )
}
fn reply(conn: u64, text: &str) -> AgentEvent {
    AgentEvent::Message {
        conn,
        msg: AgentMsg::Reply { text: text.into() },
    }
}

fn msg(update_id: i64, from: i64, thread: Option<i64>, extra: Value) -> Value {
    let mut m = json!({
        "message_id": update_id + 1000, "date": 1,
        "from": { "id": from, "is_bot": false, "first_name": "x" },
        "chat": { "id": CHAT, "type": "supergroup", "is_forum": true },
    });
    if let Some(t) = thread {
        m["message_thread_id"] = json!(t);
        m["is_topic_message"] = json!(true);
    }
    for (k, v) in extra.as_object().unwrap() {
        m[k] = v.clone();
    }
    json!({ "update_id": update_id, "message": m })
}

/// Mirrors `hub::route_inbound` (private): commands to the worker, other
/// input to the actor, services other than TopicEdited dropped.
fn route(
    updates: Vec<Value>,
    control: &mpsc::UnboundedSender<Control>,
    commands: &mut Vec<String>,
) {
    let allow: Allowlist = [USER].into_iter().collect();
    let (_, routed) = route_batch(updates, None, CHAT, &allow);
    for r in routed {
        match r {
            Routed::Input(input) if is_command(&input) => {
                commands.push(input.text.unwrap_or_default())
            }
            Routed::Input(input) => control.send(Control::Message(input)).unwrap(),
            _ => {}
        }
    }
}

async fn settle() {
    for _ in 0..20 {
        tokio::task::yield_now().await;
    }
    tokio::time::sleep(Duration::from_millis(20)).await;
}
async fn until(what: &str, mut f: impl FnMut() -> bool) {
    for _ in 0..4000 {
        if f() {
            return;
        }
        tokio::time::sleep(Duration::from_millis(5)).await;
    }
    check!(false, "timed out waiting for {what}");
}
fn drain(rx: &mut mpsc::Receiver<HubMsg>) -> Vec<(String, Vec<(String, String)>)> {
    let mut out = Vec::new();
    while let Ok(HubMsg::Inbound { content, meta }) = rx.try_recv() {
        out.push((content, meta.into_iter().collect()));
    }
    out
}
fn kv(pairs: &[(&str, &str)]) -> Vec<(String, String)> {
    let mut v: Vec<_> = pairs.iter().map(|(a, b)| (a.to_string(), b.to_string())).collect();
    v.sort();
    v
}

struct Rig {
    fake: Arc<Fake>,
    agents: mpsc::Sender<AgentEvent>,
    hooks: mpsc::Sender<HookPost>,
    control: mpsc::UnboundedSender<Control>,
}

fn rig(tag: &str) -> Rig {
    let state = std::env::temp_dir().join(format!("qa021-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&state);
    std::fs::create_dir_all(&state).unwrap();
    let fake = Arc::new(Fake::default());
    let (scheduler, outbox) = Scheduler::new(
        fake.clone(),
        BucketConfig { capacity: 10_000, refill_every: Duration::from_millis(1), min_gap: Duration::ZERO },
    );
    tokio::spawn(scheduler.run());
    let store = RegistryStore::open(&state).unwrap();
    let options = Options { grace: Duration::ZERO, chat_id: CHAT, ..Options::default() };
    assert_eq!(options.notice_every, Duration::from_secs(60), "default notice window");
    let (slots, _view) = Slots::new(store.load().unwrap(), store, outbox, options);
    let (agents, agents_rx) = mpsc::channel(4096);
    let (hooks, hooks_rx) = mpsc::channel(64);
    let (control, control_rx) = mpsc::unbounded_channel();
    tokio::spawn(slots.run(agents_rx, hooks_rx, control_rx));
    Rig { fake, agents, hooks, control }
}

async fn main_scenario() {
    let r = rig("main");
    let mut commands = Vec::new();
    r.hooks.send(start(A, 10, "startup")).await.unwrap();
    until("topic 100", || r.fake.creates.load(Ordering::SeqCst) == 1).await;
    r.hooks.send(start(B, 11, "startup")).await.unwrap();
    until("topic 101", || r.fake.creates.load(Ordering::SeqCst) == 2).await;
    let (ev, mut rx1) = register(1, A, 10);
    r.agents.send(ev).await.unwrap();
    let (ev, mut rx2) = register(2, B, 11);
    r.agents.send(ev).await.unwrap();
    settle().await;
    tokio::time::sleep(Duration::from_millis(300)).await;
    let sends_before = r.fake.sends_to(100).len() + r.fake.sends_to(101).len();

    // 1. Inbound routing.
    route(
        vec![
            msg(1, USER, Some(100), json!({"text": "m1 QAMARK_ONE", "reply_to_message": {"message_id": 100}})),
            msg(2, USER, Some(101), json!({"text": "m2 QAMARK_TWO", "reply_to_message": {"message_id": 55, "text": "old"}})),
            msg(3, USER, None, json!({"text": "general QAMARK_GEN"})),
            msg(4, USER, Some(999), json!({"text": "foreign QAMARK_FOREIGN"})),
            msg(5, STRANGER, Some(100), json!({"text": "stranger QAMARK_STRANGER"})),
            msg(6, USER, Some(100), json!({"forum_topic_created": {"name": "x", "icon_color": 1}})),
            msg(7, USER, Some(100), json!({"forum_topic_edited": {"name": "y"}})),
            msg(8, USER, Some(100), json!({"text": "/brief 2"})),
            msg(9, USER, Some(100), json!({"text": "/QAMARK_SLASH do it"})),
            msg(10, USER, Some(100), json!({"text": "  /tmp/QAMARK_PATH.log fails"})),
        ],
        &r.control,
        &mut commands,
    );
    settle().await;
    let got1 = drain(&mut rx1);
    let got2 = drain(&mut rx2);
    check!(
        got1 == vec![("m1 QAMARK_ONE".to_string(), kv(&[("chat_id", "-1000000000001"), ("message_id", "1001"), ("thread_id", "100")]))],
        "slot A agent got exactly its message, topic-root reply_to filtered: {got1:?}"
    );
    check!(
        got2 == vec![("m2 QAMARK_TWO".to_string(), kv(&[("chat_id", "-1000000000001"), ("message_id", "1002"), ("reply_to_message_id", "55"), ("thread_id", "101")]))],
        "slot B agent got its message with explicit reply id: {got2:?}"
    );
    let all_keys_ok = got1.iter().chain(&got2).all(|(_, m)| {
        m.iter().all(|(k, _)| !k.is_empty() && k.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'_'))
    });
    check!(all_keys_ok, "meta keys only [A-Za-z0-9_]");
    check!(
        commands == vec!["/brief 2".to_string(), "/QAMARK_SLASH do it".into(), "  /tmp/QAMARK_PATH.log fails".into()],
        "commands (and any text starting with '/') go to the command worker, not to agents: {commands:?}"
    );
    check!(
        r.fake.sends_to(100).len() + r.fake.sends_to(101).len() == sends_before
            && r.fake.sends_to(999).is_empty(),
        "General/foreign/stranger/service produced no notice"
    );

    // 2. Reply split order and document.
    let para = |c: char| format!("{}\n\n", c.to_string().repeat(3000));
    let long: String = ['a', 'b', 'c', 'd'].into_iter().map(para).collect();
    let want = split_for_telegram(&long, SplitOptions::default());
    check!(want.chunks.len() == 4 && !want.prefer_file, "4-chunk reply stays messages");
    let base101 = r.fake.sends_to(101).len();
    r.agents.send(reply(2, &long)).await.unwrap();
    r.agents.send(reply(2, "short QAMARK_REPLY")).await.unwrap();
    until("B reply chunks", || r.fake.sends_to(101).len() >= base101 + 5).await;
    let mut expected: Vec<String> = want.chunks.clone();
    expected.push("short QAMARK_REPLY".into());
    let got = r.fake.sends_to(101)[base101..].to_vec();
    check!(got == expected, "reply chunks in order to topic 101 (got {} parts)", got.len());
    let concat: String = got[..4].concat();
    check!(concat == long, "chunks concatenate back to the reply");
    let base100 = r.fake.sends_to(100).len();
    let huge: String = ['a', 'b', 'c', 'd', 'e'].into_iter().map(para).collect();
    check!(split_for_telegram(&huge, SplitOptions::default()).prefer_file, "5-chunk reply prefers file");
    r.agents.send(reply(1, &huge)).await.unwrap();
    until("A document", || !r.fake.docs_to(100).is_empty()).await;
    settle().await;
    check!(
        r.fake.docs_to(100) == vec![huge.as_bytes().to_vec()] && r.fake.sends_to(100).len() == base100,
        ">4 chunks -> exactly one document with the whole text, no message chunks"
    );
    check!(r.fake.docs_to(101).is_empty(), "no document to the other topic");

    // 3. Dead slot: one notice per 60 s.
    r.hooks.send(end(B, 11, None)).await.unwrap();
    settle().await;
    let base = r.fake.sends_to(101).len();
    for i in 0..10 {
        route(vec![msg(100 + i, USER, Some(101), json!({"text": "dead QAMARK_DEAD"}))], &r.control, &mut commands);
    }
    settle().await;
    let notices = |r: &Rig| r.fake.sends_to(101)[base..].iter().filter(|t| *t == OFFLINE_NOTICE).count();
    check!(notices(&r) == 1, "10 messages to dead slot -> 1 offline notice (got {})", notices(&r));
    check!(drain(&mut rx2).is_empty(), "ended session's still-linked agent got no inbound");
    for i in 0..3 {
        route(vec![msg(200 + i, USER, Some(101), json!({"photo": [{"file_id": "f"}], "caption": "QAMARK_CAPTION"}))], &r.control, &mut commands);
    }
    settle().await;
    let text_only = r.fake.sends_to(101)[base..].iter().filter(|t| *t == TEXT_ONLY_NOTICE).count();
    check!(text_only == 1, "3 photos -> 1 text-only notice (got {text_only})");
    tokio::time::advance(Duration::from_secs(57)).await;
    route(vec![msg(300, USER, Some(101), json!({"text": "dead again"}))], &r.control, &mut commands);
    settle().await;
    check!(notices(&r) == 1, "inside 60 s -> no second notice (got {})", notices(&r));
    tokio::time::advance(Duration::from_secs(4)).await;
    route(vec![msg(301, USER, Some(101), json!({"text": "dead later"}))], &r.control, &mut commands);
    settle().await;
    check!(notices(&r) == 2, "after 60 s -> exactly one more notice (got {})", notices(&r));
    // Late reply of ended B.
    let before_late = r.fake.sends_to(101).len();
    r.agents.send(reply(2, "late B QAMARK_LATE")).await.unwrap();
    settle().await;
    check!(r.fake.sends_to(101).len() == before_late, "late reply of an ended session dropped");

    // 4. /clear on A (same pid): the connection moves.
    let creates_before = r.fake.creates.load(Ordering::SeqCst);
    r.hooks.send(end(A, 10, Some("clear"))).await.unwrap();
    r.hooks.send(start(C, 10, "clear")).await.unwrap();
    until("separator", || r.fake.sends_to(100).iter().any(|t| t.contains("cccccccc"))).await;
    route(vec![msg(400, USER, Some(100), json!({"text": "after clear QAMARK_CLEAR"}))], &r.control, &mut commands);
    settle().await;
    let got = drain(&mut rx1);
    check!(
        got.len() == 1 && got[0].0 == "after clear QAMARK_CLEAR",
        "after /clear the moved connection receives the slot's message: {got:?}"
    );
    let b100 = r.fake.sends_to(100).len();
    r.agents.send(reply(1, "reply after clear")).await.unwrap();
    until("reply after clear", || r.fake.sends_to(100).len() > b100).await;
    check!(r.fake.sends_to(100).last().map(String::as_str) == Some("reply after clear"), "reply after /clear lands in the slot topic");
    check!(r.fake.creates.load(Ordering::SeqCst) == creates_before, "/clear created no topic");

    // 5. Link drop + re-register with the stale env id A: follows pid to C.
    r.agents.send(AgentEvent::Disconnected { conn: 1 }).await.unwrap();
    let (ev, mut rx3) = register(3, A, 10);
    r.agents.send(ev).await.unwrap();
    settle().await;
    route(vec![msg(500, USER, Some(100), json!({"text": "after relink"}))], &r.control, &mut commands);
    settle().await;
    let got = drain(&mut rx3);
    check!(got.len() == 1 && got[0].0 == "after relink", "re-registered stale-id agent gets inbound: {got:?}");
    let b100 = r.fake.sends_to(100).len();
    r.agents.send(reply(1, "ghost QAMARK_GHOST")).await.unwrap();
    r.agents.send(reply(3, "relinked reply")).await.unwrap();
    until("relinked reply", || r.fake.sends_to(100).len() > b100).await;
    settle().await;
    check!(r.fake.sends_to(100)[b100..] == ["relinked reply".to_string()], "gone conn dropped, relinked conn replies");

    // 6. Two replies interleaved from two live slots keep their own order.
    r.hooks.send(start(B, 11, "resume")).await.unwrap();
    let (ev, mut rx4) = register(4, B, 11);
    r.agents.send(ev).await.unwrap();
    settle().await;
    route(vec![msg(600, USER, Some(101), json!({"text": "resumed B"}))], &r.control, &mut commands);
    settle().await;
    let got = drain(&mut rx4);
    check!(got.len() == 1, "resumed session in its old slot gets inbound: {got:?}");
    let (b0, b1) = (r.fake.sends_to(100).len(), r.fake.sends_to(101).len());
    for i in 0..20 {
        r.agents.send(reply(3, &format!("c{i}"))).await.unwrap();
        r.agents.send(reply(4, &format!("b{i}"))).await.unwrap();
    }
    until("interleaved", || r.fake.sends_to(100).len() >= b0 + 20 && r.fake.sends_to(101).len() >= b1 + 20).await;
    let c: Vec<String> = (0..20).map(|i| format!("c{i}")).collect();
    let b: Vec<String> = (0..20).map(|i| format!("b{i}")).collect();
    check!(r.fake.sends_to(100)[b0..] == c[..] && r.fake.sends_to(101)[b1..] == b[..], "per-topic FIFO under interleaving");
}

async fn stalled_scenario() {
    let r = rig("stall");
    r.hooks.send(start(A, 10, "startup")).await.unwrap();
    until("topic", || r.fake.creates.load(Ordering::SeqCst) == 1).await;
    let (ev, mut rx1) = register(1, A, 10);
    r.agents.send(ev).await.unwrap();
    settle().await;
    tokio::time::sleep(Duration::from_millis(300)).await;
    r.fake.stall_sends.store(true, Ordering::SeqCst);
    // Replies pile up behind a stuck send; agent events and inbound keep flowing.
    for i in 0..3000 {
        r.agents.send(reply(1, &format!("r{i}"))).await.unwrap();
    }
    let mut commands = Vec::new();
    route(vec![msg(1, USER, Some(100), json!({"text": "through the stall"}))], &r.control, &mut commands);
    settle().await;
    let got = drain(&mut rx1);
    check!(got.len() == 1 && got[0].0 == "through the stall", "inbound reaches agent while Telegram stalls: {got:?}");
}


/// Probe (not a pass/fail check): a stale duplicate connection of the same
/// claude pid when /clear happens. Which conn does the new session get?
async fn duplicate_clear_probe() -> (usize, usize) {
    let (mut to_new, mut to_stale) = (0, 0);
    for t in 0..20 {
        let r = rig(&format!("dup{t}"));
        r.hooks.send(start(A, 10, "startup")).await.unwrap();
        until("topic", || r.fake.creates.load(Ordering::SeqCst) == 1).await;
        let (ev, mut stale) = register(1, A, 10);
        r.agents.send(ev).await.unwrap();
        let (ev, mut fresh) = register(2, A, 10);
        r.agents.send(ev).await.unwrap();
        settle().await;
        r.hooks.send(end(A, 10, Some("clear"))).await.unwrap();
        r.hooks.send(start(C, 10, "clear")).await.unwrap();
        settle().await;
        let mut commands = Vec::new();
        route(vec![msg(1, USER, Some(100), json!({"text": "x"}))], &r.control, &mut commands);
        settle().await;
        if !drain(&mut fresh).is_empty() { to_new += 1; }
        if !drain(&mut stale).is_empty() { to_stale += 1; }
    }
    (to_new, to_stale)
}

fn main() {
    let captured = Captured::default();
    let writer = captured.clone();
    let sub = tracing_subscriber::fmt()
        .without_time()
        .with_max_level(tracing::Level::TRACE)
        .with_writer(move || writer.clone())
        .finish();
    tracing::subscriber::set_global_default(sub).unwrap();
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .start_paused(true)
        .build()
        .unwrap();
    rt.block_on(async {
        main_scenario().await;
        stalled_scenario().await;
        let (n, st) = duplicate_clear_probe().await;
        println!("[PROBE] after /clear with a stale duplicate conn: newest conn got inbound {n}/20, stale conn got it {st}/20");
    });
    let logs = String::from_utf8_lossy(&captured.0.lock().unwrap()).to_string();
    let leaks: Vec<&str> = ["QAMARK", &USER.to_string(), &STRANGER.to_string(), "proj", r"C:\w"]
        .into_iter()
        .filter(|m| logs.contains(m))
        .collect::<Vec<_>>()
        .into_iter()
        .map(|s| if s.starts_with("QAMARK") { "QAMARK" } else { "id/path" })
        .collect();
    for line in logs.lines().filter(|l| l.contains("QAMARK") || l.contains(&USER.to_string()) || l.contains("proj")) {
        println!("LEAK LINE: {line}");
    }
    check!(leaks.is_empty(), "no message text, user id or path in logs");
    std::fs::write(
        std::env::temp_dir().join("qa021-logs.txt"),
        logs.as_bytes(),
    )
    .ok();
    let fails = FAILS.load(Ordering::SeqCst);
    println!("fails={fails}");
    std::process::exit(if fails == 0 { 0 } else { 1 });
}
