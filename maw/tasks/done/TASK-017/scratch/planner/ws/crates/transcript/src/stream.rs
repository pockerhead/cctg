//! What one transcript line adds to the live stream of a session's topic.
//!
//! The stream shows what `/brief` shows, one message at a time: prompts typed
//! in the terminal, the text the assistant writes before a tool call, and one
//! line per tool call once its result is in. The final answer of a turn is not
//! part of it: the hub sends it from the `Stop` hook. Telegram messages taken
//! into work are reported by their `message_id`, never by their text.

use serde::Deserialize;
use serde_json::Value;

use crate::render::{self, UserText};
use crate::{Block, Role};

/// One event of a transcript line, in the order of the line's blocks.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum StreamEvent {
    /// A prompt typed in the terminal or a slash command, shown as `/brief`
    /// shows it (without the `> `).
    Prompt(String),
    /// A Telegram message went into the session: the `message_id` attribute of
    /// its `<channel ...>` tag.
    Channel { message_id: i64 },
    /// Assistant text written before a tool call (`stop_reason: tool_use`),
    /// or Claude Code's `[Request interrupted by user...]` line.
    Note(String),
    /// A tool call and its `/brief` line (`• Bash: ...`, `↳ Explore: ...`).
    Call { id: String, line: String },
    /// The result of a tool call; `error` is set for a failed call and holds
    /// its first line (possibly empty).
    Result { id: String, error: Option<String> },
    /// Assistant text that ends a turn (`stop_reason` set and not
    /// `tool_use`). Its text is not carried: the `Stop` hook sends it.
    TurnEnd,
}

/// The `source` of cctg's channel tags: the server name cctg is registered
/// under (`cctg agent-install`, `docs/poc.md`).
const SOURCE: &str = "cctg";

#[derive(Default, Deserialize)]
#[serde(default)]
struct RawAttachmentRecord {
    #[serde(rename = "type")]
    kind: String,
    attachment: Value,
}

/// The events of one jsonl line. Anything that is not a main-transcript
/// `user`/`assistant` record or a queued channel message gives none; a bad
/// line gives none.
pub fn stream_events(line: &str) -> Vec<StreamEvent> {
    // A UTF-8 BOM is not JSON whitespace; a file written with one keeps it.
    let line = line.trim().trim_start_matches('\u{feff}');
    if line.is_empty() {
        return Vec::new();
    }
    if let Some(message_id) = queued_channel(line) {
        return vec![StreamEvent::Channel { message_id }];
    }
    let Some(turn) = crate::parse(line).into_iter().next() else {
        return Vec::new();
    };
    if turn.is_sidechain {
        return Vec::new();
    }
    let mut events = Vec::new();
    let mut answer = false;
    for block in &turn.blocks {
        match (turn.role, block) {
            (Role::User, Block::Text(text)) if turn.is_meta => {
                if let Some(message_id) = channel_message_id(text) {
                    events.push(StreamEvent::Channel { message_id });
                }
            }
            (Role::User, Block::Text(text)) => {
                match render::user_text(&turn, text) {
                    // Claude Code's own line after Esc: not a prompt, and no
                    // new turn starts with it.
                    Some(UserText::Prompt(prompt))
                        if prompt.starts_with(render::INTERRUPT_PREFIX) =>
                    {
                        events.push(StreamEvent::Note(prompt.into_owned()));
                    }
                    Some(UserText::Prompt(prompt)) => {
                        events.push(StreamEvent::Prompt(prompt.into_owned()));
                    }
                    _ => {}
                }
            }
            (Role::Assistant, Block::Text(text)) => {
                let text = text.trim();
                match turn.stop_reason.as_deref() {
                    Some("tool_use") if !text.is_empty() => {
                        events.push(StreamEvent::Note(text.to_owned()));
                    }
                    Some("tool_use") | None => {}
                    Some(_) => answer = true,
                }
            }
            (_, Block::ToolUse { id, name, input }) => events.push(StreamEvent::Call {
                id: id.clone(),
                line: render::tool_line(name, input, None, None),
            }),
            (
                _,
                Block::ToolResult {
                    tool_use_id,
                    content,
                    is_error,
                    ..
                },
            ) => events.push(StreamEvent::Result {
                id: tool_use_id.clone(),
                error: is_error.then(|| error_line(content)),
            }),
        }
    }
    if answer {
        events.push(StreamEvent::TurnEnd);
    }
    events
}

/// A channel message queued while a turn ran reaches Claude as a
/// `queued_command` attachment (the shape Claude Code uses for other queued
/// prompts; not yet seen for a channel message).
fn queued_channel(line: &str) -> Option<i64> {
    let record: RawAttachmentRecord = serde_json::from_str(line).ok()?;
    if record.kind != "attachment"
        || record.attachment.get("type").and_then(Value::as_str) != Some("queued_command")
    {
        return None;
    }
    channel_message_id(record.attachment.get("prompt")?.as_str()?)
}

/// `message_id` of a `<channel source="cctg" ...>` opening tag, when it is
/// all digits. Another channel server's tag never counts, whatever its ids.
fn channel_message_id(text: &str) -> Option<i64> {
    if channel_attribute(text, "source")? != SOURCE {
        return None;
    }
    let id = channel_attribute(text, "message_id")?;
    if id.is_empty() || !id.bytes().all(|byte| byte.is_ascii_digit()) {
        return None;
    }
    id.parse().ok()
}

/// The value of attribute `name` of the `<channel ...>` tag that starts
/// `text`. Only the opening tag is read: the message body can hold anything.
fn channel_attribute<'a>(text: &'a str, name: &str) -> Option<&'a str> {
    let mut rest = text.trim_start().strip_prefix("<channel")?;
    if !rest.starts_with(char::is_whitespace) {
        return None;
    }
    loop {
        rest = rest.trim_start();
        let (key, after) = rest.split_once('=')?;
        if key.is_empty() || key.contains(['>', '<']) || key.contains(char::is_whitespace) {
            return None;
        }
        let quote = after.chars().next().filter(|c| *c == '"' || *c == '\'')?;
        let body = &after[1..];
        let (value, tail) = body.split_once(quote)?;
        if key == name {
            return Some(value);
        }
        rest = tail;
    }
}

/// First non-empty line of a failed result, without Claude Code's
/// `<tool_use_error>` wrapper, cut like a `/brief` summary.
fn error_line(content: &str) -> String {
    let text = content.trim();
    let text = text.strip_prefix("<tool_use_error>").unwrap_or(text);
    let text = text.strip_suffix("</tool_use_error>").unwrap_or(text);
    let first = text.lines().map(str::trim).find(|line| !line.is_empty());
    render::one_line(first.unwrap_or_default())
}
