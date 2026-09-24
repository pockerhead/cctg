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
//! Pure: the slots actor owns every [`Buffer`] through the registry and does
//! all IO. Nothing here holds a Telegram user id: [`crate::hub::updates`]
//! drops it before a message reaches the actor.

use std::collections::VecDeque;

use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use super::permissions::MAX_CALLBACK_DATA;
use super::registry::cut;

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

/// One topic message as the agent will get it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Parked {
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
}

/// Heads a forwarded message in the content the session reads.
pub const FORWARDED: &str = "(переслано)";

impl Parked {
    /// What the session reads: the quoted words as `> ` lines and a blank
    /// line, then [`FORWARDED`] on its own line for a forward, then the text.
    pub fn content(&self) -> String {
        let mut content = String::new();
        if let Some(quote) = &self.quote {
            for line in quote.lines() {
                content.push_str(format!("> {line}").trim_end());
                content.push('\n');
            }
            content.push('\n');
        }
        if self.forwarded {
            content.push_str(FORWARDED);
            content.push('\n');
        }
        content.push_str(&self.text);
        content
    }
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
    pub message_id: Option<i64>,
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

    /// Keeps `message` as the newest. `true`: the oldest was dropped for it.
    pub fn push(&mut self, message: Parked) -> bool {
        let dropped = self.messages.len() >= MAX_BUFFERED;
        if dropped {
            self.messages.pop_front();
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
            message_id,
            thread_id: 100,
            text: format!("m{message_id}"),
            reply_to: None,
            quote: None,
            forwarded: false,
        }
    }

    #[test]
    fn one_more_than_the_cap_drops_the_oldest() {
        let mut buffer = Buffer::default();
        for id in 0..MAX_BUFFERED as i64 {
            assert!(!buffer.push(parked(id)));
        }
        assert!(buffer.push(parked(50)));
        assert!(buffer.push(parked(51)));
        let ids: Vec<i64> = buffer.messages.iter().map(|m| m.message_id).collect();
        assert_eq!(ids, (2..52).collect::<Vec<_>>());
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
    fn closing_a_period_forgets_its_marks_and_hands_back_the_note() {
        let mut buffer = Buffer {
            overflow_told: true,
            queued_told: true,
            resume_asked: true,
            resume: Some(ResumeNote {
                session: A.into(),
                number: 1,
                message_id: Some(7),
            }),
            ..Buffer::default()
        };
        assert!(!buffer.is_idle());
        let note = buffer.close();
        assert_eq!(note.and_then(|note| note.message_id), Some(7));
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
        // A message kept before quotes and forwards were read.
        let old: Parked =
            serde_json::from_str(r#"{"message_id":1,"thread_id":100,"text":"m1"}"#).unwrap();
        assert_eq!(old, parked(1));
        let plain = serde_json::to_string(&parked(1)).unwrap();
        assert!(
            !plain.contains("quote") && !plain.contains("forwarded"),
            "{plain}"
        );
        let mut buffer = Buffer::default();
        buffer.push(Parked {
            reply_to: Some(5),
            quote: Some("q".into()),
            ..parked(1)
        });
        buffer.push(Parked {
            forwarded: true,
            ..parked(2)
        });
        buffer.resume = Some(ResumeNote {
            session: A.into(),
            number: 3,
            message_id: None,
        });
        let text = serde_json::to_string(&buffer).unwrap();
        assert_eq!(serde_json::from_str::<Buffer>(&text).unwrap(), buffer);
    }
}
