//! Plain-text brief and full views of a slice of turns.

use std::collections::HashMap;

use serde_json::Value;

use crate::{Block, Role, Turn};

/// Last line of a rendering whose slice ends while the assistant is still working.
pub const IN_PROGRESS_MARKER: &str = "в работе…";

const SUMMARY_KEYS: [&str; 8] = [
    "description",
    "file_path",
    "notebook_path",
    "pattern",
    "url",
    "query",
    "command",
    "skill",
];
const SUMMARY_CHARS: usize = 120;
const INPUT_CHARS: usize = 500;
const RESULT_CHARS: usize = 1500;
const INTERRUPT_PREFIX: &str = "[Request interrupted by user";

/// Prompts, final assistant answers and one line per tool call.
pub fn render_brief(turns: &[Turn]) -> String {
    render(turns, false)
}

/// Brief plus intermediate assistant text, tool inputs and truncated tool results.
pub fn render_full(turns: &[Turn]) -> String {
    render(turns, true)
}

fn render(turns: &[Turn], full: bool) -> String {
    let calls = tool_calls(turns);
    let agents = agent_ids(turns);
    let tool_after = tool_after(turns);
    let mut out = String::new();
    let mut finished = true;
    let mut any = false;
    for (turn, &tool_after) in turns.iter().zip(&tool_after) {
        for block in &turn.blocks {
            match (turn.role, block) {
                (Role::User, Block::Text(text)) => {
                    let Some(prompt) = prompt_text(turn, text) else {
                        continue;
                    };
                    if !out.is_empty() {
                        out.push('\n');
                    }
                    push_line(&mut out, &format!("> {prompt}"));
                    finished = prompt.starts_with(INTERRUPT_PREFIX);
                    any = true;
                }
                (Role::Assistant, Block::Text(text)) => {
                    let text = text.trim();
                    if text.is_empty() {
                        continue;
                    }
                    let is_final = is_final(turn, tool_after);
                    if full || is_final {
                        push_line(&mut out, text);
                    }
                    finished = is_final;
                    any = true;
                }
                (_, Block::ToolUse { id, name, input }) => {
                    push_line(&mut out, &tool_line(name, input, agents.get(id.as_str())));
                    if full {
                        push_line(
                            &mut out,
                            &indent(&truncate(&input.to_string(), INPUT_CHARS)),
                        );
                    }
                    finished = false;
                    any = true;
                }
                (
                    _,
                    Block::ToolResult {
                        tool_use_id,
                        content,
                        is_error,
                        ..
                    },
                ) => {
                    if full {
                        let name = calls.get(tool_use_id.as_str()).copied().unwrap_or("tool");
                        let label = if *is_error { "error" } else { name };
                        let body = truncate(content.trim(), RESULT_CHARS);
                        push_line(&mut out, &indent(&format!("← {label}: {body}")));
                    }
                    finished = false;
                    any = true;
                }
            }
        }
    }
    if any && !finished {
        push_line(&mut out, IN_PROGRESS_MARKER);
    }
    out
}

/// Text of a user turn shown as a prompt; `None` for hidden meta records.
fn prompt_text<'a>(turn: &Turn, text: &'a str) -> Option<&'a str> {
    let text = text.trim();
    if text.is_empty() {
        return None;
    }
    if !turn.is_meta {
        return Some(text);
    }
    let inner = text.strip_prefix("<channel")?.strip_suffix("</channel>")?;
    inner.split_once('>').map(|(_, body)| body.trim())
}

/// `end_turn` text is the answer, `tool_use` text is not; any other stop reason (null in
/// subagent transcripts, `stop_sequence`, `refusal`, ...) is the answer unless a tool call follows.
fn is_final(turn: &Turn, tool_after: bool) -> bool {
    match turn.stop_reason.as_deref() {
        Some("end_turn") => true,
        Some("tool_use") => false,
        _ => !tool_after,
    }
}

/// For each turn: does a tool call appear in it or later, before the next prompt?
fn tool_after(turns: &[Turn]) -> Vec<bool> {
    let mut flags = vec![false; turns.len()];
    let mut seen = false;
    for (i, turn) in turns.iter().enumerate().rev() {
        if turn.role == Role::User
            && turn.blocks.iter().any(|block| match block {
                Block::Text(text) => prompt_text(turn, text).is_some(),
                _ => false,
            })
        {
            seen = false;
        }
        seen |= turn
            .blocks
            .iter()
            .any(|block| matches!(block, Block::ToolUse { .. }));
        flags[i] = seen;
    }
    flags
}

fn tool_calls(turns: &[Turn]) -> HashMap<&str, &str> {
    turns
        .iter()
        .flat_map(|turn| &turn.blocks)
        .filter_map(|block| match block {
            Block::ToolUse { id, name, .. } => Some((id.as_str(), name.as_str())),
            _ => None,
        })
        .collect()
}

fn agent_ids(turns: &[Turn]) -> HashMap<&str, &str> {
    turns
        .iter()
        .flat_map(|turn| &turn.blocks)
        .filter_map(|block| match block {
            Block::ToolResult {
                tool_use_id,
                agent_id: Some(agent_id),
                ..
            } => Some((tool_use_id.as_str(), agent_id.as_str())),
            _ => None,
        })
        .collect()
}

fn tool_line(name: &str, input: &Value, agent_id: Option<&&str>) -> String {
    let field = |key: &str| {
        input
            .get(key)
            .and_then(Value::as_str)
            .map(one_line)
            .filter(|value| !value.is_empty())
    };
    if name == "Agent" {
        let kind = field("subagent_type").unwrap_or_else(|| "agent".to_owned());
        let mut line = format!("↳ {kind}");
        if let Some(agent_id) = agent_id {
            line.push(' ');
            line.push_str(agent_id);
        }
        if let Some(description) = field("description") {
            line.push_str(": ");
            line.push_str(&description);
        }
        return line;
    }
    match SUMMARY_KEYS.iter().find_map(|key| field(key)) {
        Some(summary) => format!("• {name}: {summary}"),
        None => format!("• {name}"),
    }
}

/// Collapses whitespace runs to one space and cuts to `SUMMARY_CHARS`.
fn one_line(text: &str) -> String {
    truncate(
        &text.split_whitespace().collect::<Vec<_>>().join(" "),
        SUMMARY_CHARS,
    )
}

/// Keeps the first `max` chars and says how many were dropped.
fn truncate(text: &str, max: usize) -> String {
    match text.char_indices().nth(max) {
        None => text.to_owned(),
        Some((cut, _)) => {
            let dropped = text[cut..].chars().count();
            format!("{}… [+{dropped} chars]", &text[..cut])
        }
    }
}

fn indent(text: &str) -> String {
    text.lines()
        .map(|line| format!("  {line}"))
        .collect::<Vec<_>>()
        .join("\n")
}

fn push_line(out: &mut String, line: &str) {
    if !out.is_empty() {
        out.push('\n');
    }
    out.push_str(line);
}
