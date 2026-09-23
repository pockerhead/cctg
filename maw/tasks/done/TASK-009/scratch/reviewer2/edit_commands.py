"""Applies the reviewer-2 changes to ws/crates/cctg/src/hub/commands.rs."""
import io, os

HERE = os.path.dirname(os.path.abspath(__file__))
p = os.path.join(HERE, 'ws', 'crates', 'cctg', 'src', 'hub', 'commands.rs')
s = io.open(p, encoding='utf-8', newline='').read()


def rep(old, new):
    global s
    assert s.count(old) == 1, (old[:60], s.count(old))
    s = s.replace(old, new)


rep("""//! path or a project directory name.

use std::sync::Arc;
""", """//! path or a project directory name.

use std::io::{self, Read};
use std::path::Path;
use std::sync::Arc;
""")
rep("""const SHORT_ID_LEN: usize = 8;
""", """const SHORT_ID_LEN: usize = 8;
/// Largest transcript `/brief` and `/full` read. Parsing needs about as much
/// memory again; the largest real session seen was 92 MiB.
pub const MAX_TRANSCRIPT_BYTES: u64 = 256 * 1024 * 1024;
""")
rep("""pub struct Reply {
    /// Header line, a blank line, then the library rendering.
    pub text: String,
    pub file_name: String,
    /// The header line; the document caption.
    pub caption: String,""", """pub struct Reply {
    /// Exactly the library rendering, nothing added.
    pub body: String,
    pub file_name: String,
    /// `brief · <short id> · последние N`, used only as the document caption.
    pub caption: String,""")
rep("""/// First line of every reply: `brief · <project> · <short id> · последние N`.
pub fn header(command: &TranscriptCommand, located: &Located) -> String {
    format!(
        "{} · {} · {} · последние {}",
        command.view.name(),
        located.project,
        short_id(&located.session_id),
        command.prompts
    )
}
""", """/// Document caption: `brief · <short id> · последние N`. No project name.
fn caption(command: &TranscriptCommand, short: &str) -> String {
    format!(
        "{} · {short} · последние {}",
        command.view.name(),
        command.prompts
    )
}

/// Reads at most `limit` bytes; `None` when the file is larger. The length is
/// checked before reading and again after, for a file that grows meanwhile.
fn read_limited(path: &Path, limit: u64) -> io::Result<Option<Vec<u8>>> {
    let file = std::fs::File::open(path)?;
    if file.metadata()?.len() > limit {
        return Ok(None);
    }
    let mut bytes = Vec::new();
    file.take(limit + 1).read_to_end(&mut bytes)?;
    Ok((bytes.len() as u64 <= limit).then_some(bytes))
}
""")
rep("""    command: &TranscriptCommand,
) -> Prepared {
    let located""", """    command: &TranscriptCommand,
) -> Prepared {
    prepare_limited(locator, thread_id, command, MAX_TRANSCRIPT_BYTES)
}

fn prepare_limited<L: TranscriptLocator + ?Sized>(
    locator: &L,
    thread_id: Option<i64>,
    command: &TranscriptCommand,
    limit: u64,
) -> Prepared {
    let located""")
rep("""    let bytes = match std::fs::read(&located.path) {
        Ok(bytes) => bytes,
""", """    let bytes = match read_limited(&located.path, limit) {
        Ok(Some(bytes)) => bytes,
        Ok(None) => {
            warn!(session = %short, "transcript too large to read");
            return Prepared::Notice(format!(
                "Транскрипт сессии {short} больше {} МБ, такие пока не показываются.",
                limit / (1024 * 1024)
            ));
        }
""")
rep("""    let caption = header(command, &located);
    Prepared::Transcript(Reply {
        text: format!("{caption}\\n\\n{body}"),
        file_name: format!("{}-{short}.txt", command.view.name()),
        caption,
        short_id: short,
    })""", """    Prepared::Transcript(Reply {
        body,
        file_name: format!("{}-{short}.txt", command.view.name()),
        caption: caption(command, &short),
        short_id: short,
    })""")
rep("""    let split = split_for_telegram(&reply.text, SplitOptions::default());
    if split.prefer_file {
        return send_document(outbox, thread_id, reply, reply.text.clone()).await;""",
    """    let split = split_for_telegram(&reply.body, SplitOptions::default());
    if split.prefer_file {
        return send_document(outbox, thread_id, reply, reply.body.clone()).await;""")

# ---- tests ----
rep("""    fn expected(jsonl: &str, view: View, prompts: usize) -> String {
        let turns = parse_jsonl(jsonl);
        let slice = last_prompts(&turns, prompts);
        let body = match view {
            View::Brief => render_brief(slice),
            View::Full => render_full(slice),
        };
        let command = TranscriptCommand {
            view,
            prompts,
            session_prefix: None,
        };
        let located = Located {
            session_id: SESSION.to_owned(),
            project: PROJECT.to_owned(),
            path: Default::default(),
        };
        format!("{}\\n\\n{body}", header(&command, &located))
    }""", """    /// The library output for the command, with nothing added.
    fn expected(jsonl: &str, view: View, prompts: usize) -> String {
        let turns = parse_jsonl(jsonl);
        let slice = last_prompts(&turns, prompts);
        match view {
            View::Brief => render_brief(slice),
            View::Full => render_full(slice),
        }
    }""")
rep("""                assert_eq!(document.caption.as_deref(), want.lines().next());""",
    """                assert_eq!(
                    document.caption.as_deref(),
                    Some("full · 5e551017 · последние 8")
                );""")
rep("""            Op::SendDocument { document, .. } => {
                assert_eq!(document.bytes, chunks[1..].concat().as_bytes());
            }""", """            Op::SendDocument { document, .. } => {
                assert_eq!(document.bytes, chunks[1..].concat().as_bytes());
                // What the user got: the accepted chunk plus the document is
                // the library output, nothing lost or repeated.
                let document = String::from_utf8(document.bytes.clone()).unwrap();
                assert_eq!(format!("{}{document}", chunks[0]), want);
            }""")
rep("""        assert!(sent[1].starts_with("brief · C--proj-demo · 5e551017 · последние 3"));""",
    """        assert_eq!(sent[1], expected(jsonl, View::Brief, DEFAULT_BRIEF_PROMPTS));""")
rep("""    #[test]
    fn ambiguous_prefix_lists_candidates() {""", """    #[test]
    fn oversized_transcripts_become_a_notice() {
        let dir = projects("");
        let session = dir.path().join(PROJECT).join(format!("{SESSION}.jsonl"));
        let root = ProjectsDir::new(dir.path().to_owned());
        let command = TranscriptCommand {
            view: View::Full,
            prompts: 1,
            session_prefix: None,
        };
        let with_len = |len: u64| {
            std::fs::File::options()
                .write(true)
                .open(&session)
                .unwrap()
                .set_len(len)
                .unwrap();
            prepare_limited(&root, None, &command, 16)
        };
        assert_eq!(
            with_len(17),
            Prepared::Notice(
                "Транскрипт сессии 5e551017 больше 0 МБ, такие пока не показываются.".to_owned()
            )
        );
        // At the limit the file is read; zero bytes render nothing.
        assert_eq!(
            with_len(16),
            Prepared::Notice("В сессии 5e551017 пока нечего показывать.".to_owned())
        );
        assert_eq!(read_limited(&session, 15).unwrap(), None);
        assert_eq!(read_limited(&session, 16).unwrap(), Some(vec![0; 16]));
    }

    #[test]
    fn ambiguous_prefix_lists_candidates() {""")
io.open(p, 'w', encoding='utf-8', newline='').write(s)
print('ok')
