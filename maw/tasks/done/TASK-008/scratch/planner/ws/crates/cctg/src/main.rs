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
    init_tracing();

    let cli = Cli::parse();
    match cli.command {
        Command::Hub { env_file } => cctg::hub::run(env_file.as_deref()).await?,
        Command::Agent | Command::Hook { .. } => {}
    }

    Ok(())
}

fn init_tracing() {
    let _ = tracing_subscriber::fmt()
        .with_writer(std::io::stderr)
        .with_target(false)
        .try_init();
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
