//! The hook spool end to end (TASK-018): real `cctg hook` and `cctg agent`
//! processes (home and state in a temp dir, so no real `~/.cctg` is read or
//! written) against the real `serve_hooks` / `serve_agents`. A `SessionStart`
//! that finds the hub down is kept on disk and reaches the hub before the next
//! hook of its session, or when the session's agent registers.

use std::io::Write;
use std::net::{Ipv4Addr, SocketAddr};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::time::Duration;

use cctg::hub::ingress::{self, AgentEvent};
use cctg::wire::{HookEvent, HookPost, Secret};
use tokio::net::TcpListener;
use tokio::sync::mpsc;

const SECRET: &str = "spool-e2e-secret-0123456789";
const SESSION: &str = "5b001e2e-0000-4000-8000-000000000001";
const WAIT: Duration = Duration::from_secs(20);

struct Home(PathBuf);

impl Drop for Home {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

impl Home {
    /// `<tmp>/<name>` with `.cctg/device.env` naming the two hub ports.
    fn new(name: &str, hook_port: u16, agent_port: u16) -> Self {
        let root =
            std::env::temp_dir().join(format!("cctg-spool-e2e-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(root.join(".cctg")).unwrap();
        std::fs::write(
            root.join(".cctg").join("device.env"),
            format!(
                "CCTG_HUB_SECRET={SECRET}\nCCTG_HUB_HOOK_ADDR=127.0.0.1:{hook_port}\n\
                 CCTG_HUB_AGENT_ADDR=127.0.0.1:{agent_port}\nCCTG_HOST=box\n"
            ),
        )
        .unwrap();
        Self(root)
    }

    fn spool(&self) -> PathBuf {
        self.0.join(".cctg").join("spool")
    }

    /// Every spool file, with its text.
    fn kept(&self) -> Vec<(PathBuf, String)> {
        let mut out = Vec::new();
        let Ok(sessions) = std::fs::read_dir(self.spool()) else {
            return out;
        };
        for session in sessions.flatten() {
            for file in std::fs::read_dir(session.path()).unwrap().flatten() {
                out.push((file.path(), std::fs::read_to_string(file.path()).unwrap()));
            }
        }
        out.sort();
        out
    }
}

fn command(home: &Path) -> Command {
    let mut command = Command::new(env!("CARGO_BIN_EXE_cctg"));
    command
        .env("USERPROFILE", home)
        .env("HOME", home)
        .env_remove("CCTG_HUB_SECRET")
        .env_remove("CCTG_HUB_HOOK_ADDR")
        .env_remove("CCTG_HUB_AGENT_ADDR")
        .env_remove("CCTG_HOST")
        .env_remove("CCTG_STATE_DIR");
    command
}

/// Runs `cctg hook <event>` with `input` as stdin; returns its stderr.
fn hook(home: &Home, event: &str, input: serde_json::Value) -> String {
    let mut child = command(&home.0)
        .args(["hook", event])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("cctg hook");
    let mut stdin = child.stdin.take().unwrap();
    stdin.write_all(input.to_string().as_bytes()).unwrap();
    drop(stdin);
    let output = child.wait_with_output().unwrap();
    assert!(output.status.success());
    assert!(output.stdout.is_empty());
    let stderr = String::from_utf8_lossy(&output.stderr).into_owned();
    assert!(!stderr.contains(SECRET), "{stderr}");
    stderr
}

fn input(event: &str, extra: serde_json::Value) -> serde_json::Value {
    let mut value = serde_json::json!({
        "session_id": SESSION,
        "cwd": "/nowhere/w",
        "transcript_path": "/nowhere/t.jsonl",
        "hook_event_name": event,
    });
    value
        .as_object_mut()
        .unwrap()
        .extend(extra.as_object().unwrap().clone());
    value
}

fn free_port() -> u16 {
    let listener = std::net::TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).unwrap();
    listener.local_addr().unwrap().port()
}

/// A hub that answers every hook 503 at once (down without a slow connect).
async fn refusing_hub() -> u16 {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).await.unwrap();
    let port = listener.local_addr().unwrap().port();
    tokio::spawn(async move {
        while let Ok((mut stream, _)) = listener.accept().await {
            tokio::spawn(async move {
                let mut buf = vec![0u8; 64 * 1024];
                let _ = stream.read(&mut buf).await;
                let _ = stream
                    .write_all(b"HTTP/1.1 503 Service Unavailable\r\nContent-Length: 0\r\n\r\n")
                    .await;
                let _ = stream.shutdown().await;
                let _ = stream.read_to_end(&mut buf).await;
            });
        }
    });
    port
}

async fn hooks_hub(port: u16) -> mpsc::Receiver<HookPost> {
    let listener = ingress::bind(SocketAddr::from((Ipv4Addr::LOCALHOST, port)))
        .await
        .unwrap();
    let (tx, rx) = mpsc::channel(64);
    tokio::spawn(ingress::serve_hooks(
        listener,
        Secret::parse(SECRET).unwrap(),
        tx,
    ));
    rx
}

async fn next(events: &mut mpsc::Receiver<HookPost>) -> HookPost {
    tokio::time::timeout(WAIT, events.recv())
        .await
        .expect("hook event in time")
        .expect("hook channel open")
}

async fn nothing_more(events: &mut mpsc::Receiver<HookPost>) {
    tokio::time::sleep(Duration::from_millis(300)).await;
    assert!(events.try_recv().is_err(), "an event came twice");
}

fn blocking<T: Send + 'static>(
    f: impl FnOnce() -> T + Send + 'static,
) -> tokio::task::JoinHandle<T> {
    tokio::task::spawn_blocking(f)
}

/// The acceptance path: the hub is down at `SessionStart`; the next hook of
/// the session delivers the kept start first, then its own event; nothing is
/// delivered twice, also when a delivered file comes back.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_missed_session_start_reaches_the_hub_before_the_next_hook() {
    let port = free_port();
    let home = Home::new("next-hook", port, free_port());
    // No listener on the port: the hub is down.
    let home = std::sync::Arc::new(home);
    let h = home.clone();
    let stderr = blocking(move || {
        hook(
            &h,
            "SessionStart",
            input("SessionStart", serde_json::json!({"source": "startup"})),
        )
    })
    .await
    .unwrap();
    assert!(stderr.contains("kept for the next hook"), "{stderr}");
    assert!(
        !stderr.contains(SESSION) && !stderr.contains("nowhere"),
        "{stderr}"
    );
    let kept = home.kept();
    assert_eq!(kept.len(), 1);
    let saved: serde_json::Value = serde_json::from_str(&kept[0].1).unwrap();
    assert_eq!(saved["event"]["type"], "session_start");
    assert_eq!(saved["session_id"], SESSION);

    let mut events = hooks_hub(port).await;
    let h = home.clone();
    blocking(move || {
        hook(
            &h,
            "UserPromptSubmit",
            input(
                "UserPromptSubmit",
                serde_json::json!({"prompt": "private prompt text", "prompt_id": "p1"}),
            ),
        )
    })
    .await
    .unwrap();
    let first = next(&mut events).await;
    assert_eq!(first.event.kind(), "session_start");
    assert_eq!(
        serde_json::to_value(&first).unwrap(),
        saved,
        "sent as it was kept"
    );
    assert_eq!(next(&mut events).await.event.kind(), "user_prompt_submit");
    nothing_more(&mut events).await;
    assert!(home.kept().is_empty(), "the spool is empty after delivery");

    // A crash between the hub's answer and the delete: the file comes back.
    let (path, text) = kept.into_iter().next().unwrap();
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(&path, text).unwrap();
    let h = home.clone();
    blocking(move || {
        hook(
            &h,
            "Stop",
            input("Stop", serde_json::json!({"last_assistant_message": "ok"})),
        )
    })
    .await
    .unwrap();
    assert_eq!(next(&mut events).await.event.kind(), "stop");
    nothing_more(&mut events).await;
    assert!(home.kept().is_empty());
}

/// Only session starts and ends are kept, never a prompt or an answer, and
/// a session keeps at most `MAX_PER_SESSION` files.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn the_spool_holds_no_text_and_is_bounded() {
    let port = refusing_hub().await;
    let home = std::sync::Arc::new(Home::new("bounded", port, free_port()));
    let h = home.clone();
    blocking(move || {
        let stop = input(
            "Stop",
            serde_json::json!({"last_assistant_message": "private answer text"}),
        );
        let stderr = hook(&h, "Stop", stop);
        assert!(stderr.contains("hook event not delivered"), "{stderr}");
        hook(
            &h,
            "UserPromptSubmit",
            input(
                "UserPromptSubmit",
                serde_json::json!({"prompt": "private prompt text", "prompt_id": "p1"}),
            ),
        );
    })
    .await
    .unwrap();
    assert!(home.kept().is_empty(), "no prompt or answer is kept");
    let h = home.clone();
    let last = blocking(move || {
        let mut last = String::new();
        for n in 0..cctg::spool::MAX_PER_SESSION + 2 {
            let event = if n % 2 == 0 {
                "SessionStart"
            } else {
                "SessionEnd"
            };
            last = hook(
                &h,
                event,
                input(
                    event,
                    serde_json::json!({"source": "resume", "reason": "other"}),
                ),
            );
        }
        last
    })
    .await
    .unwrap();
    assert!(last.contains("not kept") && last.contains("full"), "{last}");
    let kept = home.kept();
    assert_eq!(kept.len(), cctg::spool::MAX_PER_SESSION);
    for (_, text) in &kept {
        assert!(!text.contains("private"), "{text}");
        assert!(!text.contains(SECRET));
        let value: serde_json::Value = serde_json::from_str(text).unwrap();
        let keys: Vec<&str> = value
            .as_object()
            .unwrap()
            .keys()
            .map(String::as_str)
            .collect();
        assert_eq!(
            keys,
            [
                "cwd",
                "event",
                "event_id",
                "host",
                "session_id",
                "transcript_path",
                "v"
            ]
        );
    }
}

struct Agent(Child);

impl Drop for Agent {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

/// The session's agent registers once the hub is back and delivers the kept
/// start itself: the hub learns the session without another hook.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn the_agent_delivers_its_sessions_kept_start_when_it_registers() {
    let (hook_port, agent_port) = (free_port(), free_port());
    let home = std::sync::Arc::new(Home::new("agent", hook_port, agent_port));
    let h = home.clone();
    blocking(move || {
        hook(
            &h,
            "SessionStart",
            input("SessionStart", serde_json::json!({"source": "startup"})),
        )
    })
    .await
    .unwrap();
    assert_eq!(home.kept().len(), 1);

    let mut hooks = hooks_hub(hook_port).await;
    let agents_listener = ingress::bind(SocketAddr::from((Ipv4Addr::LOCALHOST, agent_port)))
        .await
        .unwrap();
    let (agents_tx, mut agents) = mpsc::channel(16);
    tokio::spawn(ingress::serve_agents(
        agents_listener,
        Secret::parse(SECRET).unwrap(),
        agents_tx,
    ));
    let mut child = command(&home.0)
        .arg("agent")
        .env_remove("CLAUDE_CODE_ENTRYPOINT")
        .env("CLAUDE_CODE_SESSION_ID", SESSION)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .expect("cctg agent");
    // Keep stdin open (the agent stops at EOF); drain stdout.
    std::mem::forget(child.stdin.take());
    let mut stdout = child.stdout.take().unwrap();
    std::thread::spawn(move || {
        let _ = std::io::copy(&mut stdout, &mut std::io::sink());
    });
    let _agent = Agent(child);
    let registered = tokio::time::timeout(WAIT, agents.recv())
        .await
        .expect("agent registers")
        .unwrap();
    assert!(
        matches!(&registered, AgentEvent::Registered { register, .. } if register.session_id == SESSION)
    );
    let start = next(&mut hooks).await;
    assert!(matches!(start.event, HookEvent::SessionStart { .. }));
    assert_eq!(start.session_id, SESSION);
    nothing_more(&mut hooks).await;
    let deadline = tokio::time::Instant::now() + WAIT;
    while !home.kept().is_empty() {
        assert!(tokio::time::Instant::now() < deadline, "spool emptied");
        tokio::time::sleep(Duration::from_millis(25)).await;
    }
}
