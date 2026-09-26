import sys
root = sys.argv[1]


def patch(rel, pairs):
    p = root + '/' + rel
    s = open(p, encoding='utf-8').read()
    for old, new, *count in pairs:
        n = s.count(old)
        assert n == (count[0] if count else 1), (rel, old[:80], n)
        s = s.replace(old, new)
    open(p, 'w', encoding='utf-8', newline='\n').write(s)


patch('crates/cctg/src/hub/mod.rs', [
("""pub mod config;
pub mod console;
""", """pub mod config;
pub mod console;
pub mod devices;
"""),
("""pub mod registry;
pub mod scheduler;
""", """pub mod registry;
pub mod roster;
pub mod scheduler;
"""),
("""use config::{AGENT_LISTEN_VAR, API_URL_VAR, Config, HOOK_LISTEN_VAR, SECRET_VAR, STATE_VAR};""",
 """use config::{
    AGENT_LISTEN_VAR, API_URL_VAR, Config, HOOK_LISTEN_VAR, SECRET_VAR, SHARED_VAR, STATE_VAR,
};
use devices::Devices;"""),
("""/// The poll callback: commands go to the command worker's queue; other
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
        }""", """/// The poll callback: commands go to the command worker's queue, `/devices`
/// in General and its buttons to the device list's; other messages, button
/// presses and topic edit notices go to the slot actor. Nothing here waits,
/// so a slow command or a slow Telegram never holds up polling. A pin notice
/// goes to the slot actor only when this bot (`bot_id`) pinned: a person's
/// pin, even of a status message, is theirs to keep.
fn route_inbound<'a>(
    commands: &'a mpsc::UnboundedSender<Inbound>,
    roster: &'a mpsc::UnboundedSender<roster::Input>,
    control: &'a mpsc::UnboundedSender<Control>,
    bot_id: i64,
) -> impl FnMut(Routed) + 'a {
    move |routed| match routed {
        Routed::Input(input) if commands::is_command(&input) => {
            if commands.send(input).is_err() {
                warn!("command worker stopped; command dropped");
            }
        }
        Routed::Input(input) if roster::is_command(&input) => {
            if roster.send(roster::Input::Command(input)).is_err() {
                warn!("device list worker stopped; command dropped");
            }
        }
        Routed::Callback(input) if roster::is_callback(&input) => {
            if roster.send(roster::Input::Press(input)).is_err() {
                warn!("device list worker stopped; button press dropped");
            }
        }"""),
("""    let secret = config.hub_secret.clone().with_context(|| {
        format!("{SECRET_VAR} is not set; agents and hooks authenticate with it (16+ visible ASCII characters)")
    })?;""", """    let shared = if config.shared_secret {
        Some(config.hub_secret.clone().with_context(|| {
            format!(
                "{SECRET_VAR} is not set; agents and hooks authenticate with it (16+ visible ASCII characters), \\
                 or with their own secrets once {SHARED_VAR}=off"
            )
        })?)
    } else {
        info!("{SHARED_VAR}=off: only devices with their own secret get in");
        None
    };
    let devices = Devices::open(&config.state_dir, shared)
        .with_context(|| format!("cannot load the device list from {STATE_VAR}"))?;"""),
("""    let (commands_tx, commands_rx) = mpsc::unbounded_channel();
    tokio::spawn(commands::serve(
        commands_rx,
        outbox,
        Arc::new(commands::Asks(transcript_asks)),
        me.username.clone(),
    ));
    let (agents_tx, agents_rx) = mpsc::channel(256);
    let (hooks_tx, hooks_rx) = mpsc::channel(256);
    tokio::spawn(ingress::serve_agents(
        agent_listener,
        secret.clone(),
        agents_tx,
    ));
    tokio::spawn(ingress::serve_hooks_and_asks(
        hook_listener,
        secret,
        hooks_tx,""", """    let (commands_tx, commands_rx) = mpsc::unbounded_channel();
    tokio::spawn(commands::serve(
        commands_rx,
        outbox.clone(),
        Arc::new(commands::Asks(transcript_asks)),
        me.username.clone(),
    ));
    let (roster_tx, roster_rx) = mpsc::unbounded_channel();
    tokio::spawn(roster::serve(
        roster_rx,
        outbox,
        devices.clone(),
        me.username.clone(),
    ));
    let (agents_tx, agents_rx) = mpsc::channel(256);
    let (hooks_tx, hooks_rx) = mpsc::channel(256);
    tokio::spawn(ingress::serve_agents(
        agent_listener,
        devices.clone(),
        agents_tx,
    ));
    tokio::spawn(ingress::serve_hooks_and_asks(
        hook_listener,
        devices,
        hooks_tx,"""),
("""        route_inbound(&commands_tx, &control_tx, me.id),""",
 """        route_inbound(&commands_tx, &roster_tx, &control_tx, me.id),"""),
("""/// How long a stopping hub waits for the slot actor to write the registry.""",
 """/// `cctg hub code` (TASK-045): prints a new join code for one device. It
/// needs only the hub's state directory ([`STATE_VAR`] of the process
/// environment or `env_file`), so it runs next to a running hub, e.g.
/// `docker compose exec hub cctg hub code`; the hub takes the code from
/// there. Only the code goes to stdout.
pub fn mint_code(env_file: Option<&Path>) -> anyhow::Result<String> {
    let state_dir = config::state_dir(env_file)?;
    devices::mint_code(&state_dir, std::time::SystemTime::now())
        .with_context(|| format!("no join code; check {STATE_VAR}"))
}

/// How long a stopping hub waits for the slot actor to write the registry."""),
("""                let (control_tx, _control_rx) = mpsc::unbounded_channel();
                updates::poll(
                    source.as_ref(),
                    &allowlist,
                    &store,
                    route_inbound(&commands_tx, &control_tx, BOT),
                )""", """                let (control_tx, _control_rx) = mpsc::unbounded_channel();
                let (roster_tx, _roster_rx) = mpsc::unbounded_channel();
                updates::poll(
                    source.as_ref(),
                    &allowlist,
                    &store,
                    route_inbound(&commands_tx, &roster_tx, &control_tx, BOT),
                )"""),
("""        let (commands_tx, mut commands_rx) = mpsc::unbounded_channel();
        let (control_tx, mut control_rx) = mpsc::unbounded_channel();
        let input = |text: &str| Inbound {""", """        let (commands_tx, mut commands_rx) = mpsc::unbounded_channel();
        let (roster_tx, mut roster_rx) = mpsc::unbounded_channel();
        let (control_tx, mut control_rx) = mpsc::unbounded_channel();
        let input = |text: &str| Inbound {"""),
("""        let mut route = route_inbound(&commands_tx, &control_tx, BOT);
        route(Routed::Input(input("hello")));
        route(Routed::Input(input("/brief 2")));""", """        let mut route = route_inbound(&commands_tx, &roster_tx, &control_tx, BOT);
        route(Routed::Input(input("hello")));
        route(Routed::Input(input("/brief 2")));
        // `/devices` is the device list's in General, the session's in a
        // topic; so are its buttons, wherever they are.
        route(Routed::Input(Inbound {
            thread_id: None,
            ..input("/devices")
        }));
        route(Routed::Input(input("/devices")));
        route(Routed::Callback(updates::CallbackInput {
            query_id: "d".to_owned(),
            data: Some("dev:n".to_owned()),
            message_id: Some(8),
            from_name: None,
        }));"""),
("""        assert_eq!(
            control_rx.try_recv().unwrap(),
            Control::Message(input("hello"))
        );""", """        assert!(matches!(
            roster_rx.try_recv().unwrap(),
            roster::Input::Command(Inbound { thread_id: None, .. })
        ));
        assert!(matches!(
            roster_rx.try_recv().unwrap(),
            roster::Input::Press(updates::CallbackInput { message_id: Some(8), .. })
        ));
        assert!(roster_rx.try_recv().is_err());
        assert_eq!(
            control_rx.try_recv().unwrap(),
            Control::Message(input("hello"))
        );
        assert_eq!(
            control_rx.try_recv().unwrap(),
            Control::Message(input("/devices"))
        );"""),
])

patch('crates/cctg/src/hub/config.rs', [
("""/// `http://localhost`, `http://127.x.x.x` or `http://[::1]`, any port and path.""",
 """/// The hub state directory alone ([`STATE_VAR`], process environment first,
/// then `env_file` or `./.env`), for `cctg hub code`: it needs neither the
/// token nor the other settings.
pub fn state_dir(env_file: Option<&Path>) -> Result<PathBuf, ConfigError> {
    let file_vars = match env_file {
        Some(path) => load_env_file(path)?,
        None if Path::new(".env").is_file() => load_env_file(Path::new(".env"))?,
        None => HashMap::new(),
    };
    let value = std::env::var(STATE_VAR)
        .ok()
        .or_else(|| file_vars.get(STATE_VAR).cloned())
        .map(|value| value.trim().to_owned())
        .filter(|value| !value.is_empty());
    Ok(PathBuf::from(value.as_deref().unwrap_or(DEFAULT_STATE_DIR)))
}

/// `http://localhost`, `http://127.x.x.x` or `http://[::1]`, any port and path."""),
])
print('ok')
