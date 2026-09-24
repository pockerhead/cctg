//! Permission prompts: the topic message for a relayed `permission_request`,
//! its Allow/Deny buttons and the book of prompts the hub has shown.
//!
//! Pure: the slots actor owns a [`Prompts`] and does all IO. A button carries
//! only the action and the five-letter request id; the prompt is found by the
//! Telegram message the button belongs to, because two sessions can issue the
//! same request id and one message never belongs to two prompts.
//!
//! A prompt is [`State::Open`] until the first press fixes the answer
//! ([`State::Selected`]); it is [`State::Decided`] once the agent took the
//! verdict and [`State::Closed`] when its session ended first. A prompt of a
//! `PermissionRequest` hook ([`Prompt::hook`]) has no agent: the first press
//! decides it at once, and it is [`State::Expired`] when the hook stopped
//! waiting first. An ended prompt
//! owes Telegram one final edit that removes the buttons; a failed edit is
//! tried again on the retry tick.
//!
//! Claude Code never says that a prompt was answered in the terminal, so an
//! open prompt is not proof that the request is still pending. A verdict for
//! a request that is no longer pending is ignored by Claude Code.

use std::collections::{HashMap, VecDeque};
use std::time::Duration;

use serde_json::{Value, json};
use tokio::time::Instant;
use transcript::{TELEGRAM_TEXT_LIMIT, telegram_len};

use super::registry::cut;
use crate::channel::is_request_id;
use crate::wire::{Behavior, PermissionRequest};

/// Prompts remembered at a time. A full book forgets the oldest finished
/// prompt, else expires the oldest open one, else takes no new prompt.
pub const MAX_PROMPTS: usize = 256;
/// A final edit that failed this many times is given up.
pub const MAX_EDIT_ATTEMPTS: u32 = 5;
/// Bot API limit for `callback_data`.
pub const MAX_CALLBACK_DATA: usize = 64;
const ALLOW: &str = "allow";
const DENY: &str = "deny";
pub const ALLOWED_MARK: &str = "\n\n✅ Разрешено из Telegram";
pub const DENIED_MARK: &str = "\n\n⛔ Отклонено из Telegram";
/// The whole text of a prompt whose session ended before an answer.
pub const CLOSED_TEXT: &str = "Сессия завершилась";
pub const ANSWER_ALLOWED: &str = "Разрешено";
pub const ANSWER_DENIED: &str = "Отклонено";
pub const ANSWER_DECIDED: &str = "Уже решено";
/// Also the whole text of a prompt the full book expired.
pub const ANSWER_EXPIRED: &str = "Запрос устарел";
pub const ANSWER_OFFLINE: &str = "Сессия не на связи, ответьте в терминале";

/// A request id for a hook prompt, in the form Claude Code uses (five
/// lowercase letters without `l`), so the buttons parse like any other.
pub fn hook_request_id() -> String {
    const LETTERS: &[u8] = b"abcdefghijkmnopqrstuvwxyz";
    let mut bits = crate::wire::random_u64();
    (0..5)
        .map(|_| {
            let letter = LETTERS[(bits % LETTERS.len() as u64) as usize];
            bits /= LETTERS.len() as u64;
            letter as char
        })
        .collect()
}

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

/// Sent with every final edit: an explicit empty keyboard removes the buttons
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

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum State {
    /// Buttons live, no answer yet.
    Open,
    /// The first press fixed the answer; the verdict waits for an agent of
    /// the session to take it. Later presses cannot change it.
    Selected { behavior: Behavior, verdict_id: u64 },
    /// An agent of the session took the verdict.
    Decided(Behavior),
    /// The session ended before an agent took an answer.
    Closed,
    /// The hook of a hook prompt stopped waiting (timeout, gone) first.
    Expired,
}

impl State {
    pub fn is_active(self) -> bool {
        matches!(self, Self::Open | Self::Selected { .. })
    }
}

/// The final edit of an ended prompt.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Edit {
    /// Not owed (yet): the prompt is active or its message id is unknown.
    None,
    Due,
    InFlight,
    /// Failed; due again on the next retry tick.
    Failed,
    /// Applied or given up.
    Done,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Prompt {
    /// The agent connection that relayed the request.
    pub conn: u64,
    /// Device and claude process of that agent: after a link drop the same
    /// process comes back on another connection.
    pub host: String,
    pub claude_pid: Option<u32>,
    /// The session the agent was bound to when the request came in. Only an
    /// agent bound to this session gets the verdict.
    pub session: String,
    pub request_id: String,
    pub text: String,
    /// Handed to Telegram (the send may still be in flight).
    pub sent: bool,
    /// Set once Telegram accepted the message.
    pub message_id: Option<i64>,
    pub state: State,
    /// Counts for the session's waiting icon. Stop and UserPromptSubmit
    /// clear it: a prompt answered in the terminal is never reported.
    pub waits: bool,
    pub edit: Edit,
    /// Final edits that failed so far.
    pub edit_failures: u32,
    /// Asked by a `PermissionRequest` hook, not relayed by an agent: the
    /// answer goes back to the waiting hook, never to an agent.
    pub hook: bool,
    /// When the hub got the request.
    pub opened: Instant,
}

impl Prompt {
    pub fn new(
        conn: u64,
        host: String,
        claude_pid: Option<u32>,
        session: String,
        request: &PermissionRequest,
    ) -> Self {
        Self {
            conn,
            host,
            claude_pid,
            session,
            request_id: request.request_id.clone(),
            text: prompt_text(request),
            sent: false,
            message_id: None,
            state: State::Open,
            waits: true,
            edit: Edit::None,
            edit_failures: 0,
            hook: false,
            opened: Instant::now(),
        }
    }

    /// What Telegram should show once the prompt ended; `None` while active.
    pub fn final_text(&self) -> Option<String> {
        match self.state {
            State::Decided(behavior) => Some(decided_text(&self.text, behavior)),
            State::Closed => Some(CLOSED_TEXT.to_owned()),
            State::Expired => Some(ANSWER_EXPIRED.to_owned()),
            State::Open | State::Selected { .. } => None,
        }
    }

    /// Ended with nothing left to tell Telegram: safe to forget.
    fn finished(&self) -> bool {
        !self.state.is_active() && self.edit == Edit::Done
    }

    /// Open, and not waiting for a sendMessage answer.
    fn expirable(&self) -> bool {
        self.state == State::Open && (self.message_id.is_some() || !self.sent)
    }
}

#[derive(Debug, PartialEq, Eq)]
pub enum Opened {
    /// `expired`: an open prompt that made room; its message (if any) should
    /// lose its buttons.
    Added { key: u64, expired: Option<Prompt> },
    /// An active prompt of the same session and request id is there.
    Duplicate,
    /// Every prompt is still in use; this one is not shown.
    Full,
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
    pub fn open(&mut self, prompt: Prompt) -> Opened {
        let duplicate = self.prompts.values().any(|known| {
            known.state.is_active()
                && known.session == prompt.session
                && known.request_id == prompt.request_id
        });
        if duplicate {
            return Opened::Duplicate;
        }
        let mut expired = None;
        if self.order.len() >= MAX_PROMPTS {
            let at = |ok: fn(&Prompt) -> bool| {
                self.order
                    .iter()
                    .position(|key| self.prompts.get(key).is_some_and(ok))
            };
            let Some(victim) = at(Prompt::finished).or_else(|| at(Prompt::expirable)) else {
                return Opened::Full;
            };
            if let Some(key) = self.order.get(victim).copied() {
                expired = self.remove(key).filter(|gone| gone.state.is_active());
            }
        }
        let key = self.next;
        self.next += 1;
        self.order.push_back(key);
        self.prompts.insert(key, prompt);
        Opened::Added { key, expired }
    }

    pub fn remove(&mut self, key: u64) -> Option<Prompt> {
        self.order.retain(|known| *known != key);
        let gone = self.prompts.remove(&key)?;
        if let Some(message_id) = gone.message_id
            && self.by_message.get(&message_id) == Some(&key)
        {
            self.by_message.remove(&message_id);
        }
        Some(gone)
    }

    pub fn get(&self, key: u64) -> Option<&Prompt> {
        self.prompts.get(&key)
    }

    pub fn get_mut(&mut self, key: u64) -> Option<&mut Prompt> {
        self.prompts.get_mut(&key)
    }

    /// Telegram accepted the prompt as `message_id`. A prompt that ended
    /// while the send was in flight owes its final edit now.
    pub fn delivered(&mut self, key: u64, message_id: i64) {
        if let Some(prompt) = self.prompts.get_mut(&key) {
            prompt.message_id = Some(message_id);
            if !prompt.state.is_active() {
                prompt.edit = Edit::Due;
            }
            self.by_message.insert(message_id, key);
        }
    }

    /// Ends an active prompt with `state` (`Decided` or `Closed`). One that
    /// never went to Telegram is forgotten. `false`: it was not active.
    pub fn finish(&mut self, key: u64, state: State) -> bool {
        let Some(prompt) = self.prompts.get_mut(&key) else {
            return false;
        };
        if !prompt.state.is_active() {
            return false;
        }
        prompt.state = state;
        if prompt.message_id.is_some() {
            prompt.edit = Edit::Due;
        } else if !prompt.sent {
            self.remove(key);
        }
        true
    }

    pub fn by_message(&self, message_id: i64) -> Option<u64> {
        self.by_message.get(&message_id).copied()
    }

    /// The selected prompt waiting for the ack of `verdict_id`.
    pub fn by_verdict(&self, verdict_id: u64) -> Option<u64> {
        self.prompts.iter().find_map(|(key, prompt)| {
            matches!(prompt.state, State::Selected { verdict_id: id, .. } if id == verdict_id)
                .then_some(*key)
        })
    }

    fn keys(&self, wanted: impl Fn(&Prompt) -> bool) -> Vec<u64> {
        self.order
            .iter()
            .copied()
            .filter(|key| self.prompts.get(key).is_some_and(&wanted))
            .collect()
    }

    /// Active prompts not handed to Telegram yet, oldest first.
    pub fn unsent(&self) -> Vec<u64> {
        self.keys(|prompt| prompt.state.is_active() && !prompt.sent)
    }

    /// Prompts whose answer is fixed but not taken yet, oldest first.
    pub fn selected(&self) -> Vec<u64> {
        self.keys(|prompt| matches!(prompt.state, State::Selected { .. }))
    }

    /// Active prompts, oldest first.
    pub fn active(&self) -> Vec<u64> {
        self.keys(|prompt| prompt.state.is_active())
    }

    /// Prompts whose final edit should go out now.
    pub fn due_edits(&self) -> Vec<u64> {
        self.keys(|prompt| prompt.edit == Edit::Due && prompt.message_id.is_some())
    }

    /// Whether `session` has an active prompt that counts for the icon.
    pub fn waiting(&self, session: &str) -> bool {
        self.prompts
            .values()
            .any(|prompt| prompt.session == session && prompt.waits && prompt.state.is_active())
    }

    /// A turn of `session` ended or began: its prompts stop counting for the
    /// icon. Their buttons stay.
    pub fn quiet(&mut self, session: &str) {
        for prompt in self.prompts.values_mut() {
            if prompt.session == session {
                prompt.waits = false;
            }
        }
    }

    /// A later call of `session` started or ended: its prompts that came in
    /// at least `settle` before `now` were answered in the terminal and stop
    /// counting for the icon. Younger ones still count: the tool hooks run
    /// in the background, so the end of the call before a prompt can come in
    /// after it. Their buttons stay.
    pub fn quiet_settled(&mut self, session: &str, now: Instant, settle: Duration) {
        for prompt in self.prompts.values_mut() {
            if prompt.session == session && prompt.opened + settle <= now {
                prompt.waits = false;
            }
        }
    }

    pub fn edit_done(&mut self, key: u64) {
        if let Some(prompt) = self.prompts.get_mut(&key) {
            prompt.edit = Edit::Done;
        }
    }

    /// Counts a failed final edit; returns the attempts so far. After
    /// [`MAX_EDIT_ATTEMPTS`] the edit is given up.
    pub fn edit_failed(&mut self, key: u64) -> u32 {
        let Some(prompt) = self.prompts.get_mut(&key) else {
            return 0;
        };
        prompt.edit_failures += 1;
        prompt.edit = if prompt.edit_failures >= MAX_EDIT_ATTEMPTS {
            Edit::Done
        } else {
            Edit::Failed
        };
        prompt.edit_failures
    }

    /// The retry tick: failed final edits are due again.
    pub fn retry_failed_edits(&mut self) {
        for prompt in self.prompts.values_mut() {
            if prompt.edit == Edit::Failed {
                prompt.edit = Edit::Due;
            }
        }
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
        let mut request = request("p");
        request.request_id = request_id.into();
        Prompt::new(1, "box".into(), Some(10), session.into(), &request)
    }

    fn added(opened: Opened) -> u64 {
        match opened {
            Opened::Added { key, .. } => key,
            other => panic!("{other:?}"),
        }
    }

    /// Shown in Telegram as `message_id`.
    fn shown(book: &mut Prompts, key: u64, message_id: i64) {
        book.get_mut(key).unwrap().sent = true;
        book.delivered(key, message_id);
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
    fn hook_request_ids_are_valid_request_ids() {
        for _ in 0..1000 {
            let id = hook_request_id();
            assert!(is_request_id(&id), "{id}");
            assert_eq!(
                parse_callback(&callback_data(Behavior::Deny, &id)),
                Some((Behavior::Deny, id.as_str()))
            );
        }
    }

    #[test]
    fn an_expired_prompt_ends_with_the_expired_text() {
        let mut book = Prompts::default();
        let key = added(book.open(prompt("A", "abcde")));
        shown(&mut book, key, 10);
        assert!(book.finish(key, State::Expired));
        assert!(!book.waiting("A"));
        assert_eq!(
            book.get(key).unwrap().final_text().as_deref(),
            Some(ANSWER_EXPIRED)
        );
        assert_eq!(book.due_edits(), [key]);
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
    fn a_huge_prompt_and_its_final_texts_stay_within_the_limit() {
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
        assert!(telegram_len(CLOSED_TEXT) <= TELEGRAM_TEXT_LIMIT);
        let short = prompt_text(&request("{\"command\":\"cargo test\"}"));
        assert_eq!(
            short,
            "Запрос разрешения: Bash\nrun the tests\n\n{\"command\":\"cargo test\"}"
        );
    }

    #[test]
    fn duplicates_are_refused_per_session_and_the_same_id_lives_in_two() {
        let mut book = Prompts::default();
        let first = added(book.open(prompt("A", "abcde")));
        assert_eq!(book.open(prompt("A", "abcde")), Opened::Duplicate);
        let other = added(book.open(prompt("B", "abcde")));
        shown(&mut book, first, 10);
        shown(&mut book, other, 11);
        assert_eq!(book.by_message(10), Some(first));
        assert_eq!(book.by_message(11), Some(other));
        // Once A's prompt ended, the same id may ask again.
        assert!(book.finish(first, State::Closed));
        added(book.open(prompt("A", "abcde")));
    }

    #[test]
    fn the_answer_is_fixed_once_and_ends_in_one_final_edit() {
        let mut book = Prompts::default();
        let key = added(book.open(prompt("A", "abcde")));
        shown(&mut book, key, 10);
        assert_eq!(book.get(key).unwrap().final_text(), None);
        book.get_mut(key).unwrap().state = State::Selected {
            behavior: Behavior::Deny,
            verdict_id: 7,
        };
        assert_eq!(book.by_verdict(7), Some(key));
        assert_eq!(book.selected(), [key]);
        assert!(
            book.due_edits().is_empty(),
            "no edit before the agent took it"
        );
        assert!(book.finish(key, State::Decided(Behavior::Deny)));
        assert!(
            !book.finish(key, State::Closed),
            "a decided prompt stays decided"
        );
        assert_eq!(book.by_verdict(7), None);
        let prompt = book.get(key).unwrap();
        assert_eq!(
            prompt.final_text(),
            Some(decided_text(&prompt.text, Behavior::Deny))
        );
        assert_eq!(book.due_edits(), [key]);
    }

    #[test]
    fn a_prompt_that_ends_before_its_message_id_is_edited_once_the_id_comes() {
        let mut book = Prompts::default();
        let in_flight = added(book.open(prompt("A", "abcde")));
        book.get_mut(in_flight).unwrap().sent = true;
        let unsent = added(book.open(prompt("A", "bcdef")));
        assert!(book.finish(in_flight, State::Closed));
        assert!(book.finish(unsent, State::Closed));
        assert!(book.get(unsent).is_none(), "never shown: forgotten");
        assert!(book.due_edits().is_empty());
        book.delivered(in_flight, 10);
        assert_eq!(book.due_edits(), [in_flight]);
        assert_eq!(
            book.get(in_flight).unwrap().final_text().as_deref(),
            Some(CLOSED_TEXT)
        );
    }

    #[test]
    fn a_failed_final_edit_is_due_again_until_it_is_given_up() {
        let mut book = Prompts::default();
        let key = added(book.open(prompt("A", "abcde")));
        shown(&mut book, key, 10);
        book.finish(key, State::Closed);
        for attempt in 1..MAX_EDIT_ATTEMPTS {
            book.get_mut(key).unwrap().edit = Edit::InFlight;
            assert_eq!(book.edit_failed(key), attempt);
            assert!(book.due_edits().is_empty(), "not before the tick");
            book.retry_failed_edits();
            assert_eq!(book.due_edits(), [key]);
        }
        assert_eq!(book.edit_failed(key), MAX_EDIT_ATTEMPTS);
        book.retry_failed_edits();
        assert!(book.due_edits().is_empty(), "given up");
        assert!(book.get(key).unwrap().finished());
    }

    #[test]
    fn waiting_counts_active_prompts_until_the_turn_ends() {
        let mut book = Prompts::default();
        let first = added(book.open(prompt("A", "abcde")));
        let second = added(book.open(prompt("A", "bcdef")));
        shown(&mut book, first, 10);
        shown(&mut book, second, 11);
        assert!(book.waiting("A") && !book.waiting("B"));
        book.finish(first, State::Decided(Behavior::Allow));
        assert!(book.waiting("A"), "the second one is still open");
        book.quiet("A");
        assert!(!book.waiting("A"), "Stop: answered in the terminal, maybe");
        let third = added(book.open(prompt("A", "cdefg")));
        assert!(book.waiting("A"));
        book.finish(third, State::Closed);
        assert!(!book.waiting("A"));
        assert_eq!(book.active(), [second]);
    }

    #[test]
    fn a_later_call_quiets_only_prompts_older_than_the_settle_time() {
        let mut book = Prompts::default();
        let old = added(book.open(prompt("A", "abcde")));
        let young = added(book.open(prompt("A", "bcdef")));
        let settle = Duration::from_secs(2);
        let opened = book.get(old).unwrap().opened;
        book.get_mut(young).unwrap().opened = opened + Duration::from_secs(1);
        book.quiet_settled("A", opened + Duration::from_millis(1500), settle);
        assert!(book.waiting("A"), "neither has settled");
        book.quiet_settled("A", opened + settle, settle);
        assert!(!book.get(old).unwrap().waits);
        assert!(
            book.get(young).unwrap().waits,
            "the younger one still waits"
        );
        assert!(book.waiting("A"));
        book.quiet_settled("B", opened + Duration::from_secs(9), settle);
        assert!(book.waiting("A"), "another session's call changes nothing");
        assert_eq!(book.active(), [old, young], "the buttons stay");
    }

    #[test]
    fn a_full_book_forgets_finished_then_expires_open_never_selected() {
        let mut book = Prompts::default();
        let id = |n: usize| {
            const LETTERS: &[u8] = b"abcdefghijkmnopqrstuvwxyz";
            (0..5)
                .map(|place| LETTERS[(n / LETTERS.len().pow(place)) % LETTERS.len()] as char)
                .collect::<String>()
        };
        let keys: Vec<u64> = (0..MAX_PROMPTS)
            .map(|n| added(book.open(prompt("A", &id(n)))))
            .collect();
        for (n, key) in keys.iter().enumerate() {
            shown(&mut book, *key, 100 + n as i64);
        }
        // The first one is finished: it goes silently.
        book.finish(keys[0], State::Decided(Behavior::Allow));
        book.edit_done(keys[0]);
        let Opened::Added { expired: None, .. } = book.open(prompt("B", "abcde")) else {
            panic!("a finished prompt makes room");
        };
        assert_eq!(book.by_message(100), None);
        // Then the oldest open one expires; a selected one never does.
        book.get_mut(keys[1]).unwrap().state = State::Selected {
            behavior: Behavior::Deny,
            verdict_id: 1,
        };
        let Opened::Added {
            expired: Some(gone),
            ..
        } = book.open(prompt("B", "bcdef"))
        else {
            panic!("an open prompt makes room");
        };
        assert_eq!(gone.message_id, Some(102));
        assert_eq!(book.by_message(102), None);
        assert_eq!(book.by_message(101), Some(keys[1]));
        assert_eq!(book.len(), MAX_PROMPTS);
        // Nothing expirable left: every other prompt is selected.
        for key in book.active() {
            book.get_mut(key).unwrap().state = State::Selected {
                behavior: Behavior::Allow,
                verdict_id: key,
            };
        }
        assert_eq!(book.open(prompt("C", "abcde")), Opened::Full);
        assert_eq!(book.len(), MAX_PROMPTS);
    }

    #[test]
    fn unsent_lists_only_active_prompts_not_handed_out() {
        let mut book = Prompts::default();
        let a = added(book.open(prompt("A", "abcde")));
        let b = added(book.open(prompt("B", "abcde")));
        book.get_mut(a).unwrap().sent = true;
        assert_eq!(book.unsent(), [b]);
        book.remove(b);
        assert!(book.unsent().is_empty());
        assert_eq!(book.len(), 1);
    }
}
