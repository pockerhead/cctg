//! Bang and slash commands from a topic that go to the session's terminal
//! (TASK-043), not to the model.
//!
//! A topic message that starts with `!` (Claude Code's bash mode) or with a
//! slash command the hub does not serve itself (`/compact`, `/model`, `/cost`,
//! a skill `/plugin:name`; `/brief` and `/full` never get here) is typed into
//! the input box of the claude console of the slot's live session by its
//! agent, as if the user typed it at the terminal ([`crate::keys::type_line`]:
//! typed, read back from the screen, Enter only on an exact match). Its output
//! comes back through the ordinary transcript stream.
//!
//! A command is refused with an answer in the topic, never queued and never
//! passed to the model as text: while a turn runs or a permission prompt
//! waits (typing would go into the turn's queue or answer the dialog; a hub
//! queue would have to outlive the turn and keep its place among the
//! messages waiting in the slot, for a command the user can simply send
//! again), when the slot has no live session with an agent that types
//! ([`crate::wire::Register::console_commands`]), when the text is not
//! one short plain line ([`crate::keys::typable`]), and when the agent finds
//! the terminal showing the agent view or a working background agent
//! (TASK-047).

use std::time::Duration;

use crate::keys;

/// A command the agent was asked to type and has not answered is forgotten
/// after this.
pub const COMMAND_WAIT: Duration = Duration::from_secs(30);
/// Commands asked and not answered, at most.
pub const MAX_COMMAND_ASKS: usize = 32;

pub const INVALID_NOTICE: &str = "Команда для терминала не набрана: нужна одна строка до 200 символов \
без переводов строки, управляющих символов и эмодзи.";
pub const OFFLINE_NOTICE: &str = "Команда для терминала не набрана: сессия не на связи.";
pub const NO_CONSOLE_NOTICE: &str = "Команда для терминала не набрана: клиент этой сессии \
не умеет набирать команды (нужен свежий cctg на Windows).";
pub const BUSY_NOTICE: &str = "Сессия занята: команда не набрана, повторите после хода.";
pub const WAITING_NOTICE: &str =
    "Сессия ждёт ответа на запрос разрешения: команда не набрана, повторите после него.";
pub const DRAFT_NOTICE: &str =
    "В поле ввода терминала есть неотправленный текст: команда не набрана.";
pub const FAILED_NOTICE: &str = "Не получилось набрать команду в терминале сессии.";
/// The terminal shows the agent view or a working background agent
/// ([`crate::keys::agents_block`], TASK-047).
pub const AGENTS_NOTICE: &str = "В терминале открыт вид субагента или работают фоновые агенты: команда не набрана. Повторите, когда они закончат (или вернитесь к main в терминале).";

/// A console command whose text cannot be typed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Invalid;

/// `None`: `text` is a message for the model. `Some(Ok(line))`: a console
/// command, `line` is what to type (trimmed; a slash command loses the
/// `@bot` Telegram adds from its command menu). `Some(Err(Invalid))`: a
/// console command that cannot be typed (see [`keys::typable`]).
///
/// A slash command is `/` plus a name of letters, digits, `_`, `-`, `:` or
/// `.`, then a space or the end: `/tmp/x fails` or `/ hello` are messages.
/// A bang command needs something after the `!`.
pub fn classify(text: &str) -> Option<Result<String, Invalid>> {
    let text = text.trim();
    let line = if let Some(command) = text.strip_prefix('!') {
        if command.trim().is_empty() {
            return None;
        }
        text.to_owned()
    } else {
        let rest = text.strip_prefix('/')?;
        let end = rest.find(char::is_whitespace).unwrap_or(rest.len());
        let (head, args) = rest.split_at(end);
        let name = match head.split_once('@') {
            Some((name, bot)) if is_bot_name(bot) => name,
            Some(_) => return None,
            None => head,
        };
        let name_ok = !name.is_empty()
            && name
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || matches!(c, '_' | '-' | ':' | '.'));
        if !name_ok {
            return None;
        }
        format!("/{name}{args}")
    };
    Some(if keys::typable(&line) {
        Ok(line)
    } else {
        Err(Invalid)
    })
}

fn is_bot_name(name: &str) -> bool {
    !name.is_empty() && name.chars().all(|c| c.is_ascii_alphanumeric() || c == '_')
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bang_and_slash_commands_are_typed_and_the_rest_is_a_message() {
        for (text, line) in [
            ("!echo hi", "!echo hi"),
            ("  ! ls -la  ", "! ls -la"),
            ("/compact", "/compact"),
            ("/compact keep the plan", "/compact keep the plan"),
            ("/model@cctg_bot sonnet", "/model sonnet"),
            ("/cost", "/cost"),
            ("/maw-tasks add one", "/maw-tasks add one"),
            ("/plugin:skill", "/plugin:skill"),
            ("/brief", "/brief"),
        ] {
            assert_eq!(classify(text), Some(Ok(line.to_owned())), "{text}");
        }
        for text in [
            "hello",
            "",
            "!",
            "!   ",
            "/",
            "/ hello",
            "/tmp/app.log fails",
            "/путь",
            "/x@ bot",
            "/x@a.b",
            "say !echo hi",
            "look at /compact",
        ] {
            assert_eq!(classify(text), None, "{text}");
        }
    }

    #[test]
    fn a_command_that_cannot_be_typed_is_refused() {
        for text in [
            "!echo a\nb",
            "/compact\nand more",
            "!echo \u{1b}[31m",
            "!echo \u{1F600}",
        ] {
            assert_eq!(classify(text), Some(Err(Invalid)), "{text:?}");
        }
        let long = format!("!{}", "x".repeat(keys::MAX_LINE_CHARS));
        assert_eq!(classify(&long), Some(Err(Invalid)));
        assert!(INVALID_NOTICE.contains(&keys::MAX_LINE_CHARS.to_string()));
    }
}
