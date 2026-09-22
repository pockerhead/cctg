use serde::Deserialize;
use serde_json::Value;

#[derive(Debug, PartialEq)]
enum Block {
    Text(String),
    ToolUse { id: String, name: String, input: Value },
    ToolResult { tool_use_id: String, content: String, is_error: bool, agent_id: Option<String> },
}

#[derive(Debug, Default, Deserialize)]
#[serde(default)]
struct RawMessage { content: Value }

#[derive(Debug, Default, Deserialize)]
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

#[derive(Debug, Deserialize)]
#[serde(tag = "type")]
enum RawBlock {
    #[serde(rename = "text")]
    Text { #[serde(default)] text: String },
    #[serde(rename = "tool_use")]
    ToolUse {
        #[serde(default)] id: String,
        #[serde(default)] name: String,
        #[serde(default)] input: Value,
    },
    #[serde(rename = "tool_result")]
    ToolResult {
        #[serde(default)] tool_use_id: String,
        #[serde(default)] content: Value,
        #[serde(default)] is_error: Value,
    },
    #[serde(other)]
    Ignored,
}

fn result_text(value: &Value) -> String {
    match value {
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

fn to_block(value: Value, agent_id: Option<&str>) -> Option<Block> {
    match serde_json::from_value(value).ok()? {
        RawBlock::Text { text } => Some(Block::Text(text)),
        RawBlock::ToolUse { id, name, input } => Some(Block::ToolUse { id, name, input }),
        RawBlock::ToolResult { tool_use_id, content, is_error } => Some(Block::ToolResult {
            tool_use_id,
            content: result_text(&content),
            is_error: is_error.as_bool().unwrap_or(false),
            agent_id: agent_id.map(str::to_owned),
        }),
        RawBlock::Ignored => None,
    }
}

fn parse(jsonl: &str) -> Vec<Block> {
    jsonl.lines().filter_map(|line| {
        let record: RawTurnRecord = serde_json::from_str(line.trim()).ok()?;
        if !matches!(record.kind.as_str(), "user" | "assistant") { return None; }
        let agent_id = record.tool_use_result.get("agentId").and_then(Value::as_str);
        let _flags = (record.is_meta.as_bool().unwrap_or(false), record.is_sidechain.as_bool().unwrap_or(false));
        let blocks = match record.message?.content {
            Value::String(text) => vec![Block::Text(text)],
            Value::Array(items) => items.into_iter().filter_map(|item| to_block(item, agent_id)).collect(),
            _ => Vec::new(),
        };
        (!blocks.is_empty()).then_some(blocks)
    }).flatten().collect()
}

fn main() {
    let fixtures = std::env::args().skip(1);
    for fixture in fixtures {
        let Ok(source) = std::fs::read_to_string(&fixture) else { continue };
        println!("{} {}", fixture, parse(&source).len());
    }
    let record = r#"{"type":"user","message":{"content":"keep me"},"aiTitle":5}"#;
    let block = r#"{"type":"user","message":{"content":[{"type":"text","text":"keep me","id":5}]}}"#;
    println!("counterexamples {} {}", parse(record).len(), parse(block).len());
}
