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
    Hub,
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
        Command::Hub | Command::Agent | Command::Hook { .. } => {}
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
            Command::Hub
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
