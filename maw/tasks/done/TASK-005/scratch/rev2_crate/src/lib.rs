//! Pure transcript parsing and rendering primitives.
#![cfg_attr(
    not(test),
    deny(clippy::unwrap_used, clippy::expect_used, clippy::panic)
)]

use serde::Deserialize;
use serde_json::Value;

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
        #[serde(default)]
        text: String,
    },
    #[serde(rename = "tool_use")]
    ToolUse {
        #[serde(default)]
        id: String,
        #[serde(default)]
        name: String,
        #[serde(default)]
        input: Value,
    },
    #[serde(rename = "tool_result")]
    ToolResult {
        #[serde(default)]
        tool_use_id: String,
        #[serde(default)]
        content: Value,
        #[serde(default)]
        is_error: Value,
    },
    #[serde(other)]
    Ignored,
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
    let blocks: Vec<Block> = match record.message?.content {
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
