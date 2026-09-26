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


patch('crates/cctg/src/main.rs', [
("""    /// Run the Telegram hub.
    Hub {
        /// Env file with CCTG_* settings; defaults to ./.env when it exists.
        #[arg(long)]
        env_file: Option<PathBuf>,
        /// Stop gracefully when stdin closes (set by `cctg supervise`).
        #[arg(long)]
        stop_on_stdin: bool,
    },""", """    /// Run the Telegram hub.
    Hub {
        /// Env file with CCTG_* settings; defaults to ./.env when it exists.
        #[arg(long, global = true)]
        env_file: Option<PathBuf>,
        /// Stop gracefully when stdin closes (set by `cctg supervise`).
        #[arg(long)]
        stop_on_stdin: bool,
        #[command(subcommand)]
        command: Option<HubCommand>,
    },"""),
("""    /// Check this device's hub settings (~/.cctg/device.env): both hub
    /// links, the certificate pin and the secret. Exit 0 when they work.
    Doctor,
}
""", """    /// Check this device's hub settings (~/.cctg/device.env): both hub
    /// links, the certificate pin and the secret. Exit 0 when they work.
    Doctor,
    /// Enroll this device with the hub: exchange a one-time join code for
    /// this device's own secret, written to ~/.cctg/device.env (never
    /// printed). The hub address and pin come from device.env.
    Join {
        /// The join code; else CCTG_JOIN_CODE.
        code: Option<String>,
    },
}

#[derive(Debug, Subcommand)]
enum HubCommand {
    /// Print a one-time join code for one device, good for 10 minutes; the
    /// running hub of the same state directory takes it (`docker compose
    /// exec hub cctg hub code`).
    Code,
}
"""),
("""    match cli.command {
        Command::Hub {
            env_file,
            stop_on_stdin,
        } => cctg::hub::run(env_file.as_deref(), stop_on_stdin).await?,""", """    match cli.command {
        Command::Hub {
            env_file,
            command: Some(HubCommand::Code),
            ..
        } => println!("{}", cctg::hub::mint_code(env_file.as_deref())?),
        Command::Hub {
            env_file,
            stop_on_stdin,
            command: None,
        } => cctg::hub::run(env_file.as_deref(), stop_on_stdin).await?,"""),
("""        Command::Doctor => std::process::exit(cctg::doctor::run().await),""",
 """        Command::Doctor => std::process::exit(cctg::doctor::run().await),
        Command::Join { code } => std::process::exit(cctg::join::run(code).await),"""),
("""    use super::{Cli, Command};""", """    use super::{Cli, Command, HubCommand};"""),
("""        assert!(matches!(
            Cli::try_parse_from(["cctg", "hub"]).unwrap().command,
            Command::Hub {
                env_file: None,
                stop_on_stdin: false
            }
        ));""", """        assert!(matches!(
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
        ));"""),
])

patch('crates/cctg/src/lib.rs', [
("""pub mod hub;
pub mod keys;""", """pub mod hub;
pub mod join;
pub mod keys;"""),
])
print('ok')
