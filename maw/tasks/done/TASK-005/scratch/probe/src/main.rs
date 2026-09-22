// Read-only probe of the planned parser design against real transcripts and fixtures.
// Reads files given on argv, prints only structural counts. Not project code.
use serde::Deserialize;
use serde_json::Value;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Role { User, Assistant }

#[derive(Debug, Clone, PartialEq)]
pub enum Block {
    Text(String),
    ToolUse { id: String, name: String, input: Value },
    ToolResult { tool_use_id: String, content: String, is_error: bool, agent_id: Option<String> },
}

#[derive(Debug, Clone, PartialEq)]
pub struct Turn { pub role: Role, pub blocks: Vec<Block>, pub is_meta: bool, pub is_sidechain: bool }

#[derive(Deserialize, Default)]
#[serde(default)]
struct RawRecord {
    #[serde(rename = "type")] kind: String,
    message: Option<RawMessage>,
    #[serde(rename = "isMeta")] is_meta: bool,
    #[serde(rename = "isSidechain")] is_sidechain: bool,
    #[serde(rename = "toolUseResult")] tool_use_result: Value,
    #[serde(rename = "aiTitle")] ai_title: Option<String>,
}

#[derive(Deserialize, Default)]
#[serde(default)]
struct RawMessage { content: Value }

#[derive(Deserialize, Default)]
#[serde(default)]
struct RawBlock {
    #[serde(rename = "type")] kind: String,
    text: String,
    id: String,
    name: String,
    input: Value,
    tool_use_id: String,
    content: Value,
    is_error: Option<bool>,
}

fn result_text(v: &Value) -> String {
    match v {
        Value::String(s) => s.clone(),
        Value::Array(items) => items.iter()
            .filter(|i| i.get("type").and_then(Value::as_str) == Some("text"))
            .filter_map(|i| i.get("text").and_then(Value::as_str))
            .collect::<Vec<_>>().join("\n"),
        _ => String::new(),
    }
}

fn to_block(v: Value, agent_id: &Option<String>) -> Option<Block> {
    let b = RawBlock::deserialize(v).ok()?;
    match b.kind.as_str() {
        "text" => Some(Block::Text(b.text)),
        "tool_use" => Some(Block::ToolUse { id: b.id, name: b.name, input: b.input }),
        "tool_result" => Some(Block::ToolResult {
            tool_use_id: b.tool_use_id, content: result_text(&b.content),
            is_error: b.is_error.unwrap_or(false), agent_id: agent_id.clone() }),
        _ => None, // thinking, redacted_thinking, image, unknown
    }
}

pub fn parse(jsonl: &str) -> Vec<Turn> {
    jsonl.lines().filter_map(|line| {
        let r: RawRecord = serde_json::from_str(line.trim()).ok()?;
        let role = match r.kind.as_str() { "user" => Role::User, "assistant" => Role::Assistant, _ => return None };
        let agent_id = r.tool_use_result.get("agentId").and_then(Value::as_str).map(str::to_owned);
        let blocks: Vec<Block> = match r.message?.content {
            Value::String(s) => vec![Block::Text(s)],
            Value::Array(items) => items.into_iter().filter_map(|v| to_block(v, &agent_id)).collect(),
            _ => Vec::new(),
        };
        if blocks.is_empty() { return None; }
        Some(Turn { role, blocks, is_meta: r.is_meta, is_sidechain: r.is_sidechain })
    }).collect()
}

pub fn ai_title(jsonl: &str) -> Option<String> {
    jsonl.lines().find_map(|line| {
        let r: RawRecord = serde_json::from_str(line.trim()).ok()?;
        if r.kind == "ai-title" { r.ai_title.filter(|t| !t.is_empty()) } else { None }
    })
}

fn main() {
    let mut total = 0usize;
    for path in std::env::args().skip(1) {
        let Ok(s) = std::fs::read_to_string(&path) else { continue };
        let lines = s.lines().filter(|l| !l.trim().is_empty()).count();
        let turns = parse(&s);
        let dbg = format!("{turns:?}");
        let leaks = dbg.contains("SECRET-THINKING-MARKER") || dbg.contains("SECRET-SIGNATURE-MARKER");
        let (mut t, mut u, mut r, mut meta, mut side, mut agent) = (0, 0, 0, 0, 0, 0);
        for turn in &turns {
            if turn.is_meta { meta += 1 }
            if turn.is_sidechain { side += 1 }
            for b in &turn.blocks { match b {
                Block::Text(_) => t += 1,
                Block::ToolUse { .. } => u += 1,
                Block::ToolResult { agent_id, .. } => { r += 1; if agent_id.is_some() { agent += 1 } }
            } }
        }
        total += turns.len();
        let name = std::path::Path::new(&path).file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
        let short: String = name.chars().take(12).collect();
        println!("{short:<12} lines={lines:<5} turns={:<5} text={t:<4} use={u:<4} result={r:<4} agent_ids={agent} meta={meta} side={side} title={} leak={leaks}",
            turns.len(), ai_title(&s).is_some());
    }
    // garbage / truncation probes
    let deep = "[".repeat(100_000);
    let garbage = ["", "\n\n", "{", "null", "[]", "\"str\"", "{\"type\":\"user\"}", "{\"type\":\"user\",\"message\":5}",
        "{\"type\":\"user\",\"message\":{\"content\":[1,null,{\"type\":7}]}}", "\u{feff}{}", &deep, "\u{0}\u{1}\u{fffd}",
        "{\"type\":\"assistant\",\"message\":{\"content\":[{\"type\":\"text\",\"text\":\"ok\"}]}}\n{\"type\":\"assistant\",\"mess"];
    for g in garbage { let _ = parse(g); let _ = ai_title(g); }
    println!("garbage ok; last-line truncation keeps first: {}", parse(garbage[12]).len());
    println!("total turns {total}");
}
