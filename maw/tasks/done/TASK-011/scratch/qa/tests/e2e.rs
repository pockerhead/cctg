//! QA (TASK-011): scripted event sequences against the real `Slots` actor,
//! the real `Scheduler` and a fake `Transport`. Counts topics created,
//! separators sent, icon edits. Written by QA, independent of the author tests.

use std::sync::{Arc, Mutex};
use std::time::Duration;

use cctg::hub::api::{ApiError, ForumTopic, Message};
use cctg::hub::ingress::AgentEvent;
use cctg::hub::registry::{
    ICON_ALIVE, ICON_DEAD, ICON_NO_CHANNEL, ICON_WAITING, Registry, RegistryStore,
};
use cctg::hub::scheduler::{BucketConfig, Delivery, Op, Outcome, Scheduler, Transport};
use cctg::hub::slots::{Control, Options, Slots};
use cctg::wire::{AgentMsg, HookEvent, HookPost, HubMsg, PermissionRequest, Register};
use tokio::sync::mpsc;

const HOST: &str = "box";

#[derive(Default)]
struct Fake {
    ops: Mutex<Vec<Op>>,
    next_topic: Mutex<i64>,
    /// Errors for the next EditTopic / Send calls (popped from the end).
    edit_errors: Mutex<Vec<&'static str>>,
}

impl Transport for Fake {
    async fn execute(&self, op: &Op) -> Delivery {
        self.ops.lock().unwrap().push(op.clone());
        match op {
            Op::CreateTopic { name, .. } => {
                let mut next = self.next_topic.lock().unwrap();
                *next = (*next).max(100);
                let id = *next;
                *next += 1;
                Ok(Outcome::Topic(ForumTopic {
                    message_thread_id: id,
                    name: name.clone(),
                    icon_custom_emoji_id: None,
                }))
            }
            Op::EditTopic { .. } => match self.edit_errors.lock().unwrap().pop() {
                Some(d) => Err(ApiError::Telegram {
                    code: 400,
                    description: d.to_owned(),
                }),
                None => Ok(Outcome::Done),
            },
            Op::Send { .. } => Ok(Outcome::Sent(Message::default())),
            _ => Ok(Outcome::Done),
        }
    }
}

struct Rig {
    fake: Arc<Fake>,
    agents: mpsc::Sender<AgentEvent>,
    hooks: mpsc::Sender<HookPost>,
    control: mpsc::UnboundedSender<Control>,
    dir: std::path::PathBuf,
    keep: Vec<mpsc::Receiver<HubMsg>>,
}

fn temp_dir(name: &str) -> std::path::PathBuf {
    let dir = std::env::temp_dir().join(format!(
        "qa011-{name}-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

fn options() -> Options {
    Options {
        grace: Duration::ZERO,
        ..Options::default()
    }
}

fn rig_in(dir: std::path::PathBuf, options: Options) -> Rig {
    let fake = Arc::new(Fake::default());
    let store = RegistryStore::open(&dir).unwrap();
    let registry = store.load().unwrap();
    let (scheduler, outbox) = Scheduler::new(
        fake.clone(),
        BucketConfig {
            capacity: 1000,
            refill_every: Duration::from_millis(1),
            min_gap: Duration::ZERO,
        },
    );
    tokio::spawn(scheduler.run());
    let (slots, _view) = Slots::new(registry, store, outbox, options);
    let (agents, agents_rx) = mpsc::channel(16);
    let (hooks, hooks_rx) = mpsc::channel(16);
    let (control, control_rx) = mpsc::unbounded_channel();
    tokio::spawn(slots.run(agents_rx, hooks_rx, control_rx));
    Rig {
        fake,
        agents,
        hooks,
        control,
        dir,
        keep: Vec::new(),
    }
}

fn rig(name: &str) -> Rig {
    rig_in(temp_dir(name), options())
}

fn post(session: &str, cwd: &str, event: HookEvent) -> HookPost {
    HookPost::new(
        HOST.into(),
        session.into(),
        cwd.into(),
        String::new(),
        event,
    )
}

fn start(session: &str, cwd: &str, source: &str, pid: u32, parent: Option<u32>) -> HookPost {
    post(
        session,
        cwd,
        HookEvent::SessionStart {
            source: Some(source.into()),
            claude_pid: Some(pid),
            parent_claude_pid: parent,
        },
    )
}

fn end(session: &str, cwd: &str, reason: &str) -> HookPost {
    end_pid(session, cwd, reason, None)
}

fn end_pid(session: &str, cwd: &str, reason: &str, pid: Option<u32>) -> HookPost {
    post(
        session,
        cwd,
        HookEvent::SessionEnd {
            reason: Some(reason.into()),
            claude_pid: pid,
        },
    )
}

fn sid(tag: char) -> String {
    let t: String = std::iter::repeat_n(tag, 8).collect();
    format!("{t}-0000-4000-8000-00000000000{}", tag as u32 % 10)
}

impl Rig {
    async fn hook(&self, p: HookPost) {
        self.hooks.send(p).await.unwrap();
    }
    async fn agent(&mut self, conn: u64, session: &str, cwd: &str) {
        let (tx, rx) = mpsc::channel(4);
        self.keep.push(rx);
        self.agents
            .send(AgentEvent::Registered {
                conn,
                register: Register {
                    session_id: session.into(),
                    host: HOST.into(),
                    cwd: cwd.into(),
                },
                to_agent: tx,
            })
            .await
            .unwrap();
    }
    async fn agent_msg(&self, conn: u64, msg: AgentMsg) {
        self.agents
            .send(AgentEvent::Message { conn, msg })
            .await
            .unwrap();
    }
    async fn agent_gone(&self, conn: u64) {
        self.agents
            .send(AgentEvent::Disconnected { conn })
            .await
            .unwrap();
    }
    fn ops(&self) -> Vec<Op> {
        self.fake.ops.lock().unwrap().clone()
    }
    /// Waits until no new op appears for 250 ms.
    async fn settle(&self) -> Vec<Op> {
        let mut last = usize::MAX;
        for _ in 0..200 {
            tokio::time::sleep(Duration::from_millis(250)).await;
            let n = self.ops().len();
            if n == last {
                return self.ops();
            }
            last = n;
        }
        panic!("ops never settled");
    }
    fn registry(&self) -> Registry {
        // Only after settle: the save loop writes the latest snapshot.
        let bytes = std::fs::read(self.dir.join("registry.json")).unwrap();
        serde_json::from_slice(&bytes).unwrap()
    }
}

struct Count {
    creates: Vec<String>,
    separators: Vec<(i64, String)>,
    icon_edits: Vec<(i64, String)>,
    name_edits: Vec<(i64, String)>,
    other: Vec<Op>,
}

fn count(ops: &[Op]) -> Count {
    let mut c = Count {
        creates: vec![],
        separators: vec![],
        icon_edits: vec![],
        name_edits: vec![],
        other: vec![],
    };
    for op in ops {
        match op {
            Op::CreateTopic { name, .. } => c.creates.push(name.clone()),
            Op::Send {
                thread_id: Some(t),
                text,
                ..
            } if text.starts_with("── session ") => c.separators.push((*t, text.clone())),
            Op::EditTopic {
                thread_id,
                name,
                icon_custom_emoji_id,
            } => {
                if let Some(i) = icon_custom_emoji_id {
                    c.icon_edits.push((*thread_id, i.clone()));
                }
                if let Some(n) = name {
                    c.name_edits.push((*thread_id, n.clone()));
                }
            }
            other => c.other.push(other.clone()),
        }
    }
    c
}

fn assert_icons_allowed(c: &Count) {
    for (_, icon) in &c.icon_edits {
        assert!(
            [ICON_ALIVE, ICON_DEAD, ICON_WAITING, ICON_NO_CHANNEL].contains(&icon.as_str()),
            "icon {icon} not from the list"
        );
    }
}

const F: &str = r"C:\Work\Project";

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn s1_session_after_death_reuses_topic_one_separator_no_close() {
    let mut r = rig("s1");
    let (a, b) = (sid('a'), sid('b'));
    r.hook(start(&a, F, "startup", 11, None)).await;
    r.settle().await;
    r.agent(1, &a, F).await;
    r.settle().await;
    r.hook(end(&a, F, "prompt_input_exit")).await;
    r.agent_gone(1).await;
    r.settle().await;
    r.hook(start(&b, r"c:/work/project/", "startup", 12, None)).await;
    r.settle().await;
    r.agent(2, &b, F).await;
    let c = count(&r.settle().await);
    assert_eq!(c.creates.len(), 1, "creates {:?}", c.creates);
    assert_eq!(c.separators, vec![(100, "── session bbbbbbbb · new ──".into())]);
    assert!(c.other.is_empty(), "unexpected ops {:?}", c.other);
    assert_icons_allowed(&c);
    // dead icon was set at SessionEnd
    assert!(c.icon_edits.iter().any(|(_, i)| i == ICON_DEAD));
    assert_eq!(c.icon_edits.last().unwrap().1, ICON_ALIVE);
    // title now shows b's short id
    assert!(c.name_edits.last().unwrap().1.contains("bbbbbbbb"));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn s2_three_concurrent_sessions_three_topics() {
    let r = rig("s2");
    for (i, t) in ['a', 'b', 'c'].into_iter().enumerate() {
        r.hook(start(&sid(t), F, "startup", 20 + i as u32, None)).await;
    }
    let c = count(&r.settle().await);
    assert_eq!(c.creates.len(), 3, "{:?}", c.creates);
    assert!(c.creates[0].starts_with("[box] Project ·"), "{}", c.creates[0]);
    assert!(c.creates[1].starts_with("[box] Project #2"), "{}", c.creates[1]);
    assert!(c.creates[2].starts_with("[box] Project #3"), "{}", c.creates[2]);
    assert!(c.separators.is_empty());
}

/// /clear with SessionEnd(reason=clear) first, session in #2 while #1 free.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn s3a_clear_end_first_keeps_slot() {
    let r = rig("s3a");
    let (a, b, c) = (sid('a'), sid('b'), sid('c'));
    r.hook(start(&a, F, "startup", 1, None)).await;
    r.hook(start(&b, F, "startup", 2, None)).await;
    r.settle().await;
    r.hook(end(&a, F, "prompt_input_exit")).await;
    r.hook(end(&b, F, "clear")).await;
    r.hook(start(&c, F, "clear", 2, None)).await;
    let cnt = count(&r.settle().await);
    assert_eq!(cnt.creates.len(), 2);
    assert_eq!(
        cnt.separators,
        vec![(101, "── session cccccccc · new ──".into())],
        "clear must stay in #2 (thread 101)"
    );
    let reg = r.registry();
    assert_eq!(reg.slots[reg.sessions[&c].slot.unwrap().0].ordinal, 2);
}

/// /clear with SessionStart(clear) first, then SessionEnd(clear).
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn s3b_clear_start_first_keeps_slot() {
    let r = rig("s3b");
    let (a, b, c) = (sid('a'), sid('b'), sid('c'));
    r.hook(start(&a, F, "startup", 1, None)).await;
    r.hook(start(&b, F, "startup", 2, None)).await;
    r.settle().await;
    r.hook(end(&a, F, "prompt_input_exit")).await;
    r.hook(start(&c, F, "clear", 2, None)).await;
    r.hook(end(&b, F, "clear")).await;
    let cnt = count(&r.settle().await);
    assert_eq!(cnt.creates.len(), 2);
    assert_eq!(cnt.separators, vec![(101, "── session cccccccc · new ──".into())]);
    // Next startup in the folder takes #1 (free), not #2 (c is alive)
    let d = sid('d');
    r.hook(start(&d, F, "startup", 3, None)).await;
    let cnt = count(&r.settle().await);
    assert_eq!(cnt.creates.len(), 2);
    assert_eq!(cnt.separators.last().unwrap().0, 100);
}

/// Reused pid, normal startup, after a clean SessionEnd (reason=clear or other):
/// must go to the first free slot, not inherit #2.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn s4a_reused_pid_normal_startup_after_clean_end() {
    for reason in ["clear", "prompt_input_exit"] {
        let r = rig("s4a");
        let (a, b, c) = (sid('a'), sid('b'), sid('c'));
        r.hook(start(&a, F, "startup", 1, None)).await;
        r.hook(start(&b, F, "startup", 2, None)).await;
        r.settle().await;
        r.hook(end(&a, F, "prompt_input_exit")).await;
        r.hook(end(&b, F, reason)).await;
        r.hook(start(&c, F, "startup", 2, None)).await;
        let cnt = count(&r.settle().await);
        assert_eq!(
            cnt.separators.last().map(|s| s.0),
            Some(100),
            "reason={reason}: must go to #1"
        );
    }
}

/// Reused pid, normal startup, while the old session's SessionEnd was lost
/// (crash): does the new unrelated session inherit the old slot?
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn s4b_reused_pid_normal_startup_after_lost_end() {
    let r = rig("s4b");
    let (a, b, c) = (sid('a'), sid('b'), sid('c'));
    r.hook(start(&a, F, "startup", 1, None)).await;
    r.hook(start(&b, F, "startup", 2, None)).await;
    r.settle().await;
    r.hook(end(&a, F, "prompt_input_exit")).await;
    // b crashed: no SessionEnd. Windows gives pid 2 to an unrelated startup.
    r.hook(start(&c, F, "startup", 2, None)).await;
    let cnt = count(&r.settle().await);
    assert_eq!(
        cnt.separators.last().map(|s| s.0),
        Some(100),
        "unrelated startup with a reused pid inherited slot #2 (thread {:?})",
        cnt.separators.last()
    );
}

/// Nested start of a known (ended) top-level session keeps its kind and slot.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn s5a_nested_resume_of_known_toplevel() {
    let r = rig("s5a");
    let (a, b) = (sid('a'), sid('b'));
    let f2 = r"C:\Work\Other";
    r.hook(start(&a, F, "startup", 10, None)).await;
    r.hook(start(&b, f2, "startup", 20, None)).await;
    r.settle().await;
    r.hook(end(&b, f2, "prompt_input_exit")).await;
    r.settle().await;
    r.hook(start(&b, f2, "resume", 30, Some(10))).await;
    r.hook(end(&b, f2, "other")).await;
    r.settle().await;
    r.hook(start(&b, f2, "resume", 40, None)).await;
    let cnt = count(&r.settle().await);
    assert_eq!(cnt.creates.len(), 2);
    assert!(cnt.separators.is_empty(), "{:?}", cnt.separators);
    let reg = r.registry();
    assert_eq!(reg.sessions[&b].kind, cctg::hub::registry::SessionKind::TopLevel);
    assert_eq!(reg.slots[reg.sessions[&b].slot.unwrap().0].topic_id, Some(101));
}

/// Nested `claude -p --resume B` while B is still alive interactively: the
/// nested run's SessionEnd must not free B's slot for another session.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn s5b_nested_resume_of_live_toplevel_does_not_free_its_slot() {
    let r = rig("s5b");
    let (a, b, c) = (sid('a'), sid('b'), sid('c'));
    let f2 = r"C:\Work\Other";
    r.hook(start(&a, F, "startup", 10, None)).await;
    r.hook(start(&b, f2, "startup", 20, None)).await;
    r.settle().await;
    // from A's Bash: claude -p --resume B
    r.hook(start(&b, f2, "resume", 30, Some(10))).await;
    r.hook(end_pid(&b, f2, "other", Some(30))).await;
    r.settle().await;
    // B is still alive (pid 20). A new session in f2 must get #2.
    r.hook(start(&c, f2, "startup", 50, None)).await;
    let cnt = count(&r.settle().await);
    assert_eq!(
        cnt.creates.len(),
        3,
        "c took live B's topic: creates={:?} separators={:?}",
        cnt.creates,
        cnt.separators
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn s6_nested_and_subagents_make_no_topics() {
    let r = rig("s6");
    let (a, n, n2, u) = (sid('a'), sid('n'), sid('m'), sid('u'));
    r.hook(start(&a, F, "startup", 10, None)).await;
    r.settle().await;
    let before = count(&r.ops()).creates.len();
    r.hook(start(&n, r"C:\Elsewhere", "startup", 11, Some(10))).await;
    r.hook(start(&n2, r"C:\Elsewhere2", "startup", 12, Some(11))).await;
    r.hook(start(&u, r"C:\Elsewhere3", "startup", 13, Some(999))).await;
    for (t, ty) in [("ag1", "Explore"), ("ag2", "")] {
        r.hook(post(
            &a,
            F,
            HookEvent::SubagentStart {
                agent_id: t.into(),
                agent_type: ty.into(),
            },
        ))
        .await;
        r.hook(post(
            &a,
            F,
            HookEvent::SubagentStop {
                agent_id: t.into(),
                agent_type: ty.into(),
                agent_transcript_path: None,
                last_assistant_message: None,
            },
        ))
        .await;
    }
    r.hook(end(&n, r"C:\Elsewhere", "other")).await;
    r.hook(end(&n2, r"C:\Elsewhere2", "other")).await;
    let cnt = count(&r.settle().await);
    assert_eq!(cnt.creates.len(), before);
    assert!(cnt.separators.is_empty());
    let reg = r.registry();
    let parent_slot = reg.sessions[&a].slot;
    assert_eq!(reg.sessions[&n].slot, parent_slot);
    assert_eq!(reg.sessions[&n2].slot, parent_slot);
    assert_eq!(reg.sessions[&u].slot, None);
    assert_eq!(reg.subagents["ag1"].slot, parent_slot);
    assert!(!reg.subagents.contains_key("ag2"));
    // A still alive (nested SessionEnd must not kill A)
    assert!(!reg.sessions[&a].ended);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn s7_hook_only_then_agent_then_waiting() {
    let mut r = rig("s7");
    let a = sid('a');
    r.hook(start(&a, F, "startup", 10, None)).await;
    let ops = r.settle().await;
    match &ops[0] {
        Op::CreateTopic {
            icon_custom_emoji_id,
            ..
        } => assert_eq!(icon_custom_emoji_id.as_deref(), Some(ICON_NO_CHANNEL)),
        other => panic!("{other:?}"),
    }
    r.agent(1, &a, F).await;
    r.settle().await;
    r.agent_msg(
        1,
        AgentMsg::PermissionRequest(PermissionRequest {
            request_id: "abcde".into(),
            tool_name: "Bash".into(),
            description: "x".into(),
            input_preview: "y".into(),
        }),
    )
    .await;
    r.settle().await;
    r.hook(post(&a, F, HookEvent::Stop {
        prompt_id: None,
        last_assistant_message: None,
    }))
    .await;
    r.settle().await;
    r.agent_gone(1).await;
    let cnt = count(&r.settle().await);
    assert_eq!(cnt.creates.len(), 1);
    let icons: Vec<&str> = cnt.icon_edits.iter().map(|(_, i)| i.as_str()).collect();
    assert_eq!(icons, vec![ICON_ALIVE, ICON_WAITING, ICON_ALIVE, ICON_NO_CHANNEL]);
}

/// Agent registers before its SessionStart hook.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn s7b_agent_before_hook() {
    let mut r = rig("s7b");
    let a = sid('a');
    r.agent(1, &a, F).await;
    let ops = r.settle().await;
    assert!(ops.is_empty());
    r.hook(start(&a, F, "startup", 10, None)).await;
    let cnt = count(&r.settle().await);
    assert_eq!(cnt.creates.len(), 1);
    // created with no-channel then edited to alive, or created alive: either way final alive
    let last_icon = cnt
        .icon_edits
        .last()
        .map(|(_, i)| i.clone())
        .or_else(|| match &r.ops()[0] {
            Op::CreateTopic {
                icon_custom_emoji_id,
                ..
            } => icon_custom_emoji_id.clone(),
            _ => None,
        });
    assert_eq!(last_icon.as_deref(), Some(ICON_ALIVE));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn s8_windows_spellings_one_slot() {
    let r = rig("s8");
    let spellings = [
        r"C:\Work\Project",
        r"c:/work/project/",
        r"\\?\C:\Work\Project",
        r"C:\WORK\PROJECT\",
    ];
    for (i, cwd) in spellings.iter().enumerate() {
        let s = sid(char::from(b'a' + i as u8));
        r.hook(start(&s, cwd, "startup", 100 + i as u32, None)).await;
        r.settle().await;
        r.hook(end(&s, cwd, "prompt_input_exit")).await;
        r.settle().await;
    }
    let cnt = count(&r.settle().await);
    assert_eq!(cnt.creates.len(), 1, "{:?}", cnt.creates);
    assert_eq!(cnt.separators.len(), 3);
    assert!(cnt.creates[0].starts_with("[box] Project"));
    // UNC variants
    let r = rig("s8unc");
    for (i, cwd) in [r"\\Server\Share\Dir", r"\\?\UNC\server\share\dir\"].iter().enumerate() {
        let s = sid(char::from(b'a' + i as u8));
        r.hook(start(&s, cwd, "startup", 100 + i as u32, None)).await;
        r.settle().await;
        r.hook(end(&s, cwd, "other")).await;
        r.settle().await;
    }
    assert_eq!(count(&r.ops()).creates.len(), 1);
    // POSIX is case-sensitive: two slots/topics.
    let r = rig("s8posix");
    for (i, cwd) in ["/home/u/Proj", "/home/u/proj"].iter().enumerate() {
        let s = sid(char::from(b'a' + i as u8));
        r.hook(start(&s, cwd, "startup", 100 + i as u32, None)).await;
        r.settle().await;
        r.hook(end(&s, cwd, "other")).await;
        r.settle().await;
    }
    assert_eq!(count(&r.ops()).creates.len(), 2);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn s9_compact_and_resume() {
    let r = rig("s9");
    let (a, b) = (sid('a'), sid('b'));
    r.hook(start(&a, F, "startup", 10, None)).await;
    r.settle().await;
    r.hook(start(&a, F, "compact", 10, None)).await;
    r.settle().await;
    assert!(count(&r.ops()).separators.is_empty(), "compact made a separator");
    r.hook(end(&a, F, "other")).await;
    r.hook(start(&b, F, "startup", 11, None)).await;
    r.hook(end(&b, F, "other")).await;
    r.hook(start(&a, F, "resume", 12, None)).await;
    let cnt = count(&r.settle().await);
    assert_eq!(cnt.creates.len(), 1);
    let texts: Vec<&str> = cnt.separators.iter().map(|(_, t)| t.as_str()).collect();
    // b's separator may be superseded if not yet sent; a's must be "resumed"
    assert_eq!(*texts.last().unwrap(), "── session aaaaaaaa · resumed ──");
}

/// Hub restart: registry persists, next session reuses the topic.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn s10_restart_keeps_topics() {
    let dir = temp_dir("s10");
    let (a, b, c) = (sid('a'), sid('b'), sid('c'));
    {
        let r = rig_in(dir.clone(), options());
        r.hook(start(&a, F, "startup", 10, None)).await;
        r.hook(start(&b, F, "startup", 11, None)).await;
        r.settle().await;
        r.hook(end(&a, F, "other")).await;
        r.settle().await;
        tokio::time::sleep(Duration::from_millis(300)).await;
    }
    let r = rig_in(dir.clone(), options());
    r.hook(start(&c, F, "startup", 12, None)).await;
    let cnt = count(&r.settle().await);
    assert!(cnt.creates.is_empty(), "restart created {:?}", cnt.creates);
    assert_eq!(cnt.separators, vec![(100, "── session cccccccc · new ──".into())]);
    // b (alive, no agent) must not be shown dead after restart
    assert!(!cnt.icon_edits.iter().any(|(t, i)| *t == 101 && i == ICON_DEAD));
}

/// forum_topic_edited in a slot topic is deleted; in an unknown topic not.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn s11_service_message_delete() {
    let r = rig("s11");
    r.hook(start(&sid('a'), F, "startup", 10, None)).await;
    r.settle().await;
    r.control
        .send(Control::TopicEdited {
            thread_id: Some(100),
            message_id: 55,
        })
        .unwrap();
    r.control
        .send(Control::TopicEdited {
            thread_id: Some(999),
            message_id: 56,
        })
        .unwrap();
    r.control
        .send(Control::TopicEdited {
            thread_id: None,
            message_id: 57,
        })
        .unwrap();
    let ops = r.settle().await;
    let deletes: Vec<i64> = ops
        .iter()
        .filter_map(|o| match o {
            Op::Delete { message_id } => Some(*message_id),
            _ => None,
        })
        .collect();
    assert_eq!(deletes, vec![55]);
}

/// TOPIC_ID_INVALID on a separator: exactly one replacement topic.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn s12_topic_gone_one_replacement() {
    let mut r = rig("s12");
    let (a, b) = (sid('a'), sid('b'));
    r.hook(start(&a, F, "startup", 10, None)).await;
    r.settle().await;
    r.agent(1, &a, F).await;
    r.settle().await;
    r.hook(end(&a, F, "other")).await;
    r.settle().await;
    *r.fake.edit_errors.lock().unwrap() = vec!["Bad Request: TOPIC_ID_INVALID"];
    r.hook(start(&b, F, "startup", 11, None)).await;
    r.settle().await;
    r.hook(end(&b, F, "other")).await;
    let cnt = count(&r.settle().await);
    assert_eq!(cnt.creates.len(), 2, "{:?}", cnt.creates);
    let _ = &mut r;
}

/// Many sessions in many folders plus churn: topics == max parallelism per folder.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn s13_churn_topics_equal_max_parallelism() {
    let r = rig("s13");
    let folders = [r"C:\P1", r"C:\P2", r"C:\P3"];
    let mut pid = 1000;
    let mut n = 0u32;
    let mut mk = || {
        n += 1;
        format!("{:08x}-0000-4000-8000-{:012x}", n, n)
    };
    for round in 0..5 {
        let mut live = vec![];
        for f in folders {
            for _ in 0..(1 + round % 3) {
                let s = mk();
                pid += 1;
                r.hook(start(&s, f, "startup", pid, None)).await;
                live.push((s, f));
            }
        }
        r.settle().await;
        for (s, f) in live {
            r.hook(end(&s, f, "other")).await;
        }
        r.settle().await;
    }
    let cnt = count(&r.settle().await);
    assert_eq!(cnt.creates.len(), 9, "{:?}", cnt.creates);
    assert_icons_allowed(&cnt);
}

/// Nested `claude -p --resume A` from A's own Bash (parent pid == A's pid),
/// then A keeps working (Stop). Is A's topic freed for another session?
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn s5c_nested_self_resume_end_frees_live_parent_slot() {
    let mut r = rig("s5c");
    let (a, c) = (sid('a'), sid('c'));
    r.hook(start(&a, F, "startup", 10, None)).await;
    r.settle().await;
    r.agent(1, &a, F).await;
    r.settle().await;
    r.hook(start(&a, F, "resume", 30, Some(10))).await;
    r.hook(end_pid(&a, F, "other", Some(30))).await;
    r.hook(post(&a, F, HookEvent::Stop { prompt_id: None, last_assistant_message: None })).await;
    let mid = count(&r.settle().await);
    let dead_while_alive = mid.icon_edits.iter().any(|(_, i)| i == ICON_DEAD);
    r.hook(start(&c, F, "startup", 50, None)).await;
    let cnt = count(&r.settle().await);
    assert!(
        !dead_while_alive && cnt.creates.len() == 2,
        "dead_while_alive={dead_while_alive} creates={:?} separators={:?}",
        cnt.creates,
        cnt.separators
    );
}
