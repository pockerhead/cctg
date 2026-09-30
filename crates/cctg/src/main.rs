use std::path::PathBuf;
use std::time::Duration;

use clap::{Parser, Subcommand};

#[derive(Debug, Parser)]
#[command(
    name = "cctg",
    version = cctg::client::LONG_VERSION,
    about = "Claude Code Telegram bridge"
)]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Debug, Subcommand)]
enum Command {
    /// Run the Telegram hub.
    Hub {
        /// Env file with CCTG_* settings; defaults to ./.env when it exists.
        #[arg(long, global = true)]
        env_file: Option<PathBuf>,
        /// Stop gracefully when stdin closes (set by `cctg supervise`).
        #[arg(long)]
        stop_on_stdin: bool,
        #[command(subcommand)]
        command: Option<HubCommand>,
    },
    /// Keep `cctg hub` running from this executable's file; restart it when
    /// `cctg deploy` asks.
    Supervise {
        /// Passed to `cctg hub`.
        #[arg(long)]
        env_file: Option<PathBuf>,
        /// Append the supervisor's and the hub's log to this file (moved to
        /// <file>.prev at start when larger than 5 MiB).
        #[arg(long)]
        log_file: Option<PathBuf>,
    },
    /// Install a built binary next to the running `cctg supervise`, have the
    /// hub restarted on it and roll back when it does not keep running.
    Deploy {
        /// The new cctg executable.
        exe: PathBuf,
        /// Where the supervised binary lives; defaults to this executable's
        /// directory.
        #[arg(long)]
        bin_dir: Option<PathBuf>,
        /// Seconds to wait for the supervisor to start a hub.
        #[arg(long, default_value_t = cctg::deploy::DEFAULT_DEPLOY_TIMEOUT.as_secs())]
        timeout_secs: u64,
        /// Seconds a new hub must keep running before a deploy counts as done.
        #[arg(long, default_value_t = cctg::deploy::DEFAULT_TRIAL.as_secs())]
        trial_secs: u64,
    },
    /// Run the Claude Code channel agent (spawned by Claude Code over stdio):
    /// a shim that runs the worker agent and hands over to a newer binary.
    Agent,
    /// The worker agent behind `cctg agent`.
    #[command(hide = true)]
    AgentWorker,
    /// Run claude in this console and start it again (`--resume`) when its
    /// cctg agent asks for a restart.
    Run {
        /// Arguments for claude, after `--`.
        #[arg(last = true)]
        args: Vec<String>,
    },
    /// Print the command that registers `cctg agent` with Claude Code at
    /// user scope, with this executable's absolute path.
    AgentInstall,
    /// Handle a Claude Code hook event.
    Hook {
        /// Hook event name.
        event: String,
    },
    /// Claude Code status line command of cctg sessions: sends the numbers
    /// to the hub and prints the user's own status line.
    Statusline,
    /// Exit 0 when the hub on this machine takes connections on both of
    /// its listeners (`CCTG_AGENT_LISTEN`, `CCTG_HOOK_LISTEN`), else 1: the
    /// Docker healthcheck.
    Health,
    /// Check this device's hub settings (~/.cctg/device.env): both hub
    /// links, the certificate pin and the secret. Exit 0 when they work.
    Doctor,
    /// Enroll this device with the hub: exchange a one-time join code for
    /// this device's own secret, written to ~/.cctg/device.env (never
    /// printed). The hub address and pin come from device.env.
    Join {
        /// The join code; else CCTG_JOIN_CODE.
        code: Option<String>,
    },
    /// Sandbox mode of a folder: sessions started in it cannot read or
    /// write outside it (Linux, macOS; docs/sandbox.md).
    Sandbox {
        #[command(subcommand)]
        action: SandboxAction,
    },
    /// The file-tool gate of a sandbox profile (PreToolUse hook): exit 2
    /// blocks the call.
    #[command(hide = true)]
    SandboxGate,
    /// For the claude-cctg wrapper: the sandbox profile of the current
    /// folder (0), not marked (10), or do not start (3).
    #[command(hide = true)]
    SandboxCheck {
        /// cctg's settings.json.
        #[arg(long)]
        settings: PathBuf,
    },
}

#[derive(Debug, Subcommand)]
enum SandboxAction {
    /// Turn it on for the folder (default: the current one) and below.
    On {
        #[arg(long)]
        folder: Option<PathBuf>,
    },
    /// Turn it off for the folder.
    Off {
        #[arg(long)]
        folder: Option<PathBuf>,
    },
    /// Whether it is on and whether this device can run it.
    Status {
        #[arg(long)]
        folder: Option<PathBuf>,
    },
}

#[derive(Debug, Subcommand)]
enum HubCommand {
    /// Print a one-time join code for one device, good for 10 minutes; the
    /// running hub of the same state directory takes it (`docker compose
    /// exec hub cctg hub code`).
    Code,
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let cli = match Cli::try_parse() {
        Ok(cli) => cli,
        // A hook must never exit non-zero: code 2 would block UserPromptSubmit.
        Err(_) if std::env::args().nth(1).as_deref() == Some("hook") => {
            eprintln!("cctg hook: bad arguments");
            std::process::exit(0);
        }
        Err(_) if std::env::args().nth(1).as_deref() == Some("statusline") => {
            eprintln!("cctg statusline: bad arguments");
            std::process::exit(0);
        }
        // The gate fails closed: only exit 2 blocks the tool call.
        Err(_) if std::env::args().nth(1).as_deref() == Some("sandbox-gate") => {
            eprintln!("cctg sandbox-gate: bad arguments");
            std::process::exit(cctg::sandbox::gate::DENY_CODE);
        }
        // Anything but 0 and 10 keeps the wrapper from starting claude.
        Err(_) if std::env::args().nth(1).as_deref() == Some("sandbox-check") => {
            eprintln!("cctg sandbox-check: bad arguments");
            std::process::exit(cctg::sandbox::cli::CHECK_FAILED);
        }
        Err(error) => error.exit(),
    };
    // A supervisor with a log file sets its log up once it holds its lock.
    if !matches!(
        &cli.command,
        Command::Supervise {
            log_file: Some(_),
            ..
        }
    ) {
        init_tracing(matches!(
            &cli.command,
            Command::Hook { .. }
                | Command::Agent
                | Command::AgentWorker
                | Command::Statusline
                | Command::Run { .. }
                | Command::SandboxGate
                | Command::SandboxCheck { .. }
        ));
    }
    match cli.command {
        Command::Hub {
            env_file,
            command: Some(HubCommand::Code),
            ..
        } => println!("{}", cctg::hub::mint_code(env_file.as_deref())?),
        Command::Hub {
            env_file,
            stop_on_stdin,
            command: None,
        } => cctg::hub::run(env_file.as_deref(), stop_on_stdin).await?,
        Command::Supervise { env_file, log_file } => {
            let hub_args = env_file
                .map(|path| vec!["--env-file".into(), path.into_os_string()])
                .unwrap_or_default();
            let exe = std::env::current_exe()?;
            // Held until this process ends.
            let Some(_lock) = cctg::supervise::lock(&exe)? else {
                eprintln!("cctg supervise: another supervisor runs from this folder; leaving");
                return Ok(());
            };
            let log = match log_file {
                Some(path) => {
                    let file = cctg::supervise::open_log(&path)?;
                    let _ = tracing_subscriber::fmt()
                        .with_writer(std::sync::Mutex::new(file.try_clone()?))
                        .with_target(false)
                        .with_ansi(false)
                        .try_init();
                    Some(file)
                }
                None => None,
            };
            cctg::supervise::supervise(cctg::supervise::Settings { exe, hub_args, log }).await?;
        }
        Command::Deploy {
            exe,
            bin_dir,
            timeout_secs,
            trial_secs,
        } => {
            let files = match bin_dir {
                Some(dir) => {
                    let exe_name = format!("cctg{}", std::env::consts::EXE_SUFFIX);
                    cctg::deploy::Files::new(&dir, &exe_name)
                }
                None => cctg::deploy::Files::of_current_exe()?,
            };
            let outcome = cctg::deploy::deploy(
                &exe,
                &files,
                Duration::from_secs(timeout_secs),
                Duration::from_secs(trial_secs),
            )
            .await?;
            println!("{outcome}");
            if !outcome.is_success() {
                std::process::exit(1);
            }
        }
        Command::Hook { event } => {
            // A panic message could quote hook input: print a fixed line.
            std::panic::set_hook(Box::new(|_| eprintln!("cctg hook: internal error")));
            // Spawned, so a panic ends the task, not the process; the hook
            // exits 0 whatever happened, and at once: the stdin reader thread
            // may still be blocked.
            let _ = tokio::spawn(async move { cctg::hook::run(&event).await }).await;
            std::process::exit(0);
        }
        Command::Agent => {
            // stdout belongs to JSON-RPC; a panic message goes to stderr as a
            // fixed line (it could quote channel content).
            std::panic::set_hook(Box::new(|_| {
                let _ = cctg::agent::write_panic_message(std::io::stderr());
            }));
            // At once: the stdin reader thread may still be blocked.
            std::process::exit(cctg::shim::run());
        }
        Command::AgentWorker => {
            std::panic::set_hook(Box::new(|_| {
                let _ = cctg::agent::write_panic_message(std::io::stderr());
            }));
            let code = tokio::spawn(cctg::agent::run_stdio()).await.unwrap_or(0);
            // At once: the stdin reader thread may still be blocked.
            std::process::exit(code);
        }
        Command::Run { args } => {
            let state = cctg::device::DeviceConfig::load().state_dir;
            std::process::exit(cctg::run::run(args, state).await);
        }
        Command::Statusline => {
            // Status line input carries paths and names: a fixed line only.
            std::panic::set_hook(Box::new(|_| eprintln!("cctg statusline: internal error")));
            let code = tokio::spawn(cctg::statusline::run()).await.unwrap_or(0);
            // At once: the stdin reader thread may still be blocked.
            std::process::exit(code);
        }
        Command::Health => {
            std::process::exit(if cctg::hub::healthy().await { 0 } else { 1 });
        }
        Command::Doctor => std::process::exit(cctg::doctor::run().await),
        Command::Join { code } => std::process::exit(cctg::join::run(code).await),
        Command::Sandbox { action } => std::process::exit(match action {
            SandboxAction::On { folder } => cctg::sandbox::cli::on(folder),
            SandboxAction::Off { folder } => cctg::sandbox::cli::off(folder),
            SandboxAction::Status { folder } => cctg::sandbox::cli::status(folder),
        }),
        Command::SandboxGate => {
            // A panic must block too, and must not quote the input.
            std::panic::set_hook(Box::new(|_| {
                eprintln!("cctg sandbox-gate: internal error");
                std::process::exit(cctg::sandbox::gate::DENY_CODE);
            }));
            std::process::exit(cctg::sandbox::gate::run());
        }
        Command::SandboxCheck { settings } => {
            std::panic::set_hook(Box::new(|_| {
                eprintln!("cctg sandbox-check: internal error");
                std::process::exit(cctg::sandbox::cli::CHECK_FAILED);
            }));
            std::process::exit(cctg::sandbox::cli::check(&settings));
        }
        Command::AgentInstall => {
            let exe = std::env::current_exe()?;
            let exe = cctg::device::canonical_cwd(&exe.to_string_lossy());
            println!("{}", cctg::agent::install_command(&exe));
        }
    }

    Ok(())
}

/// `plain`: no colours and no time, for output Claude Code captures (hooks,
/// the agent's stderr lands in its debug log).
fn init_tracing(plain: bool) {
    if plain {
        let _ = tracing_subscriber::fmt()
            .with_writer(std::io::stderr)
            .with_target(false)
            .with_ansi(false)
            .without_time()
            .try_init();
    } else {
        let _ = tracing_subscriber::fmt()
            .with_writer(std::io::stderr)
            .with_target(false)
            .try_init();
    }
}

#[cfg(test)]
mod tests {
    use super::{Cli, Command, HubCommand, SandboxAction};
    use clap::Parser;

    #[test]
    fn parses_all_subcommands() {
        assert!(matches!(
            Cli::try_parse_from(["cctg", "hub"]).unwrap().command,
            Command::Hub {
                env_file: None,
                stop_on_stdin: false,
                command: None,
            }
        ));
        for args in [
            &["cctg", "hub", "code", "--env-file", "x.env"][..],
            &["cctg", "hub", "--env-file", "x.env", "code"][..],
        ] {
            assert!(matches!(
                Cli::try_parse_from(args).unwrap().command,
                Command::Hub { env_file: Some(path), command: Some(HubCommand::Code), .. }
                    if path == std::path::Path::new("x.env")
            ));
        }
        assert!(matches!(
            Cli::try_parse_from(["cctg", "join", "ABCD-EFGH-JKMN-PQRS"]).unwrap().command,
            Command::Join { code: Some(code) } if code == "ABCD-EFGH-JKMN-PQRS"
        ));
        assert!(matches!(
            Cli::try_parse_from(["cctg", "join"]).unwrap().command,
            Command::Join { code: None }
        ));
        assert!(matches!(
            Cli::try_parse_from(["cctg", "hub", "--env-file", "x.env"])
                .unwrap()
                .command,
            Command::Hub { env_file: Some(path), .. } if path == std::path::Path::new("x.env")
        ));
        assert!(matches!(
            Cli::try_parse_from(["cctg", "hub", "--stop-on-stdin"])
                .unwrap()
                .command,
            Command::Hub {
                stop_on_stdin: true,
                ..
            }
        ));
        assert!(matches!(
            Cli::try_parse_from(["cctg", "supervise"]).unwrap().command,
            Command::Supervise {
                env_file: None,
                log_file: None
            }
        ));
        assert!(matches!(
            Cli::try_parse_from(["cctg", "supervise", "--log-file", "hub.log"]).unwrap().command,
            Command::Supervise { log_file: Some(path), .. } if path == std::path::Path::new("hub.log")
        ));
        assert!(matches!(
            Cli::try_parse_from(["cctg", "deploy", "new.exe"])
                .unwrap()
                .command,
            Command::Deploy { exe, bin_dir: None, timeout_secs: 120, trial_secs: 10 } if exe == std::path::Path::new("new.exe")
        ));
        assert!(matches!(
            Cli::try_parse_from(["cctg", "deploy", "new.exe", "--trial-secs", "3"])
                .unwrap()
                .command,
            Command::Deploy { trial_secs: 3, .. }
        ));
        assert!(matches!(
            Cli::try_parse_from(["cctg", "agent"]).unwrap().command,
            Command::Agent
        ));
        assert!(matches!(
            Cli::try_parse_from(["cctg", "agent-worker"])
                .unwrap()
                .command,
            Command::AgentWorker
        ));
        assert!(matches!(
            Cli::try_parse_from(["cctg", "run"]).unwrap().command,
            Command::Run { args } if args.is_empty()
        ));
        assert!(matches!(
            Cli::try_parse_from(["cctg", "run", "--", "--resume", "x", "-c"]).unwrap().command,
            Command::Run { args } if args == ["--resume", "x", "-c"]
        ));
        assert!(matches!(
            Cli::try_parse_from(["cctg", "agent-install"])
                .unwrap()
                .command,
            Command::AgentInstall
        ));
        assert!(matches!(
            Cli::try_parse_from(["cctg", "hook", "SessionStart"])
                .unwrap()
                .command,
            Command::Hook { event } if event == "SessionStart"
        ));
        assert!(matches!(
            Cli::try_parse_from(["cctg", "statusline"]).unwrap().command,
            Command::Statusline
        ));
        assert!(matches!(
            Cli::try_parse_from(["cctg", "health"]).unwrap().command,
            Command::Health
        ));
        assert!(matches!(
            Cli::try_parse_from(["cctg", "doctor"]).unwrap().command,
            Command::Doctor
        ));
        assert!(matches!(
            Cli::try_parse_from(["cctg", "sandbox", "on"])
                .unwrap()
                .command,
            Command::Sandbox {
                action: SandboxAction::On { folder: None }
            }
        ));
        assert!(matches!(
            Cli::try_parse_from(["cctg", "sandbox", "off", "--folder", "x"]).unwrap().command,
            Command::Sandbox { action: SandboxAction::Off { folder: Some(path) } }
                if path == std::path::Path::new("x")
        ));
        assert!(matches!(
            Cli::try_parse_from(["cctg", "sandbox", "status"])
                .unwrap()
                .command,
            Command::Sandbox {
                action: SandboxAction::Status { folder: None }
            }
        ));
        assert!(matches!(
            Cli::try_parse_from(["cctg", "sandbox-gate"])
                .unwrap()
                .command,
            Command::SandboxGate
        ));
        assert!(Cli::try_parse_from(["cctg", "sandbox-gate", "extra"]).is_err());
        assert!(matches!(
            Cli::try_parse_from(["cctg", "sandbox-check", "--settings", "s.json"]).unwrap().command,
            Command::SandboxCheck { settings } if settings == std::path::Path::new("s.json")
        ));
        assert!(Cli::try_parse_from(["cctg", "sandbox-check"]).is_err());
    }
}
