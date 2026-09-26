//! Questions of Claude's `AskUserQuestion` tool (TASK-038): the topic
//! message, its buttons and the book of questions the hub shows.
//!
//! Pure: the slots actor owns an [`Asks`] and does all IO. One tool call (1
//! to 4 questions) is one message. It shows the current question with a
//! button per option, ✏️ Другое (the next text in the topic, or a reply to
//! the message, is the answer) and ⌨ В терминале (the hook lets go and the
//! terminal dialog opens). A multiSelect question ticks its options and is
//! answered with ✅ Готово. Each answer moves the message on to the next
//! question; after the last one it shows the answers without buttons and
//! they go to the waiting hook.
//!
//! A button carries only `ask:<id>:<question>:<action>` (at most 16 bytes)
//! and is found by the Telegram message it belongs to, like a permission
//! button. A press on an older question or an ended ask changes nothing.

use std::collections::{BTreeMap, BTreeSet};

use serde_json::{Value, json};
use transcript::TELEGRAM_TEXT_LIMIT;

use super::permissions::no_keyboard;
use super::registry::cut;
use crate::channel::is_request_id;
use crate::wire::{Answered, AskedQuestion};

/// Asks kept at a time, open or owing their last edit; one more is refused
/// and its hook gets no decision.
pub const MAX_ASKS: usize = 16;
/// An edit that failed this many times is given up.
pub const MAX_EDIT_ATTEMPTS: u32 = 5;
const PREFIX: &str = "ask";
/// UTF-16 units of a label on its button; the message text has it whole.
const BUTTON_LABEL: usize = 40;
/// UTF-16 units of a question and of an option in the message text.
const QUESTION_TEXT: usize = 1500;
const OPTION_TEXT: usize = 300;
/// Bytes of the user's own answer kept (more than one Telegram message).
pub const MAX_OWN_TEXT: usize = 16 << 10;

pub const OTHER_BUTTON: &str = "✏️ Другое";
pub const TERMINAL_BUTTON: &str = "⌨ В терминале";
pub const DONE_BUTTON: &str = "✅ Готово";
pub const ANSWER_TAKEN: &str = "Ответ принят";
pub const ANSWER_NEXT: &str = "Принято, дальше следующий вопрос";
pub const ANSWER_PICK_ONE: &str = "Отметьте хотя бы один вариант или нажмите ✏️ Другое";
pub const ANSWER_TYPE: &str = "Напишите ответ следующим сообщением в этой теме";
pub const ANSWER_TERMINAL: &str = "Ответьте в терминале";
pub const ANSWER_STALE: &str = "Вопрос уже закрыт";
const MULTI_HINT: &str = "Отметьте варианты и нажмите ✅ Готово.\n";
const OTHER_HINT: &str = "Свой ответ: ✏️ Другое или ответ (reply) на это сообщение.";
const TYPING_HINT: &str = "✏️ Жду ваш ответ следующим сообщением в этой теме.";
pub const ANSWERED_TITLE: &str = "✅ Ответ отправлен в Claude";
pub const TERMINAL_TITLE: &str = "⌨ Вопрос ушёл в терминал";
pub const EXPIRED_TITLE: &str = "⌛ Ответа не было, вопрос ушёл в терминал";
pub const GONE_TITLE: &str = "Вопрос закрыт в терминале";
pub const CLOSED_TITLE: &str = "Сессия завершилась";
/// Sent once per session when a question comes only as a `PermissionRequest`
/// hook: its client has no `PreToolUse` hook for `AskUserQuestion`.
pub const NO_HOOK_NOTICE: &str = "❓ Claude задал вопрос, он ждёт ответа в терминале. \
Чтобы отвечать на такие вопросы из Telegram, клиенту этой машины нужен хук PreToolUse \
для AskUserQuestion: запустите install.sh этого релиза ещё раз (или добавьте хук по \
docs/poc.md) и перезапустите сессию.";

/// A button of the current question.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Press {
    /// The option with this index: the answer, or a tick of a multiSelect
    /// question.
    Option(usize),
    /// ✏️ Другое: the next text in the topic is the answer.
    Other,
    /// ✅ Готово of a multiSelect question.
    Done,
    /// ⌨ В терминале.
    Terminal,
}

pub fn callback_data(id: &str, question: usize, press: Press) -> String {
    let action = match press {
        Press::Option(index) => format!("o{index}"),
        Press::Other => "x".to_owned(),
        Press::Done => "d".to_owned(),
        Press::Terminal => "t".to_owned(),
    };
    format!("{PREFIX}:{id}:{question}:{action}")
}

/// `ask:<id>:<question>:<action>` with a valid id; anything else is not a
/// question button.
pub fn parse_callback(data: &str) -> Option<(&str, usize, Press)> {
    let mut parts = data.split(':');
    let (prefix, id, question, action) =
        (parts.next()?, parts.next()?, parts.next()?, parts.next()?);
    if prefix != PREFIX || parts.next().is_some() || !is_request_id(id) {
        return None;
    }
    let press = match action {
        "x" => Press::Other,
        "d" => Press::Done,
        "t" => Press::Terminal,
        _ => Press::Option(index(action.strip_prefix('o')?)?),
    };
    Some((id, index(question)?, press))
}

/// One or two decimal digits without a leading zero.
fn index(text: &str) -> Option<usize> {
    let digits = !text.is_empty() && text.len() <= 2 && text.bytes().all(|b| b.is_ascii_digit());
    if !digits || (text.len() > 1 && text.starts_with('0')) {
        return None;
    }
    text.parse().ok()
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum State {
    /// Waiting for answers.
    Open,
    /// Every question answered: the answers go to the hook.
    Answered,
    /// ⌨ В терминале: the hook lets go, the terminal dialog opens.
    Terminal,
    /// Nobody answered in time: the same, by the clock (or Telegram did
    /// not take the message).
    Expired,
    /// The hook stopped waiting first (Claude Code went on without it).
    Gone,
    /// The session ended.
    Closed,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Ask {
    pub session: String,
    /// Five letters like a permission request id; buttons carry it.
    pub id: String,
    pub questions: Vec<AskedQuestion>,
    /// The question shown now; `questions.len()` once all are answered.
    pub step: usize,
    pub answers: Vec<Answered>,
    /// Options of the current multiSelect question ticked so far.
    pub picked: BTreeSet<usize>,
    /// ✏️ Другое was pressed: the next text in the topic is the answer.
    pub typing: bool,
    pub state: State,
    /// The topic it went to; `None` until it is handed to Telegram.
    pub thread_id: Option<i64>,
    /// Handed to Telegram, no answer yet.
    pub sending: bool,
    pub message_id: Option<i64>,
    /// Bumped by every change of what the message should show.
    pub version: u64,
    /// The version Telegram shows.
    pub shown: u64,
    /// An edit is in flight.
    pub editing: bool,
    /// A failed edit waits for the retry tick.
    pub retry: bool,
    pub edit_failures: u32,
}

impl Ask {
    pub fn new(session: String, id: String, questions: Vec<AskedQuestion>) -> Self {
        Self {
            session,
            id,
            questions,
            step: 0,
            answers: Vec::new(),
            picked: BTreeSet::new(),
            typing: false,
            state: State::Open,
            thread_id: None,
            sending: false,
            message_id: None,
            version: 1,
            shown: 0,
            editing: false,
            retry: false,
            edit_failures: 0,
        }
    }

    pub fn is_open(&self) -> bool {
        self.state == State::Open
    }

    /// A button of question `question`. The answer for the button press;
    /// `None` for a quiet tick.
    pub fn press(&mut self, question: usize, press: Press) -> Option<&'static str> {
        if !self.is_open() || question != self.step {
            return Some(ANSWER_STALE);
        }
        let current = self.questions.get(self.step)?;
        match press {
            Press::Option(index) if index < current.options.len() => {
                if current.multi_select {
                    if !self.picked.remove(&index) {
                        self.picked.insert(index);
                    }
                    self.version += 1;
                    None
                } else {
                    Some(self.advance(Answered {
                        options: vec![index],
                        text: None,
                    }))
                }
            }
            Press::Done if current.multi_select => {
                if self.picked.is_empty() {
                    return Some(ANSWER_PICK_ONE);
                }
                let options = self.picked.iter().copied().collect();
                Some(self.advance(Answered {
                    options,
                    text: None,
                }))
            }
            Press::Other => {
                if !self.typing {
                    self.typing = true;
                    self.version += 1;
                }
                Some(ANSWER_TYPE)
            }
            Press::Terminal => {
                self.end(State::Terminal);
                Some(ANSWER_TERMINAL)
            }
            Press::Option(_) | Press::Done => Some(ANSWER_STALE),
        }
    }

    /// The user's own text for the current question, as typed (at most
    /// [`MAX_OWN_TEXT`] bytes). A multiSelect question keeps its ticked
    /// options in front of it. `false`: nothing taken (ended, or a blank
    /// text).
    pub fn type_answer(&mut self, text: &str) -> bool {
        if !self.is_open() || text.trim().is_empty() || self.step >= self.questions.len() {
            return false;
        }
        let mut end = text.len().min(MAX_OWN_TEXT);
        while !text.is_char_boundary(end) {
            end -= 1;
        }
        let options = self.picked.iter().copied().collect();
        self.advance(Answered {
            options,
            text: Some(text[..end].to_owned()),
        });
        true
    }

    /// How the message shows the answer to question `index`.
    fn shown_answer(&self, index: usize) -> String {
        let (Some(question), Some(answer)) = (self.questions.get(index), self.answers.get(index))
        else {
            return String::new();
        };
        let mut parts: Vec<&str> = answer
            .options
            .iter()
            .filter_map(|option| question.options.get(*option))
            .map(|option| option.label.trim())
            .collect();
        parts.extend(answer.text.as_deref().map(str::trim));
        parts.join(", ")
    }

    fn advance(&mut self, answer: Answered) -> &'static str {
        self.answers.push(answer);
        self.step += 1;
        self.picked.clear();
        self.typing = false;
        self.version += 1;
        if self.step >= self.questions.len() {
            self.state = State::Answered;
            ANSWER_TAKEN
        } else {
            ANSWER_NEXT
        }
    }

    /// Ends an open ask with `state`. `false`: it had ended.
    pub fn end(&mut self, state: State) -> bool {
        if !self.is_open() || state == State::Open {
            return false;
        }
        self.state = state;
        self.typing = false;
        self.version += 1;
        true
    }

    /// The hook left before it could take the answers: the message says the
    /// question closed in the terminal instead of "sent".
    pub fn answers_not_taken(&mut self) {
        if self.state == State::Answered {
            self.state = State::Gone;
            self.version += 1;
        }
    }

    /// The answers for the hook, once every question has one.
    pub fn answers(&self) -> Option<Vec<Answered>> {
        (self.state == State::Answered).then(|| self.answers.clone())
    }

    /// What the message shows now, plain text within the Telegram limit.
    pub fn text(&self) -> String {
        let mut text = String::new();
        let title = match self.state {
            State::Open => return cut(&self.open_text(), TELEGRAM_TEXT_LIMIT),
            State::Answered => ANSWERED_TITLE,
            State::Terminal => TERMINAL_TITLE,
            State::Expired => EXPIRED_TITLE,
            State::Gone => GONE_TITLE,
            State::Closed => CLOSED_TITLE,
        };
        text.push_str(title);
        for (index, question) in self.questions.iter().enumerate() {
            text.push_str("\n\n");
            text.push_str(&cut(question.question.trim(), QUESTION_TEXT));
            if self.state == State::Answered && index < self.answers.len() {
                text.push_str("\n→ ");
                text.push_str(&self.shown_answer(index));
            }
        }
        cut(&text, TELEGRAM_TEXT_LIMIT)
    }

    fn open_text(&self) -> String {
        let total = self.questions.len();
        let mut text = String::from("❓ Вопрос от Claude");
        if total > 1 {
            text.push_str(&format!(" ({} из {total})", self.step + 1));
        }
        for (index, question) in self.questions.iter().enumerate().take(self.answers.len()) {
            let name = if question.header.trim().is_empty() {
                cut(question.question.trim(), BUTTON_LABEL)
            } else {
                question.header.trim().to_owned()
            };
            let answer = self.shown_answer(index);
            text.push_str(&format!("\n✓ {name}: {}", cut(&answer, OPTION_TEXT)));
        }
        let Some(current) = self.questions.get(self.step) else {
            return text;
        };
        text.push_str("\n\n");
        if !current.header.trim().is_empty() {
            text.push_str(current.header.trim());
            text.push('\n');
        }
        text.push_str(&cut(current.question.trim(), QUESTION_TEXT));
        text.push('\n');
        for (index, option) in current.options.iter().enumerate() {
            let mut line = format!(
                "\n{}{}. {}",
                self.tick(current, index),
                index + 1,
                option.label.trim()
            );
            if !option.description.trim().is_empty() {
                line.push_str(" — ");
                line.push_str(option.description.trim());
            }
            text.push_str(&cut(&line, OPTION_TEXT));
        }
        text.push_str("\n\n");
        if current.multi_select {
            text.push_str(MULTI_HINT);
        }
        text.push_str(if self.typing { TYPING_HINT } else { OTHER_HINT });
        text
    }

    fn tick(&self, question: &AskedQuestion, index: usize) -> &'static str {
        match (question.multi_select, self.picked.contains(&index)) {
            (false, _) => "",
            (true, true) => "☑ ",
            (true, false) => "☐ ",
        }
    }

    /// The buttons of the current question; none once the ask ended.
    pub fn keyboard(&self) -> Value {
        let Some(current) = self.questions.get(self.step).filter(|_| self.is_open()) else {
            return no_keyboard();
        };
        let button = |text: String, press: Press| json!({ "text": text, "callback_data": callback_data(&self.id, self.step, press) });
        let mut rows: Vec<Value> = current
            .options
            .iter()
            .enumerate()
            .map(|(index, option)| {
                let text = format!(
                    "{}{}. {}",
                    self.tick(current, index),
                    index + 1,
                    cut(option.label.trim(), BUTTON_LABEL)
                );
                json!([button(text, Press::Option(index))])
            })
            .collect();
        let mut last = Vec::new();
        if current.multi_select {
            last.push(button(DONE_BUTTON.to_owned(), Press::Done));
        }
        last.push(button(OTHER_BUTTON.to_owned(), Press::Other));
        last.push(button(TERMINAL_BUTTON.to_owned(), Press::Terminal));
        rows.push(Value::Array(last));
        json!({ "inline_keyboard": rows })
    }

    /// Telegram shows an older version and nothing is in flight or waits
    /// for the retry tick.
    pub fn edit_due(&self) -> bool {
        self.message_id.is_some() && self.version > self.shown && !self.editing && !self.retry
    }

    /// Ended, and nothing left to tell Telegram: safe to forget.
    pub fn finished(&self) -> bool {
        !self.is_open()
            && !self.sending
            && !self.editing
            && (self.message_id.is_none() || self.shown >= self.version)
    }
}

/// The book of asks, at most [`MAX_ASKS`].
#[derive(Debug, Default)]
pub struct Asks {
    next: u64,
    asks: BTreeMap<u64, Ask>,
}

impl Asks {
    /// Keeps `ask`; `None` when the book is full.
    pub fn open(&mut self, ask: Ask) -> Option<u64> {
        if self.asks.len() >= MAX_ASKS {
            return None;
        }
        let key = self.next;
        self.next += 1;
        self.asks.insert(key, ask);
        Some(key)
    }

    pub fn get(&self, key: u64) -> Option<&Ask> {
        self.asks.get(&key)
    }

    pub fn get_mut(&mut self, key: u64) -> Option<&mut Ask> {
        self.asks.get_mut(&key)
    }

    pub fn remove(&mut self, key: u64) -> Option<Ask> {
        self.asks.remove(&key)
    }

    /// Every key, oldest first.
    pub fn keys(&self) -> Vec<u64> {
        self.asks.keys().copied().collect()
    }

    pub fn by_message(&self, message_id: i64) -> Option<u64> {
        self.asks
            .iter()
            .find(|(_, ask)| ask.message_id == Some(message_id))
            .map(|(key, _)| *key)
    }

    /// The open ask of topic `thread_id` a text message answers: the one it
    /// replies to; a plain message (no reply) answers the newest one waiting
    /// for typed text. A reply to any other message answers nothing.
    pub fn text_target(&self, thread_id: i64, reply_to: Option<i64>) -> Option<u64> {
        let open = |ask: &Ask| ask.is_open() && ask.thread_id == Some(thread_id);
        let found = match reply_to {
            Some(reply_to) => self
                .asks
                .iter()
                .find(|(_, ask)| open(ask) && ask.message_id == Some(reply_to)),
            None => self
                .asks
                .iter()
                .rev()
                .find(|(_, ask)| open(ask) && ask.typing),
        };
        found.map(|(key, _)| *key)
    }

    /// `session` has an open ask.
    pub fn waiting(&self, session: &str) -> bool {
        self.asks
            .values()
            .any(|ask| ask.is_open() && ask.session == session)
    }

    /// An open ask of `session` has the id `id`.
    pub fn id_taken(&self, session: &str, id: &str) -> bool {
        self.asks
            .values()
            .any(|ask| ask.is_open() && ask.session == session && ask.id == id)
    }

    /// The retry tick: failed edits are due again.
    pub fn retry_edits(&mut self) {
        for ask in self.asks.values_mut() {
            ask.retry = false;
        }
    }

    pub fn len(&self) -> usize {
        self.asks.len()
    }

    pub fn is_empty(&self) -> bool {
        self.asks.is_empty()
    }
}

#[cfg(test)]
mod tests {
    use transcript::telegram_len;

    use super::*;
    use crate::hub::permissions::MAX_CALLBACK_DATA;
    use crate::wire::AskedOption;

    fn question(text: &str, multi_select: bool, labels: &[&str]) -> AskedQuestion {
        AskedQuestion {
            question: text.into(),
            header: String::new(),
            multi_select,
            options: labels
                .iter()
                .map(|label| AskedOption {
                    label: (*label).into(),
                    description: String::new(),
                })
                .collect(),
        }
    }

    fn ask(questions: Vec<AskedQuestion>) -> Ask {
        Ask::new("s".into(), "abcde".into(), questions)
    }

    fn answered(options: &[usize], text: Option<&str>) -> Answered {
        Answered {
            options: options.to_vec(),
            text: text.map(str::to_owned),
        }
    }

    fn data(keyboard: &Value) -> Vec<Vec<String>> {
        keyboard["inline_keyboard"]
            .as_array()
            .unwrap()
            .iter()
            .map(|row| {
                row.as_array()
                    .unwrap()
                    .iter()
                    .map(|button| button["callback_data"].as_str().unwrap().to_owned())
                    .collect()
            })
            .collect()
    }

    #[test]
    fn buttons_round_trip_and_fit_callback_data() {
        for press in [
            Press::Option(0),
            Press::Option(7),
            Press::Option(99),
            Press::Other,
            Press::Done,
            Press::Terminal,
        ] {
            let data = callback_data("zzzzz", 3, press);
            assert!(data.len() <= MAX_CALLBACK_DATA);
            assert_eq!(parse_callback(&data), Some(("zzzzz", 3, press)));
        }
        for bad in [
            "ask:abcde:0",
            "ask:abcde:0:o",
            "ask:abcde:0:o100",
            "ask:abcde:00:x",
            "ask:abcde:01:x",
            "ask:abcde:-1:x",
            "ask:abcde:0:q",
            "ask:abcdl:0:x",
            "ask:abcde:0:x:more",
            "asks:abcde:0:x",
            "allow:abcde",
            "status:stop",
            "",
        ] {
            assert_eq!(parse_callback(bad), None, "{bad}");
        }
    }

    #[test]
    fn a_single_choice_answers_and_moves_on() {
        let mut ask = ask(vec![
            question("Color?", false, &["Red", "Blue"]),
            question("Size?", false, &["S", "M"]),
        ]);
        let keyboard = data(&ask.keyboard());
        assert_eq!(keyboard[0], ["ask:abcde:0:o0"]);
        assert_eq!(keyboard[1], ["ask:abcde:0:o1"]);
        assert_eq!(keyboard[2], ["ask:abcde:0:x", "ask:abcde:0:t"]);
        assert!(ask.text().contains("(1 из 2)") && ask.text().contains("2. Blue"));
        let before = ask.version;
        assert_eq!(ask.press(0, Press::Option(1)), Some(ANSWER_NEXT));
        assert!(ask.version > before);
        assert!(ask.text().contains("(2 из 2)") && ask.text().contains("Blue"));
        // A button of the first question no longer counts.
        assert_eq!(ask.press(0, Press::Option(0)), Some(ANSWER_STALE));
        assert_eq!(ask.press(1, Press::Option(5)), Some(ANSWER_STALE));
        assert_eq!(ask.press(1, Press::Done), Some(ANSWER_STALE));
        assert_eq!(ask.answers(), None);
        assert_eq!(ask.press(1, Press::Option(0)), Some(ANSWER_TAKEN));
        assert_eq!(ask.state, State::Answered);
        assert_eq!(
            ask.answers(),
            Some(vec![answered(&[1], None), answered(&[0], None)])
        );
        assert_eq!(data(&ask.keyboard()), Vec::<Vec<String>>::new());
        assert!(ask.text().starts_with(ANSWERED_TITLE));
        assert!(ask.text().contains("Color?\n→ Blue") && ask.text().contains("Size?\n→ S"));
        assert_eq!(ask.press(1, Press::Option(0)), Some(ANSWER_STALE));
        // The hook left before it took them: the message must not say "sent".
        let before = ask.version;
        ask.answers_not_taken();
        assert!(ask.version > before && ask.text().starts_with(GONE_TITLE));
        assert_eq!(ask.answers(), None);
    }

    #[test]
    fn a_multi_select_ticks_and_needs_one() {
        let mut ask = ask(vec![question("Fruits?", true, &["Apple", "Pear", "Plum"])]);
        assert_eq!(
            data(&ask.keyboard()).last().unwrap(),
            &["ask:abcde:0:d", "ask:abcde:0:x", "ask:abcde:0:t"]
        );
        assert_eq!(ask.press(0, Press::Done), Some(ANSWER_PICK_ONE));
        assert_eq!(ask.press(0, Press::Option(2)), None);
        assert_eq!(ask.press(0, Press::Option(0)), None);
        assert_eq!(ask.press(0, Press::Option(1)), None);
        assert_eq!(ask.press(0, Press::Option(1)), None);
        assert!(ask.text().contains("☑ 1. Apple") && ask.text().contains("☐ 2. Pear"));
        assert_eq!(ask.keyboard()["inline_keyboard"][2][0]["text"], "☑ 3. Plum");
        assert_eq!(ask.press(0, Press::Done), Some(ANSWER_TAKEN));
        assert_eq!(ask.answers(), Some(vec![answered(&[0, 2], None)]));
        assert!(ask.text().contains("Fruits?\n→ Apple, Plum"));
    }

    #[test]
    fn own_text_answers_the_current_question() {
        let mut ask = ask(vec![
            question("Fruits?", true, &["Apple", "Pear"]),
            question("Note?", false, &["Yes", "No"]),
        ]);
        assert!(!ask.typing);
        assert_eq!(ask.press(0, Press::Other), Some(ANSWER_TYPE));
        assert!(ask.typing && ask.text().contains(TYPING_HINT));
        assert_eq!(ask.press(0, Press::Option(1)), None);
        assert!(!ask.type_answer("   "));
        assert!(ask.type_answer("  and a fig "));
        assert!(!ask.typing);
        assert!(ask.type_answer("my own words"));
        // The text goes as typed; the message shows it trimmed.
        assert_eq!(
            ask.answers(),
            Some(vec![
                answered(&[1], Some("  and a fig ")),
                answered(&[], Some("my own words"))
            ])
        );
        assert!(ask.text().contains("Fruits?\n→ Pear, and a fig\n"));
        assert!(!ask.type_answer("late"));
        // A long text is cut at a character boundary.
        let mut long = self::ask(vec![question("Note?", false, &["Yes"])]);
        assert!(long.type_answer(&"й".repeat(MAX_OWN_TEXT)));
        let text = long.answers().unwrap().remove(0).text.unwrap();
        assert_eq!(text, "й".repeat(MAX_OWN_TEXT / 2));
    }

    #[test]
    fn the_terminal_button_and_the_end_close_the_ask() {
        let mut ask = ask(vec![question("Color?", false, &["Red"])]);
        assert_eq!(ask.press(0, Press::Terminal), Some(ANSWER_TERMINAL));
        assert_eq!(ask.state, State::Terminal);
        assert_eq!(ask.answers(), None);
        assert!(ask.text().starts_with(TERMINAL_TITLE));
        assert!(!ask.end(State::Expired));
        let mut other = self::ask(vec![question("Color?", false, &["Red"])]);
        assert!(other.end(State::Expired));
        assert!(other.text().starts_with(EXPIRED_TITLE) && other.text().contains("Color?"));
        assert!(!other.end(State::Closed));
    }

    #[test]
    fn texts_stay_within_the_telegram_limit() {
        let label = "й".repeat(200);
        let labels: Vec<&str> = vec![label.as_str(); 8];
        let mut long = question(&"вопрос ".repeat(2000), true, &labels);
        long.header = "Header".into();
        for option in &mut long.options {
            option.description = "д".repeat(1000);
        }
        let mut ask = ask(vec![long.clone(), long.clone(), long.clone(), long]);
        for step in 0..4 {
            assert!(telegram_len(&ask.text()) <= TELEGRAM_TEXT_LIMIT, "{step}");
            for row in ask.keyboard()["inline_keyboard"].as_array().unwrap() {
                for button in row.as_array().unwrap() {
                    assert!(telegram_len(button["text"].as_str().unwrap()) <= BUTTON_LABEL + 8);
                }
            }
            assert!(ask.type_answer(&"ответ ".repeat(700)));
        }
        assert!(telegram_len(&ask.text()) <= TELEGRAM_TEXT_LIMIT);
    }

    #[test]
    fn edits_are_due_for_a_newer_version_only() {
        let mut ask = ask(vec![question("Color?", false, &["Red"])]);
        assert!(!ask.edit_due() && !ask.finished());
        ask.message_id = Some(7);
        ask.shown = ask.version;
        assert!(!ask.edit_due());
        ask.press(0, Press::Other);
        assert!(ask.edit_due());
        ask.retry = true;
        assert!(!ask.edit_due());
        ask.retry = false;
        ask.editing = true;
        assert!(!ask.edit_due());
        ask.editing = false;
        ask.end(State::Gone);
        assert!(!ask.finished());
        ask.shown = ask.version;
        assert!(ask.finished());
        let mut unsent = self::ask(vec![question("Color?", false, &["Red"])]);
        unsent.end(State::Closed);
        assert!(unsent.finished());
        unsent.sending = true;
        assert!(!unsent.finished());
    }

    #[test]
    fn the_book_finds_asks_by_message_and_text() {
        let mut book = Asks::default();
        let mut first = ask(vec![question("A?", false, &["x"])]);
        first.thread_id = Some(100);
        first.message_id = Some(5);
        let mut second = ask(vec![question("B?", false, &["y"])]);
        second.id = "bcdef".into();
        second.thread_id = Some(100);
        second.message_id = Some(6);
        let first = book.open(first).unwrap();
        let second = book.open(second).unwrap();
        assert_eq!(book.by_message(6), Some(second));
        assert_eq!(book.text_target(100, Some(5)), Some(first));
        assert_eq!(book.text_target(100, None), None);
        assert_eq!(book.text_target(100, Some(9)), None);
        book.get_mut(first).unwrap().press(0, Press::Other);
        assert_eq!(book.text_target(100, None), Some(first));
        assert_eq!(book.text_target(101, None), None);
        assert_eq!(book.text_target(100, Some(6)), Some(second));
        assert!(book.waiting("s") && book.id_taken("s", "abcde") && !book.id_taken("t", "abcde"));
        book.get_mut(first).unwrap().end(State::Gone);
        assert_eq!(book.text_target(100, Some(5)), None);
        // With ✏️ Другое armed, a reply to any other message is no answer.
        book.get_mut(second).unwrap().press(0, Press::Other);
        assert_eq!(book.text_target(100, None), Some(second));
        assert_eq!(book.text_target(100, Some(6)), Some(second));
        assert_eq!(book.text_target(100, Some(777)), None);
        for _ in book.len()..MAX_ASKS {
            assert!(
                book.open(ask(vec![question("C?", false, &["z"])]))
                    .is_some()
            );
        }
        assert_eq!(book.open(ask(vec![question("D?", false, &["z"])])), None);
    }

    /// The review's repro: ✏️ Другое armed, then a reply to some other bot
    /// message (a subagent block, an older answer) goes on to the session;
    /// a padded label is answered by its index, not by trimmed text.
    #[test]
    fn a_reply_to_another_message_is_no_answer_while_typing() {
        let q = question("Q?", false, &["  Padded label  "]);
        let mut typing = ask(vec![q.clone()]);
        typing.thread_id = Some(100);
        typing.message_id = Some(5);
        typing.press(0, Press::Other);
        let mut book = Asks::default();
        let key = book.open(typing).unwrap();
        assert_eq!(book.text_target(100, Some(777)), None);
        assert_eq!(book.text_target(100, Some(5)), Some(key));
        assert_eq!(book.text_target(100, None), Some(key));
        let mut plain = ask(vec![q]);
        plain.press(0, Press::Option(0));
        assert_eq!(plain.answers(), Some(vec![answered(&[0], None)]));
    }
}
