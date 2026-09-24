//! `/brief [n] [session-id-prefix]` and `/full [n] [session-id-prefix]`.
//!
//! One worker answers commands in arrival order, so the chunks of two replies
//! never interleave. File reading and rendering run on the blocking pool.
//! Logs carry the view, the prompt count and the short session id; never a
//! path or a project directory name.

use std::io::{self, Read};
use std::path::Path;
use std::sync::Arc;
use std::time::{Duration, SystemTime};

use tokio::sync::mpsc;
use tracing::{debug, info, warn};
use transcript::{SplitOptions, split_for_telegram};

use super::api::{ApiError, Document};
use super::scheduler::{Op, Outbox, Outcome};
use super::sessions::{LocateError, TranscriptLocator};
use super::updates::Inbound;

pub const DEFAULT_BRIEF_PROMPTS: usize = 3;
pub const DEFAULT_FULL_PROMPTS: usize = 1;
pub const MAX_PROMPTS: usize = 100;
/// Candidates listed for an ambiguous prefix.
const MAX_CANDIDATES: usize = 10;
const SHORT_ID_LEN: usize = 8;
const CANDIDATE_TITLE_BYTES: u64 = 64 * 1024;
const DELIVERY_FAILURE_NOTICE: &str = "Не удалось отправить транскрипт.";
/// Largest transcript `/brief` and `/full` read. Parsing needs about as much
/// memory again; the largest real session seen was 92 MiB.
pub const MAX_TRANSCRIPT_BYTES: u64 = 256 * 1024 * 1024;

pub const USAGE: &str = "Использование: /brief [n] [начало id сессии] или /full [n] [начало id сессии]. \
n: сколько последних промптов показать, от 1 до 100 (по умолчанию brief 3, full 1). \
Если начало id состоит только из цифр, укажите n перед ним, например /brief 3 2026. \
Без id берётся самая свежая сессия.";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum View {
    Brief,
    Full,
}

impl View {
    fn name(self) -> &'static str {
        match self {
            View::Brief => "brief",
            View::Full => "full",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TranscriptCommand {
    pub view: View,
    /// How many of the last prompts to show, `1..=MAX_PROMPTS`.
    pub prompts: usize,
    /// Lowercase hex digits and dashes.
    pub session_prefix: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Parsed {
    Command(TranscriptCommand),
    /// `/brief` or `/full` with arguments that do not parse.
    Usage,
    /// Not one of these commands, or addressed to another bot.
    NotOurs,
}

/// True for texts the command worker should see: only `/brief` and `/full`
/// (optionally `@bot`). Any other text goes to the slot actor: `/compact`
/// or `!ls` as a console command ([`super::console`]), a path like `/tmp/x`
/// as a message for the session.
pub fn is_command(input: &Inbound) -> bool {
    input.text.as_deref().is_some_and(|text| {
        text.split_whitespace()
            .next()
            .and_then(|word| word.strip_prefix('/'))
            .map(|head| head.split_once('@').map_or(head, |(name, _)| name))
            .is_some_and(|name| name == "brief" || name == "full")
    })
}

/// Parses `/brief`, `/full`, optionally `@<bot_username>`, then `[n] [prefix]`.
/// A single numeric argument is `n`; a digits-only prefix needs an explicit `n`.
pub fn parse(text: &str, bot_username: Option<&str>) -> Parsed {
    let mut words = text.split_whitespace();
    let Some(head) = words.next().and_then(|word| word.strip_prefix('/')) else {
        return Parsed::NotOurs;
    };
    let (name, target) = match head.split_once('@') {
        Some((name, target)) => (name, Some(target)),
        None => (head, None),
    };
    if let (Some(target), Some(bot)) = (target, bot_username)
        && !target.eq_ignore_ascii_case(bot)
    {
        return Parsed::NotOurs;
    }
    let (view, default_prompts) = if name.eq_ignore_ascii_case("brief") {
        (View::Brief, DEFAULT_BRIEF_PROMPTS)
    } else if name.eq_ignore_ascii_case("full") {
        (View::Full, DEFAULT_FULL_PROMPTS)
    } else {
        return Parsed::NotOurs;
    };

    let args: Vec<&str> = words.collect();
    let count = |word: &str| {
        word.parse::<usize>()
            .ok()
            .filter(|n| (1..=MAX_PROMPTS).contains(n))
    };
    let prefix = |word: &str| {
        let word = word.to_ascii_lowercase();
        (word.chars().all(|c| c.is_ascii_hexdigit() || c == '-')).then_some(word)
    };
    let parsed = match args.as_slice() {
        [] => Some((default_prompts, None)),
        [one] if one.bytes().all(|b| b.is_ascii_digit()) => count(one).map(|n| (n, None)),
        [one] => prefix(one).map(|p| (default_prompts, Some(p))),
        [n, p] => count(n).zip(prefix(p)).map(|(n, p)| (n, Some(p))),
        _ => None,
    };
    match parsed {
        Some((prompts, session_prefix)) => Parsed::Command(TranscriptCommand {
            view,
            prompts,
            session_prefix,
        }),
        None => Parsed::Usage,
    }
}

/// A rendered transcript ready for delivery.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Reply {
    /// Exactly the library rendering, nothing added.
    pub body: String,
    pub file_name: String,
    /// `brief · <short id> · последние N`, used only as the document caption.
    pub caption: String,
    /// First `SHORT_ID_LEN` chars of the session id, for logs.
    pub short_id: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Prepared {
    Transcript(Reply),
    /// A single message for the user: not found, ambiguous, nothing to show.
    Notice(String),
}

fn short_id(session_id: &str) -> &str {
    session_id.get(..SHORT_ID_LEN).unwrap_or(session_id)
}

/// Document caption: `brief · <short id> · последние N`. No project name.
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

fn relative_age(path: &Path, now: SystemTime) -> String {
    let Some(age) = std::fs::metadata(path)
        .and_then(|metadata| metadata.modified())
        .ok()
        .and_then(|modified| now.duration_since(modified).ok())
    else {
        return "возраст неизвестен".to_owned();
    };
    if age < Duration::from_secs(60) {
        "только что".to_owned()
    } else if age < Duration::from_secs(60 * 60) {
        format!("{} мин назад", age.as_secs() / 60)
    } else if age < Duration::from_secs(24 * 60 * 60) {
        format!("{} ч назад", age.as_secs() / (60 * 60))
    } else {
        format!("{} дн назад", age.as_secs() / (24 * 60 * 60))
    }
}

fn candidate_title(path: &Path) -> Option<String> {
    let mut bytes = Vec::new();
    std::fs::File::open(path)
        .ok()?
        .take(CANDIDATE_TITLE_BYTES)
        .read_to_end(&mut bytes)
        .ok()?;
    transcript::ai_title(&String::from_utf8_lossy(&bytes))
        .map(|title| title.split_whitespace().collect::<Vec<_>>().join(" "))
}

fn locate_notice(error: &LocateError) -> String {
    match error {
        LocateError::RootMissing => {
            "Каталог проектов Claude Code не найден. Проверьте CCTG_PROJECTS_DIR.".to_owned()
        }
        LocateError::RootUnreadable(kind) => {
            format!("Каталог проектов Claude Code не читается ({kind:?}).")
        }
        LocateError::NoSessions => "Сессий Claude Code пока нет.".to_owned(),
        LocateError::NoMatch => "Нет сессии с таким началом id.".to_owned(),
        LocateError::NoTranscript => {
            "У сессии этой темы пока нет известного транскрипта.".to_owned()
        }
        LocateError::Ambiguous(candidates) => {
            let mut text = format!(
                "Под это начало id подходят {} сессий, уточните:",
                candidates.len()
            );
            let now = SystemTime::now();
            for candidate in candidates.iter().take(MAX_CANDIDATES) {
                text.push_str(&format!(
                    "\n{} · {}",
                    short_id(&candidate.session_id),
                    relative_age(&candidate.path, now)
                ));
                if let Some(title) = candidate_title(&candidate.path) {
                    text.push_str(&format!(" · {title}"));
                }
            }
            if candidates.len() > MAX_CANDIDATES {
                text.push_str(&format!("\n… и ещё {}", candidates.len() - MAX_CANDIDATES));
            }
            text
        }
    }
}

/// Locates, reads, parses and renders. Blocking: file IO and CPU-bound parsing.
pub fn prepare<L: TranscriptLocator + ?Sized>(
    locator: &L,
    thread_id: Option<i64>,
    command: &TranscriptCommand,
) -> Prepared {
    prepare_limited(locator, thread_id, command, MAX_TRANSCRIPT_BYTES)
}

fn prepare_limited<L: TranscriptLocator + ?Sized>(
    locator: &L,
    thread_id: Option<i64>,
    command: &TranscriptCommand,
    limit: u64,
) -> Prepared {
    let located = match locator.locate(thread_id, command.session_prefix.as_deref()) {
        Ok(located) => located,
        Err(error) => return Prepared::Notice(locate_notice(&error)),
    };
    let short = short_id(&located.session_id).to_owned();
    let bytes = match read_limited(&located.path, limit) {
        Ok(Some(bytes)) => bytes,
        Ok(None) => {
            warn!(session = %short, "transcript too large to read");
            return Prepared::Notice(format!(
                "Транскрипт сессии {short} больше {} МБ, такие пока не показываются.",
                limit / (1024 * 1024)
            ));
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            return Prepared::Notice(format!(
                "Транскрипт сессии {short} не найден: файла нет или он ещё не записан."
            ));
        }
        Err(error) => {
            warn!(session = %short, kind = ?error.kind(), "transcript cannot be read");
            return Prepared::Notice(format!(
                "Транскрипт сессии {short} не читается ({:?}).",
                error.kind()
            ));
        }
    };
    // A half-written last line or stray bytes must not fail the whole command.
    let jsonl = String::from_utf8_lossy(&bytes);
    let turns = transcript::parse(&jsonl);
    let slice = transcript::last_prompts(&turns, command.prompts);
    let body = match command.view {
        View::Brief => transcript::render_brief(slice),
        View::Full => transcript::render_full(slice),
    };
    if body.trim().is_empty() {
        return Prepared::Notice(format!("В сессии {short} пока нечего показывать."));
    }
    Prepared::Transcript(Reply {
        body,
        file_name: format!("{}-{short}.txt", command.view.name()),
        caption: caption(command, &short),
        short_id: short,
    })
}

#[derive(Debug, thiserror::Error)]
pub enum DeliveryError {
    #[error(transparent)]
    Api(#[from] ApiError),
    #[error("the outbound scheduler has stopped")]
    Stopped,
}

async fn submit(outbox: &Outbox, op: Op) -> Result<Outcome, DeliveryError> {
    outbox
        .submit(op)
        .await
        .await
        .map_err(|_| DeliveryError::Stopped)?
        .map_err(DeliveryError::from)
}

pub async fn send_text(
    outbox: &Outbox,
    thread_id: Option<i64>,
    text: String,
) -> Result<(), DeliveryError> {
    let op = Op::Send {
        thread_id,
        text,
        html: None,
        reply_markup: None,
        permission: false,
        reply_to: None,
        notify: false,
    };
    submit(outbox, op).await.map(drop)
}

async fn send_document(
    outbox: &Outbox,
    thread_id: Option<i64>,
    reply: &Reply,
    text: String,
) -> Result<(), DeliveryError> {
    let document = Document {
        file_name: reply.file_name.clone(),
        bytes: text.into_bytes(),
        caption: Some(reply.caption.clone()),
    };
    submit(
        outbox,
        Op::SendDocument {
            thread_id,
            document,
            notify: false,
        },
    )
    .await
    .map(drop)
}

/// Telegram's answer to a text over its limit: `400 Bad Request: message is too long`.
fn is_too_long(error: &DeliveryError) -> bool {
    matches!(
        error,
        DeliveryError::Api(ApiError::Telegram { code: 400, description })
            if description.to_ascii_lowercase().contains("too long")
    )
}

async fn send_delivery_failure_notice(
    outbox: &Outbox,
    thread_id: Option<i64>,
    error: &DeliveryError,
) {
    if is_too_long(error) {
        return;
    }
    if let Err(notice_error) =
        send_text(outbox, thread_id, DELIVERY_FAILURE_NOTICE.to_owned()).await
    {
        warn!(%notice_error, "delivery failure notice failed");
    }
}

/// Sends `reply` as ordered messages, or as one document when the split asks
/// for it. When Telegram rejects a chunk as too long, the rest (that chunk and
/// everything after it) goes as one document; that switch happens once and the
/// document is never retried as text.
pub async fn deliver(
    outbox: &Outbox,
    thread_id: Option<i64>,
    reply: &Reply,
) -> Result<(), DeliveryError> {
    let split = split_for_telegram(&reply.body, SplitOptions::default());
    if split.prefer_file {
        return send_document(outbox, thread_id, reply, reply.body.clone()).await;
    }
    for (index, chunk) in split.chunks.iter().enumerate() {
        match send_text(outbox, thread_id, chunk.clone()).await {
            Ok(()) => {}
            Err(error) if is_too_long(&error) => {
                warn!(
                    chunk = index,
                    "telegram rejected a chunk as too long; sending the rest as a document"
                );
                let rest = split.chunks[index..].concat();
                return send_document(outbox, thread_id, reply, rest).await;
            }
            Err(error) => return Err(error),
        }
    }
    Ok(())
}

/// Answers one inbound message if it is a transcript command. Never panics and
/// never stops the caller: every failure becomes a notice or a log line.
pub async fn handle<L: TranscriptLocator>(
    input: &Inbound,
    outbox: &Outbox,
    locator: &Arc<L>,
    bot_username: Option<&str>,
) {
    let Some(text) = input.text.as_deref() else {
        return;
    };
    let thread_id = input.thread_id;
    let command = match parse(text, bot_username) {
        Parsed::NotOurs => {
            if text.trim_start().starts_with('/') {
                debug!("unknown slash command");
            }
            return;
        }
        Parsed::Usage => {
            if let Err(error) = send_text(outbox, thread_id, USAGE.to_owned()).await {
                warn!(%error, "usage reply failed");
                send_delivery_failure_notice(outbox, thread_id, &error).await;
            }
            return;
        }
        Parsed::Command(command) => command,
    };
    let view = command.view;
    let prompts = command.prompts;
    let locator = Arc::clone(locator);
    let prepared =
        tokio::task::spawn_blocking(move || prepare(locator.as_ref(), thread_id, &command))
            .await
            .unwrap_or_else(|_| {
                warn!(?view, "transcript preparation panicked");
                Prepared::Notice("Не удалось подготовить транскрипт.".to_owned())
            });
    let (result, session) = match &prepared {
        Prepared::Transcript(reply) => (
            deliver(outbox, thread_id, reply).await,
            Some(reply.short_id.as_str()),
        ),
        Prepared::Notice(notice) => (send_text(outbox, thread_id, notice.clone()).await, None),
    };
    match result {
        Ok(()) => info!(
            ?view,
            prompts,
            session = session.unwrap_or("-"),
            "transcript command answered"
        ),
        Err(error) => {
            warn!(?view, %error, "transcript command reply failed");
            send_delivery_failure_notice(outbox, thread_id, &error).await;
        }
    }
}

/// Handles commands one at a time until the sender side is dropped.
pub async fn serve<L: TranscriptLocator>(
    mut inbox: mpsc::UnboundedReceiver<Inbound>,
    outbox: Outbox,
    locator: Arc<L>,
    bot_username: Option<String>,
) {
    while let Some(input) = inbox.recv().await {
        handle(&input, &outbox, &locator, bot_username.as_deref()).await;
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn only_our_commands_are_commands() {
        let input = |text: &str| crate::hub::updates::Inbound {
            message_id: 1,
            thread_id: Some(2),
            text: Some(text.to_owned()),
            reply_to: None,
            quote: None,
            forwarded: false,
        };
        for text in ["/brief", "/full 2", " /brief@cctg_bot 3", "/full@other_bot"] {
            assert!(super::is_command(&input(text)), "{text}");
        }
        for text in [
            "/compact",
            "/tmp/app.log fails",
            "/briefly",
            "hello",
            "",
            "/",
        ] {
            assert!(!super::is_command(&input(text)), "{text}");
        }
    }

    use std::collections::VecDeque;
    use std::path::Path;
    use std::sync::Mutex;

    use transcript::{last_prompts, parse as parse_jsonl, render_brief, render_full};

    use super::*;
    use crate::hub::api::Message;
    use crate::hub::scheduler::{BucketConfig, Delivery, Scheduler, Transport};
    use crate::hub::sessions::{Located, ProjectsDir};
    use crate::hub::testdir::TempDir;

    const SESSION: &str = "5e551017-0000-4000-8000-000000000001";
    const PROJECT: &str = "C--proj-demo";
    const THREAD: Option<i64> = Some(7);

    macro_rules! fixture {
        ($name:literal) => {
            include_str!(concat!(
                env!("CARGO_MANIFEST_DIR"),
                "/../transcript/tests/fixtures/",
                $name
            ))
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

    /// Records every op; answers from a script, then `Sent`/`Done`.
    #[derive(Default)]
    struct Fake {
        ops: Mutex<Vec<Op>>,
        script: Mutex<VecDeque<Option<ApiError>>>,
        /// Answer every op with this error once the script is empty.
        always: Option<(i64, &'static str)>,
    }

    impl Fake {
        fn scripted(script: Vec<Option<ApiError>>) -> Arc<Self> {
            Arc::new(Self {
                script: Mutex::new(script.into()),
                ..Self::default()
            })
        }

        fn ops(&self) -> Vec<Op> {
            self.ops.lock().unwrap().clone()
        }

        fn texts(&self) -> Vec<String> {
            self.ops()
                .into_iter()
                .filter_map(|op| match op {
                    Op::Send { text, .. } => Some(text),
                    _ => None,
                })
                .collect()
        }
    }

    fn too_long() -> ApiError {
        ApiError::Telegram {
            code: 400,
            description: "Bad Request: message is too long".to_owned(),
        }
    }

    impl Transport for Fake {
        async fn execute(&self, op: &Op) -> Delivery {
            self.ops.lock().unwrap().push(op.clone());
            if let Some(Some(error)) = self.script.lock().unwrap().pop_front() {
                return Err(error);
            }
            if let Some((code, description)) = self.always {
                return Err(ApiError::Telegram {
                    code,
                    description: description.to_owned(),
                });
            }
            Ok(Outcome::Sent(Message::default()))
        }
    }

    fn projects(jsonl: &str) -> TempDir {
        let dir = TempDir::new("commands");
        let project = dir.path().join(PROJECT);
        std::fs::create_dir_all(&project).unwrap();
        std::fs::write(project.join(format!("{SESSION}.jsonl")), jsonl).unwrap();
        dir
    }

    /// Runs `texts` through `serve` against `root` and a fresh scheduler.
    async fn run(fake: &Arc<Fake>, root: &Path, texts: &[&str]) {
        let (scheduler, outbox) = Scheduler::new(fake.clone(), BucketConfig::default());
        let scheduler = tokio::spawn(scheduler.run());
        let (tx, rx) = mpsc::unbounded_channel();
        for text in texts {
            tx.send(Inbound {
                message_id: 1,
                thread_id: THREAD,
                text: Some((*text).to_owned()),
                reply_to: None,
                quote: None,
                forwarded: false,
            })
            .unwrap();
        }
        drop(tx);
        let locator = Arc::new(ProjectsDir::new(root.to_owned()));
        serve(rx, outbox, locator, Some("cctg_bot".to_owned())).await;
        scheduler.await.unwrap();
    }

    /// The library output for the command, with nothing added.
    fn expected(jsonl: &str, view: View, prompts: usize) -> String {
        let turns = parse_jsonl(jsonl);
        let slice = last_prompts(&turns, prompts);
        match view {
            View::Brief => render_brief(slice),
            View::Full => render_full(slice),
        }
    }

    #[tokio::test(start_paused = true)]
    async fn replies_match_the_library_on_fixtures() {
        for jsonl in FIXTURES {
            let dir = projects(jsonl);
            let fake = Arc::new(Fake::default());
            run(
                &fake,
                dir.path(),
                &["/brief", "/full 2", "/brief@cctg_bot 100 5e55"],
            )
            .await;
            let want = [
                expected(jsonl, View::Brief, DEFAULT_BRIEF_PROMPTS),
                expected(jsonl, View::Full, 2),
                expected(jsonl, View::Brief, 100),
            ];
            let mut sent = Vec::new();
            for op in fake.ops() {
                match op {
                    Op::Send {
                        thread_id, text, ..
                    } => {
                        assert_eq!(thread_id, THREAD);
                        sent.push(text);
                    }
                    other => panic!("fixtures fit in messages, got {other:?}"),
                }
            }
            let want_chunks: Vec<String> = want
                .iter()
                .flat_map(|text| split_for_telegram(text, SplitOptions::default()).chunks)
                .collect();
            assert_eq!(sent, want_chunks);
        }
    }

    /// `exchanges` prompts, each answered with `answer_len` chars that name the exchange.
    fn long_session(exchanges: usize, answer_len: usize) -> String {
        let mut lines = Vec::new();
        for i in 0..exchanges {
            lines.push(serde_json::json!({
                "type": "user", "message": { "role": "user", "content": format!("prompt {i}") }
            }));
            let answer = format!("answer-{i} ").repeat(answer_len / 10);
            lines.push(serde_json::json!({
                "type": "assistant",
                "message": { "role": "assistant", "stop_reason": "end_turn",
                    "content": [{ "type": "text", "text": answer }] }
            }));
        }
        lines
            .iter()
            .map(|line| format!("{line}\n"))
            .collect::<String>()
    }

    #[tokio::test(start_paused = true)]
    async fn multi_chunk_reply_keeps_order() {
        let jsonl = long_session(3, 3000);
        let dir = projects(&jsonl);
        let fake = Arc::new(Fake::default());
        run(&fake, dir.path(), &["/brief 3"]).await;
        let sent = fake.texts();
        let want = expected(&jsonl, View::Brief, 3);
        assert!(sent.len() > 1 && sent.len() <= 4, "{} chunks", sent.len());
        assert_eq!(
            sent,
            split_for_telegram(&want, SplitOptions::default()).chunks
        );
        assert_eq!(sent.concat(), want);
    }

    #[tokio::test(start_paused = true)]
    async fn large_reply_goes_as_one_document() {
        let jsonl = long_session(8, 4000);
        let dir = projects(&jsonl);
        let fake = Arc::new(Fake::default());
        run(&fake, dir.path(), &["/full 8"]).await;
        let want = expected(&jsonl, View::Full, 8);
        let ops = fake.ops();
        assert_eq!(ops.len(), 1);
        match &ops[0] {
            Op::SendDocument {
                thread_id,
                document,
                notify: false,
            } => {
                assert_eq!(*thread_id, THREAD);
                assert_eq!(document.bytes, want.as_bytes());
                assert_eq!(document.file_name, "full-5e551017.txt");
                assert_eq!(
                    document.caption.as_deref(),
                    Some("full · 5e551017 · последние 8")
                );
            }
            other => panic!("expected a document, got {other:?}"),
        }
    }

    #[tokio::test(start_paused = true)]
    async fn too_long_switches_to_a_document_once() {
        let jsonl = long_session(3, 3000);
        let dir = projects(&jsonl);
        let want = expected(&jsonl, View::Brief, 3);
        let chunks = split_for_telegram(&want, SplitOptions::default()).chunks;
        assert!(chunks.len() >= 2);

        // The second chunk is rejected: the rest goes as a document.
        let fake = Fake::scripted(vec![None, Some(too_long())]);
        run(&fake, dir.path(), &["/brief 3"]).await;
        let ops = fake.ops();
        assert_eq!(ops.len(), 3, "{ops:?}");
        assert!(matches!(&ops[0], Op::Send { text, .. } if *text == chunks[0]));
        assert!(matches!(&ops[1], Op::Send { text, .. } if *text == chunks[1]));
        match &ops[2] {
            Op::SendDocument { document, .. } => {
                assert_eq!(document.bytes, chunks[1..].concat().as_bytes());
                // What the user got: the accepted chunk plus the document is
                // the library output, nothing lost or repeated.
                let document = String::from_utf8(document.bytes.clone()).unwrap();
                assert_eq!(format!("{}{document}", chunks[0]), want);
            }
            other => panic!("expected a document, got {other:?}"),
        }

        // Everything is rejected: one text attempt, one document, nothing more.
        let fake = Arc::new(Fake {
            always: Some((400, "Bad Request: message is too long")),
            ..Fake::default()
        });
        run(&fake, dir.path(), &["/brief 3"]).await;
        let ops = fake.ops();
        assert_eq!(ops.len(), 2, "{ops:?}");
        assert!(matches!(ops[0], Op::Send { .. }));
        assert!(matches!(ops[1], Op::SendDocument { .. }));
    }

    #[tokio::test(start_paused = true)]
    async fn other_errors_do_not_switch_to_a_document() {
        let jsonl = long_session(3, 3000);
        let dir = projects(&jsonl);
        let fake = Fake::scripted(vec![Some(ApiError::Telegram {
            code: 400,
            description: "Bad Request: message thread not found".to_owned(),
        })]);
        run(&fake, dir.path(), &["/brief 3"]).await;
        let ops = fake.ops();
        assert_eq!(ops.len(), 2, "{ops:?}");
        assert!(matches!(ops[0], Op::Send { .. }));
        assert!(matches!(
            &ops[1],
            Op::Send { thread_id, text, .. }
                if *thread_id == THREAD && text == DELIVERY_FAILURE_NOTICE
        ));
    }

    #[tokio::test(start_paused = true)]
    async fn worker_handles_the_next_command_after_failed_delivery() {
        let jsonl = fixture!("final_answer.jsonl");
        let dir = projects(jsonl);
        let fake = Fake::scripted(vec![
            Some(ApiError::Telegram {
                code: 400,
                description: "Bad Request: message thread not found".to_owned(),
            }),
            None,
            None,
        ]);
        run(&fake, dir.path(), &["/brief", "/full 1"]).await;
        let texts = fake.texts();
        assert_eq!(texts.len(), 3, "{texts:?}");
        assert_eq!(texts[1], DELIVERY_FAILURE_NOTICE);
        assert_eq!(texts[2], expected(jsonl, View::Full, 1));
    }

    #[tokio::test(start_paused = true)]
    async fn rejected_fallback_document_is_not_retried() {
        let jsonl = long_session(3, 3000);
        let dir = projects(&jsonl);
        let fake = Fake::scripted(vec![
            Some(too_long()),
            Some(ApiError::Telegram {
                code: 400,
                description: "Bad Request: document rejected".to_owned(),
            }),
            None,
        ]);
        run(&fake, dir.path(), &["/brief 3"]).await;
        let ops = fake.ops();
        assert_eq!(ops.len(), 3, "{ops:?}");
        assert!(matches!(ops[0], Op::Send { .. }));
        assert!(matches!(ops[1], Op::SendDocument { .. }));
        assert!(matches!(
            &ops[2],
            Op::Send { thread_id, text, .. }
                if *thread_id == THREAD && text == DELIVERY_FAILURE_NOTICE
        ));
    }

    #[tokio::test(start_paused = true)]
    async fn bad_paths_get_a_notice_and_the_worker_keeps_going() {
        let jsonl = fixture!("final_answer.jsonl");
        let dir = projects(jsonl);
        let session = dir.path().join(PROJECT).join(format!("{SESSION}.jsonl"));
        // A directory named like a session is not a session.
        let fake_dir = dir
            .path()
            .join(PROJECT)
            .join("5e551017-0000-4000-8000-000000000002.jsonl");
        std::fs::create_dir_all(&fake_dir).unwrap();

        let fake = Arc::new(Fake::default());
        run(&fake, dir.path(), &["/brief 1 ffff", "/brief", "/brief x!"]).await;
        let sent = fake.texts();
        assert_eq!(sent[0], "Нет сессии с таким началом id.");
        assert_eq!(sent[1], expected(jsonl, View::Brief, DEFAULT_BRIEF_PROMPTS));
        assert_eq!(sent[2], USAGE);

        std::fs::remove_file(&session).unwrap();
        let fake = Arc::new(Fake::default());
        run(&fake, dir.path(), &["/brief", "/full"]).await;
        assert_eq!(
            fake.texts(),
            [
                "Сессий Claude Code пока нет.",
                "Сессий Claude Code пока нет."
            ]
        );

        // Only records the renderers skip: nothing to show.
        std::fs::write(
            &session,
            "{\"type\":\"ai-title\",\"aiTitle\":\"t\"}
not json
",
        )
        .unwrap();
        let fake = Arc::new(Fake::default());
        run(&fake, dir.path(), &["/full"]).await;
        assert_eq!(fake.texts(), ["В сессии 5e551017 пока нечего показывать."]);
        std::fs::remove_file(&session).unwrap();

        let missing = dir.path().join("absent");
        let fake = Arc::new(Fake::default());
        run(&fake, &missing, &["/brief"]).await;
        assert!(fake.texts()[0].starts_with("Каталог проектов Claude Code не найден"));
    }

    /// A locator that points at a path it does not check.
    struct Fixed(Located);

    impl TranscriptLocator for Fixed {
        fn locate(&self, _: Option<i64>, _: Option<&str>) -> Result<Located, LocateError> {
            Ok(self.0.clone())
        }
    }

    #[test]
    fn missing_and_unreadable_files_become_notices() {
        let dir = TempDir::new("commands-unreadable");
        let command = TranscriptCommand {
            view: View::Brief,
            prompts: 1,
            session_prefix: None,
        };
        let at = |path: std::path::PathBuf| {
            Fixed(Located {
                session_id: SESSION.to_owned(),
                project: PROJECT.to_owned(),
                path,
            })
        };
        let gone = prepare(&at(dir.path().join("gone.jsonl")), None, &command);
        assert_eq!(
            gone,
            Prepared::Notice(
                "Транскрипт сессии 5e551017 не найден: файла нет или он ещё не записан.".to_owned()
            )
        );
        let unreadable = prepare(&at(dir.path().to_owned()), None, &command);
        assert!(
            matches!(&unreadable, Prepared::Notice(text) if text.starts_with("Транскрипт сессии 5e551017 не читается")),
            "{unreadable:?}"
        );
        for prepared in [gone, unreadable] {
            if let Prepared::Notice(text) = prepared {
                assert!(!text.contains(&dir.path().display().to_string()));
            }
        }
    }

    #[test]
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
    fn ambiguous_prefix_lists_candidates() {
        let dir = TempDir::new("ambiguous-candidates");
        let project = "C--Users-private-name-dev";
        let candidates: Vec<Located> = (0..12)
            .map(|i| {
                let path = dir.path().join(format!("candidate-{i}.jsonl"));
                std::fs::write(
                    &path,
                    format!("{{\"type\":\"ai-title\",\"aiTitle\":\"Session {i}\"}}\n"),
                )
                .unwrap();
                std::fs::File::options()
                    .write(true)
                    .open(&path)
                    .unwrap()
                    .set_modified(
                        std::time::SystemTime::now() - std::time::Duration::from_secs(2 * 60 * 60),
                    )
                    .unwrap();
                Located {
                    session_id: format!("aaaa{i:04}-0000-4000-8000-000000000000"),
                    project: project.to_owned(),
                    path,
                }
            })
            .collect();
        let text = locate_notice(&LocateError::Ambiguous(candidates));
        assert!(text.starts_with("Под это начало id подходят 12 сессий"));
        assert!(
            text.contains("\naaaa0000 · 2 ч назад · Session 0"),
            "{text}"
        );
        assert!(
            text.contains("\naaaa0009 · 2 ч назад · Session 9"),
            "{text}"
        );
        assert!(!text.contains("\naaaa0010"));
        assert!(!text.contains(project), "project directory leaked: {text}");
        assert!(
            !text.contains("aaaa0000-"),
            "full session id leaked: {text}"
        );
        assert!(text.ends_with("… и ещё 2"));
    }

    #[test]
    fn unreadable_projects_root_has_a_notice() {
        assert_eq!(
            locate_notice(&LocateError::RootUnreadable(
                std::io::ErrorKind::PermissionDenied
            )),
            "Каталог проектов Claude Code не читается (PermissionDenied)."
        );
    }

    #[test]
    fn parses_arguments() {
        let command = |view, prompts, prefix: Option<&str>| {
            Parsed::Command(TranscriptCommand {
                view,
                prompts,
                session_prefix: prefix.map(str::to_owned),
            })
        };
        let bot = Some("cctg_bot");
        assert_eq!(parse("/brief", bot), command(View::Brief, 3, None));
        assert_eq!(parse("/full", bot), command(View::Full, 1, None));
        assert_eq!(parse(" /brief  5 ", bot), command(View::Brief, 5, None));
        assert_eq!(
            parse("/brief 1F2C", bot),
            command(View::Brief, 3, Some("1f2c"))
        );
        assert_eq!(
            parse("/full 2 1f2c-01", bot),
            command(View::Full, 2, Some("1f2c-01"))
        );
        assert_eq!(
            parse("/brief 1 0133", bot),
            command(View::Brief, 1, Some("0133"))
        );
        assert_eq!(
            parse("/brief@CCTG_bot 4", bot),
            command(View::Brief, 4, None)
        );
        assert_eq!(parse("/brief@other_bot", bot), Parsed::NotOurs);
        assert_eq!(
            parse("/brief@other_bot", None),
            command(View::Brief, 3, None)
        );
        for bad in [
            "/brief 0",
            "/brief 101",
            "/brief x!",
            "/brief 1 2 3",
            "/full ../x",
            "/brief 2 zz",
        ] {
            assert_eq!(parse(bad, bot), Parsed::Usage, "{bad}");
        }
        assert!(USAGE.contains("/brief 3 2026"));
        for other in ["/sessions", "brief", "hello", "", "/", "/briefly"] {
            assert_eq!(parse(other, bot), Parsed::NotOurs, "{other}");
        }
    }
}
