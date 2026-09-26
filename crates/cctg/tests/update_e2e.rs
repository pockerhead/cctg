//! `cctg agent` hands over to a newer binary without Claude Code noticing
//! (TASK-040), with real processes: a copy of the built `cctg` in a temp bin
//! directory runs as the shim, the hub end is the real `serve_agents` over
//! TCP and this test plays the slots actor and Claude Code.
//!
//! The copy is replaced by a different build (the same bytes plus a tail,
//! which Windows still runs, or with the baked commit rewritten when the
//! test was built from a clean one) while its worker runs; `update` makes the worker
//! write the switch marker, answer every line Claude Code sent meanwhile,
//! leave (`update_answer reloading`), take what was queued before `released`
//! and exit; the shim starts the new file, whose worker registers with the
//! new build and needs no second `initialize`; it asks Claude Code to list
//! the tools again, and its `send_file` works through the shim (TASK-032).
//! The real `~/.cctg` is never touched: home, state and config are temp dirs.
//!
//! TASK-050: an `update` that names the hub's release makes the worker
//! download that release's binary for its platform from a fake release on
//! loopback HTTP (`CCTG_RELEASE_BASE_URL`), check it against `SHA256SUMS`,
//! put it in place of the shim's file and hand over to it; a bad checksum,
//! a missing file and a cut connection leave the old file and the agent,
//! and then a newer file put in place otherwise is still taken.

use std::io::{BufRead, BufReader, Write};
use std::net::{Ipv4Addr, SocketAddr, TcpListener};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use cctg::hub::ingress::{self, AgentEvent};
use cctg::wire::{AgentMsg, FileOutcome, HubMsg, Register, Secret, UpdateOutcome};
use serde_json::Value;
use tokio::sync::mpsc;

mod common;

const SECRET: &str = "update-e2e-secret-0123456789";
const SESSION: &str = "5e550000-0000-4000-8000-000000000040";
const WAIT: Duration = Duration::from_secs(30);
const EXE: &str = std::env::consts::EXE_SUFFIX;

struct Shim(Child);
impl Drop for Shim {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

struct Root(std::path::PathBuf);
impl Drop for Root {
    fn drop(&mut self) {
        // The replaced binary may still be held for a moment.
        for _ in 0..20 {
            if std::fs::remove_dir_all(&self.0).is_ok() {
                return;
            }
            std::thread::sleep(Duration::from_millis(100));
        }
    }
}

async fn registered(
    events: &mut mpsc::Receiver<AgentEvent>,
) -> (u64, Register, mpsc::Sender<HubMsg>) {
    loop {
        match tokio::time::timeout(WAIT, events.recv())
            .await
            .expect("a registration in time")
        {
            Some(AgentEvent::Registered {
                conn,
                register,
                to_agent,
            }) => return (conn, register, to_agent),
            Some(_) => continue,
            None => panic!("ingress stopped"),
        }
    }
}

async fn message(events: &mut mpsc::Receiver<AgentEvent>, from: u64) -> AgentMsg {
    loop {
        match tokio::time::timeout(WAIT, events.recv())
            .await
            .expect("a message in time")
        {
            Some(AgentEvent::Message { conn, msg, .. }) if conn == from => return msg,
            Some(_) => continue,
            None => panic!("ingress stopped"),
        }
    }
}

async fn wait_for(what: &str, check: impl Fn() -> bool) {
    let deadline = Instant::now() + WAIT;
    while !check() {
        assert!(Instant::now() < deadline, "waited too long for {what}");
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
}

fn send(claude: &mut std::process::ChildStdin, line: &str) {
    claude.write_all(line.as_bytes()).unwrap();
    claude
        .write_all(
            b"
",
        )
        .unwrap();
    claude.flush().unwrap();
}

/// The build a worker started from `path` reports: the commit when this
/// test was built from a clean one, else from the file's hash.
fn build_of(path: &Path) -> String {
    cctg::client::build_id_of(path).unwrap()
}

/// Writes to `path` a build other than `original` and returns the build its
/// worker reports. Built from a clean commit, every copy reports that
/// commit, so the copy gets it rewritten (same length, other characters);
/// otherwise the build comes from the file hash, which a tail changes.
fn write_newer(path: &Path, original: &[u8]) -> String {
    let source = cctg::client::SOURCE;
    if source.is_empty() || source.ends_with("-dirty") {
        let mut newer = original.to_vec();
        newer.extend_from_slice(b"\0update-e2e newer build");
        common::write_program(path, &newer);
        return build_of(path);
    }
    assert!(
        source.len() >= 16,
        "a build id this short cannot be rewritten"
    );
    let other: String = source
        .chars()
        .map(|c| match c {
            '0'..='8' | 'a'..='y' | 'A'..='Y' => char::from(c as u8 + 1),
            '9' => '0',
            'z' => 'a',
            'Z' => 'A',
            c => c,
        })
        .collect();
    assert_ne!(other, source);
    let mut newer = original.to_vec();
    let mut rewritten = 0;
    let mut at = 0;
    while let Some(found) = newer[at..]
        .windows(source.len())
        .position(|window| window == source.as_bytes())
    {
        let start = at + found;
        newer[start..start + source.len()].copy_from_slice(other.as_bytes());
        rewritten += 1;
        at = start + source.len();
    }
    assert!(rewritten > 0, "the build id is in the binary");
    common::write_program(path, &newer);
    // The changed bytes void the linker's ad-hoc signature (Apple Silicon).
    #[cfg(target_os = "macos")]
    {
        let status = Command::new("codesign")
            .args(["--force", "--sign", "-"])
            .arg(path)
            .status()
            .expect("codesign runs");
        assert!(status.success(), "codesign");
    }
    other
}

#[tokio::test]
async fn a_new_binary_is_taken_without_losing_a_line() {
    let root = Root(std::env::temp_dir().join(format!("cctg-update-e2e-{}", std::process::id())));
    let _ = std::fs::remove_dir_all(&root.0);
    let [bin, home, work] = ["bin", "home", "work"].map(|name| root.0.join(name));
    for dir in [&bin, &home, &work] {
        std::fs::create_dir_all(dir).unwrap();
    }
    let exe = bin.join(format!("cctg{EXE}"));
    let original = std::fs::read(env!("CARGO_BIN_EXE_cctg")).unwrap();
    common::write_program(&exe, &original);

    let listener = ingress::bind(SocketAddr::from((Ipv4Addr::LOCALHOST, 0)))
        .await
        .unwrap();
    let port = listener.local_addr().unwrap().port();
    let (events_tx, mut events) = mpsc::channel(64);
    tokio::spawn(ingress::serve_agents(
        listener,
        Secret::parse(SECRET).unwrap(),
        events_tx,
    ));

    let mut command = Command::new(&exe);
    common::isolate(&mut command, &home);
    let mut child = command
        .arg("agent")
        .current_dir(&work)
        .env("CCTG_HUB_SECRET", SECRET)
        .env("CCTG_HUB_AGENT_ADDR", format!("127.0.0.1:{port}"))
        .env("CCTG_HOST", "box")
        .env("CCTG_STATE_DIR", root.0.join("state"))
        .env("CLAUDE_CODE_SESSION_ID", SESSION)
        .env("CLAUDE_CONFIG_DIR", home.join("claude"))
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .expect("cctg agent starts");
    let mut claude = child.stdin.take().unwrap();
    let out = Arc::new(Mutex::new(Vec::<String>::new()));
    {
        let out = out.clone();
        let stdout = child.stdout.take().unwrap();
        std::thread::spawn(move || {
            for line in BufReader::new(stdout).lines() {
                let Ok(line) = line else { break };
                out.lock().unwrap().push(line);
            }
        });
    }
    let shim = Shim(child);
    send(
        &mut claude,
        r#"{"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"2025-11-25"}}"#,
    );
    send(
        &mut claude,
        r#"{"jsonrpc":"2.0","method":"notifications/initialized"}"#,
    );

    let (first, register, to_first) = registered(&mut events).await;
    let client = register.client.expect("a client");
    assert!(client.self_update, "under the shim");
    let first_build = build_of(&exe);
    assert_eq!(client.build, first_build);

    // A different build takes the file's place; the old one keeps running.
    std::fs::rename(&exe, bin.join(format!("cctg.old{EXE}"))).unwrap();
    let newer_build = write_newer(&exe, &original);
    assert_ne!(newer_build, first_build, "the two files are two builds");

    to_first
        .send(HubMsg::Update {
            update_id: 7,
            release: None,
        })
        .await
        .unwrap();
    // Claude Code keeps talking while the worker changes: a reply and a
    // permission prompt in flight, then pings.
    send(
        &mut claude,
        r#"{"jsonrpc":"2.0","id":107,"method":"tools/call","params":{"name":"reply","arguments":{"text":"in-flight reply"}}}"#,
    );
    send(
        &mut claude,
        r#"{"jsonrpc":"2.0","method":"notifications/claude/channel/permission_request","params":{"request_id":"qwert","tool_name":"Bash","description":"in flight","input_preview":"echo"}}"#,
    );
    for id in 100..106 {
        send(
            &mut claude,
            &format!(r#"{{"jsonrpc":"2.0","id":{id},"method":"ping"}}"#),
        );
    }
    // What the old worker got before its stdin ended reaches the hub ahead
    // of its `reloading`, on its own link; the rest goes to the new worker.
    let mut relayed = Vec::new();
    let reloading = loop {
        match message(&mut events, first).await {
            answer @ AgentMsg::UpdateAnswer { .. } => break answer,
            msg => relayed.push(msg),
        }
    };
    assert_eq!(
        reloading,
        AgentMsg::UpdateAnswer {
            update_id: 7,
            outcome: UpdateOutcome::Reloading
        }
    );
    // The worker let go of its stdin: these wait in the shim for the next.
    for id in 110..113 {
        send(
            &mut claude,
            &format!(r#"{{"jsonrpc":"2.0","id":{id},"method":"ping"}}"#),
        );
    }
    // Queued before `released`: still reaches Claude Code.
    to_first
        .send(HubMsg::Inbound {
            content: "before-release".into(),
            meta: Default::default(),
        })
        .await
        .unwrap();
    to_first
        .send(HubMsg::Released {
            update_id: 7,
            session_id: SESSION.into(),
        })
        .await
        .unwrap();

    let (second, register, to_second) = registered(&mut events).await;
    assert_ne!(second, first);
    assert_eq!(register.session_id, SESSION);
    assert_eq!(
        register.client.expect("a client").build,
        newer_build,
        "the new worker runs the new file"
    );
    to_second
        .send(HubMsg::Inbound {
            content: "after-handover".into(),
            meta: Default::default(),
        })
        .await
        .unwrap();
    send(&mut claude, r#"{"jsonrpc":"2.0","id":106,"method":"ping"}"#);
    let answered = |id: i64| {
        let out = out.clone();
        move || {
            out.lock().unwrap().iter().any(|line| {
                serde_json::from_str::<Value>(line).is_ok_and(|value| value["id"] == id)
            })
        }
    };
    wait_for("the ping after the hand-over", answered(106)).await;
    let delivered = |text: &'static str| {
        let out = out.clone();
        move || out.lock().unwrap().iter().any(|line| line.contains(text))
    };
    wait_for(
        "the message queued before the release",
        delivered("before-release"),
    )
    .await;
    wait_for(
        "a message to the new worker, without a second initialize",
        delivered("after-handover"),
    )
    .await;

    // The new worker asked Claude Code to list the tools again, and lists
    // `send_file`, which works through the shim.
    send(
        &mut claude,
        r#"{"jsonrpc":"2.0","id":120,"method":"tools/list"}"#,
    );
    wait_for("the tools of the new worker", answered(120)).await;
    let png = [b"\x89PNG\r\n\x1a\n".as_slice(), &[5; 300_000]].concat();
    std::fs::write(work.join("shot.png"), &png).unwrap();
    send(
        &mut claude,
        r#"{"jsonrpc":"2.0","id":121,"method":"tools/call","params":{"name":"send_file","arguments":{"path":"shot.png"}}}"#,
    );
    let (transfer_id, size) = loop {
        match message(&mut events, second).await {
            AgentMsg::FileOffer {
                transfer_id, size, ..
            } => break (transfer_id, size),
            msg => relayed.push(msg),
        }
    };
    assert_eq!(size, png.len() as u64);
    let accepted = HubMsg::FileAnswer {
        transfer_id,
        outcome: FileOutcome::Accepted,
        parts: Vec::new(),
    };
    to_second.send(accepted).await.unwrap();
    let mut received = 0;
    while received < size {
        match message(&mut events, second).await {
            AgentMsg::FileChunk(chunk) => {
                assert_eq!(chunk.offset, received);
                received += cctg::files::CHUNK.min((size - received) as usize) as u64;
            }
            msg => relayed.push(msg),
        }
    }
    let sent = HubMsg::FileAnswer {
        transfer_id,
        outcome: FileOutcome::Sent,
        parts: Vec::new(),
    };
    to_second.send(sent).await.unwrap();
    wait_for("the send_file answer", answered(121)).await;

    // Nothing up to date asks for nothing.
    to_second
        .send(HubMsg::Update {
            update_id: 8,
            release: None,
        })
        .await
        .unwrap();
    let up_to_date = loop {
        match message(&mut events, second).await {
            answer @ AgentMsg::UpdateAnswer { .. } => break answer,
            msg => relayed.push(msg),
        }
    };
    assert_eq!(
        up_to_date,
        AgentMsg::UpdateAnswer {
            update_id: 8,
            outcome: UpdateOutcome::UpToDate
        }
    );
    let replies = relayed
        .iter()
        .filter(|msg| matches!(msg, AgentMsg::Reply { text } if text == "in-flight reply"))
        .count();
    let prompts = relayed
        .iter()
        .filter(|msg| matches!(msg, AgentMsg::PermissionRequest(request) if request.request_id == "qwert"))
        .count();
    assert_eq!((replies, prompts), (1, 1), "each relayed once: {relayed:?}");

    let lines = out.lock().unwrap().clone();
    for id in [
        1, 107, 100, 101, 102, 103, 104, 105, 110, 111, 112, 106, 120, 121,
    ] {
        let answers = lines
            .iter()
            .filter(|line| serde_json::from_str::<Value>(line).is_ok_and(|value| value["id"] == id))
            .count();
        assert_eq!(answers, 1, "one answer for id {id}:\n{lines:#?}");
    }
    for line in &lines {
        let value: Value = serde_json::from_str(line).expect("only JSON-RPC on stdout");
        assert_eq!(value["jsonrpc"], "2.0", "{line}");
    }
    assert!(!lines.iter().any(|line| line.contains("cctg_shim")));
    let changed = lines
        .iter()
        .filter(|line| line.contains("notifications/tools/list_changed"))
        .count();
    assert_eq!(changed, 1, "only the new worker asks:\n{lines:#?}");
    let answer = |id: i64| {
        lines
            .iter()
            .filter_map(|line| serde_json::from_str::<Value>(line).ok())
            .find(|value| value["id"] == id)
            .unwrap()
    };
    let tools: Vec<Value> = answer(120)["result"]["tools"]
        .as_array()
        .unwrap()
        .iter()
        .map(|tool| tool["name"].clone())
        .collect();
    assert_eq!(tools, ["reply", "send_file"]);
    assert_eq!(answer(121)["result"]["isError"], false, "{lines:#?}");

    // Claude Code closes stdin: shim and worker end.
    drop(claude);
    let mut shim = shim;
    let deadline = Instant::now() + Duration::from_secs(10);
    let status = loop {
        if let Some(status) = shim.0.try_wait().unwrap() {
            break status;
        }
        assert!(Instant::now() < deadline, "the shim ends with its stdin");
        tokio::time::sleep(Duration::from_millis(50)).await;
    };
    assert!(status.success(), "{status:?}");
    let links = cctg::shim::links_dir(&exe);
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        let left: Vec<_> = std::fs::read_dir(&links).unwrap().flatten().collect();
        if left.is_empty() {
            break;
        }
        assert!(
            Instant::now() < deadline,
            "the shim removes its worker links: {left:?}"
        );
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
}

// ------------------------------------------------------------ TASK-050

const TAG: &str = "v9.9.9-e2e";

/// Serves the files under `dir` over HTTP/1.1 on loopback: `GET /a/b` is
/// `dir/a/b`, anything else 404; a path under `/cut/` gets a body cut
/// short (a dropped connection). The asked paths are kept.
fn serve(dir: PathBuf) -> (String, Arc<Mutex<Vec<String>>>) {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let base = format!("http://127.0.0.1:{}", listener.local_addr().unwrap().port());
    let asked = Arc::new(Mutex::new(Vec::new()));
    let log = asked.clone();
    std::thread::spawn(move || {
        for stream in listener.incoming() {
            let Ok(mut stream) = stream else { continue };
            let mut reader = BufReader::new(stream.try_clone().unwrap());
            let mut request = String::new();
            if reader.read_line(&mut request).is_err() {
                continue;
            }
            loop {
                let mut line = String::new();
                if reader.read_line(&mut line).unwrap_or(0) == 0 || line == "\r\n" {
                    break;
                }
            }
            let path = request.split(' ').nth(1).unwrap_or("/").to_owned();
            log.lock().unwrap().push(path.clone());
            let file = dir.join(path.trim_start_matches('/'));
            let answer =
                if path.starts_with("/cut/") {
                    b"HTTP/1.1 200 OK\r\nContent-Length: 100000\r\nConnection: close\r\n\r\nshort"
                        .to_vec()
                } else {
                    match std::fs::read(&file) {
                    Ok(body) if file.is_file() => [
                        format!(
                            "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                            body.len()
                        )
                        .into_bytes(),
                        body,
                    ]
                    .concat(),
                    _ => b"HTTP/1.1 404 Not Found\r\nContent-Length: 0\r\nConnection: close\r\n\r\n"
                        .to_vec(),
                }
                };
            let _ = stream.write_all(&answer);
        }
    });
    (base, asked)
}

fn asset(tag: &str) -> String {
    cctg::download::asset_name(tag, cctg::download::target().expect("a release platform"))
}

/// A shim started from `exe` with the release at `base`, and Claude Code's
/// end of it: its stdin and the lines it wrote.
struct Session {
    shim: Shim,
    claude: std::process::ChildStdin,
    out: Arc<Mutex<Vec<String>>>,
}

fn start_session(root: &Path, exe: &Path, port: u16, base: &str) -> Session {
    let (home, work) = (root.join("home"), root.join("work"));
    for dir in [&home, &work] {
        std::fs::create_dir_all(dir).unwrap();
    }
    let mut command = Command::new(exe);
    common::isolate(&mut command, &home);
    let mut child = command
        .arg("agent")
        .current_dir(&work)
        .env("CCTG_HUB_SECRET", SECRET)
        .env("CCTG_HUB_AGENT_ADDR", format!("127.0.0.1:{port}"))
        .env("CCTG_HOST", "box")
        .env("CCTG_STATE_DIR", root.join("state"))
        .env("CCTG_RELEASE_BASE_URL", base)
        .env("CLAUDE_CODE_SESSION_ID", SESSION)
        .env("CLAUDE_CONFIG_DIR", home.join("claude"))
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .expect("cctg agent starts");
    let mut claude = child.stdin.take().unwrap();
    let out = Arc::new(Mutex::new(Vec::<String>::new()));
    {
        let out = out.clone();
        let stdout = child.stdout.take().unwrap();
        std::thread::spawn(move || {
            for line in BufReader::new(stdout).lines() {
                let Ok(line) = line else { break };
                out.lock().unwrap().push(line);
            }
        });
    }
    send(
        &mut claude,
        r#"{"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"2025-11-25"}}"#,
    );
    send(
        &mut claude,
        r#"{"jsonrpc":"2.0","method":"notifications/initialized"}"#,
    );
    Session {
        shim: Shim(child),
        claude,
        out,
    }
}

impl Session {
    /// Claude Code pings and gets its answer: the channel loop runs.
    async fn ping(&mut self, id: i64) {
        send(
            &mut self.claude,
            &format!(r#"{{"jsonrpc":"2.0","id":{id},"method":"ping"}}"#),
        );
        let out = self.out.clone();
        wait_for("a ping answer", move || {
            out.lock().unwrap().iter().any(|line| {
                serde_json::from_str::<Value>(line).is_ok_and(|value| value["id"] == id)
            })
        })
        .await;
    }

    /// Claude Code closes stdin: shim and worker end.
    async fn end(self) {
        let Session {
            mut shim, claude, ..
        } = self;
        drop(claude);
        let deadline = Instant::now() + Duration::from_secs(10);
        loop {
            if let Some(status) = shim.0.try_wait().unwrap() {
                assert!(status.success(), "{status:?}");
                return;
            }
            assert!(Instant::now() < deadline, "the shim ends with its stdin");
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
    }
}

async fn update_answer(
    events: &mut mpsc::Receiver<AgentEvent>,
    conn: u64,
    to_agent: &mpsc::Sender<HubMsg>,
    update_id: u64,
    release: Option<&str>,
) -> UpdateOutcome {
    to_agent
        .send(HubMsg::Update {
            update_id,
            release: release.map(str::to_owned),
        })
        .await
        .unwrap();
    loop {
        if let AgentMsg::UpdateAnswer {
            update_id: answered,
            outcome,
        } = message(events, conn).await
        {
            assert_eq!(answered, update_id);
            return outcome;
        }
    }
}

async fn agent_link() -> (u16, mpsc::Receiver<AgentEvent>) {
    let listener = ingress::bind(SocketAddr::from((Ipv4Addr::LOCALHOST, 0)))
        .await
        .unwrap();
    let port = listener.local_addr().unwrap().port();
    let (events_tx, events) = mpsc::channel(64);
    tokio::spawn(ingress::serve_agents(
        listener,
        Secret::parse(SECRET).unwrap(),
        events_tx,
    ));
    (port, events)
}

#[tokio::test]
async fn the_hubs_release_is_downloaded_checked_and_taken() {
    let root =
        Root(std::env::temp_dir().join(format!("cctg-update-e2e-dl-{}", std::process::id())));
    let _ = std::fs::remove_dir_all(&root.0);
    let (bin, release) = (root.0.join("bin"), root.0.join("release").join(TAG));
    for dir in [&bin, &release] {
        std::fs::create_dir_all(dir).unwrap();
    }
    let exe = bin.join(format!("cctg{EXE}"));
    let original = std::fs::read(env!("CARGO_BIN_EXE_cctg")).unwrap();
    common::write_program(&exe, &original);
    // The release: a newer build under this platform's asset name.
    let published = release.join(asset(TAG));
    let newer_build = write_newer(&published, &original);
    let sum = cctg::client::build_of(&published).unwrap();
    std::fs::write(
        release.join("SHA256SUMS"),
        format!(
            "{}  cctg-{TAG}-other-target\n{sum}  {}\n",
            "0".repeat(64),
            asset(TAG)
        ),
    )
    .unwrap();
    let (base, asked) = serve(root.0.join("release"));

    let (port, mut events) = agent_link().await;
    let mut session = start_session(&root.0, &exe, port, &base);
    let (first, register, to_first) = registered(&mut events).await;
    let first_build = register.client.expect("a client").build;
    assert_ne!(first_build, newer_build);

    let outcome = update_answer(&mut events, first, &to_first, 7, Some(TAG)).await;
    assert_eq!(outcome, UpdateOutcome::Reloading);
    assert_eq!(
        std::fs::read(&exe).unwrap(),
        std::fs::read(&published).unwrap(),
        "the release's binary is in the file's place"
    );
    if cfg!(windows) {
        assert_eq!(
            std::fs::read(bin.join(format!("cctg.old{EXE}"))).unwrap(),
            original,
            "the running file was moved aside"
        );
    }
    to_first
        .send(HubMsg::Released {
            update_id: 7,
            session_id: SESSION.into(),
        })
        .await
        .unwrap();
    let (second, register, to_second) = registered(&mut events).await;
    assert_eq!(
        register.client.expect("a client").build,
        newer_build,
        "the new worker runs the downloaded file"
    );
    session.ping(50).await;

    // The file is the release now: nothing more is downloaded.
    let outcome = update_answer(&mut events, second, &to_second, 8, Some(TAG)).await;
    assert_eq!(outcome, UpdateOutcome::UpToDate);
    let asked = asked.lock().unwrap().clone();
    let count = |path: String| asked.iter().filter(|seen| **seen == path).count();
    assert_eq!(count(format!("/{TAG}/SHA256SUMS")), 2, "{asked:?}");
    assert_eq!(count(format!("/{TAG}/{}", asset(TAG))), 1, "{asked:?}");
    let parts: Vec<_> = std::fs::read_dir(&bin)
        .unwrap()
        .flatten()
        .filter(|entry| entry.file_name().to_string_lossy().ends_with(".part"))
        .collect();
    assert!(parts.is_empty(), "{parts:?}");
    session.end().await;
}

#[tokio::test]
async fn a_failed_download_leaves_the_old_binary_and_the_agent() {
    let root =
        Root(std::env::temp_dir().join(format!("cctg-update-e2e-bad-{}", std::process::id())));
    let _ = std::fs::remove_dir_all(&root.0);
    let bin = root.0.join("bin");
    std::fs::create_dir_all(&bin).unwrap();
    let exe = bin.join(format!("cctg{EXE}"));
    let original = std::fs::read(env!("CARGO_BIN_EXE_cctg")).unwrap();
    common::write_program(&exe, &original);
    let releases = root.0.join("release");
    // `bad`: the file does not match its line. `nosum`: no line for this
    // platform. `gone`: no such release (404). `cut`: the connection drops.
    for tag in ["bad", "nosum"] {
        let dir = releases.join(tag);
        std::fs::create_dir_all(&dir).unwrap();
        write_newer(&dir.join(asset(tag)), &original);
    }
    std::fs::write(
        releases.join("bad").join("SHA256SUMS"),
        format!("{}  {}\n", "ab".repeat(32), asset("bad")),
    )
    .unwrap();
    std::fs::write(
        releases.join("nosum").join("SHA256SUMS"),
        format!("{}  cctg-nosum-other-target\n", "ab".repeat(32)),
    )
    .unwrap();
    let (base, asked) = serve(releases);

    let (port, mut events) = agent_link().await;
    let mut session = start_session(&root.0, &exe, port, &base);
    let (conn, _, to_agent) = registered(&mut events).await;
    for (update_id, tag, want) in [
        (1, "bad", UpdateOutcome::ChecksumMismatch),
        (2, "nosum", UpdateOutcome::NoReleaseBuild),
        (3, "gone", UpdateOutcome::NoReleaseBuild),
        (4, "cut", UpdateOutcome::DownloadFailed),
        (5, "../x", UpdateOutcome::NoReleaseBuild),
    ] {
        let outcome = update_answer(&mut events, conn, &to_agent, update_id, Some(tag)).await;
        assert_eq!(outcome, want, "{tag}");
        assert_eq!(std::fs::read(&exe).unwrap(), original, "{tag}: untouched");
        session.ping(100 + update_id as i64).await;
    }
    let left: Vec<String> = std::fs::read_dir(&bin)
        .unwrap()
        .flatten()
        .map(|entry| entry.file_name().to_string_lossy().into_owned())
        .filter(|name| name.starts_with("cctg") && name != "cctg-workers")
        .collect();
    assert_eq!(left, [format!("cctg{EXE}")], "nothing written next to it");
    assert!(
        !asked.lock().unwrap().iter().any(|path| path.contains("..")),
        "a bad tag is never asked for"
    );
    // A hub without a release tag: the disk alone, as before TASK-050.
    let outcome = update_answer(&mut events, conn, &to_agent, 6, None).await;
    assert_eq!(outcome, UpdateOutcome::UpToDate);
    // The release does not come, but a newer file put in place otherwise
    // (by hand, install.sh) is still taken.
    std::fs::rename(&exe, bin.join(format!("cctg.old{EXE}"))).unwrap();
    let newer_build = write_newer(&exe, &original);
    let outcome = update_answer(&mut events, conn, &to_agent, 7, Some("cut")).await;
    assert_eq!(outcome, UpdateOutcome::Reloading);
    to_agent
        .send(HubMsg::Released {
            update_id: 7,
            session_id: SESSION.into(),
        })
        .await
        .unwrap();
    let (_, register, _) = registered(&mut events).await;
    assert_eq!(
        register.client.expect("a client").build,
        newer_build,
        "the new worker runs the file put in place"
    );
    session.ping(200).await;
    session.end().await;
}
