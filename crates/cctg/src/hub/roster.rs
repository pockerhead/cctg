//! `/devices` in General and its «Отозвать» buttons (TASK-045).
//!
//! One worker, like the transcript commands: `/devices` sends the list of
//! enrolled devices with one button per device; a press asks once more
//! («Да, отозвать» / «Отмена») on the same message, and the confirmation
//! revokes the device at once ([`Devices::revoke`]) and edits the message
//! back into the list. Only allowlisted users get here (the poll drops the
//! rest). Logs carry device ids, never names, codes or secrets.

use std::time::SystemTime;

use serde_json::{Value, json};
use tokio::sync::mpsc;
use tracing::{info, warn};

use super::devices::{CODE_TTL, Devices, Listed, SharedState};
use super::scheduler::{Op, Outbox};
use super::updates::{CallbackInput, Inbound};

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

/// `/devices` (optionally `@bot`) in General. The same text in a topic is a
/// message for its session, like every other slash text there (TASK-021).
pub fn is_command(input: &Inbound) -> bool {
    input.thread_id.is_none()
        && input
            .text
            .as_deref()
            .and_then(|text| text.split_whitespace().next())
            .and_then(|word| word.strip_prefix('/'))
            .map(|head| head.split_once('@').map_or(head, |(name, _)| name))
            .is_some_and(|name| name.eq_ignore_ascii_case("devices"))
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

/// Serves `/devices` and its buttons until `inputs` closes.
pub async fn serve(
    mut inputs: mpsc::UnboundedReceiver<Input>,
    outbox: Outbox,
    devices: Devices,
    bot_username: Option<String>,
) {
    while let Some(input) = inputs.recv().await {
        match input {
            Input::Command(input) => {
                if addressed_elsewhere(input.text.as_deref(), bot_username.as_deref()) {
                    continue;
                }
                let (text, keyboard) = list(&devices, None);
                submit(
                    &outbox,
                    Op::Send {
                        thread_id: None,
                        text,
                        html: None,
                        reply_markup: keyboard,
                        permission: false,
                        reply_to: None,
                        notify: false,
                    },
                )
                .await;
            }
            Input::Press(press) => on_press(&outbox, &devices, press).await,
        }
    }
}

/// `/devices@other_bot`.
fn addressed_elsewhere(text: Option<&str>, bot: Option<&str>) -> bool {
    let target = text
        .and_then(|text| text.split_whitespace().next())
        .and_then(|word| word.split_once('@'))
        .map(|(_, target)| target);
    matches!((target, bot), (Some(target), Some(bot)) if !target.eq_ignore_ascii_case(bot))
}

async fn on_press(outbox: &Outbox, devices: &Devices, input: CallbackInput) {
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
    if let Some(message_id) = input.message_id {
        let (text, keyboard) = edit;
        let op = Op::Edit {
            message_id,
            text,
            reply_markup: Some(keyboard.unwrap_or_else(|| json!({ "inline_keyboard": [] }))),
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
        "\n\nНовое устройство: код на {} минут даёт «cctg hub code» на машине hub (в Docker: docker compose exec hub cctg hub code); на устройстве install.sh --join КОД или cctg join КОД.",
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

    impl Transport for Fake {
        async fn execute(&self, op: &Op) -> Delivery {
            self.0.lock().unwrap().push(op.clone());
            Ok(Outcome::Sent(Message::default()))
        }
    }

    fn command(text: &str, thread_id: Option<i64>) -> Inbound {
        Inbound {
            message_id: 1,
            thread_id,
            text: Some(text.to_owned()),
            reply_to: None,
            quote: None,
            forwarded: false,
            media: None,
            from_name: None,
        }
    }

    fn press(data: &str) -> CallbackInput {
        CallbackInput {
            query_id: "q".into(),
            data: Some(data.into()),
            message_id: Some(77),
            from_name: Some("Иван".into()),
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
        serve(rx, outbox, devices.clone(), Some("cctg_bot".into())).await;
        scheduler.await.unwrap();
        fake.0.lock().unwrap().clone()
    }

    fn enroll(dir: &TempDir, devices: &Devices, name: &str) -> (String, Secret) {
        let code = mint_code(dir.path(), SystemTime::now()).unwrap();
        let joined = devices.join(&code, name).unwrap();
        (joined.id, joined.secret)
    }

    #[test]
    fn commands_and_buttons_are_recognised() {
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
