//! The menu in the General of a person's private chat with the bot (TASK-073).
//!
//! One message there, pinned, edited in place: its tabs show the person's
//! sessions (with buttons to lift a topic, share or unshare it, stop a turn
//! and update the clients), what the topics of the private chat show and
//! how loud they are. Settings belong to the person and live in
//! `registry.json` ([`Person`], by private chat). Only the person sees and
//! changes them: a private chat is its user's, and a press counts only in
//! the private chat it came from. The detail level, 💭 and the turn view
//! (TASK-076: compact puts the tool lines and 💭 of the turn message into a
//! collapsed quote that Telegram opens on a tap) apply per view (TASK-078):
//! the owner's private settings in their private chat, the owner's
//! [`Settings::group`] in the group topic of their sessions (default:
//! everything, 💭 on, full turn, rich messages on). Rich messages
//! (TASK-075: answers and turn text as Telegram rich markdown) are a setting
//! of each view too, on by default in both (the group since TASK-077; a
//! saved choice stays). The sound setting changes the private chat only;
//! the group sounds as before. The menu does not refresh itself: ↻ does.
//!
//! TASK-077: in the group topic of a shared session the agent answers only
//! mentions ([`crate::hub::mention`]); the session row switches a slot
//! between that and every message, and [`Settings::history`] is how much of
//! the group's history a mention takes along.
//!
//! TASK-081: an owner's menu has a «👥 Люди» tab: the members owners added
//! to the allowlist ([`super::people`]), a remove button each (asking once
//! more) and the invite link button.
//!
//! TASK-074: an owner's menu also has «💻 Устройства» (the hub's devices
//! with their owners, a revoke button each, asking once more, and the
//! install line of `/join`) and «🛠 Hub» (version, uptime, sessions, 429s
//! and the cleanup of the topics of long dead slots).
//!
//! This module is pure: data, rendering and the press codes. The slot actor
//! sends, pins and edits.

use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use super::chat::PrivateChat;
use super::devices::CODE_TTL;
use super::people;
use super::registry::cut;

/// Session rows per page of the sessions tab.
pub const PAGE_SIZE: usize = 6;
/// Ended slots listed at most, newest first, after the live ones.
pub const MAX_ENDED_ROWS: usize = 12;
/// A title in a session row, at most (UTF-16 units).
const TITLE_LIMIT: usize = 100;
/// Quiet hours when they are switched on.
const QUIET_DEFAULT: Quiet = Quiet { from: 23, to: 8 };
/// Time zones as minutes from UTC.
pub const ZONE_MIN: i16 = -720;
pub const ZONE_MAX: i16 = 840;
/// Zones with :30 and :45 offered on request.
const FRACTION_ZONES: [i16; 12] = [-570, -210, 210, 270, 330, 345, 390, 525, 570, 630, 765, 825];
const PREFIX: &str = "menu:";

pub const ANSWER_SAVED: &str = "Сохранено";
pub const ANSWER_UNCHANGED: &str = "Без изменений";
pub const ANSWER_ZONE_FIRST: &str = "Сначала укажите часовой пояс";
pub const ANSWER_NOT_YOURS: &str = "Это не ваша сессия";
pub const ANSWER_PRIVATE_ONLY: &str = "Меню работает в личке с ботом";
pub const ANSWER_ALL_CURRENT: &str = "Все клиенты уже обновлены";
pub const ANSWER_STALE_MENU: &str = "Это меню устарело: /menu";
/// The answer to a member's press of an owner's tab (TASK-074).
pub const ANSWER_OWNER_ONLY: &str = "Это может только владелец";

/// The ages a cleanup offers, in days (TASK-074); 0 is «dead now».
pub const CLEANUP_DAYS: [u16; 5] = [0, 1, 7, 30, 90];
pub const DEFAULT_CLEANUP_DAYS: u16 = 30;

/// The answer to ↗: the topic of `title` moved to the top of the list.
pub fn lifted(title: &str) -> String {
    format!("Тема поднята наверх: «{}»", cut(title, TITLE_LIMIT))
}

/// The answer to «Обновить все клиенты» when `n` sessions were asked.
pub fn updates_asked(n: usize) -> String {
    format!("Обновление запрошено: сессий {n}")
}

/// One person's menu and settings, by their private chat.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Person {
    pub chat: PrivateChat,
    /// The current menu message in the General of the private chat.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub menu: Option<i64>,
    /// No menu comes by itself, only on `/menu`: the person deleted theirs,
    /// or Telegram refused it. Without it a person with no menu gets one.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub on_request: bool,
    #[serde(default)]
    pub settings: Settings,
}

impl Person {
    pub fn new(chat: PrivateChat) -> Self {
        Self {
            chat,
            menu: None,
            on_request: false,
            settings: Settings::default(),
        }
    }
}

/// What the topics of a person's private chat show and how loud they are.
/// A missing field reads as its default: today's behaviour.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct Settings {
    pub detail: Detail,
    /// 💭 thinking in the topics, at any detail level.
    pub thinking: bool,
    pub turn: TurnView,
    pub sound: Sound,
    #[serde(
        skip_serializing_if = "Option::is_none",
        deserialize_with = "hours_of_a_day"
    )]
    pub quiet: Option<Quiet>,
    /// Minutes from UTC, [`ZONE_MIN`]..=[`ZONE_MAX`]; no daylight saving.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tz: Option<i16>,
    /// What the group topics of the person's sessions show (TASK-078);
    /// written only when it is not the default.
    #[serde(skip_serializing_if = "Display::is_default")]
    pub group: Display,
    /// Rich messages in the person's private topics (TASK-075); on by
    /// default.
    pub rich: bool,
    /// How much of the group topic's history a mention takes along to the
    /// person's shared sessions (TASK-077); written only when not the
    /// default.
    #[serde(skip_serializing_if = "HistoryLimit::is_default")]
    pub history: HistoryLimit,
}

impl Settings {
    /// What the person's private topics show.
    pub fn display(&self) -> Display {
        Display {
            detail: self.detail,
            thinking: self.thinking,
            turn: self.turn,
            rich: self.rich,
        }
    }
}

/// What the topics of one view show (TASK-078): the stream's detail level,
/// 💭 and the turn view. A missing field reads as its default: everything.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct Display {
    pub detail: Detail,
    pub thinking: bool,
    pub turn: TurnView,
    /// Agent text as rich messages (TASK-075); on by default (the group
    /// view since TASK-077).
    pub rich: bool,
}

impl Default for Display {
    fn default() -> Self {
        Self {
            detail: Detail::Full,
            thinking: true,
            turn: TurnView::Full,
            rich: true,
        }
    }
}

impl Display {
    pub fn is_default(&self) -> bool {
        *self == Self::default()
    }
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            detail: Detail::Full,
            thinking: true,
            turn: TurnView::Full,
            sound: Sound::Replies,
            quiet: None,
            tz: None,
            group: Display::default(),
            rich: true,
            history: HistoryLimit::default(),
        }
    }
}

/// Characters of the group history a mention takes along (TASK-077); a
/// longer one is compressed on the session's device, or cut. Default last,
/// as [`Detail`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum HistoryLimit {
    Short,
    Long,
    #[default]
    #[serde(other)]
    Medium,
}

impl HistoryLimit {
    pub const ALL: [Self; 3] = [Self::Short, Self::Medium, Self::Long];

    pub fn chars(self) -> u32 {
        match self {
            Self::Short => 2000,
            Self::Medium => 4000,
            Self::Long => 8000,
        }
    }

    pub fn is_default(&self) -> bool {
        *self == Self::default()
    }

    fn code(self) -> &'static str {
        match self {
            Self::Short => "s",
            Self::Medium => "m",
            Self::Long => "l",
        }
    }
}

/// How much of a turn the stream shows. The default variant is the last
/// one: `#[serde(other)]` must be there, and a level written by a later hub
/// reads as the default instead of stopping this one.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Detail {
    /// «Кратко»: no tool lines.
    Brief,
    /// «Только ответы»: terminal prompts, then the turn's answer.
    Answers,
    /// «Всё».
    #[default]
    #[serde(other)]
    Full,
}

/// How the turn message shows its tool lines and 💭 (TASK-076). Default
/// last, as [`Detail`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TurnView {
    /// «Сжатый»: each run of them in a collapsed quote
    /// (`<blockquote expandable>`) that the reader opens with a tap.
    Compact,
    /// «Полный»: as lines of the message.
    #[default]
    #[serde(other)]
    Full,
}

/// Which new messages of the private chat ring. Default last, as [`Detail`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Sound {
    /// «Всё»: every new message.
    All,
    /// «Ничего»: none, permission prompts and questions too.
    Off,
    /// «Ответы и запросы»: what rings today (turn answers, prompts,
    /// questions, the outdated-client warning).
    #[default]
    #[serde(other)]
    Replies,
}

/// Quiet hours `[from, to)` in the person's time zone, across midnight when
/// `from > to`; `from == to` is empty.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct Quiet {
    pub from: u8,
    pub to: u8,
}

/// Quiet hours as `registry.json` has them; an hour past 23 (only a hand
/// edit writes one) reads as no quiet hours.
fn hours_of_a_day<'de, D: serde::Deserializer<'de>>(
    deserializer: D,
) -> Result<Option<Quiet>, D::Error> {
    let quiet = Option::<Quiet>::deserialize(deserializer)?;
    Ok(quiet.filter(|quiet| quiet.from < 24 && quiet.to < 24))
}

/// What a stream message is.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Piece {
    /// A prompt typed in the terminal.
    Prompt,
    /// Assistant text.
    Text,
    Thinking,
    /// A finished tool call.
    Tool,
    /// Claude Code's own note that the user interrupted the turn.
    Interrupt,
}

/// `piece` shows in the topics of a view with `display`. Turn answers,
/// subagent blocks, prompts, questions and notices are no pieces: they
/// always show.
pub fn shows(display: &Display, piece: Piece) -> bool {
    match piece {
        Piece::Prompt | Piece::Interrupt => true,
        Piece::Thinking => display.thinking,
        Piece::Text => display.detail != Detail::Answers,
        Piece::Tool => display.detail == Detail::Full,
    }
}

/// It is within the quiet hours at `unix_secs`; never without a time zone.
pub fn quiet_now(settings: &Settings, unix_secs: i64) -> bool {
    let (Some(quiet), Some(tz)) = (settings.quiet, settings.tz) else {
        return false;
    };
    let minute = local_minute(unix_secs, tz);
    let (from, to) = (i64::from(quiet.from) * 60, i64::from(quiet.to) * 60);
    match from.cmp(&to) {
        std::cmp::Ordering::Less => from <= minute && minute < to,
        std::cmp::Ordering::Greater => minute >= from || minute < to,
        std::cmp::Ordering::Equal => false,
    }
}

/// The minute of the day in zone `tz` at `unix_secs`.
fn local_minute(unix_secs: i64, tz: i16) -> i64 {
    (unix_secs.div_euclid(60) + i64::from(tz)).rem_euclid(1440)
}

/// A new message into the person's private chat rings. `ringing`: it would
/// today; `asks`: a permission prompt or a question (they hold the session
/// until answered); `counts`: what the session says (its stream, answers,
/// replies, prompts, questions, blocks, files), not a notice, an echo, a
/// status message or the menu.
pub fn loud(settings: &Settings, ringing: bool, asks: bool, counts: bool, unix_secs: i64) -> bool {
    match settings.sound {
        Sound::Off => false,
        _ if quiet_now(settings, unix_secs) => ringing && asks,
        Sound::Replies => ringing,
        Sound::All => ringing || counts,
    }
}

/// What a press of a setting makes of `settings`: the settings after it and
/// the page to show then; `Err`: refused, with the page and the answer.
/// Quiet hours come on (23 to 08) only once the time zone is known: the
/// zone page is shown first. A press that is no setting changes nothing.
pub fn change(
    settings: &Settings,
    press: MenuPress,
) -> Result<(Settings, Page), (Page, &'static str)> {
    let mut new = settings.clone();
    let page = match press {
        MenuPress::Detail(detail) => {
            new.detail = detail;
            Page::Display
        }
        MenuPress::Thinking(on) => {
            new.thinking = on;
            Page::Display
        }
        MenuPress::TurnView(turn) => {
            new.turn = turn;
            Page::Display
        }
        MenuPress::Rich(on) => {
            new.rich = on;
            Page::Display
        }
        MenuPress::GroupDetail(detail) => {
            new.group.detail = detail;
            Page::GroupDisplay
        }
        MenuPress::GroupThinking(on) => {
            new.group.thinking = on;
            Page::GroupDisplay
        }
        MenuPress::GroupTurn(turn) => {
            new.group.turn = turn;
            Page::GroupDisplay
        }
        MenuPress::GroupRich(on) => {
            new.group.rich = on;
            Page::GroupDisplay
        }
        MenuPress::GroupHistory(limit) => {
            new.history = limit;
            Page::GroupDisplay
        }
        MenuPress::SoundMode(sound) => {
            new.sound = sound;
            Page::Sound
        }
        MenuPress::Quiet(false) => {
            new.quiet = None;
            Page::Sound
        }
        MenuPress::Quiet(true) if new.quiet.is_some() => Page::Sound,
        MenuPress::Quiet(true) if new.tz.is_none() => {
            return Err((Page::Zone { fractions: false }, ANSWER_ZONE_FIRST));
        }
        MenuPress::Quiet(true) => {
            new.quiet = Some(QUIET_DEFAULT);
            Page::Sound
        }
        MenuPress::QuietFrom(hour) | MenuPress::QuietTo(hour) => {
            let Some(quiet) = new.quiet.as_mut().filter(|_| hour < 24) else {
                return Err((Page::Sound, ANSWER_UNCHANGED));
            };
            if matches!(press, MenuPress::QuietFrom(_)) {
                quiet.from = hour;
            } else {
                quiet.to = hour;
            }
            Page::Sound
        }
        MenuPress::SetZone(tz) if (ZONE_MIN..=ZONE_MAX).contains(&tz) => {
            new.tz = Some(tz);
            Page::Sound
        }
        MenuPress::SetZone(_) => return Err((Page::Sound, ANSWER_UNCHANGED)),
        MenuPress::Sessions { .. }
        | MenuPress::Display
        | MenuPress::GroupDisplay
        | MenuPress::Sound
        | MenuPress::Zone { .. }
        | MenuPress::Slot { .. }
        | MenuPress::UpdateAll { .. }
        | MenuPress::People
        | MenuPress::Remove { .. }
        | MenuPress::RemoveConfirm { .. }
        | MenuPress::Invite
        | MenuPress::AddYes { .. }
        | MenuPress::AddNo { .. }
        | MenuPress::Devices
        | MenuPress::DeviceRevoke { .. }
        | MenuPress::DeviceRevokeConfirm { .. }
        | MenuPress::DeviceAdd
        | MenuPress::Hub
        | MenuPress::CleanupDays(_)
        | MenuPress::Cleanup { .. }
        | MenuPress::CleanupConfirm { .. } => return Err((Page::Sessions(0), ANSWER_UNCHANGED)),
    };
    Ok((new, page))
}

/// A button of a session row.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SlotAction {
    /// ↗: lift the slot's topic.
    Open,
    Share,
    Unshare,
    UnshareConfirm,
    Stop,
    StopConfirm,
    /// 💬: the group topic of the shared slot takes mentions only
    /// (TASK-077).
    Mentions,
    /// 📣: it takes every message.
    EveryMessage,
    /// 👥 n…: the group picker in the slot's topic (TASK-069).
    Groups,
}

impl SlotAction {
    const ALL: [Self; 9] = [
        Self::Open,
        Self::Share,
        Self::Unshare,
        Self::UnshareConfirm,
        Self::Stop,
        Self::StopConfirm,
        Self::Mentions,
        Self::EveryMessage,
        Self::Groups,
    ];

    fn code(self) -> &'static str {
        match self {
            Self::Open => "o",
            Self::Share => "sh",
            Self::Unshare => "us",
            Self::UnshareConfirm => "uc",
            Self::Stop => "st",
            Self::StopConfirm => "sc",
            Self::Mentions => "mn",
            Self::EveryMessage => "ma",
            Self::Groups => "gp",
        }
    }
}

/// A press on the menu; `page` is the sessions page to show after it,
/// `slot` the registry index of the slot.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MenuPress {
    Sessions {
        page: u32,
    },
    Display,
    /// The display tab's page of the group (TASK-078).
    GroupDisplay,
    Sound,
    Zone {
        fractions: bool,
    },
    Slot {
        action: SlotAction,
        page: u32,
        slot: u32,
    },
    UpdateAll {
        page: u32,
    },
    Detail(Detail),
    /// 💭 on or off: the value the button sets, so a double tap sets it
    /// twice instead of undoing itself.
    Thinking(bool),
    TurnView(TurnView),
    /// Rich messages on or off (TASK-075), as [`MenuPress::Thinking`].
    Rich(bool),
    /// The group's detail level, 💭 and turn view (TASK-078), as the
    /// private ones.
    GroupDetail(Detail),
    GroupThinking(bool),
    GroupTurn(TurnView),
    GroupRich(bool),
    /// The group history a mention takes along (TASK-077).
    GroupHistory(HistoryLimit),
    SoundMode(Sound),
    /// Quiet hours on or off, as [`MenuPress::Thinking`].
    Quiet(bool),
    QuietFrom(u8),
    QuietTo(u8),
    SetZone(i16),
    /// The people tab (TASK-081), an owner's only.
    People,
    /// 🗑 of member `key`: asks once more.
    Remove {
        key: u32,
    },
    /// «точно?» of member `key`: removes them.
    RemoveConfirm {
        key: u32,
    },
    /// A new invite link.
    Invite,
    /// «Добавить» of proposal `token`, on its own message.
    AddYes {
        token: u32,
    },
    /// «Отмена» of proposal `token`.
    AddNo {
        token: u32,
    },
    /// The devices tab (TASK-074), an owner's only.
    Devices,
    /// 🗑 of device `id` (its 8 hex digits as a number): asks once more.
    DeviceRevoke {
        id: u32,
    },
    /// «точно?» of device `id`: revokes it.
    DeviceRevokeConfirm {
        id: u32,
    },
    /// ➕: the install line of `/join`, as a message of its own.
    DeviceAdd,
    /// The hub tab (TASK-074), an owner's only.
    Hub,
    /// The age of a cleanup, one of [`CLEANUP_DAYS`].
    CleanupDays(u16),
    /// 🧹 of a cleanup of `days` that counted `topics` of the slots whose
    /// [`cleanup_key`] is `slots`: asks once more.
    Cleanup {
        days: u16,
        topics: u32,
        slots: u32,
    },
    /// «точно?» of that cleanup: it starts, when it still takes those
    /// slots and topics.
    CleanupConfirm {
        days: u16,
        topics: u32,
        slots: u32,
    },
}

/// The key of the slots a cleanup takes (TASK-074 review): FNV-1a of their
/// registry indices, so a confirmation acts only on the slots it counted.
pub fn cleanup_key(slots: impl IntoIterator<Item = usize>) -> u32 {
    let mut hash: u32 = 0x811c_9dc5;
    for slot in slots {
        for byte in (slot as u64).to_le_bytes() {
            hash ^= u32::from(byte);
            hash = hash.wrapping_mul(0x0100_0193);
        }
    }
    hash
}

impl MenuPress {
    /// Only shows another page.
    pub fn navigates(self) -> bool {
        matches!(
            self,
            Self::Sessions { .. }
                | Self::Display
                | Self::GroupDisplay
                | Self::Sound
                | Self::Zone { .. }
                | Self::People
                | Self::Devices
                | Self::Hub
                | Self::CleanupDays(_)
        )
    }

    /// A press of an owner's tab (TASK-081 people, TASK-074 devices and
    /// hub): an owner's only.
    pub fn owners_only(self) -> bool {
        self.manages_people()
            || matches!(
                self,
                Self::Devices
                    | Self::DeviceRevoke { .. }
                    | Self::DeviceRevokeConfirm { .. }
                    | Self::DeviceAdd
                    | Self::Hub
                    | Self::CleanupDays(_)
                    | Self::Cleanup { .. }
                    | Self::CleanupConfirm { .. }
            )
    }

    /// A press about the members (TASK-081): an owner's only.
    pub fn manages_people(self) -> bool {
        matches!(
            self,
            Self::People
                | Self::Remove { .. }
                | Self::RemoveConfirm { .. }
                | Self::Invite
                | Self::AddYes { .. }
                | Self::AddNo { .. }
        )
    }
}

fn detail_code(detail: Detail) -> &'static str {
    match detail {
        Detail::Full => "f",
        Detail::Brief => "b",
        Detail::Answers => "a",
    }
}

fn turn_code(turn: TurnView) -> &'static str {
    match turn {
        TurnView::Full => "f",
        TurnView::Compact => "c",
    }
}

fn sound_code(sound: Sound) -> &'static str {
    match sound {
        Sound::Replies => "r",
        Sound::All => "a",
        Sound::Off => "o",
    }
}

/// The callback data of `press` (ASCII, at most 64 bytes).
pub fn data(press: &MenuPress) -> String {
    let rest = match *press {
        MenuPress::Sessions { page } => format!("s:{page}"),
        MenuPress::Display => "d".to_owned(),
        MenuPress::GroupDisplay => "dg".to_owned(),
        MenuPress::Sound => "n".to_owned(),
        MenuPress::Zone { fractions } => format!("z:{}", u8::from(fractions)),
        MenuPress::Slot { action, page, slot } => format!("{}:{page}:{slot}", action.code()),
        MenuPress::UpdateAll { page } => format!("up:{page}"),
        MenuPress::Detail(detail) => format!("dl:{}", detail_code(detail)),
        MenuPress::Thinking(on) => format!("th:{}", u8::from(on)),
        MenuPress::TurnView(turn) => format!("tv:{}", turn_code(turn)),
        MenuPress::Rich(on) => format!("rc:{}", u8::from(on)),
        MenuPress::GroupDetail(detail) => format!("gdl:{}", detail_code(detail)),
        MenuPress::GroupThinking(on) => format!("gth:{}", u8::from(on)),
        MenuPress::GroupTurn(turn) => format!("gtv:{}", turn_code(turn)),
        MenuPress::GroupRich(on) => format!("grc:{}", u8::from(on)),
        MenuPress::GroupHistory(limit) => format!("ghl:{}", limit.code()),
        MenuPress::SoundMode(sound) => format!("sn:{}", sound_code(sound)),
        MenuPress::Quiet(on) => format!("q:{}", u8::from(on)),
        MenuPress::QuietFrom(hour) => format!("qf:{hour}"),
        MenuPress::QuietTo(hour) => format!("qt:{hour}"),
        MenuPress::SetZone(tz) => format!("tz:{tz}"),
        MenuPress::People => "pp".to_owned(),
        MenuPress::Remove { key } => format!("pr:{key}"),
        MenuPress::RemoveConfirm { key } => format!("prc:{key}"),
        MenuPress::Invite => "pi".to_owned(),
        MenuPress::AddYes { token } => format!("pa:{token}"),
        MenuPress::AddNo { token } => format!("pn:{token}"),
        MenuPress::Devices => "dv".to_owned(),
        MenuPress::DeviceRevoke { id } => format!("dr:{id:08x}"),
        MenuPress::DeviceRevokeConfirm { id } => format!("drc:{id:08x}"),
        MenuPress::DeviceAdd => "da".to_owned(),
        MenuPress::Hub => "hb".to_owned(),
        MenuPress::CleanupDays(days) => format!("hd:{days}"),
        MenuPress::Cleanup {
            days,
            topics,
            slots,
        } => format!("hx:{days}:{topics}:{slots}"),
        MenuPress::CleanupConfirm {
            days,
            topics,
            slots,
        } => format!("hxc:{days}:{topics}:{slots}"),
    };
    format!("{PREFIX}{rest}")
}

/// A device id of callback data: exactly 8 lowercase hex digits, as
/// `devices.json` has them.
fn device_number(text: &str) -> Option<u32> {
    let hex = text.len() == 8
        && text
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte));
    hex.then(|| u32::from_str_radix(text, 16).ok()).flatten()
}

/// A cleanup age of callback data: one of [`CLEANUP_DAYS`].
fn cleanup_days(text: &str) -> Option<u16> {
    text.parse().ok().filter(|days| CLEANUP_DAYS.contains(days))
}

/// The press of callback data `data`; `None` when it is no menu press.
pub fn parse_callback(data: &str) -> Option<MenuPress> {
    let rest = data.strip_prefix(PREFIX)?;
    let parts: Vec<&str> = rest.split(':').collect();
    let number = |text: &str| text.parse::<u32>().ok();
    Some(match parts.as_slice() {
        ["s", page] => MenuPress::Sessions {
            page: number(page)?,
        },
        ["d"] => MenuPress::Display,
        ["dg"] => MenuPress::GroupDisplay,
        ["n"] => MenuPress::Sound,
        ["z", "0"] => MenuPress::Zone { fractions: false },
        ["z", "1"] => MenuPress::Zone { fractions: true },
        ["hx", days, topics, slots] => MenuPress::Cleanup {
            days: cleanup_days(days)?,
            topics: number(topics)?,
            slots: number(slots)?,
        },
        ["hxc", days, topics, slots] => MenuPress::CleanupConfirm {
            days: cleanup_days(days)?,
            topics: number(topics)?,
            slots: number(slots)?,
        },
        [code, page, slot] => MenuPress::Slot {
            action: SlotAction::ALL
                .into_iter()
                .find(|action| action.code() == *code)?,
            page: number(page)?,
            slot: number(slot)?,
        },
        ["up", page] => MenuPress::UpdateAll {
            page: number(page)?,
        },
        ["dl", code] => MenuPress::Detail(
            [Detail::Full, Detail::Brief, Detail::Answers]
                .into_iter()
                .find(|detail| detail_code(*detail) == *code)?,
        ),
        ["th", "0"] => MenuPress::Thinking(false),
        ["th", "1"] => MenuPress::Thinking(true),
        ["rc", "0"] => MenuPress::Rich(false),
        ["rc", "1"] => MenuPress::Rich(true),
        ["tv", code] => MenuPress::TurnView(
            [TurnView::Full, TurnView::Compact]
                .into_iter()
                .find(|turn| turn_code(*turn) == *code)?,
        ),
        ["gdl", code] => MenuPress::GroupDetail(
            [Detail::Full, Detail::Brief, Detail::Answers]
                .into_iter()
                .find(|detail| detail_code(*detail) == *code)?,
        ),
        ["gth", "0"] => MenuPress::GroupThinking(false),
        ["gth", "1"] => MenuPress::GroupThinking(true),
        ["grc", "0"] => MenuPress::GroupRich(false),
        ["grc", "1"] => MenuPress::GroupRich(true),
        ["ghl", code] => MenuPress::GroupHistory(
            HistoryLimit::ALL
                .into_iter()
                .find(|limit| limit.code() == *code)?,
        ),
        ["gtv", code] => MenuPress::GroupTurn(
            [TurnView::Full, TurnView::Compact]
                .into_iter()
                .find(|turn| turn_code(*turn) == *code)?,
        ),
        ["sn", code] => MenuPress::SoundMode(
            [Sound::Replies, Sound::All, Sound::Off]
                .into_iter()
                .find(|sound| sound_code(*sound) == *code)?,
        ),
        ["q", "0"] => MenuPress::Quiet(false),
        ["q", "1"] => MenuPress::Quiet(true),
        ["qf", hour] => MenuPress::QuietFrom(hour.parse().ok()?),
        ["qt", hour] => MenuPress::QuietTo(hour.parse().ok()?),
        ["tz", minutes] => MenuPress::SetZone(minutes.parse().ok()?),
        ["pp"] => MenuPress::People,
        ["pr", key] => MenuPress::Remove { key: number(key)? },
        ["prc", key] => MenuPress::RemoveConfirm { key: number(key)? },
        ["pi"] => MenuPress::Invite,
        ["pa", token] => MenuPress::AddYes {
            token: number(token)?,
        },
        ["pn", token] => MenuPress::AddNo {
            token: number(token)?,
        },
        ["dv"] => MenuPress::Devices,
        ["dr", id] => MenuPress::DeviceRevoke {
            id: device_number(id)?,
        },
        ["drc", id] => MenuPress::DeviceRevokeConfirm {
            id: device_number(id)?,
        },
        ["da"] => MenuPress::DeviceAdd,
        ["hb"] => MenuPress::Hub,
        ["hd", days] => MenuPress::CleanupDays(cleanup_days(days)?),
        _ => return None,
    })
}

/// What a session row says of its slot's current session.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RowState {
    Working,
    Idle,
    Asking,
    NoChannel,
    Ended,
}

impl RowState {
    fn icon_and_text(self) -> (&'static str, &'static str) {
        match self {
            Self::Working => ("⚡", "работает"),
            Self::Idle => ("💬", "ждёт сообщения"),
            Self::Asking => ("❓", "ждёт ответа"),
            Self::NoChannel => ("👀", "без канала"),
            Self::Ended => ("🏁", "завершена"),
        }
    }
}

/// One row of the sessions tab.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SessionRow {
    pub slot: u32,
    pub title: String,
    pub state: RowState,
    /// Shared to the group; `None`: no share button (the slot is not
    /// answered in the private chat).
    pub shared: Option<bool>,
    /// The unshare button asks for its second press.
    pub share_confirm: bool,
    /// ⏹, asking for its second press when `true`; `None`: nothing to stop.
    pub stop: Option<bool>,
    /// The group topic takes mentions only (`true`) or every message; `None`
    /// unless `shared` is `Some(true)` (TASK-077).
    pub mentions: Option<bool>,
    /// The slot has two or more groups to choose from (TASK-069): one
    /// button opens the group picker instead of 👥 and 🙈.
    pub pick: bool,
    /// The titles of the groups it shows in (TASK-069), when `pick`.
    pub groups: Vec<String>,
}

/// Group titles a session row names at most (TASK-069).
const ROW_GROUPS: usize = 3;

/// Where a shared row shows: `, в группе` with one group, else the titles
/// of its groups (at most [`ROW_GROUPS`], each cut).
fn groups_text(row: &SessionRow) -> String {
    if !row.pick {
        return ", в группе".to_owned();
    }
    let titles: Vec<String> = row
        .groups
        .iter()
        .take(ROW_GROUPS)
        .map(|title| cut(title, super::groups::TITLE_BUTTON_LIMIT))
        .collect();
    match row.groups.len() {
        0 => ", в группе".to_owned(),
        1 => format!(", в группе «{}»", titles[0]),
        n if n <= ROW_GROUPS => format!(", в группах: {}", titles.join(", ")),
        n => format!(
            ", в группах: {} и ещё {}",
            titles.join(", "),
            n - ROW_GROUPS
        ),
    }
}

/// One page of the sessions tab.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SessionsView {
    pub rows: Vec<SessionRow>,
    /// This page, `0..pages`.
    pub page: usize,
    /// At least 1.
    pub pages: usize,
    /// Live sessions of the person whose client is outdated.
    pub outdated: usize,
}

/// A page of the menu.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Page {
    Sessions(usize),
    Display,
    /// The display tab for the group (TASK-078).
    GroupDisplay,
    Sound,
    Zone {
        fractions: bool,
    },
    /// The people tab (TASK-081).
    People,
    /// The devices tab (TASK-074).
    Devices,
    /// The hub tab (TASK-074).
    Hub,
}

/// A device in the devices tab (TASK-074).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DeviceRow {
    /// Its 8 hex digits as a number.
    pub id: u32,
    pub name: String,
    /// Who owns it, as words («ваше», a member's name, ...).
    pub owner: String,
    /// `YYYY-MM-DD` of its join.
    pub joined: String,
    /// When its secret was last taken, as words.
    pub seen: String,
    /// Its 🗑 asks for its second press.
    pub confirm: bool,
}

/// The devices tab of an owner (TASK-074).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DevicesView {
    pub rows: Vec<DeviceRow>,
    /// What `/devices` says about the shared secret.
    pub shared: String,
}

/// The hub tab of an owner (TASK-074).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HubView {
    /// The release tag, or words for a local build.
    pub release: String,
    /// The short build id.
    pub build: String,
    /// When the hub started, UTC.
    pub started: String,
    pub uptime: String,
    /// Slots with a live session and dead ones not archived.
    pub live: usize,
    pub ended: usize,
    /// Topics of every slot's views.
    pub topics: usize,
    pub devices: usize,
    /// 429 answers of the last day, or since the start when `!floods_day`.
    pub floods: usize,
    pub floods_day: bool,
    /// The chosen age of the cleanup, one of [`CLEANUP_DAYS`].
    pub days: u16,
    /// What that cleanup would take: slots, their [`cleanup_key`], topics
    /// to delete, and topics the bot cannot delete, which stay.
    pub slots: usize,
    pub key: u32,
    pub clean: u32,
    pub kept: usize,
    /// Its 🧹 asks for its second press.
    pub armed: bool,
    /// Topics a cleanup still has to delete, and those it deleted or could
    /// not delete since the hub started.
    pub left: usize,
    pub done: usize,
    pub failed: usize,
}

/// The devices or hub tab of an owner (TASK-074).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum OwnerPage {
    Devices(DevicesView),
    Hub(HubView),
}

/// The people tab of an owner (TASK-081).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PeopleView {
    /// Owners of `CCTG_ALLOWED_USER_IDS`.
    pub owners: usize,
    /// The members, in the order they were added.
    pub members: Vec<PersonRow>,
    /// The bot has a username: an invite link can be made.
    pub invite: bool,
}

/// A member in the people tab.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PersonRow {
    pub key: u32,
    pub name: String,
    pub username: Option<String>,
    /// Their 🗑 asks for its second press.
    pub confirm: bool,
}

/// A member's name on a button, at most (UTF-16 units).
const PERSON_BUTTON_LIMIT: usize = 40;

/// The buttons of a proposal (TASK-081): «Добавить» and «Отмена».
pub fn add_keyboard(token: u32) -> Value {
    json!({ "inline_keyboard": [[
        button("✅ Добавить", MenuPress::AddYes { token }),
        button("✖ Отмена", MenuPress::AddNo { token }),
    ]] })
}

/// The invite link button alone (TASK-081).
pub fn invite_keyboard() -> Value {
    json!({ "inline_keyboard": [[button(people::INVITE_BUTTON, MenuPress::Invite)]] })
}

fn button(text: impl Into<String>, press: MenuPress) -> Value {
    json!({ "text": text.into(), "callback_data": data(&press) })
}

/// A page number as the press carries it.
fn page_number(page: usize) -> u32 {
    u32::try_from(page).unwrap_or(u32::MAX)
}

/// `✅ label` for the chosen one of a radio row.
fn radio(label: &str, chosen: bool) -> String {
    if chosen {
        format!("✅ {label}")
    } else {
        label.to_owned()
    }
}

/// `+3`, `−9:30`, `+0`.
fn offset_label(tz: i16) -> String {
    let sign = if tz < 0 { "−" } else { "+" };
    let minutes = tz.unsigned_abs();
    match minutes % 60 {
        0 => format!("{sign}{}", minutes / 60),
        rest => format!("{sign}{}:{rest:02}", minutes / 60),
    }
}

/// `UTC+3`, `UTC+5:45`.
pub fn zone_label(tz: i16) -> String {
    format!("UTC{}", offset_label(tz))
}

/// `HH:MM` in zone `tz` at `unix_secs`.
fn clock(unix_secs: i64, tz: i16) -> String {
    let minute = local_minute(unix_secs, tz);
    format!("{:02}:{:02}", minute / 60, minute % 60)
}

/// The text and keyboard of `page` for a person with `settings`;
/// `sessions` is the sessions tab's page (`None` shows it empty); `people`
/// the people tab of an owner (`None`: no owner's tabs, and their pages
/// show the sessions tab); `owner_page` the devices or hub tab of an owner
/// (TASK-074; missing for its page: the sessions tab).
pub fn render(
    page: &Page,
    settings: &Settings,
    sessions: Option<&SessionsView>,
    people: Option<&PeopleView>,
    owner_page: Option<&OwnerPage>,
    unix_secs: i64,
) -> (String, Value) {
    let owner_page = owner_page.filter(|_| people.is_some());
    let page = match (page, people, owner_page) {
        (Page::People, None, _)
        | (Page::Devices, _, None | Some(OwnerPage::Hub(_)))
        | (Page::Hub, _, None | Some(OwnerPage::Devices(_))) => &Page::Sessions(0),
        _ => page,
    };
    let tab = |label: &str, current: bool, press: MenuPress| {
        let text = if current {
            format!("· {label}")
        } else {
            label.to_owned()
        };
        button(text, press)
    };
    let mut rows = vec![vec![
        tab(
            "🗂 Сессии",
            matches!(page, Page::Sessions(_)),
            MenuPress::Sessions { page: 0 },
        ),
        tab(
            "👁 Показ",
            matches!(page, Page::Display | Page::GroupDisplay),
            MenuPress::Display,
        ),
        tab(
            "🔔 Звук",
            matches!(page, Page::Sound | Page::Zone { .. }),
            MenuPress::Sound,
        ),
    ]];
    // The owner's tabs, a row of their own (TASK-074).
    if people.is_some() {
        rows.push(vec![
            tab("👥 Люди", matches!(page, Page::People), MenuPress::People),
            tab(
                "💻 Устройства",
                matches!(page, Page::Devices),
                MenuPress::Devices,
            ),
            tab("🛠 Hub", matches!(page, Page::Hub), MenuPress::Hub),
        ]);
    }
    let text = match (page, people, owner_page) {
        (Page::People, Some(view), _) => render_people(view, &mut rows),
        (Page::Devices, _, Some(OwnerPage::Devices(view))) => render_devices(view, &mut rows),
        (Page::Hub, _, Some(OwnerPage::Hub(view))) => render_hub(view, &mut rows),
        (Page::Display, ..) => render_display(settings, &mut rows),
        (Page::GroupDisplay, ..) => render_group_display(settings, &mut rows),
        (Page::Sound, ..) => render_sound(settings, &mut rows),
        (Page::Zone { fractions }, ..) => render_zone(*fractions, unix_secs, &mut rows),
        (Page::Sessions(_) | Page::People | Page::Devices | Page::Hub, ..) => {
            render_sessions(sessions, &mut rows)
        }
    };
    (
        cut(&text, transcript::TELEGRAM_TEXT_LIMIT),
        json!({ "inline_keyboard": rows }),
    )
}

fn render_sessions(sessions: Option<&SessionsView>, rows: &mut Vec<Vec<Value>>) -> String {
    let empty = SessionsView {
        rows: Vec::new(),
        page: 0,
        pages: 1,
        outdated: 0,
    };
    let view = sessions.unwrap_or(&empty);
    let page = page_number(view.page);
    let mut text = format!(
        "Сессии (стр. {}/{})\nСессии ваших устройств приходят сюда, каждая в своей теме; пишите в тему сессии.\n",
        view.page + 1,
        view.pages.max(1)
    );
    if view.rows.is_empty() {
        text.push_str("\nСессий пока нет.");
    }
    for (index, row) in view.rows.iter().enumerate() {
        let n = index + 1;
        let (icon, state) = row.state.icon_and_text();
        let group = match (row.shared, row.mentions) {
            (Some(true), Some(true)) => format!("{}, по упоминанию", groups_text(row)),
            (Some(true), Some(false)) => format!("{}, все сообщения", groups_text(row)),
            (Some(true), None) => groups_text(row),
            _ => String::new(),
        };
        text.push_str(&format!(
            "\n{n}. {icon} {} — {state}{group}",
            cut(&row.title, TITLE_LIMIT)
        ));
        let press = |action| MenuPress::Slot {
            action,
            page,
            slot: row.slot,
        };
        let mut buttons = vec![button(format!("↗ {n}"), press(SlotAction::Open))];
        match (row.shared, row.share_confirm) {
            (Some(_), _) if row.pick => {
                buttons.push(button(format!("👥 {n}…"), press(SlotAction::Groups)));
            }
            (Some(false), _) => buttons.push(button(format!("👥 {n}"), press(SlotAction::Share))),
            (Some(true), false) => {
                buttons.push(button(format!("🙈 {n}"), press(SlotAction::Unshare)));
            }
            (Some(true), true) => buttons.push(button(
                format!("🙈 {n} точно?"),
                press(SlotAction::UnshareConfirm),
            )),
            (None, _) => {}
        }
        match row.mentions.filter(|_| row.shared == Some(true)) {
            Some(true) => buttons.push(button(format!("📣 {n}"), press(SlotAction::EveryMessage))),
            Some(false) => buttons.push(button(format!("💬 {n}"), press(SlotAction::Mentions))),
            None => {}
        }
        match row.stop {
            Some(false) => buttons.push(button(format!("⏹ {n}"), press(SlotAction::Stop))),
            Some(true) => buttons.push(button(
                format!("⏹ {n} точно?"),
                press(SlotAction::StopConfirm),
            )),
            None => {}
        }
        rows.push(buttons);
    }
    let mut paging = Vec::new();
    if view.page > 0 {
        paging.push(button(
            "‹",
            MenuPress::Sessions {
                page: page_number(view.page - 1),
            },
        ));
    }
    paging.push(button("↻", MenuPress::Sessions { page }));
    if view.page + 1 < view.pages {
        paging.push(button(
            "›",
            MenuPress::Sessions {
                page: page_number(view.page + 1),
            },
        ));
    }
    rows.push(paging);
    let update = match view.outdated {
        0 => "⬆️ Обновить все клиенты".to_owned(),
        n => format!("⬆️ Обновить все клиенты ({n})"),
    };
    rows.push(vec![button(update, MenuPress::UpdateAll { page })]);
    text
}

/// The presses of one view's display rows: detail, 💭, turn view, rich
/// messages.
type DisplayPresses = (
    fn(Detail) -> MenuPress,
    fn(bool) -> MenuPress,
    fn(TurnView) -> MenuPress,
    fn(bool) -> MenuPress,
);

/// The rows of the display tab for one view (TASK-078): the switch between
/// the private chat and the group, then its detail level, 💭, rich messages
/// (TASK-075) and turn view.
fn display_rows(
    display: &Display,
    group: bool,
    (detail_press, thinking_press, turn_press, rich_press): DisplayPresses,
    rows: &mut Vec<Vec<Value>>,
) {
    rows.push(vec![
        button(radio("👤 Личка", !group), MenuPress::Display),
        button(radio("👥 Группа", group), MenuPress::GroupDisplay),
    ]);
    let detail = |label: &str, detail: Detail| {
        button(radio(label, display.detail == detail), detail_press(detail))
    };
    rows.push(vec![
        detail("Всё", Detail::Full),
        detail("Кратко", Detail::Brief),
        detail("Только ответы", Detail::Answers),
    ]);
    let thinking = if display.thinking {
        "вкл"
    } else {
        "выкл"
    };
    rows.push(vec![button(
        format!("💭 Размышления: {thinking}"),
        thinking_press(!display.thinking),
    )]);
    let rich = if display.rich { "вкл" } else { "выкл" };
    rows.push(vec![button(
        format!("✨ Rich-разметка: {rich}"),
        rich_press(!display.rich),
    )]);
    let turn =
        |label: &str, turn: TurnView| button(radio(label, display.turn == turn), turn_press(turn));
    rows.push(vec![
        turn("Ход: полный", TurnView::Full),
        turn("Ход: сжатый", TurnView::Compact),
    ]);
}

/// What the levels mean, the same for both views.
const DISPLAY_HELP: &str = "Всё: промпты из терминала, текст Claude и строка на каждый вызов инструмента.\n\
Кратко: без строк инструментов.\n\
Только ответы: промпты из терминала и ответ хода.\n\
💭 размышления включаются отдельно, при любой подробности.\n\
Ход сжатый: строки инструментов и 💭 свёрнуты в цитату, нажмите на неё, чтобы раскрыть.\n\
Rich-разметка: ответы и текст хода с таблицами, заголовками и списками. Telegram Web и Telegram X их не показывают: для них выключите.\n\
Ответы хода, запросы разрешений, вопросы и субагенты видны всегда.";

fn render_display(settings: &Settings, rows: &mut Vec<Vec<Value>>) -> String {
    display_rows(
        &settings.display(),
        false,
        (
            MenuPress::Detail,
            MenuPress::Thinking,
            MenuPress::TurnView,
            MenuPress::Rich,
        ),
        rows,
    );
    format!(
        "Что показывать в темах лички\n\n{DISPLAY_HELP}\n\n\
В группе ваши сессии показываются по настройкам группы: кнопка «👥 Группа»."
    )
}

fn render_group_display(settings: &Settings, rows: &mut Vec<Vec<Value>>) -> String {
    display_rows(
        &settings.group,
        true,
        (
            MenuPress::GroupDetail,
            MenuPress::GroupThinking,
            MenuPress::GroupTurn,
            MenuPress::GroupRich,
        ),
        rows,
    );
    let history = |limit: HistoryLimit| {
        button(
            radio(&format!("📜 {}", limit.chars()), settings.history == limit),
            MenuPress::GroupHistory(limit),
        )
    };
    rows.push(HistoryLimit::ALL.into_iter().map(history).collect());
    format!(
        "Что показывать в темах группы (ваши сессии, добавленные в группу)\n\n{DISPLAY_HELP}\n\
📜 История до обращения: сколько символов истории темы группы агент получает с обращением; длиннее — сжимает haiku на вашем устройстве, при сбое обрезает.\n\n\
По умолчанию всё, rich-разметка включена. Звук в группе не меняется."
    )
}

fn render_sound(settings: &Settings, rows: &mut Vec<Vec<Value>>) -> String {
    let sound = |label: &str, sound: Sound| {
        button(
            radio(label, settings.sound == sound),
            MenuPress::SoundMode(sound),
        )
    };
    rows.push(vec![
        sound("Ответы и запросы", Sound::Replies),
        sound("Всё", Sound::All),
        sound("Ничего", Sound::Off),
    ]);
    let quiet = match settings.quiet {
        None => "🌙 Тихие часы: выкл".to_owned(),
        Some(quiet) => format!("🌙 Тихие часы: с {:02} до {:02}", quiet.from, quiet.to),
    };
    rows.push(vec![button(
        quiet,
        MenuPress::Quiet(settings.quiet.is_none()),
    )]);
    if let Some(quiet) = settings.quiet {
        // `% 24` first: never past `u8`, whatever the hour.
        let earlier = |hour: u8| (hour % 24 + 23) % 24;
        let later = |hour: u8| (hour % 24 + 1) % 24;
        rows.push(vec![
            button("с −1", MenuPress::QuietFrom(earlier(quiet.from))),
            button("с +1", MenuPress::QuietFrom(later(quiet.from))),
            button("до −1", MenuPress::QuietTo(earlier(quiet.to))),
            button("до +1", MenuPress::QuietTo(later(quiet.to))),
        ]);
    }
    let zone = match settings.tz {
        Some(tz) => format!("🕒 Пояс: {}", zone_label(tz)),
        None => "🕒 Указать часовой пояс".to_owned(),
    };
    rows.push(vec![button(zone, MenuPress::Zone { fractions: false })]);
    "Звук сообщений в личке\n\n\
Ответы и запросы: со звуком ответы хода, запросы разрешений и вопросы, остальное тихо.\n\
Всё: со звуком каждое новое сообщение сессии.\n\
Ничего: всё тихо.\n\n\
В тихие часы звук только у запросов разрешений и вопросов. Летнее время не учитывается: при переходе укажите пояс заново. «Ничего» выключает звук и у запросов.\n\
Звук в группе не меняется."
        .to_owned()
}

fn render_people(view: &PeopleView, rows: &mut Vec<Vec<Value>>) -> String {
    let mut text = format!(
        "Люди\n\nВладельцев из .env: {} (меняются только в .env).\nУчастники:",
        view.owners
    );
    if view.members.is_empty() {
        text.push_str(" пока никого.");
    }
    for (index, member) in view.members.iter().enumerate() {
        let label = people::label(&member.name, member.username.as_deref());
        text.push_str(&format!("\n{}. {label}", index + 1));
        let name = cut(&member.name, PERSON_BUTTON_LIMIT);
        rows.push(vec![if member.confirm {
            button(
                format!("🗑 {name} точно?"),
                MenuPress::RemoveConfirm { key: member.key },
            )
        } else {
            button(format!("🗑 {name}"), MenuPress::Remove { key: member.key })
        }]);
    }
    text.push_str(
        "\n\nКак добавить: перешлите сюда, вне тем, любое сообщение человека. Если Telegram скрывает его аккаунт в пересылках, дайте ему ссылку-приглашение.\n\
Участник пишет сессиям и отвечает на вопросы и разрешения, как вы; не подключает и не отзывает устройства и не меняет этот список.\n\
Удаление сразу закрывает доступ и отключает его устройства, но не убирает его из групп Telegram.",
    );
    let mut last = Vec::new();
    if view.invite {
        last.push(button(people::INVITE_BUTTON, MenuPress::Invite));
    }
    last.push(button("↻", MenuPress::People));
    rows.push(last);
    text
}

/// A device's name on a button, at most (UTF-16 units).
const DEVICE_BUTTON_LIMIT: usize = 40;

fn render_devices(view: &DevicesView, rows: &mut Vec<Vec<Value>>) -> String {
    let mut text = if view.rows.is_empty() {
        "Устройства\n\nСвоих секретов у устройств пока нет.".to_owned()
    } else {
        format!("Устройства ({}):", view.rows.len())
    };
    // Not `device`: the hub's file guard (tests/hub_reads_no_files.rs)
    // forbids that name, the crate's module.
    for (index, machine) in view.rows.iter().enumerate() {
        text.push_str(&format!(
            "\n{}. {} · {:08x} · с {} · {} · {}",
            index + 1,
            machine.name,
            machine.id,
            machine.joined,
            machine.seen,
            machine.owner,
        ));
        let name = cut(&machine.name, DEVICE_BUTTON_LIMIT);
        rows.push(vec![if machine.confirm {
            button(
                format!("🗑 {name} — точно?"),
                MenuPress::DeviceRevokeConfirm { id: machine.id },
            )
        } else {
            button(
                format!("🗑 {name}"),
                MenuPress::DeviceRevoke { id: machine.id },
            )
        }]);
    }
    text.push_str(&format!(
        "\n\n{}\n\n\
🗑 отзывает устройство: его агенты сразу теряют связь с hub, хуки больше не принимаются; вернуть его можно только новым кодом.\n\
➕ присылает ниже отдельным сообщением строку установки с одноразовым кодом на {} минут; устройство с этим кодом будет вашим.",
        view.shared,
        CODE_TTL.as_secs() / 60
    ));
    rows.push(vec![
        button("➕ Добавить устройство", MenuPress::DeviceAdd),
        button("↻", MenuPress::Devices),
    ]);
    text
}

fn render_hub(view: &HubView, rows: &mut Vec<Vec<Value>>) -> String {
    let floods = if view.floods_day {
        format!("429 за сутки: {}", view.floods)
    } else {
        format!("429 с запуска: {}", view.floods)
    };
    let mut text = format!(
        "Hub\n\n\
Версия: {} (сборка {})\n\
Запущен: {}, работает {}\n\
Сессии: живых {}, завершённых {}\n\
Темы: {}\n\
Устройства: {}\n\
{floods}\n\n\
Уборка тем\n",
        view.release,
        view.build,
        view.started,
        view.uptime,
        view.live,
        view.ended,
        view.topics,
        view.devices,
    );
    if view.days == 0 {
        text.push_str("Мёртвые сейчас (дольше минуты)");
    } else {
        text.push_str(&format!("Завершены больше {} дн. назад", view.days));
    }
    text.push_str(&format!(": слоты: {}, темы: {}", view.slots, view.clean));
    if view.kept > 0 {
        text.push_str(&format!(
            "\nТемы, которые бот удалить не может, останутся как есть: {}",
            view.kept
        ));
    }
    if view.left > 0 || view.done + view.failed > 0 {
        text.push_str(&format!(
            "\nУборка: осталось {}, удалено {}, не удалось {}",
            view.left, view.done, view.failed
        ));
    }
    text.push_str(
        "\n\nУборка общая на весь hub: темы всех людей, в личках и в группах; делают её только владельцы. \
Тема удаляется со всеми сообщениями. Слоты с сообщениями, ждущими Resume, и с открытым запросом разрешения или вопросом не трогаются. \
Следующая сессия той же папки получит новую тему; в группу её нужно добавить заново. \
Возраст считается с того момента, когда hub увидел сессию завершённой; завершённые до этой версии hub считаются с обновления.",
    );
    rows.push(
        CLEANUP_DAYS
            .into_iter()
            .map(|days| {
                let label = match days {
                    0 => "сейчас".to_owned(),
                    days => format!("{days} дн"),
                };
                button(
                    radio(&label, days == view.days),
                    MenuPress::CleanupDays(days),
                )
            })
            .collect(),
    );
    if view.clean > 0 {
        let (days, topics, slots) = (view.days, view.clean, view.key);
        rows.push(vec![if view.armed {
            button(
                format!("🧹 Удалить темы: {topics} — точно?"),
                MenuPress::CleanupConfirm {
                    days,
                    topics,
                    slots,
                },
            )
        } else {
            button(
                format!("🧹 Удалить темы: {topics}"),
                MenuPress::Cleanup {
                    days,
                    topics,
                    slots,
                },
            )
        }]);
    }
    rows.push(vec![button("↻", MenuPress::Hub)]);
    text
}

fn render_zone(fractions: bool, unix_secs: i64, rows: &mut Vec<Vec<Value>>) -> String {
    let mut zones: Vec<i16> = (-12..=14).map(|hours: i16| hours * 60).collect();
    if fractions {
        zones.extend(FRACTION_ZONES);
        zones.sort_unstable();
    }
    for chunk in zones.chunks(4) {
        rows.push(
            chunk
                .iter()
                .map(|&tz| {
                    button(
                        format!("{} ({})", clock(unix_secs, tz), offset_label(tz)),
                        MenuPress::SetZone(tz),
                    )
                })
                .collect(),
        );
    }
    if !fractions {
        rows.push(vec![button(
            "ещё: пояса с :30 и :45",
            MenuPress::Zone { fractions: true },
        )]);
    }
    rows.push(vec![button("‹ Назад", MenuPress::Sound)]);
    "Сколько у вас сейчас времени? Нажмите это время: по нему hub узнает ваш часовой пояс (в скобках смещение от UTC)."
        .to_owned()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Every callback data of a keyboard.
    fn datas(keyboard: &Value) -> Vec<String> {
        keyboard["inline_keyboard"]
            .as_array()
            .unwrap()
            .iter()
            .flat_map(|row| row.as_array().unwrap())
            .map(|button| button["callback_data"].as_str().unwrap().to_owned())
            .collect()
    }

    /// Every button text of a keyboard.
    fn labels(keyboard: &Value) -> Vec<String> {
        keyboard["inline_keyboard"]
            .as_array()
            .unwrap()
            .iter()
            .flat_map(|row| row.as_array().unwrap())
            .map(|button| button["text"].as_str().unwrap().to_owned())
            .collect()
    }

    fn row(slot: u32, shared: Option<bool>, share_confirm: bool, stop: Option<bool>) -> SessionRow {
        SessionRow {
            slot,
            title: format!("[box] project · {slot}"),
            state: RowState::Working,
            shared,
            share_confirm,
            stop,
            // Even slots take mentions only, odd ones every message.
            mentions: shared
                .filter(|shared| *shared)
                .map(|_| slot.is_multiple_of(2)),
            pick: false,
            groups: Vec::new(),
        }
    }

    fn all_pages(settings: &Settings, view: &SessionsView) -> Vec<(String, Value)> {
        let devices = OwnerPage::Devices(devices_view());
        let hubs = [hub_view(false), hub_view(true)].map(OwnerPage::Hub);
        [
            (Page::Sessions(view.page), None),
            (Page::Display, None),
            (Page::GroupDisplay, None),
            (Page::Sound, None),
            (Page::Zone { fractions: false }, None),
            (Page::Zone { fractions: true }, None),
            (Page::People, None),
            (Page::Devices, Some(&devices)),
            (Page::Hub, Some(&hubs[0])),
            (Page::Hub, Some(&hubs[1])),
        ]
        .iter()
        .map(|(page, owner_page)| {
            render(
                page,
                settings,
                Some(view),
                Some(&people_view()),
                *owner_page,
                1_700_000_000,
            )
        })
        .collect()
    }

    /// 32 devices of the longest names, the one of `u32::MAX` asking for
    /// its second press (TASK-074).
    fn devices_view() -> DevicesView {
        DevicesView {
            rows: (0..32u32)
                .map(|index| DeviceRow {
                    id: if index == 31 { u32::MAX } else { index },
                    name: "Ж".repeat(crate::hub::devices::NAME_LIMIT),
                    owner: "владелец не записан".into(),
                    joined: "2026-09-29".into(),
                    seen: "последний вход 2 ч назад".into(),
                    confirm: index == 31,
                })
                .collect(),
            shared: "Общий секрет hub (CCTG_HUB_SECRET) отключён.".into(),
        }
    }

    /// A hub whose cleanup of 90 days counts `u32::MAX` topics, armed or
    /// not (TASK-074).
    fn hub_view(armed: bool) -> HubView {
        HubView {
            release: "v0.1.27".into(),
            build: "0123abcd".into(),
            started: "2026-09-29 10:00 UTC".into(),
            uptime: "3 ч 5 мин".into(),
            live: 2,
            ended: 5,
            topics: 9,
            devices: 3,
            floods: 4,
            floods_day: true,
            days: 90,
            slots: 3,
            key: u32::MAX,
            clean: u32::MAX,
            kept: 1,
            armed,
            left: 2,
            done: 1,
            failed: 0,
        }
    }

    /// TASK-074: the owner's tabs are a second row, only with the people
    /// view; the devices and hub pages need their own view, else they are
    /// the sessions page.
    #[test]
    fn the_owner_tabs_are_a_second_row() {
        let settings = Settings::default();
        let devices = OwnerPage::Devices(devices_view());
        let hub = OwnerPage::Hub(hub_view(false));
        let (_, keyboard) = render(&Page::Sound, &settings, None, None, None, 0);
        let rows = keyboard["inline_keyboard"].as_array().unwrap();
        assert_eq!(rows[0].as_array().unwrap().len(), 3, "{keyboard}");
        for absent in ["👥 Люди", "💻 Устройства", "🛠 Hub"] {
            assert!(!labels(&keyboard).contains(&absent.to_owned()), "{absent}");
        }
        let (_, keyboard) = render(&Page::Sound, &settings, None, Some(&people_view()), None, 0);
        let second: Vec<&str> = keyboard["inline_keyboard"][1]
            .as_array()
            .unwrap()
            .iter()
            .map(|button| button["text"].as_str().unwrap())
            .collect();
        assert_eq!(second, ["👥 Люди", "💻 Устройства", "🛠 Hub"]);
        // Without the people view (a member): the sessions page.
        for (page, owner_page) in [(Page::Hub, &hub), (Page::Devices, &devices)] {
            let (text, keyboard) = render(&page, &settings, None, None, Some(owner_page), 0);
            assert!(text.starts_with("Сессии"), "{text}");
            assert!(labels(&keyboard).contains(&"· 🗂 Сессии".to_owned()));
        }
        // A page without its own view: the sessions page too.
        for (page, owner_page) in [
            (Page::Hub, None),
            (Page::Hub, Some(&devices)),
            (Page::Devices, Some(&hub)),
        ] {
            let (text, _) = render(&page, &settings, None, Some(&people_view()), owner_page, 0);
            assert!(text.starts_with("Сессии"), "{page:?}: {text}");
        }
        let (text, keyboard) = render(
            &Page::Devices,
            &settings,
            None,
            Some(&people_view()),
            Some(&devices),
            0,
        );
        assert!(text.starts_with("Устройства (32):"), "{text}");
        assert!(transcript::telegram_len(&text) <= transcript::TELEGRAM_TEXT_LIMIT);
        let shown = labels(&keyboard);
        let pressed = datas(&keyboard);
        assert!(shown.contains(&"· 💻 Устройства".to_owned()), "{shown:?}");
        let at = shown
            .iter()
            .position(|label| label.ends_with("— точно?"))
            .unwrap();
        assert_eq!(pressed[at], "menu:drc:ffffffff");
        let at = shown
            .iter()
            .position(|label| label.starts_with("🗑 "))
            .unwrap();
        assert_eq!(pressed[at], "menu:dr:00000000");
        assert!(pressed.contains(&"menu:da".to_owned()));
        assert!(pressed.contains(&"menu:dv".to_owned()));
        let empty = OwnerPage::Devices(DevicesView {
            rows: Vec::new(),
            shared: "Общий секрет hub (CCTG_HUB_SECRET) отключён.".into(),
        });
        let (text, _) = render(
            &Page::Devices,
            &settings,
            None,
            Some(&people_view()),
            Some(&empty),
            0,
        );
        assert!(
            text.contains("Своих секретов у устройств пока нет."),
            "{text}"
        );
        assert!(text.contains("отключён"), "{text}");
    }

    /// TASK-074: the hub page shows its numbers, the chosen age, what the
    /// cleanup takes, and asks once more before it starts.
    #[test]
    fn the_hub_page_counts_and_asks_before_the_cleanup() {
        let settings = Settings::default();
        let render_hub_page = |view: HubView| {
            render(
                &Page::Hub,
                &settings,
                None,
                Some(&people_view()),
                Some(&OwnerPage::Hub(view)),
                0,
            )
        };
        let (text, keyboard) = render_hub_page(HubView {
            clean: 7,
            ..hub_view(false)
        });
        for want in [
            "Версия: v0.1.27 (сборка 0123abcd)",
            "Сессии: живых 2, завершённых 5",
            "429 за сутки: 4",
            "Завершены больше 90 дн. назад: слоты: 3, темы: 7",
            "Темы, которые бот удалить не может, останутся как есть: 1",
            "получит новую тему; в группу её нужно добавить заново",
            "Уборка: осталось 2, удалено 1, не удалось 0",
            "общая на весь hub",
        ] {
            assert!(text.contains(want), "{want}: {text}");
        }
        let shown = labels(&keyboard);
        let pressed = datas(&keyboard);
        assert!(shown.contains(&"· 🛠 Hub".to_owned()), "{shown:?}");
        assert!(shown.contains(&"✅ 90 дн".to_owned()), "{shown:?}");
        assert!(shown.contains(&"сейчас".to_owned()), "{shown:?}");
        let at = shown
            .iter()
            .position(|label| label.starts_with("🧹"))
            .unwrap();
        assert_eq!(shown[at], "🧹 Удалить темы: 7");
        assert_eq!(pressed[at], format!("menu:hx:90:7:{}", u32::MAX));
        assert!(pressed.contains(&"menu:hd:0".to_owned()));
        assert!(pressed.contains(&"menu:hb".to_owned()));
        let (_, keyboard) = render_hub_page(HubView {
            clean: 7,
            key: 12,
            ..hub_view(true)
        });
        assert!(datas(&keyboard).contains(&"menu:hxc:90:7:12".to_owned()));
        assert!(labels(&keyboard).contains(&"🧹 Удалить темы: 7 — точно?".to_owned()));
        // Nothing to clean: no button; now, since the start, nothing done.
        let (text, keyboard) = render_hub_page(HubView {
            days: 0,
            clean: 0,
            slots: 0,
            kept: 0,
            left: 0,
            done: 0,
            failed: 0,
            floods_day: false,
            ..hub_view(false)
        });
        assert!(
            text.contains("Мёртвые сейчас (дольше минуты): слоты: 0, темы: 0"),
            "{text}"
        );
        assert!(text.contains("429 с запуска: 4"), "{text}");
        assert!(!text.contains("бот удалить не может") && !text.contains("Уборка: осталось"));
        assert!(
            !labels(&keyboard)
                .iter()
                .any(|label| label.starts_with("🧹"))
        );
        assert!(labels(&keyboard).contains(&"✅ сейчас".to_owned()));
    }

    /// TASK-074: the new presses round-trip, only valid ids and ages parse,
    /// the cleanup codes are not taken for a slot button, and every one of
    /// them is an owner's and no setting.
    #[test]
    fn the_owner_presses_parse_strictly() {
        for press in [
            MenuPress::Devices,
            MenuPress::DeviceRevoke { id: 0 },
            MenuPress::DeviceRevokeConfirm { id: u32::MAX },
            MenuPress::DeviceAdd,
            MenuPress::Hub,
            MenuPress::CleanupDays(0),
            MenuPress::CleanupDays(90),
            MenuPress::Cleanup {
                days: 30,
                topics: 7,
                slots: 0,
            },
            MenuPress::CleanupConfirm {
                days: 0,
                topics: u32::MAX,
                slots: u32::MAX,
            },
        ] {
            assert_eq!(parse_callback(&data(&press)), Some(press), "{press:?}");
            assert!(press.owners_only(), "{press:?}");
            assert!(change(&Settings::default(), press).is_err(), "{press:?}");
        }
        assert_eq!(
            parse_callback("menu:dr:0123abcd"),
            Some(MenuPress::DeviceRevoke { id: 0x0123_abcd })
        );
        assert_eq!(
            parse_callback("menu:hx:30:7:5"),
            Some(MenuPress::Cleanup {
                days: 30,
                topics: 7,
                slots: 5,
            })
        );
        assert_eq!(
            parse_callback("menu:hxc:0:1:9"),
            Some(MenuPress::CleanupConfirm {
                days: 0,
                topics: 1,
                slots: 9,
            })
        );
        for junk in [
            "menu:dr:ABCDEF12",
            "menu:dr:123",
            "menu:dr:0123abcde",
            "menu:drc:+123abcd",
            "menu:hd:5",
            "menu:hd",
            "menu:hx:30",
            "menu:hx:30:7",
            "menu:hxc:0:1",
            "menu:hxc:30:x:1",
            "menu:hx:2:7:1",
            "menu:hx:30:-1:1",
            "menu:hx:30:7:x",
            "menu:hx:30:7:1:1",
            "menu:dv:1",
            "menu:hb:1",
        ] {
            assert_eq!(parse_callback(junk), None, "{junk}");
        }
        assert!(MenuPress::Devices.navigates());
        assert!(MenuPress::Hub.navigates());
        assert!(MenuPress::CleanupDays(1).navigates());
        assert!(!MenuPress::DeviceAdd.navigates());
        assert!(
            !MenuPress::Cleanup {
                days: 1,
                topics: 1,
                slots: 1
            }
            .navigates()
        );
        // The key depends on the slots and their order.
        assert_ne!(cleanup_key([0, 1]), cleanup_key([0, 2]));
        assert_ne!(cleanup_key([0, 1]), cleanup_key([1, 0]));
        assert_eq!(cleanup_key([3, 4]), cleanup_key(vec![3, 4]));
        assert!(MenuPress::People.owners_only());
        assert!(!MenuPress::Sound.owners_only());
        assert!(
            !MenuPress::Slot {
                action: SlotAction::Open,
                page: 0,
                slot: 0
            }
            .owners_only()
        );
    }

    /// Two members, the second asking for its second press (TASK-081).
    fn people_view() -> PeopleView {
        PeopleView {
            owners: 2,
            members: vec![
                PersonRow {
                    key: 1,
                    name: "Анна".into(),
                    username: Some("anna".into()),
                    confirm: false,
                },
                PersonRow {
                    key: u32::MAX,
                    name: "Борис Очень-Длинная-Фамилия-Которая-Не-Влезает".into(),
                    username: None,
                    confirm: true,
                },
            ],
            invite: true,
        }
    }

    /// TASK-081: the people tab only with a view; its rows, buttons and the
    /// proposal's buttons.
    #[test]
    fn the_people_tab_lists_members_with_their_buttons() {
        let (text, keyboard) = render(&Page::People, &Settings::default(), None, None, None, 0);
        assert!(text.starts_with("Сессии"), "no tab, the sessions: {text}");
        assert!(!labels(&keyboard).iter().any(|label| label.contains("Люди")));
        assert!(!datas(&keyboard).contains(&"menu:pp".to_owned()));

        let (_, keyboard) = render(
            &Page::Sound,
            &Settings::default(),
            None,
            Some(&people_view()),
            None,
            0,
        );
        assert!(labels(&keyboard).contains(&"👥 Люди".to_owned()));

        let (text, keyboard) = render(
            &Page::People,
            &Settings::default(),
            None,
            Some(&people_view()),
            None,
            0,
        );
        assert!(text.starts_with("Люди"), "{text}");
        assert!(text.contains("Владельцев из .env: 2"), "{text}");
        assert!(text.contains("\n1. Анна (@anna)"), "{text}");
        assert!(text.contains("\n2. Борис"), "{text}");
        let shown = labels(&keyboard);
        let pressed = datas(&keyboard);
        assert!(shown.contains(&"· 👥 Люди".to_owned()), "{shown:?}");
        let at = shown.iter().position(|label| label == "🗑 Анна").unwrap();
        assert_eq!(pressed[at], "menu:pr:1");
        let at = shown
            .iter()
            .position(|label| label.ends_with("точно?"))
            .unwrap();
        assert_eq!(pressed[at], format!("menu:prc:{}", u32::MAX));
        assert!(shown[at].starts_with("🗑 Борис"), "{}", shown[at]);
        assert!(pressed.contains(&"menu:pi".to_owned()));
        assert!(pressed.contains(&"menu:pp".to_owned()));

        let alone = PeopleView {
            owners: 1,
            members: Vec::new(),
            invite: false,
        };
        let (text, keyboard) = render(
            &Page::People,
            &Settings::default(),
            None,
            Some(&alone),
            None,
            0,
        );
        assert!(text.contains("Участники: пока никого."), "{text}");
        assert!(!datas(&keyboard).contains(&"menu:pi".to_owned()));

        assert_eq!(datas(&add_keyboard(7)), ["menu:pa:7", "menu:pn:7"]);
        assert_eq!(datas(&invite_keyboard()), ["menu:pi"]);
        for press in [
            MenuPress::People,
            MenuPress::Remove { key: 3 },
            MenuPress::RemoveConfirm { key: 3 },
            MenuPress::Invite,
            MenuPress::AddYes { token: u32::MAX },
            MenuPress::AddNo { token: 0 },
        ] {
            assert!(press.manages_people());
            assert_eq!(parse_callback(&data(&press)), Some(press));
            assert!(
                change(&Settings::default(), press).is_err(),
                "{press:?} is no setting"
            );
        }
        assert!(MenuPress::People.navigates());
        assert!(!MenuPress::Remove { key: 1 }.navigates());
        assert!(!MenuPress::Sound.manages_people());
        for junk in [
            "menu:pr",
            "menu:pr:x",
            "menu:pa:-1",
            "menu:pp:1",
            "menu:pi:1",
        ] {
            assert_eq!(parse_callback(junk), None, "{junk}");
        }
    }

    #[test]
    fn every_press_round_trips_and_fits_64_bytes() {
        let rows = vec![
            row(u32::MAX, Some(false), false, Some(false)),
            row(1, Some(true), false, Some(true)),
            row(2, Some(true), true, None),
            row(3, None, false, None),
            row(u32::MAX - 1, Some(true), true, Some(true)),
            row(0, Some(false), false, Some(false)),
        ];
        let view = SessionsView {
            rows,
            page: u32::MAX as usize - 1,
            pages: u32::MAX as usize,
            outdated: 3,
        };
        let quiet = Settings {
            quiet: Some(Quiet { from: 0, to: 23 }),
            tz: Some(-720),
            ..Settings::default()
        };
        let mut seen = 0;
        for settings in [Settings::default(), quiet] {
            for (_, keyboard) in all_pages(&settings, &view) {
                for data in datas(&keyboard) {
                    assert!(data.len() <= 64, "{data}");
                    assert!(data.is_ascii(), "{data}");
                    let press = parse_callback(&data).unwrap_or_else(|| panic!("{data}"));
                    assert_eq!(super::data(&press), data);
                    seen += 1;
                }
            }
        }
        assert!(seen > 80, "{seen}");
        for junk in [
            "menu:",
            "menu:tz:x",
            "menu:qf:-1",
            "menu:st:1",
            "menu:s",
            "menu:s:1:2",
            "menu:dl:x",
            "menu:z:2",
            "menu:xx:1:2",
            "status:stop",
            "menu:qf:256",
            "menu:th",
            "menu:q",
            "menu:th:2",
            "menu:q:x",
            "menu:tv",
            "menu:tv:x",
            "menu:tv:c:1",
            "menu:gdl",
            "menu:gdl:x",
            "menu:gth:2",
            "menu:gtv:",
            "menu:dg:1",
        ] {
            assert_eq!(parse_callback(junk), None, "{junk}");
        }
    }

    #[test]
    fn quiet_hours_across_midnight_and_zones() {
        let at = |hour: i64, minute: i64| hour * 3600 + minute * 60;
        let with = |from, to, tz| Settings {
            quiet: Some(Quiet { from, to }),
            tz: Some(tz),
            ..Settings::default()
        };
        // 23 -> 8 in UTC.
        let night = with(23, 8, 0);
        assert!(quiet_now(&night, at(23, 0)), "from is in");
        assert!(quiet_now(&night, at(2, 30)));
        assert!(quiet_now(&night, at(7, 59)));
        assert!(!quiet_now(&night, at(8, 0)), "to is out");
        assert!(!quiet_now(&night, at(22, 59)));
        // 8 -> 23.
        let day = with(8, 23, 0);
        assert!(quiet_now(&day, at(8, 0)));
        assert!(quiet_now(&day, at(22, 59)));
        assert!(!quiet_now(&day, at(23, 0)));
        assert!(!quiet_now(&day, at(7, 59)));
        // Empty.
        assert!(!quiet_now(&with(5, 5, 0), at(5, 0)));
        // UTC-9:30: 23:00 local is 08:30 UTC.
        let west = with(23, 8, -570);
        assert!(quiet_now(&west, at(8, 30)));
        assert!(!quiet_now(&west, at(8, 29)));
        // UTC+5:45: 08:00 local is 02:15 UTC.
        let east = with(23, 8, 345);
        assert!(quiet_now(&east, at(2, 14)));
        assert!(!quiet_now(&east, at(2, 15)));
        // Before 1970 too.
        assert!(quiet_now(&night, -at(0, 30)), "23:30 of the day before");
        // No zone: never.
        let unzoned = Settings {
            quiet: Some(Quiet { from: 0, to: 23 }),
            ..Settings::default()
        };
        assert!(!quiet_now(&unzoned, at(12, 0)));
        assert!(!quiet_now(&Settings::default(), at(12, 0)));
    }

    #[test]
    fn loudness_by_mode_and_quiet_hours() {
        // 12:00 UTC, inside the window 12 -> 13 when it is on.
        let now = 12 * 3600;
        for sound in [Sound::Replies, Sound::All, Sound::Off] {
            for quiet in [false, true] {
                let settings = Settings {
                    sound,
                    quiet: quiet.then_some(Quiet { from: 12, to: 13 }),
                    tz: Some(0),
                    ..Settings::default()
                };
                assert_eq!(quiet_now(&settings, now), quiet);
                for ringing in [false, true] {
                    for asks in [false, true] {
                        for counts in [false, true] {
                            let want = match (sound, quiet) {
                                (Sound::Off, _) => false,
                                (_, true) => ringing && asks,
                                (Sound::Replies, false) => ringing,
                                (Sound::All, false) => ringing || counts,
                            };
                            assert_eq!(
                                loud(&settings, ringing, asks, counts, now),
                                want,
                                "{sound:?} quiet={quiet} ringing={ringing} asks={asks} counts={counts}"
                            );
                        }
                    }
                }
            }
        }
    }

    #[test]
    fn detail_shows_pieces() {
        for detail in [Detail::Full, Detail::Brief, Detail::Answers] {
            for thinking in [false, true] {
                let settings = Settings {
                    detail,
                    thinking,
                    ..Settings::default()
                };
                let display = settings.display();
                assert!(shows(&display, Piece::Prompt));
                assert!(shows(&display, Piece::Interrupt));
                assert_eq!(shows(&display, Piece::Thinking), thinking);
                assert_eq!(shows(&display, Piece::Text), detail != Detail::Answers);
                assert_eq!(shows(&display, Piece::Tool), detail == Detail::Full);
            }
        }
    }

    #[test]
    fn the_default_settings_are_todays_behaviour() {
        let settings = Settings::default();
        assert_eq!(settings.detail, Detail::Full);
        assert!(settings.thinking);
        assert_eq!(settings.turn, TurnView::Full);
        assert_eq!(settings.sound, Sound::Replies);
        assert_eq!(settings.quiet, None);
        assert_eq!(settings.tz, None);
        assert_eq!(settings.group, Display::default());
        // TASK-075: rich messages on in the private chat; in the group too
        // since TASK-077.
        assert_eq!(settings.display(), Display::default());
        assert!(settings.display().rich);
        assert!(settings.group.rich);
        assert_eq!(settings.history, HistoryLimit::Medium);
        assert_eq!(settings.history.chars(), 4000);
        for piece in [
            Piece::Prompt,
            Piece::Text,
            Piece::Thinking,
            Piece::Tool,
            Piece::Interrupt,
        ] {
            assert!(shows(&settings.display(), piece));
            assert!(shows(&settings.group, piece));
        }
        for ringing in [false, true] {
            for asks in [false, true] {
                for counts in [false, true] {
                    assert_eq!(loud(&settings, ringing, asks, counts, 0), ringing);
                }
            }
        }
    }

    #[test]
    fn an_unknown_level_reads_as_the_default() {
        let settings: Settings =
            serde_json::from_str(r#"{"detail":"later","sound":"later","turn":"later"}"#).unwrap();
        assert_eq!(settings.detail, Detail::Full);
        assert_eq!(settings.sound, Sound::Replies);
        assert_eq!(settings.turn, TurnView::Full);
        assert!(settings.thinking);
        let settings: Settings = serde_json::from_str("{}").unwrap();
        assert_eq!(settings, Settings::default());
        // The known values keep their names.
        let written = serde_json::to_value(Settings {
            detail: Detail::Answers,
            sound: Sound::Off,
            quiet: Some(Quiet { from: 22, to: 7 }),
            tz: Some(180),
            thinking: false,
            turn: TurnView::Compact,
            group: Display::default(),
            rich: true,
            history: HistoryLimit::Long,
        })
        .unwrap();
        assert_eq!(
            written,
            json!({"detail": "answers", "thinking": false, "turn": "compact", "sound": "off",
                   "quiet": {"from": 22, "to": 7}, "tz": 180, "rich": true, "history": "long"})
        );
        let back: Settings = serde_json::from_value(written).unwrap();
        assert_eq!(back.detail, Detail::Answers);
        assert_eq!(back.turn, TurnView::Compact);
        assert_eq!(back.history, HistoryLimit::Long);
        let later: Settings = serde_json::from_str(r#"{"history":"later"}"#).unwrap();
        assert_eq!(later.history, HistoryLimit::Medium);
        assert_eq!(
            serde_json::to_value(Settings::default()).unwrap(),
            json!({"detail": "full", "thinking": true, "turn": "full", "sound": "replies",
                   "rich": true})
        );
    }

    #[test]
    fn the_sessions_page_pages_and_marks_confirms() {
        let rows = vec![
            row(10, Some(false), false, Some(false)),
            row(11, Some(true), true, Some(true)),
            row(12, None, false, None),
        ];
        let first = SessionsView {
            rows,
            page: 0,
            pages: 2,
            outdated: 2,
        };
        let (text, keyboard) = render(
            &Page::Sessions(0),
            &Settings::default(),
            Some(&first),
            None,
            None,
            0,
        );
        assert!(text.starts_with("Сессии (стр. 1/2)"), "{text}");
        assert!(
            text.contains("\n1. ⚡ [box] project · 10 — работает"),
            "{text}"
        );
        assert!(
            text.contains("\n2. ⚡ [box] project · 11 — работает, в группе"),
            "{text}"
        );
        let labels = labels(&keyboard);
        for want in [
            "· 🗂 Сессии",
            "↗ 1",
            "👥 1",
            "⏹ 1",
            "↗ 2",
            "🙈 2 точно?",
            "⏹ 2 точно?",
            "↗ 3",
            "↻",
            "›",
            "⬆️ Обновить все клиенты (2)",
        ] {
            assert!(labels.contains(&want.to_owned()), "{want}: {labels:?}");
        }
        assert!(
            !labels.contains(&"‹".to_owned()),
            "no way back from the first"
        );
        assert!(!labels.contains(&"⏹ 3".to_owned()));
        assert!(!labels.contains(&"👥 3".to_owned()));
        let datas = datas(&keyboard);
        assert!(datas.contains(&"menu:sc:0:11".to_owned()), "{datas:?}");
        assert!(datas.contains(&"menu:uc:0:11".to_owned()), "{datas:?}");
        assert!(datas.contains(&"menu:s:1".to_owned()), "{datas:?}");
        // The last page: back, no forward, no count without outdated ones.
        let last = SessionsView {
            rows: vec![row(13, Some(true), false, Some(false))],
            page: 1,
            pages: 2,
            outdated: 0,
        };
        let (text, keyboard) = render(
            &Page::Sessions(1),
            &Settings::default(),
            Some(&last),
            None,
            None,
            0,
        );
        assert!(text.starts_with("Сессии (стр. 2/2)"), "{text}");
        let labels = super::tests::labels(&keyboard);
        assert!(labels.contains(&"‹".to_owned()));
        assert!(!labels.contains(&"›".to_owned()));
        assert!(labels.contains(&"🙈 1".to_owned()));
        assert!(labels.contains(&"⬆️ Обновить все клиенты".to_owned()));
        // None at all.
        let (text, _) = render(
            &Page::Sessions(0),
            &Settings::default(),
            None,
            None,
            None,
            0,
        );
        assert!(text.ends_with("Сессий пока нет."), "{text}");
    }

    #[test]
    fn the_zone_page_offers_local_times() {
        // 2023-11-14 22:13:20 UTC.
        let now = 1_700_000_000;
        let (text, keyboard) = render(
            &Page::Zone { fractions: false },
            &Settings::default(),
            None,
            None,
            None,
            now,
        );
        assert!(text.starts_with("Сколько у вас сейчас времени?"));
        let labels = labels(&keyboard);
        let datas = datas(&keyboard);
        let at = datas.iter().position(|d| d == "menu:tz:180").unwrap();
        // The labels are the tab row's three first.
        assert_eq!(labels[at], "01:13 (+3)");
        let utc = datas.iter().position(|d| d == "menu:tz:0").unwrap();
        assert_eq!(labels[utc], "22:13 (+0)");
        assert!(!datas.contains(&"menu:tz:345".to_owned()));
        assert!(datas.contains(&"menu:z:1".to_owned()));
        assert!(datas.contains(&"menu:n".to_owned()));
        let (_, keyboard) = render(
            &Page::Zone { fractions: true },
            &Settings::default(),
            None,
            None,
            None,
            now,
        );
        let labels = super::tests::labels(&keyboard);
        let datas = super::tests::datas(&keyboard);
        let at = datas.iter().position(|d| d == "menu:tz:345").unwrap();
        assert_eq!(labels[at], "03:58 (+5:45)");
        let at = datas.iter().position(|d| d == "menu:tz:-570").unwrap();
        assert_eq!(labels[at], "12:43 (−9:30)");
        assert!(!datas.contains(&"menu:z:1".to_owned()));
        // The sound tab shows the zone.
        let settings = Settings {
            tz: Some(345),
            quiet: Some(Quiet { from: 23, to: 8 }),
            ..Settings::default()
        };
        let (_, keyboard) = render(&Page::Sound, &settings, None, None, None, now);
        let labels = super::tests::labels(&keyboard);
        assert!(
            labels.contains(&"🕒 Пояс: UTC+5:45".to_owned()),
            "{labels:?}"
        );
        assert!(labels.contains(&"🌙 Тихие часы: с 23 до 08".to_owned()));
        let datas = super::tests::datas(&keyboard);
        for want in ["menu:qf:22", "menu:qf:0", "menu:qt:7", "menu:qt:9"] {
            assert!(datas.contains(&want.to_owned()), "{want}: {datas:?}");
        }
    }

    /// A toggle carries the value it sets: a double tap (both presses on the
    /// same keyboard) sets it twice, the second changes nothing.
    #[test]
    fn a_toggle_sets_its_value_and_a_repeat_changes_nothing() {
        let before = Settings {
            tz: Some(180),
            ..Settings::default()
        };
        let (_, keyboard) = render(&Page::Display, &before, None, None, None, 0);
        assert!(datas(&keyboard).contains(&"menu:th:0".to_owned()));
        let (_, keyboard) = render(&Page::Sound, &before, None, None, None, 0);
        assert!(datas(&keyboard).contains(&"menu:q:1".to_owned()));
        for press in [MenuPress::Thinking(false), MenuPress::Quiet(true)] {
            let (once, _) = change(&before, press).unwrap();
            assert_ne!(once, before, "{press:?}");
            let (twice, _) = change(&once, press).unwrap();
            assert_eq!(twice, once, "{press:?}");
        }
        let on = Settings {
            quiet: Some(QUIET_DEFAULT),
            ..before.clone()
        };
        let (off, _) = change(&on, MenuPress::Quiet(false)).unwrap();
        assert_eq!(off.quiet, None);
        assert_eq!(change(&off, MenuPress::Quiet(false)).unwrap().0, off);
        let (_, keyboard) = render(&Page::Sound, &on, None, None, None, 0);
        assert!(datas(&keyboard).contains(&"menu:q:0".to_owned()));
    }

    /// TASK-076: the turn view is a radio row of the display tab; a press
    /// sets it and stays on that tab, a repeat changes nothing.
    #[test]
    fn the_turn_view_is_chosen_on_the_display_tab() {
        let full = Settings::default();
        let (text, keyboard) = render(&Page::Display, &full, None, None, None, 0);
        assert!(text.contains("Ход сжатый: "), "{text}");
        let labels = labels(&keyboard);
        assert!(labels.contains(&"✅ Ход: полный".to_owned()), "{labels:?}");
        assert!(labels.contains(&"Ход: сжатый".to_owned()), "{labels:?}");
        let datas = datas(&keyboard);
        assert!(datas.contains(&"menu:tv:c".to_owned()), "{datas:?}");
        assert!(datas.contains(&"menu:tv:f".to_owned()), "{datas:?}");
        let press = parse_callback("menu:tv:c").unwrap();
        assert_eq!(press, MenuPress::TurnView(TurnView::Compact));
        assert!(!press.navigates());
        let (compact, page) = change(&full, press).unwrap();
        assert_eq!(page, Page::Display);
        assert_eq!(
            compact,
            Settings {
                turn: TurnView::Compact,
                ..full.clone()
            },
            "nothing else changes"
        );
        assert_eq!(change(&compact, press).unwrap().0, compact);
        let (_, keyboard) = render(&Page::Display, &compact, None, None, None, 0);
        assert!(super::tests::labels(&keyboard).contains(&"✅ Ход: сжатый".to_owned()));
        let (back, _) = change(&compact, MenuPress::TurnView(TurnView::Full)).unwrap();
        assert_eq!(back, full);
    }

    /// An hour past 23 in `registry.json` reads as no quiet hours, and the
    /// sound page never overflows on one.
    #[test]
    fn an_out_of_range_hour_reads_as_off_and_never_panics() {
        let settings: Settings =
            serde_json::from_str(r#"{"quiet":{"from":250,"to":8},"tz":0}"#).unwrap();
        assert_eq!(settings.quiet, None);
        assert_eq!(settings.tz, Some(0));
        let settings: Settings =
            serde_json::from_str(r#"{"quiet":{"from":23,"to":24},"tz":0}"#).unwrap();
        assert_eq!(settings.quiet, None);
        let settings: Settings =
            serde_json::from_str(r#"{"quiet":{"from":22,"to":7},"tz":0}"#).unwrap();
        assert_eq!(settings.quiet, Some(Quiet { from: 22, to: 7 }));
        // Made in memory past the check: the page still renders.
        let wild = Settings {
            quiet: Some(Quiet { from: 255, to: 250 }),
            tz: Some(0),
            ..Settings::default()
        };
        let (_, keyboard) = render(&Page::Sound, &wild, None, None, None, 0);
        let datas = datas(&keyboard);
        for want in ["menu:qf:14", "menu:qf:16", "menu:qt:9", "menu:qt:11"] {
            assert!(datas.contains(&want.to_owned()), "{want}: {datas:?}");
        }
    }

    #[test]
    fn no_draft_toggle_until_task_065() {
        let view = SessionsView {
            rows: vec![row(1, Some(false), false, Some(false))],
            page: 0,
            pages: 1,
            outdated: 0,
        };
        for (text, keyboard) in all_pages(&Settings::default(), &view) {
            assert!(!text.contains('✏'), "{text}");
            assert!(
                labels(&keyboard).iter().all(|label| !label.contains('✏')),
                "{keyboard}"
            );
        }
    }

    /// TASK-078: the group's settings are pressed on their own page and
    /// change nothing else; the press codes round-trip.
    #[test]
    fn the_group_display_has_its_own_presses_and_page() {
        let presses = [
            MenuPress::GroupDisplay,
            MenuPress::GroupDetail(Detail::Full),
            MenuPress::GroupDetail(Detail::Brief),
            MenuPress::GroupDetail(Detail::Answers),
            MenuPress::GroupThinking(false),
            MenuPress::GroupThinking(true),
            MenuPress::GroupTurn(TurnView::Full),
            MenuPress::GroupTurn(TurnView::Compact),
            MenuPress::GroupRich(false),
            MenuPress::GroupRich(true),
        ];
        for press in presses {
            let data = data(&press);
            assert_eq!(parse_callback(&data), Some(press), "{data}");
        }
        assert_eq!(data(&MenuPress::GroupDisplay), "menu:dg");
        assert_eq!(data(&MenuPress::GroupDetail(Detail::Brief)), "menu:gdl:b");
        assert!(MenuPress::GroupDisplay.navigates());
        assert!(!MenuPress::GroupDetail(Detail::Brief).navigates());
        let before = Settings {
            detail: Detail::Answers,
            tz: Some(60),
            ..Settings::default()
        };
        for (press, want) in [
            (
                MenuPress::GroupDetail(Detail::Brief),
                Display {
                    detail: Detail::Brief,
                    ..Display::default()
                },
            ),
            (
                MenuPress::GroupThinking(false),
                Display {
                    thinking: false,
                    ..Display::default()
                },
            ),
            (
                MenuPress::GroupTurn(TurnView::Compact),
                Display {
                    turn: TurnView::Compact,
                    ..Display::default()
                },
            ),
            (
                MenuPress::GroupRich(false),
                Display {
                    rich: false,
                    ..Display::default()
                },
            ),
        ] {
            let (after, page) = change(&before, press).unwrap();
            assert_eq!(page, Page::GroupDisplay, "{press:?}");
            assert_eq!(after.group, want, "{press:?}");
            assert_eq!(
                Settings {
                    group: before.group,
                    ..after
                },
                before,
                "only the group changes: {press:?}"
            );
        }
    }

    /// TASK-078: `group` is written only when it is not the default, and a
    /// file without it reads as the default.
    #[test]
    fn the_group_display_is_saved_only_when_chosen() {
        let settings: Settings = serde_json::from_str(r#"{"detail":"brief"}"#).unwrap();
        assert_eq!(settings.group, Display::default());
        let written = serde_json::to_value(Settings::default()).unwrap();
        assert!(written.get("group").is_none(), "{written}");
        let chosen = Settings {
            group: Display {
                detail: Detail::Brief,
                ..Display::default()
            },
            ..Settings::default()
        };
        let written = serde_json::to_value(&chosen).unwrap();
        assert_eq!(written["group"]["detail"], "brief", "{written}");
        let back: Settings = serde_json::from_value(written).unwrap();
        assert_eq!(back, chosen);
        let partial: Settings = serde_json::from_str(r#"{"group":{"thinking":false}}"#).unwrap();
        assert_eq!(
            partial.group,
            Display {
                thinking: false,
                ..Display::default()
            }
        );
    }

    /// TASK-078: the display tab switches between the private chat and the
    /// group; each page shows its own choice.
    #[test]
    fn the_display_tab_switches_between_the_views() {
        let settings = Settings {
            detail: Detail::Answers,
            group: Display {
                detail: Detail::Brief,
                thinking: false,
                turn: TurnView::Compact,
                rich: true,
            },
            ..Settings::default()
        };
        let (text, keyboard) = render(&Page::Display, &settings, None, None, None, 0);
        assert!(text.starts_with("Что показывать в темах лички"), "{text}");
        assert!(text.contains("«👥 Группа»"), "{text}");
        let private = labels(&keyboard);
        for want in [
            "· 👁 Показ",
            "✅ 👤 Личка",
            "👥 Группа",
            "✅ Только ответы",
            "💭 Размышления: вкл",
            "✨ Rich-разметка: вкл",
            "✅ Ход: полный",
        ] {
            assert!(private.contains(&want.to_owned()), "{want}: {private:?}");
        }
        assert!(datas(&keyboard).contains(&"menu:rc:0".to_owned()));
        assert!(datas(&keyboard).contains(&"menu:dg".to_owned()));
        let (text, keyboard) = render(&Page::GroupDisplay, &settings, None, None, None, 0);
        assert!(text.starts_with("Что показывать в темах группы"), "{text}");
        assert!(
            text.ends_with("По умолчанию всё, rich-разметка включена. Звук в группе не меняется.")
        );
        let group = labels(&keyboard);
        for want in [
            "· 👁 Показ",
            "👤 Личка",
            "✅ 👥 Группа",
            "✅ Кратко",
            "💭 Размышления: выкл",
            "✨ Rich-разметка: вкл",
            "✅ Ход: сжатый",
        ] {
            assert!(group.contains(&want.to_owned()), "{want}: {group:?}");
        }
        let datas = datas(&keyboard);
        for want in [
            "menu:d",
            "menu:gdl:f",
            "menu:gth:1",
            "menu:gtv:f",
            "menu:grc:0",
        ] {
            assert!(datas.contains(&want.to_owned()), "{want}: {datas:?}");
        }
        assert!(!datas.iter().any(|data| data.starts_with("menu:dl:")));
    }

    /// TASK-075: rich messages are a setting of each view: on by default in
    /// the private chat and (TASK-077) in the group; a group choice written
    /// before stays; each press changes its own view only.
    #[test]
    fn rich_messages_are_a_setting_of_each_view() {
        for press in [
            MenuPress::Rich(false),
            MenuPress::Rich(true),
            MenuPress::GroupRich(false),
            MenuPress::GroupRich(true),
        ] {
            let data = data(&press);
            assert_eq!(parse_callback(&data), Some(press), "{data}");
            assert!(!press.navigates());
        }
        assert_eq!(data(&MenuPress::Rich(true)), "menu:rc:1");
        assert_eq!(data(&MenuPress::GroupRich(false)), "menu:grc:0");
        let old: Settings =
            serde_json::from_str(r#"{"detail":"brief","group":{"detail":"brief"}}"#).unwrap();
        assert!(old.rich);
        assert!(old.display().rich);
        assert!(old.group.rich);
        let chosen: Settings = serde_json::from_str(
            r#"{"group":{"detail":"full","thinking":true,"turn":"full","rich":false}}"#,
        )
        .unwrap();
        assert!(!chosen.group.rich, "a saved choice stays");
        assert!(serde_json::to_value(&chosen).unwrap()["group"]["rich"] == false);
        let (off, page) = change(&Settings::default(), MenuPress::Rich(false)).unwrap();
        assert_eq!(page, Page::Display);
        assert_eq!(
            off,
            Settings {
                rich: false,
                ..Settings::default()
            }
        );
        let (off, page) = change(&Settings::default(), MenuPress::GroupRich(false)).unwrap();
        assert_eq!(page, Page::GroupDisplay);
        assert_eq!(
            off,
            Settings {
                group: Display {
                    rich: false,
                    ..Display::default()
                },
                ..Settings::default()
            }
        );
        assert_eq!(
            change(&off, MenuPress::GroupRich(true)).unwrap().0,
            Settings::default()
        );
    }

    /// TASK-077: the history limit is a radio row of the group page; a press
    /// sets it and stays there; it is written only when not the default.
    #[test]
    fn the_history_limit_is_chosen_on_the_group_page() {
        for limit in HistoryLimit::ALL {
            let press = MenuPress::GroupHistory(limit);
            let data = data(&press);
            assert_eq!(parse_callback(&data), Some(press), "{data}");
            assert!(!press.navigates());
        }
        assert_eq!(
            data(&MenuPress::GroupHistory(HistoryLimit::Short)),
            "menu:ghl:s"
        );
        for junk in ["menu:ghl", "menu:ghl:x", "menu:ghl:m:1"] {
            assert_eq!(parse_callback(junk), None, "{junk}");
        }
        assert_eq!(
            HistoryLimit::ALL.map(HistoryLimit::chars),
            [2000, 4000, 8000]
        );
        let (text, keyboard) = render(
            &Page::GroupDisplay,
            &Settings::default(),
            None,
            None,
            None,
            0,
        );
        assert!(text.contains("📜 История до обращения: "), "{text}");
        let labels = labels(&keyboard);
        for want in ["📜 2000", "✅ 📜 4000", "📜 8000"] {
            assert!(labels.contains(&want.to_owned()), "{want}: {labels:?}");
        }
        let (short, page) = change(
            &Settings::default(),
            MenuPress::GroupHistory(HistoryLimit::Short),
        )
        .unwrap();
        assert_eq!(page, Page::GroupDisplay);
        assert_eq!(
            short,
            Settings {
                history: HistoryLimit::Short,
                ..Settings::default()
            }
        );
        assert_eq!(serde_json::to_value(&short).unwrap()["history"], "short");
        let (_, keyboard) = render(&Page::GroupDisplay, &short, None, None, None, 0);
        assert!(super::tests::labels(&keyboard).contains(&"✅ 📜 2000".to_owned()));
        // The private page has no such row.
        let (_, keyboard) = render(&Page::Display, &short, None, None, None, 0);
        assert!(
            !datas(&keyboard)
                .iter()
                .any(|data| data.starts_with("menu:ghl"))
        );
    }

    /// TASK-077: a shared row says how its group topic takes messages and
    /// offers the other mode; an unshared row has neither.
    #[test]
    fn a_shared_row_switches_between_mentions_and_every_message() {
        let view = SessionsView {
            rows: vec![
                row(2, Some(true), false, None),
                row(3, Some(true), false, None),
                row(4, Some(false), false, None),
                SessionRow {
                    mentions: Some(true),
                    ..row(5, None, false, None)
                },
            ],
            page: 0,
            pages: 1,
            outdated: 0,
        };
        let (text, keyboard) = render(
            &Page::Sessions(0),
            &Settings::default(),
            Some(&view),
            None,
            None,
            0,
        );
        assert!(
            text.contains("\n1. ⚡ [box] project · 2 — работает, в группе, по упоминанию"),
            "{text}"
        );
        assert!(
            text.contains("\n2. ⚡ [box] project · 3 — работает, в группе, все сообщения"),
            "{text}"
        );
        assert!(
            text.contains("\n3. ⚡ [box] project · 4 — работает\n"),
            "{text}"
        );
        assert!(
            text.ends_with("\n4. ⚡ [box] project · 5 — работает"),
            "{text}"
        );
        let labels = labels(&keyboard);
        let datas = datas(&keyboard);
        let at = |label: &str| {
            labels
                .iter()
                .position(|l| l == label)
                .unwrap_or_else(|| panic!("{label}: {labels:?}"))
        };
        assert_eq!(datas[at("📣 1")], "menu:ma:0:2");
        assert_eq!(datas[at("💬 2")], "menu:mn:0:3");
        for absent in ["📣 3", "💬 3", "📣 4", "💬 4"] {
            assert!(!labels.contains(&absent.to_owned()), "{absent}");
        }
        for (code, action) in [
            ("mn", SlotAction::Mentions),
            ("ma", SlotAction::EveryMessage),
        ] {
            assert_eq!(
                parse_callback(&format!("menu:{code}:1:9")),
                Some(MenuPress::Slot {
                    action,
                    page: 1,
                    slot: 9
                })
            );
        }
        // Every code of a row button is its own.
        let codes: Vec<&str> = SlotAction::ALL.iter().map(|action| action.code()).collect();
        let unique: std::collections::BTreeSet<&str> = codes.iter().copied().collect();
        assert_eq!(unique.len(), codes.len(), "{codes:?}");
    }

    /// TASK-069: with several groups a row has one picker button instead of
    /// 👥 and 🙈 and names the groups it shows in, three at most.
    #[test]
    fn a_row_with_several_groups_opens_the_picker_and_names_its_groups() {
        let titles =
            |n: usize| -> Vec<String> { (1..=n).map(|i| format!("Группа {i}")).collect() };
        let picked = |slot: u32, shared: bool, groups: usize| SessionRow {
            pick: true,
            groups: titles(groups),
            ..row(slot, Some(shared), false, None)
        };
        let view = SessionsView {
            rows: vec![
                picked(2, true, 1),
                picked(3, true, 2),
                picked(4, true, 5),
                picked(5, false, 0),
            ],
            page: 0,
            pages: 1,
            outdated: 0,
        };
        let (text, keyboard) = render(
            &Page::Sessions(0),
            &Settings::default(),
            Some(&view),
            None,
            None,
            0,
        );
        assert!(
            text.contains(
                "\n1. ⚡ [box] project · 2 — работает, в группе «Группа 1», по упоминанию"
            ),
            "{text}"
        );
        assert!(
            text.contains(
                "\n2. ⚡ [box] project · 3 — работает, в группах: Группа 1, Группа 2, все сообщения"
            ),
            "{text}"
        );
        assert!(
            text.contains(
                "\n3. ⚡ [box] project · 4 — работает, в группах: Группа 1, Группа 2, Группа 3 и ещё 2, по упоминанию"
            ),
            "{text}"
        );
        assert!(
            text.ends_with("\n4. ⚡ [box] project · 5 — работает"),
            "{text}"
        );
        let labels = labels(&keyboard);
        let datas = datas(&keyboard);
        for n in 1..=4 {
            let at = labels
                .iter()
                .position(|label| *label == format!("👥 {n}…"))
                .unwrap_or_else(|| panic!("{n}: {labels:?}"));
            assert!(datas[at].starts_with("menu:gp:0:"), "{}", datas[at]);
            assert!(!labels.contains(&format!("🙈 {n}")), "{labels:?}");
            assert!(!labels.contains(&format!("👥 {n}")), "{labels:?}");
        }
        assert_eq!(
            parse_callback("menu:gp:1:9"),
            Some(MenuPress::Slot {
                action: SlotAction::Groups,
                page: 1,
                slot: 9
            })
        );
    }
}
