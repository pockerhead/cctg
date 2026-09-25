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
//! Only a transcript in the agent's own project folder is opened
//! ([`OwnProject`]: `<CLAUDE_CONFIG_DIR|~/.claude>/projects/<project>` of
//! its claude session): `<project>/<session_id>.jsonl`, a path the agent
//! builds from the session id alone (the `path` of a `transcript_read` is
//! never opened) and checks on the canonical path (no symlink or junction
//! way out). Any session id in that folder is served (after `/clear` the id
//! changes, the folder does not); another project's sessions never are
//! (TASK-034 decision 12: the hub may run on another machine).

use std::fs::File;
use std::io::{BufRead, BufReader, Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};
use std::sync::OnceLock;

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

/// Claude Code keeps a longer project folder name to this many characters
/// and adds `-<hash>`.
const MAX_FOLDER_NAME: usize = 200;

/// The agent's own project folder `<projects root>/<project>` (TASK-034
/// decision 12): where Claude Code keeps the transcript of the agent's claude
/// session (its env `CLAUDE_CODE_SESSION_ID`). Found, not computed: the
/// folder that holds `<session id>.jsonl` ([`Self::folder`]), looked for on
/// each read until it is there and then kept. Until then Claude Code's names
/// for the cwd ([`project_folder_name`] of the resolved and the given cwd)
/// stand in, never kept: they serve a session that `/clear`ed before its
/// first record, whose env id never gets a transcript.
#[derive(Debug)]
pub struct OwnProject {
    root: PathBuf,
    session_id: String,
    by_cwd: Vec<PathBuf>,
    found: OnceLock<PathBuf>,
}

impl OwnProject {
    /// Under the projects directory `root`, for the claude session
    /// `session_id` started in `cwd`. `None` without a plain session id: the
    /// agent then reads nothing. Blocking: it resolves `cwd`.
    pub fn new(root: PathBuf, session_id: Option<&str>, cwd: Option<&str>) -> Option<Self> {
        let session_id = session_id.filter(|id| is_plain_session_id(id))?.to_owned();
        let mut by_cwd = Vec::new();
        if let Some(cwd) = cwd.filter(|cwd| !cwd.is_empty()) {
            // Claude Code names the folder after the resolved cwd; the given
            // spelling comes second (the case of a drive letter changes the
            // hash of a long name).
            for spelling in [crate::device::canonical_cwd(cwd), cwd.to_owned()] {
                let folder = root.join(project_folder_name(&spelling));
                if !by_cwd.contains(&folder) {
                    by_cwd.push(folder);
                }
            }
        }
        Some(Self {
            root,
            session_id,
            by_cwd,
            found: OnceLock::new(),
        })
    }

    /// A folder known already (tests), kept from the start.
    pub fn at(folder: PathBuf) -> Self {
        let root = folder.parent().map(Path::to_path_buf).unwrap_or_default();
        Self {
            root,
            session_id: String::new(),
            by_cwd: Vec::new(),
            found: OnceLock::from(folder),
        }
    }

    /// The folder to read from now: the one that holds the session's
    /// transcript (kept once found), else an existing folder named after the
    /// cwd, else none. Blocking: it may list the projects root.
    pub fn folder(&self) -> Option<PathBuf> {
        if let Some(found) = self.found.get() {
            return Some(found.clone());
        }
        let file = format!("{}.jsonl", self.session_id);
        let holds = |folder: &Path| folder.join(&file).is_file();
        let found = self
            .by_cwd
            .iter()
            .find(|folder| holds(folder))
            .cloned()
            .or_else(|| {
                let mut holding = std::fs::read_dir(&self.root)
                    .ok()?
                    .flatten()
                    .map(|entry| entry.path())
                    .filter(|folder| holds(folder));
                let first = holding.next()?;
                // Claude Code's own lookup gives up on an id in two folders.
                holding.next().is_none().then_some(first)
            });
        match found {
            Some(found) => Some(self.found.get_or_init(|| found).clone()),
            None => self.by_cwd.iter().find(|folder| folder.is_dir()).cloned(),
        }
    }

    /// Opens `<folder>/<parts>` only when the folder sits right in the
    /// projects root, the canonical path is exactly `<canonical
    /// folder>/<parts>` (no link, `..` or other spelling on the way) and it
    /// is a file. `parts` are plain names (session and agent ids checked by
    /// the caller): the agent builds every path itself and never opens one
    /// the hub sent. Blocking.
    pub(crate) fn open(&self, parts: &[&str]) -> Option<File> {
        let folder = self.folder()?;
        let own = std::fs::canonicalize(&folder).ok()?;
        let root = std::fs::canonicalize(&self.root).ok()?;
        if own.parent() != Some(root.as_path()) {
            return None;
        }
        let (mut path, mut want) = (folder, own);
        for part in parts {
            path.push(part);
            want.push(part);
        }
        if std::fs::canonicalize(&path).ok()? != want {
            return None;
        }
        let file = File::open(&want).ok()?;
        file.metadata().ok()?.is_file().then_some(file)
    }
}

/// Claude Code's project folder name for a working folder (its `dx`, read
/// from the 2.1.28x binary in TASK-034): every UTF-16 unit that is not an
/// ASCII letter or digit becomes `-` (`C:\Users\a_b` is `C--Users-a-b`); a
/// name longer than 200 keeps its first 200 characters plus `-<hash>`.
pub fn project_folder_name(cwd: &str) -> String {
    let name: String = cwd
        .encode_utf16()
        .map(|unit| match char::from_u32(u32::from(unit)) {
            Some(c) if c.is_ascii_alphanumeric() => c,
            _ => '-',
        })
        .collect();
    if name.len() <= MAX_FOLDER_NAME {
        return name;
    }
    let hash = base36(js_hash(cwd).unsigned_abs());
    format!("{}-{hash}", &name[..MAX_FOLDER_NAME])
}

/// JavaScript's `h = (h << 5) - h + s.charCodeAt(i) | 0` over the UTF-16
/// units of `text`.
fn js_hash(text: &str) -> i32 {
    text.encode_utf16().fold(0, |hash: i32, unit| {
        hash.wrapping_shl(5)
            .wrapping_sub(hash)
            .wrapping_add(i32::from(unit))
    })
}

/// JavaScript's `n.toString(36)`.
fn base36(mut n: u32) -> String {
    let mut digits = Vec::new();
    loop {
        digits.extend(char::from_digit(n % 36, 36));
        n /= 36;
        if n == 0 {
            return digits.into_iter().rev().collect();
        }
    }
}

/// Reads the chunk the hub asked for from `<own folder>/<session_id>.jsonl`
/// ([`open_transcript`]; the hub's `path` is never used). Never fails: a
/// file that cannot be opened is `missing`, a read error ends the chunk
/// where it happened.
pub fn read_chunk(project: Option<&OwnProject>, session_id: &str, from: Option<u64>) -> AgentMsg {
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
    let Some(mut file) = project.and_then(|project| open_transcript(project, session_id)) else {
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

/// The transcript `<own folder>/<session_id>.jsonl` of the agent's own
/// project folder ([`OwnProject::open`]), any session id of that folder
/// (after `/clear` the id changes, the folder does not).
pub(crate) fn open_transcript(project: &OwnProject, session_id: &str) -> Option<File> {
    if !is_plain_session_id(session_id) {
        return None;
    }
    project.open(&[&format!("{session_id}.jsonl")])
}

/// A session id that can only name one file: letters, digits and dashes.
pub(crate) fn is_plain_session_id(session_id: &str) -> bool {
    !session_id.is_empty()
        && session_id.len() <= 64
        && session_id
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || byte == b'-')
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
        StreamEvent::Thinking(text) => StreamItem::Thinking { text: cap(text) },
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
            StreamItem::Prompt { text }
            | StreamItem::Note { text }
            | StreamItem::Thinking { text } => text.len(),
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

    /// The agent's own project folder in these tests.
    fn project(dir: &TempDir) -> PathBuf {
        root(dir).join("C--work")
    }

    fn read(dir: &TempDir, from: Option<u64>) -> AgentMsg {
        read_chunk(Some(&OwnProject::at(project(dir))), SESSION, from)
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
        let first = read(&dir, Some(0));
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
        let second = read(&dir, Some(one.len() as u64));
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
        let (from, to, got, ..) = texts(&read(&dir, Some(0)));
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
            texts(&read(&dir, None)),
            (end, end, vec![], false, false, false)
        );
        let mode = "{\"type\":\"mode\",\"mode\":\"x\"}\n";
        append(&path, mode);
        assert_eq!(
            texts(&read(&dir, Some(end))),
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
            texts(&read(&dir, None)),
            (boundary, boundary, vec![], false, false, false)
        );
        // Whole now: read from the boundary once, no reset, no history.
        append(&path, &torn[torn.len() - 10..]);
        append(&path, &prompt("new"));
        let (from, to, got, missing, more, reset) = texts(&read(&dir, Some(boundary)));
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
            texts(&read(&dir, None)),
            (0, 0, vec![], false, false, false)
        );
    }

    #[test]
    fn a_missing_file_or_an_id_that_is_no_plain_name_is_missing() {
        let dir = TempDir::new("tail-missing");
        let path = transcript(&dir);
        std::fs::remove_file(&path).unwrap();
        assert_eq!(
            texts(&read(&dir, Some(5))),
            (5, 5, vec![], true, false, false)
        );
        let path = transcript(&dir);
        append(&path, &prompt("mine"));
        let own = OwnProject::at(project(&dir));
        assert!(texts(&read_chunk(None, SESSION, Some(0))).3);
        // Another session's name, or ids that could name another file.
        for id in ["other-session", "../x", "..", "a/b", r"a\b", ""] {
            assert!(texts(&read_chunk(Some(&own), id, Some(0))).3, "{id}");
        }
        // The projects directory itself is no project folder.
        let root_itself = OwnProject::at(root(&dir));
        assert!(texts(&read_chunk(Some(&root_itself), SESSION, Some(0))).3);
        assert_eq!(texts(&read(&dir, Some(0))).2, ["mine"]);
    }

    #[test]
    fn only_transcripts_of_the_agents_own_project_folder_are_served() {
        let dir = TempDir::new("tail-own-project");
        let mine = transcript(&dir);
        append(&mine, &prompt("mine"));
        // Another project's session: the agent never looks outside its
        // own folder.
        let other = "5e551017-0000-4000-8000-000000000003";
        let foreign = root(&dir).join("C--other");
        std::fs::create_dir_all(&foreign).unwrap();
        std::fs::write(foreign.join(format!("{other}.jsonl")), prompt("private")).unwrap();
        let own = OwnProject::at(project(&dir));
        assert!(texts(&read_chunk(Some(&own), other, Some(0))).3);
        // A new session id in the same folder (after `/clear`) is served.
        let cleared = "5e551017-0000-4000-8000-000000000002";
        let next = project(&dir).join(format!("{cleared}.jsonl"));
        std::fs::write(&next, prompt("after clear")).unwrap();
        let got = read_chunk(Some(&own), cleared, Some(0));
        assert_eq!(texts(&got).2, ["after clear"]);
    }

    #[test]
    fn claude_codes_folder_names_are_matched_long_ones_cut_with_its_hash() {
        assert_eq!(
            project_folder_name(r"C:\Users\a_b\my dev.x"),
            "C--Users-a-b-my-dev-x"
        );
        assert_eq!(project_folder_name("/home/я/w"), "-home---w");
        // Expected names from Claude Code's own `dx`, run under node
        // (TASK-034 scratch/fixer/claude_folder_name.js).
        let long = format!(r"C:\work\{}", "x".repeat(220));
        assert_eq!(
            project_folder_name(&long),
            format!("C--work-{}-5dl4ti", "x".repeat(192))
        );
        let negative_hash = format!(r"D:\{}", "y".repeat(230));
        assert_eq!(
            project_folder_name(&negative_hash),
            format!("D--{}-ue235m", "y".repeat(197))
        );
        let wide = format!("/home/я/{}", "проект-😀-".repeat(30));
        assert_eq!(
            project_folder_name(&wide),
            format!("-home{}-onm1xj", "-".repeat(195))
        );
        assert_eq!(project_folder_name(&"a".repeat(200)), "a".repeat(200));
    }

    #[test]
    fn the_own_folder_is_found_by_the_sessions_transcript_and_then_kept() {
        let dir = TempDir::new("tail-own-find");
        let root = root(&dir);
        std::fs::create_dir_all(&root).unwrap();
        let cwd = r"C:\Users\a_b\my dev.x";
        let by_cwd = root.join("C--Users-a-b-my-dev-x");
        // A new session: nothing written, no folder named after the cwd.
        let own = OwnProject::new(root.clone(), Some(SESSION), Some(cwd)).unwrap();
        assert_eq!(own.folder(), None);
        // Once the cwd's folder exists it stands in, not kept.
        std::fs::create_dir_all(&by_cwd).unwrap();
        assert_eq!(own.folder(), Some(by_cwd.clone()));
        // Claude Code wrote the session's first record elsewhere (a
        // junction cwd, a shortened name): that folder is found and kept.
        let real = root.join("C--real-folder");
        std::fs::create_dir_all(&real).unwrap();
        std::fs::write(real.join(format!("{SESSION}.jsonl")), prompt("one")).unwrap();
        assert_eq!(own.folder(), Some(real.clone()));
        std::fs::write(by_cwd.join(format!("{SESSION}.jsonl")), prompt("x")).unwrap();
        assert_eq!(own.folder(), Some(real.clone()));
        // A resumed session starts with its transcript's folder, the cwd's
        // own folder comes first when it holds it.
        let resumed = OwnProject::new(root.clone(), Some(SESSION), Some(cwd)).unwrap();
        assert_eq!(resumed.folder(), Some(by_cwd));
        let resumed = OwnProject::new(root.clone(), Some(SESSION), None).unwrap();
        assert_eq!(resumed.folder(), None, "an id in two folders is no answer");
        std::fs::remove_file(real.join(format!("{SESSION}.jsonl"))).unwrap();
        assert!(resumed.folder().is_some());
        // Nothing to find it by: no reads at all.
        assert!(OwnProject::new(root.clone(), None, Some(cwd)).is_none());
        assert!(OwnProject::new(root.clone(), Some("../x"), Some(cwd)).is_none());
    }

    #[test]
    fn a_new_session_with_a_long_cwd_is_served_from_claude_codes_folder() {
        let dir = TempDir::new("tail-own-long");
        let root = root(&dir);
        std::fs::create_dir_all(&root).unwrap();
        let cwd = format!(r"C:\work\{}", "x".repeat(220));
        let own = OwnProject::new(root.clone(), Some(SESSION), Some(&cwd)).unwrap();
        // Claude Code writes the first record under its cut name.
        let folder = root.join(format!("C--work-{}-5dl4ti", "x".repeat(192)));
        std::fs::create_dir_all(&folder).unwrap();
        let path = folder.join(format!("{SESSION}.jsonl"));
        append(&path.to_string_lossy(), &prompt("long"));
        assert_eq!(texts(&read_chunk(Some(&own), SESSION, Some(0))).2, ["long"]);
        // Before its first record a `/clear`ed session is served from it too.
        let cleared = "5e551017-0000-4000-8000-000000000002";
        std::fs::remove_file(&path).unwrap();
        let fresh = OwnProject::new(root, Some(SESSION), Some(&cwd)).unwrap();
        append(
            &folder.join(format!("{cleared}.jsonl")).to_string_lossy(),
            &prompt("after clear"),
        );
        assert_eq!(
            texts(&read_chunk(Some(&fresh), cleared, Some(0))).2,
            ["after clear"]
        );
    }

    #[cfg(windows)]
    #[test]
    fn a_new_session_under_a_junction_is_served_from_its_resolved_folder() {
        let dir = TempDir::new("tail-own-junction");
        let real = dir.path().join("real").join("proj");
        std::fs::create_dir_all(&real).unwrap();
        let link = dir.path().join("link");
        let made = std::process::Command::new("cmd")
            .args(["/C", "mklink", "/J"])
            .arg(&link)
            .arg(dir.path().join("real"))
            .output()
            .map(|out| out.status.success())
            .unwrap_or(false);
        if !made {
            eprintln!("mklink /J unavailable; skipped");
            return;
        }
        let root = root(&dir);
        std::fs::create_dir_all(&root).unwrap();
        // Agent start: the cwd as given goes through the junction, nothing
        // is written yet.
        let raw_cwd = link.join("proj").to_string_lossy().into_owned();
        let own = OwnProject::new(root.clone(), Some(SESSION), Some(&raw_cwd)).unwrap();
        // Claude Code names its folder after the resolved cwd.
        let resolved = crate::device::canonical_cwd(&real.to_string_lossy());
        let folder = root.join(project_folder_name(&resolved));
        assert_ne!(folder, root.join(project_folder_name(&raw_cwd)));
        std::fs::create_dir_all(&folder).unwrap();
        append(
            &folder.join(format!("{SESSION}.jsonl")).to_string_lossy(),
            &prompt("real"),
        );
        assert_eq!(texts(&read_chunk(Some(&own), SESSION, Some(0))).2, ["real"]);
    }

    /// `link` -> `target`, a folder link: a symlink on Unix, a junction on
    /// Windows. False when it cannot be made.
    fn folder_link(link: &Path, target: &Path) -> bool {
        #[cfg(unix)]
        {
            std::os::unix::fs::symlink(target, link).is_ok()
        }
        #[cfg(windows)]
        {
            std::process::Command::new("cmd")
                .args(["/C", "mklink", "/J"])
                .arg(link)
                .arg(target)
                .output()
                .map(|out| out.status.success())
                .unwrap_or(false)
        }
    }

    /// The macOS shape of TASK-031 CI run 2 on any OS: the cwd is reached
    /// through a link (there the temp dir `/var` -> `/private/var`), so the
    /// linked spelling names another folder than Claude Code's (its
    /// `realpathSync(process.cwd())`), and another project's folder holds
    /// the same session id, so the id alone finds nothing. The agent serves
    /// Claude Code's folder whether its cwd comes resolved (Unix `getcwd`)
    /// or as given (Windows keeps the linked spelling).
    #[test]
    fn a_linked_cwd_is_served_from_the_resolved_folder_when_another_holds_the_id() {
        let dir = TempDir::new("tail-own-link");
        let real = dir.path().join("real");
        std::fs::create_dir_all(real.join("w")).unwrap();
        let link = dir.path().join("link");
        if !folder_link(&link, &real) {
            eprintln!("no folder link; skipped");
            return;
        }
        let root = root(&dir);
        std::fs::create_dir_all(&root).unwrap();
        let linked = link.join("w").to_string_lossy().into_owned();
        let resolved = crate::device::canonical_cwd(&linked);
        let claude = root.join(project_folder_name(&resolved));
        assert_ne!(claude, root.join(project_folder_name(&linked)));
        std::fs::create_dir_all(&claude).unwrap();
        std::fs::write(claude.join(format!("{SESSION}.jsonl")), prompt("own")).unwrap();
        let foreign = root.join("C--another-project");
        std::fs::create_dir_all(&foreign).unwrap();
        std::fs::write(foreign.join(format!("{SESSION}.jsonl")), prompt("private")).unwrap();
        for cwd in [&resolved, &linked] {
            let own = OwnProject::new(root.clone(), Some(SESSION), Some(cwd)).unwrap();
            assert_eq!(
                texts(&read_chunk(Some(&own), SESSION, Some(0))).2,
                ["own"],
                "{cwd}"
            );
        }
        // The CI failure itself: a transcript in a folder named after the
        // linked spelling is not the agent's own when its cwd comes
        // resolved, and the id in two folders finds none: "не найден".
        std::fs::remove_file(claude.join(format!("{SESSION}.jsonl"))).unwrap();
        let by_link = root.join(project_folder_name(&linked));
        std::fs::create_dir_all(&by_link).unwrap();
        std::fs::write(by_link.join(format!("{SESSION}.jsonl")), prompt("linked")).unwrap();
        let own = OwnProject::new(root.clone(), Some(SESSION), Some(&resolved)).unwrap();
        assert!(texts(&read_chunk(Some(&own), SESSION, Some(0))).3);
    }

    #[cfg(windows)]
    #[test]
    fn the_own_folder_named_in_another_case_than_on_disk_is_served() {
        // Claude Code may have named the folder after a `c:\` spelling.
        let dir = TempDir::new("tail-own-case");
        let root = root(&dir);
        let on_disk = root.join("c--users-a-proj");
        std::fs::create_dir_all(&on_disk).unwrap();
        append(
            &on_disk.join(format!("{SESSION}.jsonl")).to_string_lossy(),
            &prompt("cased"),
        );
        let own = OwnProject::new(root, Some(SESSION), Some(r"C:\Users\a\proj")).unwrap();
        assert_eq!(
            texts(&read_chunk(Some(&own), SESSION, Some(0))).2,
            ["cased"]
        );
    }

    #[cfg(windows)]
    #[test]
    fn a_junction_out_of_the_projects_root_is_not_followed() {
        let dir = TempDir::new("tail-junction");
        transcript(&dir);
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
        // An own folder that is a junction out of the projects root.
        let own = OwnProject::at(link);
        assert!(texts(&read_chunk(Some(&own), SESSION, Some(0))).3);
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
            texts(&read(&dir, Some(end))),
            (end, end, vec![], false, false, true)
        );
        // Replaced by a longer file whose offset falls inside a line.
        std::fs::write(&path, prompt("a much longer first line")).unwrap();
        append(&path, &prompt("more"));
        let (.., reset) = texts(&read(&dir, Some(one.len() as u64)));
        assert!(reset);
        // From its start it reads like any file.
        assert_eq!(
            texts(&read(&dir, Some(0))).2,
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
        let first = texts(&read(&dir, Some(0)));
        assert_eq!(first.2.len(), MAX_CHUNK_LINES);
        assert!(first.4, "more");
        let second = texts(&read(&dir, Some(first.1)));
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
            let chunk = read(&dir, Some(from));
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
