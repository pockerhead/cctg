//! Agent side of the live transcript stream (TASK-016): one `transcript_read`
//! from the hub becomes one `transcript_chunk`.
//!
//! The agent runs on the device that writes the transcript, so the hub never
//! reads files over the network. The hub owns the offsets; the agent only
//! reads what it is asked, complete lines only: a last line without its
//! newline may still be being written and is left for the next read. Each
//! line becomes its [`transcript::stream_events`]; lines without events are
//! passed over but still move `to`.
//!
//! Only the session's own transcript under the Claude projects directory of
//! this device is opened: `<CLAUDE_CONFIG_DIR|~/.claude>/projects/<project>/
//! <session_id>.jsonl`, checked on the canonical path (no `..`, symlink or
//! junction way out). The hub is trusted, but the link must never become a
//! way to read other files of the device.

use std::fs::File;
use std::io::{BufRead, BufReader, Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};

use transcript::StreamEvent;

use crate::wire::{AgentMsg, StreamItem, StreamLine};

/// File bytes looked at for one chunk; more waits for the next read.
pub const MAX_CHUNK_BYTES: u64 = 4 << 20;
/// Lines with events in one chunk.
pub const MAX_CHUNK_LINES: usize = 64;
/// Items in one chunk.
pub const MAX_CHUNK_ITEMS: usize = 256;
/// Weight of one chunk's items ([`item_len`]): bytes of text plus
/// [`ITEM_OVERHEAD`] each. Even with every byte escaped as `\u00XX` the link
/// line stays under `wire::MAX_LINE`.
pub const MAX_CHUNK_TEXT: usize = 128 << 10;
const ITEM_OVERHEAD: usize = 32;
/// One prompt or note; the hub splits long ones for Telegram.
pub const MAX_ITEM_TEXT: usize = 16 << 10;
/// A tool call id longer than this is not a real one; its item is dropped.
pub const MAX_ID: usize = 256;
/// A longer line is passed over whole: it is a huge tool result, never a
/// line the stream shows.
pub const MAX_RECORD: u64 = 64 << 20;

/// `<CLAUDE_CONFIG_DIR>/projects`, or `~/.claude/projects` without it: the
/// only directory whose transcripts this device's agent reads.
pub fn projects_root() -> Option<PathBuf> {
    let var = |name: &str| {
        std::env::var(name)
            .ok()
            .filter(|value| !value.trim().is_empty())
    };
    if let Some(config) = var("CLAUDE_CONFIG_DIR") {
        return Some(PathBuf::from(config).join("projects"));
    }
    let home = if cfg!(windows) {
        var("USERPROFILE").or_else(|| var("HOME"))
    } else {
        var("HOME")
    };
    home.map(|home| PathBuf::from(home).join(".claude").join("projects"))
}

/// Reads the chunk the hub asked for. Never fails: a file that cannot be
/// opened (or is not the session's transcript under `root`) is `missing`, a
/// read error ends the chunk where it happened.
pub fn read_chunk(
    root: Option<&Path>,
    session_id: &str,
    path: &str,
    from: Option<u64>,
) -> AgentMsg {
    let asked = from.unwrap_or(0);
    let empty = |missing: bool, reset: bool| AgentMsg::TranscriptChunk {
        session_id: session_id.to_owned(),
        from: asked,
        to: asked,
        lines: Vec::new(),
        missing,
        more: false,
        reset,
    };
    let Some(mut file) = root.and_then(|root| open_transcript(root, session_id, path)) else {
        return empty(true, false);
    };
    let len = file.metadata().map(|meta| meta.len()).unwrap_or(0);
    let start = match from {
        // `None` starts at the end, but never inside a line: a last line
        // still being written (or torn) is read once it is whole.
        None => last_line_start(&mut file, len),
        Some(at) if at > len || !at_line_start(&mut file, at) => return empty(false, true),
        Some(at) => at,
    };
    if file.seek(SeekFrom::Start(start)).is_err() {
        return empty(false, false);
    }
    let mut reader = BufReader::new(file);
    let mut pos = start;
    let mut lines = Vec::new();
    let mut text = 0;
    let mut count = 0;
    let mut line = Vec::new();
    let more = loop {
        if pos - start >= MAX_CHUNK_BYTES
            || lines.len() >= MAX_CHUNK_LINES
            || count >= MAX_CHUNK_ITEMS
            || text >= MAX_CHUNK_TEXT
        {
            break pos < len;
        }
        line.clear();
        let read = match (&mut reader).take(MAX_RECORD).read_until(b'\n', &mut line) {
            Ok(0) | Err(_) => break false,
            Ok(read) => read as u64,
        };
        if !line.ends_with(b"\n") {
            if read < MAX_RECORD {
                // Still being written: left for the next read.
                break false;
            }
            match skip_line(&mut reader) {
                Some(rest) => {
                    pos += read + rest;
                    continue;
                }
                None => break false,
            }
        }
        let mut items: Vec<StreamItem> = transcript::stream_events(&String::from_utf8_lossy(&line))
            .into_iter()
            .filter_map(item)
            .collect();
        let weight: usize = items.iter().map(item_len).sum();
        if !lines.is_empty()
            && (text + weight > MAX_CHUNK_TEXT || count + items.len() > MAX_CHUNK_ITEMS)
        {
            // Does not fit behind the lines already taken: the next read.
            break true;
        }
        // Alone, a line keeps only what fits (a record no real session writes).
        let mut kept = 0;
        items.retain(|item| {
            kept += item_len(item);
            kept <= MAX_CHUNK_TEXT
        });
        items.truncate(MAX_CHUNK_ITEMS);
        pos += read;
        if !items.is_empty() {
            text += items.iter().map(item_len).sum::<usize>();
            count += items.len();
            lines.push(StreamLine { end: pos, items });
        }
    };
    AgentMsg::TranscriptChunk {
        session_id: session_id.to_owned(),
        from: start,
        to: pos,
        lines,
        missing: false,
        more,
        reset: false,
    }
}

/// The session's own transcript, opened only when its canonical path is
/// `<canonical root>/<project>/<session_id>.jsonl` and it is a file.
fn open_transcript(root: &Path, session_id: &str, path: &str) -> Option<File> {
    let plain = !session_id.is_empty()
        && session_id.len() <= 64
        && session_id
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || byte == b'-');
    if !plain {
        return None;
    }
    let name = format!("{session_id}.jsonl");
    let path = std::fs::canonicalize(path).ok()?;
    let root = std::fs::canonicalize(root).ok()?;
    let inside = path.parent().and_then(Path::parent) == Some(root.as_path())
        && path.file_name().is_some_and(|file| *file == *name);
    if !inside {
        return None;
    }
    let file = File::open(&path).ok()?;
    file.metadata().ok()?.is_file().then_some(file)
}

/// Offsets the hub keeps are ends of lines: byte `at - 1` is a newline. A
/// file where it is not was replaced or rewritten.
fn at_line_start(file: &mut File, at: u64) -> bool {
    if at == 0 {
        return true;
    }
    let mut byte = [0];
    file.seek(SeekFrom::Start(at - 1)).is_ok()
        && file.read_exact(&mut byte).is_ok()
        && byte[0] == b'\n'
}

/// The start of the line that holds byte `len - 1`, or `len` when that byte
/// is a newline: the end of the last complete line. 0 without any newline;
/// `len` when the file cannot be read.
fn last_line_start(file: &mut File, len: u64) -> u64 {
    let mut block = vec![0; 64 << 10];
    let mut end = len;
    while end > 0 {
        let begin = end.saturating_sub(block.len() as u64);
        let part = &mut block[..(end - begin) as usize];
        if file.seek(SeekFrom::Start(begin)).is_err() || file.read_exact(part).is_err() {
            return len;
        }
        if let Some(at) = part.iter().rposition(|&byte| byte == b'\n') {
            return begin + at as u64 + 1;
        }
        end = begin;
    }
    0
}

/// Discards up to and including the next newline; the bytes discarded, or
/// `None` when the file ends first (the line is still being written).
fn skip_line(reader: &mut impl BufRead) -> Option<u64> {
    let mut skipped = 0;
    loop {
        let (found, used) = match reader.fill_buf() {
            Ok([]) | Err(_) => return None,
            Ok(buf) => match buf.iter().position(|&byte| byte == b'\n') {
                Some(at) => (true, at + 1),
                None => (false, buf.len()),
            },
        };
        reader.consume(used);
        skipped += used as u64;
        if found {
            return Some(skipped);
        }
    }
}

fn item(event: StreamEvent) -> Option<StreamItem> {
    Some(match event {
        StreamEvent::Prompt(text) => StreamItem::Prompt { text: cap(text) },
        StreamEvent::Channel { message_id } => StreamItem::Channel { message_id },
        StreamEvent::Note(text) => StreamItem::Note { text: cap(text) },
        StreamEvent::Call { id, line } if id.len() <= MAX_ID => StreamItem::Call {
            id,
            line: cap(line),
        },
        StreamEvent::Result { id, error } if id.len() <= MAX_ID => StreamItem::Result {
            id,
            error: error.map(cap),
        },
        StreamEvent::Call { .. } | StreamEvent::Result { .. } => return None,
        StreamEvent::TurnEnd => StreamItem::TurnEnd,
    })
}

fn item_len(item: &StreamItem) -> usize {
    ITEM_OVERHEAD
        + match item {
            StreamItem::Prompt { text } | StreamItem::Note { text } => text.len(),
            StreamItem::Call { id, line } => id.len() + line.len(),
            StreamItem::Result { id, error } => id.len() + error.as_ref().map_or(0, String::len),
            StreamItem::Channel { .. } | StreamItem::TurnEnd | StreamItem::Other => 0,
        }
}

/// Cuts on a char boundary and marks the cut.
fn cap(mut text: String) -> String {
    if text.len() > MAX_ITEM_TEXT {
        let cut = text.floor_char_boundary(MAX_ITEM_TEXT - '\u{2026}'.len_utf8());
        text.truncate(cut);
        text.push('\u{2026}');
    }
    text
}

#[cfg(test)]
mod tests {
    use std::io::Write;

    use super::*;
    use crate::hub::testdir::TempDir;

    const SESSION: &str = "5e551017-0000-4000-8000-000000000001";

    fn prompt(text: &str) -> String {
        format!("{{\"type\":\"user\",\"message\":{{\"role\":\"user\",\"content\":\"{text}\"}}}}\n")
    }

    fn root(dir: &TempDir) -> PathBuf {
        dir.path().join("projects")
    }

    fn transcript(dir: &TempDir) -> String {
        let project = root(dir).join("C--work");
        std::fs::create_dir_all(&project).unwrap();
        let path = project.join(format!("{SESSION}.jsonl"));
        std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(&path)
            .unwrap();
        path.to_string_lossy().into_owned()
    }

    fn append(path: &str, bytes: &str) {
        let mut file = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(path)
            .unwrap();
        file.write_all(bytes.as_bytes()).unwrap();
    }

    fn read(dir: &TempDir, path: &str, from: Option<u64>) -> AgentMsg {
        read_chunk(Some(&root(dir)), SESSION, path, from)
    }

    /// (from, to, texts, missing, more, reset)
    fn texts(msg: &AgentMsg) -> (u64, u64, Vec<String>, bool, bool, bool) {
        let AgentMsg::TranscriptChunk {
            from,
            to,
            lines,
            missing,
            more,
            reset,
            ..
        } = msg
        else {
            panic!("{msg:?}");
        };
        let texts = lines
            .iter()
            .flat_map(|line| &line.items)
            .map(|item| match item {
                StreamItem::Prompt { text } => text.clone(),
                other => format!("{other:?}"),
            })
            .collect();
        (*from, *to, texts, *missing, *more, *reset)
    }

    #[test]
    fn only_complete_lines_are_read_and_a_partial_one_waits_whole() {
        let dir = TempDir::new("tail-partial");
        let path = transcript(&dir);
        let one = prompt("one");
        let two = prompt("two");
        append(&path, &one);
        append(&path, &two[..10]);
        let first = read(&dir, &path, Some(0));
        assert_eq!(
            texts(&first),
            (
                0,
                one.len() as u64,
                vec!["one".to_owned()],
                false,
                false,
                false
            )
        );
        append(&path, &two[10..]);
        let second = read(&dir, &path, Some(one.len() as u64));
        assert_eq!(
            texts(&second),
            (
                one.len() as u64,
                (one.len() + two.len()) as u64,
                vec!["two".to_owned()],
                false,
                false,
                false
            )
        );
    }

    #[test]
    fn bom_and_crlf_lines_keep_their_byte_offsets() {
        let dir = TempDir::new("tail-bom");
        let path = transcript(&dir);
        let first = format!("\u{feff}{}", prompt("one").replace('\n', "\r\n"));
        let second = prompt("two").replace('\n', "\r\n");
        append(&path, &first);
        append(&path, &second);
        let (from, to, got, ..) = texts(&read(&dir, &path, Some(0)));
        assert_eq!((from, to), (0, (first.len() + second.len()) as u64));
        assert_eq!(got, ["one", "two"]);
    }

    #[test]
    fn no_offset_starts_at_the_end_and_lines_without_events_still_move_on() {
        let dir = TempDir::new("tail-end");
        let path = transcript(&dir);
        append(&path, &prompt("history"));
        let end = std::fs::metadata(&path).unwrap().len();
        assert_eq!(
            texts(&read(&dir, &path, None)),
            (end, end, vec![], false, false, false)
        );
        let mode = "{\"type\":\"mode\",\"mode\":\"x\"}\n";
        append(&path, mode);
        assert_eq!(
            texts(&read(&dir, &path, Some(end))),
            (end, end + mode.len() as u64, vec![], false, false, false)
        );
    }

    #[test]
    fn no_offset_at_a_torn_end_starts_at_that_line_and_never_before_it() {
        let dir = TempDir::new("tail-torn-end");
        let path = transcript(&dir);
        append(&path, &prompt("history 0"));
        append(&path, &prompt("history 1"));
        let boundary = std::fs::metadata(&path).unwrap().len();
        // Longer than one backward scan block.
        let torn = prompt(&"x".repeat(100 << 10));
        append(&path, &torn[..torn.len() - 10]);
        assert_eq!(
            texts(&read(&dir, &path, None)),
            (boundary, boundary, vec![], false, false, false)
        );
        // Whole now: read from the boundary once, no reset, no history.
        append(&path, &torn[torn.len() - 10..]);
        append(&path, &prompt("new"));
        let (from, to, got, missing, more, reset) = texts(&read(&dir, &path, Some(boundary)));
        let end = std::fs::metadata(&path).unwrap().len();
        assert_eq!(
            (from, to, missing, more, reset),
            (boundary, end, false, false, false)
        );
        assert_eq!(got.len(), 2);
        assert_eq!(got[1], "new");
        assert!(!got.iter().any(|text| text.starts_with("history")));
    }

    #[test]
    fn no_offset_in_a_file_without_a_newline_starts_at_its_start() {
        let dir = TempDir::new("tail-torn-only");
        let path = transcript(&dir);
        let first = prompt("first");
        append(&path, &first[..8]);
        assert_eq!(
            texts(&read(&dir, &path, None)),
            (0, 0, vec![], false, false, false)
        );
    }

    #[test]
    fn a_missing_file_or_a_foreign_path_is_missing() {
        let dir = TempDir::new("tail-missing");
        let path = transcript(&dir);
        std::fs::remove_file(&path).unwrap();
        assert_eq!(
            texts(&read(&dir, &path, Some(5))),
            (5, 5, vec![], true, false, false)
        );
        // Right name, wrong place: outside the projects root, one level too
        // deep, through `..`, or the root of another config.
        let outside = dir.path().join("other").join("C--work");
        std::fs::create_dir_all(&outside).unwrap();
        let deep = root(&dir).join("C--work").join("sub");
        std::fs::create_dir_all(&deep).unwrap();
        for place in [outside.clone(), deep] {
            let file = place.join(format!("{SESSION}.jsonl"));
            std::fs::write(&file, prompt("private")).unwrap();
            let file = file.to_string_lossy().into_owned();
            assert!(texts(&read(&dir, &file, Some(0))).3, "{file}");
        }
        let dotdot = root(&dir)
            .join("C--work")
            .join("..")
            .join("..")
            .join("other")
            .join("C--work")
            .join(format!("{SESSION}.jsonl"));
        assert!(texts(&read(&dir, &dotdot.to_string_lossy(), Some(0))).3);
        let path = transcript(&dir);
        append(&path, &prompt("mine"));
        let other_root = dir.path().join("other");
        assert!(texts(&read_chunk(Some(&other_root), SESSION, &path, Some(0))).3);
        assert!(texts(&read_chunk(None, SESSION, &path, Some(0))).3);
        // Another session's name, or a session id that is not plain.
        assert!(
            texts(&read_chunk(
                Some(&root(&dir)),
                "other-session",
                &path,
                Some(0)
            ))
            .3
        );
        assert!(texts(&read_chunk(Some(&root(&dir)), "../x", &path, Some(0))).3);
        assert_eq!(texts(&read(&dir, &path, Some(0))).2, ["mine"]);
    }

    #[cfg(windows)]
    #[test]
    fn a_junction_out_of_the_projects_root_is_not_followed() {
        let dir = TempDir::new("tail-junction");
        let outside = dir.path().join("elsewhere");
        std::fs::create_dir_all(&outside).unwrap();
        std::fs::write(outside.join(format!("{SESSION}.jsonl")), prompt("private")).unwrap();
        std::fs::create_dir_all(root(&dir)).unwrap();
        let link = root(&dir).join("C--link");
        let made = std::process::Command::new("cmd")
            .args(["/C", "mklink", "/J"])
            .arg(&link)
            .arg(&outside)
            .output()
            .map(|out| out.status.success())
            .unwrap_or(false);
        if !made {
            eprintln!("mklink /J unavailable; skipped");
            return;
        }
        let path = link.join(format!("{SESSION}.jsonl"));
        assert!(texts(&read(&dir, &path.to_string_lossy(), Some(0))).3);
    }

    #[test]
    fn a_shorter_or_rewritten_file_is_reset_and_read_again_from_its_start() {
        let dir = TempDir::new("tail-reset");
        let path = transcript(&dir);
        let one = prompt("one");
        append(&path, &one);
        append(&path, &prompt("two"));
        let end = std::fs::metadata(&path).unwrap().len();
        // Truncated below the offset.
        std::fs::write(&path, prompt("new")).unwrap();
        assert_eq!(
            texts(&read(&dir, &path, Some(end))),
            (end, end, vec![], false, false, true)
        );
        // Replaced by a longer file whose offset falls inside a line.
        std::fs::write(&path, prompt("a much longer first line")).unwrap();
        append(&path, &prompt("more"));
        let (.., reset) = texts(&read(&dir, &path, Some(one.len() as u64)));
        assert!(reset);
        // From its start it reads like any file.
        assert_eq!(
            texts(&read(&dir, &path, Some(0))).2,
            ["a much longer first line", "more"]
        );
    }

    #[test]
    fn a_long_backlog_comes_in_bounded_chunks_in_order() {
        let dir = TempDir::new("tail-backlog");
        let path = transcript(&dir);
        for n in 0..(MAX_CHUNK_LINES + 3) {
            append(&path, &prompt(&format!("p{n}")));
        }
        let first = texts(&read(&dir, &path, Some(0)));
        assert_eq!(first.2.len(), MAX_CHUNK_LINES);
        assert!(first.4, "more");
        let second = texts(&read(&dir, &path, Some(first.1)));
        assert_eq!(second.2, ["p64", "p65", "p66"]);
        assert!(!second.4);
    }

    #[test]
    fn no_record_makes_a_chunk_longer_than_a_link_line() {
        let dir = TempDir::new("tail-frame");
        let path = transcript(&dir);
        // Control characters are escaped six times their size on the link.
        let text: String = "\u{1}".repeat(MAX_ITEM_TEXT * 2);
        let id: String = "\u{1}".repeat(MAX_ID);
        let blocks: Vec<String> = (0..2000)
            .map(|n| {
                format!(
                    "{{\"type\":\"tool_use\",\"id\":\"{}{n}\",\"name\":\"Bash\",\"input\":{{\"description\":\"{}\"}}}}",
                    &id[..MAX_ID - 4],
                    &text[..200]
                )
            })
            .collect();
        let huge_call = format!(
            "{{\"type\":\"assistant\",\"message\":{{\"role\":\"assistant\",\"stop_reason\":\"tool_use\",\"content\":[{}]}}}}\n",
            blocks.join(",")
        )
        .replace('\u{1}', "\\u0001");
        // Seven notes of a full item each: two such lines never fit together.
        let block = format!("{{\"type\":\"text\",\"text\":\"{text}\"}}");
        let note = format!(
            "{{\"type\":\"assistant\",\"message\":{{\"role\":\"assistant\",\"stop_reason\":\"tool_use\",\"content\":[{}]}}}}\n",
            vec![block; 7].join(",")
        )
        .replace('\u{1}', "\\u0001");
        for _ in 0..20 {
            append(&path, &note);
        }
        append(&path, &huge_call);
        let mut from = 0;
        let end = std::fs::metadata(&path).unwrap().len();
        let mut reads = 0;
        while from < end {
            let chunk = read(&dir, &path, Some(from));
            let bytes = crate::wire::encode(&chunk);
            assert!(bytes.len() < crate::wire::MAX_LINE, "{}", bytes.len());
            let (_, to, ..) = texts(&chunk);
            assert!(to > from, "every read moves on");
            from = to;
            reads += 1;
        }
        assert!(reads > 1);
    }
}
