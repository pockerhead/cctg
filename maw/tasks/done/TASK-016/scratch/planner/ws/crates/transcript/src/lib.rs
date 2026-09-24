//! Pure transcript parsing and rendering primitives.
#![forbid(unsafe_code)]
#![cfg_attr(
    not(test),
    deny(clippy::unwrap_used, clippy::expect_used, clippy::panic)
)]

use serde::Deserialize;
use serde_json::Value;

mod render;
mod split;
mod stream;
mod subagent;

pub use render::{
    last_prompts, render_brief, render_brief_with_subagents, render_full,
    render_full_with_subagents,
};
pub use split::{SplitOptions, SplitResult, TELEGRAM_TEXT_LIMIT, split_for_telegram, telegram_len};
pub use stream::{StreamEvent, stream_events};
pub use subagent::{Subagent, SubagentBody, SubagentInput, SubagentMeta, parse_subagent_meta};

/// Author of a turn, taken from the record's top-level `type`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Role {
    User,
    Assistant,
}

/// A visible content block. Thinking blocks are intentionally not representable.
#[derive(Debug, Clone, PartialEq)]
pub enum Block {
    /// Plain text; a string `message.content` becomes exactly one `Text`.
    Text(String),
    /// A tool call with its raw JSON input.
    ToolUse {
        id: String,
        name: String,
        input: Value,
    },
    /// A tool result.
    ToolResult {
        tool_use_id: String,
        /// Result text; for array content, the `text` items joined with `\n`, other items skipped.
        content: String,
        /// True only when the record says `"is_error": true`.
        is_error: bool,
        /// Id of the subagent spawned by an `Agent` call, from the record-level `toolUseResult.agentId`.
        agent_id: Option<String>,
    },
}

/// One `user` or `assistant` jsonl record. One API response may span several turns.
#[derive(Debug, Clone, PartialEq)]
pub struct Turn {
    pub role: Role,
    /// Never empty.
    pub blocks: Vec<Block>,
    pub is_meta: bool,
    pub is_sidechain: bool,
    /// `message.stop_reason` when it is a string (`end_turn`, `tool_use`, ...); `None` when absent or null.
    /// Subagent transcripts set it only on the last record of a response.
    pub stop_reason: Option<String>,
}

#[derive(Default, Deserialize)]
#[serde(default)]
struct RawTurnRecord {
    #[serde(rename = "type")]
    kind: String,
    message: Option<RawMessage>,
    #[serde(rename = "isMeta")]
    is_meta: Value,
    #[serde(rename = "isSidechain")]
    is_sidechain: Value,
    #[serde(rename = "toolUseResult")]
    tool_use_result: Value,
}

#[derive(Default, Deserialize)]
#[serde(default)]
struct RawMessage {
    content: Value,
    stop_reason: Value,
}

#[derive(Default, Deserialize)]
#[serde(default)]
struct RawTitleRecord {
    #[serde(rename = "type")]
    kind: String,
    #[serde(rename = "aiTitle")]
    ai_title: Value,
}

#[derive(Deserialize)]
#[serde(tag = "type")]
enum RawBlock {
    #[serde(rename = "text")]
    Text {
        #[serde(default, deserialize_with = "string_or_default")]
        text: String,
    },
    #[serde(rename = "tool_use")]
    ToolUse {
        #[serde(default, deserialize_with = "string_or_default")]
        id: String,
        #[serde(default, deserialize_with = "string_or_default")]
        name: String,
        #[serde(default)]
        input: Value,
    },
    #[serde(rename = "tool_result")]
    ToolResult {
        #[serde(default, deserialize_with = "string_or_default")]
        tool_use_id: String,
        #[serde(default)]
        content: Value,
        #[serde(default)]
        is_error: Value,
    },
    #[serde(other)]
    Ignored,
}

fn string_or_default<'de, D>(deserializer: D) -> Result<String, D::Error>
where
    D: serde::Deserializer<'de>,
{
    Option::<String>::deserialize(deserializer).map(Option::unwrap_or_default)
}

/// Parses jsonl text into turns. Bad lines, other record types and unknown blocks are skipped.
pub fn parse(jsonl: &str) -> Vec<Turn> {
    records::<RawTurnRecord>(jsonl)
        .filter_map(to_turn)
        .collect()
}

/// Returns the first non-empty `aiTitle` of an `ai-title` record.
pub fn ai_title(jsonl: &str) -> Option<String> {
    records::<RawTitleRecord>(jsonl)
        .filter(|record| record.kind == "ai-title")
        .find_map(|record| match record.ai_title {
            Value::String(title) if !title.is_empty() => Some(title),
            _ => None,
        })
}

fn records<'a, T: Deserialize<'a>>(jsonl: &'a str) -> impl Iterator<Item = T> + 'a {
    jsonl
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty())
        .filter_map(|line| serde_json::from_str(line).ok())
}

fn to_turn(record: RawTurnRecord) -> Option<Turn> {
    let role = match record.kind.as_str() {
        "user" => Role::User,
        "assistant" => Role::Assistant,
        _ => return None,
    };
    let agent_id = record
        .tool_use_result
        .get("agentId")
        .and_then(Value::as_str);
    let message = record.message?;
    let stop_reason = match message.stop_reason {
        Value::String(reason) => Some(reason),
        _ => None,
    };
    let blocks: Vec<Block> = match message.content {
        Value::String(text) => vec![Block::Text(text)],
        Value::Array(items) => items
            .into_iter()
            .filter_map(|item| to_block(item, agent_id))
            .collect(),
        _ => Vec::new(),
    };
    if blocks.is_empty() {
        return None;
    }
    Some(Turn {
        role,
        blocks,
        is_meta: record.is_meta.as_bool().unwrap_or(false),
        is_sidechain: record.is_sidechain.as_bool().unwrap_or(false),
        stop_reason,
    })
}

fn to_block(item: Value, agent_id: Option<&str>) -> Option<Block> {
    match serde_json::from_value(item).ok()? {
        RawBlock::Text { text } => Some(Block::Text(text)),
        RawBlock::ToolUse { id, name, input } => Some(Block::ToolUse { id, name, input }),
        RawBlock::ToolResult {
            tool_use_id,
            content,
            is_error,
        } => Some(Block::ToolResult {
            tool_use_id,
            content: result_text(&content),
            is_error: is_error.as_bool().unwrap_or(false),
            agent_id: agent_id.map(str::to_owned),
        }),
        RawBlock::Ignored => None,
    }
}

fn result_text(content: &Value) -> String {
    match content {
        Value::String(text) => text.clone(),
        Value::Array(items) => items
            .iter()
            .filter(|item| item.get("type").and_then(Value::as_str) == Some("text"))
            .filter_map(|item| item.get("text").and_then(Value::as_str))
            .collect::<Vec<_>>()
            .join("\n"),
        _ => String::new(),
    }
}
