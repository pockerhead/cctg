use std::path::PathBuf;

use clap::{Parser, Subcommand};

#[derive(Debug, Parser)]
#[command(name = "cctg", version, about = "Claude Code Telegram bridge")]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Debug, Subcommand)]
enum Command {
    /// Run the Telegram hub.
    Hub {
        /// Env file with CCTG_* settings; defaults to ./.env when it exists.
        #[arg(long)]
        env_file: Option<PathBuf>,
    },
    /// Run the Claude Code channel agent (spawned by Claude Code over stdio).
    Agent,
    /// Print the command that registers `cctg agent` with Claude Code at
    /// user scope, with this executable's absolute path.
    AgentInstall,
    /// Handle a Claude Code hook event.
    Hook {
        /// Hook event name.
        event: String,
    },
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
        Err(error) => error.exit(),
    };
    init_tracing(matches!(
        &cli.command,
        Command::Hook { .. } | Command::Agent
    ));
    match cli.command {
        Command::Hub { env_file } => cctg::hub::run(env_file.as_deref()).await?,
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
            let _ = tokio::spawn(cctg::agent::run_stdio()).await;
            // At once: the stdin reader thread may still be blocked.
            std::process::exit(0);
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
    use super::{Cli, Command};
    use clap::Parser;

    #[test]
    fn parses_all_subcommands() {
        assert!(matches!(
            Cli::try_parse_from(["cctg", "hub"]).unwrap().command,
            Command::Hub { env_file: None }
        ));
        assert!(matches!(
            Cli::try_parse_from(["cctg", "hub", "--env-file", "x.env"])
                .unwrap()
                .command,
            Command::Hub { env_file: Some(path) } if path == std::path::Path::new("x.env")
        ));
        assert!(matches!(
            Cli::try_parse_from(["cctg", "agent"]).unwrap().command,
            Command::Agent
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
    }
}
