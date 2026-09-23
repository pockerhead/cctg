//! Permission prompts: the topic message for a relayed `permission_request`,
//! its Allow/Deny buttons and the book of prompts the hub has shown.
//!
//! Pure: the slots actor owns a [`Prompts`] and does all IO. A button carries
//! only the action and the five-letter request id; the prompt is found by the
//! Telegram message the button belongs to, because two sessions can issue the
//! same request id and one message never belongs to two prompts.
//!
//! Claude Code never says that a prompt was answered in the terminal, so an
//! undecided prompt is not proof that the request is still pending. A verdict
//! for a request that is no longer pending is ignored by Claude Code.

use std::collections::{HashMap, VecDeque};

use serde_json::{Value, json};
use transcript::{TELEGRAM_TEXT_LIMIT, telegram_len};

use super::registry::cut;
use crate::channel::is_request_id;
use crate::wire::{Behavior, PermissionRequest};

/// Prompts remembered at a time; beyond this the oldest decided one, or else
/// the oldest one, is forgotten (its buttons then answer "expired").
pub const MAX_PROMPTS: usize = 256;
/// Bot API limit for `callback_data`.
pub const MAX_CALLBACK_DATA: usize = 64;
const ALLOW: &str = "allow";
const DENY: &str = "deny";
pub const ALLOWED_MARK: &str = "\n\n✅ Разрешено из Telegram";
pub const DENIED_MARK: &str = "\n\n⛔ Отклонено из Telegram";
pub const ANSWER_ALLOWED: &str = "Разрешено";
pub const ANSWER_DENIED: &str = "Отклонено";
pub const ANSWER_DECIDED: &str = "Уже решено";
pub const ANSWER_EXPIRED: &str = "Запрос устарел";
pub const ANSWER_OFFLINE: &str = "Сессия не на связи, ответьте в терминале";

pub fn callback_data(behavior: Behavior, request_id: &str) -> String {
    let action = match behavior {
        Behavior::Allow => ALLOW,
        Behavior::Deny => DENY,
    };
    format!("{action}:{request_id}")
}

/// `allow:<id>` or `deny:<id>` with a valid request id; anything else is not
/// a permission button.
pub fn parse_callback(data: &str) -> Option<(Behavior, &str)> {
    let (action, request_id) = data.split_once(':')?;
    let behavior = match action {
        ALLOW => Behavior::Allow,
        DENY => Behavior::Deny,
        _ => return None,
    };
    is_request_id(request_id).then_some((behavior, request_id))
}

pub fn keyboard(request_id: &str) -> Value {
    json!({ "inline_keyboard": [[
        { "text": "Разрешить", "callback_data": callback_data(Behavior::Allow, request_id) },
        { "text": "Запретить", "callback_data": callback_data(Behavior::Deny, request_id) },
    ]] })
}

/// Sent with the decision edit: an explicit empty keyboard removes the buttons
/// whatever the default of `editMessageText` without `reply_markup` is.
pub fn no_keyboard() -> Value {
    json!({ "inline_keyboard": [] })
}

fn mark(behavior: Behavior) -> &'static str {
    match behavior {
        Behavior::Allow => ALLOWED_MARK,
        Behavior::Deny => DENIED_MARK,
    }
}

/// The prompt as plain text (no parse mode, so nothing Claude sent is markup),
/// cut so that the decision mark still fits the Telegram limit.
pub fn prompt_text(request: &PermissionRequest) -> String {
    let mut text = format!("Запрос разрешения: {}", request.tool_name.trim());
    if !request.description.trim().is_empty() {
        text.push('\n');
        text.push_str(request.description.trim());
    }
    if !request.input_preview.trim().is_empty() {
        text.push_str("\n\n");
        text.push_str(request.input_preview.trim());
    }
    let room = telegram_len(ALLOWED_MARK).max(telegram_len(DENIED_MARK));
    cut(&text, TELEGRAM_TEXT_LIMIT - room)
}

/// The prompt after a decision; within the limit because [`prompt_text`]
/// left room for the mark.
pub fn decided_text(prompt: &str, behavior: Behavior) -> String {
    format!("{prompt}{}", mark(behavior))
}

pub fn answer(behavior: Behavior) -> &'static str {
    match behavior {
        Behavior::Allow => ANSWER_ALLOWED,
        Behavior::Deny => ANSWER_DENIED,
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Prompt {
    /// The agent connection that relayed the request.
    pub conn: u64,
    /// Device and claude process of that agent: after a link drop the same
    /// process comes back on another connection.
    pub host: String,
    pub claude_pid: Option<u32>,
    /// The session the agent was bound to when the request came in.
    pub session: String,
    pub request_id: String,
    pub text: String,
    /// Handed to Telegram (the send may still be in flight).
    pub sent: bool,
    /// Set once Telegram accepted the message.
    pub message_id: Option<i64>,
    pub decided: Option<Behavior>,
}

/// Prompt book, bounded by [`MAX_PROMPTS`].
#[derive(Debug, Default)]
pub struct Prompts {
    next: u64,
    order: VecDeque<u64>,
    prompts: HashMap<u64, Prompt>,
    by_message: HashMap<i64, u64>,
}

impl Prompts {
    /// Adds a prompt and returns its key, or `None` when an undecided prompt
    /// of the same session and request id is already there.
    pub fn open(&mut self, prompt: Prompt) -> Option<u64> {
        let duplicate = self.prompts.values().any(|known| {
            known.decided.is_none()
                && known.session == prompt.session
                && known.request_id == prompt.request_id
        });
        if duplicate {
            return None;
        }
        if self.order.len() >= MAX_PROMPTS {
            let victim = self
                .order
                .iter()
                .position(|key| self.prompts.get(key).is_some_and(|p| p.decided.is_some()))
                .unwrap_or(0);
            if let Some(key) = self.order.remove(victim) {
                self.forget(key);
            }
        }
        let key = self.next;
        self.next += 1;
        self.order.push_back(key);
        self.prompts.insert(key, prompt);
        Some(key)
    }

    fn forget(&mut self, key: u64) {
        if let Some(gone) = self.prompts.remove(&key)
            && let Some(message_id) = gone.message_id
        {
            self.by_message.remove(&message_id);
        }
    }

    /// Drops a prompt that never reached Telegram.
    pub fn remove(&mut self, key: u64) {
        self.order.retain(|known| *known != key);
        self.forget(key);
    }

    pub fn get(&self, key: u64) -> Option<&Prompt> {
        self.prompts.get(&key)
    }

    pub fn get_mut(&mut self, key: u64) -> Option<&mut Prompt> {
        self.prompts.get_mut(&key)
    }

    /// Telegram accepted the prompt as `message_id`.
    pub fn delivered(&mut self, key: u64, message_id: i64) {
        if let Some(prompt) = self.prompts.get_mut(&key) {
            prompt.message_id = Some(message_id);
            self.by_message.insert(message_id, key);
        }
    }

    pub fn by_message(&self, message_id: i64) -> Option<u64> {
        self.by_message.get(&message_id).copied()
    }

    /// Keys of prompts not handed to Telegram yet, oldest first.
    pub fn unsent(&self) -> Vec<u64> {
        self.order
            .iter()
            .copied()
            .filter(|key| self.prompts.get(key).is_some_and(|p| !p.sent))
            .collect()
    }

    pub fn len(&self) -> usize {
        self.prompts.len()
    }

    pub fn is_empty(&self) -> bool {
        self.prompts.is_empty()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn request(preview: &str) -> PermissionRequest {
        PermissionRequest {
            request_id: "abcde".into(),
            tool_name: "Bash".into(),
            description: "run the tests".into(),
            input_preview: preview.into(),
        }
    }

    fn prompt(session: &str, request_id: &str) -> Prompt {
        Prompt {
            conn: 1,
            host: "box".into(),
            claude_pid: Some(10),
            session: session.into(),
            request_id: request_id.into(),
            text: "t".into(),
            sent: false,
            message_id: None,
            decided: None,
        }
    }

    #[test]
    fn callback_data_fits_and_round_trips() {
        // The longest valid id and the longer action.
        for behavior in [Behavior::Allow, Behavior::Deny] {
            let data = callback_data(behavior, "zzzzz");
            assert!((1..=MAX_CALLBACK_DATA).contains(&data.len()), "{data}");
            assert_eq!(parse_callback(&data), Some((behavior, "zzzzz")));
        }
        let buttons = keyboard("abcde");
        let data: Vec<&str> = buttons["inline_keyboard"][0]
            .as_array()
            .unwrap()
            .iter()
            .map(|button| button["callback_data"].as_str().unwrap())
            .collect();
        assert_eq!(data, ["allow:abcde", "deny:abcde"]);
    }

    #[test]
    fn foreign_callback_data_is_not_a_verdict() {
        for data in [
            "",
            "allow",
            "allow:",
            "allow:abcdl",
            "allow:ABCDE",
            "allow:abcdef",
            "maybe:abcde",
            "resume:abcde",
            "allow:abcde:x",
        ] {
            assert_eq!(parse_callback(data), None, "{data}");
        }
    }

    #[test]
    fn a_huge_prompt_and_its_decision_stay_within_the_limit() {
        // Astral characters count two UTF-16 units each.
        let huge = "😀".repeat(16 * 1024);
        let text = prompt_text(&request(&huge));
        assert!(text.starts_with("Запрос разрешения: Bash\nrun the tests\n\n"));
        assert!(text.ends_with('…'));
        for behavior in [Behavior::Allow, Behavior::Deny] {
            let decided = decided_text(&text, behavior);
            assert!(
                telegram_len(&decided) <= TELEGRAM_TEXT_LIMIT,
                "{}",
                telegram_len(&decided)
            );
        }
        let short = prompt_text(&request("{\"command\":\"cargo test\"}"));
        assert_eq!(
            short,
            "Запрос разрешения: Bash\nrun the tests\n\n{\"command\":\"cargo test\"}"
        );
    }

    #[test]
    fn duplicates_are_refused_and_the_book_is_bounded() {
        let mut book = Prompts::default();
        let first = book.open(prompt("A", "abcde")).unwrap();
        assert_eq!(
            book.open(prompt("A", "abcde")),
            None,
            "same session, same id"
        );
        let other = book.open(prompt("B", "abcde")).unwrap();
        book.delivered(first, 10);
        book.delivered(other, 11);
        assert_eq!(book.by_message(10), Some(first));
        assert_eq!(book.by_message(11), Some(other));
        // A decided prompt goes first when the book is full.
        book.get_mut(other).unwrap().decided = Some(Behavior::Deny);
        assert!(book.open(prompt("A", "abcde")).is_none());
        for i in 0..MAX_PROMPTS - 2 {
            book.open(prompt("C", &format!("{i:0>5}"))).unwrap();
        }
        assert_eq!(book.len(), MAX_PROMPTS);
        book.open(prompt("D", "abcde")).unwrap();
        assert_eq!(book.len(), MAX_PROMPTS);
        assert_eq!(book.by_message(11), None, "the decided one was forgotten");
        assert_eq!(book.by_message(10), Some(first));
        // Then the oldest undecided one.
        book.open(prompt("E", "abcde")).unwrap();
        assert_eq!(book.by_message(10), None);
        assert_eq!(book.len(), MAX_PROMPTS);
    }

    #[test]
    fn unsent_lists_only_prompts_not_handed_out() {
        let mut book = Prompts::default();
        let a = book.open(prompt("A", "abcde")).unwrap();
        let b = book.open(prompt("B", "abcde")).unwrap();
        book.get_mut(a).unwrap().sent = true;
        assert_eq!(book.unsent(), [b]);
        book.remove(b);
        assert!(book.unsent().is_empty());
        assert_eq!(book.len(), 1);
    }
}
