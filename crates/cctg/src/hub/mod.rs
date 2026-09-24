//! `cctg hub`: Telegram side of the bridge.

pub mod api;
pub mod buffer;
pub mod commands;
pub mod config;
pub mod console;
pub mod fetch;
pub mod ingress;
pub mod offset;
pub mod permissions;
pub mod registry;
pub mod scheduler;
pub mod sessions;
pub mod slots;
pub mod status;
pub mod stream;
pub mod subagents;
#[cfg(test)]
pub(crate) mod testdir;
pub mod updates;

use std::path::Path;
use std::sync::Arc;
use std::time::Duration;

use anyhow::Context;
use tokio::sync::{mpsc, oneshot};
use tracing::{info, warn};

use api::{ApiError, BotApi, ChatMember, Sticker};
use config::{
    AGENT_LISTEN_VAR, API_URL_VAR, Config, HOOK_LISTEN_VAR, PROJECTS_VAR, SECRET_VAR, STATE_VAR,
};
use offset::OffsetStore;
use registry::{Icons, RegistryStore};
use scheduler::{BucketConfig, Scheduler};
use sessions::{ProjectsDir, SlotLocator};
use slots::{Control, Slots};
use updates::{Inbound, Routed, ServiceKind};

#[derive(Debug, PartialEq, Eq, thiserror::Error)]
pub enum RightsError {
    #[error(
        "the bot is not an administrator of the configured chat (status: {0}); \
         promote it and grant the \"Manage Topics\" right"
    )]
    NotAdmin(String),
    #[error(
        "the bot is an administrator without the \"Manage Topics\" right \
         (can_manage_topics); grant it in the group admin settings"
    )]
    NoManageTopics,
}

/// Startup check: the hub cannot create topics without `can_manage_topics`.
pub fn check_topic_rights(member: &ChatMember) -> Result<(), RightsError> {
    match member.status.as_str() {
        "creator" => Ok(()),
        "administrator" if member.can_manage_topics => Ok(()),
        "administrator" => Err(RightsError::NoManageTopics),
        other => Err(RightsError::NotAdmin(other.to_owned())),
    }
}

/// The poll callback: commands go to the command worker's queue; other
/// messages, button presses and topic edit notices go to the slot actor. Nothing here waits,
/// so a slow command or a slow Telegram never holds up polling. A pin notice
/// goes to the slot actor only when this bot (`bot_id`) pinned: a person's
/// pin, even of a status message, is theirs to keep.
fn route_inbound<'a>(
    commands: &'a mpsc::UnboundedSender<Inbound>,
    control: &'a mpsc::UnboundedSender<Control>,
    bot_id: i64,
) -> impl FnMut(Routed) + 'a {
    move |routed| match routed {
        Routed::Input(input) if commands::is_command(&input) => {
            if commands.send(input).is_err() {
                warn!("command worker stopped; command dropped");
            }
        }
        Routed::Input(input) => {
            if control.send(Control::Message(input)).is_err() {
                warn!("slot actor stopped; message dropped");
            }
        }
        Routed::Callback(input) => {
            if control.send(Control::Callback(input)).is_err() {
                warn!("slot actor stopped; button press dropped");
            }
        }
        Routed::Service(service) if service.kind == ServiceKind::TopicEdited => {
            let edited = Control::TopicEdited {
                thread_id: service.thread_id,
                message_id: service.message_id,
            };
            if control.send(edited).is_err() {
                warn!("slot actor stopped; service message kept");
            }
        }
        Routed::Service(updates::ServiceMessage {
            kind: ServiceKind::Pinned(pinned),
            message_id,
            from: Some(from),
            ..
        }) if from == bot_id => {
            if control
                .send(Control::Pinned { message_id, pinned })
                .is_err()
            {
                warn!("slot actor stopped; service message kept");
            }
        }
        Routed::Service(_) | Routed::Ignored(_) => {}
    }
}

/// Topic icons from `getForumTopicIconStickers`. A failed lookup stops the
/// start: an icon id Telegram did not offer is never sent. A preferred icon
/// that is not offered is replaced from the offered set with a warning.
fn checked_icons(lookup: Result<Vec<Sticker>, ApiError>) -> anyhow::Result<Icons> {
    let stickers =
        lookup.context("getForumTopicIconStickers failed; topic icons must come from it")?;
    let (icons, substituted) = Icons::from_offered(
        stickers
            .into_iter()
            .filter_map(|sticker| sticker.custom_emoji_id),
    )?;
    for state in substituted {
        warn!(
            state,
            "preferred topic icon is not offered by Telegram; another offered icon stands in"
        );
    }
    Ok(icons)
}

/// How long a stopping hub waits for the slot actor to write the registry.
const STOP_TIMEOUT: Duration = Duration::from_secs(10);

/// Completes on Ctrl+C (Ctrl+Break on Windows, SIGTERM on Unix), or when
/// stdin closes if `watch_stdin` (how `cctg supervise` stops its hub: it
/// closes the pipe, also when it dies itself).
async fn stop_requested(watch_stdin: bool) {
    let stdin_closed = async {
        if !watch_stdin {
            return std::future::pending().await;
        }
        let (closed, closed_rx) = oneshot::channel();
        // A plain thread: it never holds up the runtime's shutdown.
        std::thread::spawn(move || {
            let _ = std::io::copy(&mut std::io::stdin().lock(), &mut std::io::sink());
            let _ = closed.send(());
        });
        let _ = closed_rx.await;
    };
    tokio::select! {
        () = stdin_closed => info!("stdin closed; hub stopping"),
        signal = crate::supervise::stop_signal() => info!(signal, "hub stopping"),
    }
}

/// Runs the hub until [`stop_requested`]; then stops polling between
/// batches and lets the slot actor write the registry before returning.
pub async fn run(env_file: Option<&Path>, stop_on_stdin: bool) -> anyhow::Result<()> {
    let config = Config::load(env_file)?;
    let projects_dir = config.projects_dir.clone().with_context(|| {
        format!("no home directory found; set {PROJECTS_VAR} to the Claude Code projects directory")
    })?;
    let offsets = OffsetStore::open(&config.state_dir)
        .with_context(|| format!("cannot create the hub state directory; check {STATE_VAR}"))?;
    let registry_store = RegistryStore::open(&config.state_dir)
        .with_context(|| format!("cannot create the hub state directory; check {STATE_VAR}"))?;
    let registry = registry_store
        .load()
        .with_context(|| format!("cannot load the slot registry from {STATE_VAR}"))?;
    let secret = config.hub_secret.clone().with_context(|| {
        format!("{SECRET_VAR} is not set; agents and hooks authenticate with it (16+ visible ASCII characters)")
    })?;
    let agent_listener = ingress::bind(config.agent_listen)
        .await
        .with_context(|| format!("cannot listen for agents; check {AGENT_LISTEN_VAR}"))?;
    let hook_listener = ingress::bind(config.hook_listen)
        .await
        .with_context(|| format!("cannot listen for hooks; check {HOOK_LISTEN_VAR}"))?;
    if config.api_url != api::TELEGRAM_API {
        // Never the URL: it is the base of every request URL.
        warn!("{API_URL_VAR} is set: the hub talks to another Bot API server (test only)");
    }
    let api = Arc::new(BotApi::with_api_url(
        &config.api_url,
        &config.token,
        config.chat_id,
    )?);

    let me = api
        .get_me()
        .await
        .context("getMe failed; check CCTG_BOT_TOKEN")?;
    let member = api.get_chat_member(me.id).await.context(
        "getChatMember for the bot failed; check CCTG_CHAT_ID and that the bot is in the group",
    )?;
    check_topic_rights(&member)?;
    let can_delete = member.status == "creator" || member.can_delete_messages;
    if !can_delete {
        warn!("the bot lacks can_delete_messages; forum service messages will stay visible");
    }
    let can_pin = member.status == "creator" || member.can_pin_messages;
    if !can_pin {
        warn!("the bot lacks can_pin_messages; status messages will not be pinned");
    }
    let icons = checked_icons(api.get_forum_topic_icon_stickers().await)?;
    // Agents running another build are shown as outdated (TASK-040).
    let build = tokio::task::spawn_blocking(crate::client::own_build)
        .await
        .ok()
        .flatten();
    if build.is_none() {
        warn!("cannot read this executable; agents are not checked for updates");
    }
    info!(
        bot = me.username.as_deref().unwrap_or("?"),
        build = build.as_deref().map_or("?", crate::client::short),
        "hub started, polling"
    );

    let (scheduler, outbox) = Scheduler::new(api.clone(), BucketConfig::default());
    tokio::spawn(scheduler.run());
    let options = slots::Options {
        icons,
        chat_id: config.chat_id,
        can_delete,
        can_pin,
        status_every: Some(slots::STATUS_EVERY),
        build,
        ..slots::Options::default()
    };
    let (mut slots, view) = Slots::new(registry, registry_store, outbox.clone(), options);
    let permission_asks = slots.permission_asks();
    slots.fetch_files(api.clone());
    let (commands_tx, commands_rx) = mpsc::unbounded_channel();
    tokio::spawn(commands::serve(
        commands_rx,
        outbox,
        Arc::new(SlotLocator::new(view, ProjectsDir::new(projects_dir))),
        me.username.clone(),
    ));
    let (agents_tx, agents_rx) = mpsc::channel(256);
    let (hooks_tx, hooks_rx) = mpsc::channel(256);
    tokio::spawn(ingress::serve_agents(
        agent_listener,
        secret.clone(),
        agents_tx,
    ));
    tokio::spawn(ingress::serve_hooks_and_permissions(
        hook_listener,
        secret,
        hooks_tx,
        permission_asks,
    ));
    let (control_tx, control_rx) = mpsc::unbounded_channel();
    let actor = tokio::spawn(slots.run(agents_rx, hooks_rx, control_rx));

    updates::poll_until(
        api.as_ref(),
        &config.allowlist,
        &offsets,
        route_inbound(&commands_tx, &control_tx, me.id),
        stop_requested(stop_on_stdin),
    )
    .await;
    // Behind every message the poll already handed over.
    let _ = control_tx.send(Control::Stop);
    match tokio::time::timeout(STOP_TIMEOUT, actor).await {
        Ok(Ok(())) => info!("hub stopped"),
        Ok(Err(_)) => warn!("slot actor failed; the registry may miss the last changes"),
        Err(_) => warn!("slot actor did not stop in time; the registry may miss the last changes"),
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use std::sync::Mutex;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::time::Duration;

    use serde_json::{Value, json};
    use tokio::sync::Notify;

    use super::*;
    use api::Message;
    use scheduler::{Delivery, Op, Outcome, Transport};
    use sessions::{LocateError, Located, TranscriptLocator};
    use testdir::TempDir;
    use updates::UpdateSource;

    const CHAT: i64 = -1000000000001;
    const ALLOWED: i64 = 1001;
    /// The bot's own user id (`getMe`).
    const BOT: i64 = 4242;
    const SESSION: &str = "5e551017-0000-4000-8000-000000000001";

    /// One command per call, then an idle long poll; counts calls.
    struct Batches {
        calls: AtomicUsize,
        polled_twice: Notify,
    }

    impl UpdateSource for Batches {
        fn chat_id(&self) -> i64 {
            CHAT
        }

        async fn get_updates(&self, _: Option<i64>, _: Duration) -> Result<Vec<Value>, ApiError> {
            let call = self.calls.fetch_add(1, Ordering::SeqCst);
            let text = match call {
                0 => "/brief",
                1 => "/full",
                _ => {
                    self.polled_twice.notify_one();
                    return std::future::pending().await;
                }
            };
            Ok(vec![json!({ "update_id": call + 1, "message": {
                "message_id": call + 10, "date": 1, "text": text,
                "from": { "id": ALLOWED, "is_bot": false, "first_name": "x" },
                "chat": { "id": CHAT, "type": "supergroup", "is_forum": true },
            }})])
        }
    }

    /// Blocks each `locate` until the test lets it through.
    struct Gated {
        gate: Mutex<std::sync::mpsc::Receiver<()>>,
        file: std::path::PathBuf,
    }

    impl TranscriptLocator for Gated {
        fn locate(&self, _: Option<i64>, _: Option<&str>) -> Result<Located, LocateError> {
            let _ = self.gate.lock().unwrap().recv();
            Ok(Located {
                session_id: SESSION.to_owned(),
                project: "C--proj".to_owned(),
                path: self.file.clone(),
            })
        }
    }

    #[derive(Default)]
    struct Sent(Mutex<Vec<String>>);

    impl Transport for Sent {
        async fn execute(&self, op: &Op) -> Delivery {
            if let Op::Send { text, .. } = op {
                self.0.lock().unwrap().push(text.clone());
            }
            Ok(Outcome::Sent(Message::default()))
        }
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn a_slow_command_does_not_hold_up_polling() {
        let dir = TempDir::new("hub-slow-command");
        let file = dir.path().join(format!("{SESSION}.jsonl"));
        std::fs::write(
            &file,
            "{\"type\":\"user\",\"message\":{\"role\":\"user\",\"content\":\"hello\"}}\n",
        )
        .unwrap();
        let (open_gate, gate) = std::sync::mpsc::channel();
        let sent = Arc::new(Sent::default());
        let (scheduler, outbox) = Scheduler::new(sent.clone(), BucketConfig::default());
        tokio::spawn(scheduler.run());
        let (commands_tx, commands_rx) = mpsc::unbounded_channel();
        let locator = Arc::new(Gated {
            gate: Mutex::new(gate),
            file: file.clone(),
        });
        tokio::spawn(commands::serve(commands_rx, outbox, locator, None));

        let source = Arc::new(Batches {
            calls: AtomicUsize::new(0),
            polled_twice: Notify::new(),
        });
        let store = Arc::new(OffsetStore::open(dir.path()).unwrap());
        let polling = {
            let (source, store) = (source.clone(), store.clone());
            tokio::spawn(async move {
                let allowlist: config::Allowlist = [ALLOWED].into_iter().collect();
                let (control_tx, _control_rx) = mpsc::unbounded_channel();
                updates::poll(
                    source.as_ref(),
                    &allowlist,
                    &store,
                    route_inbound(&commands_tx, &control_tx, BOT),
                )
                .await;
            })
        };

        // The first command is stuck in `locate`, yet both batches were
        // fetched and the offset saved past them.
        tokio::time::timeout(Duration::from_secs(10), source.polled_twice.notified())
            .await
            .expect("poll kept going while a command was stuck");
        assert_eq!(store.load(), Some(3));
        assert!(sent.0.lock().unwrap().is_empty());

        open_gate.send(()).unwrap();
        open_gate.send(()).unwrap();
        let answered = async {
            while sent.0.lock().unwrap().len() < 2 {
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        };
        tokio::time::timeout(Duration::from_secs(10), answered)
            .await
            .expect("both commands answered");
        let turns = transcript::parse(&std::fs::read_to_string(&file).unwrap());
        let want = [
            transcript::render_brief(&turns),
            transcript::render_full(&turns),
        ];
        assert_eq!(*sent.0.lock().unwrap(), want);
        polling.abort();
    }

    #[test]
    fn messages_go_to_the_slot_actor_and_commands_do_not() {
        let (commands_tx, mut commands_rx) = mpsc::unbounded_channel();
        let (control_tx, mut control_rx) = mpsc::unbounded_channel();
        let input = |text: &str| Inbound {
            message_id: 5,
            thread_id: Some(100),
            text: Some(text.to_owned()),
            reply_to: None,
            quote: None,
            forwarded: false,
            media: None,
        };
        let mut route = route_inbound(&commands_tx, &control_tx, BOT);
        route(Routed::Input(input("hello")));
        route(Routed::Input(input("/brief 2")));
        route(Routed::Input(Inbound {
            text: None,
            ..input("")
        }));
        let press = updates::CallbackInput {
            query_id: "q".to_owned(),
            data: Some("allow:abcde".to_owned()),
            message_id: Some(9),
        };
        route(Routed::Callback(press.clone()));
        for kind in [ServiceKind::TopicCreated, ServiceKind::TopicClosed] {
            route(Routed::Service(updates::ServiceMessage {
                kind,
                message_id: 9,
                thread_id: Some(100),
                from: Some(BOT),
            }));
        }
        // Pins of the status message by a person (allowlisted or not) or
        // by nobody known stay; only the bot's own pin notice goes on.
        for from in [Some(ALLOWED), Some(BOT + 1), None, Some(BOT)] {
            route(Routed::Service(updates::ServiceMessage {
                kind: ServiceKind::Pinned(1000),
                message_id: 11,
                thread_id: Some(100),
                from,
            }));
        }
        drop(route);
        assert_eq!(
            commands_rx.try_recv().unwrap().text.as_deref(),
            Some("/brief 2")
        );
        assert!(commands_rx.try_recv().is_err());
        assert_eq!(
            control_rx.try_recv().unwrap(),
            Control::Message(input("hello"))
        );
        assert!(matches!(
            control_rx.try_recv().unwrap(),
            Control::Message(Inbound { text: None, .. })
        ));
        assert_eq!(control_rx.try_recv().unwrap(), Control::Callback(press));
        assert_eq!(
            control_rx.try_recv().unwrap(),
            Control::Pinned {
                message_id: 11,
                pinned: 1000
            }
        );
        assert!(
            control_rx.try_recv().is_err(),
            "other service messages are not input"
        );
    }

    fn member(status: &str, topics: bool) -> ChatMember {
        ChatMember {
            status: status.to_owned(),
            can_manage_topics: topics,
            can_delete_messages: true,
            can_pin_messages: true,
        }
    }

    #[test]
    fn icons_come_only_from_a_successful_lookup() {
        let failed = checked_icons(Err(ApiError::Telegram {
            code: 500,
            description: "Internal Server Error".to_owned(),
        }));
        assert!(failed.is_err());
        let sticker = |id: &str| Sticker {
            custom_emoji_id: Some(id.to_owned()),
            ..Sticker::default()
        };
        let offered = vec![sticker("4"), sticker("3"), sticker("2"), sticker("1")];
        let icons = checked_icons(Ok(offered)).unwrap();
        assert_eq!(icons.alive.as_deref(), Some("1"));
        assert_eq!(icons.no_channel.as_deref(), Some("4"));
        assert!(checked_icons(Ok(vec![sticker("1")])).is_err());
    }

    #[test]
    fn missing_manage_topics_is_a_startup_error() {
        assert_eq!(check_topic_rights(&member("administrator", true)), Ok(()));
        assert_eq!(check_topic_rights(&member("creator", false)), Ok(()));
        assert_eq!(
            check_topic_rights(&member("administrator", false)),
            Err(RightsError::NoManageTopics)
        );
        for status in ["member", "restricted", "left", "kicked", ""] {
            assert_eq!(
                check_topic_rights(&member(status, false)),
                Err(RightsError::NotAdmin(status.to_owned()))
            );
        }
        let message = RightsError::NoManageTopics.to_string();
        assert!(message.contains("can_manage_topics"));
    }
}
