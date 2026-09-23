//! Subagent blocks: which typed subagent hooks are real subagents, and the
//! texts of their blocks.
//!
//! A typed `SubagentStart`/`SubagentStop` is only a candidate. It gets a block
//! once the parent's transcript shows an `Agent` call whose result carries the
//! same `toolUseResult.agentId`. Claude Code's internal agents and the main
//! agent of an `--agent` session never have one. The parent transcript lags
//! the hooks by seconds, so a candidate is looked up again after a pause that
//! doubles, until a window ends; its stop opens the window again.
//!
//! [`scan`] and [`read_body`] block on file IO and run on `spawn_blocking`;
//! everything else is pure. Texts never reach the logs.

use std::collections::{HashMap, VecDeque};
use std::io::{BufRead, BufReader, Read, Seek, SeekFrom};
use std::time::Duration;

use serde_json::Value;
use tokio::time::Instant;
use transcript::{Block, Subagent, SubagentInput, TELEGRAM_TEXT_LIMIT, parse, telegram_len};

use super::registry::cut;

/// A parent transcript is indexed up to this many bytes (the `/brief` cap).
pub const MAX_TRANSCRIPT_BYTES: u64 = 256 * 1024 * 1024;
/// A subagent transcript is read up to this many bytes for its block.
pub const MAX_AGENT_BYTES: u64 = 64 * 1024 * 1024;
const MAX_META_BYTES: u64 = 64 * 1024;
/// Candidates waiting for their `Agent` call; beyond this the one whose
/// window ends first is dropped.
pub const MAX_CANDIDATES: usize = 256;
/// Handed-back reports kept until their subagent stops.
pub const MAX_REPORTS: usize = 256;
/// `Agent` calls and results one session index keeps; the oldest go first.
pub const MAX_INDEX_ENTRIES: usize = 1024;
/// A call's `subagent_type` and `description` are kept up to this many
/// UTF-16 units (the header shows 120 characters).
const MAX_CALL_FIELD: usize = 256;
/// The pause before a lookup at most doubles this many times.
const MAX_DOUBLINGS: u32 = 4;
const AGENT_TOOL: &str = "Agent";
/// Ends a block text that was cut; the whole text follows as a file.
pub const CUT_NOTE: &str = "\n(полный текст в файле ниже)";
/// Ends a nested run's block when it gave no answer.
pub const NESTED_DONE: &str = " · завершён";

/// Agent ids go into channel meta values and Telegram texts: only plain
/// ids (`a8c1bff86acd31609`) are taken.
pub fn is_agent_id(id: &str) -> bool {
    !id.is_empty()
        && id.len() <= 64
        && id
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || byte == b'_' || byte == b'-')
}

/// `subagent_type` and `description` of a parent `Agent` call.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct AgentCall {
    pub subagent_type: Option<String>,
    pub description: Option<String>,
}

/// What one read of a parent transcript found after its start offset.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Scan {
    pub path: String,
    /// End of the last complete line read.
    pub offset: u64,
    /// `Agent` calls by tool use id.
    pub calls: Vec<(String, AgentCall)>,
    /// Agent id -> tool use id of the call that launched it.
    pub links: Vec<(String, String)>,
}

impl Scan {
    pub fn nothing(path: String, offset: u64) -> Self {
        Self {
            path,
            offset,
            ..Self::default()
        }
    }
}

/// Reads the complete lines of `path` after byte `from`. A missing file or a
/// read error finds nothing.
pub fn scan(path: &str, from: u64) -> Scan {
    let mut found = Scan::nothing(path.to_owned(), from);
    let Ok(mut file) = std::fs::File::open(path) else {
        return found;
    };
    if file.seek(SeekFrom::Start(from)).is_err() {
        return found;
    }
    scan_lines(file, MAX_TRANSCRIPT_BYTES.saturating_sub(from), &mut found);
    found
}

/// A last line without its newline may still be being written: it is left
/// for the next scan.
fn scan_lines(jsonl: impl Read, limit: u64, found: &mut Scan) {
    let mut reader = BufReader::new(jsonl.take(limit));
    let mut line = Vec::new();
    loop {
        line.clear();
        match reader.read_until(b'\n', &mut line) {
            Ok(0) | Err(_) => return,
            Ok(_) if !line.ends_with(b"\n") => return,
            Ok(read) => found.offset += read as u64,
        }
        let text = String::from_utf8_lossy(&line);
        if !text.contains("\"Agent\"") && !text.contains("agentId") {
            continue;
        }
        for turn in parse(&text) {
            for block in turn.blocks {
                match block {
                    Block::ToolUse { id, name, input } if name == AGENT_TOOL => {
                        let field = |key: &str| {
                            input
                                .get(key)
                                .and_then(Value::as_str)
                                .map(|text| cut(text, MAX_CALL_FIELD))
                        };
                        let call = AgentCall {
                            subagent_type: field("subagent_type"),
                            description: field("description"),
                        };
                        found.calls.push((id, call));
                    }
                    Block::ToolResult {
                        tool_use_id,
                        agent_id: Some(agent_id),
                        ..
                    } if !agent_id.is_empty() => found.links.push((agent_id, tool_use_id)),
                    _ => {}
                }
            }
        }
    }
}

/// The `Agent` calls of one session's transcript found so far, at most
/// [`MAX_INDEX_ENTRIES`] calls and as many results.
#[derive(Debug, Default)]
pub struct AgentIndex {
    path: String,
    offset: u64,
    calls: HashMap<String, AgentCall>,
    links: HashMap<String, String>,
    /// Insertion order of `calls` and `links`, for eviction.
    call_order: VecDeque<String>,
    link_order: VecDeque<String>,
}

impl AgentIndex {
    /// Where the next scan of `path` starts.
    pub fn resume_at(&self, path: &str) -> u64 {
        if self.path == path { self.offset } else { 0 }
    }

    pub fn merge(&mut self, scan: Scan) {
        if scan.path != self.path {
            *self = Self {
                path: scan.path,
                ..Self::default()
            };
        }
        self.offset = scan.offset;
        for (id, call) in scan.calls {
            if self.calls.insert(id.clone(), call).is_none() {
                self.call_order.push_back(id);
            }
        }
        for (agent_id, id) in scan.links {
            if self.links.insert(agent_id.clone(), id).is_none() {
                self.link_order.push_back(agent_id);
            }
        }
        while self.call_order.len() > MAX_INDEX_ENTRIES {
            if let Some(oldest) = self.call_order.pop_front() {
                self.calls.remove(&oldest);
            }
        }
        while self.link_order.len() > MAX_INDEX_ENTRIES {
            if let Some(oldest) = self.link_order.pop_front() {
                self.links.remove(&oldest);
            }
        }
    }

    /// The `Agent` call whose result named `agent_id`.
    pub fn call(&self, agent_id: &str) -> Option<&AgentCall> {
        self.calls.get(self.links.get(agent_id)?)
    }
}

/// What `SubagentStop` said.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Stopped {
    pub agent_type: String,
    pub agent_path: String,
    pub last: Option<String>,
}

#[derive(Debug, Clone)]
pub struct Candidate {
    pub session: String,
    pub agent_type: String,
    pub stop: Option<Stopped>,
    checks: u32,
    next_check: Instant,
    deadline: Instant,
}

/// Typed subagents not yet matched to an `Agent` call.
#[derive(Debug, Default)]
pub struct Candidates {
    by_agent: HashMap<String, Candidate>,
}

impl Candidates {
    /// A hook named the subagent. A stop is kept and gives it a new window
    /// and an immediate lookup. Another session's hook for a known id is
    /// ignored.
    pub fn seen(
        &mut self,
        agent_id: &str,
        session: &str,
        agent_type: &str,
        stop: Option<Stopped>,
        now: Instant,
        window: Duration,
    ) {
        if let Some(candidate) = self.by_agent.get_mut(agent_id) {
            if candidate.session == session
                && let Some(stop) = stop
            {
                candidate.stop = Some(stop);
                candidate.deadline = now + window;
                candidate.next_check = now;
            }
            return;
        }
        if self.by_agent.len() >= MAX_CANDIDATES
            && let Some(oldest) = self
                .by_agent
                .iter()
                .min_by_key(|(_, candidate)| candidate.deadline)
                .map(|(id, _)| id.clone())
        {
            self.by_agent.remove(&oldest);
        }
        self.by_agent.insert(
            agent_id.to_owned(),
            Candidate {
                session: session.to_owned(),
                agent_type: agent_type.to_owned(),
                stop,
                checks: 0,
                next_check: now,
                deadline: now + window,
            },
        );
    }

    /// Sessions with a candidate due for a lookup.
    pub fn due_sessions(&self, now: Instant) -> Vec<String> {
        let mut sessions: Vec<String> = self
            .by_agent
            .values()
            .filter(|candidate| candidate.next_check <= now)
            .map(|candidate| candidate.session.clone())
            .collect();
        sessions.sort();
        sessions.dedup();
        sessions
    }

    pub fn of_session(&self, session: &str) -> Vec<String> {
        let mut agents: Vec<String> = self
            .by_agent
            .iter()
            .filter(|(_, candidate)| candidate.session == session)
            .map(|(id, _)| id.clone())
            .collect();
        agents.sort();
        agents
    }

    pub fn take(&mut self, agent_id: &str) -> Option<Candidate> {
        self.by_agent.remove(agent_id)
    }

    /// A lookup did not find the call. `true`: the window is over and the
    /// candidate is dropped; otherwise the pause doubles, never past the
    /// window's end, where one last lookup happens.
    pub fn missed(&mut self, agent_id: &str, now: Instant, first_pause: Duration) -> bool {
        let Some(candidate) = self.by_agent.get_mut(agent_id) else {
            return false;
        };
        if now >= candidate.deadline {
            self.by_agent.remove(agent_id);
            return true;
        }
        let pause = first_pause * 2u32.pow(candidate.checks.min(MAX_DOUBLINGS));
        candidate.checks += 1;
        candidate.next_check = (now + pause).min(candidate.deadline);
        false
    }

    /// The next lookup, not counting sessions for which `busy` is true (a
    /// read is out; its answer brings the next pass).
    pub fn next_due(&self, busy: impl Fn(&str) -> bool) -> Option<Instant> {
        self.by_agent
            .values()
            .filter(|candidate| !busy(&candidate.session))
            .map(|candidate| candidate.next_check)
            .min()
    }

    pub fn len(&self) -> usize {
        self.by_agent.len()
    }

    pub fn is_empty(&self) -> bool {
        self.by_agent.is_empty()
    }
}

/// `SubagentHandback` reports by agent id, until the stop uses them.
#[derive(Debug, Default)]
pub struct Reports {
    by_agent: HashMap<String, String>,
    order: VecDeque<String>,
}

impl Reports {
    pub fn insert(&mut self, agent_id: String, report: String) {
        if self.by_agent.insert(agent_id.clone(), report).is_none() {
            self.order.push_back(agent_id);
        }
        while self.order.len() > MAX_REPORTS {
            if let Some(oldest) = self.order.pop_front() {
                self.by_agent.remove(&oldest);
            }
        }
    }

    pub fn take(&mut self, agent_id: &str) -> Option<String> {
        let report = self.by_agent.remove(agent_id)?;
        self.order.retain(|id| id != agent_id);
        Some(report)
    }
}

/// The running block's header: `↳ <type> <id>[: <description>]`.
pub fn header(agent_id: &str, agent_type: Option<&str>, description: Option<&str>) -> String {
    Subagent::new(SubagentInput {
        agent_id,
        agent_type,
        description,
        ..SubagentInput::default()
    })
    .header()
}

/// Everything a finished block is made of, besides the files.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct BodyInput {
    pub agent_id: String,
    pub agent_type: Option<String>,
    pub description: Option<String>,
    pub agent_path: String,
    pub report: Option<String>,
    pub last: Option<String>,
}

/// The finished block: reads `agent-<id>.jsonl` and its `.meta.json`, then
/// the library picks the body (report, finished transcript, last message).
pub fn read_body(input: &BodyInput) -> String {
    let transcript = read_capped(&input.agent_path, MAX_AGENT_BYTES);
    let meta = input
        .agent_path
        .strip_suffix(".jsonl")
        .and_then(|stem| read_capped(&format!("{stem}.meta.json"), MAX_META_BYTES));
    body_text(input, meta.as_deref(), transcript.as_deref())
}

/// [`read_body`] once the files are read.
pub fn body_text(input: &BodyInput, meta: Option<&str>, transcript: Option<&str>) -> String {
    Subagent::new(SubagentInput {
        agent_id: &input.agent_id,
        agent_type: input.agent_type.as_deref(),
        description: input.description.as_deref(),
        meta,
        report: input.report.as_deref(),
        transcript,
        last_assistant_message: input.last.as_deref(),
    })
    .render()
}

fn read_capped(path: &str, cap: u64) -> Option<String> {
    if path.is_empty() {
        return None;
    }
    let mut bytes = Vec::new();
    std::fs::File::open(path)
        .ok()?
        .take(cap)
        .read_to_end(&mut bytes)
        .ok()?;
    Some(String::from_utf8_lossy(&bytes).into_owned())
}

/// The nested run's final block: its last answer, or "завершён".
pub fn nested_text(header: &str, answer: Option<&str>) -> String {
    match answer.map(str::trim).filter(|answer| !answer.is_empty()) {
        Some(answer) => format!("{header}\n{answer}"),
        None => format!("{header}{NESTED_DONE}"),
    }
}

/// The text for one Telegram message, and the whole text when it had to be
/// cut (it then goes out as a file).
pub fn fit(text: String) -> (String, Option<String>) {
    if telegram_len(&text) <= TELEGRAM_TEXT_LIMIT {
        return (text, None);
    }
    let room = TELEGRAM_TEXT_LIMIT - telegram_len(CUT_NOTE);
    (format!("{}{CUT_NOTE}", cut(&text, room)), Some(text))
}

#[cfg(test)]
mod tests {
    use super::*;

    const PARENT: &str = include_str!("../../../transcript/tests/fixtures/final_answer.jsonl");
    const SUBAGENT: &str =
        include_str!("../../../transcript/tests/fixtures/subagent_handback.jsonl");
    const META: &str =
        include_str!("../../../transcript/tests/fixtures/subagent_handback.meta.json");
    const REPORT: &str = "Modules: lib, render, split.";

    fn scan_text(text: &str) -> Scan {
        let mut found = Scan::nothing("p".into(), 0);
        scan_lines(text.as_bytes(), u64::MAX, &mut found);
        found
    }

    #[test]
    fn an_agent_call_and_its_result_link_the_agent_id() {
        let mut index = AgentIndex::default();
        index.merge(scan_text(PARENT));
        assert_eq!(
            index.call("a0000000000000002"),
            Some(&AgentCall {
                subagent_type: Some("Explore".into()),
                description: Some("Explore crate".into()),
            })
        );
        assert_eq!(index.call("a0000000000000009"), None);
        assert_eq!(index.resume_at("p"), PARENT.len() as u64);
        assert_eq!(index.resume_at("other"), 0);
    }

    #[test]
    fn a_result_without_its_call_or_a_call_without_its_result_links_nothing() {
        let lines: Vec<&str> = PARENT.lines().collect();
        let call_only = format!("{}\n", lines[8]);
        let result_only = format!("{}\n", lines[9]);
        for text in [&call_only, &result_only] {
            let mut index = AgentIndex::default();
            index.merge(scan_text(text));
            assert_eq!(index.call("a0000000000000002"), None);
        }
        // Scanned in two passes, the pair still matches.
        let mut index = AgentIndex::default();
        index.merge(scan_text(&call_only));
        let mut second = scan_text(&result_only);
        second.offset += call_only.len() as u64;
        index.merge(second);
        assert!(index.call("a0000000000000002").is_some());
    }

    #[test]
    fn a_line_still_being_written_is_left_for_the_next_scan() {
        let lines: Vec<&str> = PARENT.lines().collect();
        let head = format!("{}\n{}", lines[8], &lines[9][..40]);
        let found = scan_text(&head);
        assert_eq!(found.offset, lines[8].len() as u64 + 1);
        assert!(found.links.is_empty());
    }

    #[test]
    fn a_missing_file_finds_nothing_and_keeps_the_offset() {
        let found = scan("no/such/dir/x.jsonl", 7);
        assert_eq!(found, Scan::nothing("no/such/dir/x.jsonl".into(), 7));
    }

    #[test]
    fn a_candidate_waits_longer_each_time_until_its_window_ends() {
        let mut candidates = Candidates::default();
        let now = Instant::now();
        let window = Duration::from_secs(60);
        let pause = Duration::from_secs(1);
        candidates.seen("a1", "s", "Explore", None, now, window);
        assert_eq!(candidates.due_sessions(now), ["s"]);
        let mut at = now;
        let mut waits = Vec::new();
        loop {
            if candidates.missed("a1", at, pause) {
                break;
            }
            let next = candidates.next_due(|_| false).unwrap();
            waits.push((next - at).as_secs());
            at = next;
        }
        assert_eq!(waits, [1, 2, 4, 8, 16, 16, 13]);
        assert!(candidates.is_empty());
        // A stop opens a new window and asks for a lookup now.
        candidates.seen("a2", "s", "Explore", None, now, window);
        candidates.missed("a2", now, pause);
        let later = now + Duration::from_secs(59);
        candidates.seen(
            "a2",
            "s",
            "Explore",
            Some(Stopped::default()),
            later,
            window,
        );
        assert_eq!(candidates.next_due(|_| false), Some(later));
        assert!(!candidates.missed("a2", now + Duration::from_secs(100), pause));
        // A busy session asks for no wake-up.
        assert_eq!(candidates.next_due(|session| session == "s"), None);
        // Another session's hook for the same id changes nothing.
        candidates.seen("a2", "t", "Explore", Some(Stopped::default()), now, window);
        assert_eq!(candidates.of_session("t"), Vec::<String>::new());
    }

    #[test]
    fn candidates_and_reports_are_bounded() {
        let mut candidates = Candidates::default();
        let now = Instant::now();
        for i in 0..MAX_CANDIDATES + 10 {
            let at = now + Duration::from_millis(i as u64);
            candidates.seen(&format!("a{i}"), "s", "Explore", None, at, Duration::ZERO);
        }
        assert_eq!(candidates.len(), MAX_CANDIDATES);
        assert!(candidates.take("a0").is_none());
        let mut reports = Reports::default();
        for i in 0..MAX_REPORTS + 10 {
            reports.insert(format!("a{i}"), "r".into());
        }
        assert_eq!(reports.take("a0"), None);
        assert_eq!(
            reports.take(&format!("a{}", MAX_REPORTS + 9)),
            Some("r".into())
        );
    }

    #[test]
    fn an_index_keeps_the_newest_calls_and_short_fields() {
        let mut text = String::new();
        for i in 0..MAX_INDEX_ENTRIES + 5 {
            let call = serde_json::json!({
                "type": "assistant",
                "message": { "role": "assistant", "content": [{
                    "type": "tool_use", "id": format!("t{i}"), "name": "Agent",
                    "input": { "description": "d".repeat(5000), "subagent_type": "Explore" },
                }]},
            });
            let result = serde_json::json!({
                "type": "user",
                "message": { "role": "user", "content": [{
                    "type": "tool_result", "tool_use_id": format!("t{i}"), "content": "ok",
                }]},
                "toolUseResult": { "agentId": format!("a{i}") },
            });
            text.push_str(&format!("{call}\n{result}\n"));
        }
        let mut index = AgentIndex::default();
        index.merge(scan_text(&text));
        assert_eq!(index.calls.len(), MAX_INDEX_ENTRIES);
        assert_eq!(index.links.len(), MAX_INDEX_ENTRIES);
        assert_eq!(index.call("a0"), None);
        let newest = index.call(&format!("a{}", MAX_INDEX_ENTRIES + 4)).unwrap();
        let description = newest.description.as_deref().unwrap();
        assert_eq!(telegram_len(description), MAX_CALL_FIELD);
    }

    #[test]
    fn agent_ids_are_plain() {
        for good in ["a8c1bff86acd31609", "a_1-B"] {
            assert!(is_agent_id(good), "{good}");
        }
        for bad in ["", "a b", "a\"b", "a>b", "é", &"a".repeat(65)] {
            assert!(!is_agent_id(bad), "{bad:?}");
        }
    }

    fn input(report: Option<&str>, last: Option<&str>) -> BodyInput {
        BodyInput {
            agent_id: "a0000000000000002".into(),
            agent_type: Some("Explore".into()),
            description: Some("Explore crate".into()),
            agent_path: String::new(),
            report: report.map(str::to_owned),
            last: last.map(str::to_owned),
        }
    }

    #[test]
    fn the_block_body_follows_the_fallback_order() {
        let head = "↳ Explore a0000000000000002: Explore crate";
        let lagging: String = SUBAGENT.lines().take(6).map(|l| format!("{l}\n")).collect();
        let cases = [
            // The handed-back report wins over everything.
            (
                Some(REPORT),
                Some("other"),
                Some(SUBAGENT),
                format!("{head}\n{REPORT}"),
            ),
            // A finished transcript whose final answer is the stop's.
            (
                None,
                Some("Report handed back."),
                Some(SUBAGENT),
                format!(
                    "{head}\n• Bash: List source files\n• SubagentHandback\nReport handed back."
                ),
            ),
            // The file lags the stop: the stop's last message.
            (
                None,
                Some("Final."),
                Some(lagging.as_str()),
                format!("{head}\nFinal."),
            ),
            (None, Some("Final."), None, format!("{head}\nFinal.")),
            // Nothing to show.
            (None, None, None, head.to_owned()),
        ];
        for (report, last, transcript, want) in cases {
            let text = body_text(&input(report, last), Some(META), transcript);
            assert_eq!(text, want, "{report:?} {last:?}");
        }
    }

    #[test]
    fn the_body_is_read_from_the_subagent_files() {
        let dir = crate::hub::testdir::TempDir::new("subagent-body");
        let path = dir.path().join("agent-a0000000000000002.jsonl");
        std::fs::write(&path, SUBAGENT).unwrap();
        std::fs::write(dir.path().join("agent-a0000000000000002.meta.json"), META).unwrap();
        let mut body = input(None, Some("Report handed back."));
        body.agent_type = None;
        body.description = None;
        body.agent_path = path.to_string_lossy().into_owned();
        let text = read_body(&body);
        assert!(
            text.starts_with("↳ Explore a0000000000000002: Explore crate\n"),
            "{text}"
        );
        assert!(text.ends_with("Report handed back."), "{text}");
        body.agent_path = dir.path().join("gone.jsonl").to_string_lossy().into_owned();
        assert_eq!(
            read_body(&body),
            "↳ agent a0000000000000002\nReport handed back."
        );
    }

    #[test]
    fn long_texts_are_cut_to_one_message_and_kept_whole() {
        let short = "↳ Explore a1\nok".to_owned();
        assert_eq!(fit(short.clone()), (short, None));
        let long = format!("↳ Explore a1\n{}", "я".repeat(5000));
        let (shown, whole) = fit(long.clone());
        assert_eq!(telegram_len(&shown), TELEGRAM_TEXT_LIMIT);
        assert!(shown.ends_with(CUT_NOTE));
        assert_eq!(whole, Some(long));
        assert_eq!(
            nested_text("⇣ nested n1", Some("  ")),
            "⇣ nested n1 · завершён"
        );
        assert_eq!(
            nested_text("⇣ nested n1", Some(" done ")),
            "⇣ nested n1\ndone"
        );
    }
}
