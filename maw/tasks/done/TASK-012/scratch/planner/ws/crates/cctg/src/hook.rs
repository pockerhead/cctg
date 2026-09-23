//! `cctg hook <event>`: read the Claude Code hook input from stdin, keep only
//! the fields the hub uses, and send them in one HTTP POST. The hook always
//! exits 0 and never writes to stdout (for `SessionStart` and
//! `UserPromptSubmit` stdout would become context for Claude). Failures go to
//! stderr as fixed texts: never the input, never the secret.
//!
//! Transport: plain TCP instead of `reqwest`: the body is tiny, the hub is
//! local or on a private network, and a TLS-capable client costs start-up
//! time the `SessionEnd` budget (1.5 s shared) cannot spare.
//!
//! [`HookPost::new`](crate::wire::HookPost::new) mints the event id; calling
//! [`post`] again with the same value re-sends the same event, which the hub
//! drops as a repeat.
//!
//! Registration: `docs/hook-settings.json`. Configuration: [`crate::device`].

use std::io::Read;
use std::path::Path;
use std::time::Duration;

use serde::Deserialize;
use tokio::io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader};
use tokio::net::TcpStream;
use tracing::{debug, warn};

use crate::device::{self, DeviceConfig};
use crate::proctree::{self, Lineage};
use crate::wire::{HOOK_PATH, HookEvent, HookPost, Secret};

/// Budget of the POST, connect included. `SessionEnd` hooks share 1.5 s in
/// all, start-up and the process snapshot included; `UserPromptSubmit` and the
/// handback matcher block Claude Code while they run. On Windows a connect to
/// a closed local port is retried and lasts until this timeout, so a stopped
/// hub costs every hook exactly this much.
pub const POST_TIMEOUT: Duration = Duration::from_millis(500);
/// Claude Code writes the whole input at once and closes stdin.
pub const STDIN_TIMEOUT: Duration = Duration::from_millis(300);
/// Larger input is dropped, not truncated: cut JSON is not JSON.
pub const MAX_STDIN: u64 = 8 << 20;
/// Cap of one free-text field (assistant message, handback report), in bytes.
/// Even fully `\u`-escaped it keeps the body under `wire::MAX_HOOK_BODY`.
pub const MAX_TEXT: usize = 128 << 10;
const HANDBACK_TOOL: &str = "SubagentHandback";

/// Runs one hook invocation. Never fails: every problem ends as one fixed
/// line on stderr.
pub async fn run(event: &str) {
    let Some(input) = read_stdin(MAX_STDIN, STDIN_TIMEOUT) else {
        warn!("hook input unreadable, too large or late; nothing sent");
        return;
    };
    let config = DeviceConfig::load();
    let hook_post = match build_here(event, &input, &config.host) {
        Ok(hook_post) => hook_post,
        Err(skip) => {
            debug!(reason = skip.0, "hook event skipped");
            return;
        }
    };
    let secret = match &config.secret {
        Ok(secret) => secret,
        Err(problem) => {
            warn!(%problem, "hook event not sent");
            return;
        }
    };
    if let Err(error) = post(&config.hook_addr, secret, &hook_post, POST_TIMEOUT).await {
        warn!(event = hook_post.event.kind(), %error, "hook event not delivered");
    }
}

/// [`build`] with this device as the probe.
fn build_here(event: &str, input: &[u8], host: &str) -> Result<HookPost, Skip> {
    let env_pid = std::env::var("CLAUDE_PID")
        .ok()
        .and_then(|pid| pid.trim().parse().ok());
    let env_session = std::env::var("CLAUDE_CODE_SESSION_ID").ok();
    let lineage =
        |session: &str| proctree::current_lineage(env_pid, env_session.as_deref(), session);
    let probe = Probe {
        host,
        cwd: &device::canonical_cwd,
        lineage: &lineage,
        exists: &|path| path.exists(),
    };
    build(event, input, &probe)
}

/// Reads stdin on its own thread: a blocking read cannot be cancelled, and a
/// tokio stdin read would hold up runtime shutdown. The caller exits the
/// process, which ends the thread if it is still blocked.
fn read_stdin(limit: u64, timeout: Duration) -> Option<Vec<u8>> {
    let (tx, rx) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        let mut buf = Vec::new();
        let read = std::io::stdin()
            .lock()
            .take(limit + 1)
            .read_to_end(&mut buf);
        let _ = tx.send(read.ok().map(|_| buf));
    });
    let buf = rx.recv_timeout(timeout).ok()??;
    (buf.len() as u64 <= limit).then_some(buf)
}

/// What a hook learns from its device besides stdin. Injected for tests.
pub struct Probe<'a> {
    pub host: &'a str,
    /// Folder resolution, see [`device::canonical_cwd`].
    pub cwd: &'a dyn Fn(&str) -> String,
    /// Process-tree lineage for the stdin session id; asked only by
    /// `SessionStart` and `SessionEnd`.
    pub lineage: &'a dyn Fn(&str) -> Lineage,
    pub exists: &'a dyn Fn(&Path) -> bool,
}

/// Why an input produced no POST. Fixed text, safe to log.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Skip(pub &'static str);

/// The hook input fields cctg uses; everything else is ignored.
#[derive(Debug, Default, Deserialize)]
#[serde(default)]
struct Input {
    session_id: String,
    cwd: String,
    transcript_path: String,
    hook_event_name: Option<String>,
    source: Option<String>,
    reason: Option<String>,
    prompt_id: Option<String>,
    last_assistant_message: Option<String>,
    agent_id: Option<String>,
    agent_type: Option<String>,
    agent_transcript_path: Option<String>,
    tool_name: Option<String>,
    tool_input: Option<ToolInput>,
}

#[derive(Debug, Default, Deserialize)]
#[serde(default)]
struct ToolInput {
    message: Option<String>,
}

/// Turns one hook input into the POST for the hub, or says why there is none.
/// `event` is the name the settings passed on the command line.
pub fn build(event: &str, input: &[u8], probe: &Probe<'_>) -> Result<HookPost, Skip> {
    let input: Input =
        serde_json::from_slice(input).map_err(|_| Skip("input is not a hook JSON object"))?;
    if input.session_id.is_empty() {
        return Err(Skip("input has no session_id"));
    }
    if input
        .hook_event_name
        .as_deref()
        .is_some_and(|name| name != event)
    {
        return Err(Skip("input is for another hook event"));
    }
    let hook_event = match event {
        "SessionStart" => {
            let lineage = (probe.lineage)(&input.session_id);
            HookEvent::SessionStart {
                source: input.source,
                claude_pid: lineage.claude_pid,
                parent_claude_pid: lineage.parent_claude_pid,
            }
        }
        "SessionEnd" => HookEvent::SessionEnd {
            reason: input.reason,
            claude_pid: (probe.lineage)(&input.session_id).claude_pid,
        },
        "UserPromptSubmit" => HookEvent::UserPromptSubmit {
            prompt_id: input.prompt_id,
        },
        "Stop" => HookEvent::Stop {
            prompt_id: input.prompt_id,
            last_assistant_message: input.last_assistant_message.map(cap_text),
        },
        "SubagentStart" => HookEvent::SubagentStart {
            agent_id: agent_id(input.agent_id)?,
            agent_type: agent_type(input.agent_type)?,
        },
        "SubagentStop" => {
            let agent_id = agent_id(input.agent_id)?;
            let agent_type = agent_type(input.agent_type)?;
            let agent_transcript_path = input.agent_transcript_path.filter(|path| !path.is_empty());
            if let Some(path) = &agent_transcript_path
                && !has_agent_files(path, probe.exists)
            {
                // Claude Code's own agents (prompt suggestions, /btw, the
                // `--agent` session name) leave no subagent files behind.
                return Err(Skip("internal agent"));
            }
            HookEvent::SubagentStop {
                agent_id,
                agent_type,
                agent_transcript_path,
                last_assistant_message: input.last_assistant_message.map(cap_text),
            }
        }
        "PreToolUse" | "PostToolUse" => {
            if input.tool_name.as_deref() != Some(HANDBACK_TOOL) {
                return Err(Skip("tool is not SubagentHandback"));
            }
            let message = input
                .tool_input
                .and_then(|tool_input| tool_input.message)
                .ok_or(Skip("handback without a message"))?;
            HookEvent::SubagentHandback {
                agent_id: agent_id(input.agent_id)?,
                message: cap_text(message),
            }
        }
        _ => return Err(Skip("unsupported hook event")),
    };
    Ok(HookPost::new(
        probe.host.to_owned(),
        input.session_id,
        (probe.cwd)(&input.cwd),
        input.transcript_path,
        hook_event,
    ))
}

fn agent_id(id: Option<String>) -> Result<String, Skip> {
    id.filter(|id| !id.is_empty())
        .ok_or(Skip("subagent event without agent_id"))
}

/// An empty type is a Claude Code internal agent (TASK-003: 14 of 14).
fn agent_type(kind: Option<String>) -> Result<String, Skip> {
    kind.filter(|kind| !kind.is_empty())
        .ok_or(Skip("internal agent"))
}

/// A real subagent leaves `agent-<id>.jsonl` and/or `agent-<id>.meta.json`;
/// the 14 internal agents of TASK-003 left neither.
fn has_agent_files(transcript: &str, exists: &dyn Fn(&Path) -> bool) -> bool {
    let meta = transcript
        .strip_suffix(".jsonl")
        .map(|stem| format!("{stem}.meta.json"));
    exists(Path::new(transcript)) || meta.is_some_and(|meta| exists(Path::new(&meta)))
}

fn cap_text(mut text: String) -> String {
    if text.len() > MAX_TEXT {
        let cut = text.floor_char_boundary(MAX_TEXT - '\u{2026}'.len_utf8());
        text.truncate(cut);
        text.push('\u{2026}');
    }
    text
}

const MAX_STATUS_LINE: u64 = 256;

#[derive(Debug, PartialEq, Eq, thiserror::Error)]
pub enum PostError {
    #[error("hub did not answer within {0:?}")]
    Timeout(Duration),
    #[error("cannot reach the hub: {0:?}")]
    Io(std::io::ErrorKind),
    #[error("hub answered HTTP {0}")]
    Status(u16),
    #[error("hub answer is not HTTP")]
    BadResponse,
}

/// Sends `post` to the hub hook endpoint at `addr` (`host:port`). Everything,
/// connect included, fits in `timeout`. `Ok` means the hub has the event.
pub async fn post(
    addr: &str,
    secret: &Secret,
    post: &HookPost,
    timeout: Duration,
) -> Result<(), PostError> {
    let body = serde_json::to_vec(post).expect("hook posts always serialize");
    let head = format!(
        "POST {HOOK_PATH} HTTP/1.1\r\nHost: cctg-hub\r\nAuthorization: Bearer {}\r\n\
         Content-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
        secret.expose(),
        body.len()
    );
    let exchange = async {
        let io = |error: std::io::Error| PostError::Io(error.kind());
        let mut stream = TcpStream::connect(addr).await.map_err(io)?;
        let _ = stream.set_nodelay(true);
        stream
            .write_all(&[head.as_bytes(), &body].concat())
            .await
            .map_err(io)?;
        let mut status_line = Vec::new();
        BufReader::new(stream)
            .take(MAX_STATUS_LINE)
            .read_until(b'\n', &mut status_line)
            .await
            .map_err(io)?;
        match parse_status(&status_line) {
            Some(204) => Ok(()),
            Some(code) => Err(PostError::Status(code)),
            None => Err(PostError::BadResponse),
        }
    };
    tokio::time::timeout(timeout, exchange)
        .await
        .unwrap_or(Err(PostError::Timeout(timeout)))
}

/// Accepts only a complete `HTTP/1.1 NNN[ reason]\r\n` line: a truncated or
/// foreign answer must never count as delivered.
fn parse_status(line: &[u8]) -> Option<u16> {
    let rest = line.strip_suffix(b"\r\n")?.strip_prefix(b"HTTP/1.1 ")?;
    let (code, tail) = rest.split_at_checked(3)?;
    if !code.iter().all(u8::is_ascii_digit) || !(tail.is_empty() || tail[0] == b' ') {
        return None;
    }
    std::str::from_utf8(code).ok()?.parse().ok()
}

#[cfg(test)]
mod tests {
    use std::net::{Ipv4Addr, SocketAddr};
    use std::time::Instant;

    use tokio::net::TcpListener;
    use tokio::sync::mpsc;

    use super::*;
    use crate::hub::ingress;
    use crate::wire::HookEvent;

    const SECRET: &str = "0123456789abcdef-secret";

    fn secret() -> Secret {
        Secret::parse(SECRET).unwrap()
    }

    fn sample() -> HookPost {
        HookPost::new(
            "box".into(),
            "5e551017-0000-4000-8000-000000000001".into(),
            "/w".into(),
            "/w/s.jsonl".into(),
            HookEvent::SessionEnd {
                reason: Some("other".into()),
                claude_pid: None,
            },
        )
    }

    async fn hub() -> (String, mpsc::Receiver<HookPost>) {
        let listener = ingress::bind(SocketAddr::from((Ipv4Addr::LOCALHOST, 0)))
            .await
            .unwrap();
        let addr = listener.local_addr().unwrap().to_string();
        let (tx, rx) = mpsc::channel(8);
        tokio::spawn(ingress::serve_hooks(listener, secret(), tx));
        (addr, rx)
    }

    #[tokio::test]
    async fn a_resent_post_reaches_the_hub_once() {
        let (addr, mut events) = hub().await;
        let sent = sample();
        let timeout = Duration::from_secs(5);
        let started = Instant::now();
        post(&addr, &secret(), &sent, timeout).await.unwrap();
        assert!(
            started.elapsed() < Duration::from_millis(500),
            "{:?}",
            started.elapsed()
        );
        post(&addr, &secret(), &sent, timeout).await.unwrap();
        assert_eq!(events.recv().await, Some(sent));
        tokio::time::sleep(Duration::from_millis(50)).await;
        assert!(events.try_recv().is_err());
    }

    #[tokio::test]
    async fn a_wrong_secret_is_refused() {
        let (addr, mut events) = hub().await;
        let wrong = Secret::parse("0123456789abcdef-secreT").unwrap();
        let result = post(&addr, &wrong, &sample(), Duration::from_secs(5)).await;
        assert_eq!(result, Err(PostError::Status(401)));
        assert!(events.try_recv().is_err());
    }

    #[tokio::test]
    async fn a_silent_hub_costs_at_most_the_timeout() {
        // Accepts and never answers.
        let listener = TcpListener::bind(SocketAddr::from((Ipv4Addr::LOCALHOST, 0)))
            .await
            .unwrap();
        let addr = listener.local_addr().unwrap().to_string();
        let _held = tokio::spawn(async move {
            let mut open = Vec::new();
            while let Ok((stream, _)) = listener.accept().await {
                open.push(stream);
            }
        });
        let timeout = Duration::from_millis(300);
        let started = Instant::now();
        let result = post(&addr, &secret(), &sample(), timeout).await;
        assert_eq!(result, Err(PostError::Timeout(timeout)));
        assert!(
            started.elapsed() < Duration::from_secs(1),
            "{:?}",
            started.elapsed()
        );
    }

    #[tokio::test]
    async fn no_hub_is_an_error_within_the_timeout() {
        let listener = TcpListener::bind(SocketAddr::from((Ipv4Addr::LOCALHOST, 0)))
            .await
            .unwrap();
        let addr = listener.local_addr().unwrap().to_string();
        drop(listener);
        let timeout = Duration::from_millis(800);
        let started = Instant::now();
        let error = post(&addr, &secret(), &sample(), timeout)
            .await
            .unwrap_err();
        assert!(
            matches!(error, PostError::Io(_) | PostError::Timeout(_)),
            "{error:?}"
        );
        assert!(started.elapsed() < Duration::from_millis(1500));
        assert!(!format!("{error} {error:?}").contains(SECRET));
    }

    #[test]
    fn status_lines() {
        assert_eq!(parse_status(b"HTTP/1.1 204 No Content\r\n"), Some(204));
        assert_eq!(parse_status(b"HTTP/1.1 401 Unauthorized\r\n"), Some(401));
        assert_eq!(parse_status(b"HTTP/1.1 204\r\n"), Some(204));
        for bad in [
            &b"HTTP/1.1 204 No Content"[..],
            b"HTTP/1.1 204",
            b"HTTP/1.1 204 No Content\n",
            b"HTTP/1.x 204 No Content\r\n",
            b"HTTP/1.0 204 No Content\r\n",
            b"HTTP/1.1 20 No Content\r\n",
            b"HTTP/1.1 2044 No Content\r\n",
            b"HTTP/1.1 2x4 No Content\r\n",
            b"HTTP/1.1  204 No Content\r\n",
            b"SSH-2.0-x\r\n",
            b"",
        ] {
            assert_eq!(parse_status(bad), None, "{}", String::from_utf8_lossy(bad));
        }
    }

    /// A fake hub that reads the request, writes `answer` and closes.
    async fn answering(answer: &'static [u8]) -> String {
        let listener = TcpListener::bind(SocketAddr::from((Ipv4Addr::LOCALHOST, 0)))
            .await
            .unwrap();
        let addr = listener.local_addr().unwrap().to_string();
        tokio::spawn(async move {
            while let Ok((mut stream, _)) = listener.accept().await {
                let mut request = vec![0u8; 64 * 1024];
                let _ = stream.read(&mut request).await;
                let _ = stream.write_all(answer).await;
                let _ = stream.shutdown().await;
                let _ = stream.read_to_end(&mut request).await;
            }
        });
        addr
    }

    #[tokio::test]
    async fn a_truncated_or_foreign_status_line_is_not_success() {
        let timeout = Duration::from_secs(5);
        for answer in [
            &b"HTTP/1.1 204"[..],
            b"HTTP/1.x 204 No Content\r\n\r\n",
            b"HTTP/1.1 2040 No Content\r\n\r\n",
            b"",
        ] {
            let addr = answering(answer).await;
            let result = post(&addr, &secret(), &sample(), timeout).await;
            assert_eq!(
                result,
                Err(PostError::BadResponse),
                "{}",
                String::from_utf8_lossy(answer)
            );
        }
        let addr = answering(b"HTTP/1.1 204 No Content\r\n\r\n").await;
        assert_eq!(post(&addr, &secret(), &sample(), timeout).await, Ok(()));
    }
}

#[cfg(test)]
mod build_tests {
    use std::cell::Cell;
    use std::collections::BTreeSet;
    use std::path::Path;

    use serde_json::Value;

    use super::*;

    fn fixture(name: &str) -> Vec<u8> {
        let path = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("tests/fixtures/hook")
            .join(format!("{name}.json"));
        std::fs::read(&path).expect("hook fixture")
    }

    const OWN: Lineage = Lineage {
        claude_pid: Some(28764),
        parent_claude_pid: None,
    };

    /// `exists` answers true for every path: subagent files present.
    fn with_probe<T>(
        lineage: Lineage,
        exists: bool,
        f: impl FnOnce(&Probe<'_>, &Cell<u32>) -> T,
    ) -> T {
        let asked = Cell::new(0);
        let lineage_fn = |_: &str| {
            asked.set(asked.get() + 1);
            lineage
        };
        let cwd_fn = |cwd: &str| format!("canon:{cwd}");
        let exists_fn = move |_: &Path| exists;
        let probe = Probe {
            host: "box",
            cwd: &cwd_fn,
            lineage: &lineage_fn,
            exists: &exists_fn,
        };
        f(&probe, &asked)
    }

    fn keys(value: &Value) -> BTreeSet<String> {
        value.as_object().unwrap().keys().cloned().collect()
    }

    fn set(names: &[&str]) -> BTreeSet<String> {
        names.iter().map(|name| (*name).to_owned()).collect()
    }

    /// Serialized POST: the top-level keys are always the same, and the
    /// event object has exactly `want` keys.
    fn check(event: &str, input: &[u8], lineage: Lineage, want: &[&str]) -> Value {
        with_probe(lineage, true, |probe, asked| {
            let post = build(event, input, probe).expect("event is sent");
            let value = serde_json::to_value(&post).unwrap();
            assert_eq!(
                keys(&value),
                set(&[
                    "v",
                    "event_id",
                    "host",
                    "session_id",
                    "cwd",
                    "transcript_path",
                    "event"
                ])
            );
            assert_eq!(keys(&value["event"]), set(want), "{event}");
            let walks = u32::from(matches!(event, "SessionStart" | "SessionEnd"));
            assert_eq!(asked.get(), walks, "{event}: process tree asked");
            assert_eq!(value["host"], "box");
            assert!(value["cwd"].as_str().unwrap().starts_with("canon:"));
            value
        })
    }

    #[test]
    fn each_event_carries_its_fields_and_nothing_else() {
        let v = check(
            "SessionStart",
            &fixture("session_start"),
            OWN,
            &["type", "source", "claude_pid", "parent_claude_pid"],
        );
        assert_eq!(v["event"]["source"], "startup");
        assert_eq!(v["event"]["claude_pid"], 28764);
        assert_eq!(v["event"]["parent_claude_pid"], Value::Null);
        assert_eq!(v["session_id"], "1e087ca8-58b5-4c0b-9f03-b7926645b744");
        assert_eq!(v["cwd"], "canon:~\\dev\\cctg");
        assert!(
            v["transcript_path"]
                .as_str()
                .unwrap()
                .ends_with("1e087ca8-58b5-4c0b-9f03-b7926645b744.jsonl")
        );

        let v = check(
            "SessionEnd",
            &fixture("session_end"),
            OWN,
            &["type", "reason", "claude_pid"],
        );
        assert_eq!(v["event"]["reason"], "other");
        assert_eq!(v["event"]["claude_pid"], 28764);

        let v = check(
            "UserPromptSubmit",
            &fixture("user_prompt_submit"),
            OWN,
            &["type", "prompt_id"],
        );
        assert_eq!(
            v["event"]["prompt_id"],
            "0bb27012-0d04-43a9-9fa9-128e91ee7e34"
        );
        assert!(!v.to_string().contains("private prompt text"));

        let v = check(
            "Stop",
            &fixture("stop"),
            OWN,
            &["type", "prompt_id", "last_assistant_message"],
        );
        assert_eq!(v["event"]["last_assistant_message"], "Ok.");

        let v = check(
            "SubagentStart",
            &fixture("subagent_start"),
            OWN,
            &["type", "agent_id", "agent_type"],
        );
        assert_eq!(v["event"]["agent_id"], "a8c1bff86acd31609");
        assert_eq!(v["event"]["agent_type"], "Explore");

        let v = check(
            "SubagentStop",
            &fixture("subagent_stop"),
            OWN,
            &[
                "type",
                "agent_id",
                "agent_type",
                "agent_transcript_path",
                "last_assistant_message",
            ],
        );
        assert!(
            v["event"]["agent_transcript_path"]
                .as_str()
                .unwrap()
                .ends_with("agent-a8c1bff86acd31609.jsonl")
        );
        assert!(
            v["event"]["last_assistant_message"]
                .as_str()
                .unwrap()
                .contains("cctg")
        );
        assert!(!v.to_string().contains("background_tasks"));

        for (event, name) in [
            ("PreToolUse", "pre_tool_use_handback"),
            ("PostToolUse", "post_tool_use_handback"),
        ] {
            let v = check(event, &fixture(name), OWN, &["type", "agent_id", "message"]);
            assert_eq!(v["event"]["type"], "subagent_handback");
            assert_eq!(v["event"]["agent_id"], "ae1e99b76211e2536");
            assert_eq!(v["event"]["message"], "2+2 = 4.");
        }
    }

    #[test]
    fn nesting_comes_from_the_lineage() {
        let nested = Lineage {
            claude_pid: Some(25388),
            parent_claude_pid: Some(776),
        };
        let v = check(
            "SessionStart",
            &fixture("session_start"),
            nested,
            &["type", "source", "claude_pid", "parent_claude_pid"],
        );
        assert_eq!(v["event"]["claude_pid"], 25388);
        assert_eq!(v["event"]["parent_claude_pid"], 776);
        // SessionEnd reports the own pid only, never the parent.
        let v = check(
            "SessionEnd",
            &fixture("session_end"),
            nested,
            &["type", "reason", "claude_pid"],
        );
        assert_eq!(v["event"]["claude_pid"], 25388);
    }

    #[test]
    fn source_is_optional_and_read_only_from_session_start() {
        let v = check(
            "SessionStart",
            br#"{"session_id":"s","cwd":"/w","transcript_path":"/t"}"#,
            OWN,
            &["type", "source", "claude_pid", "parent_claude_pid"],
        );
        assert_eq!(v["event"]["source"], Value::Null);
        // A `source` on another event is ignored, not an error.
        check(
            "SessionEnd",
            br#"{"session_id":"s","source":"startup","hook_event_name":"SessionEnd"}"#,
            OWN,
            &["type", "reason", "claude_pid"],
        );
    }

    #[test]
    fn internal_agents_are_dropped() {
        with_probe(OWN, true, |probe, _| {
            // Empty agent_type, as in all 14 TASK-003 noise events.
            assert_eq!(
                build("SubagentStop", &fixture("subagent_stop_internal"), probe).unwrap_err(),
                Skip("internal agent")
            );
            let start = br#"{"session_id":"s","agent_id":"a1","agent_type":""}"#;
            assert_eq!(
                build("SubagentStart", start, probe).unwrap_err(),
                Skip("internal agent")
            );
        });
        // A typed agent (the `--agent` session name) with no subagent files.
        let typed = br#"{"session_id":"s","agent_id":"a1","agent_type":"my-agent","agent_transcript_path":"/p/s/subagents/agent-a1.jsonl"}"#;
        with_probe(OWN, false, |probe, _| {
            assert_eq!(
                build("SubagentStop", typed, probe).unwrap_err(),
                Skip("internal agent")
            );
        });
        // Only the meta file exists yet: a real subagent.
        let asked = std::cell::RefCell::new(Vec::new());
        let exists = |path: &Path| {
            asked.borrow_mut().push(path.to_string_lossy().into_owned());
            path.to_string_lossy().ends_with(".meta.json")
        };
        let probe = Probe {
            host: "box",
            cwd: &|cwd: &str| cwd.to_owned(),
            lineage: &|_: &str| OWN,
            exists: &exists,
        };
        assert!(build("SubagentStop", typed, &probe).is_ok());
        assert!(
            asked
                .borrow()
                .contains(&"/p/s/subagents/agent-a1.meta.json".to_owned())
        );
    }

    #[test]
    fn other_tools_and_incomplete_handbacks_are_skipped() {
        with_probe(OWN, true, |probe, _| {
            let bash = br#"{"session_id":"s","hook_event_name":"PreToolUse","tool_name":"Bash","tool_input":{"command":"ls"}}"#;
            assert!(build("PreToolUse", bash, probe).is_err());
            let no_message = br#"{"session_id":"s","agent_id":"a","tool_name":"SubagentHandback","tool_input":{}}"#;
            assert!(build("PostToolUse", no_message, probe).is_err());
            let no_agent = br#"{"session_id":"s","tool_name":"SubagentHandback","tool_input":{"message":"m"}}"#;
            assert!(build("PostToolUse", no_agent, probe).is_err());
        });
    }

    #[test]
    fn broken_input_is_skipped_without_panicking() {
        let whole = fixture("session_start");
        with_probe(OWN, true, |probe, asked| {
            for cut in 0..whole.len() {
                // Every prefix of a real input, plus garbage.
                let _ = build("SessionStart", &whole[..cut], probe);
            }
            for input in [
                &b""[..],
                b"\xff\xfe\x00",
                b"null",
                b"[]",
                b"42",
                br#"{"session_id":7}"#,
                br#"{"session_id":""}"#,
                br#"{"cwd":"/w"}"#,
                br#"{"session_id":"s","tool_input":"x"}"#,
            ] {
                assert!(build("SessionStart", input, probe).is_err());
                assert!(build("PostToolUse", input, probe).is_err());
            }
            assert!(asked.get() <= 1, "tree walked for rejected input");
        });
    }

    #[test]
    fn misrouted_and_unknown_events_are_skipped() {
        with_probe(OWN, true, |probe, _| {
            assert_eq!(
                build("SessionStart", &fixture("session_end"), probe).unwrap_err(),
                Skip("input is for another hook event")
            );
            assert_eq!(
                build("PreCompact", br#"{"session_id":"s"}"#, probe).unwrap_err(),
                Skip("unsupported hook event")
            );
        });
    }

    #[test]
    fn long_texts_are_capped_on_a_char_boundary() {
        let long = "й".repeat(MAX_TEXT); // 2 bytes each
        let input = serde_json::json!({
            "session_id": "s",
            "agent_id": "a",
            "tool_name": "SubagentHandback",
            "tool_input": { "message": long },
        });
        with_probe(OWN, true, |probe, _| {
            let post = build("PostToolUse", input.to_string().as_bytes(), probe).unwrap();
            let HookEvent::SubagentHandback { message, .. } = &post.event else {
                panic!("wrong event");
            };
            assert!(message.len() <= MAX_TEXT);
            assert!(message.ends_with('\u{2026}'));
            let body = serde_json::to_vec(&post).unwrap();
            assert!(body.len() < crate::wire::MAX_HOOK_BODY);
        });
        assert_eq!(cap_text("short".into()), "short");
    }
}
