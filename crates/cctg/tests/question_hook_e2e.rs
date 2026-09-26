//! `cctg hook PreToolUse` for `AskUserQuestion` as Claude Code runs it (a
//! process with JSON on stdin, configured through `<home>/.cctg/device.env`)
//! against the real `serve_hooks_and_asks`, slot actor and scheduler with a
//! fake Telegram (TASK-038): choices, ticks and the user's own text in the
//! topic come back as Claude Code's `allow` + `updatedInput` with `answers`;
//! ⌨ В терминале, a question nobody answers and a stopped hub give no
//! decision, exit 0 and an empty stdout; the `PermissionRequest` hook of a
//! question never waits, shows no Allow/Deny and tells the topic once that
//! the client lacks the question hook.

use std::io::Write;
use std::net::{Ipv4Addr, SocketAddr};
use std::path::PathBuf;
use std::process::{Output, Stdio};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use cctg::hub::api::{ForumTopic, Message};
use cctg::hub::ingress::{self, AgentEvent};
use cctg::hub::questions::{self, Press};
use cctg::hub::registry::RegistryStore;
use cctg::hub::scheduler::{BucketConfig, Delivery, Op, Outcome, Scheduler, Transport};
use cctg::hub::slots::{Control, Options, Slots};
use cctg::hub::updates::{CallbackInput, Inbound};
use cctg::wire::{HookEvent, HookPost, HubMsg, Register, Secret};
use serde_json::{Value, json};
use tokio::sync::mpsc;

mod common;

const SECRET: &str = "question-hook-secret-0123456789";
const SESSION: &str = "5e551017-0000-4000-8000-00000000a5c0";
const COLOR: &str = "Which private color?";
const FRUITS: &str = "Which private fruits?";

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

    /// (message id, ask id) of every question sent.
    fn questions(&self) -> Vec<(i64, String)> {
        let mut message_id = 1000;
        let mut found = Vec::new();
        for op in self.ops() {
            if let Op::Send { reply_markup, .. } = &op {
                let data = reply_markup
                    .as_ref()
                    .and_then(|markup| markup["inline_keyboard"][0][0]["callback_data"].as_str());
                if let Some((id, _, _)) = data.and_then(questions::parse_callback) {
                    found.push((message_id, id.to_owned()));
                }
                message_id += 1;
            }
        }
        found
    }

    fn permission_prompts(&self) -> usize {
        self.ops()
            .iter()
            .filter(|op| {
                matches!(op, Op::Send { reply_markup: Some(markup), .. }
                    if markup.to_string().contains("\"allow:"))
            })
            .count()
    }

    /// Sends of the "no question hook" notice: whether each had buttons.
    fn hook_hints(&self) -> Vec<bool> {
        self.ops()
            .iter()
            .filter_map(|op| match op {
                Op::Send {
                    text, reply_markup, ..
                } if text == questions::NO_HOOK_NOTICE => Some(reply_markup.is_some()),
                _ => None,
            })
            .collect()
    }

    fn last_edit_of(&self, message: i64) -> Option<String> {
        self.ops().into_iter().rev().find_map(|op| match op {
            Op::Edit {
                message_id, text, ..
            } if message_id == message => Some(text),
            _ => None,
        })
    }
}

/// A question's buttons are pressed as soon as its send reaches Telegram;
/// Telegram's answer (the message id) comes back to the hub only this much
/// later, so every press lands before the hub knows the message (TASK-060).
const QUESTION_ANSWER_LAG: Duration = Duration::from_millis(300);

impl Transport for Fake {
    async fn execute(&self, op: &Op) -> Delivery {
        self.ops.lock().unwrap().push(op.clone());
        if let Op::Send {
            reply_markup: Some(markup),
            ..
        } = op
            && markup["inline_keyboard"][0][0]["callback_data"]
                .as_str()
                .and_then(questions::parse_callback)
                .is_some()
        {
            tokio::time::sleep(QUESTION_ANSWER_LAG).await;
        }
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
    control: mpsc::UnboundedSender<Control>,
    _hooks: mpsc::Sender<HookPost>,
    _agents: mpsc::Sender<AgentEvent>,
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
async fn hub(test: &str, question_wait: Duration) -> Hub {
    let state =
        std::env::temp_dir().join(format!("cctg-question-hook-{test}-{}", std::process::id()));
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
        question_wait,
        ..Options::default()
    };
    let mut slots = Slots::new(store.load().unwrap(), store, outbox, options);
    let asks = slots.permission_asks();
    let questions = slots.question_asks();
    let (agents, agents_rx) = mpsc::channel(16);
    let (hooks, hooks_rx) = mpsc::channel(16);
    let (control, control_rx) = mpsc::unbounded_channel();
    tokio::spawn(slots.run(agents_rx, hooks_rx, control_rx));
    let (hook_tx, mut hook_rx) = mpsc::channel(16);
    let listener = ingress::bind(SocketAddr::from((Ipv4Addr::LOCALHOST, 0)))
        .await
        .unwrap();
    let addr = listener.local_addr().unwrap().to_string();
    tokio::spawn(ingress::serve_hooks_and_asks(
        listener,
        Secret::parse(SECRET).unwrap(),
        hook_tx,
        asks,
        questions,
    ));
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
                console_commands: false,
                client: None,
                files: false,
                session_reads: false,
                status_lines: false,
                heartbeat: false,
            },
            to_agent,
        })
        .await
        .unwrap();
    Hub {
        addr,
        fake,
        control,
        _hooks: hooks,
        _agents: agents,
        _to_agent: to_agent_rx,
        _state: TempState(state),
    }
}

/// A home directory whose `.cctg/device.env` points at `addr`.
fn home(test: &str, addr: &str) -> PathBuf {
    let home = common::own_tmp().join(format!("question-hook-{test}"));
    let dir = home.join(".cctg");
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(
        dir.join("device.env"),
        format!("CCTG_HUB_SECRET={SECRET}\nCCTG_HUB_HOOK_ADDR={addr}\nCCTG_HOST=box\n"),
    )
    .unwrap();
    home
}

fn tool_input() -> Value {
    json!({
        "questions": [
            { "question": COLOR, "header": "Color", "multiSelect": false,
              "options": [{ "label": "Red", "description": "warm" }, { "label": "Blue" }] },
            { "question": FRUITS, "header": "Fruits", "multiSelect": true,
              "options": [{ "label": "Apple" }, { "label": "Pear" }, { "label": "Plum" }] },
        ],
    })
}

fn input(event: &str) -> Vec<u8> {
    json!({
        "session_id": SESSION,
        "transcript_path": "/p/s.jsonl",
        "cwd": r"C:\w\p",
        "hook_event_name": event,
        "permission_mode": "default",
        "tool_name": "AskUserQuestion",
        "tool_use_id": "toolu_01question",
        "tool_input": tool_input(),
    })
    .to_string()
    .into_bytes()
}

/// Runs the real hook binary for `event` on a blocking thread.
async fn run_hook(home: PathBuf, event: &'static str) -> (Output, Duration) {
    tokio::task::spawn_blocking(move || {
        let started = Instant::now();
        let mut child = common::cctg(&home)
            .args(["hook", event])
            .env("RUST_LOG", "trace")
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .expect("cctg starts");
        let mut stdin = child.stdin.take().unwrap();
        let _ = stdin.write_all(&input(event));
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
    for private in [SECRET, "private color", "private fruits", "Plum", "fig"] {
        assert!(!stderr.contains(private), "{private} in {stderr}");
    }
}

fn press(hub: &Hub, message_id: i64, id: &str, question: usize, press: Press) {
    hub.control
        .send(Control::Callback(CallbackInput {
            query_id: "q".into(),
            data: Some(questions::callback_data(id, question, press)),
            message_id: Some(message_id),
            from_name: None,
        }))
        .unwrap();
}

fn say(hub: &Hub, message_id: i64, text: &str, reply_to: Option<i64>) {
    hub.control
        .send(Control::Message(Inbound {
            message_id,
            thread_id: Some(100),
            text: Some(text.into()),
            reply_to,
            quote: None,
            forwarded: false,
            media: None,
            from_name: None,
        }))
        .unwrap();
}

/// The decision a hook printed: Claude Code's `PreToolUse` `allow` with the
/// whole input plus `answers`.
fn assert_answers(output: &Output, color: &str, fruits: &str) {
    let decision: Value = serde_json::from_slice(&output.stdout).expect("decision JSON");
    let mut updated = tool_input();
    updated["answers"] = json!({ COLOR: color, FRUITS: fruits });
    assert_eq!(
        decision,
        json!({ "hookSpecificOutput": {
            "hookEventName": "PreToolUse",
            "permissionDecision": "allow",
            "permissionDecisionReason": "Answered in Telegram",
            "updatedInput": updated,
        }})
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn choices_in_the_topic_are_the_hooks_answers() {
    let hub = hub("choices", Duration::from_secs(60)).await;
    let running = tokio::spawn(run_hook(home("choices", &hub.addr), "PreToolUse"));
    until("question", || hub.fake.questions().len() == 1).await;
    let (message_id, id) = hub.fake.questions().remove(0);
    let text = hub
        .fake
        .ops()
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
    assert!(
        text.contains(COLOR) && text.contains("1. Red — warm"),
        "{text}"
    );
    press(&hub, message_id, &id, 0, Press::Option(1));
    press(&hub, message_id, &id, 1, Press::Option(0));
    press(&hub, message_id, &id, 1, Press::Option(2));
    press(&hub, message_id, &id, 1, Press::Done);
    let (output, _) = running.await.unwrap();
    assert_clean(&output);
    assert_answers(&output, "Blue", "Apple, Plum");
    until("answers shown", || {
        hub.fake
            .last_edit_of(message_id)
            .is_some_and(|text| text.starts_with(questions::ANSWERED_TITLE))
    })
    .await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn own_text_answers_after_other_or_as_a_reply() {
    let hub = hub("other", Duration::from_secs(60)).await;
    let running = tokio::spawn(run_hook(home("other", &hub.addr), "PreToolUse"));
    until("question", || hub.fake.questions().len() == 1).await;
    let (message_id, id) = hub.fake.questions().remove(0);
    press(&hub, message_id, &id, 0, Press::Other);
    // Cyrillic own text: the hook's stdout carries it as UTF-8 JSON.
    say(&hub, 50, "Бирюзовый, как море", None);
    // The second question: a tick, then a reply to the question message.
    press(&hub, message_id, &id, 1, Press::Option(1));
    // A reply needs the message the user sees: the hub knows its id once it
    // edits it (a press, unlike a reply, does not wait for that).
    until("message known", || {
        hub.fake.last_edit_of(message_id).is_some()
    })
    .await;
    say(&hub, 51, "and a fig", Some(message_id));
    let (output, _) = running.await.unwrap();
    assert_clean(&output);
    assert_answers(&output, "Бирюзовый, как море", "Pear, and a fig");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn the_terminal_button_gives_no_decision_at_once() {
    let hub = hub("terminal", Duration::from_secs(60)).await;
    let running = tokio::spawn(run_hook(home("terminal", &hub.addr), "PreToolUse"));
    until("question", || hub.fake.questions().len() == 1).await;
    let (message_id, id) = hub.fake.questions().remove(0);
    let pressed = Instant::now();
    press(&hub, message_id, &id, 0, Press::Terminal);
    let (output, _) = running.await.unwrap();
    assert_clean(&output);
    assert!(output.stdout.is_empty());
    assert!(
        pressed.elapsed() < Duration::from_secs(5),
        "{:?}",
        pressed.elapsed()
    );
    until("question closed", || {
        hub.fake
            .last_edit_of(message_id)
            .is_some_and(|text| text.starts_with(questions::TERMINAL_TITLE))
    })
    .await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn an_unanswered_question_gives_no_decision() {
    let hub = hub("expired", Duration::from_millis(500)).await;
    let (output, elapsed) = run_hook(home("expired", &hub.addr), "PreToolUse").await;
    assert_clean(&output);
    assert!(output.stdout.is_empty());
    assert!(elapsed < Duration::from_secs(10), "{elapsed:?}");
    let (message_id, _) = hub.fake.questions().remove(0);
    until("question closed", || {
        hub.fake
            .last_edit_of(message_id)
            .is_some_and(|text| text.starts_with(questions::EXPIRED_TITLE))
    })
    .await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn the_permission_hook_of_a_question_never_waits() {
    let hub = hub("permission", Duration::from_secs(60)).await;
    for _ in 0..2 {
        let (output, elapsed) = run_hook(home("permission", &hub.addr), "PermissionRequest").await;
        assert_clean(&output);
        assert!(output.stdout.is_empty());
        assert!(elapsed < Duration::from_millis(1500), "{elapsed:?}");
    }
    tokio::time::sleep(Duration::from_secs(2)).await;
    assert_eq!(hub.fake.permission_prompts(), 0, "no Allow/Deny");
    assert!(hub.fake.questions().is_empty());
    // No question hook asked first: the topic is told once, without buttons.
    assert_eq!(hub.fake.hook_hints(), [false]);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_stopped_hub_means_no_decision_and_a_quick_exit() {
    let port = std::net::TcpListener::bind((Ipv4Addr::LOCALHOST, 0))
        .unwrap()
        .local_addr()
        .unwrap();
    let (output, elapsed) = run_hook(home("down", &port.to_string()), "PreToolUse").await;
    assert_clean(&output);
    assert!(output.stdout.is_empty());
    assert!(elapsed < Duration::from_secs(4), "{elapsed:?}");
}
