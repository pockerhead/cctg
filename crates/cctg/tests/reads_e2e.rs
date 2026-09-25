//! Session reads end to end (TASK-034): the hub never opens a file of the
//! session's machine. The real `cctg agent` binary (its own process, home
//! and `CLAUDE_CONFIG_DIR` in a temp dir, cwd `<root>/w`) is linked over real
//! TCP to the real `serve_agents`, `Slots`, `Scheduler` and command worker,
//! with a fake Telegram transport. The hooks name the transcript and the
//! subagent files by paths relative to the agent's cwd: the agent resolves
//! them, while the hub, in this test process with another cwd, cannot open
//! them at all. `/brief`, `/full`, the ai-title and the subagent block still
//! work; an agent built before the reads, a session without an agent, an
//! ended one and a read whose agent goes away get notices. The agent serves
//! only its own project folder (decision 12), found by its session's
//! transcript or named after its cwd, and never opens a path the hub sends.

use std::io::{BufRead, BufReader, Write};
use std::net::TcpStream;
use std::path::{Path, PathBuf};
use std::process::{Child, Stdio};
use std::sync::atomic::{AtomicI64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use cctg::device::canonical_cwd;
use cctg::hub::api::{ForumTopic, Message};
use cctg::hub::commands::{self, Asks};
use cctg::hub::ingress::serve_agents;
use cctg::hub::registry::RegistryStore;
use cctg::hub::scheduler::{BucketConfig, Delivery, Op, Outcome, Scheduler, Transport};
use cctg::hub::slots::{Options, Slots};
use cctg::hub::updates::Inbound;
use cctg::wire::{HookEvent, HookPost, Secret};
use serde_json::{Value, json};
use tokio::net::TcpListener;
use tokio::sync::mpsc;
use tokio::task::JoinHandle;

mod common;

const SECRET: &str = "e2e-secret-0123456789abcdef";
const HOST: &str = "e2ebox";
const THREAD: i64 = 100;
const AGENT: &str = "a0000000000000002";
const PARENT: &str = include_str!("../../transcript/tests/fixtures/final_answer.jsonl");
const SUBAGENT: &str = include_str!("../../transcript/tests/fixtures/subagent_handback.jsonl");
const META: &str = include_str!("../../transcript/tests/fixtures/subagent_handback.meta.json");

#[derive(Default)]
struct Fake {
    ops: Mutex<Vec<Op>>,
    next_id: AtomicI64,
}

impl Transport for Fake {
    async fn execute(&self, op: &Op) -> Delivery {
        self.ops.lock().unwrap().push(op.clone());
        let id = 1000 + self.next_id.fetch_add(1, Ordering::SeqCst);
        match op {
            Op::CreateTopic { name, .. } => Ok(Outcome::Topic(ForumTopic {
                message_thread_id: THREAD,
                name: name.clone(),
                icon_custom_emoji_id: None,
            })),
            Op::Send { .. } | Op::SendDocument { .. } | Op::Stream { .. } => {
                Ok(Outcome::Sent(Message {
                    message_id: id,
                    ..Message::default()
                }))
            }
            _ => Ok(Outcome::Done),
        }
    }
}

impl Fake {
    fn ops(&self) -> Vec<Op> {
        self.ops.lock().unwrap().clone()
    }

    /// Plain sends (not stream lines), in order.
    fn sends(&self) -> Vec<String> {
        self.ops()
            .into_iter()
            .filter_map(|op| match op {
                Op::Send { text, .. } => Some(text),
                _ => None,
            })
            .collect()
    }

    /// What each block or message shows in the end (edits replace).
    fn edits(&self) -> Vec<String> {
        self.ops()
            .into_iter()
            .filter_map(|op| match op {
                Op::Edit { text, .. } => Some(text),
                _ => None,
            })
            .collect()
    }

    fn names(&self) -> Vec<String> {
        self.ops()
            .into_iter()
            .filter_map(|op| match op {
                Op::EditTopic {
                    name: Some(name), ..
                }
                | Op::CreateTopic { name, .. } => Some(name),
                _ => None,
            })
            .collect()
    }
}

struct Hub {
    fake: Arc<Fake>,
    hooks: mpsc::Sender<HookPost>,
    commands: mpsc::UnboundedSender<Inbound>,
    _control: mpsc::UnboundedSender<cctg::hub::slots::Control>,
    tasks: Vec<JoinHandle<()>>,
}

impl Drop for Hub {
    fn drop(&mut self) {
        for task in &self.tasks {
            task.abort();
        }
    }
}

async fn start_hub(state: &Path, listener: TcpListener, read_wait: Duration) -> Hub {
    let fake = Arc::new(Fake::default());
    let bucket = BucketConfig {
        capacity: 1000,
        refill_every: Duration::from_millis(1),
        min_gap: Duration::ZERO,
    };
    let (scheduler, outbox) = Scheduler::new(fake.clone(), bucket);
    let store = RegistryStore::open(state).expect("store");
    let registry = store.load().expect("load registry");
    let options = Options {
        grace: Duration::ZERO,
        correlate_for: Duration::from_secs(5),
        recheck_after: Duration::from_millis(50),
        stream_every: Duration::from_millis(50),
        read_wait,
        ..Options::default()
    };
    let mut slots = Slots::new(registry, store, outbox.clone(), options);
    let asks = slots.transcript_asks();
    let (agents, agents_rx) = mpsc::channel(64);
    let (hooks, hooks_rx) = mpsc::channel(64);
    let (control, control_rx) = mpsc::unbounded_channel();
    let (commands, commands_rx) = mpsc::unbounded_channel();
    let tasks = vec![
        tokio::spawn(scheduler.run()),
        tokio::spawn(slots.run(agents_rx, hooks_rx, control_rx)),
        tokio::spawn(serve_agents(
            listener,
            Secret::parse(SECRET).expect("secret"),
            agents,
        )),
        tokio::spawn(commands::serve(
            commands_rx,
            outbox,
            Arc::new(Asks(asks)),
            None,
        )),
    ];
    Hub {
        fake,
        hooks,
        commands,
        _control: control,
        tasks,
    }
}

impl Hub {
    async fn hook(&self, post: HookPost) {
        self.hooks.send(post).await.unwrap();
    }

    fn command(&self, thread: Option<i64>, text: &str) {
        self.commands
            .send(Inbound {
                message_id: 1,
                thread_id: thread,
                text: Some(text.to_owned()),
                reply_to: None,
                quote: None,
                forwarded: false,
                media: None,
            })
            .unwrap();
    }
}

/// One session's folders: `<root>/cfg` is its `CLAUDE_CONFIG_DIR`,
/// `<root>/w` its cwd.
struct Session {
    root: PathBuf,
    id: String,
    project: PathBuf,
    workdir: PathBuf,
    config: PathBuf,
    home: PathBuf,
    state: PathBuf,
}

impl Drop for Session {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.root);
    }
}

fn session(name: &str, n: u32) -> Session {
    let root = std::env::temp_dir().join(format!("cctg-reads-e2e-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    let config = root.join("cfg");
    let project = config.join("projects").join("C--qa-w");
    let (workdir, home, state) = (root.join("w"), root.join("home"), root.join("state"));
    for dir in [&project, &workdir, &home, &state] {
        std::fs::create_dir_all(dir).unwrap();
    }
    Session {
        id: format!("0a16e2e0-0000-4000-8000-{n:012}"),
        root,
        project,
        workdir,
        config,
        home,
        state,
    }
}

impl Session {
    /// The transcript, as the agent (cwd `<root>/w`) finds it.
    fn transcript(&self) -> String {
        format!("../cfg/projects/C--qa-w/{}.jsonl", self.id)
    }

    fn subagent_file(&self) -> String {
        format!(
            "../cfg/projects/C--qa-w/{}/subagents/agent-{AGENT}.jsonl",
            self.id
        )
    }

    /// Writes `jsonl` as the transcript and the subagent's files; checks
    /// that the hub side cannot see them by the paths the hooks give.
    fn write(&self, jsonl: &str) {
        std::fs::write(self.project.join(format!("{}.jsonl", self.id)), jsonl).unwrap();
        let subagents = self.project.join(&self.id).join("subagents");
        std::fs::create_dir_all(&subagents).unwrap();
        std::fs::write(subagents.join(format!("agent-{AGENT}.jsonl")), SUBAGENT).unwrap();
        std::fs::write(subagents.join(format!("agent-{AGENT}.meta.json")), META).unwrap();
        for path in [self.transcript(), self.subagent_file()] {
            assert!(self.workdir.join(&path).is_file(), "the agent sees {path}");
            assert!(!Path::new(&path).exists(), "the hub must not see {path}");
        }
    }

    fn post(&self, event: HookEvent) -> HookPost {
        HookPost::new(
            HOST.into(),
            self.id.clone(),
            canonical_cwd(&self.workdir.to_string_lossy()),
            self.transcript(),
            event,
        )
    }

    fn start(&self, pid: u32) -> HookPost {
        self.post(HookEvent::SessionStart {
            source: Some("startup".into()),
            claude_pid: Some(pid),
            parent_claude_pid: None,
        })
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
    let mut child = common::cctg(&s.home)
        .arg("agent")
        .current_dir(&s.workdir)
        .env("CCTG_HUB_SECRET", SECRET)
        .env("CCTG_HUB_AGENT_ADDR", format!("127.0.0.1:{port}"))
        .env("CCTG_HOST", HOST)
        .env("CLAUDE_CODE_SESSION_ID", &s.id)
        .env("CLAUDE_CONFIG_DIR", &s.config)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .expect("spawn cctg agent");
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

/// An agent speaking the link by hand: `register` is sent as given (an
/// agent built before TASK-034 leaves `session_reads` out); every line the
/// hub sends it is kept. Dropping it closes the link.
struct RawAgent {
    stream: TcpStream,
    lines: Arc<Mutex<Vec<Value>>>,
}

impl RawAgent {
    fn connect(port: u16, register: Value) -> Self {
        let stream = TcpStream::connect(("127.0.0.1", port)).unwrap();
        let mut write = stream.try_clone().unwrap();
        let hello = json!({"v": 1, "type": "hello", "secret": SECRET});
        writeln!(write, "{hello}\n{register}").unwrap();
        let lines = Arc::new(Mutex::new(Vec::new()));
        let kept = lines.clone();
        let read = stream.try_clone().unwrap();
        std::thread::spawn(move || {
            for line in BufReader::new(read).lines() {
                let Ok(line) = line else { break };
                if let Ok(value) = serde_json::from_str(&line) {
                    kept.lock().unwrap().push(value);
                }
            }
        });
        Self { stream, lines }
    }

    fn kinds(&self) -> Vec<String> {
        self.lines
            .lock()
            .unwrap()
            .iter()
            .filter_map(|line| line["type"].as_str().map(str::to_owned))
            .collect()
    }
}

impl Drop for RawAgent {
    fn drop(&mut self) {
        let _ = self.stream.shutdown(std::net::Shutdown::Both);
    }
}

fn register(s: &Session, session_reads: bool) -> Value {
    let mut register = json!({"v": 1, "type": "register", "session_id": s.id, "host": HOST,
        "cwd": canonical_cwd(&s.workdir.to_string_lossy()), "claude_pid": 4242});
    if session_reads {
        register["session_reads"] = json!(true);
    }
    register
}

async fn wait_for(what: &str, secs: u64, mut cond: impl FnMut() -> bool) {
    let deadline = Instant::now() + Duration::from_secs(secs);
    while !cond() {
        assert!(Instant::now() < deadline, "timed out waiting for: {what}");
        tokio::time::sleep(Duration::from_millis(25)).await;
    }
}

async fn listener() -> (TcpListener, u16) {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    (listener, port)
}

fn title_line(title: &str) -> String {
    format!("{}\n", json!({"type": "ai-title", "aiTitle": title}))
}

fn render(jsonl: &str, full: bool, prompts: usize) -> String {
    let turns = transcript::parse(jsonl);
    let slice = transcript::last_prompts(&turns, prompts);
    if full {
        transcript::render_full(slice)
    } else {
        transcript::render_brief(slice)
    }
}

fn subagent_stop(s: &Session, last: &str) -> HookPost {
    s.post(HookEvent::SubagentStop {
        agent_id: AGENT.into(),
        agent_type: "Explore".into(),
        agent_transcript_path: Some(s.subagent_file()),
        last_assistant_message: Some(last.into()),
    })
}

fn subagent_start(s: &Session) -> HookPost {
    s.post(HookEvent::SubagentStart {
        agent_id: AGENT.into(),
        agent_type: "Explore".into(),
    })
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn brief_title_and_blocks_come_from_the_agent_when_the_hub_cannot_see_the_files() {
    let s = session("agent", 1);
    let jsonl = format!("{PARENT}{}", title_line("Reads over the link"));
    s.write(&jsonl);
    let (listener, port) = listener().await;
    let hub = start_hub(&s.state, listener, Duration::from_secs(20)).await;
    let _agent = start_agent(&s, port);
    hub.hook(s.start(4242)).await;
    let fake = hub.fake.clone();
    wait_for("topic", 20, || !fake.names().is_empty()).await;

    // The ai-title: asked on each prompt hook once the agent is bound, read
    // by the agent.
    let titled = || {
        fake.names()
            .iter()
            .any(|name| name.ends_with("· Reads over the link"))
    };
    for _ in 0..80 {
        if titled() {
            break;
        }
        hub.hook(s.post(HookEvent::UserPromptSubmit { prompt_id: None }))
            .await;
        tokio::time::sleep(Duration::from_millis(250)).await;
    }
    assert!(titled(), "{:?}", fake.names());

    // The subagent block: the agent finds the `Agent` call in the parent
    // transcript and renders the finished subagent from its files.
    hub.hook(subagent_start(&s)).await;
    hub.hook(subagent_stop(&s, "Report handed back.")).await;
    let block = format!(
        "↳ Explore {AGENT}: Explore crate\n• Bash: List source files\n• SubagentHandback\nReport handed back."
    );
    wait_for("block", 20, || {
        fake.edits().contains(&block) || fake.sends().contains(&block)
    })
    .await;

    // `/brief` and `/full` in the topic: exactly the agent's rendering.
    hub.command(Some(THREAD), "/brief");
    hub.command(None, "/full 1 0a16");
    let (brief, full) = (render(&jsonl, false, 3), render(&jsonl, true, 1));
    wait_for("brief and full", 20, || {
        let sends = fake.sends();
        sends.contains(&brief) && sends.contains(&full)
    })
    .await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn old_agents_missing_agents_and_ended_sessions_get_notices() {
    let s = session("old", 2);
    s.write(PARENT);
    let (listener, port) = listener().await;
    let hub = start_hub(&s.state, listener, Duration::from_secs(20)).await;
    let fake = hub.fake.clone();
    // No agent at all first.
    hub.hook(s.start(4242)).await;
    wait_for("topic", 20, || !fake.names().is_empty()).await;
    hub.command(Some(THREAD), "/brief");
    wait_for("no agent notice", 20, || {
        fake.sends()
            .iter()
            .any(|text| text.contains("нет связи с агентом"))
    })
    .await;
    // An agent built before TASK-034: never asked to read.
    let old = RawAgent::connect(port, register(&s, false));
    wait_for("registered", 20, || {
        old.kinds().contains(&"registered".to_owned())
    })
    .await;
    hub.command(Some(THREAD), "/brief");
    wait_for("old agent notice", 20, || {
        fake.sends()
            .iter()
            .any(|text| text.contains("старой версии"))
    })
    .await;
    // Its subagent's block comes from the stop alone.
    hub.hook(subagent_start(&s)).await;
    hub.hook(subagent_stop(&s, "Done without files.")).await;
    let block = format!("↳ Explore {AGENT}\nDone without files.");
    wait_for("block from the stop", 20, || fake.sends().contains(&block)).await;
    hub.hook(s.post(HookEvent::UserPromptSubmit { prompt_id: None }))
        .await;
    tokio::time::sleep(Duration::from_millis(300)).await;
    assert!(
        !old.kinds().iter().any(|kind| kind == "session_read"),
        "{:?}",
        old.kinds()
    );
    // The session ends: its transcript is out of reach.
    hub.hook(s.post(HookEvent::SessionEnd {
        reason: None,
        claude_pid: Some(4242),
    }))
    .await;
    hub.command(Some(THREAD), "/brief");
    let resume = format!("claude --resume {}", s.id);
    wait_for("ended notice", 20, || {
        fake.sends().iter().any(|text| text.ends_with(&resume))
    })
    .await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_read_the_agent_does_not_answer_gets_a_notice() {
    let s = session("silent", 3);
    s.write(PARENT);
    let (listener, port) = listener().await;
    let hub = start_hub(&s.state, listener, Duration::from_millis(500)).await;
    let fake = hub.fake.clone();
    let silent = RawAgent::connect(port, register(&s, true));
    wait_for("registered", 20, || {
        silent.kinds().contains(&"registered".to_owned())
    })
    .await;
    hub.hook(s.start(4242)).await;
    wait_for("topic", 20, || !fake.names().is_empty()).await;
    hub.command(Some(THREAD), "/brief");
    wait_for("asked", 20, || {
        silent.kinds().contains(&"session_read".to_owned())
    })
    .await;
    wait_for("no answer notice", 20, || {
        fake.sends().iter().any(|text| text.contains("не ответил"))
    })
    .await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_read_whose_agent_goes_away_gets_a_notice_at_once() {
    let s = session("gone", 4);
    s.write(PARENT);
    let (listener, port) = listener().await;
    // Far longer than the test: the notice comes from the closed link.
    let hub = start_hub(&s.state, listener, Duration::from_secs(600)).await;
    let fake = hub.fake.clone();
    let leaving = RawAgent::connect(port, register(&s, true));
    wait_for("registered", 20, || {
        leaving.kinds().contains(&"registered".to_owned())
    })
    .await;
    hub.hook(s.start(4242)).await;
    wait_for("topic", 20, || !fake.names().is_empty()).await;
    hub.command(Some(THREAD), "/brief");
    wait_for("asked", 20, || {
        leaving.kinds().contains(&"session_read".to_owned())
    })
    .await;
    // The link closes before an answer (a worker swap, TASK-040).
    drop(leaving);
    wait_for("link lost notice", 20, || {
        fake.sends().iter().any(|text| text.contains("прервалась"))
    })
    .await;
}

/// A hook post of `s` that names `path` as its transcript.
fn post_naming(s: &Session, path: &str, event: HookEvent) -> HookPost {
    HookPost::new(
        HOST.into(),
        s.id.clone(),
        canonical_cwd(&s.workdir.to_string_lossy()),
        path.into(),
        event,
    )
}

/// Sends `/brief` to the topic until a send contains `wanted`.
async fn brief_until(hub: &Hub, what: &str, wanted: &str) {
    let deadline = Instant::now() + Duration::from_secs(20);
    loop {
        hub.command(Some(THREAD), "/brief");
        tokio::time::sleep(Duration::from_millis(300)).await;
        if hub.fake.sends().iter().any(|text| text.contains(wanted)) {
            return;
        }
        assert!(
            Instant::now() < deadline,
            "timed out waiting for: {what}: {:?}",
            hub.fake.sends()
        );
    }
}

/// TASK-034 decision 12: the agent serves only its own project folder. For
/// a new session no transcript exists yet when the agent starts, so the
/// folder comes from its cwd the way Claude Code names it; the transcript
/// written there afterwards is served.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_new_sessions_folder_comes_from_the_cwd_and_is_served() {
    let s = session("own", 5);
    let folder = cctg::tail::project_folder_name(&s.workdir.to_string_lossy());
    let path = format!("../cfg/projects/{folder}/{}.jsonl", s.id);
    let (listener, port) = listener().await;
    let hub = start_hub(&s.state, listener, Duration::from_secs(20)).await;
    let _agent = start_agent(&s, port);
    hub.hook(post_naming(
        &s,
        &path,
        HookEvent::SessionStart {
            source: Some("startup".into()),
            claude_pid: Some(4242),
            parent_claude_pid: None,
        },
    ))
    .await;
    // The agent answers before the transcript exists: not found.
    brief_until(&hub, "the agent's first answer", "не найден").await;
    let own = s.config.join("projects").join(&folder);
    std::fs::create_dir_all(&own).unwrap();
    std::fs::write(own.join(format!("{}.jsonl", s.id)), PARENT).unwrap();
    assert!(!Path::new(&path).exists(), "the hub must not see {path}");
    brief_until(&hub, "the brief", &render(PARENT, false, 3)).await;
}

/// TASK-034 decision 12: the agent builds its paths itself. The hook names
/// a file of another project folder for the session; the hub passes that
/// path on, and the agent still serves the transcript in its own folder.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_path_the_hub_names_is_never_opened() {
    let s = session("foreign", 6);
    let folder = cctg::tail::project_folder_name(&s.workdir.to_string_lossy());
    let own = s.config.join("projects").join(&folder);
    std::fs::create_dir_all(&own).unwrap();
    std::fs::write(own.join(format!("{}.jsonl", s.id)), PARENT).unwrap();
    let private =
        "{\"type\":\"user\",\"message\":{\"role\":\"user\",\"content\":\"private prompt\"}}\n";
    let foreign = s.config.join("projects").join("C--another-project");
    std::fs::create_dir_all(&foreign).unwrap();
    std::fs::write(foreign.join(format!("{}.jsonl", s.id)), private).unwrap();
    let path = format!("../cfg/projects/C--another-project/{}.jsonl", s.id);
    let (listener, port) = listener().await;
    let hub = start_hub(&s.state, listener, Duration::from_secs(20)).await;
    let _agent = start_agent(&s, port);
    hub.hook(post_naming(
        &s,
        &path,
        HookEvent::SessionStart {
            source: Some("startup".into()),
            claude_pid: Some(4242),
            parent_claude_pid: None,
        },
    ))
    .await;
    brief_until(&hub, "the own brief", &render(PARENT, false, 3)).await;
    assert!(
        !hub.fake
            .sends()
            .iter()
            .any(|text| text.contains("private prompt")),
        "{:?}",
        hub.fake.sends()
    );
}
