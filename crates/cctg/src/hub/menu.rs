//! The menu in the General of a person's private chat with the bot (TASK-073).
//!
//! One message there, pinned, edited in place: its tabs show the person's
//! sessions (with buttons to lift a topic, share or unshare it, stop a turn
//! and update the clients), what the topics of the private chat show and
//! how loud they are. Settings belong to the person and live in
//! `registry.json` ([`Person`], by private chat). Only the person sees and
//! changes them: a private chat is its user's, and a press counts only in
//! the private chat it came from. The detail level filters the stream of a
//! slot that shows in the owner's private chat alone; while the slot shows
//! anywhere else too, every view gets the full layout. The sound setting
//! changes new messages into the private chat only; the group sounds as
//! before. The menu does not refresh itself: ↻ does.
//!
//! This module is pure: data, rendering and the press codes. The slot actor
//! sends, pins and edits.

use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use super::chat::PrivateChat;
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
    pub sound: Sound,
    #[serde(
        skip_serializing_if = "Option::is_none",
        deserialize_with = "hours_of_a_day"
    )]
    pub quiet: Option<Quiet>,
    /// Minutes from UTC, [`ZONE_MIN`]..=[`ZONE_MAX`]; no daylight saving.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tz: Option<i16>,
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            detail: Detail::Full,
            thinking: true,
            sound: Sound::Replies,
            quiet: None,
            tz: None,
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

/// `piece` shows in the topics of a person with `settings`. Turn answers,
/// subagent blocks, prompts, questions and notices are no pieces: they
/// always show.
pub fn shows(settings: &Settings, piece: Piece) -> bool {
    match piece {
        Piece::Prompt | Piece::Interrupt => true,
        Piece::Thinking => settings.thinking,
        Piece::Text => settings.detail != Detail::Answers,
        Piece::Tool => settings.detail == Detail::Full,
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
        | MenuPress::Sound
        | MenuPress::Zone { .. }
        | MenuPress::Slot { .. }
        | MenuPress::UpdateAll { .. } => return Err((Page::Sessions(0), ANSWER_UNCHANGED)),
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
}

impl SlotAction {
    const ALL: [Self; 6] = [
        Self::Open,
        Self::Share,
        Self::Unshare,
        Self::UnshareConfirm,
        Self::Stop,
        Self::StopConfirm,
    ];

    fn code(self) -> &'static str {
        match self {
            Self::Open => "o",
            Self::Share => "sh",
            Self::Unshare => "us",
            Self::UnshareConfirm => "uc",
            Self::Stop => "st",
            Self::StopConfirm => "sc",
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
    SoundMode(Sound),
    /// Quiet hours on or off, as [`MenuPress::Thinking`].
    Quiet(bool),
    QuietFrom(u8),
    QuietTo(u8),
    SetZone(i16),
}

impl MenuPress {
    /// Only shows another page.
    pub fn navigates(self) -> bool {
        matches!(
            self,
            Self::Sessions { .. } | Self::Display | Self::Sound | Self::Zone { .. }
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
        MenuPress::Sound => "n".to_owned(),
        MenuPress::Zone { fractions } => format!("z:{}", u8::from(fractions)),
        MenuPress::Slot { action, page, slot } => format!("{}:{page}:{slot}", action.code()),
        MenuPress::UpdateAll { page } => format!("up:{page}"),
        MenuPress::Detail(detail) => format!("dl:{}", detail_code(detail)),
        MenuPress::Thinking(on) => format!("th:{}", u8::from(on)),
        MenuPress::SoundMode(sound) => format!("sn:{}", sound_code(sound)),
        MenuPress::Quiet(on) => format!("q:{}", u8::from(on)),
        MenuPress::QuietFrom(hour) => format!("qf:{hour}"),
        MenuPress::QuietTo(hour) => format!("qt:{hour}"),
        MenuPress::SetZone(tz) => format!("tz:{tz}"),
    };
    format!("{PREFIX}{rest}")
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
        ["n"] => MenuPress::Sound,
        ["z", "0"] => MenuPress::Zone { fractions: false },
        ["z", "1"] => MenuPress::Zone { fractions: true },
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
    Sound,
    Zone { fractions: bool },
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
/// `sessions` is the sessions tab's page (`None` shows it empty).
pub fn render(
    page: &Page,
    settings: &Settings,
    sessions: Option<&SessionsView>,
    unix_secs: i64,
) -> (String, Value) {
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
        tab("👁 Показ", *page == Page::Display, MenuPress::Display),
        tab(
            "🔔 Звук",
            matches!(page, Page::Sound | Page::Zone { .. }),
            MenuPress::Sound,
        ),
    ]];
    let text = match page {
        Page::Sessions(_) => render_sessions(sessions, &mut rows),
        Page::Display => render_display(settings, &mut rows),
        Page::Sound => render_sound(settings, &mut rows),
        Page::Zone { fractions } => render_zone(*fractions, unix_secs, &mut rows),
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
        let group = if row.shared == Some(true) {
            ", в группе"
        } else {
            ""
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

fn render_display(settings: &Settings, rows: &mut Vec<Vec<Value>>) -> String {
    let detail = |label: &str, detail: Detail| {
        button(
            radio(label, settings.detail == detail),
            MenuPress::Detail(detail),
        )
    };
    rows.push(vec![
        detail("Всё", Detail::Full),
        detail("Кратко", Detail::Brief),
        detail("Только ответы", Detail::Answers),
    ]);
    let thinking = if settings.thinking {
        "вкл"
    } else {
        "выкл"
    };
    rows.push(vec![button(
        format!("💭 Размышления: {thinking}"),
        MenuPress::Thinking(!settings.thinking),
    )]);
    "Что показывать в темах лички\n\n\
Всё: промпты из терминала, текст Claude и строка на каждый вызов инструмента.\n\
Кратко: без строк инструментов.\n\
Только ответы: промпты из терминала и ответ хода.\n\
💭 размышления включаются отдельно, при любой подробности.\n\
Ответы хода, запросы разрешений, вопросы и субагенты видны всегда.\n\n\
Пока сессия показана и в группе, показ полный и в личке, и в группе."
        .to_owned()
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
        }
    }

    fn all_pages(settings: &Settings, view: &SessionsView) -> Vec<(String, Value)> {
        [
            Page::Sessions(view.page),
            Page::Display,
            Page::Sound,
            Page::Zone { fractions: false },
            Page::Zone { fractions: true },
        ]
        .iter()
        .map(|page| render(page, settings, Some(view), 1_700_000_000))
        .collect()
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
                assert!(shows(&settings, Piece::Prompt));
                assert!(shows(&settings, Piece::Interrupt));
                assert_eq!(shows(&settings, Piece::Thinking), thinking);
                assert_eq!(shows(&settings, Piece::Text), detail != Detail::Answers);
                assert_eq!(shows(&settings, Piece::Tool), detail == Detail::Full);
            }
        }
    }

    #[test]
    fn the_default_settings_are_todays_behaviour() {
        let settings = Settings::default();
        assert_eq!(settings.detail, Detail::Full);
        assert!(settings.thinking);
        assert_eq!(settings.sound, Sound::Replies);
        assert_eq!(settings.quiet, None);
        assert_eq!(settings.tz, None);
        for piece in [
            Piece::Prompt,
            Piece::Text,
            Piece::Thinking,
            Piece::Tool,
            Piece::Interrupt,
        ] {
            assert!(shows(&settings, piece));
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
            serde_json::from_str(r#"{"detail":"later","sound":"later"}"#).unwrap();
        assert_eq!(settings.detail, Detail::Full);
        assert_eq!(settings.sound, Sound::Replies);
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
        })
        .unwrap();
        assert_eq!(
            written,
            json!({"detail": "answers", "thinking": false, "sound": "off",
                   "quiet": {"from": 22, "to": 7}, "tz": 180})
        );
        let back: Settings = serde_json::from_value(written).unwrap();
        assert_eq!(back.detail, Detail::Answers);
        assert_eq!(
            serde_json::to_value(Settings::default()).unwrap(),
            json!({"detail": "full", "thinking": true, "sound": "replies"})
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
        let (text, keyboard) = render(&Page::Sessions(0), &Settings::default(), Some(&first), 0);
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
        let (text, keyboard) = render(&Page::Sessions(1), &Settings::default(), Some(&last), 0);
        assert!(text.starts_with("Сессии (стр. 2/2)"), "{text}");
        let labels = super::tests::labels(&keyboard);
        assert!(labels.contains(&"‹".to_owned()));
        assert!(!labels.contains(&"›".to_owned()));
        assert!(labels.contains(&"🙈 1".to_owned()));
        assert!(labels.contains(&"⬆️ Обновить все клиенты".to_owned()));
        // None at all.
        let (text, _) = render(&Page::Sessions(0), &Settings::default(), None, 0);
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
        let (_, keyboard) = render(&Page::Sound, &settings, None, now);
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
        let (_, keyboard) = render(&Page::Display, &before, None, 0);
        assert!(datas(&keyboard).contains(&"menu:th:0".to_owned()));
        let (_, keyboard) = render(&Page::Sound, &before, None, 0);
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
        let (_, keyboard) = render(&Page::Sound, &on, None, 0);
        assert!(datas(&keyboard).contains(&"menu:q:0".to_owned()));
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
        let (_, keyboard) = render(&Page::Sound, &wild, None, 0);
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
}
