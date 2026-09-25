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
//! The hub reads no file: the `Agent` calls of the parent transcript and
//! the files of a finished subagent are read by the session's agent
//! ([`crate::reads`], TASK-034). Everything here is pure. Texts never reach
//! the logs.

use std::collections::{HashMap, VecDeque};
use std::time::Duration;

use tokio::time::Instant;
use transcript::{Subagent, SubagentInput, TELEGRAM_TEXT_LIMIT, parse_subagent_meta, telegram_len};

use super::registry::cut;

/// Candidates waiting for their `Agent` call; beyond this the one whose
/// window ends first is dropped.
pub const MAX_CANDIDATES: usize = 256;
/// Handed-back reports kept until their subagent stops.
pub const MAX_REPORTS: usize = 256;
/// `Agent` calls and results one session index keeps; the oldest go first.
pub const MAX_INDEX_ENTRIES: usize = 1024;
/// The pause before a lookup at most doubles this many times.
const MAX_DOUBLINGS: u32 = 4;
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

/// What one read of a parent transcript found after its start offset
/// (the agent's `calls` answers).
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

    /// Takes the candidates of `session` whose stop came, by agent id.
    pub fn take_stopped(&mut self, session: &str) -> Vec<(String, Candidate)> {
        let mut agents: Vec<String> = self
            .by_agent
            .iter()
            .filter(|(_, candidate)| candidate.session == session && candidate.stop.is_some())
            .map(|(id, _)| id.clone())
            .collect();
        agents.sort();
        agents
            .into_iter()
            .filter_map(|id| self.by_agent.remove(&id).map(|candidate| (id, candidate)))
            .collect()
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
    /// `agent-<id>.jsonl` on the session's machine; only its agent opens it.
    pub agent_path: String,
    pub report: Option<String>,
    pub last: Option<String>,
    /// The running block's saved header; kept when the meta has no
    /// description, so the header does not change when the call is forgotten.
    pub header: Option<String>,
}

/// The finished block text from `input` and the subagent's files as read
/// by the agent ([`crate::reads`]); without files (no agent, a report) the
/// hook data alone.
pub fn body_text(input: &BodyInput, meta: Option<&str>, transcript: Option<&str>) -> String {
    let subagent = Subagent::new(SubagentInput {
        agent_id: &input.agent_id,
        agent_type: input.agent_type.as_deref(),
        description: input.description.as_deref(),
        meta,
        report: input.report.as_deref(),
        transcript,
        last_assistant_message: input.last.as_deref(),
    });
    let meta_described = meta
        .and_then(|meta| parse_subagent_meta(meta).description)
        .is_some_and(|description| !description.trim().is_empty());
    match input.header.as_deref().filter(|_| !meta_described) {
        Some(header) => match subagent.body().text() {
            "" => header.to_owned(),
            body => format!("{header}\n{body}"),
        },
        None => subagent.render(),
    }
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

    const META: &str =
        include_str!("../../../transcript/tests/fixtures/subagent_handback.meta.json");
    const SUBAGENT: &str =
        include_str!("../../../transcript/tests/fixtures/subagent_handback.jsonl");
    const REPORT: &str = "Modules: lib, render, split.";

    fn call(id: &str) -> (String, AgentCall) {
        (
            id.to_owned(),
            AgentCall {
                subagent_type: Some("Explore".into()),
                description: Some(format!("d{id}")),
            },
        )
    }

    #[test]
    fn a_call_and_its_result_link_the_agent_id_across_scans() {
        let mut index = AgentIndex::default();
        index.merge(Scan {
            path: "p".into(),
            offset: 10,
            calls: vec![call("t1")],
            links: Vec::new(),
        });
        assert_eq!(index.call("a1"), None);
        index.merge(Scan {
            path: "p".into(),
            offset: 20,
            calls: Vec::new(),
            links: vec![("a1".into(), "t1".into())],
        });
        assert_eq!(index.call("a1"), Some(&call("t1").1));
        assert_eq!(index.resume_at("p"), 20);
        assert_eq!(index.resume_at("other"), 0);
        // Another file starts the index over.
        index.merge(Scan::nothing("q".into(), 5));
        assert_eq!(index.call("a1"), None);
        assert_eq!(index.resume_at("q"), 5);
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
    fn an_index_keeps_the_newest_calls() {
        let mut index = AgentIndex::default();
        let n = MAX_INDEX_ENTRIES + 5;
        index.merge(Scan {
            path: "p".into(),
            offset: 1,
            calls: (0..n).map(|i| call(&format!("t{i}"))).collect(),
            links: (0..n).map(|i| (format!("a{i}"), format!("t{i}"))).collect(),
        });
        assert_eq!(index.calls.len(), MAX_INDEX_ENTRIES);
        assert_eq!(index.links.len(), MAX_INDEX_ENTRIES);
        assert_eq!(index.call("a0"), None);
        assert!(index.call(&format!("a{}", n - 1)).is_some());
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
            header: None,
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
    fn without_a_meta_description_the_saved_header_stays() {
        // The call is forgotten (session ended, hub restarted): only the
        // saved header still has the description.
        let mut body = input(None, Some("Final."));
        body.description = None;
        body.header = Some("↳ Explore a0000000000000002: Explore crate".into());
        assert_eq!(
            body_text(&body, None, None),
            "↳ Explore a0000000000000002: Explore crate
Final."
        );
        body.last = None;
        assert_eq!(
            body_text(&body, None, None),
            "↳ Explore a0000000000000002: Explore crate"
        );
        // A meta with a description still decides.
        body.header = Some("↳ Explore a0000000000000002: old".into());
        body.last = Some("Final.".into());
        assert_eq!(
            body_text(&body, Some(META), None),
            "↳ Explore a0000000000000002: Explore crate
Final."
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
