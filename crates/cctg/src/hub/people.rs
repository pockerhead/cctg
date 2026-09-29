//! People owners add to the allowlist from the menu (TASK-081).
//!
//! The owners are `CCTG_ALLOWED_USER_IDS`; the members they add live in
//! `registry.json` ([`Member`]) and count at once through the shared
//! [`super::config::Allowlist`]. An owner adds a person by forwarding one of
//! their messages into the General of the owner's private chat, or, when
//! Telegram hides that person's account in forwards, with a one-time link
//! `t.me/<bot>?start=inv_<code>`; either way the owner confirms with a
//! button. Proposals and links live in memory only ([`Book`]): a restart of
//! the hub drops them.
//!
//! This module is pure: data, limits and texts. The slot actor sends and
//! edits. Nothing here prints an id, a name or a code in `Debug`.

use std::fmt;
use std::time::{Duration, Instant};

use base64::Engine;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use serde::{Deserialize, Serialize};
use subtle::ConstantTimeEq;

use super::chat::PrivateChat;

/// Members at most.
pub const MAX_MEMBERS: usize = 30;
/// Proposals waiting for the owner's answer, at most.
pub const MAX_PENDING: usize = 16;
/// A proposal's «Добавить» counts this long.
pub const PENDING_TTL: Duration = Duration::from_secs(60 * 60);
/// Unused invite links at most.
pub const MAX_INVITES: usize = 8;
/// An invite link counts this long.
pub const INVITE_TTL: Duration = Duration::from_secs(10 * 60);
/// The `/start` payload of an invite link: this prefix and the code.
pub const INVITE_PREFIX: &str = "inv_";
/// Characters of an invite code: 16 random bytes in base64url.
pub const INVITE_LEN: usize = 22;

/// A person an owner added from the menu. `Debug` prints nothing of them.
#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Member {
    pub chat: PrivateChat,
    /// Their name when they were added; shown in the menu, never logged.
    pub name: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub username: Option<String>,
    /// Their number in the menu's buttons.
    pub key: u32,
}

impl fmt::Debug for Member {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("Member(..)")
    }
}

impl Member {
    /// How the menu names them.
    pub fn label(&self) -> String {
        label(&self.name, self.username.as_deref())
    }
}

/// `Имя (@username)`, `@username` when the name is the username, or the
/// name alone.
pub fn label(name: &str, username: Option<&str>) -> String {
    match username {
        Some(username) if username == name => format!("@{username}"),
        Some(username) => format!("{name} (@{username})"),
        None => name.to_owned(),
    }
}

/// The name of a person Telegram named nothing of.
pub const NO_NAME: &str = "без имени";

/// `word` is the `/start` payload of an invite link: [`INVITE_PREFIX`] and
/// [`INVITE_LEN`] characters of base64url.
pub fn is_invite_payload(word: &str) -> bool {
    word.strip_prefix(INVITE_PREFIX).is_some_and(|code| {
        code.len() == INVITE_LEN
            && code
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_'))
    })
}

/// The invite link of `code` for bot `bot_username`.
pub fn invite_link(bot_username: &str, code: &str) -> String {
    format!("https://t.me/{bot_username}?start={INVITE_PREFIX}{code}")
}

/// Why the [`Book`] took nothing.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Refusal {
    /// The same owner already waits to confirm the same person.
    Waiting,
    /// Too many wait already.
    Full,
}

/// A person an owner may add with «Добавить».
pub struct Pending {
    pub token: u32,
    pub owner: PrivateChat,
    pub user: PrivateChat,
    pub name: String,
    pub username: Option<String>,
    until: Instant,
}

struct Invite {
    owner: PrivateChat,
    code: String,
    until: Instant,
}

/// Proposals and invite links, in memory. No `Debug`: it holds names and
/// codes.
pub struct Book {
    pending: Vec<Pending>,
    invites: Vec<Invite>,
    next_token: u32,
}

impl Default for Book {
    /// Tokens start at a random number: a «Добавить» button of an earlier
    /// run of the hub never confirms a new proposal with its number.
    fn default() -> Self {
        let mut seed = [0u8; 4];
        aws_lc_rs::rand::fill(&mut seed).expect("the system random generator works");
        Self {
            pending: Vec::new(),
            invites: Vec::new(),
            next_token: u32::from_le_bytes(seed),
        }
    }
}

impl Book {
    fn prune(&mut self, now: Instant) {
        self.pending.retain(|pending| now < pending.until);
        self.invites.retain(|invite| now < invite.until);
    }

    /// `owner` may add `user` with the returned token.
    pub fn propose(
        &mut self,
        owner: PrivateChat,
        user: PrivateChat,
        name: String,
        username: Option<String>,
        now: Instant,
    ) -> Result<u32, Refusal> {
        self.prune(now);
        if self
            .pending
            .iter()
            .any(|pending| pending.owner == owner && pending.user == user)
        {
            return Err(Refusal::Waiting);
        }
        if self.pending.len() >= MAX_PENDING {
            return Err(Refusal::Full);
        }
        let token = self.next_token;
        self.next_token = self.next_token.wrapping_add(1);
        self.pending.push(Pending {
            token,
            owner,
            user,
            name,
            username,
            until: now + PENDING_TTL,
        });
        Ok(token)
    }

    /// The proposal `token` of `owner`, gone from the book; `None` when it
    /// is someone else's, expired or taken.
    pub fn take(&mut self, owner: PrivateChat, token: u32, now: Instant) -> Option<Pending> {
        self.prune(now);
        let at = self
            .pending
            .iter()
            .position(|pending| pending.owner == owner && pending.token == token)?;
        Some(self.pending.remove(at))
    }

    /// A new one-time invite code of `owner`.
    pub fn mint_invite(&mut self, owner: PrivateChat, now: Instant) -> Result<String, Refusal> {
        self.prune(now);
        if self.invites.len() >= MAX_INVITES {
            return Err(Refusal::Full);
        }
        let mut bytes = [0u8; 16];
        aws_lc_rs::rand::fill(&mut bytes).expect("the system random generator works");
        let code = URL_SAFE_NO_PAD.encode(bytes);
        self.invites.push(Invite {
            owner,
            code: code.clone(),
            until: now + INVITE_TTL,
        });
        Ok(code)
    }

    /// The owner of invite `code`, which is spent now; `None` for an
    /// unknown, used or expired code.
    pub fn redeem(&mut self, code: &str, now: Instant) -> Option<PrivateChat> {
        self.prune(now);
        let mut found = None;
        // Every code is compared, in constant time each.
        for (at, invite) in self.invites.iter().enumerate() {
            if invite.code.len() == code.len()
                && bool::from(invite.code.as_bytes().ct_eq(code.as_bytes()))
            {
                found = Some(at);
            }
        }
        found.map(|at| self.invites.remove(at).owner)
    }
}

// ------------------------------------------------------------------ texts

pub const ONLY_OWNER_ADDS: &str = "Добавлять людей может только владелец.";
pub const NO_BOTS: &str = "Бота добавить нельзя.";
pub const ALREADY: &str = "Он(а) уже в списке.";
pub const WAITING: &str = "Это подтверждение уже ждёт выше.";
pub const TOO_MANY_PENDING: &str =
    "Слишком много неподтверждённых добавлений; подтвердите или отмените их.";
pub const HIDDEN: &str = "Telegram скрывает аккаунт этого человека в пересылках. Дайте ему одноразовую ссылку-приглашение.";
pub const OTHER: &str = "Это сообщение от имени чата или канала: по нему человека не добавить. Перешлите его личное сообщение или дайте ссылку-приглашение.";
pub const INVITE_RECEIVED: &str = "Приглашение получено: владелец подтвердит доступ.";
pub const STALE: &str = "Подтверждение устарело.";
pub const CANCELLED: &str = "Отменено.";
pub const FAREWELL: &str =
    "Владелец закрыл вам доступ к cctg: бот больше не принимает ваши сообщения.";
/// Answers to presses (at most 200 characters each).
pub const ANSWER_OWNER_ONLY: &str = "Список людей меняет владелец";
pub const ANSWER_GONE: &str = "Его уже нет в списке";
pub const ANSWER_NO_USERNAME: &str = "У бота нет username: ссылку не сделать";
/// The button that makes an invite link.
pub const INVITE_BUTTON: &str = "🔗 Ссылка-приглашение";

/// The list is full.
pub fn full_list() -> String {
    format!("В списке уже {MAX_MEMBERS} человек: сначала удалите кого-нибудь.")
}

/// Every invite link is taken.
pub fn invites_full() -> String {
    format!("Уже есть {MAX_INVITES} неиспользованных ссылок; новая будет, когда они истекут.")
}

/// The owner's question before `label` is added.
pub fn confirm_text(label: &str) -> String {
    format!(
        "Добавить «{label}» в cctg? Он(а) сможет писать сессиям в группах и отвечать на вопросы и разрешения, как вы. Устройства и список людей остаются за владельцами."
    )
}

/// The invite link message.
pub fn invite_text(link: &str) -> String {
    format!(
        "Одноразовая ссылка-приглашение на 10 минут: {link}\nОтправьте её человеку; когда он откроет её и нажмёт Start, я спрошу вас, добавить ли его."
    )
}

/// The confirmation after `label` was added; `bot`: the bot's username.
pub fn added_text(label: &str, bot: Option<&str>) -> String {
    let open = match bot {
        Some(bot) => format!("пусть откроет @{bot} и нажмёт Start"),
        None => "пусть откроет бота и нажмёт Start".to_owned(),
    };
    format!(
        "Добавлен(а): {label}. Он(а) уже может писать сессиям. Если он(а) ещё не открывал(а) бота, {open}: без этого бот не может написать ему в личку."
    )
}

/// The greeting of a new member.
pub fn welcome_text(bot: Option<&str>) -> String {
    match bot {
        Some(bot) => format!(
            "Вам открыт доступ к cctg: пишите в темы сессий в группе (если тема отвечает только на обращения, начните с @{bot})."
        ),
        None => "Вам открыт доступ к cctg: пишите в темы сессий в группе.".to_owned(),
    }
}

/// The answer to a removal.
pub fn removed_answer(label: &str, devices: usize) -> String {
    format!("Удалён(а): {label}. Устройств отключено: {devices}")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn chat(id: i64) -> PrivateChat {
        PrivateChat::of_user(id)
    }

    #[test]
    fn invite_payloads_are_the_prefix_and_22_base64url_characters() {
        let code = "AbCdEfGhIjKlMnOpQr-_09";
        assert_eq!(code.len(), INVITE_LEN);
        assert!(is_invite_payload(&format!("inv_{code}")));
        for bad in [
            "inv_short",
            "inv_AbCdEfGhIjKlMnOpQr-_0!",
            "inv_AbCdEfGhIjKlMnOpQr-_090",
            "AbCdEfGhIjKlMnOpQr-_09",
            "INV_AbCdEfGhIjKlMnOpQr-_09",
            "",
        ] {
            assert!(!is_invite_payload(bad), "{bad}");
        }
        let link = invite_link("cctg_bot", code);
        assert_eq!(link, format!("https://t.me/cctg_bot?start=inv_{code}"));
        let payload = link.split("start=").nth(1).unwrap();
        assert!(payload.len() <= 64 && is_invite_payload(payload));
    }

    #[test]
    fn minted_codes_are_one_time_and_expire() {
        let mut book = Book::default();
        let now = Instant::now();
        let code = book.mint_invite(chat(1), now).unwrap();
        assert!(
            is_invite_payload(&format!("{INVITE_PREFIX}{code}")),
            "{code}"
        );
        assert_eq!(book.redeem("AAAAAAAAAAAAAAAAAAAAAA", now), None);
        assert_eq!(book.redeem(&code, now), Some(chat(1)));
        assert_eq!(book.redeem(&code, now), None, "one time");

        let late = book.mint_invite(chat(1), now).unwrap();
        assert_eq!(book.redeem(&late, now + INVITE_TTL), None, "expired");

        for _ in 0..MAX_INVITES {
            book.mint_invite(chat(2), now).unwrap();
        }
        assert_eq!(book.mint_invite(chat(2), now), Err(Refusal::Full));
        assert!(book.mint_invite(chat(2), now + INVITE_TTL).is_ok());
    }

    #[test]
    fn proposals_belong_to_their_owner_and_expire() {
        let mut book = Book::default();
        let now = Instant::now();
        let token = book
            .propose(chat(1), chat(9), "Анна".into(), None, now)
            .unwrap();
        assert_eq!(
            book.propose(chat(1), chat(9), "Анна".into(), None, now)
                .unwrap_err(),
            Refusal::Waiting
        );
        assert!(book.take(chat(2), token, now).is_none(), "another owner's");
        assert!(book.take(chat(1), token.wrapping_add(1), now).is_none());
        let taken = book.take(chat(1), token, now).unwrap();
        assert_eq!((taken.owner, taken.user), (chat(1), chat(9)));
        assert!(book.take(chat(1), token, now).is_none(), "taken once");

        let token = book
            .propose(chat(1), chat(9), "Анна".into(), None, now)
            .unwrap();
        assert!(book.take(chat(1), token, now + PENDING_TTL).is_none());

        for user in 0..MAX_PENDING as i64 {
            book.propose(chat(1), chat(100 + user), "x".into(), None, now)
                .unwrap();
        }
        assert_eq!(
            book.propose(chat(1), chat(99), "x".into(), None, now)
                .unwrap_err(),
            Refusal::Full
        );
    }

    #[test]
    fn tokens_start_at_random() {
        let starts: Vec<u32> = (0..8).map(|_| Book::default().next_token).collect();
        assert!(starts.iter().any(|start| *start != starts[0]), "{starts:?}");
    }

    #[test]
    fn labels_and_debug_show_no_id() {
        let member = Member {
            chat: chat(7_319_402_518),
            name: "Анна".into(),
            username: Some("anna".into()),
            key: 1,
        };
        assert_eq!(member.label(), "Анна (@anna)");
        assert_eq!(label("anna", Some("anna")), "@anna");
        assert_eq!(label("Анна", None), "Анна");
        assert_eq!(format!("{member:?}"), "Member(..)");
        let json = serde_json::to_string(&member).unwrap();
        let back: Member = serde_json::from_str(&json).unwrap();
        assert_eq!(back, member);
        assert!(removed_answer(&"я".repeat(80), 3).chars().count() <= 200);
    }
}
