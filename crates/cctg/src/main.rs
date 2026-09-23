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
    /// Run the Claude Code channel agent.
    Agent,
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
    init_tracing(matches!(&cli.command, Command::Hook { .. }));
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
        Command::Agent => {}
    }

    Ok(())
}

fn init_tracing(is_hook: bool) {
    if is_hook {
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
            Cli::try_parse_from(["cctg", "hook", "SessionStart"])
                .unwrap()
                .command,
            Command::Hook { event } if event == "SessionStart"
        ));
    }
}
