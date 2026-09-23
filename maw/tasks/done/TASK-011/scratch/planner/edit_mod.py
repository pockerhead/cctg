import os
p = os.path.join(os.path.dirname(os.path.abspath(__file__)), 'ws', 'crates', 'cctg', 'src', 'hub', 'mod.rs')
s = open(p, encoding='utf-8').read()


def rep(a, b):
    global s
    assert a in s, a[:80]
    s = s.replace(a, b)


rep('''pub mod offset;
pub mod scheduler;
pub mod sessions;''', '''pub mod offset;
pub mod registry;
pub mod scheduler;
pub mod sessions;
pub mod slots;''')
rep('''use std::path::Path;
use std::sync::Arc;
''', '''use std::collections::HashSet;
use std::path::Path;
use std::sync::Arc;
''')
rep('''use crate::wire::HookPost;
use api::{BotApi, ChatMember};
use config::{AGENT_LISTEN_VAR, Config, HOOK_LISTEN_VAR, PROJECTS_VAR, SECRET_VAR, STATE_VAR};
use ingress::AgentEvent;
use offset::OffsetStore;
use scheduler::{BucketConfig, Scheduler};
use sessions::ProjectsDir;
use updates::{Inbound, Routed};
''', '''use api::{BotApi, ChatMember};
use config::{AGENT_LISTEN_VAR, Config, HOOK_LISTEN_VAR, PROJECTS_VAR, SECRET_VAR, STATE_VAR};
use offset::OffsetStore;
use registry::{Icons, RegistryStore};
use scheduler::{BucketConfig, Scheduler};
use sessions::{ProjectsDir, SlotLocator};
use slots::{Control, Slots};
use updates::{Inbound, Routed, ServiceKind};
''')
rep('''/// The poll callback: commands go to the command worker's queue, nothing here
/// waits, so a slow command never holds up polling.
fn route_inbound(commands: &mpsc::UnboundedSender<Inbound>) -> impl FnMut(Routed) + '_ {
    move |routed| match routed {
        Routed::Input(input) if commands::is_command(&input) => {
            if commands.send(input).is_err() {
                warn!("command worker stopped; command dropped");
            }
        }
        Routed::Input(input) => info!(thread = ?input.thread_id, "inbound message"),
        Routed::Callback(_) => info!("inbound button press"),
        Routed::Service(_) | Routed::Ignored(_) => {}
    }
}

/// Until the slot registry exists, ingress only logs what arrives (short
/// session ids and event types, never paths or text).
async fn drain_ingress(
    mut agents: mpsc::Receiver<AgentEvent>,
    mut hooks: mpsc::Receiver<HookPost>,
) {
    loop {
        tokio::select! {
            Some(event) = agents.recv() => {
                if let AgentEvent::Message { conn, .. } = event {
                    info!(conn, "agent message not routed yet");
                }
            }
            Some(_) = hooks.recv() => {}
            else => return,
        }
    }
}
''', '''/// The poll callback: commands go to the command worker's queue and topic
/// edit notices to the slot actor; nothing here waits, so a slow command
/// never holds up polling.
fn route_inbound<'a>(
    commands: &'a mpsc::UnboundedSender<Inbound>,
    control: &'a mpsc::UnboundedSender<Control>,
) -> impl FnMut(Routed) + 'a {
    move |routed| match routed {
        Routed::Input(input) if commands::is_command(&input) => {
            if commands.send(input).is_err() {
                warn!("command worker stopped; command dropped");
            }
        }
        Routed::Input(input) => info!(thread = ?input.thread_id, "inbound message"),
        Routed::Callback(_) => info!("inbound button press"),
        Routed::Service(service) if service.kind == ServiceKind::TopicEdited => {
            let edited = Control::TopicEdited {
                thread_id: service.thread_id,
                message_id: service.message_id,
            };
            if control.send(edited).is_err() {
                warn!("slot actor stopped; service message kept");
            }
        }
        Routed::Service(_) | Routed::Ignored(_) => {}
    }
}

/// Default icons that Telegram does not offer are dropped with a warning; a
/// failed lookup keeps the defaults.
async fn checked_icons(api: &BotApi) -> Icons {
    let mut icons = Icons::default();
    match api.get_forum_topic_icon_stickers().await {
        Ok(stickers) => {
            let offered: HashSet<String> = stickers
                .into_iter()
                .filter_map(|sticker| sticker.custom_emoji_id)
                .collect();
            for state in icons.keep_valid(&offered) {
                warn!(state, "topic icon is not offered by Telegram; that state keeps the current icon");
            }
        }
        Err(error) => warn!(%error, "getForumTopicIconStickers failed; using the default icons"),
    }
    icons
}
''')
rep('''    let offsets = OffsetStore::open(&config.state_dir)
        .with_context(|| format!("cannot create the hub state directory; check {STATE_VAR}"))?;
''', '''    let offsets = OffsetStore::open(&config.state_dir)
        .with_context(|| format!("cannot create the hub state directory; check {STATE_VAR}"))?;
    let registry_store = RegistryStore::open(&config.state_dir)
        .with_context(|| format!("cannot create the hub state directory; check {STATE_VAR}"))?;
    let registry = registry_store
        .load()
        .with_context(|| format!("cannot load the slot registry from {STATE_VAR}"))?;
''')
rep('''    if member.status == "administrator" && !member.can_delete_messages {
        warn!("the bot lacks can_delete_messages; forum service messages will stay visible");
    }
''', '''    let can_delete = member.status == "creator" || member.can_delete_messages;
    if !can_delete {
        warn!("the bot lacks can_delete_messages; forum service messages will stay visible");
    }
    let icons = checked_icons(&api).await;
''')
rep('''    let (scheduler, outbox) = Scheduler::new(api.clone(), BucketConfig::default());
    tokio::spawn(scheduler.run());
    let (commands_tx, commands_rx) = mpsc::unbounded_channel();
    tokio::spawn(commands::serve(
        commands_rx,
        outbox,
        Arc::new(ProjectsDir::new(projects_dir)),
        me.username.clone(),
    ));
''', '''    let (scheduler, outbox) = Scheduler::new(api.clone(), BucketConfig::default());
    tokio::spawn(scheduler.run());
    let options = slots::Options {
        icons,
        can_delete,
        ..slots::Options::default()
    };
    let (slots, view) = Slots::new(registry, registry_store, outbox.clone(), options);
    let (commands_tx, commands_rx) = mpsc::unbounded_channel();
    tokio::spawn(commands::serve(
        commands_rx,
        outbox,
        Arc::new(SlotLocator::new(view, ProjectsDir::new(projects_dir))),
        me.username.clone(),
    ));
''')
rep('''    tokio::spawn(ingress::serve_hooks(hook_listener, secret, hooks_tx));
    tokio::spawn(drain_ingress(agents_rx, hooks_rx));

    updates::poll(
        api.as_ref(),
        &config.allowlist,
        &offsets,
        route_inbound(&commands_tx),
    )
    .await;''', '''    tokio::spawn(ingress::serve_hooks(hook_listener, secret, hooks_tx));
    let (control_tx, control_rx) = mpsc::unbounded_channel();
    tokio::spawn(slots.run(agents_rx, hooks_rx, control_rx));

    updates::poll(
        api.as_ref(),
        &config.allowlist,
        &offsets,
        route_inbound(&commands_tx, &control_tx),
    )
    .await;''')
rep('''                let allowlist: config::Allowlist = [ALLOWED].into_iter().collect();
                updates::poll(
                    source.as_ref(),
                    &allowlist,
                    &store,
                    route_inbound(&commands_tx),
                )
                .await;''', '''                let allowlist: config::Allowlist = [ALLOWED].into_iter().collect();
                let (control_tx, _control_rx) = mpsc::unbounded_channel();
                updates::poll(
                    source.as_ref(),
                    &allowlist,
                    &store,
                    route_inbound(&commands_tx, &control_tx),
                )
                .await;''')
open(p, 'w', encoding='utf-8', newline='\n').write(s)
print('ok')
