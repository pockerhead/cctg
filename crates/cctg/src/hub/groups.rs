//! The forum groups the hub shows sessions in (TASK-069).
//!
//! The group of `CCTG_CHAT_ID` is the default one: the hub does not start
//! without it, fallback views and new slots without a private chat go
//! there. More groups join while the hub runs: the bot is added to one as
//! an administrator by an allowlisted user (`my_chat_member`), or an
//! allowlisted user writes `/connect@<bot>` there. Each known group is a
//! [`Group`] record in `registry.json`; a group the bot left is kept as
//! `left`, with its views, for when it comes back.
//!
//! The update poll lets through messages and presses only from known
//! groups ([`KnownGroups`], written by the slots actor). A group's chat id
//! is never logged and never shown: logs name a group by its number in the
//! registry, Telegram texts by its title.

use std::collections::HashSet;
use std::fmt;
use std::future::Future;
use std::sync::{Arc, PoisonError, RwLock};

use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use super::api::{ApiError, BotApi, ChatInfo, ChatMember};
use super::chat::{Chat, GroupChat};
use super::registry::cut;

/// A group the hub knows, as `registry.json` keeps it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Group {
    pub chat: GroupChat,
    /// Its title as Telegram last named it; shown in the group picker and
    /// the menu, never logged.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
    /// A forum supergroup where the bot manages topics: sessions may show
    /// there.
    #[serde(default)]
    pub ready: bool,
    /// The bot may delete messages there (`can_delete_messages`).
    #[serde(default)]
    pub can_delete: bool,
    /// The bot is no longer in it: its views wait, nothing goes there.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub left: bool,
}

/// The groups the update poll lets through: shared by the slots actor,
/// which writes it, and the poll, which reads it.
#[derive(Clone, Default)]
pub struct KnownGroups(Arc<RwLock<HashSet<GroupChat>>>);

impl fmt::Debug for KnownGroups {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("KnownGroups(..)")
    }
}

impl KnownGroups {
    pub fn of(groups: impl IntoIterator<Item = GroupChat>) -> Self {
        Self(Arc::new(RwLock::new(groups.into_iter().collect())))
    }

    /// Chat `id` is a known group.
    pub fn contains(&self, id: i64) -> bool {
        self.0
            .read()
            .unwrap_or_else(PoisonError::into_inner)
            .contains(&GroupChat::of(id))
    }

    pub(crate) fn replace(&self, groups: impl IntoIterator<Item = GroupChat>) {
        *self.0.write().unwrap_or_else(PoisonError::into_inner) = groups.into_iter().collect();
    }
}

/// What keeps sessions out of a group the bot is in.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Missing {
    /// It is no forum supergroup.
    Topics,
    /// The bot is no administrator.
    Admin,
    /// The bot may not manage topics.
    ManageTopics,
}

/// The bot's place in a group.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Membership {
    /// Not in it: left, kicked, or restricted outside it.
    Gone,
    /// In it; ready when nothing is `missing`.
    Present {
        missing: Vec<Missing>,
        can_delete: bool,
    },
}

/// The bot's place in a group: `supergroup` and `is_forum` as Telegram
/// describes the chat, `member` the bot's `ChatMember` there.
pub fn membership(supergroup: bool, is_forum: bool, member: &ChatMember) -> Membership {
    let status = member.status.as_str();
    if matches!(status, "left" | "kicked") || (status == "restricted" && !member.is_member) {
        return Membership::Gone;
    }
    let creator = status == "creator";
    let admin = status == "administrator";
    let mut missing = Vec::new();
    if !supergroup || !is_forum {
        missing.push(Missing::Topics);
    }
    if !creator && !admin {
        missing.push(Missing::Admin);
    }
    if admin && !member.can_manage_topics {
        missing.push(Missing::ManageTopics);
    }
    Membership::Present {
        missing,
        can_delete: creator || (admin && member.can_delete_messages),
    }
}

/// Told in a group's General once sessions may show there.
pub const GROUP_READY_NOTICE: &str = "Группа подключена к cctg. Сессию из лички можно показать здесь: 👥 на статусе сессии, в меню или /share в её теме.";
/// [`GROUP_READY_NOTICE`] while the bot's private chat has no topics
/// (Threaded Mode off): nothing can be shared then.
pub const GROUP_READY_NO_SHARE_NOTICE: &str = "Группа подключена к cctg, но показать здесь сессию пока нельзя: у бота выключены темы в личке (Threaded Mode в @BotFather). Включите их и перезапустите hub.";
/// Added to the notices of a group where the bot may not delete messages.
pub const NO_DELETE_LINE: &str = "Без права \"Удаление сообщений\" служебные строки о темах останутся видны, а когда сессию уберут из группы, её тема не удалится.";

/// `/connect` could not be checked with Telegram.
pub fn connect_failed_notice(username: Option<&str>) -> String {
    format!(
        "Не удалось проверить права бота в группе. Напишите {} ещё раз чуть позже.",
        connect_command(username)
    )
}

/// `/connect@<bot>`: a bot that is no administrator (privacy mode) sees
/// only commands with its name.
fn connect_command(username: Option<&str>) -> String {
    match username {
        Some(username) => format!("/connect@{username}"),
        None => "/connect".to_owned(),
    }
}
/// The default group's name when Telegram gave none.
pub const DEFAULT_GROUP_TITLE: &str = "основная группа";
/// UTF-16 units of a group title on a button or in a menu row.
pub const TITLE_BUTTON_LIMIT: usize = 32;

/// The name of group number `number` (from 1) when Telegram gave none.
pub fn fallback_title(number: usize) -> String {
    format!("группа {number}")
}

/// Told in a group the bot is in but cannot show sessions in; `basic`: a
/// plain group (Telegram turns it into a supergroup when topics go on).
pub fn not_ready_notice(missing: &[Missing], basic: bool, username: Option<&str>) -> String {
    let items: Vec<&str> = missing
        .iter()
        .map(|missing| match missing {
            Missing::Topics if basic => {
                "включите темы в настройках группы (Telegram при этом сделает её супергруппой)"
            }
            Missing::Topics => "включите темы в настройках группы",
            Missing::Admin => "сделайте бота администратором",
            Missing::ManageTopics => "дайте боту право \"Управление темами\"",
        })
        .collect();
    let command = connect_command(username);
    format!(
        "Бот в группе, но показывать здесь сессии пока нельзя: {}. Исправьте и напишите здесь {command}.",
        items.join("; ")
    )
}

/// A button of the group picker (TASK-069).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PickAction {
    Share,
    /// A first unshare press: it asks for the second.
    Unshare,
    Confirm,
}

const PICK: &str = "grp";

impl PickAction {
    fn code(self) -> &'static str {
        match self {
            Self::Share => "s",
            Self::Unshare => "u",
            Self::Confirm => "c",
        }
    }
}

/// `grp:<slot>:<chat id>:<s|u|c>`, at most 33 bytes.
pub fn pick_data(slot: u32, group: GroupChat, action: PickAction) -> String {
    format!("{PICK}:{slot}:{}:{}", group.expose(), action.code())
}

/// The press of a picker button; anything else is none.
pub fn parse_pick(data: &str) -> Option<(u32, GroupChat, PickAction)> {
    let [prefix, slot, id, action] = data.split(':').collect::<Vec<_>>()[..] else {
        return None;
    };
    if prefix != PICK {
        return None;
    }
    let id: i64 = id.parse().ok().filter(|id| *id < 0)?;
    let action = [PickAction::Share, PickAction::Unshare, PickAction::Confirm]
        .into_iter()
        .find(|known| known.code() == action)?;
    Some((slot.parse().ok()?, GroupChat::of(id), action))
}

/// One group of the picker.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PickRow {
    pub group: GroupChat,
    pub title: String,
    /// The slot shows there.
    pub shared: bool,
    /// Its unshare button asks for the second press.
    pub confirm: bool,
}

/// Heads the group picker.
pub const PICKER_TEXT: &str = "Где ещё показывать эту сессию:";

/// The text and keyboard of the group picker of `slot`: a button per group.
pub fn picker(slot: u32, rows: &[PickRow]) -> (String, Value) {
    let buttons: Vec<Value> = rows
        .iter()
        .map(|row| {
            let title = cut(&row.title, TITLE_BUTTON_LIMIT);
            let (text, action) = match (row.shared, row.confirm) {
                (false, _) => (format!("👥 {title}"), PickAction::Share),
                (true, false) => (format!("🙈 {title}"), PickAction::Unshare),
                (true, true) => (format!("🙈 {title} — точно?"), PickAction::Confirm),
            };
            json!([{ "text": text, "callback_data": pick_data(slot, row.group, action) }])
        })
        .collect();
    (
        PICKER_TEXT.to_owned(),
        json!({ "inline_keyboard": buttons }),
    )
}

/// What the hub asks Telegram about a group: `BotApi` in production, a
/// fake in tests. The calls go around the scheduler: they are reads (and a
/// leave), not messages in a chat.
pub trait GroupLookup: Send + Sync + 'static {
    /// The bot's `ChatMember` in `group`.
    fn member(&self, group: GroupChat)
    -> impl Future<Output = Result<ChatMember, ApiError>> + Send;
    /// `getChat` of `group`.
    fn info(&self, group: GroupChat) -> impl Future<Output = Result<ChatInfo, ApiError>> + Send;
    /// The bot leaves `group`.
    fn leave(&self, group: GroupChat) -> impl Future<Output = Result<(), ApiError>> + Send;
}

/// [`GroupLookup`] through the Bot API for the bot `bot_id`.
pub struct BotLookup {
    api: Arc<BotApi>,
    bot_id: i64,
}

impl BotLookup {
    pub fn new(api: Arc<BotApi>, bot_id: i64) -> Self {
        Self { api, bot_id }
    }
}

impl GroupLookup for BotLookup {
    async fn member(&self, group: GroupChat) -> Result<ChatMember, ApiError> {
        self.api
            .get_chat_member(Chat::Group(group), self.bot_id)
            .await
    }

    async fn info(&self, group: GroupChat) -> Result<ChatInfo, ApiError> {
        self.api.get_chat(Chat::Group(group)).await
    }

    async fn leave(&self, group: GroupChat) -> Result<(), ApiError> {
        self.api.leave_chat(Chat::Group(group)).await.map(drop)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn member(status: &str, topics: bool, delete: bool, is_member: bool) -> ChatMember {
        ChatMember {
            status: status.to_owned(),
            can_manage_topics: topics,
            can_delete_messages: delete,
            is_member,
            ..ChatMember::default()
        }
    }

    fn present(missing: &[Missing], can_delete: bool) -> Membership {
        Membership::Present {
            missing: missing.to_vec(),
            can_delete,
        }
    }

    #[test]
    fn the_bots_place_in_a_group() {
        use Missing::{Admin, ManageTopics, Topics};
        let cases = [
            (
                member("creator", false, false, false),
                true,
                true,
                present(&[], true),
            ),
            (
                member("administrator", true, true, false),
                true,
                true,
                present(&[], true),
            ),
            (
                member("administrator", false, true, false),
                true,
                true,
                present(&[ManageTopics], true),
            ),
            (
                member("administrator", true, false, false),
                true,
                true,
                present(&[], false),
            ),
            (
                member("member", false, false, false),
                true,
                true,
                present(&[Admin], false),
            ),
            (
                member("restricted", false, false, true),
                true,
                true,
                present(&[Admin], false),
            ),
            (
                member("restricted", false, false, false),
                true,
                true,
                Membership::Gone,
            ),
            (
                member("left", true, true, false),
                true,
                true,
                Membership::Gone,
            ),
            (
                member("kicked", true, true, false),
                true,
                true,
                Membership::Gone,
            ),
            (
                member("administrator", true, true, false),
                true,
                false,
                present(&[Topics], true),
            ),
            (
                member("member", false, false, false),
                false,
                false,
                present(&[Topics, Admin], false),
            ),
        ];
        for (member, supergroup, forum, want) in cases {
            assert_eq!(
                membership(supergroup, forum, &member),
                want,
                "{member:?} {supergroup} {forum}"
            );
        }
    }

    #[test]
    fn picker_data_fits_and_parses_back() {
        for action in [PickAction::Share, PickAction::Unshare, PickAction::Confirm] {
            let data = pick_data(u32::MAX, GroupChat::of(i64::MIN), action);
            assert!(data.len() <= 64, "{data}");
            assert_eq!(
                parse_pick(&data),
                Some((u32::MAX, GroupChat::of(i64::MIN), action))
            );
        }
        let data = pick_data(3, GroupChat::of(-1_001_234), PickAction::Share);
        assert_eq!(data, "grp:3:-1001234:s");
        for bad in [
            "grp:",
            "grp:3:-1001234:s:x",
            "grp:3:1001234:s",
            "grp:3:-1001234:x",
            "grp:x:-1001234:s",
            "grx:3:-1001234:s",
            "status:share",
            "menu:sh:0:1",
        ] {
            assert_eq!(parse_pick(bad), None, "{bad}");
        }
    }

    #[test]
    fn the_picker_has_a_button_per_group() {
        let long = "Очень длинное название группы, которое не влезет в кнопку";
        let rows = [
            PickRow {
                group: GroupChat::of(-1001),
                title: "Команда".into(),
                shared: false,
                confirm: false,
            },
            PickRow {
                group: GroupChat::of(-1002),
                title: long.into(),
                shared: true,
                confirm: false,
            },
            PickRow {
                group: GroupChat::of(-1003),
                title: "Три".into(),
                shared: true,
                confirm: true,
            },
        ];
        let (text, keyboard) = picker(7, &rows);
        assert_eq!(text, PICKER_TEXT);
        let buttons: Vec<(String, String)> = keyboard["inline_keyboard"]
            .as_array()
            .unwrap()
            .iter()
            .map(|row| {
                (
                    row[0]["text"].as_str().unwrap().to_owned(),
                    row[0]["callback_data"].as_str().unwrap().to_owned(),
                )
            })
            .collect();
        assert_eq!(buttons[0], ("👥 Команда".into(), "grp:7:-1001:s".into()));
        assert!(buttons[1].0.starts_with("🙈 ") && buttons[1].0.ends_with('…'));
        assert!(transcript::telegram_len(&buttons[1].0) <= TITLE_BUTTON_LIMIT + 3);
        assert_eq!(buttons[1].1, "grp:7:-1002:u");
        assert_eq!(
            buttons[2],
            ("🙈 Три — точно?".into(), "grp:7:-1003:c".into())
        );
    }

    #[test]
    fn a_not_ready_group_is_told_what_is_missing() {
        let all = [Missing::Topics, Missing::Admin, Missing::ManageTopics];
        let text = not_ready_notice(&all, false, Some("cctg_bot"));
        assert!(
            text.contains("включите темы в настройках группы;"),
            "{text}"
        );
        assert!(text.contains("сделайте бота администратором;"), "{text}");
        assert!(text.contains("\"Управление темами\""), "{text}");
        assert!(
            text.ends_with("напишите здесь /connect@cctg_bot."),
            "{text}"
        );
        assert!(!text.contains("супергруппой"), "{text}");
        let basic = not_ready_notice(&[Missing::Topics], true, None);
        assert!(basic.contains("супергруппой"), "{basic}");
        assert!(basic.ends_with("напишите здесь /connect."), "{basic}");
        // A bot in privacy mode sees only `/connect@<bot>` (TASK-069 review).
        let failed = connect_failed_notice(Some("cctg_bot"));
        assert!(
            failed.contains("Напишите /connect@cctg_bot ещё раз"),
            "{failed}"
        );
        assert!(connect_failed_notice(None).contains("Напишите /connect ещё раз"));
        for text in [
            text,
            basic,
            failed,
            GROUP_READY_NOTICE.to_owned(),
            GROUP_READY_NO_SHARE_NOTICE.to_owned(),
            NO_DELETE_LINE.to_owned(),
        ] {
            assert!(text.chars().count() < 300, "{text}");
        }
        assert_eq!(fallback_title(2), "группа 2");
    }

    #[test]
    fn known_groups_are_shared_between_clones() {
        let known = KnownGroups::of([GroupChat::of(-1001)]);
        let poll = known.clone();
        assert!(poll.contains(-1001) && !poll.contains(-1002));
        known.replace([GroupChat::of(-1002)]);
        assert!(!poll.contains(-1001) && poll.contains(-1002));
        assert_eq!(format!("{known:?}"), "KnownGroups(..)");
    }
}
