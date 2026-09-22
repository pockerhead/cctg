#![deny(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
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

fn rb(s: &str) -> String {
    let v: Value = match serde_json::from_str(s) { Ok(v) => v, Err(e) => return format!("value err {e}") };
    match serde_json::from_value::<RawBlock>(v) { Ok(b) => format!("{b:?}"), Err(e) => format!("ERR {e}") }
}

fn nested(depth: usize) -> String {
    format!(r#"{{"type":"assistant","message":{{"content":[{{"type":"tool_use","id":"x","name":"n","input":{}{}}}]}}}}"#, "[".repeat(depth), "]".repeat(depth))
}

fn main() {
    for s in [
        r#"{"type":"text","text":"keep me","id":5}"#,
        r#"{"type":"text","text":5}"#,
        r#"{"type":"thinking","thinking":"SECRET","signature":"S"}"#,
        r#"{"type":"redacted_thinking","data":"x"}"#,
        r#"{"text":"no tag"}"#,
        r#"{"type":7}"#,
        r#"{"type":null}"#,
        r#"{"type":"text"}"#,
        r#"{"type":"tool_result","tool_use_id":"t","content":[{"type":"text","text":"a"},{"type":"tool_reference","tool_name":"x"},{"type":"text","text":"b"}],"is_error":"yes"}"#,
        r#"{"type":"tool_use","id":"t","name":"Bash","input":{"a":[1,2.5,-3,18446744073709551615,null,true]}}"#,
        r#"["text","positional"]"#,
        r#"{"text":"x","type":"text"}"#,
    ] { println!("{s}\n  -> {}", rb(s)); }
    let ok = r#"{"type":"user","message":{"content":"ok"}}"#;
    for d in [100usize, 120, 125, 126, 127, 128, 200, 100_000] {
        let input = format!("{ok}\n{}\n{ok}", nested(d));
        let h = std::thread::Builder::new().stack_size(2 << 20).spawn(move || parse(&input).len()).map(|h| h.join());
        println!("depth {d}: {:?}", h.map(|r| r.ok()).ok());
    }
    // main thread (1 MiB on Windows)
    println!("main depth 126: {}", parse(&nested(126)).len());
    // positional/array record curiosity
    println!("array record: {}", parse(r#"["user",{"content":"sneaky"}]"#).len());
    println!("message null: {}", parse(r#"{"type":"user","message":null}"#).len());
    println!("message 5: {}", parse(r#"{"type":"user","message":5}"#).len());
    println!("bom: {}", parse("\u{feff}{\"type\":\"user\",\"message\":{\"content\":\"x\"}}").len());
    println!("isMeta str: {}", parse(r#"{"type":"user","isMeta":"true","message":{"content":"x"}}"#).len());
    println!("type 5: {}", parse(r#"{"type":5,"message":{"content":"x"}}"#).len());
    println!("dup key: {}", parse(r#"{"type":"user","type":"assistant","message":{"content":"x"}}"#).len());
    println!("value eq: {}", std::any::type_name::<Value>());
}
