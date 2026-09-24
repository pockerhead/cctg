//! Transport contracts between the hub and its per-session companions.
//!
//! Agent link: a persistent TCP connection carrying one JSON object per line.
//! Every line has `"v"` (the protocol version) and `"type"`. The agent sends
//! `hello` with the shared secret, then `register`; the hub answers
//! `registered` or `rejected` and closes. Only the agent reconnects.
//!
//! Hook ingress: one HTTP POST of a [`HookPost`] per hook invocation to
//! [`HOOK_PATH`], secret in `Authorization: Bearer`. The hook mints the
//! [`EventId`] once and sends the same one again only when it re-sends the
//! same POST; the hub drops repeats by it.
//!
//! A new optional field (`#[serde(default)]`) keeps [`VERSION`]; so does a new
//! message type that a peer sends only after the other side announced it in
//! such a field (`permission_ack`, see [`Register::verdict_ack`];
//! `transcript_read`, see [`Register::transcript_reads`]). Any other
//! new message type or a changed meaning bumps it. Errors never carry the
//! offending input: a line can contain the secret.

use std::collections::BTreeMap;
use std::collections::hash_map::RandomState;
use std::fmt;
use std::hash::{BuildHasher, Hasher};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use subtle::ConstantTimeEq;
use tokio::io::{AsyncBufRead, AsyncBufReadExt, AsyncReadExt, AsyncWrite, AsyncWriteExt};

pub const VERSION: u32 = 1;
/// Longest accepted agent-link line, newline included.
pub const MAX_LINE: usize = 1 << 20;
/// Longest accepted hook POST body.
pub const MAX_HOOK_BODY: usize = 1 << 20;
pub const HOOK_PATH: &str = "/v1/hook";
pub const MIN_SECRET_LEN: usize = 16;

#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum WireError {
    #[error("line longer than {MAX_LINE} bytes")]
    TooLong,
    #[error("message is not valid for its type")]
    Malformed,
    #[error("unsupported or missing protocol version")]
    Version,
    #[error("unknown message type")]
    UnknownKind,
    #[error("connection closed")]
    Closed,
    #[error("connection error: {0:?}")]
    Io(std::io::ErrorKind),
}

#[derive(Debug, PartialEq, Eq, thiserror::Error)]
pub enum SecretError {
    #[error("the shared secret must be at least {MIN_SECRET_LEN} characters")]
    TooShort,
    #[error("the shared secret may contain only visible ASCII characters (no spaces)")]
    Charset,
}

/// The hub shared secret. `Debug` never prints it; comparison is constant-time.
#[derive(Clone, Serialize, Deserialize)]
#[serde(transparent)]
pub struct Secret(String);

impl Secret {
    /// Trims surrounding whitespace. The value goes into an HTTP header, so it
    /// is limited to visible ASCII.
    pub fn parse(value: &str) -> Result<Self, SecretError> {
        let value = value.trim();
        if !value.bytes().all(|byte| byte.is_ascii_graphic()) {
            return Err(SecretError::Charset);
        }
        if value.len() < MIN_SECRET_LEN {
            return Err(SecretError::TooShort);
        }
        Ok(Self(value.to_owned()))
    }

    pub fn expose(&self) -> &str {
        &self.0
    }

    pub fn matches(&self, candidate: &[u8]) -> bool {
        constant_time_eq(self.0.as_bytes(), candidate)
    }
}

impl PartialEq for Secret {
    fn eq(&self, other: &Self) -> bool {
        self.matches(other.0.as_bytes())
    }
}

impl fmt::Debug for Secret {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("Secret(<redacted>)")
    }
}

/// Leaks only whether the lengths differ, never where the bytes differ.
fn constant_time_eq(a: &[u8], b: &[u8]) -> bool {
    bool::from(a.ct_eq(b))
}

/// Message types a decoder accepts; keeps unknown types apart from bad fields.
pub trait Kinds {
    const KINDS: &'static [&'static str];
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Register {
    /// `CLAUDE_CODE_SESSION_ID` of the Claude Code process that spawned the agent.
    pub session_id: String,
    pub host: String,
    pub cwd: String,
    /// The Claude Code process that spawned the agent. `CLAUDE_CODE_SESSION_ID`
    /// goes stale after `/clear` (the server is not restarted, TASK-013); the
    /// hub follows the process through its `pids` map instead.
    #[serde(default)]
    pub claude_pid: Option<u32>,
    /// The agent answers a `permission_verdict` that carries a `verdict_id`
    /// with `permission_ack`. Agents built before TASK-014 leave it out: the
    /// hub then takes a verdict handed to their link as delivered.
    #[serde(default)]
    pub verdict_ack: bool,
    /// The agent answers `transcript_read` with `transcript_chunk` (TASK-016).
    /// Agents built before leave it out and are never asked.
    #[serde(default)]
    pub transcript_reads: bool,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PermissionRequest {
    pub request_id: String,
    pub tool_name: String,
    pub description: String,
    pub input_preview: String,
}

/// Agent to hub.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum AgentMsg {
    Hello {
        secret: Secret,
    },
    Register(Register),
    /// Text from the channel `reply` tool, for the session's topic.
    Reply {
        text: String,
    },
    PermissionRequest(PermissionRequest),
    /// The agent queued the verdict `verdict_id` for Claude Code. Sent only
    /// to a hub that put the id into the verdict.
    PermissionAck {
        verdict_id: u64,
    },
    /// The answer to one `transcript_read`: the stream events of the complete
    /// lines in `from..to`. `missing`: the file is not there (yet). `more`:
    /// the file has complete lines past `to` that did not fit. `reset`: the
    /// file no longer continues at `from` (it is shorter, or `from` is not
    /// the end of a line): a new file, to be read from its start.
    TranscriptChunk {
        session_id: String,
        from: u64,
        to: u64,
        #[serde(default)]
        lines: Vec<StreamLine>,
        #[serde(default)]
        missing: bool,
        #[serde(default)]
        more: bool,
        #[serde(default)]
        reset: bool,
    },
}

/// The stream events of one transcript line that ends at byte `end`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct StreamLine {
    pub end: u64,
    pub items: Vec<StreamItem>,
}

/// [`transcript::StreamEvent`] on the wire.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum StreamItem {
    Prompt {
        text: String,
    },
    Channel {
        message_id: i64,
    },
    Note {
        text: String,
    },
    Call {
        id: String,
        line: String,
    },
    Result {
        id: String,
        #[serde(default)]
        error: Option<String>,
    },
    /// The assistant text that ends a turn; its text comes from `Stop`.
    TurnEnd,
    /// A kind from a newer agent; skipped.
    #[serde(other)]
    Other,
}

impl Kinds for AgentMsg {
    const KINDS: &'static [&'static str] = &[
        "hello",
        "register",
        "reply",
        "permission_request",
        "permission_ack",
        "transcript_chunk",
    ];
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Rejection {
    Auth,
    Version,
    Protocol,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Behavior {
    Allow,
    Deny,
}

/// Hub to agent.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum HubMsg {
    Registered,
    Rejected {
        reason: Rejection,
    },
    /// A Telegram message for `notifications/claude/channel`.
    Inbound {
        content: String,
        #[serde(default)]
        meta: BTreeMap<String, String>,
    },
    PermissionVerdict {
        request_id: String,
        behavior: Behavior,
        /// Set only for an agent that registered with `verdict_ack`; the same
        /// id comes again when the hub re-sends the same answer.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        verdict_id: Option<u64>,
    },
    /// Sent only to an agent that registered with `transcript_reads`: read
    /// the complete lines of the transcript `path` from byte `from` (`None`:
    /// from its current end) and answer with one `transcript_chunk`.
    TranscriptRead {
        session_id: String,
        path: String,
        #[serde(default)]
        from: Option<u64>,
    },
}

impl Kinds for HubMsg {
    const KINDS: &'static [&'static str] = &[
        "registered",
        "rejected",
        "inbound",
        "permission_verdict",
        "transcript_read",
    ];
}

/// One agent-link line: the message plus `"v"`, newline-terminated.
pub fn encode<T: Serialize>(msg: &T) -> Vec<u8> {
    #[derive(Serialize)]
    struct Framed<'a, T> {
        v: u32,
        #[serde(flatten)]
        msg: &'a T,
    }
    let mut line = serde_json::to_vec(&Framed { v: VERSION, msg })
        .expect("wire messages have string keys and always serialize");
    line.push(b'\n');
    line
}

/// Checks the version, then the type, then the fields.
pub fn decode<T: DeserializeOwned + Kinds>(line: &[u8]) -> Result<T, WireError> {
    let value: Value = serde_json::from_slice(line).map_err(|_| WireError::Malformed)?;
    check_version(&value)?;
    check_kind::<T>(value.get("type"))?;
    serde_json::from_value(value).map_err(|_| WireError::Malformed)
}

fn check_version(value: &Value) -> Result<(), WireError> {
    match value.get("v").and_then(Value::as_u64) {
        Some(v) if v == u64::from(VERSION) => Ok(()),
        _ => Err(WireError::Version),
    }
}

fn check_kind<T: Kinds>(kind: Option<&Value>) -> Result<(), WireError> {
    match kind.and_then(Value::as_str) {
        Some(kind) if T::KINDS.contains(&kind) => Ok(()),
        Some(_) => Err(WireError::UnknownKind),
        None => Err(WireError::Malformed),
    }
}

/// Resumes reading one line into `buf`. Reads at most [`MAX_LINE`] bytes, so
/// a peer that never sends a newline cannot grow `buf` further.
///
/// A cancelled call keeps any bytes already read. After `Ok(())`, the caller
/// must consume the complete line and clear `buf` before starting the next.
pub async fn read_line<R: AsyncBufRead + Unpin>(
    reader: &mut R,
    buf: &mut Vec<u8>,
) -> Result<(), WireError> {
    if buf.last() == Some(&b'\n') {
        return Ok(());
    }
    if buf.len() >= MAX_LINE {
        return Err(WireError::TooLong);
    }
    let remaining = MAX_LINE - buf.len();
    let read = (&mut *reader)
        .take(remaining as u64)
        .read_until(b'\n', buf)
        .await
        .map_err(|error| WireError::Io(error.kind()))?;
    match buf.last() {
        Some(b'\n') => Ok(()),
        _ if read >= remaining => Err(WireError::TooLong),
        _ => Err(WireError::Closed),
    }
}

pub async fn write_msg<W: AsyncWrite + Unpin, T: Serialize>(
    writer: &mut W,
    msg: &T,
) -> Result<(), WireError> {
    let io = |error: std::io::Error| WireError::Io(error.kind());
    writer.write_all(&encode(msg)).await.map_err(io)?;
    writer.flush().await.map_err(io)
}

/// Per-invocation hook event key: 32 lowercase hex digits.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(try_from = "String", into = "String")]
pub struct EventId(String);

impl EventId {
    /// A fresh id. Unique, not secret: requests are authenticated separately.
    pub fn new() -> Self {
        Self(format!("{:016x}{:016x}", random_u64(), random_u64()))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl Default for EventId {
    fn default() -> Self {
        Self::new()
    }
}

impl TryFrom<String> for EventId {
    type Error = &'static str;

    fn try_from(value: String) -> Result<Self, Self::Error> {
        let valid = value.len() == 32
            && value
                .bytes()
                .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte));
        if valid {
            Ok(Self(value))
        } else {
            Err("event_id must be 32 lowercase hex digits")
        }
    }
}

impl From<EventId> for String {
    fn from(id: EventId) -> Self {
        id.0
    }
}

/// 64 bits from a randomly keyed SipHash (the std `RandomState` keys come
/// from the OS generator) over a counter, the clock and the pid. No `rand`
/// crate: ids and jitter need uniqueness, not cryptographic strength.
pub(crate) fn random_u64() -> u64 {
    static COUNTER: AtomicU64 = AtomicU64::new(0);
    let mut hasher = RandomState::new().build_hasher();
    hasher.write_u64(COUNTER.fetch_add(1, Ordering::Relaxed));
    hasher.write_u128(
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|elapsed| elapsed.as_nanos())
            .unwrap_or_default(),
    );
    hasher.write_u32(std::process::id());
    hasher.finish()
}

/// Body of one hook POST. Paths are strings: they may come from another OS.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct HookPost {
    pub v: u32,
    pub event_id: EventId,
    pub host: String,
    pub session_id: String,
    #[serde(default)]
    pub cwd: String,
    #[serde(default)]
    pub transcript_path: String,
    pub event: HookEvent,
}

impl HookPost {
    /// Mints the event id; keep the value to re-send the same POST.
    pub fn new(
        host: String,
        session_id: String,
        cwd: String,
        transcript_path: String,
        event: HookEvent,
    ) -> Self {
        Self {
            v: VERSION,
            event_id: EventId::new(),
            host,
            session_id,
            cwd,
            transcript_path,
            event,
        }
    }
}

/// Event-specific fields; `TASK-012` fills them from the hook stdin.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum HookEvent {
    SessionStart {
        #[serde(default)]
        source: Option<String>,
        /// `CLAUDE_PID` of the hook: the session's own claude process.
        #[serde(default)]
        claude_pid: Option<u32>,
        /// Next `claude` ancestor in the process tree, when there is one.
        #[serde(default)]
        parent_claude_pid: Option<u32>,
    },
    SessionEnd {
        #[serde(default)]
        reason: Option<String>,
        /// `CLAUDE_PID` of the hook. Tells the end of a nested
        /// `claude -p --resume <id>` from the end of the session itself.
        #[serde(default)]
        claude_pid: Option<u32>,
    },
    UserPromptSubmit {
        #[serde(default)]
        prompt_id: Option<String>,
    },
    Stop {
        #[serde(default)]
        prompt_id: Option<String>,
        #[serde(default)]
        last_assistant_message: Option<String>,
    },
    SubagentStart {
        agent_id: String,
        #[serde(default)]
        agent_type: String,
    },
    SubagentStop {
        agent_id: String,
        #[serde(default)]
        agent_type: String,
        #[serde(default)]
        agent_transcript_path: Option<String>,
        #[serde(default)]
        last_assistant_message: Option<String>,
    },
    /// `SubagentHandback` report, taken from `PreToolUse`/`PostToolUse`.
    SubagentHandback { agent_id: String, message: String },
}

impl HookEvent {
    pub fn kind(&self) -> &'static str {
        match self {
            Self::SessionStart { .. } => "session_start",
            Self::SessionEnd { .. } => "session_end",
            Self::UserPromptSubmit { .. } => "user_prompt_submit",
            Self::Stop { .. } => "stop",
            Self::SubagentStart { .. } => "subagent_start",
            Self::SubagentStop { .. } => "subagent_stop",
            Self::SubagentHandback { .. } => "subagent_handback",
        }
    }
}

impl Kinds for HookEvent {
    const KINDS: &'static [&'static str] = &[
        "session_start",
        "session_end",
        "user_prompt_submit",
        "stop",
        "subagent_start",
        "subagent_stop",
        "subagent_handback",
    ];
}

pub fn decode_hook(body: &[u8]) -> Result<HookPost, WireError> {
    let value: Value = serde_json::from_slice(body).map_err(|_| WireError::Malformed)?;
    check_version(&value)?;
    check_kind::<HookEvent>(value.get("event").and_then(|event| event.get("type")))?;
    serde_json::from_value(value).map_err(|_| WireError::Malformed)
}

#[cfg(test)]
mod tests {
    use std::collections::HashSet;

    use serde_json::json;

    use super::*;

    const SECRET: &str = "0123456789abcdef-secret";

    fn secret() -> Secret {
        Secret::parse(SECRET).unwrap()
    }

    fn agent_samples() -> Vec<AgentMsg> {
        vec![
            AgentMsg::Hello { secret: secret() },
            AgentMsg::Register(Register {
                session_id: "5e551017-0000-4000-8000-000000000001".into(),
                host: "box".into(),
                cwd: "C:\\work\\app".into(),
                claude_pid: Some(4242),
                verdict_ack: true,
                transcript_reads: true,
            }),
            AgentMsg::Reply {
                text: "multi\nline \u{2014} text".into(),
            },
            AgentMsg::PermissionRequest(PermissionRequest {
                request_id: "abcde".into(),
                tool_name: "Bash".into(),
                description: "run tests".into(),
                input_preview: "{\"command\":\"cargo test\"}".into(),
            }),
            AgentMsg::PermissionAck {
                verdict_id: u64::MAX,
            },
            AgentMsg::TranscriptChunk {
                session_id: "s".into(),
                from: 10,
                to: 20,
                lines: vec![StreamLine {
                    end: 20,
                    items: vec![
                        StreamItem::Prompt { text: "p".into() },
                        StreamItem::Channel { message_id: 7 },
                        StreamItem::Note { text: "n".into() },
                        StreamItem::Call {
                            id: "t1".into(),
                            line: "• Bash: x".into(),
                        },
                        StreamItem::Result {
                            id: "t1".into(),
                            error: Some("boom".into()),
                        },
                        StreamItem::TurnEnd,
                    ],
                }],
                missing: false,
                more: true,
                reset: false,
            },
        ]
    }

    fn hub_samples() -> Vec<HubMsg> {
        vec![
            HubMsg::Registered,
            HubMsg::Rejected {
                reason: Rejection::Auth,
            },
            HubMsg::Rejected {
                reason: Rejection::Version,
            },
            HubMsg::Rejected {
                reason: Rejection::Protocol,
            },
            HubMsg::Inbound {
                content: "hi".into(),
                meta: [("target_agent".to_owned(), "a1".to_owned())].into(),
            },
            HubMsg::PermissionVerdict {
                request_id: "abcde".into(),
                behavior: Behavior::Allow,
                verdict_id: None,
            },
            HubMsg::PermissionVerdict {
                request_id: "abcde".into(),
                behavior: Behavior::Deny,
                verdict_id: Some(u64::MAX),
            },
            HubMsg::TranscriptRead {
                session_id: "s".into(),
                path: "/p/s.jsonl".into(),
                from: None,
            },
        ]
    }

    fn hook_samples() -> Vec<HookEvent> {
        vec![
            HookEvent::SessionStart {
                source: Some("resume".into()),
                claude_pid: Some(10),
                parent_claude_pid: None,
            },
            HookEvent::SessionEnd {
                reason: Some("prompt_input_exit".into()),
                claude_pid: Some(10),
            },
            HookEvent::UserPromptSubmit {
                prompt_id: Some("p1".into()),
            },
            HookEvent::Stop {
                prompt_id: Some("p1".into()),
                last_assistant_message: Some("done".into()),
            },
            HookEvent::SubagentStart {
                agent_id: "a1".into(),
                agent_type: "Explore".into(),
            },
            HookEvent::SubagentStop {
                agent_id: "a1".into(),
                agent_type: "Explore".into(),
                agent_transcript_path: Some("/x/agent-a1.jsonl".into()),
                last_assistant_message: None,
            },
            HookEvent::SubagentHandback {
                agent_id: "a1".into(),
                message: "report".into(),
            },
        ]
    }

    fn kind_of(line: &[u8]) -> String {
        let value: Value = serde_json::from_slice(line).unwrap();
        value["type"].as_str().unwrap().to_owned()
    }

    #[test]
    fn every_link_message_round_trips_as_one_versioned_line() {
        for msg in agent_samples() {
            let line = encode(&msg);
            assert_eq!(line.iter().filter(|&&b| b == b'\n').count(), 1);
            assert!(line.ends_with(b"\n"));
            assert_eq!(serde_json::from_slice::<Value>(&line).unwrap()["v"], 1);
            assert_eq!(decode::<AgentMsg>(&line), Ok(msg));
        }
        for msg in hub_samples() {
            assert_eq!(decode::<HubMsg>(&encode(&msg)), Ok(msg));
        }
    }

    #[test]
    fn kind_lists_match_the_enums() {
        let agent: HashSet<String> = agent_samples()
            .iter()
            .map(|m| kind_of(&encode(m)))
            .collect();
        assert_eq!(
            agent,
            AgentMsg::KINDS.iter().map(|k| k.to_string()).collect()
        );
        let hub: HashSet<String> = hub_samples().iter().map(|m| kind_of(&encode(m))).collect();
        assert_eq!(hub, HubMsg::KINDS.iter().map(|k| k.to_string()).collect());
        let hooks: Vec<&str> = hook_samples().iter().map(HookEvent::kind).collect();
        assert_eq!(hooks, HookEvent::KINDS);
        for event in hook_samples() {
            let value = serde_json::to_value(&event).unwrap();
            assert_eq!(value["type"], event.kind());
        }
    }

    #[test]
    fn bad_lines_are_typed_errors() {
        let cases: [(&[u8], WireError); 8] = [
            (br#"{"v":2,"type":"reply","text":"x"}"#, WireError::Version),
            (br#"{"type":"reply","text":"x"}"#, WireError::Version),
            (
                br#"{"v":"1","type":"reply","text":"x"}"#,
                WireError::Version,
            ),
            (
                br#"{"v":1,"type":"teleport","text":"x"}"#,
                WireError::UnknownKind,
            ),
            (br#"{"v":1,"text":"x"}"#, WireError::Malformed),
            (br#"{"v":1,"type":"reply"}"#, WireError::Malformed),
            (b"not json at all", WireError::Malformed),
            (b"", WireError::Malformed),
        ];
        for (line, want) in cases {
            assert_eq!(
                decode::<AgentMsg>(line),
                Err(want),
                "{}",
                String::from_utf8_lossy(line)
            );
        }
        assert_eq!(
            decode::<HubMsg>(br#"{"v":1,"type":"hello","secret":"x"}"#),
            Err(WireError::UnknownKind)
        );
    }

    #[test]
    fn a_register_without_claude_pid_still_decodes() {
        let line = br#"{"v":1,"type":"register","session_id":"s","host":"h","cwd":"/w"}"#;
        assert_eq!(
            decode::<AgentMsg>(line),
            Ok(AgentMsg::Register(Register {
                session_id: "s".into(),
                host: "h".into(),
                cwd: "/w".into(),
                claude_pid: None,
                verdict_ack: false,
                transcript_reads: false,
            }))
        );
    }

    #[test]
    fn verdict_acks_stay_compatible_with_version_one_peers() {
        // A hub before TASK-014 sends no id; an agent before it reads past one.
        let legacy =
            br#"{"v":1,"type":"permission_verdict","request_id":"abcde","behavior":"allow"}"#;
        assert_eq!(
            decode::<HubMsg>(legacy),
            Ok(HubMsg::PermissionVerdict {
                request_id: "abcde".into(),
                behavior: Behavior::Allow,
                verdict_id: None,
            })
        );
        let without_id = encode(&HubMsg::PermissionVerdict {
            request_id: "abcde".into(),
            behavior: Behavior::Allow,
            verdict_id: None,
        });
        assert_eq!(without_id, [&legacy[..], b"\n"].concat());
        let with_id = encode(&HubMsg::PermissionVerdict {
            request_id: "abcde".into(),
            behavior: Behavior::Deny,
            verdict_id: Some(7),
        });
        let value: Value = serde_json::from_slice(&with_id).unwrap();
        assert_eq!(value["v"], 1);
        assert_eq!(value["verdict_id"], 7);
        let line = br#"{"v":1,"type":"register","session_id":"s","host":"h","cwd":"/w","verdict_ack":true}"#;
        assert!(matches!(
            decode::<AgentMsg>(line),
            Ok(AgentMsg::Register(Register {
                verdict_ack: true,
                ..
            }))
        ));
        assert_eq!(
            decode::<AgentMsg>(br#"{"v":1,"type":"permission_ack"}"#),
            Err(WireError::Malformed)
        );
    }

    #[test]
    fn transcript_chunks_stay_readable_across_agent_versions() {
        // A kind from a newer agent is skipped, not a broken chunk.
        let line = br#"{"v":1,"type":"transcript_chunk","session_id":"s","from":0,"to":9,"lines":[{"end":9,"items":[{"kind":"diff","x":1},{"kind":"channel","message_id":3}]}]}"#;
        assert_eq!(
            decode::<AgentMsg>(line),
            Ok(AgentMsg::TranscriptChunk {
                session_id: "s".into(),
                from: 0,
                to: 9,
                lines: vec![StreamLine {
                    end: 9,
                    items: vec![StreamItem::Other, StreamItem::Channel { message_id: 3 }],
                }],
                missing: false,
                more: false,
                reset: false,
            })
        );
        let read = br#"{"v":1,"type":"transcript_read","session_id":"s","path":"/p"}"#;
        assert!(matches!(
            decode::<HubMsg>(read),
            Ok(HubMsg::TranscriptRead { from: None, .. })
        ));
    }

    #[test]
    fn unknown_fields_are_ignored() {
        let line = br#"{"v":1,"type":"registered","extra":{"a":1}}"#;
        assert_eq!(decode::<HubMsg>(line), Ok(HubMsg::Registered));
        let line = br#"{"v":1,"type":"reply","text":"t","later":true}"#;
        assert_eq!(
            decode::<AgentMsg>(line),
            Ok(AgentMsg::Reply { text: "t".into() })
        );
    }

    #[test]
    fn errors_and_debug_never_show_the_secret() {
        let line = format!(r#"{{"v":1,"type":"register","session_id":7,"host":"{SECRET}"}}"#);
        let error = decode::<AgentMsg>(line.as_bytes()).unwrap_err();
        assert_eq!(error, WireError::Malformed);
        assert!(!format!("{error} {error:?}").contains(SECRET));
        let hello = AgentMsg::Hello { secret: secret() };
        assert!(!format!("{hello:?}").contains(SECRET));
        assert!(!format!("{:?}", secret()).contains(SECRET));
    }

    #[test]
    fn secret_rules_and_comparison() {
        assert_eq!(Secret::parse("short"), Err(SecretError::TooShort));
        assert_eq!(
            Secret::parse("has a space in it!!"),
            Err(SecretError::Charset)
        );
        assert_eq!(
            Secret::parse("кириллица-кириллица"),
            Err(SecretError::Charset)
        );
        let parsed = Secret::parse(&format!("  {SECRET}\n")).unwrap();
        assert_eq!(parsed.expose(), SECRET);
        assert!(parsed.matches(SECRET.as_bytes()));
        assert!(!parsed.matches(&SECRET.as_bytes()[1..]));
        assert!(!parsed.matches(b"0123456789abcdef-secreT"));
        assert!(!parsed.matches(b""));
    }

    #[tokio::test]
    async fn lines_are_read_one_at_a_time() {
        let mut reader = tokio::io::BufReader::new(&b"{\"a\":1}\n{\"b\":2}\ntail"[..]);
        let mut buf = Vec::new();
        read_line(&mut reader, &mut buf).await.unwrap();
        assert_eq!(buf, b"{\"a\":1}\n");
        buf.clear();
        read_line(&mut reader, &mut buf).await.unwrap();
        assert_eq!(buf, b"{\"b\":2}\n");
        buf.clear();
        // A line cut by EOF is a closed connection, not a message.
        assert_eq!(
            read_line(&mut reader, &mut buf).await,
            Err(WireError::Closed)
        );
        assert_eq!(
            read_line(&mut reader, &mut buf).await,
            Err(WireError::Closed)
        );
    }

    #[tokio::test]
    async fn a_cancelled_read_keeps_its_partial_line() {
        let (mut write, read) = tokio::io::duplex(64);
        let mut reader = tokio::io::BufReader::new(read);
        let mut buf = Vec::new();

        write.write_all(b"{\"a\":").await.unwrap();
        assert!(
            tokio::time::timeout(
                std::time::Duration::from_millis(20),
                read_line(&mut reader, &mut buf),
            )
            .await
            .is_err()
        );
        assert_eq!(buf, b"{\"a\":");

        write.write_all(b"1}\n").await.unwrap();
        read_line(&mut reader, &mut buf).await.unwrap();
        assert_eq!(buf, b"{\"a\":1}\n");
    }

    #[tokio::test]
    async fn an_endless_line_stops_at_the_limit() {
        // `repeat` never ends and never sends a newline.
        let mut reader = tokio::io::BufReader::new(tokio::io::repeat(b'a'));
        let mut buf = Vec::new();
        assert_eq!(
            read_line(&mut reader, &mut buf).await,
            Err(WireError::TooLong)
        );
        assert_eq!(buf.len(), MAX_LINE);
        assert!(buf.capacity() <= 2 * MAX_LINE, "{}", buf.capacity());

        let mut exact = vec![b'a'; MAX_LINE - 1];
        exact.push(b'\n');
        let mut reader = tokio::io::BufReader::new(&exact[..]);
        buf.clear();
        read_line(&mut reader, &mut buf).await.unwrap();
        assert_eq!(buf.len(), MAX_LINE);
    }

    #[test]
    fn hook_posts_round_trip_and_keep_their_event_id() {
        for event in hook_samples() {
            let post = HookPost::new(
                "box".into(),
                "s1".into(),
                "/w".into(),
                "/w/s1.jsonl".into(),
                event,
            );
            let body = serde_json::to_vec(&post).unwrap();
            let decoded = decode_hook(&body).unwrap();
            assert_eq!(decoded.event_id, post.event_id);
            assert_eq!(decoded, post);
        }
    }

    #[test]
    fn bad_hook_bodies_are_typed_errors() {
        let id = EventId::new();
        let body = |v: Value| serde_json::to_vec(&v).unwrap();
        let base = |event: Value| json!({ "v": 1, "event_id": id.as_str(), "host": "h", "session_id": "s", "event": event });
        assert_eq!(
            decode_hook(&body(base(json!({ "type": "session_end" })))).map(|p| p.event),
            Ok(HookEvent::SessionEnd {
                reason: None,
                claude_pid: None,
            })
        );
        let mut v2 = base(json!({ "type": "stop" }));
        v2["v"] = json!(2);
        assert_eq!(decode_hook(&body(v2)), Err(WireError::Version));
        assert_eq!(
            decode_hook(&body(base(json!({ "type": "pre_compact" })))),
            Err(WireError::UnknownKind)
        );
        assert_eq!(
            decode_hook(&body(base(json!({ "type": "subagent_start" })))),
            Err(WireError::Malformed)
        );
        let mut bad_id = base(json!({ "type": "stop" }));
        bad_id["event_id"] = json!("session-5e551017");
        assert_eq!(decode_hook(&body(bad_id)), Err(WireError::Malformed));
        assert_eq!(decode_hook(b"{"), Err(WireError::Malformed));
    }

    #[test]
    fn event_ids_are_fresh_hex() {
        let ids: HashSet<EventId> = (0..10_000).map(|_| EventId::new()).collect();
        assert_eq!(ids.len(), 10_000);
        for id in ids.iter().take(10) {
            assert!(EventId::try_from(id.as_str().to_owned()).is_ok());
        }
        assert!(EventId::try_from("A".repeat(32)).is_err());
        assert!(EventId::try_from("a".repeat(31)).is_err());
    }
}
