//! Messages for a slot whose session cannot take them now (TASK-017).
//!
//! A topic message waits in its slot, in `registry.json`, until a live
//! top-level session of that slot has a bound agent; then the slots actor
//! hands the messages to that agent oldest first and the offline period of
//! the slot ends. At most [`MAX_BUFFERED`] wait per slot: one more drops the
//! oldest, and the topic is told once per period. A dead slot shows one
//! Resume button per period; a press only records the wish (TASK-019 acts on
//! it).
//!
//! A message with a file (TASK-032) waits like any other, as a reference to
//! the Telegram file ([`Attachment`]), never its bytes: the hub downloads it
//! when the session takes it.
//!
//! Pure: the slots actor owns every [`Buffer`] through the registry and does
//! all IO. Nothing here holds a Telegram user id: [`crate::hub::updates`]
//! drops it before a message reaches the actor.

use std::collections::VecDeque;

use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use super::chat::{Chat, MessageKey, Place};
use super::permissions::MAX_CALLBACK_DATA;
use super::registry::cut;
use crate::wire::FileKind;

/// Messages kept per slot; one more drops the oldest.
pub const MAX_BUFFERED: usize = 50;
const RESUME: &str = "resume";
const SHORT_ID: usize = 8;

/// A slot whose session is running but has no agent on line got a message.
pub const QUEUED_NOTICE: &str =
    "Сессия этой темы сейчас не на связи. Сообщения сохранены и дойдут, когда она подключится.";
/// One more message than [`MAX_BUFFERED`] came in this period.
pub const OVERFLOW_NOTICE: &str = "В этой теме ждут уже 50 сообщений: самые старые отбрасываются.";
/// The Resume message after its period ended, without buttons.
pub const RESUMED_TEXT: &str = "Сессия снова на связи, сохранённые сообщения доставлены.";
pub const ANSWER_UNAVAILABLE: &str = "Запуск из Telegram пока не подключён. Возобновите сессию в терминале: сохранённые сообщения дойдут сами.";
pub const ANSWER_ALIVE: &str = "Сессия уже на связи";
/// A message of a kind the session cannot take (a sticker, a location...).
pub const UNSUPPORTED_NOTICE: &str =
    "Такие сообщения в сессию не доходят: только текст, фото, файлы, видео, голосовые и аудио.";
/// A file larger than a bot may download.
pub const TOO_BIG_NOTICE: &str =
    "Файл больше 20 МБ: бот не может скачать его из Telegram, в сессию он не передан.";
/// The file could not be downloaded from Telegram.
pub const FETCH_FAILED_NOTICE: &str =
    "Не удалось скачать файл из Telegram, в сессию он не передан. Пришлите его ещё раз.";
/// The agent's link closed during every try to hand the file over.
pub const LINK_LOST_NOTICE: &str = "Файл не передан: связь с сессией обрывалась при каждой попытке его передать. Пришлите его ещё раз.";
/// The session's agent is too old for files; its caption still goes.
pub const OLD_AGENT_NOTICE: &str = "Файл не передан: клиент cctg этой сессии не принимает файлы. Обновите его (⬆️ Обновить) и пришлите файл ещё раз.";

/// The Telegram file of a kept message: enough to download it later
/// (`file_id` stays valid), never its bytes.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Attachment {
    pub kind: FileKind,
    pub file_id: String,
    /// The sender's file name; photos and voice messages have none.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    /// As Telegram announced it; may be missing.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub size: Option<u64>,
}

/// One topic message as the agent will get it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Parked {
    /// The chat it was written in; its ids are ids of this chat.
    pub chat: Chat,
    pub message_id: i64,
    pub thread_id: i64,
    pub text: String,
    /// An explicit reply (see [`crate::hub::updates::Inbound::reply_to`]).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reply_to: Option<i64>,
    /// See [`crate::hub::updates::Inbound::quote`].
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub quote: Option<String>,
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub forwarded: bool,
    /// The file of the message (TASK-032); `text` is then its caption,
    /// maybe empty.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub file: Option<Attachment>,
    /// See [`crate::hub::updates::Inbound::from_name`].
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub from_name: Option<String>,
    /// The group topic's messages since the last mention of the agent,
    /// read before this one (TASK-077).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub history: Option<History>,
    /// A group message that addresses the agent (TASK-080): `@<bot>` in its
    /// words (taken out of `text`) or a reply to the bot's message.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub mention: bool,
}

/// The kept group messages a mention takes along ([`crate::hub::mention`]).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct History {
    pub text: String,
    /// Messages in it.
    pub count: u32,
    /// Older messages dropped before the mention came.
    #[serde(default, skip_serializing_if = "is_zero")]
    pub dropped: u32,
    /// At most this many characters of `text` reach the session.
    pub limit: u32,
    pub state: HistoryState,
}

fn is_zero(n: &u32) -> bool {
    *n == 0
}

/// Where a [`History`] is on its way to the session.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum HistoryState {
    /// Longer than its limit: the agent is to compress it first.
    Pending,
    /// Whole.
    Full,
    /// The agent's summary.
    Compressed,
    /// Its newest part only. A state of a later hub reads as this: the
    /// message goes, never waits.
    #[serde(other)]
    Cut,
}

/// Russian plural of «сообщение» after `n`.
fn messages(n: u32) -> &'static str {
    match (n % 10, n % 100) {
        (1, 11) | (_, 12..=14) | (5..=9 | 0, _) => "сообщений",
        (1, _) => "сообщение",
        _ => "сообщения",
    }
}

impl History {
    /// The block the session reads before the mention.
    fn block(&self) -> String {
        let mut head = format!(
            "(история темы группы с прошлого обращения к вам: {} {}",
            self.count,
            messages(self.count)
        );
        match self.state {
            HistoryState::Compressed => head.push_str(", сжато"),
            HistoryState::Cut => head.push_str(", начало обрезано"),
            HistoryState::Pending | HistoryState::Full => {}
        }
        if self.dropped > 0 {
            head.push_str(&format!(", ранние {} не сохранились", self.dropped));
        }
        format!("{head})\n{}\n(конец истории)", self.text)
    }
}

/// Heads a forwarded message in the content the session reads.
pub const FORWARDED: &str = "(переслано)";
/// Heads a group message that addresses the agent (TASK-080).
pub const MENTION_MARK: &str = "(обращение к вам из группы, где открыта эта сессия)";

impl Parked {
    /// The topic it was written in.
    pub fn place(&self) -> Place {
        Place::topic(self.chat, self.thread_id)
    }

    pub fn key(&self) -> MessageKey {
        MessageKey::new(self.chat, self.message_id)
    }

    /// Its history waits to be compressed (TASK-077): it does not go yet.
    pub fn pending(&self) -> bool {
        self.history
            .as_ref()
            .is_some_and(|history| history.state == HistoryState::Pending)
    }

    /// What the session reads: the group history block and a blank line
    /// (TASK-077), [`MENTION_MARK`] on its own line for a mention
    /// (TASK-080), the quoted words as `> ` lines and a blank line, then
    /// `Name: ` of a team member (TASK-036), then [`FORWARDED`] on its own
    /// line for a forward, then the text.
    pub fn content(&self) -> String {
        let mut content = String::new();
        if let Some(history) = &self.history {
            content.push_str(&history.block());
            content.push_str("\n\n");
        }
        if self.mention {
            content.push_str(MENTION_MARK);
            content.push('\n');
        }
        if let Some(quote) = &self.quote {
            for line in quote.lines() {
                content.push_str(format!("> {line}").trim_end());
                content.push('\n');
            }
            content.push('\n');
        }
        if let Some(name) = &self.from_name {
            content.push_str(name);
            content.push(':');
            if self.forwarded || !self.text.is_empty() {
                content.push(' ');
            }
        }
        if self.forwarded {
            content.push_str(FORWARDED);
            content.push('\n');
        }
        content.push_str(&self.text);
        content
    }
}

/// Between the messages of a burst that go as one inbound (TASK-048).
pub const PART_SEPARATOR: &str = "\n\n---\n\n";

/// What the session reads of messages that go as one inbound: their
/// [`Parked::content`] in order, [`PART_SEPARATOR`] between them.
pub fn burst_content(parts: &[Parked]) -> String {
    parts
        .iter()
        .map(Parked::content)
        .collect::<Vec<_>>()
        .join(PART_SEPARATOR)
}

/// The Resume message of an offline period.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ResumeNote {
    /// The ended session the button names.
    pub session: String,
    /// Which Resume send of this hub run the message is: Telegram's answer
    /// to an earlier period's send never takes a later period's note.
    #[serde(default)]
    pub number: u64,
    /// `None` while the send is out, or when Telegram gave no id; such a
    /// button is never edited away.
    #[serde(default)]
    pub message: Option<MessageKey>,
}

/// What a slot keeps between an offline period's first message and the
/// hand-off of its last one.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Buffer {
    #[serde(default)]
    pub messages: VecDeque<Parked>,
    /// The topic was told that old messages are dropped.
    #[serde(default)]
    pub overflow_told: bool,
    /// The topic was told that the running session is not on line.
    #[serde(default)]
    pub queued_told: bool,
    /// Sent (or being sent) at most once per period.
    #[serde(default)]
    pub resume: Option<ResumeNote>,
    /// A Resume press asked to bring the session back.
    #[serde(default)]
    pub resume_asked: bool,
}

impl Buffer {
    /// Nothing kept and no period open: not written to `registry.json`.
    pub fn is_idle(&self) -> bool {
        *self == Self::default()
    }

    /// Keeps `message` as the newest. `true`: the oldest was dropped for it,
    /// or the next oldest when `keep_front` (the oldest is a file on its
    /// way to the agent: dropping it would let later messages overtake it).
    pub fn push(&mut self, message: Parked, keep_front: bool) -> bool {
        let dropped = self.messages.len() >= MAX_BUFFERED;
        if dropped {
            self.messages.remove(usize::from(keep_front));
        }
        self.messages.push_back(message);
        dropped
    }

    /// Ends the period once every message went out; returns its Resume
    /// message, whose button should go.
    pub fn close(&mut self) -> Option<ResumeNote> {
        debug_assert!(self.messages.is_empty());
        let note = self.resume.take();
        *self = Self::default();
        note
    }
}

fn short(session: &str) -> &str {
    session
        .char_indices()
        .nth(SHORT_ID)
        .map_or(session, |(end, _)| &session[..end])
}

/// The text of the Resume message for `session`.
pub fn resume_text(session: &str) -> String {
    let text = format!(
        "Сессия {} завершилась. Сообщения из темы сохраняются (не больше {MAX_BUFFERED}) \
         и дойдут до первой сессии, которая оживёт в этой теме.\n\
         В терминале: claude --resume {session}",
        short(session)
    );
    cut(&text, transcript::TELEGRAM_TEXT_LIMIT)
}

fn is_token(session: &str) -> bool {
    !session.is_empty()
        && RESUME.len() + 1 + session.len() <= MAX_CALLBACK_DATA
        && session
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || byte == b'-')
}

/// `resume:<session>`, or `None` when the id would not fit the Bot API's
/// 64 bytes (Claude Code ids are 36-byte UUIDs) or is not a plain token.
pub fn callback_data(session: &str) -> Option<String> {
    is_token(session).then(|| format!("{RESUME}:{session}"))
}

/// The session of a Resume button; anything else is not one.
pub fn parse_callback(data: &str) -> Option<&str> {
    let (action, session) = data.split_once(':')?;
    (action == RESUME && is_token(session)).then_some(session)
}

pub fn keyboard(data: String) -> Value {
    json!({ "inline_keyboard": [[
        { "text": "Возобновить", "callback_data": data },
    ]] })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::hub::permissions;

    const A: &str = "aaaaaaaa-0000-4000-8000-000000000001";

    fn parked(message_id: i64) -> Parked {
        Parked {
            chat: Chat::GROUP,
            message_id,
            thread_id: 100,
            text: format!("m{message_id}"),
            reply_to: None,
            quote: None,
            forwarded: false,
            file: None,
            from_name: None,
            history: None,
            mention: false,
        }
    }

    #[test]
    fn one_more_than_the_cap_drops_the_oldest() {
        let mut buffer = Buffer::default();
        for id in 0..MAX_BUFFERED as i64 {
            assert!(!buffer.push(parked(id), false));
        }
        assert!(buffer.push(parked(50), false));
        assert!(buffer.push(parked(51), false));
        let ids: Vec<i64> = buffer.messages.iter().map(|m| m.message_id).collect();
        assert_eq!(ids, (2..52).collect::<Vec<_>>());
    }

    #[test]
    fn a_full_buffer_keeps_its_front_when_asked_and_drops_the_next_oldest() {
        let mut buffer = Buffer::default();
        for id in 0..MAX_BUFFERED as i64 {
            buffer.push(parked(id), false);
        }
        assert!(buffer.push(parked(50), true));
        assert!(buffer.push(parked(51), true));
        let ids: Vec<i64> = buffer.messages.iter().map(|m| m.message_id).collect();
        assert_eq!(ids, [0].into_iter().chain(3..52).collect::<Vec<_>>());
    }

    #[test]
    fn the_content_puts_the_quote_first_and_marks_a_forward() {
        assert_eq!(parked(1).content(), "m1");
        let reply = Parked {
            quote: Some("Удалить build/?\n\nи dist/".into()),
            text: "Удаляй".into(),
            ..parked(1)
        };
        assert_eq!(reply.content(), "> Удалить build/?\n>\n> и dist/\n\nУдаляй");
        let forward = Parked {
            forwarded: true,
            text: "чужие\nслова".into(),
            ..parked(2)
        };
        assert_eq!(forward.content(), "(переслано)\nчужие\nслова");
    }

    #[test]
    fn a_team_member_is_named_before_the_words_and_after_the_quote() {
        let named = |parked: Parked| Parked {
            from_name: Some("Анна".into()),
            ..parked
        };
        assert_eq!(named(parked(1)).content(), "Анна: m1");
        let reply = named(Parked {
            quote: Some("q".into()),
            ..parked(2)
        });
        assert_eq!(reply.content(), "> q\n\nАнна: m2");
        let forward = named(Parked {
            forwarded: true,
            ..parked(3)
        });
        assert_eq!(forward.content(), "Анна: (переслано)\nm3");
        // A file without a caption.
        let bare = named(Parked {
            text: String::new(),
            ..parked(4)
        });
        assert_eq!(bare.content(), "Анна:");
        // Each part of a burst keeps its own author.
        let other = Parked {
            from_name: Some("Иван".into()),
            ..parked(5)
        };
        assert_eq!(
            burst_content(&[named(parked(1)), other]),
            "Анна: m1\n\n---\n\nИван: m5"
        );
        assert!(
            !serde_json::to_string(&parked(1))
                .unwrap()
                .contains("from_name")
        );
        let kept = serde_json::to_string(&named(parked(1))).unwrap();
        assert_eq!(
            serde_json::from_str::<Parked>(&kept).unwrap(),
            named(parked(1))
        );
    }

    #[test]
    fn a_burst_keeps_each_message_marked_in_order_between_separators() {
        assert_eq!(burst_content(&[parked(1)]), "m1");
        let forward = Parked {
            forwarded: true,
            ..parked(1)
        };
        let reply = Parked {
            quote: Some("q".into()),
            ..parked(2)
        };
        assert_eq!(
            burst_content(&[forward, reply, parked(3)]),
            "(переслано)\nm1\n\n---\n\n> q\n\nm2\n\n---\n\nm3"
        );
    }

    fn history(state: HistoryState, dropped: u32) -> History {
        History {
            text: "Анна: a\n\n---\n\nИван: b".into(),
            count: 2,
            dropped,
            limit: 4000,
            state,
        }
    }

    /// TASK-077: a mention reads the group history first, then itself.
    #[test]
    fn a_mention_reads_the_history_block_before_its_own_words() {
        let mention = Parked {
            from_name: Some("Анна".into()),
            quote: Some("q".into()),
            history: Some(history(HistoryState::Full, 0)),
            ..parked(3)
        };
        assert_eq!(
            mention.content(),
            "(история темы группы с прошлого обращения к вам: 2 сообщения)\n\
             Анна: a\n\n---\n\nИван: b\n(конец истории)\n\n> q\n\nАнна: m3"
        );
        assert!(!mention.pending());
        for (state, dropped, head) in [
            (HistoryState::Compressed, 0, "2 сообщения, сжато)"),
            (HistoryState::Cut, 0, "2 сообщения, начало обрезано)"),
            (
                HistoryState::Cut,
                7,
                "2 сообщения, начало обрезано, ранние 7 не сохранились)",
            ),
            (
                HistoryState::Full,
                1,
                "2 сообщения, ранние 1 не сохранились)",
            ),
        ] {
            let content = Parked {
                history: Some(history(state, dropped)),
                ..parked(1)
            }
            .content();
            assert!(
                content.starts_with(&format!(
                    "(история темы группы с прошлого обращения к вам: {head}\n"
                )),
                "{content}"
            );
            assert!(content.ends_with("(конец истории)\n\nm1"), "{content}");
        }
        let waiting = Parked {
            history: Some(history(HistoryState::Pending, 0)),
            ..parked(1)
        };
        assert!(waiting.pending());
        for (n, word) in [
            (1, "сообщение"),
            (2, "сообщения"),
            (5, "сообщений"),
            (11, "сообщений"),
            (12, "сообщений"),
            (21, "сообщение"),
            (22, "сообщения"),
            (111, "сообщений"),
            (200, "сообщений"),
        ] {
            assert_eq!(messages(n), word, "{n}");
        }
    }

    /// TASK-080: a mention is marked after the history block and before its
    /// quote; a plain message has no mark and no `mention` key.
    #[test]
    fn a_mention_is_marked_as_addressed_to_the_session() {
        let mention = Parked {
            from_name: Some("Анна".into()),
            text: "дальше что?".into(),
            mention: true,
            ..parked(3)
        };
        assert_eq!(
            mention.content(),
            "(обращение к вам из группы, где открыта эта сессия)\nАнна: дальше что?"
        );
        let with_all = Parked {
            quote: Some("q".into()),
            history: Some(history(HistoryState::Full, 0)),
            ..mention.clone()
        };
        assert_eq!(
            with_all.content(),
            "(история темы группы с прошлого обращения к вам: 2 сообщения)\n\
             Анна: a\n\n---\n\nИван: b\n(конец истории)\n\n\
             (обращение к вам из группы, где открыта эта сессия)\n> q\n\nАнна: дальше что?"
        );
        assert!(!parked(1).content().contains(MENTION_MARK));
        assert!(
            !serde_json::to_string(&parked(1))
                .unwrap()
                .contains("mention")
        );
        let text = serde_json::to_string(&mention).unwrap();
        assert!(text.contains(r#""mention":true"#), "{text}");
        assert_eq!(serde_json::from_str::<Parked>(&text).unwrap(), mention);
    }

    /// TASK-077: a message without history is written as before; one with
    /// it round-trips; a state of a later hub reads as cut (it never waits).
    #[test]
    fn the_history_is_written_only_with_one_and_an_unknown_state_is_cut() {
        let plain = serde_json::to_string(&parked(1)).unwrap();
        assert_eq!(
            plain,
            r#"{"chat":{"group":0},"message_id":1,"thread_id":100,"text":"m1"}"#
        );
        let with = Parked {
            history: Some(history(HistoryState::Pending, 3)),
            ..parked(1)
        };
        let text = serde_json::to_string(&with).unwrap();
        assert!(text.contains(r#""state":"pending""#), "{text}");
        assert!(text.contains(r#""dropped":3"#), "{text}");
        assert_eq!(serde_json::from_str::<Parked>(&text).unwrap(), with);
        assert!(
            !serde_json::to_string(&history(HistoryState::Full, 0))
                .unwrap()
                .contains("dropped")
        );
        let later: History =
            serde_json::from_str(r#"{"text":"t","count":1,"limit":5,"state":"future"}"#).unwrap();
        assert_eq!(later.state, HistoryState::Cut);
    }

    #[test]
    fn closing_a_period_forgets_its_marks_and_hands_back_the_note() {
        let mut buffer = Buffer {
            overflow_told: true,
            queued_told: true,
            resume_asked: true,
            resume: Some(ResumeNote {
                session: A.into(),
                number: 1,
                message: Some(MessageKey::new(Chat::GROUP, 7)),
            }),
            ..Buffer::default()
        };
        assert!(!buffer.is_idle());
        let note = buffer.close();
        assert_eq!(
            note.and_then(|note| note.message),
            Some(MessageKey::new(Chat::GROUP, 7))
        );
        assert!(buffer.is_idle());
    }

    #[test]
    fn resume_data_fits_round_trips_and_never_looks_like_a_verdict() {
        let data = callback_data(A).unwrap();
        assert_eq!(data, format!("resume:{A}"));
        assert!(data.len() <= permissions::MAX_CALLBACK_DATA);
        assert_eq!(parse_callback(&data), Some(A));
        assert_eq!(permissions::parse_callback(&data), None);
        for foreign in [
            "",
            "resume",
            "resume:",
            "resume:a b",
            "resume:a:b",
            "allow:abcde",
            "deny:abcde",
            "Resume:abc",
        ] {
            assert_eq!(parse_callback(foreign), None, "{foreign}");
        }
        let longest = "a".repeat(permissions::MAX_CALLBACK_DATA - "resume:".len());
        assert_eq!(callback_data(&longest).map(|d| d.len()), Some(64));
        assert_eq!(callback_data(&format!("{longest}a")), None);
        assert_eq!(callback_data("ы"), None);
        let buttons = keyboard(data.clone());
        assert_eq!(buttons["inline_keyboard"][0][0]["callback_data"], data);
    }

    #[test]
    fn the_resume_text_names_the_session_and_the_answers_fit_a_toast() {
        let text = resume_text(A);
        assert!(text.contains("aaaaaaaa завершилась"), "{text}");
        assert!(text.ends_with(&format!("claude --resume {A}")), "{text}");
        // answerCallbackQuery takes at most 200 characters.
        for answer in [ANSWER_UNAVAILABLE, ANSWER_ALIVE] {
            assert!(answer.chars().count() <= 200, "{answer}");
        }
    }

    #[test]
    fn an_old_file_without_a_buffer_and_a_full_one_both_load() {
        let old: Buffer = serde_json::from_str("{}").unwrap();
        assert!(old.is_idle());
        // A message kept before quotes and forwards were read (its chat
        // comes from the version 1 migration, TASK-061).
        let old: Parked = serde_json::from_str(
            r#"{"chat":{"group":0},"message_id":1,"thread_id":100,"text":"m1"}"#,
        )
        .unwrap();
        assert_eq!(old, parked(1));
        let plain = serde_json::to_string(&parked(1)).unwrap();
        assert!(
            !plain.contains("quote") && !plain.contains("forwarded"),
            "{plain}"
        );
        let mut buffer = Buffer::default();
        buffer.push(
            Parked {
                reply_to: Some(5),
                quote: Some("q".into()),
                ..parked(1)
            },
            false,
        );
        buffer.push(
            Parked {
                forwarded: true,
                ..parked(2)
            },
            false,
        );
        buffer.resume = Some(ResumeNote {
            session: A.into(),
            number: 3,
            message: None,
        });
        buffer.push(
            Parked {
                text: String::new(),
                file: Some(Attachment {
                    kind: FileKind::Photo,
                    file_id: "AgACAgIAAx0".into(),
                    name: None,
                    size: Some(90_000),
                }),
                ..parked(3)
            },
            false,
        );
        let text = serde_json::to_string(&buffer).unwrap();
        assert_eq!(serde_json::from_str::<Buffer>(&text).unwrap(), buffer);
        // A file is kept as its reference, and a text message has no file key.
        assert!(
            text.contains(r#""file":{"kind":"photo","file_id":"AgACAgIAAx0","size":90000}"#),
            "{text}"
        );
        assert!(!serde_json::to_string(&parked(1)).unwrap().contains("file"));
    }
}
