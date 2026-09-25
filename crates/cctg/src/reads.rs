//! Agent side of session reads (TASK-034): the hub never opens a file of
//! the machine a session runs on. It asks the session's agent with
//! `session_read` and gets `session_answer`s: a rendered `/brief` or `/full`,
//! the first ai-title, the `Agent` calls of the transcript, or the finished
//! text of a subagent block.
//!
//! Only files of the agent's own project folder are opened
//! ([`tail::OwnProject`]: the `projects/<project>` folder of its claude
//! session), at paths the agent builds from the plain ids the hub names
//! (a session id, an agent id) and checks on the canonical path (no symlink
//! or junction way out): a transcript `<project>/<session_id>.jsonl`
//! ([`tail::open_transcript`]) and its subagents' `agent-<id>.jsonl` and
//! `agent-<id>.meta.json` in `<project>/<session_id>/subagents/`. Any session
//! id of the folder is served (after `/clear` the id changes, the folder does
//! not); another project's files never are, since no path leads there, and
//! a read before the folder is found is `missing`. Only an agent with no way
//! to find its folder answers `refused` (TASK-034 decision 12: a remote hub
//! must not read other projects).
//!
//! Every answer fits in one link line: texts go in pieces of at most
//! [`PIECE`] bytes (six times that when every byte is escaped still stays
//! under `wire::MAX_LINE`), `Agent` calls in batches of at most
//! [`MAX_CALLS_WEIGHT`]. Nothing here logs a path or a text.

use std::fs::File;
use std::io::{self, BufRead, BufReader, Read, Seek, SeekFrom};

use serde_json::Value;
use transcript::Block;

use crate::hub::registry::cut;
use crate::hub::subagents::{BodyInput, body_text, is_agent_id};
use crate::tail::{OwnProject, is_plain_session_id, open_transcript};
use crate::wire::{SessionAnswer, SessionAsk, SpawnCall, SpawnLink, TranscriptView};

/// Largest transcript rendered, scanned for a title or for `Agent` calls
/// (the largest real session seen was 92 MiB).
pub const MAX_TRANSCRIPT_BYTES: u64 = 256 << 20;
/// A subagent transcript is read up to this many bytes for its block.
pub const MAX_AGENT_BYTES: u64 = 64 << 20;
const MAX_META_BYTES: u64 = 64 << 10;
/// Longest text sent for one read; a longer one is `too_large`.
pub const MAX_TEXT: usize = 16 << 20;
/// Longest piece of a text in one answer.
pub const PIECE: usize = 128 << 10;
/// Weight of the `Agent` calls and links of one answer: bytes of their
/// strings plus [`ENTRY_OVERHEAD`] each; the rest comes with the next ask.
pub const MAX_CALLS_WEIGHT: usize = 64 << 10;
const ENTRY_OVERHEAD: usize = 32;
/// `subagent_type` and `description` of a call are kept up to this many
/// UTF-16 units (the block header shows 120 characters).
const MAX_CALL_FIELD: usize = 256;
/// A tool use or agent id longer than this is not a real one.
const MAX_ID: usize = 256;
/// An ai-title is kept up to this many UTF-16 units.
const MAX_TITLE: usize = 1024;
/// `/brief` and `/full` show at most this many prompts.
pub const MAX_PROMPTS: u32 = 100;
const AGENT_TOOL: &str = "Agent";

/// Answers one `session_read`. Blocking: file IO and parsing. `project` is
/// the agent's own project folder ([`OwnProject`]); without one every read
/// is `refused`.
pub fn answer(
    project: Option<&OwnProject>,
    session_id: &str,
    ask: SessionAsk,
) -> Vec<SessionAnswer> {
    let Some(project) = project else {
        return vec![SessionAnswer::Refused];
    };
    match ask {
        SessionAsk::Render { view, prompts } => render(project, session_id, view, prompts),
        SessionAsk::Title { from } => vec![title(project, session_id, from)],
        SessionAsk::Calls { from } => vec![calls(project, session_id, from)],
        SessionAsk::Subagent {
            agent_id,
            agent_type,
            description,
            header,
            last,
        } => {
            let input = BodyInput {
                agent_id,
                agent_type,
                description,
                last,
                header,
                ..BodyInput::default()
            };
            subagent(project, session_id, &input)
        }
        SessionAsk::Other => vec![SessionAnswer::Unsupported],
    }
}

/// `text` as answers of at most [`PIECE`] bytes, cut on char boundaries;
/// an empty text is one empty piece.
pub fn pieces(text: &str) -> Vec<SessionAnswer> {
    if text.len() > MAX_TEXT {
        return vec![SessionAnswer::TooLarge];
    }
    let mut out = Vec::new();
    let mut rest = text;
    loop {
        let at = if rest.len() <= PIECE {
            rest.len()
        } else {
            rest.floor_char_boundary(PIECE)
        };
        let (head, tail) = rest.split_at(at);
        rest = tail;
        out.push(SessionAnswer::Text {
            text: head.to_owned(),
            more: !rest.is_empty(),
        });
        if rest.is_empty() {
            return out;
        }
    }
}

fn render(
    project: &OwnProject,
    session_id: &str,
    view: TranscriptView,
    prompts: u32,
) -> Vec<SessionAnswer> {
    let Some(file) = open_transcript(project, session_id) else {
        return vec![SessionAnswer::Missing];
    };
    let bytes = match read_limited(file, MAX_TRANSCRIPT_BYTES) {
        Ok(Some(bytes)) => bytes,
        Ok(None) => return vec![SessionAnswer::TooLarge],
        Err(_) => return vec![SessionAnswer::Unreadable],
    };
    // A half-written last line or stray bytes must not fail the whole read.
    let turns = transcript::parse(&String::from_utf8_lossy(&bytes));
    let slice = transcript::last_prompts(&turns, prompts.clamp(1, MAX_PROMPTS) as usize);
    let body = match view {
        TranscriptView::Brief => transcript::render_brief(slice),
        TranscriptView::Full => transcript::render_full(slice),
    };
    pieces(&body)
}

/// At most `limit` bytes; `None` when the file is larger. The length is
/// checked before reading and again after, for a file that grows meanwhile.
fn read_limited(file: File, limit: u64) -> io::Result<Option<Vec<u8>>> {
    if file.metadata()?.len() > limit {
        return Ok(None);
    }
    let mut bytes = Vec::new();
    file.take(limit + 1).read_to_end(&mut bytes)?;
    Ok((bytes.len() as u64 <= limit).then_some(bytes))
}

fn title(project: &OwnProject, session_id: &str, from: u64) -> SessionAnswer {
    let Some(mut file) = open_transcript(project, session_id) else {
        return SessionAnswer::Missing;
    };
    if file.seek(SeekFrom::Start(from)).is_err() {
        return SessionAnswer::Title {
            title: None,
            scanned: from,
        };
    }
    let (title, scanned) = first_ai_title(file, MAX_TRANSCRIPT_BYTES.saturating_sub(from));
    SessionAnswer::Title {
        title: title.map(|title| cut(&title, MAX_TITLE)),
        scanned: from + scanned,
    }
}

/// Streams `jsonl` line by line, at most `limit` bytes, until an ai-title.
/// Also returns the bytes of complete lines read: a last line without its
/// newline may still be being written and is scanned again next time.
fn first_ai_title(jsonl: impl Read, limit: u64) -> (Option<String>, u64) {
    let mut reader = BufReader::new(jsonl.take(limit));
    let mut line = Vec::new();
    let mut scanned = 0;
    loop {
        line.clear();
        let read = match reader.read_until(b'\n', &mut line) {
            Ok(0) | Err(_) => return (None, scanned),
            Ok(read) => read as u64,
        };
        if line.ends_with(b"\n") {
            scanned += read;
        }
        if let Some(title) = transcript::ai_title(&String::from_utf8_lossy(&line)) {
            return (Some(title), scanned);
        }
    }
}

fn calls(project: &OwnProject, session_id: &str, from: u64) -> SessionAnswer {
    let Some(mut file) = open_transcript(project, session_id) else {
        return SessionAnswer::Missing;
    };
    let mut found = SessionAnswer::Calls {
        offset: from,
        calls: Vec::new(),
        links: Vec::new(),
        more: false,
    };
    if file.seek(SeekFrom::Start(from)).is_ok() {
        scan_lines(file, MAX_TRANSCRIPT_BYTES.saturating_sub(from), &mut found);
    }
    found
}

/// Reads complete lines into `found` (a `calls` answer) until the weight is
/// spent. A last line without its newline may still be being written: it is
/// left for the next ask. Within one line entries beyond twice the weight are
/// dropped (no real line has more than a few).
fn scan_lines(jsonl: impl Read, limit: u64, found: &mut SessionAnswer) {
    let SessionAnswer::Calls {
        offset,
        calls,
        links,
        more,
    } = found
    else {
        return;
    };
    let mut reader = BufReader::new(jsonl.take(limit));
    let mut line = Vec::new();
    let mut weight = 0;
    loop {
        if weight >= MAX_CALLS_WEIGHT {
            *more = true;
            return;
        }
        line.clear();
        match reader.read_until(b'\n', &mut line) {
            Ok(0) | Err(_) => return,
            Ok(_) if !line.ends_with(b"\n") => return,
            Ok(read) => *offset += read as u64,
        }
        let text = String::from_utf8_lossy(&line);
        if !text.contains("\"Agent\"") && !text.contains("agentId") {
            continue;
        }
        for turn in transcript::parse(&text) {
            for block in turn.blocks {
                if weight >= 2 * MAX_CALLS_WEIGHT {
                    break;
                }
                match block {
                    Block::ToolUse { id, name, input }
                        if name == AGENT_TOOL && id.len() <= MAX_ID =>
                    {
                        let field = |key: &str| {
                            input
                                .get(key)
                                .and_then(Value::as_str)
                                .map(|text| cut(text, MAX_CALL_FIELD))
                        };
                        let call = SpawnCall {
                            id,
                            subagent_type: field("subagent_type"),
                            description: field("description"),
                        };
                        weight += ENTRY_OVERHEAD
                            + call.id.len()
                            + call.subagent_type.as_ref().map_or(0, String::len)
                            + call.description.as_ref().map_or(0, String::len);
                        calls.push(call);
                    }
                    Block::ToolResult {
                        tool_use_id,
                        agent_id: Some(agent_id),
                        ..
                    } if !agent_id.is_empty()
                        && agent_id.len() <= MAX_ID
                        && tool_use_id.len() <= MAX_ID =>
                    {
                        weight += ENTRY_OVERHEAD + agent_id.len() + tool_use_id.len();
                        links.push(SpawnLink {
                            agent_id,
                            tool_use_id,
                        });
                    }
                    _ => {}
                }
            }
        }
    }
}

/// The finished block: reads the session's `subagents/agent-<id>.jsonl`
/// and its `.meta.json` from the agent's own project folder, then the
/// library picks the body (finished transcript, last message).
fn subagent(project: &OwnProject, session_id: &str, input: &BodyInput) -> Vec<SessionAnswer> {
    if !is_agent_id(&input.agent_id) || !is_plain_session_id(session_id) {
        return vec![SessionAnswer::Missing];
    }
    let read = |name: String, cap: u64| {
        project
            .open(&[session_id, "subagents", &name])
            .and_then(|file| read_capped(file, cap))
    };
    let transcript = read(format!("agent-{}.jsonl", input.agent_id), MAX_AGENT_BYTES);
    let meta = read(
        format!("agent-{}.meta.json", input.agent_id),
        MAX_META_BYTES,
    );
    pieces(&body_text(input, meta.as_deref(), transcript.as_deref()))
}

fn read_capped(file: File, cap: u64) -> Option<String> {
    let mut bytes = Vec::new();
    file.take(cap).read_to_end(&mut bytes).ok()?;
    Some(String::from_utf8_lossy(&bytes).into_owned())
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use super::*;
    use crate::hub::testdir::TempDir;

    const SESSION: &str = "5e551017-0000-4000-8000-000000000001";
    const OTHER: &str = "5e551017-0000-4000-8000-000000000002";
    const AGENT: &str = "a0000000000000002";
    const PARENT: &str = include_str!("../../transcript/tests/fixtures/final_answer.jsonl");
    const SUBAGENT: &str = include_str!("../../transcript/tests/fixtures/subagent_handback.jsonl");
    const META: &str = include_str!("../../transcript/tests/fixtures/subagent_handback.meta.json");

    macro_rules! fixture {
        ($name:literal) => {
            include_str!(concat!("../../transcript/tests/fixtures/", $name))
        };
    }

    const FIXTURES: [&str; 7] = [
        fixture!("final_answer.jsonl"),
        fixture!("tool_use_result.jsonl"),
        fixture!("slash_command.jsonl"),
        fixture!("compact_summary.jsonl"),
        fixture!("string_content.jsonl"),
        fixture!("plain_text.jsonl"),
        fixture!("thinking_ai_title.jsonl"),
    ];

    fn root(dir: &TempDir) -> PathBuf {
        dir.path().join("projects")
    }

    /// `<root>/C--proj/<session>.jsonl` with `jsonl`.
    fn transcript(dir: &TempDir, session: &str, jsonl: &str) -> String {
        let project = root(dir).join("C--proj");
        std::fs::create_dir_all(&project).unwrap();
        let path = project.join(format!("{session}.jsonl"));
        std::fs::write(&path, jsonl).unwrap();
        path.to_string_lossy().into_owned()
    }

    /// The session's `subagents/agent-<AGENT>.jsonl` and `.meta.json`.
    fn subagent_files(dir: &TempDir, session: &str) -> String {
        let subagents = root(dir).join("C--proj").join(session).join("subagents");
        std::fs::create_dir_all(&subagents).unwrap();
        std::fs::write(subagents.join(format!("agent-{AGENT}.meta.json")), META).unwrap();
        let path = subagents.join(format!("agent-{AGENT}.jsonl"));
        std::fs::write(&path, SUBAGENT).unwrap();
        path.to_string_lossy().into_owned()
    }

    /// An agent whose own project folder is `<root>/C--proj`.
    fn ask(dir: &TempDir, session: &str, ask: SessionAsk) -> Vec<SessionAnswer> {
        let own = OwnProject::at(root(dir).join("C--proj"));
        answer(Some(&own), session, ask)
    }

    fn text(answers: &[SessionAnswer]) -> String {
        answers
            .iter()
            .map(|answer| match answer {
                SessionAnswer::Text { text, .. } => text.as_str(),
                other => panic!("{other:?}"),
            })
            .collect()
    }

    fn render_ask(view: TranscriptView, prompts: u32) -> SessionAsk {
        SessionAsk::Render { view, prompts }
    }

    fn block(agent_id: &str, last: &str) -> SessionAsk {
        SessionAsk::Subagent {
            agent_id: agent_id.into(),
            agent_type: None,
            description: None,
            header: None,
            last: Some(last.into()),
        }
    }

    #[test]
    fn renders_equal_the_library_on_fixtures() {
        for jsonl in FIXTURES {
            let dir = TempDir::new("reads-render");
            transcript(&dir, SESSION, jsonl);
            let turns = transcript::parse(jsonl);
            for (view, prompts) in [
                (TranscriptView::Brief, 3),
                (TranscriptView::Full, 2),
                (TranscriptView::Brief, 100),
            ] {
                let slice = transcript::last_prompts(&turns, prompts as usize);
                let want = match view {
                    TranscriptView::Brief => transcript::render_brief(slice),
                    TranscriptView::Full => transcript::render_full(slice),
                };
                assert_eq!(text(&ask(&dir, SESSION, render_ask(view, prompts))), want);
            }
        }
    }

    #[test]
    fn a_render_shows_at_most_the_last_hundred_prompts() {
        let dir = TempDir::new("reads-prompts");
        let jsonl: String = (0..=MAX_PROMPTS)
            .map(|i| {
                format!(
                    "{{\"type\":\"user\",\"message\":{{\"role\":\"user\",\"content\":\"p{i}\"}}}}\n"
                )
            })
            .collect();
        transcript(&dir, SESSION, &jsonl);
        let brief = text(&ask(
            &dir,
            SESSION,
            render_ask(TranscriptView::Brief, u32::MAX),
        ));
        assert!(
            brief.contains("> p1\n") && brief.contains("> p100"),
            "{brief}"
        );
        assert!(!brief.contains("> p0\n"), "{brief}");
    }

    #[test]
    fn long_texts_come_in_pieces_that_each_fit_a_link_line() {
        // Control characters are escaped six times their size on the link.
        let long = "\u{1}я".repeat(PIECE);
        let answers = pieces(&long);
        assert!(answers.len() > 2);
        assert_eq!(text(&answers), long);
        for (index, answer) in answers.iter().enumerate() {
            let SessionAnswer::Text { more, .. } = answer else {
                panic!("{answer:?}");
            };
            assert_eq!(*more, index + 1 < answers.len());
            let line = crate::wire::encode(&crate::wire::AgentMsg::SessionAnswer {
                read_id: u64::MAX,
                answer: answer.clone(),
            });
            assert!(line.len() < crate::wire::MAX_LINE, "{}", line.len());
        }
        assert_eq!(
            pieces(""),
            [SessionAnswer::Text {
                text: String::new(),
                more: false
            }]
        );
        assert_eq!(pieces(&"x".repeat(MAX_TEXT + 1)), [SessionAnswer::TooLarge]);
    }

    #[test]
    fn a_missing_transcript_or_an_id_that_is_no_plain_name_is_missing() {
        let dir = TempDir::new("reads-missing");
        transcript(&dir, SESSION, PARENT);
        let kinds: [fn() -> SessionAsk; 3] = [
            || render_ask(TranscriptView::Brief, 3),
            || SessionAsk::Title { from: 0 },
            || SessionAsk::Calls { from: 0 },
        ];
        for kind in kinds {
            // A session with no transcript in the own folder, and ids that
            // could name another file.
            for id in [OTHER, "../x", "..", "a/b", r"a\b", "", &"a".repeat(65)] {
                assert_eq!(ask(&dir, id, kind()), [SessionAnswer::Missing], "{id}");
            }
        }
        // A directory named like a transcript is not one.
        let fake = root(&dir).join("C--proj").join(format!("{OTHER}.jsonl"));
        std::fs::create_dir_all(&fake).unwrap();
        assert_eq!(
            ask(&dir, OTHER, render_ask(TranscriptView::Brief, 3)),
            [SessionAnswer::Missing]
        );
        // An ask of a newer hub.
        assert_eq!(
            ask(&dir, SESSION, SessionAsk::Other),
            [SessionAnswer::Unsupported]
        );
    }

    #[test]
    fn an_oversized_transcript_is_too_large() {
        let dir = TempDir::new("reads-large");
        let path = transcript(&dir, SESSION, "");
        let open = || File::open(&path).unwrap();
        std::fs::File::options()
            .write(true)
            .open(&path)
            .unwrap()
            .set_len(17)
            .unwrap();
        assert_eq!(read_limited(open(), 16).unwrap(), None);
        assert_eq!(read_limited(open(), 17).unwrap(), Some(vec![0; 17]));
    }

    #[test]
    fn the_ai_title_is_found_past_the_head_and_the_scan_goes_on_from_its_end() {
        let dir = TempDir::new("reads-title");
        let filler = "{\"type\":\"user\",\"message\":{\"role\":\"user\",\"content\":\"x\"}}\n";
        let mut jsonl = filler.repeat(5 * 1024 * 1024 / filler.len() + 1);
        jsonl.push_str("{\"type\":\"ai-title\",\"aiTitle\":\"Late title\"}\n");
        transcript(&dir, SESSION, &jsonl);
        assert_eq!(
            ask(&dir, SESSION, SessionAsk::Title { from: 0 }),
            [SessionAnswer::Title {
                title: Some("Late title".into()),
                scanned: jsonl.len() as u64
            }]
        );
        // Past the cap: not read.
        assert_eq!(
            first_ai_title(jsonl.as_bytes(), (jsonl.len() - 10) as u64).0,
            None
        );
        // From the end of a scan the head is skipped; a line still being
        // written is not counted as scanned.
        let head = "{\"type\":\"ai-title\",\"aiTitle\":\"Head\"}\n{\"type\":\"user\"}\n";
        let end = head.len() as u64;
        let partial = "{\"type\":\"ai-ti";
        transcript(&dir, SESSION, &format!("{head}{partial}"));
        assert_eq!(
            ask(&dir, SESSION, SessionAsk::Title { from: end }),
            [SessionAnswer::Title {
                title: None,
                scanned: end
            }]
        );
        let tail = format!("{partial}tle\",\"aiTitle\":\"Tail\"}}\n");
        transcript(&dir, SESSION, &format!("{head}{tail}"));
        assert_eq!(
            ask(&dir, SESSION, SessionAsk::Title { from: end }),
            [SessionAnswer::Title {
                title: Some("Tail".into()),
                scanned: end + tail.len() as u64
            }]
        );
    }

    fn scan_text(text: &str) -> SessionAnswer {
        let mut found = SessionAnswer::Calls {
            offset: 0,
            calls: Vec::new(),
            links: Vec::new(),
            more: false,
        };
        scan_lines(text.as_bytes(), u64::MAX, &mut found);
        found
    }

    #[test]
    fn an_agent_call_and_its_result_are_found_and_a_torn_line_waits() {
        let dir = TempDir::new("reads-calls");
        transcript(&dir, SESSION, PARENT);
        let [
            SessionAnswer::Calls {
                offset,
                calls,
                links,
                more,
            },
        ] = &ask(&dir, SESSION, SessionAsk::Calls { from: 0 })[..]
        else {
            panic!("one calls answer");
        };
        assert_eq!((*offset, *more), (PARENT.len() as u64, false));
        assert!(
            calls
                .iter()
                .any(|call| call.subagent_type.as_deref() == Some("Explore")
                    && call.description.as_deref() == Some("Explore crate"))
        );
        let link = links.iter().find(|link| link.agent_id == AGENT).unwrap();
        assert!(calls.iter().any(|call| call.id == link.tool_use_id));
        // A line still being written is left for the next ask.
        let lines: Vec<&str> = PARENT.lines().collect();
        let head = format!("{}\n{}", lines[8], &lines[9][..40]);
        let SessionAnswer::Calls { offset, links, .. } = scan_text(&head) else {
            panic!();
        };
        assert_eq!(offset, lines[8].len() as u64 + 1);
        assert!(links.is_empty());
    }

    #[test]
    fn many_calls_come_in_batches_that_each_fit_a_link_line() {
        let mut text = String::new();
        for i in 0..2000 {
            let call = serde_json::json!({
                "type": "assistant",
                "message": { "role": "assistant", "content": [{
                    "type": "tool_use", "id": format!("t{i}"), "name": "Agent",
                    "input": { "description": "\u{1}".repeat(5000), "subagent_type": "Explore" },
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
        let dir = TempDir::new("reads-calls-many");
        transcript(&dir, SESSION, &text);
        let (mut from, mut calls, mut links, mut asks) = (0, 0, 0, 0);
        loop {
            let answers = ask(&dir, SESSION, SessionAsk::Calls { from });
            let line = crate::wire::encode(&crate::wire::AgentMsg::SessionAnswer {
                read_id: 1,
                answer: answers[0].clone(),
            });
            assert!(line.len() < crate::wire::MAX_LINE, "{}", line.len());
            let SessionAnswer::Calls {
                offset,
                calls: found,
                links: linked,
                more,
            } = &answers[0]
            else {
                panic!();
            };
            assert!(found.iter().all(|call| {
                call.description
                    .as_deref()
                    .is_some_and(|d| transcript::telegram_len(d) <= MAX_CALL_FIELD)
            }));
            calls += found.len();
            links += linked.len();
            asks += 1;
            from = *offset;
            if !more {
                break;
            }
        }
        assert!(asks > 1);
        assert_eq!((calls, links, from), (2000, 2000, text.len() as u64));
    }

    #[test]
    fn the_block_text_is_read_from_the_sessions_subagent_files() {
        let dir = TempDir::new("reads-subagent");
        transcript(&dir, SESSION, PARENT);
        subagent_files(&dir, SESSION);
        let got = text(&ask(&dir, SESSION, block(AGENT, "Report handed back.")));
        assert_eq!(
            got,
            format!(
                "↳ Explore {AGENT}: Explore crate\n• Bash: List source files\n• SubagentHandback\nReport handed back."
            )
        );
        // Not this session's subagent, or another agent's name: no file is
        // read, the stop's last message is the body (a file that was read
        // would add its tool lines, since the last message is the
        // transcript's final text).
        let last = "Report handed back.";
        assert_eq!(
            text(&ask(&dir, OTHER, block(AGENT, last))),
            format!("↳ agent {AGENT}\n{last}")
        );
        let renamed = "a0000000000000009";
        assert_eq!(
            text(&ask(&dir, SESSION, block(renamed, last))),
            format!("↳ agent {renamed}\n{last}")
        );
        // An agent or session id that could name another file is refused
        // outright.
        assert_eq!(
            ask(&dir, SESSION, block("../x", last)),
            [SessionAnswer::Missing]
        );
        assert_eq!(
            ask(&dir, "..", block(AGENT, last)),
            [SessionAnswer::Missing]
        );
    }

    #[test]
    fn another_projects_session_is_never_served_and_any_session_of_the_own_one_is() {
        let dir = TempDir::new("reads-own-project");
        transcript(&dir, SESSION, PARENT);
        // A session of another project, with its transcript and subagent
        // files: the agent builds paths in its own folder only.
        let foreign = root(&dir).join("C--other");
        let subagents = foreign.join(OTHER).join("subagents");
        std::fs::create_dir_all(&subagents).unwrap();
        std::fs::write(foreign.join(format!("{OTHER}.jsonl")), PARENT).unwrap();
        std::fs::write(subagents.join(format!("agent-{AGENT}.jsonl")), SUBAGENT).unwrap();
        std::fs::write(subagents.join(format!("agent-{AGENT}.meta.json")), META).unwrap();
        for kind in [
            render_ask(TranscriptView::Brief, 3),
            SessionAsk::Title { from: 0 },
            SessionAsk::Calls { from: 0 },
        ] {
            assert_eq!(ask(&dir, OTHER, kind), [SessionAnswer::Missing]);
        }
        let last = "Report handed back.";
        assert_eq!(
            text(&ask(&dir, OTHER, block(AGENT, last))),
            format!("↳ agent {AGENT}\n{last}")
        );
        // A new session id of the own folder (after `/clear`) is served.
        transcript(&dir, OTHER, PARENT);
        assert!(matches!(
            ask(&dir, OTHER, render_ask(TranscriptView::Brief, 3))[..],
            [SessionAnswer::Text { .. }]
        ));
        // No own project folder to find: nothing is served.
        assert_eq!(
            answer(None, OTHER, render_ask(TranscriptView::Brief, 3)),
            [SessionAnswer::Refused]
        );
    }

    #[test]
    fn before_the_own_folder_is_found_reads_are_missing_and_then_served() {
        let dir = TempDir::new("reads-unfound");
        std::fs::create_dir_all(root(&dir)).unwrap();
        // A new session in a folder Claude Code names otherwise (a junction,
        // a cut name): nothing named after the cwd exists.
        let own = OwnProject::new(root(&dir), Some(SESSION), Some(r"C:\nowhere")).unwrap();
        let brief = || render_ask(TranscriptView::Brief, 3);
        assert_eq!(
            answer(Some(&own), SESSION, brief()),
            [SessionAnswer::Missing]
        );
        // Claude Code writes the first record into its own folder.
        transcript(&dir, SESSION, PARENT);
        assert!(matches!(
            answer(Some(&own), SESSION, brief())[..],
            [SessionAnswer::Text { .. }]
        ));
        assert_eq!(own.folder(), Some(root(&dir).join("C--proj")));
    }

    #[cfg(windows)]
    #[test]
    fn an_id_cased_unlike_the_files_name_is_missing() {
        let dir = TempDir::new("reads-case");
        transcript(&dir, SESSION, PARENT);
        assert_eq!(
            ask(
                &dir,
                &SESSION.to_uppercase(),
                render_ask(TranscriptView::Brief, 3)
            ),
            [SessionAnswer::Missing]
        );
    }

    #[cfg(windows)]
    #[test]
    fn a_junction_out_of_the_session_is_not_followed() {
        let dir = TempDir::new("reads-junction");
        transcript(&dir, SESSION, PARENT);
        // Another session's subagents, reached through a junction named
        // like this session's folder.
        let elsewhere = subagent_files(&dir, OTHER);
        let session_link = root(&dir).join("C--proj").join(SESSION);
        let made = std::process::Command::new("cmd")
            .args(["/C", "mklink", "/J"])
            .arg(&session_link)
            .arg(root(&dir).join("C--proj").join(OTHER))
            .output()
            .map(|out| out.status.success())
            .unwrap_or(false);
        if !made {
            eprintln!("mklink /J unavailable; skipped");
            return;
        }
        assert!(std::path::Path::new(&elsewhere).exists());
        let got = text(&ask(&dir, SESSION, block(AGENT, "Final.")));
        assert_eq!(got, format!("↳ agent {AGENT}\nFinal."));
    }
}
