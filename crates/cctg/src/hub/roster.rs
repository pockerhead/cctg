//! `/devices` in General and its «Отозвать» buttons (TASK-045), and `/join`
//! in General (TASK-046): a fresh one-time code inside a ready client
//! install line.
//!
//! One worker, like the transcript commands: `/devices` sends the list of
//! enrolled devices with one button per device; a press asks once more
//! («Да, отозвать» / «Отмена») on the same message, and the confirmation
//! revokes the device at once ([`Devices::revoke`]) and edits the message
//! back into the list. Only allowlisted users get here (the poll drops the
//! rest). Logs carry device ids, never names, codes or secrets.
//!
//! `/join` answers with `curl .../<release>/install.sh | sh -s -- <hub
//! address> [--pin <sha256>] --join <code>`; the address and pin say how
//! devices reach the hub ([`JoinInfo`]), no secret is in it. The message
//! stays in the group's history, so it is edited to say the code was used
//! (device name and id) or, after [`CODE_TTL`], expired. A hub restart
//! loses those edits (its codes expire all the same). The user who asked
//! for the code owns the device it enrolls (TASK-063): its sessions show
//! in that user's private chat too.

use std::collections::HashMap;
use std::net::{IpAddr, Ipv4Addr, Ipv6Addr, SocketAddr};
use std::time::SystemTime;

use serde_json::{Value, json};
use tokio::sync::broadcast::error::RecvError;
use tokio::sync::mpsc;
use tokio::time::Instant;
use tracing::{info, warn};

use super::chat::{Chat, MessageKey, PrivateChat};
use super::config::PublicAddrs;
use super::devices::{CODE_TTL, Devices, Listed, MAX_CODES, MintError, SharedState, code_key};
use super::scheduler::{Op, Outbox, Outcome};
use super::updates::{CallbackInput, Inbound};
use crate::tls::CertPin;

const PREFIX: &str = "dev";
const ASK: &str = "r";
const YES: &str = "y";
const NO: &str = "n";

/// What the worker takes.
#[derive(Debug)]
pub enum Input {
    Command(Inbound),
    Press(CallbackInput),
}

/// Where the install script of a release is: `<RAW_BASE>/<tag>/install.sh`.
pub const RAW_BASE: &str = "https://raw.githubusercontent.com/pockerhead/cctg";
/// The ports install.sh takes for `--hub-host`.
const AGENT_PORT: u16 = 47291;
const HOOK_PORT: u16 = 47292;
/// What the `/join` message says once its code expired.
pub const EXPIRED: &str = "Код из этого сообщения истёк. Новый: /join.";

/// What the `/join` message says once a device took its code.
fn used_text(id: &str, name: &str) -> String {
    format!(
        "Код из этого сообщения использован: подключено устройство «{name}» ({id}). Новый код: /join."
    )
}

/// The command of a General text: `devices` or `join` (`/name`,
/// optionally `@bot`, any case).
fn command_name(input: &Inbound) -> Option<&'static str> {
    if input.thread_id.is_some() {
        return None;
    }
    let name = input
        .text
        .as_deref()
        .and_then(|text| text.split_whitespace().next())
        .and_then(|word| word.strip_prefix('/'))
        .map(|head| head.split_once('@').map_or(head, |(name, _)| name))?;
    ["devices", "join"]
        .into_iter()
        .find(|known| name.eq_ignore_ascii_case(known))
}

/// `/devices` or `/join` (optionally `@bot`) in General. The same text in a
/// topic is a message for its session, like every other slash text there
/// (TASK-021).
pub fn is_command(input: &Inbound) -> bool {
    command_name(input).is_some()
}

/// What `/join` puts in the install line besides the code.
#[derive(Debug, Clone)]
pub struct JoinInfo {
    /// The release of this hub's build: the install script of that tag
    /// installs the same build. `None`: a local build (`main` then).
    pub release: Option<String>,
    /// How other devices reach the hub ([`super::config::PUBLIC_AGENT_VAR`]).
    pub public: Option<PublicAddrs>,
    /// The listeners as the hub's own machine reaches them.
    pub agent_listen: SocketAddr,
    pub hook_listen: SocketAddr,
    /// The certificate's sha256 when the listeners speak TLS.
    pub pin: Option<CertPin>,
}

impl JoinInfo {
    /// A line for other machines needs their address and TLS: a device
    /// talks to another machine only with a pin.
    fn for_other_machines(&self) -> bool {
        self.public.is_some() && self.pin.is_some()
    }

    /// The install line with `code`.
    pub fn line(&self, code: &str) -> String {
        let tag = self.release.as_deref().unwrap_or("main");
        let (agent, hook) = match &self.public {
            Some(public) if self.for_other_machines() => {
                (public.agent.clone(), public.hook.clone())
            }
            _ => (local_addr(self.agent_listen), local_addr(self.hook_listen)),
        };
        let mut args = hub_args(&agent, &hook);
        if let Some(pin) = &self.pin {
            let hex = pin.to_string().replace(':', "").to_ascii_lowercase();
            args.push_str(&format!(" --pin {hex}"));
        }
        format!("curl -fsSL {RAW_BASE}/{tag}/install.sh | sh -s -- {args} --join {code}")
    }
}

/// A listener as the hub's machine reaches it: a wildcard or loopback
/// address is 127.0.0.1, or [::1] for IPv6 (on Windows `[::]` takes IPv6
/// only).
fn local_addr(listen: SocketAddr) -> String {
    let ip = match listen.ip() {
        IpAddr::V4(ip) if ip.is_unspecified() || ip.is_loopback() => {
            IpAddr::V4(Ipv4Addr::LOCALHOST)
        }
        IpAddr::V6(ip) if ip.is_unspecified() || ip.is_loopback() => {
            IpAddr::V6(Ipv6Addr::LOCALHOST)
        }
        ip => ip,
    };
    SocketAddr::new(ip, listen.port()).to_string()
}

/// A shell word: an [IPv6] address is quoted, it is a glob pattern
/// otherwise (zsh fails on it). The values are checked shell-safe
/// (config::public_addr), so single quotes suffice.
fn word(value: &str) -> String {
    if value.contains('[') {
        format!("'{value}'")
    } else {
        value.to_owned()
    }
}

/// `--hub-host H` when both are on one host at the usual ports, else both
/// addresses.
fn hub_args(agent: &str, hook: &str) -> String {
    let split = |addr: &str| {
        addr.rsplit_once(':')
            .map(|(host, port)| (host.to_owned(), port.parse::<u16>().ok()))
    };
    match (split(agent), split(hook)) {
        (Some((a, Some(AGENT_PORT))), Some((h, Some(HOOK_PORT)))) if a == h => {
            format!("--hub-host {}", word(&a))
        }
        _ => format!("--agent-addr {} --hook-addr {}", word(agent), word(hook)),
    }
}

/// The `/join` answer: plain text and its HTML (the line in a code block,
/// one tap copies it).
fn join_message(info: &JoinInfo, code: &str) -> (String, String) {
    let line = info.line(code);
    let minutes = CODE_TTL.as_secs() / 60;
    let before = if info.for_other_machines() {
        format!(
            "Установка cctg на новую машину: выполните строку на ней (Linux, macOS; Windows в Git Bash). Код в строке одноразовый и действует {minutes} минут; это сообщение видят все участники группы."
        )
    } else {
        format!(
            "Этот hub пускает устройства только со своей машины: у него нет адреса для других машин или TLS. Строка для машины hub (код одноразовый, {minutes} минут):"
        )
    };
    let mut after = Vec::new();
    if !info.for_other_machines() {
        after.push("Для других машин нужен hub с TLS и адресом: install.sh --hub --local --public-host ХОСТ или hub на сервере (install.sh --hub).");
    }
    if info.release.is_none() {
        after.push("Hub собран не из релиза: скрипт с main ставит последний релиз, сборка клиента будет другой.");
    }
    let mut text = format!(
        "{before}

{line}"
    );
    let mut html = format!(
        "{}

<pre>{}</pre>",
        transcript::escape_html(&before),
        transcript::escape_html(&line)
    );
    for note in after {
        text.push_str(&format!(
            "

{note}"
        ));
        html.push_str(&format!(
            "

{}",
            transcript::escape_html(note)
        ));
    }
    (text, html)
}

/// A button of this worker.
pub fn is_callback(input: &CallbackInput) -> bool {
    input.data.as_deref().and_then(parse_callback).is_some()
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum Press {
    Ask(String),
    Yes(String),
    No,
}

fn parse_callback(data: &str) -> Option<Press> {
    let mut parts = data.split(':');
    if parts.next()? != PREFIX {
        return None;
    }
    let press = match (parts.next()?, parts.next()) {
        (ASK, Some(id)) if is_id(id) => Press::Ask(id.to_owned()),
        (YES, Some(id)) if is_id(id) => Press::Yes(id.to_owned()),
        (NO, None) => return Some(Press::No),
        _ => return None,
    };
    parts.next().is_none().then_some(press)
}

fn is_id(id: &str) -> bool {
    id.len() == 8 && id.bytes().all(|byte| byte.is_ascii_hexdigit())
}

/// Serves `/devices`, its buttons and `/join` until `inputs` closes and
/// every `/join` message got its used or expired edit.
pub async fn serve(
    mut inputs: mpsc::UnboundedReceiver<Input>,
    outbox: Outbox,
    devices: Devices,
    join: JoinInfo,
    bot_username: Option<String>,
) {
    let mut spent = devices.spent();
    // Code key -> the `/join` message, when its code expires and who asked
    // for it. At most MAX_CODES: minting refuses beyond that.
    let mut waiting: HashMap<String, (MessageKey, Instant, PrivateChat)> = HashMap::new();
    let mut inputs_open = true;
    let mut spent_open = true;
    while inputs_open || !waiting.is_empty() {
        let next = waiting.values().map(|&(_, at, _)| at).min();
        tokio::select! {
            input = inputs.recv(), if inputs_open => match input {
                None => inputs_open = false,
                Some(Input::Command(input)) => {
                    if addressed_elsewhere(input.text.as_deref(), bot_username.as_deref()) {
                        continue;
                    }
                    if command_name(&input) == Some("join") {
                        if let Some((key, message)) = on_join(&outbox, &devices, &join, input.chat).await {
                            waiting.insert(key, (message, Instant::now() + CODE_TTL, input.sender));
                        }
                        continue;
                    }
                    let (text, keyboard) = list(&devices, None);
                    submit(
                        &outbox,
                        Op::Send { chat: input.chat, thread_id: None, text, html: None, rich: None, reply_markup: keyboard,
                            permission: false,
                            reply_to: None,
                            notify: false,
                        },
                    )
                    .await;
                }
                Some(Input::Press(press)) => on_press(&outbox, &devices, press).await,
            },
            used = spent.recv(), if spent_open => match used {
                Ok(used) => {
                    if let Some((message, _, owner)) = waiting.remove(&used.key) {
                        info!(device_id = used.id, "join message marked used");
                        own(&devices, &used.id, owner).await;
                        edit(&outbox, message, used_text(&used.id, &used.name)).await;
                    }
                }
                // Missed ones get the expiry edit.
                Err(RecvError::Lagged(_)) => {}
                Err(RecvError::Closed) => spent_open = false,
            },
            () = tokio::time::sleep_until(next.unwrap_or_else(Instant::now)), if next.is_some() => {
                let now = Instant::now();
                let due: Vec<String> = waiting
                    .iter()
                    .filter(|(_, (_, at, _))| *at <= now)
                    .map(|(key, _)| key.clone())
                    .collect();
                for key in due {
                    if let Some((message, _, _)) = waiting.remove(&key) {
                        edit(&outbox, message, EXPIRED.to_owned()).await;
                    }
                }
            }
        }
    }
}

/// Device `id` belongs to `owner` now, the user whose `/join` enrolled it.
async fn own(devices: &Devices, id: &str, owner: PrivateChat) {
    let (owning, target) = (devices.clone(), id.to_owned());
    match tokio::task::spawn_blocking(move || owning.set_owner(&target, owner)).await {
        Ok(Ok(true)) => info!(device_id = id, "device owner recorded"),
        Ok(Ok(false)) => {}
        Ok(Err(error)) => {
            warn!(device_id = id, kind = ?error.kind(), "device owner not saved");
        }
        Err(_) => warn!(device_id = id, "device owner not saved"),
    }
}

async fn edit(outbox: &Outbox, message: MessageKey, text: String) {
    submit(
        outbox,
        Op::Edit {
            chat: message.chat,
            message_id: message.id,
            text,
            reply_markup: None,
            background: false,
        },
    )
    .await;
}

/// `/devices@other_bot`.
fn addressed_elsewhere(text: Option<&str>, bot: Option<&str>) -> bool {
    let target = text
        .and_then(|text| text.split_whitespace().next())
        .and_then(|word| word.split_once('@'))
        .map(|(_, target)| target);
    matches!((target, bot), (Some(target), Some(bot)) if !target.eq_ignore_ascii_case(bot))
}

/// `/join`: a new code in a new message of `chat`; `Some((code key, message))`
/// when the line went out ([`serve`] edits the message later). A failed
/// mint is answered with why, never with a line.
async fn on_join(
    outbox: &Outbox,
    devices: &Devices,
    join: &JoinInfo,
    chat: Chat,
) -> Option<(String, MessageKey)> {
    let minting = devices.clone();
    let minted = tokio::task::spawn_blocking(move || minting.mint_code())
        .await
        .unwrap_or(Err(MintError::NoHub));
    let mut key = None;
    let (text, html) = match minted {
        Ok(code) => {
            info!("join code minted from Telegram");
            key = code_key(&code);
            let (text, html) = join_message(join, &code);
            (text, Some(html))
        }
        Err(error) => {
            warn!(%error, "no join code for /join");
            let text = match error {
                MintError::Full => format!(
                    "Уже ждут {MAX_CODES} неиспользованных кодов; новый будет, когда они истекут."
                ),
                MintError::NoHub | MintError::Io(_) => {
                    "Код сделать не вышло: hub не может записать его в свой каталог состояния (подробности в логе hub).".to_owned()
                }
            };
            (text, None)
        }
    };
    let op = Op::Send {
        chat,
        thread_id: None,
        text,
        html,
        rich: None,
        reply_markup: None,
        permission: false,
        reply_to: None,
        notify: false,
    };
    let sent = match outbox.submit(op).await.await {
        Ok(Ok(Outcome::Sent(message))) => Some(MessageKey::new(chat, message.message_id)),
        Ok(Ok(_)) => None,
        Ok(Err(error)) => {
            warn!(%error, "join message not delivered");
            None
        }
        Err(_) => {
            warn!("join message dropped: the scheduler stopped");
            None
        }
    };
    key.zip(sent)
}

async fn on_press(outbox: &Outbox, devices: &Devices, input: CallbackInput) {
    let pressed = input.message();
    let Some(press) = input.data.as_deref().and_then(parse_callback) else {
        return;
    };
    let (answer, edit) = match press {
        Press::Ask(id) => match devices.name(&id) {
            Some(name) => (None, confirm(&id, &name)),
            None => (Some("Этого устройства уже нет."), list(devices, None)),
        },
        Press::No => (None, list(devices, None)),
        Press::Yes(id) => {
            let revoking = devices.clone();
            let target = id.clone();
            let revoked = tokio::task::spawn_blocking(move || revoking.revoke(&target))
                .await
                .ok()
                .flatten();
            match revoked {
                Some(revoked) => {
                    info!(
                        device_id = id,
                        saved = revoked.saved,
                        "device revoked from Telegram"
                    );
                    let mut notice = format!("«{}» отозвано", revoked.name);
                    if let Some(by) = &input.from_name {
                        notice.push_str(&format!(" ({by})"));
                    }
                    notice.push('.');
                    if !revoked.saved {
                        notice.push_str(
                            " Записать это на диск не вышло: после перезапуска hub устройство вернётся, отзовите его ещё раз.",
                        );
                    }
                    (Some("Отозвано"), list(devices, Some(&notice)))
                }
                None => (Some("Этого устройства уже нет."), list(devices, None)),
            }
        }
    };
    submit(
        outbox,
        Op::AnswerCallback {
            query_id: input.query_id,
            text: answer.map(str::to_owned),
        },
    )
    .await;
    if let Some(message) = pressed {
        let (text, keyboard) = edit;
        let op = Op::Edit {
            chat: message.chat,
            message_id: message.id,
            text,
            reply_markup: Some(keyboard.unwrap_or_else(|| json!({ "inline_keyboard": [] }))),
            background: false,
        };
        submit(outbox, op).await;
    }
}

async fn submit(outbox: &Outbox, op: Op) {
    match outbox.submit(op).await.await {
        Ok(Ok(_)) => {}
        Ok(Err(error)) => warn!(%error, "device list message not delivered"),
        Err(_) => warn!("device list message dropped: the scheduler stopped"),
    }
}

/// The confirmation of a revoke.
fn confirm(id: &str, name: &str) -> (String, Option<Value>) {
    let text = format!(
        "Отозвать «{name}» ({id})? Его агенты сразу потеряют связь с hub, хуки перестанут приниматься. Вернуть устройство можно только новым кодом."
    );
    let keyboard = json!({ "inline_keyboard": [[
        { "text": "Да, отозвать", "callback_data": format!("{PREFIX}:{YES}:{id}") },
        { "text": "Отмена", "callback_data": format!("{PREFIX}:{NO}") },
    ]] });
    (text, Some(keyboard))
}

/// The list message: `notice` first, the devices, the shared secret's
/// state and how to add a device; one «Отозвать» button per device.
fn list(devices: &Devices, notice: Option<&str>) -> (String, Option<Value>) {
    let (listed, shared) = devices.list();
    let text = render(&listed, shared, notice, SystemTime::now());
    let rows: Vec<Value> = listed
        .iter()
        .map(|entry| {
            json!([{
                "text": format!("Отозвать {}", entry.name),
                "callback_data": format!("{PREFIX}:{ASK}:{}", entry.id),
            }])
        })
        .collect();
    let keyboard = (!rows.is_empty()).then(|| json!({ "inline_keyboard": rows }));
    (text, keyboard)
}

fn render(listed: &[Listed], shared: SharedState, notice: Option<&str>, now: SystemTime) -> String {
    let mut text = String::new();
    if let Some(notice) = notice {
        text.push_str(notice);
        text.push_str("\n\n");
    }
    if listed.is_empty() {
        text.push_str("Своих секретов у устройств пока нет.\n");
    } else {
        text.push_str(&format!("Устройства ({}):\n", listed.len()));
    }
    for (index, entry) in listed.iter().enumerate() {
        let seen = match entry.seen {
            Some(at) => format!("последний вход {}", ago(now, at)),
            None => "с запуска hub не входило".to_owned(),
        };
        text.push_str(&format!(
            "{}. {} · {} · с {} · {seen}\n",
            index + 1,
            entry.name,
            entry.id,
            crate::files::date(entry.joined),
        ));
    }
    text.push('\n');
    text.push_str(&match shared {
        SharedState::Off => "Общий секрет hub (CCTG_HUB_SECRET) отключён.".to_owned(),
        SharedState::On(seen) => format!(
            "Общий секрет hub (CCTG_HUB_SECRET) принимается; {}. Когда все устройства получат свои секреты, отключите его: CCTG_SHARED_SECRET=off в hub.env и перезапуск hub.",
            match seen {
                Some(at) => format!("последний вход с ним {}", ago(now, at)),
                None => "с запуска hub с ним не входили".to_owned(),
            }
        ),
    });
    text.push_str(&format!(
        "\n\nНовое устройство: /join здесь даёт строку установки с кодом на {} минут (или «cctg hub code» на машине hub, в Docker: docker compose exec hub cctg hub code; на устройстве install.sh --join КОД или cctg join КОД).",
        CODE_TTL.as_secs() / 60
    ));
    text
}

/// «только что», «5 мин назад», «3 ч назад», «2 дн назад».
fn ago(now: SystemTime, at: SystemTime) -> String {
    let secs = now.duration_since(at).map_or(0, |since| since.as_secs());
    match secs {
        0..60 => "только что".to_owned(),
        60..3600 => format!("{} мин назад", secs / 60),
        3600..86_400 => format!("{} ч назад", secs / 3600),
        _ => format!("{} дн назад", secs / 86_400),
    }
}

#[cfg(test)]
mod tests {
    use std::sync::{Arc, Mutex};
    use std::time::Duration;

    use super::*;
    use crate::hub::api::Message;
    use crate::hub::devices::{MAX_DEVICES, NAME_LIMIT, mint_code};
    use crate::hub::scheduler::{BucketConfig, Delivery, Outcome, Scheduler, Transport};
    use crate::hub::testdir::TempDir;
    use crate::wire::Secret;

    #[derive(Default)]
    struct Fake(Mutex<Vec<Op>>);

    /// Sent messages get ids 501, 502, ...
    impl Transport for Fake {
        async fn execute(&self, op: &Op) -> Delivery {
            let mut ops = self.0.lock().unwrap();
            ops.push(op.clone());
            let sent = ops
                .iter()
                .filter(|op| matches!(op, Op::Send { .. }))
                .count();
            Ok(Outcome::Sent(Message {
                message_id: 500 + sent as i64,
                ..Message::default()
            }))
        }
    }

    fn command(text: &str, thread_id: Option<i64>) -> Inbound {
        Inbound {
            display_name: None,
            chat: Chat::Group,
            sender: crate::hub::chat::PrivateChat::of_user(1001),
            message_id: 1,
            thread_id,
            text: Some(text.to_owned()),
            reply_to: None,
            quote: None,
            forwarded: false,
            media: None,
            from_name: None,
            author: None,
            reply_from: None,
        }
    }

    fn press(data: &str) -> CallbackInput {
        CallbackInput {
            chat: Some(Chat::Group),
            query_id: "q".into(),
            data: Some(data.into()),
            message_id: Some(77),
            thread_id: None,
            from_name: Some("Иван".into()),
            display_name: None,
        }
    }

    async fn run(devices: &Devices, inputs: Vec<Input>) -> Vec<Op> {
        let fake = Arc::new(Fake::default());
        let (scheduler, outbox) = Scheduler::new(fake.clone(), BucketConfig::default());
        let scheduler = tokio::spawn(scheduler.run());
        let (tx, rx) = mpsc::unbounded_channel();
        for input in inputs {
            tx.send(input).unwrap();
        }
        drop(tx);
        serve(
            rx,
            outbox,
            devices.clone(),
            join_info(),
            Some("cctg_bot".into()),
        )
        .await;
        scheduler.await.unwrap();
        fake.0.lock().unwrap().clone()
    }

    /// sha256 bytes 0, 1, 2, ...: `000102...1f` in a line.
    fn pin() -> CertPin {
        let hex: String = (0..32u8).map(|byte| format!("{byte:02x}")).collect();
        CertPin::parse(&hex).unwrap()
    }

    /// A hub on a server: TLS, the usual ports, a release build.
    fn join_info() -> JoinInfo {
        JoinInfo {
            release: Some("v9.9.9".into()),
            public: Some(PublicAddrs {
                agent: "hub.example.org:47291".into(),
                hook: "hub.example.org:47292".into(),
            }),
            agent_listen: "0.0.0.0:47291".parse().unwrap(),
            hook_listen: "0.0.0.0:47292".parse().unwrap(),
            pin: Some(pin()),
        }
    }

    fn enroll(dir: &TempDir, devices: &Devices, name: &str) -> (String, Secret) {
        let code = mint_code(dir.path(), SystemTime::now()).unwrap();
        let joined = devices.join(&code, name).unwrap();
        (joined.id, joined.secret)
    }

    #[test]
    fn commands_and_buttons_are_recognised() {
        assert!(is_command(&command("/join", None)));
        assert!(is_command(&command("/Join@cctg_bot", None)));
        assert!(!is_command(&command("/join", Some(7))));
        assert!(!is_command(&command("/joined", None)));
        assert!(is_command(&command("/devices", None)));
        assert!(is_command(&command("/Devices@cctg_bot", None)));
        assert!(
            !is_command(&command("/devices", Some(7))),
            "a topic's text is the session's"
        );
        assert!(!is_command(&command("/brief", None)));
        assert_eq!(
            parse_callback("dev:r:0123abcd"),
            Some(Press::Ask("0123abcd".into()))
        );
        assert_eq!(
            parse_callback("dev:y:0123abcd"),
            Some(Press::Yes("0123abcd".into()))
        );
        assert_eq!(parse_callback("dev:n"), Some(Press::No));
        for other in [
            "dev:r:xyz",
            "dev:y:0123abcd:1",
            "dev:n:1",
            "allow:abcde",
            "status:stop",
            "dev",
        ] {
            assert_eq!(parse_callback(other), None, "{other}");
        }
    }

    #[tokio::test(start_paused = true)]
    async fn the_list_asks_before_it_revokes_and_the_revoke_cuts_the_device() {
        let dir = TempDir::new("roster-revoke");
        let devices = Devices::open(
            dir.path(),
            Some(Secret::parse("shared-secret-0123456789").unwrap()),
        )
        .unwrap();
        let (id, secret) = enroll(&dir, &devices, "laptop");
        let (other, _) = enroll(&dir, &devices, "mac");
        assert!(devices.check(secret.expose().as_bytes()).is_some());

        let ops = run(
            &devices,
            vec![
                Input::Command(command("/devices", None)),
                Input::Command(command("/devices@other_bot", None)),
                Input::Press(press(&format!("dev:r:{id}"))),
                Input::Press(press("dev:n")),
                Input::Press(press(&format!("dev:y:{id}"))),
                Input::Press(press(&format!("dev:y:{id}"))),
            ],
        )
        .await;
        let Op::Send {
            thread_id: None,
            text,
            reply_markup: Some(keyboard),
            ..
        } = &ops[0]
        else {
            panic!("{ops:?}");
        };
        assert!(text.contains("Устройства (2)") && text.contains("laptop") && text.contains(&id));
        assert!(text.contains("принимается"), "{text}");
        assert!(!text.contains(secret.expose()));
        assert_eq!(
            keyboard["inline_keyboard"][0][0]["callback_data"],
            format!("dev:r:{id}")
        );
        assert_eq!(
            keyboard["inline_keyboard"][1][0]["callback_data"],
            format!("dev:r:{other}")
        );
        // `/devices@other_bot` is not ours: nothing sent for it.
        let edits: Vec<&String> = ops
            .iter()
            .filter_map(|op| match op {
                Op::Edit {
                    message_id: 77,
                    text,
                    ..
                } => Some(text),
                _ => None,
            })
            .collect();
        assert_eq!(edits.len(), 4, "{ops:?}");
        assert!(edits[0].starts_with("Отозвать «laptop»"), "{}", edits[0]);
        assert!(
            edits[1].starts_with("Устройства (2)"),
            "cancel: {}",
            edits[1]
        );
        assert!(
            edits[2].starts_with("«laptop» отозвано (Иван)."),
            "{}",
            edits[2]
        );
        assert!(edits[2].contains("Устройства (1)") && !edits[2].contains(&id));
        assert!(
            !edits[3].contains("отозвано"),
            "a second yes changes nothing: {}",
            edits[3]
        );
        let answers: Vec<Option<String>> = ops
            .iter()
            .filter_map(|op| match op {
                Op::AnswerCallback { text, .. } => Some(text.clone()),
                _ => None,
            })
            .collect();
        assert_eq!(
            answers,
            [
                None,
                None,
                Some("Отозвано".into()),
                Some("Этого устройства уже нет.".into())
            ]
        );
        assert_eq!(devices.check(secret.expose().as_bytes()), None);
        assert_eq!(Devices::open(dir.path(), None).unwrap().list().0.len(), 1);
    }

    #[test]
    fn the_install_line_says_how_devices_reach_the_hub() {
        let hex: String = (0..32u8).map(|byte| format!("{byte:02x}")).collect();
        let code = "ABCD-EFGH-JKMN-PQRS";
        // A server hub at the usual ports.
        assert_eq!(
            join_info().line(code),
            format!(
                "curl -fsSL https://raw.githubusercontent.com/pockerhead/cctg/v9.9.9/install.sh \
                 | sh -s -- --hub-host hub.example.org --pin {hex} --join {code}"
            )
        );
        // Other host ports (a changed compose.yml).
        let other_ports = JoinInfo {
            public: Some(PublicAddrs {
                agent: "hub.example.org:52191".into(),
                hook: "hub.example.org:47292".into(),
            }),
            ..join_info()
        };
        assert!(
            other_ports.line(code).ends_with(&format!(
                "| sh -s -- --agent-addr hub.example.org:52191 --hook-addr hub.example.org:47292 --pin {hex} --join {code}"
            )),
            "{}",
            other_ports.line(code)
        );
        // No TLS: only the hub's own machine, on loopback, whatever the
        // public addresses say; a local build takes main's script.
        let plain = JoinInfo {
            release: None,
            pin: None,
            ..join_info()
        };
        assert_eq!(
            plain.line(code),
            format!(
                "curl -fsSL https://raw.githubusercontent.com/pockerhead/cctg/main/install.sh \
                 | sh -s -- --hub-host 127.0.0.1 --join {code}"
            )
        );
        // TLS without an address for others: loopback with the pin; an IPv6
        // wildcard is reached on [::1] (on Windows it takes IPv6 only).
        let local_tls = JoinInfo {
            public: None,
            agent_listen: "0.0.0.0:5000".parse().unwrap(),
            hook_listen: "[::]:5001".parse().unwrap(),
            ..join_info()
        };
        assert!(
            local_tls.line(code).ends_with(&format!(
                "--agent-addr 127.0.0.1:5000 --hook-addr '[::1]:5001' --pin {hex} --join {code}"
            )),
            "{}",
            local_tls.line(code)
        );
        // An [IPv6] address is quoted: unquoted it is a glob pattern.
        let v6 = JoinInfo {
            public: Some(PublicAddrs {
                agent: "[2001:db8::1]:47291".into(),
                hook: "[2001:db8::1]:47292".into(),
            }),
            ..join_info()
        };
        assert!(
            v6.line(code)
                .contains("| sh -s -- --hub-host '[2001:db8::1]' --pin "),
            "{}",
            v6.line(code)
        );
        let v6_ports = JoinInfo {
            public: Some(PublicAddrs {
                agent: "[2001:db8::1]:52191".into(),
                hook: "[2001:db8::1]:47292".into(),
            }),
            ..join_info()
        };
        assert!(
            v6_ports.line(code).contains(
                "| sh -s -- --agent-addr '[2001:db8::1]:52191' --hook-addr '[2001:db8::1]:47292' --pin "
            ),
            "{}",
            v6_ports.line(code)
        );
        let v6_listener = JoinInfo {
            public: None,
            agent_listen: "[2001:db8::5]:5000".parse().unwrap(),
            hook_listen: "[::1]:5001".parse().unwrap(),
            ..join_info()
        };
        assert!(
            v6_listener
                .line(code)
                .contains("--agent-addr '[2001:db8::5]:5000' --hook-addr '[::1]:5001' "),
            "{}",
            v6_listener.line(code)
        );

        let (text, html) = join_message(&join_info(), code);
        assert!(
            text.contains("одноразовый") && text.contains("видят все участники"),
            "{text}"
        );
        assert!(
            html.contains(&format!("<pre>{}</pre>", join_info().line(code))),
            "{html}"
        );
        assert!(!text.contains("не из релиза") && !text.contains("только со своей машины"));
        let (text, _) = join_message(&plain, code);
        assert!(
            text.contains("только со своей машины") && text.contains("не из релиза"),
            "{text}"
        );
        assert!(transcript::telegram_len(&text) < 4096);
    }

    #[tokio::test(start_paused = true)]
    async fn join_answers_with_a_working_code_that_expires_in_the_message() {
        let dir = TempDir::new("roster-join");
        let devices = Devices::open(dir.path(), None).unwrap();
        let started = tokio::time::Instant::now();
        let ops = run(
            &devices,
            vec![
                Input::Command(command("/join@other_bot", None)),
                Input::Command(command("/join", None)),
            ],
        )
        .await;
        let [
            Op::Send {
                thread_id: None,
                text,
                html: Some(html),
                reply_markup: None,
                ..
            },
            Op::Edit {
                text: expired,
                reply_markup: None,
                ..
            },
        ] = ops.as_slice()
        else {
            panic!("{ops:?}");
        };
        assert!(
            started.elapsed() >= CODE_TTL,
            "edited only once the code expired"
        );
        assert_eq!(expired, EXPIRED);
        let line = text.lines().find(|line| line.starts_with("curl ")).unwrap();
        assert!(html.contains(line), "{html}");
        let code = line.rsplit(' ').next().unwrap();
        assert!(devices.join(code, "new box").is_ok(), "{line}");
    }

    #[tokio::test(start_paused = true)]
    async fn a_used_code_marks_its_message_with_the_device() {
        let dir = TempDir::new("roster-used");
        let devices = Devices::open(dir.path(), None).unwrap();
        let fake = Arc::new(Fake::default());
        let (scheduler, outbox) = Scheduler::new(fake.clone(), BucketConfig::default());
        let scheduler = tokio::spawn(scheduler.run());
        let (tx, rx) = mpsc::unbounded_channel();
        let worker = tokio::spawn(serve(
            rx,
            outbox,
            devices.clone(),
            join_info(),
            Some("cctg_bot".into()),
        ));
        let ops = || fake.0.lock().unwrap().clone();
        let started = Instant::now();

        tx.send(Input::Command(command("/join", None))).unwrap();
        let line = loop {
            let sent = ops().into_iter().find_map(|op| match op {
                Op::Send { text, .. } => Some(text),
                _ => None,
            });
            if let Some(text) = sent {
                break text
                    .lines()
                    .find(|line| line.starts_with("curl "))
                    .unwrap()
                    .to_owned();
            }
            tokio::time::sleep(Duration::from_millis(1)).await;
        };
        let code = line.rsplit(' ').next().unwrap();
        // A code minted past `/join` edits nothing when it is taken.
        let side = mint_code(dir.path(), SystemTime::now()).unwrap();
        devices.join(&side, "side box").unwrap();
        let enrolled = devices.join(code, "new box").unwrap();
        while ops().len() < 2 {
            tokio::time::sleep(Duration::from_millis(1)).await;
        }
        drop(tx);
        worker.await.unwrap();
        scheduler.await.unwrap();
        let ops = ops();
        let [
            Op::Send { .. },
            Op::Edit {
                message_id: 501,
                text,
                reply_markup: None,
                ..
            },
        ] = ops.as_slice()
        else {
            panic!("{ops:?}");
        };
        assert!(
            text.contains("использован")
                && text.contains("«new box»")
                && text.contains(&enrolled.id),
            "{text}"
        );
        assert!(
            started.elapsed() < CODE_TTL,
            "no expiry edit after the used one"
        );
        // TASK-063: who asked for the code owns the device; the side one
        // has no owner.
        let asker = crate::hub::chat::PrivateChat::of_user(1001);
        assert_eq!(devices.owner(&enrolled.id), Some(asker));
        let (listed, _) = devices.list();
        let side_box = listed
            .iter()
            .find(|entry| entry.name == "side box")
            .unwrap();
        assert_eq!(devices.owner(&side_box.id), None);
    }

    #[tokio::test(start_paused = true)]
    async fn join_without_a_state_directory_says_why_and_has_no_line() {
        let devices = Devices::from(Secret::parse("shared-secret-0123456789").unwrap());
        let ops = run(&devices, vec![Input::Command(command("/join", None))]).await;
        let [
            Op::Send {
                text, html: None, ..
            },
        ] = ops.as_slice()
        else {
            panic!("{ops:?}");
        };
        assert!(
            text.starts_with("Код сделать не вышло") && !text.contains("curl"),
            "{text}"
        );
    }

    #[test]
    fn a_full_list_fits_one_message() {
        let now = SystemTime::now();
        let listed: Vec<Listed> = (0..MAX_DEVICES)
            .map(|index| Listed {
                id: format!("{index:08x}"),
                name: "Ж".repeat(NAME_LIMIT),
                joined: now,
                seen: Some(now - Duration::from_secs(90_000)),
            })
            .collect();
        let text = render(
            &listed,
            SharedState::On(Some(now)),
            Some(&format!("«{}» отозвано (Иван).", "Ж".repeat(NAME_LIMIT))),
            now,
        );
        assert!(
            transcript::telegram_len(&text) <= 4096,
            "{}",
            transcript::telegram_len(&text)
        );
    }

    #[test]
    fn ages_read_as_words() {
        let now = SystemTime::now();
        let ago_by = |secs| ago(now, now - Duration::from_secs(secs));
        assert_eq!(ago_by(5), "только что");
        assert_eq!(ago_by(300), "5 мин назад");
        assert_eq!(ago_by(7200), "2 ч назад");
        assert_eq!(ago_by(200_000), "2 дн назад");
    }
}
