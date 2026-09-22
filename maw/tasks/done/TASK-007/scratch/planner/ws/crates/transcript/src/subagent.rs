//! Collapsed subagent blocks: `.meta.json` fields and the body source order.

use serde_json::Value;

use crate::render::{IN_PROGRESS_MARKER, agent_header, one_line, render};
use crate::{Block, Role, Turn, parse};

/// The fields of a subagent's `.meta.json` that a block uses.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct SubagentMeta {
    /// `agentType`; equals the parent `Agent` call's `subagent_type`.
    pub agent_type: Option<String>,
    /// `description`; equals the parent `Agent` call's `description`.
    pub description: Option<String>,
}

/// Reads `.meta.json` text. Broken JSON, a non-object, and blank or non-string fields give `None`s.
pub fn parse_subagent_meta(json: &str) -> SubagentMeta {
    let value: Value =
        serde_json::from_str(json.trim_start_matches('\u{feff}')).unwrap_or(Value::Null);
    let field = |key: &str| {
        value
            .get(key)
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|text| !text.is_empty())
            .map(str::to_owned)
    };
    SubagentMeta {
        agent_type: field("agentType"),
        description: field("description"),
    }
}

/// What the hub has read for one subagent. Fill in what is known; `Subagent::new` applies the order.
#[derive(Debug, Clone, Copy, Default)]
pub struct SubagentInput<'a> {
    /// From `SubagentStart`/`SubagentStop`, or `Block::ToolResult::agent_id` of the parent's `Agent` call.
    pub agent_id: &'a str,
    /// Hook `agent_type` or the parent call's `subagent_type`; used when the meta has no `agentType`.
    pub agent_type: Option<&'a str>,
    /// Text of `agent-<id>.meta.json`; `None` when the file is missing.
    pub meta: Option<&'a str>,
    /// `tool_input.message` of the `SubagentHandback` hook.
    pub report: Option<&'a str>,
    /// Text of `agent-<id>.jsonl`; `None` when missing. It may lag the subagent.
    pub transcript: Option<&'a str>,
    /// `SubagentStop.last_assistant_message`.
    pub last_assistant_message: Option<&'a str>,
}

/// The body of a block and the source it came from, in precedence order.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SubagentBody {
    /// The handed-back report, trimmed.
    Report(String),
    /// Brief of the transcript after the spawn prompt. It ends with a final answer, and with
    /// `last_assistant_message` when that is given.
    Transcript(String),
    /// `last_assistant_message`, trimmed: the transcript is missing, unfinished or behind it.
    LastMessage(String),
    /// Brief of an unfinished transcript, ending with the in-progress marker; no stop data yet.
    InProgress(String),
    /// Nothing to show yet.
    Empty,
}

impl SubagentBody {
    /// Plain text of the body; empty for `Empty`.
    pub fn text(&self) -> &str {
        match self {
            Self::Report(text)
            | Self::Transcript(text)
            | Self::LastMessage(text)
            | Self::InProgress(text) => text,
            Self::Empty => "",
        }
    }
}

/// One subagent as a collapsed block. Only `Subagent::new` builds it, so the body order is the library's:
///
/// ```compile_fail
/// let block = transcript::Subagent {
///     agent_id: "a1".to_owned(),
///     agent_type: "Explore".to_owned(),
///     description: None,
///     body: transcript::SubagentBody::Empty,
/// };
/// ```
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Subagent {
    agent_id: String,
    agent_type: String,
    description: Option<String>,
    body: SubagentBody,
}

impl Subagent {
    /// Header: type from the meta, else `input.agent_type`, else `agent`; description from the meta only.
    /// Body: the report, else the finished transcript brief (when it ends with `last_assistant_message`,
    /// if given), else `last_assistant_message`, else the unfinished transcript brief, else empty.
    pub fn new(input: SubagentInput<'_>) -> Self {
        let meta = input.meta.map(parse_subagent_meta).unwrap_or_default();
        let agent_type = meta
            .agent_type
            .as_deref()
            .or(input.agent_type)
            .map(one_line)
            .filter(|kind| !kind.is_empty())
            .unwrap_or_else(|| "agent".to_owned());
        Self {
            agent_id: input.agent_id.to_owned(),
            agent_type,
            description: meta.description.as_deref().map(one_line),
            body: body(&input),
        }
    }

    pub fn agent_id(&self) -> &str {
        &self.agent_id
    }

    pub fn body(&self) -> &SubagentBody {
        &self.body
    }

    /// `↳ <type> <id>[: <description>]`, then the body lines. The body is always brief.
    pub fn render(&self) -> String {
        let header = agent_header(
            &self.agent_type,
            Some(&self.agent_id),
            self.description.as_deref(),
        );
        match self.body.text() {
            "" => header,
            body => format!("{header}\n{body}"),
        }
    }
}

fn body(input: &SubagentInput<'_>) -> SubagentBody {
    if let Some(report) = non_blank(input.report) {
        return SubagentBody::Report(report.to_owned());
    }
    let (brief, finished) = input.transcript.map(transcript_brief).unwrap_or_default();
    let last = non_blank(input.last_assistant_message);
    if finished && last.is_none_or(|last| brief.ends_with(last)) {
        return SubagentBody::Transcript(brief);
    }
    if let Some(last) = last {
        return SubagentBody::LastMessage(last.to_owned());
    }
    if brief.is_empty() {
        SubagentBody::Empty
    } else {
        SubagentBody::InProgress(brief)
    }
}

/// Brief of the turns after the spawn prompt and whether it ends with a final answer. A transcript
/// that holds only the prompt so far gives the in-progress marker alone.
fn transcript_brief(jsonl: &str) -> (String, bool) {
    let turns = parse(jsonl);
    if turns.is_empty() {
        return (String::new(), false);
    }
    let (brief, in_progress) = render(after_spawn_prompt(&turns), false, &[]);
    if brief.is_empty() {
        return (IN_PROGRESS_MARKER.to_owned(), false);
    }
    (brief, !in_progress)
}

/// The spawn prompt is the first user text: the parent's `Agent` prompt, often longer than a message.
fn after_spawn_prompt(turns: &[Turn]) -> &[Turn] {
    turns
        .iter()
        .position(|turn| {
            turn.role == Role::User
                && turn
                    .blocks
                    .iter()
                    .any(|block| matches!(block, Block::Text(_)))
        })
        .map_or(turns, |index| &turns[index + 1..])
}

fn non_blank(text: Option<&str>) -> Option<&str> {
    text.map(str::trim).filter(|text| !text.is_empty())
}
