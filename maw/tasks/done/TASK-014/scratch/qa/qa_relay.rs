//! QA (TASK-014): end-to-end relay against the real `Slots` actor, the real
//! `Scheduler` (fast bucket) and fake agent links. Written by QA, independent
//! of the in-crate tests. Copied into a throwaway workspace copy as
//! `crates/cctg/tests/qa_relay.rs`; never part of the repo.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant as StdInstant};

use cctg::hub::api::{ApiError, ForumTopic, Message};
use cctg::hub::config::Allowlist;
use cctg::hub::ingress::AgentEvent;
use cctg::hub::permissions;
use cctg::hub::registry::{Icons, RegistryStore};
use cctg::hub::scheduler::{BucketConfig, Delivery, Op, Outcome, Scheduler, Transport};
use cctg::hub::slots::{Control, Options, Slots};
use cctg::hub::updates::{Ignored, Routed, route_batch};
use cctg::wire::{AgentMsg, Behavior, HookEvent, HookPost, HubMsg, PermissionRequest, Register};
use serde_json::{Value, json};
use tokio::sync::mpsc;

const CHAT: i64 = -1000000000001;
const USER: i64 = 7_318_046_259;
const STRANGER: i64 = 6_402_917_385;
const A: &str = "aaaaaaaa-0000-4000-8000-000000000001";
const B: &str = "bbbbbbbb-0000-4000-8000-000000000002";
const C: &str = "cccccccc-0000-4000-8000-000000000003";

/// Records every op with the message id it was given. Topics get thread ids
/// from 100 up, messages ids from 1000 up. The first `fail_closed_edits`
/// edits to the closing text fail with a transient error.
#[derive(Default)]
struct Tg {
    log: Mutex<Vec<(Op, Option<i64>)>>,
    next_thread: Mutex<i64>,
    next_message: Mutex<i64>,
    fail_closed_edits: Mutex<u32>,
}

impl Tg {
    fn ops(&self) -> Vec<Op> {
        self.log.lock().unwrap().iter().map(|(op, _)| op.clone()).collect()
    }
    fn log(&self) -> Vec<(Op, Option<i64>)> {
        self.log.lock().unwrap().clone()
    }
}

impl Transport for Tg {
    async fn execute(&self, op: &Op) -> Delivery {
        let (result, id) = match op {
            Op::CreateTopic { name, .. } => {
                let mut next = self.next_thread.lock().unwrap();
                let id = 100 + *next;
                *next += 1;
                (
                    Ok(Outcome::Topic(ForumTopic {
                        message_thread_id: id,
                        name: name.clone(),
                        icon_custom_emoji_id: None,
                    })),
                    Some(id),
                )
            }
            Op::Send { .. } | Op::SendDocument { .. } => {
                let mut next = self.next_message.lock().unwrap();
                let id = 1000 + *next;
                *next += 1;
                (
                    Ok(Outcome::Sent(Message {
                        message_id: id,
                        ..Message::default()
                    })),
                    Some(id),
                )
            }
            Op::Edit { text, .. } if text == permissions::CLOSED_TEXT => {
                let mut left = self.fail_closed_edits.lock().unwrap();
                if *left > 0 {
                    *left -= 1;
                    (
                        Err(ApiError::Telegram {
                            code: 500,
                            description: "Internal Server Error".into(),
                        }),
                        None,
                    )
                } else {
                    (Ok(Outcome::Done), None)
                }
            }
            _ => (Ok(Outcome::Done), None),
        };
        self.log.lock().unwrap().push((op.clone(), id));
        result
    }
}

struct Hub {
    tg: Arc<Tg>,
    agents: mpsc::Sender<AgentEvent>,
    hooks: mpsc::Sender<HookPost>,
    control: mpsc::UnboundedSender<Control>,
    state: std::path::PathBuf,
}

impl Drop for Hub {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.state);
    }
}

fn hub(name: &str, bucket: BucketConfig, retry_every: Duration) -> Hub {
    let state = std::env::temp_dir().join(format!("cctg-qa-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&state);
    std::fs::create_dir_all(&state).unwrap();
    let tg = Arc::new(Tg::default());
    let (scheduler, outbox) = Scheduler::new(tg.clone(), bucket);
    tokio::spawn(scheduler.run());
    let store = RegistryStore::open(&state).unwrap();
    let options = Options {
        grace: Duration::ZERO,
        chat_id: CHAT,
        retry_every,
        ..Options::default()
    };
    let (slots, _view) = Slots::new(store.load().unwrap(), store, outbox, options);
    let (agents, agents_rx) = mpsc::channel(64);
    let (hooks, hooks_rx) = mpsc::channel(64);
    let (control, control_rx) = mpsc::unbounded_channel();
    tokio::spawn(slots.run(agents_rx, hooks_rx, control_rx));
    Hub {
        tg,
        agents,
        hooks,
        control,
        state,
    }
}

fn fast() -> BucketConfig {
    BucketConfig {
        capacity: 100,
        refill_every: Duration::from_millis(10),
        min_gap: Duration::ZERO,
    }
}

async fn until(what: &str, mut ready: impl FnMut() -> bool) {
    let wait = async {
        while !ready() {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    };
    tokio::time::timeout(Duration::from_secs(20), wait)
        .await
        .unwrap_or_else(|_| panic!("{what} in time"));
}

fn post(session: &str, cwd: &str, event: HookEvent) -> HookPost {
    HookPost::new("box".into(), session.into(), cwd.into(), String::new(), event)
}

fn start(session: &str, cwd: &str, pid: u32, source: &str) -> HookPost {
    post(
        session,
        cwd,
        HookEvent::SessionStart {
            source: Some(source.into()),
            claude_pid: Some(pid),
            parent_claude_pid: None,
        },
    )
}

fn end(session: &str, cwd: &str, pid: u32, reason: Option<&str>) -> HookPost {
    post(
        session,
        cwd,
        HookEvent::SessionEnd {
            reason: reason.map(str::to_owned),
            claude_pid: Some(pid),
        },
    )
}

impl Hub {
    async fn hook(&self, post: HookPost) {
        self.hooks.send(post).await.unwrap();
    }

    async fn connect(&self, conn: u64, session: &str, cwd: &str, pid: u32) -> mpsc::Receiver<HubMsg> {
        let (to_agent, rx) = mpsc::channel(16);
        self.agents
            .send(AgentEvent::Registered {
                conn,
                register: Register {
                    session_id: session.into(),
                    host: "box".into(),
                    cwd: cwd.into(),
                    claude_pid: Some(pid),
                    verdict_ack: true,
                },
                to_agent,
            })
            .await
            .unwrap();
        rx
    }

    async fn request_at(&self, conn: u64, id: &str, preview: &str, at: StdInstant) {
        self.agents
            .send(AgentEvent::Message {
                conn,
                received_at: at,
                msg: AgentMsg::PermissionRequest(PermissionRequest {
                    request_id: id.into(),
                    tool_name: "Bash".into(),
                    description: "run a command".into(),
                    input_preview: preview.into(),
                }),
            })
            .await
            .unwrap();
    }

    async fn request(&self, conn: u64, id: &str, preview: &str) {
        self.request_at(conn, id, preview, StdInstant::now()).await;
    }

    async fn ack(&self, conn: u64, verdict_id: u64) {
        self.agents
            .send(AgentEvent::Message {
                conn,
                received_at: StdInstant::now(),
                msg: AgentMsg::PermissionAck { verdict_id },
            })
            .await
            .unwrap();
    }

    /// Presses a button through the real poll classification.
    fn press(&self, update_id: i64, from: i64, message_id: i64, data: &str) -> Vec<Routed> {
        let allowlist: Allowlist = [USER].into_iter().collect();
        let update = json!({ "update_id": update_id, "callback_query": {
            "id": format!("q{update_id}"),
            "from": { "id": from, "is_bot": false, "first_name": "x" },
            "chat_instance": "c",
            "data": data,
            "message": {
                "message_id": message_id, "message_thread_id": 100, "is_topic_message": true,
                "date": 1, "text": "prompt",
                "chat": { "id": CHAT, "type": "supergroup", "is_forum": true },
            },
        }});
        let (_, routed) = route_batch(vec![update], None, CHAT, &allowlist);
        for item in &routed {
            if let Routed::Callback(input) = item {
                self.control.send(Control::Callback(input.clone())).unwrap();
            }
        }
        routed
    }

    /// Permission prompts: (thread, text, message id, keyboard).
    fn prompts(&self) -> Vec<(i64, String, i64, Value)> {
        self.tg
            .log()
            .into_iter()
            .filter_map(|(op, id)| match op {
                Op::Send {
                    thread_id: Some(thread),
                    text,
                    reply_markup: Some(markup),
                    permission: true,
                } => Some((thread, text, id.unwrap(), markup)),
                _ => None,
            })
            .collect()
    }

    fn edits_of(&self, message: i64) -> Vec<(String, Option<Value>)> {
        self.tg
            .ops()
            .into_iter()
            .filter_map(|op| match op {
                Op::Edit {
                    message_id,
                    text,
                    reply_markup,
                } if message_id == message => Some((text, reply_markup)),
                _ => None,
            })
            .collect()
    }

    fn answers(&self) -> Vec<Option<String>> {
        self.tg
            .ops()
            .into_iter()
            .filter_map(|op| match op {
                Op::AnswerCallback { text, .. } => Some(text),
                _ => None,
            })
            .collect()
    }

    fn icons_of(&self, thread: i64) -> Vec<String> {
        self.tg
            .ops()
            .into_iter()
            .filter_map(|op| match op {
                Op::EditTopic {
                    thread_id,
                    icon_custom_emoji_id: Some(icon),
                    ..
                } if thread_id == thread => Some(icon),
                _ => None,
            })
            .collect()
    }

    fn topics(&self) -> HashMap<String, i64> {
        self.tg
            .log()
            .into_iter()
            .filter_map(|(op, id)| match op {
                Op::CreateTopic { name, .. } => Some((name, id.unwrap())),
                _ => None,
            })
            .collect()
    }
}

async fn verdict(rx: &mut mpsc::Receiver<HubMsg>) -> (String, Behavior, Option<u64>) {
    match tokio::time::timeout(Duration::from_secs(10), rx.recv()).await {
        Ok(Some(HubMsg::PermissionVerdict {
            request_id,
            behavior,
            verdict_id,
        })) => (request_id, behavior, verdict_id),
        other => panic!("expected a verdict, got {other:?}"),
    }
}

/// Skips retry-tick repeats of earlier verdicts.
async fn verdict_for(rx: &mut mpsc::Receiver<HubMsg>, id: &str) -> (String, Behavior, Option<u64>) {
    loop {
        let got = verdict(rx).await;
        if got.0 == id {
            return got;
        }
    }
}

async fn silent(rx: &mut mpsc::Receiver<HubMsg>, what: &str) {
    tokio::time::sleep(Duration::from_millis(250)).await;
    if let Ok(msg) = rx.try_recv() {
        panic!("{what}: unexpected {msg:?}");
    }
}

fn waiting() -> String {
    Icons::default().waiting.unwrap()
}
fn alive() -> String {
    Icons::default().alive.unwrap()
}
fn dead() -> String {
    Icons::default().dead.unwrap()
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn qa_relay_end_to_end() {
    let hub = hub("relay", fast(), Duration::from_millis(300));
    let (cwd_a, cwd_b) = (r"C:\qa\alpha", r"C:\qa\beta");
    hub.hook(start(A, cwd_a, 10, "startup")).await;
    hub.hook(start(B, cwd_b, 20, "startup")).await;
    until("two topics", || hub.topics().len() == 2).await;
    let topics = hub.topics();
    let thread_of = |folder: &str| {
        *topics
            .iter()
            .find(|(name, _)| name.contains(folder))
            .unwrap_or_else(|| panic!("topic of {folder}: {topics:?}"))
            .1
    };
    let (ta, tb) = (thread_of("alpha"), thread_of("beta"));
    assert_ne!(ta, tb);

    let mut a1 = hub.connect(1, A, cwd_a, 10).await;
    let mut b2 = hub.connect(2, B, cwd_b, 20).await;
    // Long preview with astral chars (2 UTF-16 units each).
    let huge: String = "😀".repeat(6000);
    hub.request(1, "abcde", &huge).await;
    hub.request(2, "abcde", "echo b").await;
    until("two prompts", || hub.prompts().len() == 2).await;

    // 1. Each prompt in its own session's topic, bounded, two small buttons.
    let prompts = hub.prompts();
    let pa = prompts.iter().find(|p| p.0 == ta).expect("A's prompt in A's topic").clone();
    let pb = prompts.iter().find(|p| p.0 == tb).expect("B's prompt in B's topic").clone();
    assert!(transcript::telegram_len(&pa.1) <= 4096, "{}", transcript::telegram_len(&pa.1));
    assert!(pa.1.starts_with("Запрос разрешения: Bash"));
    for p in [&pa, &pb] {
        let buttons = p.3["inline_keyboard"][0].as_array().unwrap();
        assert_eq!(buttons.len(), 2);
        let data: Vec<&str> = buttons
            .iter()
            .map(|b| b["callback_data"].as_str().unwrap())
            .collect();
        assert_eq!(data, ["allow:abcde", "deny:abcde"]);
        assert!(data.iter().all(|d| d.len() <= 64));
    }
    until("waiting icons", || {
        hub.icons_of(ta).last() == Some(&waiting()) && hub.icons_of(tb).last() == Some(&waiting())
    })
    .await;

    // 2. Stranger press: filtered at the gate, never answered, no verdict.
    let routed = hub.press(1, STRANGER, pa.2, "allow:abcde");
    assert_eq!(routed, [Routed::Ignored(Ignored::NotAllowed)]);
    silent(&mut a1, "stranger press").await;
    silent(&mut b2, "stranger press").await;
    assert!(hub.answers().is_empty(), "a stranger gets no answer");

    // 3. First press: exactly one verdict with an id, only to A's agent.
    hub.press(2, USER, pa.2, "allow:abcde");
    let (rid, behavior, vid) = verdict(&mut a1).await;
    assert_eq!((rid.as_str(), behavior), ("abcde", Behavior::Allow));
    let vid = vid.expect("acking agent gets an id");
    silent(&mut b2, "B must not get A's verdict").await;
    until("answer", || hub.answers().len() == 1).await;
    assert_eq!(hub.answers()[0].as_deref(), Some(permissions::ANSWER_ALLOWED));
    assert!(hub.edits_of(pa.2).is_empty(), "no edit before the ack");

    // 4. Link drops before the ack; the reconnected agent gets the same id.
    hub.agents.send(AgentEvent::Disconnected { conn: 1 }).await.unwrap();
    let mut a3 = hub.connect(3, A, cwd_a, 10).await;
    let (rid2, behavior2, vid2) = verdict(&mut a3).await;
    assert_eq!((rid2.as_str(), behavior2, vid2), ("abcde", Behavior::Allow, Some(vid)));
    // A press while still unacked: "already decided", the same verdict again.
    hub.press(3, USER, pa.2, "deny:abcde");
    let (_, again, vid3) = verdict(&mut a3).await;
    assert_eq!((again, vid3), (Behavior::Allow, Some(vid)), "first answer stays");
    until("answer 2", || hub.answers().len() == 2).await;
    assert_eq!(hub.answers()[1].as_deref(), Some(permissions::ANSWER_DECIDED));
    assert!(hub.edits_of(pa.2).is_empty(), "still no edit before the ack");

    // An ack from B's agent for A's verdict id is ignored.
    hub.ack(2, vid).await;
    tokio::time::sleep(Duration::from_millis(250)).await;
    assert!(hub.edits_of(pa.2).is_empty(), "foreign ack ignored");

    hub.ack(3, vid).await;
    until("decision edit", || !hub.edits_of(pa.2).is_empty()).await;
    let edits = hub.edits_of(pa.2);
    assert_eq!(edits.len(), 1);
    assert!(edits[0].0.ends_with(permissions::ALLOWED_MARK));
    assert!(transcript::telegram_len(&edits[0].0) <= 4096);
    assert_eq!(edits[0].1, Some(permissions::no_keyboard()));
    until("A icon back to alive", || hub.icons_of(ta).last() == Some(&alive())).await;
    assert_eq!(hub.icons_of(tb).last(), Some(&waiting()), "B still waits");

    // 5. Later press after the decision: already decided, no verdict.
    // Drain retry-tick repeats that may have landed before the ack.
    while a3.try_recv().is_ok() {}
    hub.press(4, USER, pa.2, "allow:abcde");
    until("answer 3", || hub.answers().len() == 3).await;
    assert_eq!(hub.answers()[2].as_deref(), Some(permissions::ANSWER_DECIDED));
    silent(&mut a3, "no verdict after decision").await;
    // A retry tick after the decision must not resend either.
    tokio::time::sleep(Duration::from_millis(700)).await;
    assert!(a3.try_recv().is_err(), "no resend after the ack");

    // 6. Same five letters in B: separate prompt, Deny goes to B only.
    hub.press(5, USER, pb.2, "deny:abcde");
    let (rb, bb, vb) = verdict(&mut b2).await;
    assert_eq!((rb.as_str(), bb), ("abcde", Behavior::Deny));
    silent(&mut a3, "A must not get B's verdict").await;
    hub.ack(2, vb.unwrap()).await;
    until("B decision edit", || !hub.edits_of(pb.2).is_empty()).await;
    assert!(hub.edits_of(pb.2)[0].0.ends_with(permissions::DENIED_MARK));
    // A's decided message was not touched again.
    assert_eq!(hub.edits_of(pa.2).len(), 1);

    // 7. Waiting icon follows active prompts: two open in B, one decided.
    hub.request(2, "fghij", "one").await;
    hub.request(2, "kmnop", "two").await;
    until("B prompts", || hub.prompts().len() == 4).await;
    let pf = hub.prompts().into_iter().find(|p| p.1.ends_with("one")).unwrap();
    let pk = hub.prompts().into_iter().find(|p| p.1.ends_with("two")).unwrap();
    until("B waiting", || hub.icons_of(tb).last() == Some(&waiting())).await;
    hub.press(6, USER, pf.2, "allow:fghij");
    let (_, _, vf) = verdict_for(&mut b2, "fghij").await;
    hub.ack(2, vf.unwrap()).await;
    until("fghij edited", || !hub.edits_of(pf.2).is_empty()).await;
    tokio::time::sleep(Duration::from_millis(300)).await;
    assert_eq!(hub.icons_of(tb).last(), Some(&waiting()), "kmnop still open");

    // 8. SessionEnd closes the open prompt; the closing edit is retried.
    *hub.tg.fail_closed_edits.lock().unwrap() = 2;
    hub.hook(end(B, cwd_b, 20, None)).await;
    until("closing edit applied", || hub.edits_of(pk.2).len() >= 3).await;
    let closing = hub.edits_of(pk.2);
    assert!(
        closing
            .iter()
            .all(|(text, markup)| text == permissions::CLOSED_TEXT
                && *markup == Some(permissions::no_keyboard())),
        "{closing:?}"
    );
    tokio::time::sleep(Duration::from_millis(900)).await;
    assert_eq!(hub.edits_of(pk.2).len(), 3, "no edit after it was applied");
    until("B dead", || hub.icons_of(tb).last() == Some(&dead())).await;
    // A press on the closed prompt is harmless.
    hub.press(7, USER, pk.2, "allow:kmnop");
    until("answer closed", || hub.answers().len() == 6).await;
    assert_eq!(hub.answers()[5].as_deref(), Some(permissions::ANSWER_EXPIRED));
    tokio::time::sleep(Duration::from_millis(250)).await;
    while let Ok(msg) = b2.try_recv() {
        assert!(
            !matches!(&msg, HubMsg::PermissionVerdict { request_id, .. } if request_id == "kmnop"),
            "no verdict on a closed prompt: {msg:?}"
        );
    }

    // 9. Disconfirmation: B's agent still linked; a request read before and
    // one read after the SessionEnd are both consumed after it: no prompt.
    let before = StdInstant::now() - Duration::from_secs(5);
    hub.request_at(2, "qrstu", "late 1", before).await;
    hub.request(2, "vwxyz", "late 2").await;
    tokio::time::sleep(Duration::from_millis(400)).await;
    assert_eq!(hub.prompts().len(), 4, "no prompt for an ended session");
    assert!(b2.try_recv().is_err());
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn qa_clear_moves_the_slot_on_and_prompts_follow_their_session() {
    let hub = hub("clear", fast(), Duration::from_millis(300));
    let cwd = r"C:\qa\gamma";
    hub.hook(start(A, cwd, 10, "startup")).await;
    until("topic", || hub.topics().len() == 1).await;
    let thread = *hub.topics().values().next().unwrap();
    let mut a1 = hub.connect(1, A, cwd, 10).await;
    hub.request(1, "abcde", "before clear").await;
    until("prompt", || hub.prompts().len() == 1).await;
    let pa = hub.prompts()[0].clone();
    let read_before_clear = StdInstant::now();
    tokio::time::sleep(Duration::from_millis(5)).await;

    // /clear, new SessionStart first (start-first order).
    hub.hook(start(C, cwd, 10, "clear")).await;
    hub.hook(end(A, cwd, 10, Some("clear"))).await;
    until("A prompt closed", || !hub.edits_of(pa.2).is_empty()).await;
    assert_eq!(
        hub.edits_of(pa.2),
        [(permissions::CLOSED_TEXT.to_owned(), Some(permissions::no_keyboard()))]
    );
    // A frame read before /clear and consumed after: still A's, dropped.
    hub.request_at(1, "bcdef", "queued before clear", read_before_clear).await;
    tokio::time::sleep(Duration::from_millis(300)).await;
    assert_eq!(hub.prompts().len(), 1, "pre-clear frame is not C's");
    // After: C's prompt lands in the same slot topic.
    hub.request(1, "cdefg", "after clear").await;
    until("C prompt", || hub.prompts().len() == 2).await;
    let pc = hub.prompts()[1].clone();
    assert_eq!(pc.0, thread);
    // Old A button now answers expired and sends nothing.
    hub.press(1, USER, pa.2, "allow:abcde");
    until("answer", || hub.answers().len() == 1).await;
    assert_eq!(hub.answers()[0].as_deref(), Some(permissions::ANSWER_EXPIRED));
    silent(&mut a1, "no verdict for A").await;
    // C's press reaches the same connection (now C's).
    hub.press(2, USER, pc.2, "allow:cdefg");
    let (rid, _, _) = verdict(&mut a1).await;
    assert_eq!(rid, "cdefg");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn qa_prompt_overtakes_another_topics_backlog() {
    // Slow bucket: one send per 150 ms.
    let bucket = BucketConfig {
        capacity: 1,
        refill_every: Duration::from_millis(150),
        min_gap: Duration::ZERO,
    };
    let hub = hub("overtake", bucket, Duration::from_secs(60));
    let (cwd_a, cwd_b) = (r"C:\qa\delta", r"C:\qa\eps");
    hub.hook(start(A, cwd_a, 10, "startup")).await;
    hub.hook(start(B, cwd_b, 20, "startup")).await;
    until("two topics", || hub.topics().len() == 2).await;
    let _a = hub.connect(1, A, cwd_a, 10).await;
    let _b = hub.connect(2, B, cwd_b, 20).await;
    tokio::time::sleep(Duration::from_millis(200)).await;
    for i in 0..10 {
        hub.agents
            .send(AgentEvent::Message {
                conn: 2,
                received_at: StdInstant::now(),
                msg: AgentMsg::Reply {
                    text: format!("reply {i}"),
                },
            })
            .await
            .unwrap();
    }
    tokio::time::sleep(Duration::from_millis(50)).await;
    hub.request(1, "abcde", "urgent").await;
    until("prompt", || hub.prompts().len() == 1).await;
    let sends: Vec<String> = hub
        .tg
        .ops()
        .into_iter()
        .filter_map(|op| match op {
            Op::Send { text, .. } => Some(text),
            _ => None,
        })
        .collect();
    let position = sends.iter().position(|t| t.ends_with("urgent")).unwrap();
    assert!(position <= 2, "prompt at {position}: {sends:?}");
}

impl Hub {
    async fn connect_with(
        &self,
        conn: u64,
        session: &str,
        cwd: &str,
        pid: Option<u32>,
        acks: bool,
        cap: usize,
    ) -> mpsc::Receiver<HubMsg> {
        let (to_agent, rx) = mpsc::channel(cap);
        self.agents
            .send(AgentEvent::Registered {
                conn,
                register: Register {
                    session_id: session.into(),
                    host: "box".into(),
                    cwd: cwd.into(),
                    claude_pid: pid,
                    verdict_ack: acks,
                },
                to_agent,
            })
            .await
            .unwrap();
        rx
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn qa_agent_before_session_start_prompt_waits_for_the_topic() {
    let hub = hub("early", fast(), Duration::from_millis(300));
    let cwd = r"C:\qa\zeta";
    let _a = hub.connect(1, A, cwd, 10).await;
    tokio::time::sleep(Duration::from_millis(50)).await;
    hub.request(1, "abcde", "early").await;
    tokio::time::sleep(Duration::from_millis(200)).await;
    assert!(hub.prompts().is_empty());
    hub.hook(start(A, cwd, 10, "startup")).await;
    until("prompt after start", || hub.prompts().len() == 1).await;
    let thread = *hub.topics().values().next().unwrap();
    assert_eq!(hub.prompts()[0].0, thread);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn qa_legacy_agent_is_decided_by_the_hand_off() {
    let hub = hub("legacy", fast(), Duration::from_millis(300));
    let cwd = r"C:\qa\eta";
    hub.hook(start(A, cwd, 10, "startup")).await;
    until("topic", || hub.topics().len() == 1).await;
    let mut a = hub.connect_with(1, A, cwd, Some(10), false, 16).await;
    hub.request(1, "abcde", "x").await;
    until("prompt", || hub.prompts().len() == 1).await;
    let p = hub.prompts()[0].clone();
    hub.press(1, USER, p.2, "deny:abcde");
    let (_, b, id) = verdict(&mut a).await;
    assert_eq!((b, id), (Behavior::Deny, None));
    until("edit", || !hub.edits_of(p.2).is_empty()).await;
    assert!(hub.edits_of(p.2)[0].0.ends_with(permissions::DENIED_MARK));
    silent(&mut a, "legacy gets one verdict").await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn qa_full_agent_queue_answers_offline_then_delivers_on_tick() {
    let hub = hub("full", fast(), Duration::from_millis(300));
    let cwd = r"C:\qa\theta";
    hub.hook(start(A, cwd, 10, "startup")).await;
    until("topic", || hub.topics().len() == 1).await;
    let mut a = hub.connect_with(1, A, cwd, Some(10), true, 1).await;
    hub.request(1, "abcde", "one").await;
    hub.request(1, "bcdef", "two").await;
    until("prompts", || hub.prompts().len() == 2).await;
    let (p1, p2) = (hub.prompts()[0].clone(), hub.prompts()[1].clone());
    hub.press(1, USER, p1.2, "allow:abcde");
    until("a1", || hub.answers().len() == 1).await;
    hub.press(2, USER, p2.2, "allow:bcdef");
    until("a2", || hub.answers().len() == 2).await;
    assert_eq!(hub.answers()[1].as_deref(), Some(permissions::ANSWER_OFFLINE));
    let (first, _, v1) = verdict(&mut a).await;
    assert_eq!(first, "abcde");
    hub.ack(1, v1.unwrap()).await;
    let (second, _, v2) = verdict_for(&mut a, "bcdef").await;
    assert_eq!(second, "bcdef");
    hub.ack(1, v2.unwrap()).await;
    until("both edited", || !hub.edits_of(p1.2).is_empty() && !hub.edits_of(p2.2).is_empty()).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn qa_nested_resume_end_keeps_the_prompt_and_reuse_of_pid_closes_it() {
    let hub = hub("nested", fast(), Duration::from_millis(300));
    let cwd = r"C:\qa\iota";
    hub.hook(start(A, cwd, 10, "startup")).await;
    until("topic", || hub.topics().len() == 1).await;
    let _a = hub.connect(1, A, cwd, 10).await;
    hub.request(1, "abcde", "x").await;
    until("prompt", || hub.prompts().len() == 1).await;
    let p = hub.prompts()[0].clone();
    // SessionEnd of a nested `--resume A` (other pid): ignored.
    hub.hook(end(A, cwd, 99, None)).await;
    tokio::time::sleep(Duration::from_millis(300)).await;
    assert!(hub.edits_of(p.2).is_empty(), "nested end closes nothing");
    // A new top-level session on the same pid (A crashed silently).
    hub.hook(start(B, cwd, 10, "startup")).await;
    until("closed", || !hub.edits_of(p.2).is_empty()).await;
    assert_eq!(hub.edits_of(p.2)[0].0, permissions::CLOSED_TEXT);
}
