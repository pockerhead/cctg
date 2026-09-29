//! Mention mode of a shared slot's group topic (TASK-077).
//!
//! In the group topic of a slot shared from its owner's private chat, a
//! message that does not address the agent does not go to the session: it
//! is kept, rendered, in the group view's [`Backlog`]. A message that does
//! address it (`@<bot username>` in its words, or an explicit reply to the
//! bot's own message) takes that backlog along as the history block of what
//! the session reads ([`crate::hub::buffer::History`]). A history longer than
//! the owner's limit is compressed by the session's agent or, when that
//! fails, cut to its newest part ([`cut_history`]).
//!
//! Pure: the slots actor owns every backlog through the registry.

use std::collections::VecDeque;

use serde::{Deserialize, Serialize};

use super::buffer::{Attachment, PART_SEPARATOR, Parked};
use super::chat::{Chat, GroupChat};
use crate::wire::FileKind;

/// Messages kept per group view; one more drops the oldest.
pub const MAX_BACKLOG: usize = 200;
/// Bytes of kept messages per group view; more drops the oldest.
pub const MAX_BACKLOG_BYTES: usize = 64 * 1024;

/// A mode switch to «every message», told in the group topic.
pub const MODE_ALL_TEXT: &str = "Агент теперь читает каждое сообщение этой темы.";

/// Told once in a group topic whose messages the agent keeps for later.
pub fn mention_hint(username: &str) -> String {
    format!(
        "Агент читает эту тему, но отвечает, только когда к нему обращаются: @{username} в сообщении или ответом на его сообщение. Всё, что написано до этого, он получит вместе с обращением."
    )
}

/// A mode switch to «mentions only», told in the group topic.
pub fn mode_mention_text(username: &str) -> String {
    format!("Агент теперь отвечает только на обращения: @{username} или ответ на его сообщение.")
}

fn word(c: char) -> bool {
    c.is_ascii_alphanumeric() || c == '_'
}

/// `text` names `@username` (ASCII case ignored) as a word of its own: no
/// `[A-Za-z0-9_]` right before the `@` (but for a `/command` there) or right
/// after the name (`/cmd@bot` counts; `x@bot.com` and `@botx` do not).
pub fn mentions(text: &str, username: &str) -> bool {
    if username.is_empty() {
        return false;
    }
    text.match_indices('@').any(|(at, _)| {
        // No word right before, or a `/command` word.
        let word_start = text[..at].trim_end_matches(word);
        let before_ok = word_start.len() == at
            || word_start
                .strip_suffix('/')
                .is_some_and(|before| before.chars().next_back().is_none_or(char::is_whitespace));
        let rest = &text[at + 1..];
        let name_ok = rest
            .get(..username.len())
            .is_some_and(|name| name.eq_ignore_ascii_case(username));
        before_ok && name_ok && !rest[username.len()..].chars().next().is_some_and(word)
    })
}

/// The group messages kept for the next mention, each as the session will
/// read it; the oldest go first when there are more than [`MAX_BACKLOG`]
/// or [`MAX_BACKLOG_BYTES`].
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Backlog {
    #[serde(default)]
    pub parts: VecDeque<String>,
    /// Kept messages dropped for newer ones.
    #[serde(default, skip_serializing_if = "is_zero")]
    pub dropped: u32,
}

fn is_zero(n: &u32) -> bool {
    *n == 0
}

impl Backlog {
    pub fn is_empty(&self) -> bool {
        *self == Self::default()
    }

    /// Keeps `part` as the newest; the oldest go while over a cap (the
    /// newest always stays).
    pub fn push(&mut self, part: String) {
        self.parts.push_back(part);
        while self.parts.len() > 1
            && (self.parts.len() > MAX_BACKLOG
                || self.parts.iter().map(String::len).sum::<usize>() > MAX_BACKLOG_BYTES)
        {
            self.parts.pop_front();
            self.dropped = self.dropped.saturating_add(1);
        }
    }
}

/// What stands for a file in the history: the file itself is never
/// downloaded for it.
fn placeholder(file: &Attachment) -> String {
    let named = |name: Option<&str>| match name {
        Some(name) => format!("[файл {name}]"),
        None => "[файл]".to_owned(),
    };
    match file.kind {
        FileKind::Photo => "[фото]".to_owned(),
        FileKind::Video | FileKind::Animation => "[видео]".to_owned(),
        FileKind::Voice => "[голосовое]".to_owned(),
        FileKind::Audio => "[аудио]".to_owned(),
        FileKind::Document | FileKind::Other => named(file.name.as_deref()),
    }
}

/// One kept message as the session will read it: like
/// [`Parked::content`] (quote, `Name: `, forward mark, words), a file as its
/// placeholder followed by the caption.
pub fn part(
    text: &str,
    quote: Option<&str>,
    from_name: Option<&str>,
    forwarded: bool,
    file: Option<&Attachment>,
) -> String {
    let words = match file {
        Some(file) if text.is_empty() => placeholder(file),
        Some(file) => format!("{} {text}", placeholder(file)),
        None => text.to_owned(),
    };
    // Only its content is read: the chat is a placeholder.
    Parked {
        chat: Chat::Group(GroupChat::of(0)),
        message_id: 0,
        thread_id: 0,
        text: words,
        reply_to: None,
        quote: quote.map(str::to_owned),
        forwarded,
        file: None,
        from_name: from_name.map(str::to_owned),
        history: None,
    }
    .content()
}

/// The kept messages as one text, [`PART_SEPARATOR`] between them.
pub fn render(backlog: &Backlog) -> String {
    backlog
        .parts
        .iter()
        .map(String::as_str)
        .collect::<Vec<_>>()
        .join(PART_SEPARATOR)
}

/// At most `limit` characters of history `text`: its newest parts (split by
/// [`PART_SEPARATOR`]) whole while they fit; when the newest alone is
/// longer, its last `limit - 1` characters after `…`.
pub fn cut_history(text: &str, limit: usize) -> String {
    if limit == 0 {
        return String::new();
    }
    let separator = PART_SEPARATOR.chars().count();
    let mut kept: Vec<&str> = Vec::new();
    let mut size = 0;
    for part in text.rsplit(PART_SEPARATOR) {
        let more = part.chars().count() + if kept.is_empty() { 0 } else { separator };
        if size + more > limit {
            break;
        }
        size += more;
        kept.push(part);
    }
    if kept.is_empty() {
        let newest = text.rsplit(PART_SEPARATOR).next().unwrap_or_default();
        let skip = newest.chars().count() - (limit - 1);
        let newest_end: String = newest.chars().skip(skip).collect();
        return format!("…{newest_end}");
    }
    kept.reverse();
    kept.join(PART_SEPARATOR)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_mention_is_the_name_as_a_word_of_its_own() {
        let bot = "cctg_bot";
        for yes in [
            "@cctg_bot сделай",
            "сделай @cctg_bot",
            "@CCTG_Bot",
            "эй, @cctg_bot, глянь",
            "/brief@cctg_bot",
            "ну /full@cctg_bot 2",
            "/@cctg_bot",
            "(@cctg_bot)",
            "@cctg_bot.",
            "привет@cctg_bot",
            "@cctg_bot\nи ещё",
            "📣@cctg_bot",
            // Telegram names are ASCII: a Cyrillic ending is no part of one.
            "@cctg_botу",
        ] {
            assert!(mentions(yes, bot), "{yes}");
        }
        for no in [
            "",
            "cctg_bot",
            "@cctg_botx",
            "@cctg_bot_",
            "x@cctg_bot.com",
            "_@cctg_bot",
            "a/cmd@cctg_bot",
            "@cctg",
            "@",
            "@cctg_bo",
            "@ cctg_bot",
        ] {
            assert!(!mentions(no, bot), "{no}");
        }
        // A later match counts after a first that does not.
        assert!(mentions("x@cctg_bot и @cctg_bot", bot));
        // A multi-byte char where the name would end never panics.
        assert!(!mentions("@cctgы", bot));
        assert!(!mentions("@cctg_boы", bot));
        assert!(!mentions("@cctg_bot", ""));
    }

    #[test]
    fn the_backlog_keeps_the_newest_within_its_caps() {
        let mut backlog = Backlog::default();
        assert!(backlog.is_empty());
        for n in 0..MAX_BACKLOG + 3 {
            backlog.push(format!("m{n}"));
        }
        assert_eq!(backlog.parts.len(), MAX_BACKLOG);
        assert_eq!(backlog.dropped, 3);
        assert_eq!(backlog.parts.front().map(String::as_str), Some("m3"));
        assert_eq!(
            backlog.parts.back().map(String::as_str),
            Some(format!("m{}", MAX_BACKLOG + 2).as_str())
        );
        // Bytes: 17 parts of 4 KiB do not fit 64 KiB, 16 do.
        let mut backlog = Backlog::default();
        for n in 0..17 {
            backlog.push(format!("{n:04}{}", "x".repeat(4092)));
        }
        assert_eq!(backlog.parts.len(), 16);
        assert_eq!(backlog.dropped, 1);
        assert!(backlog.parts[0].starts_with("0001"));
        // The newest always stays.
        let mut backlog = Backlog::default();
        backlog.push("a".into());
        backlog.push("b".repeat(MAX_BACKLOG_BYTES + 1));
        assert_eq!(backlog.parts.len(), 1);
        assert_eq!(backlog.dropped, 1);
    }

    #[test]
    fn the_backlog_is_written_only_when_it_holds_something() {
        let empty = serde_json::to_string(&Backlog::default()).unwrap();
        assert_eq!(empty, r#"{"parts":[]}"#);
        let mut backlog = Backlog::default();
        backlog.push("Анна: да".into());
        let text = serde_json::to_string(&backlog).unwrap();
        assert_eq!(text, r#"{"parts":["Анна: да"]}"#);
        assert_eq!(serde_json::from_str::<Backlog>(&text).unwrap(), backlog);
        assert_eq!(
            serde_json::from_str::<Backlog>("{}").unwrap(),
            Backlog::default()
        );
    }

    #[test]
    fn a_part_reads_like_the_message_with_files_as_placeholders() {
        assert_eq!(part("да", None, None, false, None), "да");
        assert_eq!(part("да", None, Some("Анна"), false, None), "Анна: да");
        assert_eq!(
            part("да", Some("вопрос"), Some("Анна"), false, None),
            "> вопрос\n\nАнна: да"
        );
        assert_eq!(
            part("чужое", None, Some("Анна"), true, None),
            "Анна: (переслано)\nчужое"
        );
        let file = |kind, name: Option<&str>| Attachment {
            kind,
            file_id: "f".into(),
            name: name.map(str::to_owned),
            size: None,
        };
        for (kind, name, want) in [
            (FileKind::Photo, None, "[фото]"),
            (FileKind::Video, None, "[видео]"),
            (FileKind::Animation, Some("x.mp4"), "[видео]"),
            (FileKind::Voice, None, "[голосовое]"),
            (FileKind::Audio, Some("a.mp3"), "[аудио]"),
            (FileKind::Document, Some("план.pdf"), "[файл план.pdf]"),
            (FileKind::Document, None, "[файл]"),
            (FileKind::Other, None, "[файл]"),
        ] {
            assert_eq!(part("", None, None, false, Some(&file(kind, name))), want);
        }
        assert_eq!(
            part(
                "схема",
                None,
                Some("Иван"),
                false,
                Some(&file(FileKind::Photo, None))
            ),
            "Иван: [фото] схема"
        );
    }

    #[test]
    fn the_history_is_its_parts_between_separators() {
        let mut backlog = Backlog::default();
        assert_eq!(render(&backlog), "");
        backlog.push("Анна: a".into());
        backlog.push("Иван: b".into());
        assert_eq!(render(&backlog), "Анна: a\n\n---\n\nИван: b");
    }

    #[test]
    fn a_cut_keeps_the_newest_parts_whole_or_the_tail_of_the_newest() {
        let text = ["a".repeat(10), "b".repeat(10), "c".repeat(10)].join(PART_SEPARATOR);
        let separator = PART_SEPARATOR.chars().count();
        // Everything fits.
        assert_eq!(cut_history(&text, 1000), text);
        assert_eq!(cut_history(&text, 30 + 2 * separator), text);
        // The two newest.
        assert_eq!(
            cut_history(&text, 30 + 2 * separator - 1),
            ["b".repeat(10), "c".repeat(10)].join(PART_SEPARATOR)
        );
        assert_eq!(cut_history(&text, 10), "c".repeat(10));
        // Only the newest's tail.
        assert_eq!(cut_history(&text, 5), "…cccc");
        // Char boundaries: Cyrillic and emoji.
        let wide = format!("начало{PART_SEPARATOR}абвгд😀ё");
        assert_eq!(cut_history(&wide, 4), "…д😀ё");
        assert_eq!(cut_history(&wide, 3).chars().count(), 3);
        // A compressed text is one part.
        assert_eq!(cut_history("сводка", 4), "…дка");
        // Tiny limits.
        assert_eq!(cut_history(&text, 1), "…");
        assert_eq!(cut_history(&text, 0), "");
        assert_eq!(cut_history("", 5), "");
    }

    #[test]
    fn the_notices_name_the_bot() {
        assert!(mention_hint("cctg_bot").contains("@cctg_bot"));
        assert!(mode_mention_text("cctg_bot").contains("@cctg_bot"));
        for text in [
            mention_hint("cctg_bot"),
            mode_mention_text("cctg_bot"),
            MODE_ALL_TEXT.to_owned(),
        ] {
            assert!(text.chars().count() < 300, "{text}");
        }
    }
}
