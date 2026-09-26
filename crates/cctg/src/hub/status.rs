//! The pinned status message of a slot (TASK-029): one message per slot,
//! pinned once, edited as the slot's current session works. Its first line is
//! what the session does now, as an emoji and a few words; the second line
//! the numbers of Claude Code's status line (model, effort, context and rate
//! limit percentages). While the session works, no permission prompt of it
//! waits and its agent can press keys in its console, the message carries ⏹
//! (Esc, with a confirming second press). Esc at an open permission prompt
//! answers the prompt instead of stopping the turn, so ⏹ is never offered
//! then.
//!
//! This module is the pure part: what a session does ([`Activity`], from
//! hooks, the stream and written keys), how that reads ([`render`]) and the
//! buttons. The slots actor owns the message, its edits and the presses.

use std::collections::VecDeque;
use std::time::Duration;

use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use super::permissions;

/// A first ⏹ press waits this long for the confirming second one.
pub const CONFIRM_FOR: Duration = Duration::from_secs(10);
/// A key the agent was asked to press and has not answered is forgotten
/// after this.
pub const KEY_WAIT: Duration = Duration::from_secs(30);
/// Keys asked and not answered, at most.
pub const MAX_KEY_ASKS: usize = 32;
/// Tool calls shown as running at a time; an older one goes first.
const MAX_RUNNING: usize = 16;
/// Ended calls remembered, so a start that arrives after its end (the tool
/// hooks run in the background) does not show a finished call.
const MAX_FINISHED: usize = 64;
/// The start of Claude Code's own transcript line after Esc.
pub const INTERRUPT_NOTE: &str = "[Request interrupted by user";

const STOP: &str = "status:stop";
const CONFIRM: &str = "status:confirm";
const UPDATE: &str = "status:update";
/// The update button of an outdated-client warning: `update:<session id>`.
const UPDATE_PREFIX: &str = "update:";

pub const ANSWER_CONFIRM: &str = "Нажмите ещё раз, чтобы прервать";
pub const ANSWER_INTERRUPTING: &str = "Прерываю";
pub const ANSWER_IDLE: &str = "Сейчас нечего прерывать";
pub const ANSWER_WAITING: &str = "Сначала ответьте на запрос разрешения";
pub const ANSWER_OFFLINE: &str = "Сессия не на связи";
pub const ANSWER_NO_KEYS: &str = "Эта сессия не принимает клавиши из Telegram";
pub const ANSWER_STALE: &str = "Кнопка устарела";
pub const KEY_FAILED_NOTICE: &str = "Не получилось нажать клавишу в терминале сессии.";

pub const UPDATE_BUTTON: &str = "⬆️ Обновить";
pub const OUTDATED_LINE: &str = "⬆️ Клиент cctg устарел";
pub const ANSWER_UPDATING: &str = "Обновляю";
pub const ANSWER_AFTER_TURN: &str = "Идёт ход: обновлю, когда он закончится (⏹ прервёт его)";
pub const ANSWER_UPDATE_RUNNING: &str = "Обновление уже идёт";
pub const ANSWER_CURRENT: &str = "Клиент уже обновлён";
pub const ANSWER_OLD_CLIENT: &str =
    "Этот клиент старше обновлений из Telegram: перезапустите сессию вручную";
pub const UPDATED_NOTICE: &str = "✅ Клиент cctg обновлён.";
/// The client found no newer file of itself (TASK-040). The hub cannot tell
/// whether that machine is its own, so the text covers both (TASK-035).
pub const NO_NEW_BUILD_NOTICE: &str = "На машине этой сессии нет новой сборки cctg. Если hub на этой же машине: cctg deploy, потом «Обновить». Если нет: положите на место файла cctg этой машины сборку того же коммита, что у hub (GitHub Releases, номер сборки в предупреждении), потом «Обновить».";
pub const MANUAL_RESTART_NOTICE: &str = "Нужен перезапуск claude, а сессия запущена не через cctg run (claude-cctg). Выйдите из claude и запустите claude-cctg --resume с id этой сессии.";
pub const DRAFT_NOTICE: &str = "В поле ввода терминала есть неотправленный текст, поэтому /exit не отправлен. Отправьте или сотрите его и нажмите «Обновить» ещё раз.";
pub const UPDATE_WAITS_NOTICE: &str = "⏳ Обновление клиента ждёт конца хода и продолжится само, когда он закончится (⏹ прервёт ход).";
/// An update waits for background agents (TASK-047): the terminal shows the
/// agent view or a working one (asked again after
/// [`super::slots::UPDATE_RETRY`]), or the hub saw a subagent start and not
/// stop yet.
pub const UPDATE_AGENTS_NOTICE: &str = "⏳ Обновление клиента ждёт: работают фоновые агенты или в терминале открыт вид субагента. Продолжится само, когда агенты закончат; вид субагента закройте (вернитесь к main).";
pub const UPDATE_FAILED_NOTICE: &str =
    "Обновить клиент не получилось; подробности в debug-логе claude этой сессии.";
/// The client could not download the hub's release (TASK-050).
pub const DOWNLOAD_FAILED_NOTICE: &str = "Не получилось скачать или поставить сборку cctg релиза hub: нет связи с GitHub Releases, не хватило времени или файл не записался. Старая сборка на месте; нажмите «Обновить» ещё раз позже, подробности в debug-логе claude этой сессии.";
pub const CHECKSUM_NOTICE: &str = "Скачанная сборка cctg не совпала с SHA256SUMS релиза hub и не поставлена. Старая сборка на месте.";
pub const NO_RELEASE_BUILD_NOTICE: &str = "В релизе hub нет сборки cctg для платформы этой машины или она ещё не выложена. Старая сборка на месте. Нажмите «Обновить» ещё раз через несколько минут; если не поможет, поставьте клиент вручную (install.sh --from-source) и нажмите «Обновить».";

/// A status button.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Press {
    /// ⏹, first press.
    Stop,
    /// ⏹, the confirming press.
    Confirm,
    /// ⬆️ Обновить.
    Update,
}

/// The status button of `data`; anything else is not one.
pub fn parse_callback(data: &str) -> Option<Press> {
    match data {
        STOP => Some(Press::Stop),
        CONFIRM => Some(Press::Confirm),
        UPDATE => Some(Press::Update),
        _ => None,
    }
}

/// The keyboard of an outdated-client warning for `session`; `None` when
/// the id does not fit Telegram's 64 bytes of callback data.
pub fn update_keyboard(session: &str) -> Option<Value> {
    let data = format!("{UPDATE_PREFIX}{session}");
    (data.len() <= 64)
        .then(|| json!({ "inline_keyboard": [[{ "text": UPDATE_BUTTON, "callback_data": data }]] }))
}

/// The session of a warning's update button.
pub fn parse_update(data: &str) -> Option<&str> {
    data.strip_prefix(UPDATE_PREFIX)
        .filter(|session| !session.is_empty())
}

/// The warning sent once per hub build to the topic of a session whose agent
/// runs another build. `agent`: its short build, `None` for an agent too
/// old to say.
pub fn outdated_text(agent: Option<&str>, hub: &str) -> String {
    let agent = agent.unwrap_or("старый, без номера сборки");
    format!(
        "⬆️ Клиент cctg в этой сессии устарел: сборка {agent}, у hub {hub}. Сам он не обновится: нажмите «Обновить», когда будет удобно."
    )
}

/// Numbers from Claude Code's status line, as `cctg statusline` sent them.
/// Kept per session in `registry.json`, so a restarted hub still shows them.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct Metrics {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub effort: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub context: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub five_hour: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub seven_day: Option<u32>,
}

impl Metrics {
    /// `Opus · high · ctx 50% · 5h 3% · 7d 92%`, the parts that are known.
    fn line(&self) -> String {
        let mut parts: Vec<String> = [&self.model, &self.effort]
            .into_iter()
            .flatten()
            .cloned()
            .collect();
        for (label, value) in [
            ("ctx", self.context),
            ("5h", self.five_hour),
            ("7d", self.seven_day),
        ] {
            if let Some(value) = value {
                parts.push(format!("{label} {value}%"));
            }
        }
        parts.join(" · ")
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct Running {
    id: String,
    line: String,
}

/// What one top-level session does, as far as the hub knows.
#[derive(Debug, Clone, Default)]
pub struct Activity {
    /// Between a prompt and the end of its turn (`Stop` or an interrupt).
    turn: bool,
    /// Tool calls of the main conversation that started and did not end,
    /// oldest first.
    running: Vec<Running>,
    finished: VecDeque<String>,
    /// Transcript bytes up to which interrupt notes were taken.
    noted_to: u64,
    /// Esc was written into the console during this turn. Not an end: only
    /// `Stop`, an interrupt note, the next prompt or the session's end tell
    /// that the turn is over.
    interrupt_sent: bool,
}

impl Activity {
    /// A prompt went in: a turn starts.
    pub fn prompt(&mut self) {
        self.turn = true;
        self.interrupt_sent = false;
        self.running.clear();
    }

    /// The turn ended (`Stop`, an interrupt note).
    pub fn stop(&mut self) {
        self.turn = false;
        self.interrupt_sent = false;
        self.running.clear();
    }

    /// Esc went into the console: the calls shown are no longer trusted, and
    /// the turn is not known to be over until a real signal comes.
    pub fn interrupt_written(&mut self) {
        self.interrupt_sent = true;
        self.running.clear();
    }

    /// A call started. It does not start a turn: a start that comes after
    /// its turn's `Stop` (the tool hooks run in the background) must not
    /// open the turn again.
    pub fn tool_start(&mut self, id: &str, line: &str) {
        if self.finished.iter().any(|done| done == id) || self.running.iter().any(|r| r.id == id) {
            return;
        }
        if self.running.len() >= MAX_RUNNING {
            self.running.remove(0);
        }
        self.running.push(Running {
            id: id.to_owned(),
            line: line.to_owned(),
        });
    }

    pub fn tool_end(&mut self, id: &str) {
        self.running.retain(|running| running.id != id);
        if self.finished.len() >= MAX_FINISHED {
            self.finished.pop_front();
        }
        self.finished.push_back(id.to_owned());
    }

    /// An interrupt note in the transcript line that ends at byte `end`. A
    /// note read again after a stream rewind changes nothing. `true` when it
    /// ended the turn.
    pub fn interrupted_at(&mut self, end: u64) -> bool {
        if end <= self.noted_to {
            return false;
        }
        self.noted_to = end;
        self.stop();
        true
    }

    /// The session works and no Esc was written into this turn yet: a turn
    /// or a tool call runs.
    pub fn busy(&self) -> bool {
        !self.interrupt_sent && self.working()
    }

    /// A turn or a call runs, stopped by Esc or not: a restart now cuts it
    /// off (TASK-047).
    pub fn working(&self) -> bool {
        self.turn || !self.running.is_empty()
    }
}

/// The first line of the status message.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Phase {
    Ended,
    /// A permission prompt waits.
    Waiting,
    /// Esc was written; the end of the turn is not known yet.
    InterruptSent,
    /// The newest running call's line and how many more run.
    Tool {
        line: String,
        more: usize,
    },
    Thinking,
    Idle,
}

/// What the session of a slot does: an ended session is ended, a waiting
/// prompt comes before a written Esc, that before running calls, a running
/// call before a bare turn.
pub fn phase(activity: Option<&Activity>, ended: bool, waiting: bool) -> Phase {
    if ended {
        return Phase::Ended;
    }
    if waiting {
        return Phase::Waiting;
    }
    let Some(activity) = activity else {
        return Phase::Idle;
    };
    if activity.interrupt_sent {
        return Phase::InterruptSent;
    }
    if let Some(newest) = activity.running.last() {
        return Phase::Tool {
            line: newest.line.clone(),
            more: activity.running.len() - 1,
        };
    }
    if activity.turn {
        Phase::Thinking
    } else {
        Phase::Idle
    }
}

/// Which buttons the message carries.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Buttons {
    /// ⏹: the session works, no prompt waits and its agent presses keys.
    pub interrupt: bool,
    /// ⏹ asks for its confirming press.
    pub confirm: bool,
    /// The session's client is outdated: a line and ⬆️ Обновить.
    pub update: bool,
}

/// The text and keyboard of the status message. The keyboard is always
/// explicit: an empty one removes buttons an earlier edit showed.
pub fn render(phase: &Phase, metrics: Option<&Metrics>, buttons: Buttons) -> (String, Value) {
    let head = match phase {
        Phase::Ended => "🏁 Сессия завершена".to_owned(),
        Phase::Waiting => "❓ Ждёт разрешения".to_owned(),
        Phase::InterruptSent => "⏹ Esc отправлен в терминал".to_owned(),
        Phase::Tool { line, more } => {
            let line = line.strip_prefix("• ").unwrap_or(line);
            let line = super::registry::cut(line, 200);
            match more {
                0 => format!("⚙️ {line}"),
                more => format!("⚙️ {line} (+{more})"),
            }
        }
        Phase::Thinking => "💭 Думает".to_owned(),
        Phase::Idle => "💤 Ждёт вас".to_owned(),
    };
    let numbers = metrics.map(Metrics::line).unwrap_or_default();
    let mut text = if numbers.is_empty() {
        head
    } else {
        format!("{head}\n{numbers}")
    };
    if buttons.update {
        text.push('\n');
        text.push_str(OUTDATED_LINE);
    }
    let mut row = Vec::new();
    if buttons.interrupt {
        row.push(if buttons.confirm {
            json!({ "text": "⏹ Точно прервать?", "callback_data": CONFIRM })
        } else {
            json!({ "text": "⏹ Прервать", "callback_data": STOP })
        });
    }
    if buttons.update {
        row.push(json!({ "text": UPDATE_BUTTON, "callback_data": UPDATE }));
    }
    let keyboard = if row.is_empty() {
        permissions::no_keyboard()
    } else {
        json!({ "inline_keyboard": [row] })
    };
    (text, keyboard)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn metrics() -> Metrics {
        Metrics {
            model: Some("Opus 5.5".into()),
            effort: Some("high".into()),
            context: Some(50),
            five_hour: Some(3),
            seven_day: None,
        }
    }

    fn buttons(keyboard: &Value) -> Vec<(String, String)> {
        keyboard["inline_keyboard"]
            .as_array()
            .unwrap()
            .iter()
            .flat_map(|row| row.as_array().unwrap().iter())
            .map(|button| {
                (
                    button["text"].as_str().unwrap().to_owned(),
                    button["callback_data"].as_str().unwrap().to_owned(),
                )
            })
            .collect()
    }

    #[test]
    fn the_first_line_follows_what_the_session_does() {
        let mut activity = Activity::default();
        assert_eq!(phase(Some(&activity), false, false), Phase::Idle);
        activity.prompt();
        assert_eq!(phase(Some(&activity), false, false), Phase::Thinking);
        activity.tool_start("t1", "• Bash: Run tests");
        activity.tool_start("t2", "• Read: a.rs");
        assert_eq!(
            phase(Some(&activity), false, false),
            Phase::Tool {
                line: "• Read: a.rs".into(),
                more: 1
            }
        );
        activity.tool_end("t1");
        assert_eq!(phase(Some(&activity), false, true), Phase::Waiting);
        assert_eq!(phase(Some(&activity), true, true), Phase::Ended);
        activity.tool_end("t2");
        assert_eq!(phase(Some(&activity), false, false), Phase::Thinking);
        activity.stop();
        assert_eq!(phase(Some(&activity), false, false), Phase::Idle);
        assert!(!activity.busy());
        assert_eq!(phase(None, false, false), Phase::Idle);
    }

    #[test]
    fn a_written_esc_is_not_the_end_of_the_turn() {
        let mut activity = Activity::default();
        activity.prompt();
        activity.tool_start("t1", "• Bash: sleep");
        activity.interrupt_written();
        assert_eq!(phase(Some(&activity), false, false), Phase::InterruptSent);
        assert!(!activity.busy(), "no second Esc for the same turn");
        // A prompt that opened meanwhile still comes first.
        assert_eq!(phase(Some(&activity), false, true), Phase::Waiting);
        // A late start of a call from before Esc does not bring ⏹ back.
        activity.tool_start("t2", "• Read: a.rs");
        assert_eq!(phase(Some(&activity), false, false), Phase::InterruptSent);
        activity.stop();
        assert_eq!(phase(Some(&activity), false, false), Phase::Idle);
        activity.interrupt_written();
        activity.prompt();
        assert_eq!(phase(Some(&activity), false, false), Phase::Thinking);
        assert!(activity.busy());
        activity.interrupt_written();
        assert!(activity.interrupted_at(10));
        assert_eq!(phase(Some(&activity), false, false), Phase::Idle);
    }

    #[test]
    fn a_start_after_its_end_and_a_repeated_start_change_nothing() {
        let mut activity = Activity::default();
        activity.tool_end("t1");
        activity.tool_start("t1", "• Read: a.rs");
        assert!(!activity.busy(), "the end came first: nothing runs");
        activity.tool_start("t2", "• Bash: x");
        activity.tool_start("t2", "• Bash: x");
        assert_eq!(activity.running.len(), 1);
        for index in 0..MAX_RUNNING + 3 {
            activity.tool_start(&format!("n{index}"), "• Read");
        }
        assert_eq!(activity.running.len(), MAX_RUNNING);
        for index in 0..MAX_FINISHED + 3 {
            activity.tool_end(&format!("e{index}"));
        }
        assert_eq!(activity.finished.len(), MAX_FINISHED);
    }

    #[test]
    fn a_start_after_stop_does_not_open_the_turn_again() {
        let mut activity = Activity::default();
        activity.prompt();
        activity.stop();
        activity.tool_start("t1", "• Read: a.rs");
        activity.tool_end("t1");
        assert_eq!(phase(Some(&activity), false, false), Phase::Idle);
        assert!(!activity.busy());
    }

    #[test]
    fn an_interrupt_note_ends_the_turn_once() {
        let mut activity = Activity::default();
        activity.prompt();
        activity.tool_start("t1", "• Bash: sleep");
        assert!(activity.interrupted_at(100));
        assert!(!activity.busy());
        activity.prompt();
        // The same note read again after a rewind: the new turn stays.
        assert!(!activity.interrupted_at(100));
        assert!(activity.busy());
        assert!(activity.interrupted_at(250));
        assert!(!activity.busy());
    }

    #[test]
    fn the_message_shows_the_phase_the_numbers_and_the_buttons() {
        let (text, keyboard) = render(&Phase::Idle, Some(&metrics()), Buttons::default());
        assert_eq!(text, "💤 Ждёт вас\nOpus 5.5 · high · ctx 50% · 5h 3%");
        assert_eq!(keyboard, permissions::no_keyboard());
        let (text, _) = render(&Phase::Thinking, None, Buttons::default());
        assert_eq!(text, "💭 Думает");
        let (text, _) = render(&Phase::Ended, Some(&Metrics::default()), Buttons::default());
        assert_eq!(text, "🏁 Сессия завершена");
        let (text, _) = render(&Phase::InterruptSent, None, Buttons::default());
        assert_eq!(text, "⏹ Esc отправлен в терминал");
        let running = Phase::Tool {
            line: "• Bash: Run tests".into(),
            more: 2,
        };
        let stop = Buttons {
            interrupt: true,
            confirm: false,
            update: false,
        };
        let (text, keyboard) = render(&running, None, stop);
        assert_eq!(text, "⚙️ Bash: Run tests (+2)");
        assert_eq!(
            buttons(&keyboard),
            [("⏹ Прервать".to_owned(), STOP.to_owned())]
        );
        let confirm = Buttons {
            confirm: true,
            ..stop
        };
        let outdated = Buttons {
            update: true,
            ..stop
        };
        let (text, keyboard) = render(&Phase::Idle, Some(&metrics()), outdated);
        assert_eq!(
            text,
            "💤 Ждёт вас\nOpus 5.5 · high · ctx 50% · 5h 3%\n⬆️ Клиент cctg устарел"
        );
        assert_eq!(
            buttons(&keyboard),
            [
                ("⏹ Прервать".to_owned(), STOP.to_owned()),
                (UPDATE_BUTTON.to_owned(), UPDATE.to_owned())
            ]
        );
        let (_, keyboard) = render(&running, None, confirm);
        assert_eq!(
            buttons(&keyboard),
            [("⏹ Точно прервать?".to_owned(), CONFIRM.to_owned())]
        );
    }

    #[test]
    fn metrics_survive_the_registry_file_and_leave_out_what_is_unknown() {
        let json = serde_json::to_value(metrics()).unwrap();
        assert_eq!(
            json,
            json!({ "model": "Opus 5.5", "effort": "high", "context": 50, "five_hour": 3 })
        );
        assert_eq!(serde_json::from_value::<Metrics>(json).unwrap(), metrics());
        assert_eq!(
            serde_json::from_value::<Metrics>(json!({})).unwrap(),
            Metrics::default()
        );
    }

    #[test]
    fn only_status_buttons_parse_and_they_fit_callback_data() {
        assert_eq!(parse_callback(STOP), Some(Press::Stop));
        assert_eq!(parse_callback(CONFIRM), Some(Press::Confirm));
        assert_eq!(parse_callback(UPDATE), Some(Press::Update));
        for other in [
            "allow:abcde",
            "resume:x",
            "status:",
            "status:stop ",
            "",
            "update:x",
        ] {
            assert_eq!(parse_callback(other), None, "{other}");
        }
        // Telegram allows 1-64 bytes of callback data.
        for data in [STOP, CONFIRM, UPDATE] {
            assert!(data.len() <= 64);
        }
        let session = "5e551017-0000-4000-8000-000000000001";
        let keyboard = update_keyboard(session).unwrap();
        let data = keyboard["inline_keyboard"][0][0]["callback_data"]
            .as_str()
            .unwrap();
        assert_eq!(parse_update(data), Some(session));
        assert_eq!(parse_update("update:"), None);
        assert_eq!(parse_update("status:update"), None);
        assert_eq!(
            update_keyboard(&"x".repeat(58)),
            None,
            "65 bytes do not fit"
        );
        assert!(outdated_text(Some("ab12cd34"), "ffee0011").contains("ab12cd34"));
        assert!(outdated_text(None, "ffee0011").contains("ffee0011"));
    }

    #[test]
    fn a_long_call_line_is_cut() {
        let long = format!("• Bash: {}", "я".repeat(5000));
        let (text, _) = render(
            &Phase::Tool {
                line: long,
                more: 0,
            },
            None,
            Buttons::default(),
        );
        assert!(transcript::telegram_len(&text) < 300, "{}", text.len());
    }
}
