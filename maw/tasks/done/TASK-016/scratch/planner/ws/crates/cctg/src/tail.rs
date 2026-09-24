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
//! Only a path that names this kind of file is opened:
//! `.../projects/<project>/<session_id>.jsonl`. The hub is trusted, but the
//! link must never become a way to read other files of the device.

use std::fs::File;
use std::io::{BufRead, BufReader, Read, Seek, SeekFrom};
use std::path::Path;

use transcript::StreamEvent;

use crate::wire::{AgentMsg, StreamItem, StreamLine};

/// File bytes looked at for one chunk; more waits for the next read.
pub const MAX_CHUNK_BYTES: u64 = 4 << 20;
/// Lines with events in one chunk.
pub const MAX_CHUNK_LINES: usize = 64;
/// Text of one chunk's items, in bytes; with `\u` escapes the link line stays
/// well under `wire::MAX_LINE`.
pub const MAX_CHUNK_TEXT: usize = 128 << 10;
/// One prompt or note; the hub splits long ones for Telegram.
pub const MAX_ITEM_TEXT: usize = 16 << 10;
/// A longer line is passed over whole: it is a huge tool result, never a
/// line the stream shows.
pub const MAX_RECORD: u64 = 64 << 20;

/// Reads the chunk the hub asked for. Never fails: a file that cannot be
/// opened is `missing`, a read error ends the chunk where it happened.
pub fn read_chunk(session_id: &str, path: &str, from: Option<u64>) -> AgentMsg {
    let empty = |at: u64, missing: bool| AgentMsg::TranscriptChunk {
        session_id: session_id.to_owned(),
        from: at,
        to: at,
        lines: Vec::new(),
        missing,
        more: false,
    };
    let asked = from.unwrap_or(0);
    if !is_transcript_path(session_id, path) {
        return empty(asked, true);
    }
    let Ok(mut file) = File::open(path) else {
        return empty(asked, true);
    };
    let len = file.metadata().map(|meta| meta.len()).unwrap_or(0);
    // `None` starts at the end; a file that shrank is read from its end.
    let start = from.filter(|&at| at <= len).unwrap_or(len);
    if file.seek(SeekFrom::Start(start)).is_err() {
        return empty(start, false);
    }
    let mut reader = BufReader::new(file);
    let mut pos = start;
    let mut lines = Vec::new();
    let mut text = 0;
    let mut line = Vec::new();
    let more = loop {
        if pos - start >= MAX_CHUNK_BYTES
            || lines.len() >= MAX_CHUNK_LINES
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
        pos += read;
        let items: Vec<StreamItem> = transcript::stream_events(&String::from_utf8_lossy(&line))
            .into_iter()
            .map(item)
            .collect();
        if !items.is_empty() {
            text += items.iter().map(item_len).sum::<usize>();
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
    }
}

/// `<...>/projects/<project>/<session_id>.jsonl` with a plain session id.
pub fn is_transcript_path(session_id: &str, path: &str) -> bool {
    let plain = !session_id.is_empty()
        && session_id.len() <= 64
        && session_id
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || byte == b'-');
    let path = Path::new(path);
    let named = path
        .file_name()
        .is_some_and(|name| *name == *format!("{session_id}.jsonl"));
    let in_projects = path
        .parent()
        .and_then(Path::parent)
        .and_then(Path::file_name)
        .is_some_and(|name| name == "projects");
    let plain_path = path
        .components()
        .all(|part| !matches!(part, std::path::Component::ParentDir));
    plain && named && in_projects && plain_path
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

fn item(event: StreamEvent) -> StreamItem {
    match event {
        StreamEvent::Prompt(text) => StreamItem::Prompt { text: cap(text) },
        StreamEvent::Channel { message_id } => StreamItem::Channel { message_id },
        StreamEvent::Note(text) => StreamItem::Note { text: cap(text) },
        StreamEvent::Call { id, line } => StreamItem::Call { id, line },
        StreamEvent::Result { id, error } => StreamItem::Result { id, error },
    }
}

fn item_len(item: &StreamItem) -> usize {
    match item {
        StreamItem::Prompt { text } | StreamItem::Note { text } => text.len(),
        StreamItem::Call { id, line } => id.len() + line.len(),
        StreamItem::Result { id, error } => id.len() + error.as_ref().map_or(0, String::len),
        StreamItem::Channel { .. } | StreamItem::Other => 8,
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

    fn transcript(dir: &TempDir) -> String {
        let project = dir.path().join("projects").join("C--work");
        std::fs::create_dir_all(&project).unwrap();
        project
            .join(format!("{SESSION}.jsonl"))
            .to_string_lossy()
            .into_owned()
    }

    fn append(path: &str, bytes: &str) {
        let mut file = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(path)
            .unwrap();
        file.write_all(bytes.as_bytes()).unwrap();
    }

    fn texts(msg: &AgentMsg) -> (u64, u64, Vec<String>, bool, bool) {
        let AgentMsg::TranscriptChunk {
            from,
            to,
            lines,
            missing,
            more,
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
        (*from, *to, texts, *missing, *more)
    }

    #[test]
    fn only_complete_lines_are_read_and_a_partial_one_waits_whole() {
        let dir = TempDir::new("tail-partial");
        let path = transcript(&dir);
        let one = prompt("one");
        let two = prompt("two");
        append(&path, &one);
        append(&path, &two[..10]);
        let first = read_chunk(SESSION, &path, Some(0));
        assert_eq!(
            texts(&first),
            (0, one.len() as u64, vec!["one".to_owned()], false, false)
        );
        append(&path, &two[10..]);
        let second = read_chunk(SESSION, &path, Some(one.len() as u64));
        assert_eq!(
            texts(&second),
            (
                one.len() as u64,
                (one.len() + two.len()) as u64,
                vec!["two".to_owned()],
                false,
                false
            )
        );
    }

    #[test]
    fn no_offset_starts_at_the_end_and_lines_without_events_still_move_on() {
        let dir = TempDir::new("tail-end");
        let path = transcript(&dir);
        append(&path, &prompt("history"));
        let end = std::fs::metadata(&path).unwrap().len();
        assert_eq!(
            texts(&read_chunk(SESSION, &path, None)),
            (end, end, vec![], false, false)
        );
        let mode = "{\"type\":\"mode\",\"mode\":\"x\"}\n";
        append(&path, mode);
        assert_eq!(
            texts(&read_chunk(SESSION, &path, Some(end))),
            (end, end + mode.len() as u64, vec![], false, false)
        );
    }

    #[test]
    fn a_missing_file_or_a_foreign_path_is_missing() {
        let dir = TempDir::new("tail-missing");
        let path = transcript(&dir);
        assert_eq!(
            texts(&read_chunk(SESSION, &path, Some(5))),
            (5, 5, vec![], true, false)
        );
        let secret = dir.path().join("secret.jsonl");
        std::fs::write(&secret, prompt("private")).unwrap();
        let foreign = secret.to_string_lossy().into_owned();
        assert!(texts(&read_chunk(SESSION, &foreign, Some(0))).3);
        assert!(!is_transcript_path("../x", &path));
        assert!(!is_transcript_path(
            SESSION,
            &format!("/a/projects/p/../../etc/{SESSION}.jsonl")
        ));
        assert!(!is_transcript_path("other-session", &path));
        assert!(is_transcript_path(SESSION, &path));
    }

    #[test]
    fn a_long_backlog_comes_in_bounded_chunks_in_order() {
        let dir = TempDir::new("tail-backlog");
        let path = transcript(&dir);
        for n in 0..(MAX_CHUNK_LINES + 3) {
            append(&path, &prompt(&format!("p{n}")));
        }
        let first = texts(&read_chunk(SESSION, &path, Some(0)));
        assert_eq!(first.2.len(), MAX_CHUNK_LINES);
        assert!(first.4, "more");
        let second = texts(&read_chunk(SESSION, &path, Some(first.1)));
        assert_eq!(second.2, ["p64", "p65", "p66"]);
        assert!(!second.4);
    }
}
