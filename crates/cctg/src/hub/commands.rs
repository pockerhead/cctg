//! `/brief [n] [session-id-prefix]` and `/full [n] [session-id-prefix]`.
//!
//! One worker answers commands in arrival order, so the chunks of two replies
//! never interleave. The hub reads no transcript (TASK-034): the worker asks
//! the slot actor ([`TranscriptAsk`]), which picks the session and has its
//! agent render the text on the session's machine. A session without such an
//! agent (ended, headless, nested, an agent too old) gets a notice that says
//! why. Logs carry the view, the prompt count and the short session id;
//! never a path or a project directory name.

use std::future::Future;
use std::sync::Arc;
use std::time::Duration;

use tokio::sync::{mpsc, oneshot};
use tracing::{debug, info, warn};
use transcript::{SplitOptions, split_for_telegram};

use super::api::{ApiError, Document};
use super::registry::{Registry, SessionKind};
use super::scheduler::{Op, Outbox, Outcome};
use super::updates::Inbound;
use crate::wire::TranscriptView;

pub const DEFAULT_BRIEF_PROMPTS: usize = 3;
pub const DEFAULT_FULL_PROMPTS: usize = 1;
pub const MAX_PROMPTS: usize = 100;
/// Candidates listed for an ambiguous prefix.
const MAX_CANDIDATES: usize = 10;
const SHORT_ID_LEN: usize = 8;
const DELIVERY_FAILURE_NOTICE: &str = "Не удалось отправить транскрипт.";
/// The worker's longest wait for the slot actor. The actor itself answers
/// every ask (the agent's answer, its timeout or a lost link), so this only
/// guards against a stopped actor.
pub const ANSWER_WAIT: Duration = Duration::from_secs(120);
const NO_ACTOR: &str = "Транскрипт сейчас недоступен: hub останавливается.";

pub const USAGE: &str = "Использование: /brief [n] [начало id сессии] или /full [n] [начало id сессии]. \
n: сколько последних промптов показать, от 1 до 100 (по умолчанию brief 3, full 1). \
Если начало id состоит только из цифр, укажите n перед ним, например /brief 3 2026. \
Без id в теме берётся её текущая сессия, в General самая свежая запущенная. \
Транскрипт отдаёт агент запущенной сессии с её машины.";

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

    pub fn wire(self) -> TranscriptView {
        match self {
            View::Brief => TranscriptView::Brief,
            View::Full => TranscriptView::Full,
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

/// Where the worker gets a transcript: the slot actor in the hub
/// ([`Asks`]), a fake in tests.
pub trait TranscriptSource: Send + Sync + 'static {
    fn prepare(
        &self,
        thread_id: Option<i64>,
        command: TranscriptCommand,
    ) -> impl Future<Output = Prepared> + Send;
}

/// One command for the slot actor ([`super::slots::Slots::transcript_asks`]),
/// answered exactly once through `answer`.
#[derive(Debug)]
pub struct TranscriptAsk {
    pub thread_id: Option<i64>,
    pub command: TranscriptCommand,
    pub answer: oneshot::Sender<Prepared>,
}

/// [`TranscriptSource`] over the slot actor's channel.
#[derive(Debug, Clone)]
pub struct Asks(pub mpsc::Sender<TranscriptAsk>);

impl TranscriptSource for Asks {
    async fn prepare(&self, thread_id: Option<i64>, command: TranscriptCommand) -> Prepared {
        let (answer, answered) = oneshot::channel();
        let ask = TranscriptAsk {
            thread_id,
            command,
            answer,
        };
        if self.0.send(ask).await.is_err() {
            return Prepared::Notice(NO_ACTOR.to_owned());
        }
        match tokio::time::timeout(ANSWER_WAIT, answered).await {
            Ok(Ok(prepared)) => prepared,
            _ => Prepared::Notice(NO_ACTOR.to_owned()),
        }
    }
}

/// The session a command is about: the one whose id starts with the prefix,
/// else the current session of the slot topic it was sent in, else (General,
/// a topic that is no slot) the newest running top-level session. `Err`:
/// the notice for the user. Only sessions the hub knows are found.
pub fn resolve(
    registry: &Registry,
    thread_id: Option<i64>,
    prefix: Option<&str>,
) -> Result<String, String> {
    if let Some(prefix) = prefix {
        let mut found: Vec<_> = registry
            .sessions
            .iter()
            .filter(|(id, _)| id.starts_with(prefix))
            .collect();
        return match found.len() {
            0 => Err("Нет известной hub сессии с таким началом id.".to_owned()),
            1 => Ok(found[0].0.clone()),
            count => {
                found.sort_by_key(|(_, entry)| std::cmp::Reverse(entry.seen));
                let mut text = format!("Под это начало id подходят {count} сессий, уточните:");
                for (id, entry) in found.iter().take(MAX_CANDIDATES) {
                    let state = if entry.ended {
                        "завершена"
                    } else {
                        "идёт"
                    };
                    text.push_str(&format!("\n{} · {state}", short_id(id)));
                    if let Some(title) = &entry.title {
                        text.push_str(&format!(" · {title}"));
                    }
                }
                if count > MAX_CANDIDATES {
                    text.push_str(&format!("\n… и ещё {}", count - MAX_CANDIDATES));
                }
                Err(text)
            }
        };
    }
    if let Some(slot) = thread_id.and_then(|thread_id| registry.slot_by_topic(thread_id)) {
        return registry
            .slot(slot)
            .and_then(|slot| slot.current_session.clone())
            .ok_or_else(|| "В этой теме ещё не было сессии.".to_owned());
    }
    registry
        .sessions
        .iter()
        .filter(|(_, entry)| !entry.ended && entry.kind == SessionKind::TopLevel)
        .max_by_key(|(_, entry)| entry.seen)
        .map(|(id, _)| id.clone())
        .ok_or_else(|| "Запущенных сессий нет.".to_owned())
}

/// Why a session's transcript cannot be had.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Unavailable {
    /// The session ended: its agent is gone.
    Ended,
    /// A nested run: it never has an agent of its own.
    Nested,
    /// Running without an agent link (no channel, headless, reconnecting).
    NoAgent,
    /// Its agent was built before session reads.
    OldAgent,
    /// No hook named its transcript yet.
    NoTranscript,
    /// The agent found no transcript file.
    Missing,
    /// The agent serves nothing: it cannot find its own project folder.
    Refused,
    /// The file is there but could not be read.
    Unreadable,
    /// Over what the agent renders or the hub takes.
    TooLarge,
    /// The agent did not answer in time.
    NoAnswer,
    /// The agent's link closed (an update, a reconnect) before the answer.
    LinkLost,
    /// Any other answer.
    Failed,
}

/// The notice for a transcript of `session_id` that cannot be shown. Only
/// the short id, never a path.
pub fn unavailable(why: Unavailable, session_id: &str) -> Prepared {
    let short = short_id(session_id);
    Prepared::Notice(match why {
        Unavailable::Ended => format!(
            "Сессия {short} завершена. Её транскрипт лежит на машине сессии и отдаётся только её запущенным агентом; ответы сессии есть в теме. Чтобы посмотреть транскрипт, продолжите её: claude --resume {session_id}"
        ),
        Unavailable::Nested => format!(
            "Сессия {short} — вложенный запуск без своего агента, её транскрипт получить не у кого."
        ),
        Unavailable::NoAgent => format!(
            "У сессии {short} нет связи с агентом cctg (запущена без канала или headless), транскрипт получить не у кого."
        ),
        Unavailable::OldAgent => format!(
            "Агент сессии {short} старой версии и не отдаёт транскрипт. Обновите его кнопкой ⬆️ Обновить в теме."
        ),
        Unavailable::NoTranscript => {
            "У сессии этой темы пока нет известного транскрипта.".to_owned()
        }
        Unavailable::Missing => {
            format!("Транскрипт сессии {short} не найден: файла нет или он ещё не записан.")
        }
        Unavailable::Refused => format!(
            "Агент сессии {short} не знает, где Claude Code хранит её транскрипты (нет id сессии или папки CLAUDE_CONFIG_DIR / ~/.claude на машине сессии), и транскрипты не отдаёт."
        ),
        Unavailable::Unreadable => format!("Транскрипт сессии {short} не читается."),
        Unavailable::TooLarge => {
            format!("Транскрипт сессии {short} в этом виде слишком большой; уменьшите n.")
        }
        Unavailable::NoAnswer => {
            format!("Агент сессии {short} не ответил вовремя, попробуйте ещё раз.")
        }
        Unavailable::LinkLost => {
            format!("Связь с агентом сессии {short} прервалась, повторите команду.")
        }
        Unavailable::Failed => format!("Агент сессии {short} не смог прочитать транскрипт."),
    })
}

/// The reply for `body`, the agent's rendering of `command` for
/// `session_id`, delivered exactly as rendered.
pub fn transcript_reply(command: &TranscriptCommand, session_id: &str, body: String) -> Prepared {
    let short = short_id(session_id).to_owned();
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
pub async fn handle<S: TranscriptSource>(
    input: &Inbound,
    outbox: &Outbox,
    source: &Arc<S>,
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
    let prepared = source.prepare(thread_id, command).await;
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
pub async fn serve<S: TranscriptSource>(
    mut inbox: mpsc::UnboundedReceiver<Inbound>,
    outbox: Outbox,
    source: Arc<S>,
    bot_username: Option<String>,
) {
    while let Some(input) = inbox.recv().await {
        handle(&input, &outbox, &source, bot_username.as_deref()).await;
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
            media: None,
            from_name: None,
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
    use std::sync::Mutex;

    use transcript::{last_prompts, parse as parse_jsonl, render_brief, render_full};

    use super::*;
    use crate::hub::api::Message;
    use crate::hub::scheduler::{BucketConfig, Delivery, Scheduler, Transport};
    use crate::wire::{HookEvent, HookPost};

    const SESSION: &str = "5e551017-0000-4000-8000-000000000001";
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

    /// The library output for the command, with nothing added.
    fn expected(jsonl: &str, view: View, prompts: usize) -> String {
        let turns = parse_jsonl(jsonl);
        let slice = last_prompts(&turns, prompts);
        match view {
            View::Brief => render_brief(slice),
            View::Full => render_full(slice),
        }
    }

    /// Renders `jsonl` like the session's agent would.
    struct Rendered(String);

    impl TranscriptSource for Rendered {
        async fn prepare(&self, _: Option<i64>, command: TranscriptCommand) -> Prepared {
            let body = expected(&self.0, command.view, command.prompts);
            transcript_reply(&command, SESSION, body)
        }
    }

    /// Runs `texts` through `serve` against `jsonl` and a fresh scheduler.
    async fn run(fake: &Arc<Fake>, jsonl: &str, texts: &[&str]) {
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
                media: None,
                from_name: None,
            })
            .unwrap();
        }
        drop(tx);
        let source = Arc::new(Rendered(jsonl.to_owned()));
        serve(rx, outbox, source, Some("cctg_bot".to_owned())).await;
        scheduler.await.unwrap();
    }

    #[tokio::test(start_paused = true)]
    async fn replies_are_the_agents_rendering_exactly() {
        let jsonl = fixture!("tool_use_result.jsonl");
        let fake = Arc::new(Fake::default());
        run(
            &fake,
            jsonl,
            &["/brief", "/full 2", "/brief@cctg_bot 100 5e55"],
        )
        .await;
        let want: Vec<String> = [
            expected(jsonl, View::Brief, DEFAULT_BRIEF_PROMPTS),
            expected(jsonl, View::Full, 2),
            expected(jsonl, View::Brief, 100),
        ]
        .iter()
        .flat_map(|text| split_for_telegram(text, SplitOptions::default()).chunks)
        .collect();
        assert_eq!(fake.texts(), want);
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
        let fake = Arc::new(Fake::default());
        run(&fake, &jsonl, &["/brief 3"]).await;
        let sent = fake.texts();
        let want = expected(&jsonl, View::Brief, 3);
        assert!(sent.len() > 1 && sent.len() <= 4, "{} chunks", sent.len());
        assert_eq!(sent.concat(), want);
    }

    #[tokio::test(start_paused = true)]
    async fn large_reply_goes_as_one_document() {
        let jsonl = long_session(8, 4000);
        let fake = Arc::new(Fake::default());
        run(&fake, &jsonl, &["/full 8"]).await;
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
        let want = expected(&jsonl, View::Brief, 3);
        let chunks = split_for_telegram(&want, SplitOptions::default()).chunks;
        assert!(chunks.len() >= 2);

        let fake = Fake::scripted(vec![None, Some(too_long())]);
        run(&fake, &jsonl, &["/brief 3"]).await;
        let ops = fake.ops();
        assert_eq!(ops.len(), 3, "{ops:?}");
        match &ops[2] {
            Op::SendDocument { document, .. } => {
                let document = String::from_utf8(document.bytes.clone()).unwrap();
                assert_eq!(format!("{}{document}", chunks[0]), want);
            }
            other => panic!("expected a document, got {other:?}"),
        }

        let fake = Arc::new(Fake {
            always: Some((400, "Bad Request: message is too long")),
            ..Fake::default()
        });
        run(&fake, &jsonl, &["/brief 3"]).await;
        let ops = fake.ops();
        assert_eq!(ops.len(), 2, "{ops:?}");
        assert!(matches!(ops[1], Op::SendDocument { .. }));
    }

    #[tokio::test(start_paused = true)]
    async fn other_errors_get_one_notice_and_the_worker_goes_on() {
        let jsonl = fixture!("final_answer.jsonl");
        let fake = Fake::scripted(vec![
            Some(ApiError::Telegram {
                code: 400,
                description: "Bad Request: message thread not found".to_owned(),
            }),
            None,
            None,
        ]);
        run(&fake, jsonl, &["/brief", "/full 1", "/brief x!"]).await;
        let texts = fake.texts();
        assert_eq!(texts.len(), 4, "{texts:?}");
        assert_eq!(texts[1], DELIVERY_FAILURE_NOTICE);
        assert_eq!(texts[2], expected(jsonl, View::Full, 1));
        assert_eq!(texts[3], USAGE);
    }

    #[tokio::test]
    async fn a_stopped_actor_gives_a_notice() {
        let (tx, rx) = mpsc::channel(1);
        drop(rx);
        let command = TranscriptCommand {
            view: View::Brief,
            prompts: 1,
            session_prefix: None,
        };
        assert_eq!(
            Asks(tx).prepare(None, command).await,
            Prepared::Notice(NO_ACTOR.to_owned())
        );
    }

    fn hook(session: &str, event: HookEvent) -> HookPost {
        HookPost::new(
            "box".into(),
            session.into(),
            r"C:\w\app".into(),
            String::new(),
            event,
        )
    }

    fn start(session: &str, pid: u32, parent: Option<u32>) -> HookPost {
        hook(
            session,
            HookEvent::SessionStart {
                source: Some("startup".into()),
                claude_pid: Some(pid),
                parent_claude_pid: parent,
            },
        )
    }

    #[test]
    fn a_command_finds_its_session_in_the_registry() {
        const A: &str = "aaaaaaaa-0000-4000-8000-000000000001";
        const B: &str = "aaaabbbb-0000-4000-8000-000000000002";
        const N: &str = "cccccccc-0000-4000-8000-000000000003";
        let mut registry = Registry::default();
        assert_eq!(
            resolve(&registry, None, None),
            Err("Запущенных сессий нет.".to_owned())
        );
        registry.apply_hook(&start(A, 10, None));
        registry.slots[0].topic_id = Some(100);
        registry.apply_hook(&start(B, 11, None));
        registry.apply_hook(&start(N, 12, Some(10)));
        registry.set_title(A, "Private title");
        // A slot topic: its current session; General: the newest running
        // top-level one (the nested run is newer but not top-level).
        assert_eq!(resolve(&registry, Some(100), None), Ok(A.to_owned()));
        assert_eq!(resolve(&registry, None, None), Ok(B.to_owned()));
        assert_eq!(resolve(&registry, Some(555), None), Ok(B.to_owned()));
        // A prefix, anywhere, among every known session.
        assert_eq!(
            resolve(&registry, Some(100), Some("aaaab")),
            Ok(B.to_owned())
        );
        assert_eq!(resolve(&registry, None, Some("cc")), Ok(N.to_owned()));
        let ambiguous = resolve(&registry, None, Some("aaaa")).unwrap_err();
        assert_eq!(
            ambiguous,
            "Под это начало id подходят 2 сессий, уточните:\naaaabbbb · идёт\naaaaaaaa · идёт · Private title"
        );
        assert_eq!(
            resolve(&registry, None, Some("dead")),
            Err("Нет известной hub сессии с таким началом id.".to_owned())
        );
        registry.apply_hook(&hook(
            B,
            HookEvent::SessionEnd {
                reason: None,
                claude_pid: None,
            },
        ));
        assert_eq!(resolve(&registry, None, None), Ok(A.to_owned()));
    }

    #[test]
    fn the_refused_notice_names_the_missing_config_folder() {
        // An agent refuses only when it cannot find its project folder at
        // all; the hub never asks it for another project's transcript.
        let Prepared::Notice(text) = unavailable(Unavailable::Refused, SESSION) else {
            panic!();
        };
        assert!(
            text.contains("CLAUDE_CONFIG_DIR") && !text.contains("не там"),
            "{text}"
        );
    }

    #[test]
    fn notices_name_the_short_id_only() {
        for why in [
            Unavailable::Nested,
            Unavailable::NoAgent,
            Unavailable::OldAgent,
            Unavailable::Missing,
            Unavailable::Refused,
            Unavailable::Unreadable,
            Unavailable::TooLarge,
            Unavailable::NoAnswer,
            Unavailable::LinkLost,
            Unavailable::Failed,
        ] {
            let Prepared::Notice(text) = unavailable(why, SESSION) else {
                panic!();
            };
            assert!(
                text.contains("5e551017") && !text.contains(SESSION),
                "{text}"
            );
        }
        // Only the ended session's notice names the id to resume it.
        let Prepared::Notice(text) = unavailable(Unavailable::Ended, SESSION) else {
            panic!();
        };
        assert!(
            text.ends_with(&format!("claude --resume {SESSION}")),
            "{text}"
        );
        let command = TranscriptCommand {
            view: View::Full,
            prompts: 1,
            session_prefix: None,
        };
        assert_eq!(
            transcript_reply(&command, SESSION, " \n".into()),
            Prepared::Notice("В сессии 5e551017 пока нечего показывать.".to_owned())
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
