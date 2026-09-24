//! What one transcript line adds to the live stream of a session's topic.
//!
//! The stream shows what `/brief` shows, one message at a time: prompts typed
//! in the terminal, the text the assistant writes before a tool call, and one
//! line per tool call once its result is in, plus the assistant's visible
//! thinking, cut short. The final answer of a turn is not part of it: the hub
//! sends it from the `Stop` hook. Calls of cctg's own `reply` tool are not
//! shown. Telegram messages taken into work are reported by their
//! `message_id`, never by their text. A `!` command typed in the terminal
//! shows as a prompt `! command`, and its output, like the output of a local
//! slash command (`/cost`, `/model`), as a short code block (TASK-043).

use serde::Deserialize;
use serde_json::Value;
use unicode_segmentation::UnicodeSegmentation;

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
    /// A non-empty `thinking` block of the assistant, trimmed and cut to
    /// [`THINKING_LIMIT`] graphemes (a cut ends with `…`). Redacted and
    /// signature-only thinking gives none.
    Thinking(String),
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
/// The full name of cctg's `reply` tool under that server name. Its text
/// goes to the topic by itself; its call line would repeat it.
const REPLY_CALL: &str = "mcp__cctg__reply";
/// Graphemes of one thinking block the stream shows.
pub const THINKING_LIMIT: usize = 1000;
/// Lines and graphemes of one command output the stream shows.
pub const OUTPUT_LINES: usize = 20;
pub const OUTPUT_LIMIT: usize = 1500;

#[derive(Default, Deserialize)]
#[serde(default)]
struct RawThinkingRecord {
    #[serde(rename = "type")]
    kind: String,
    #[serde(rename = "isSidechain")]
    is_sidechain: Value,
    message: Option<RawThinkingMessage>,
}

#[derive(Default, Deserialize)]
#[serde(default)]
struct RawThinkingMessage {
    content: Value,
}

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
    // A response puts its thinking before its text and tool calls.
    let mut events = thinking(line);
    let Some(turn) = crate::parse(line).into_iter().next() else {
        return events;
    };
    if turn.is_sidechain {
        return Vec::new();
    }
    let mut answer = false;
    for block in &turn.blocks {
        match (turn.role, block) {
            (Role::User, Block::Text(text)) if turn.is_meta => {
                if let Some(message_id) = channel_message_id(text) {
                    events.push(StreamEvent::Channel { message_id });
                }
            }
            (Role::User, Block::Text(text)) => {
                if let Some(event) = console_record(text) {
                    events.extend(event);
                    continue;
                }
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
            (_, Block::ToolUse { name, .. }) if name == REPLY_CALL => {}
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

/// The non-empty `thinking` blocks of a main-transcript assistant record, in
/// block order. `parse` drops thinking, so the record is read again here, only
/// when it can hold some.
fn thinking(line: &str) -> Vec<StreamEvent> {
    if !line.contains("\"thinking\"") {
        return Vec::new();
    }
    let Ok(record) = serde_json::from_str::<RawThinkingRecord>(line) else {
        return Vec::new();
    };
    if record.kind != "assistant" || record.is_sidechain.as_bool().unwrap_or(false) {
        return Vec::new();
    }
    let Some(Value::Array(blocks)) = record.message.map(|message| message.content) else {
        return Vec::new();
    };
    blocks
        .iter()
        .filter(|block| block.get("type").and_then(Value::as_str) == Some("thinking"))
        .filter_map(|block| block.get("thinking").and_then(Value::as_str))
        .map(str::trim)
        .filter(|text| !text.is_empty())
        .map(|text| StreamEvent::Thinking(cut(text, THINKING_LIMIT)))
        .collect()
}

/// Claude Code's own records of the terminal console, `None` for any other
/// text: `<bash-input>` (a `!` command) gives a prompt `! command`;
/// `<bash-stdout>` + `<bash-stderr>` (its output) and
/// `<local-command-stdout>` (the output of a local slash command) give a
/// note with the output as a code block, or nothing when it is blank.
fn console_record(text: &str) -> Option<Option<StreamEvent>> {
    let text = text.trim();
    if text.starts_with("<bash-input>") {
        let command = inside(text, "bash-input")?.trim();
        return Some((!command.is_empty()).then(|| StreamEvent::Prompt(format!("! {command}"))));
    }
    let parts = if text.starts_with("<bash-stdout>") {
        [inside(text, "bash-stdout"), inside(text, "bash-stderr")]
    } else if text.starts_with("<local-command-stdout>") {
        [
            inside(text, "local-command-stdout"),
            inside(text, "local-command-stderr"),
        ]
    } else {
        return None;
    };
    let output = parts
        .into_iter()
        .flatten()
        .map(plain)
        .filter(|part| !part.is_empty())
        .collect::<Vec<_>>()
        .join("\n");
    Some((!output.is_empty()).then(|| StreamEvent::Note(code_block(&output))))
}

/// The text between `<name>` and the next `</name>`.
fn inside<'a>(text: &'a str, name: &str) -> Option<&'a str> {
    let open = format!("<{name}>");
    let start = text.find(&open)? + open.len();
    let len = text[start..].find(&format!("</{name}>"))?;
    Some(&text[start..start + len])
}

/// Terminal output as plain text: ANSI escape sequences (colours) and other
/// control characters but newlines and tabs dropped, surrounding blank space
/// trimmed.
fn plain(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut chars = text.chars().peekable();
    while let Some(c) = chars.next() {
        match c {
            // CSI: `ESC [`, parameters, one final byte in `@`..=`~`.
            '\u{1b}' if chars.peek() == Some(&'[') => {
                chars.next();
                for c in chars.by_ref() {
                    if ('@'..='~').contains(&c) {
                        break;
                    }
                }
            }
            '\n' | '\t' => out.push(c),
            c if c.is_control() => {}
            c => out.push(c),
        }
    }
    out.trim().to_owned()
}

/// `output`, cut to [`OUTPUT_LINES`] lines and [`OUTPUT_LIMIT`] graphemes, as
/// a fenced code block whose fence is longer than any run of backticks in it.
fn code_block(output: &str) -> String {
    let mut lines = output.lines();
    let mut text = lines
        .by_ref()
        .take(OUTPUT_LINES)
        .collect::<Vec<_>>()
        .join("\n");
    let more = lines.next().is_some();
    text = cut(&text, OUTPUT_LIMIT);
    if more && !text.ends_with('\u{2026}') {
        text.push_str("\n\u{2026}");
    }
    let mut longest = 0;
    let mut run = 0;
    for c in text.chars() {
        run = if c == '`' { run + 1 } else { 0 };
        longest = longest.max(run);
    }
    let fence = "`".repeat((longest + 1).max(3));
    format!("{fence}\n{text}\n{fence}")
}

/// `text` cut to `limit` graphemes; a cut drops trailing whitespace and ends
/// with `…`.
fn cut(text: &str, limit: usize) -> String {
    match text.grapheme_indices(true).nth(limit) {
        Some((at, _)) => format!("{}\u{2026}", text[..at].trim_end()),
        None => text.to_owned(),
    }
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
