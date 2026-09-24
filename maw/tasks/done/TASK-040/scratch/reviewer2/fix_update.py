# Reviewer-2 edit script for update.rs (applied to the %TEMP% reference
# workspace; kept as evidence). Usage: python fix_update.py <crate dir>
import sys, os
p = os.path.join(sys.argv[1], 'src', 'update.rs')
s = open(p, encoding='utf-8').read()


def rep(old, new, cnt=1):
    global s
    assert s.count(old) == cnt, (old[:80], s.count(old))
    s = s.replace(old, new)


rep('''//! - restarts claude when the settings or MCP config it was started with
//!   changed afterwards (a running claude does not reload `--settings`,
//!   probe TASK-040 P1) or the shim is too old: it writes a request for
//!   `cctg run` ([`crate::run`]) and types `/exit`, and `cctg run` starts
//!   `claude --resume <session>` in the same window;''', '''//! - restarts claude when the settings or MCP config it was started with
//!   changed afterwards (a running claude does not reload `--settings`,
//!   probe TASK-040 P1): it writes a request for `cctg run`
//!   ([`crate::run`]) with the whole relaunch command line
//!   ([`relaunch_args`]: `--resume <session>`, no prompt) and types
//!   `/exit`, and `cctg run` starts claude with it in the same window. All
//!   knowledge of claude's options lives here, in the updatable worker;
//!   `cctg run` only runs what the request says;''')
rep('''use crate::client;
use crate::keys::{self, ExitTyped};
use crate::shim;
''', '''use crate::client;
use crate::keys::{self, ExitTyped};
use crate::proctree::Proc;
use crate::shim;
''')
rep('''/// Set by `cctg run`: its claude arguments as a JSON array of strings.
pub const RUN_ARGS_VAR: &str = "CCTG_RUN_ARGS";
/// The oldest shim this worker works with without a claude restart.
pub const MIN_SHIM_LEVEL: u32 = 1;

/// A restart request of a worker for `cctg run`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Request {
    pub session_id: String,
}
''', '''/// Set by `cctg run`: its claude arguments as a JSON array of strings.
pub const RUN_ARGS_VAR: &str = "CCTG_RUN_ARGS";

/// A restart request of a worker for `cctg run`: the arguments of the next
/// claude, as [`relaunch_args`] made them.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Request {
    pub args: Vec<String>,
}
''')
rep('''    /// `cctg run` pid and claude arguments (env [`RUN_VAR`], [`RUN_ARGS_VAR`]).
    pub run_pid: Option<u32>,''', '''    /// `cctg run` pid and claude arguments (env [`RUN_VAR`], [`RUN_ARGS_VAR`]).
    /// The caller clears `run_pid` when that `cctg run` did not start this
    /// claude ([`launched_by`]).
    pub run_pid: Option<u32>,''')
rep('''        let old_shim = self.shim.is_some_and(|level| level < MIN_SHIM_LEVEL);
        let changed = self
            .shim_started
            .is_some_and(|started| changed_since(&config_files(&self.run_args), started));
        match (old_shim || changed, self.restartable()) {''', '''        let changed = self
            .shim_started
            .is_some_and(|started| changed_since(&config_files(&self.run_args), started));
        match (changed, self.restartable()) {''')
rep('''        let path = request_path(state, run_pid);
        let request = Request {
            session_id: session_id.to_owned(),
        };''', '''        let path = request_path(state, run_pid);
        let request = Request {
            args: relaunch_args(&self.run_args, session_id),
        };''')
rep('''/// The files `--settings` and `--mcp-config` name''', '''/// `chain` is claude's process and its ancestors, claude first. `run_pid`
/// started this claude when it comes before any other claude (or node)
/// above it: an env `CCTG_RUN` inherited by a claude started from inside
/// another `cctg run` session must not send that session's window a restart.
pub fn launched_by(chain: &[Proc], run_pid: u32) -> bool {
    for proc in chain.iter().skip(1) {
        if proc.pid == run_pid {
            return true;
        }
        let name = proc.name.to_ascii_lowercase();
        if matches!(name.trim_end_matches(".exe"), "claude" | "node") {
            return false;
        }
    }
    false
}

/// claude options without a value (`claude --help`, 2.1.281): a word after
/// one of them is the prompt.
const NO_VALUE: &[&str] = &[
    "--allow-dangerously-skip-permissions",
    "--ax-screen-reader",
    "--background",
    "--bare",
    "--bg",
    "--brief",
    "--chrome",
    "--dangerously-skip-permissions",
    "--disable-slash-commands",
    "--exclude-dynamic-system-prompt-sections",
    "--forward-subagent-text",
    "--help",
    "--ide",
    "--include-hook-events",
    "--include-partial-messages",
    "--no-chrome",
    "--no-session-persistence",
    "--print",
    "--replay-user-messages",
    "--restricted",
    "--safe-mode",
    "--strict-mcp-config",
    "--tmux",
    "--verbose",
    "--version",
    "-h",
    "-p",
    "-v",
];

/// claude options that take every following word up to the next option.
const MANY_VALUES: &[&str] = &[
    "--add-dir",
    "--allowed-tools",
    "--allowedTools",
    "--betas",
    "--dangerously-load-development-channels",
    "--disallowed-tools",
    "--disallowedTools",
    "--file",
    "--mcp-config",
    "--tools",
];

/// The arguments of the claude that `cctg run` starts after a restart:
/// `args` (the first claude's) without what picks a session (`-c`,
/// `--continue`, `--fork-session`, `-r`/`--resume` and `--session-id` with
/// their value) and without the prompt (a word that is no option's value,
/// or anything after `--`), then `--resume <session>`. Every other option
/// takes one value, so an unknown option without one keeps the next word
/// (the prompt would be sent again) rather than lose a value.
pub fn relaunch_args(args: &[String], session: &str) -> Vec<String> {
    enum Values {
        None,
        One { keep: bool },
        Many,
    }
    let mut kept = Vec::new();
    let mut values = Values::None;
    for arg in args {
        if arg == "--" {
            break;
        }
        if arg.len() > 1 && arg.starts_with('-') {
            let (flag, inline) = match arg.split_once('=') {
                Some((flag, _)) => (flag, true),
                None => (arg.as_str(), false),
            };
            values = match flag {
                "-c" | "--continue" | "--fork-session" => Values::None,
                "-r" | "--resume" | "--session-id" if !inline => Values::One { keep: false },
                "-r" | "--resume" | "--session-id" => Values::None,
                _ => {
                    kept.push(arg.clone());
                    if inline || NO_VALUE.contains(&flag) {
                        Values::None
                    } else if MANY_VALUES.contains(&flag) {
                        Values::Many
                    } else {
                        Values::One { keep: true }
                    }
                }
            };
            continue;
        }
        match values {
            // The prompt.
            Values::None => {}
            Values::One { keep } => {
                if keep {
                    kept.push(arg.clone());
                }
                values = Values::None;
            }
            Values::Many => kept.push(arg.clone()),
        }
    }
    kept.push("--resume".to_owned());
    kept.push(session.to_owned());
    kept
}

/// The files `--settings` and `--mcp-config` name''')
rep('''    #[test]
    fn the_environment_is_read_once() {''', '''    #[test]
    fn a_restart_resumes_the_session_with_the_options_and_no_prompt() {
        let base = [
            "--mcp-config",
            "m.json",
            "n.json",
            "--strict-mcp-config",
            "--settings",
            "s.json",
            "--dangerously-load-development-channels",
            "server:cctg",
            "--debug-file",
            "d.log",
        ];
        let expected: Vec<String> = args(&base)
            .into_iter()
            .chain(args(&["--resume", "5e55"]))
            .collect();
        for extra in [
            &[][..],
            &["-c"],
            &["--continue"],
            &["--resume", "old-id"],
            &["-r", "old-id"],
            &["--resume=old-id"],
            &["--session-id", "old-id", "--fork-session"],
            &["-r"],
            // A prompt (decision 2026-09-24: never sent again).
            &["fix the bug"],
            &["-c", "fix the bug"],
            &["--", "fix", "--model", "x"],
        ] {
            let mut given = args(&base);
            given.extend(args(extra));
            assert_eq!(relaunch_args(&given, "5e55"), expected, "{extra:?}");
        }
        for (given, want) in [
            // A prompt first, after a value, after an option without one.
            (&["fix", "--model", "haiku"][..], &["--model", "haiku"][..]),
            (&["--model", "haiku", "fix"], &["--model", "haiku"]),
            (&["--verbose", "fix"], &["--verbose"]),
            (&["--settings={\\"a\\":1}", "fix"], &["--settings={\\"a\\":1}"]),
            (&["--debug", "api", "fix"], &["--debug", "api"]),
            (&["--add-dir", "a", "b", "-p"], &["--add-dir", "a", "b", "-p"]),
            // `--resume` without an id followed by an option keeps it.
            (&["--resume", "--model", "haiku"], &["--model", "haiku"]),
        ] {
            let mut want = args(want);
            want.extend(args(&["--resume", "x"]));
            assert_eq!(relaunch_args(&args(given), "x"), want, "{given:?}");
        }
    }

    #[test]
    fn only_the_cctg_run_right_above_claude_restarts_it() {
        let chain = |nodes: &[(u32, &str)]| -> Vec<Proc> {
            nodes
                .iter()
                .map(|&(pid, name)| Proc {
                    pid,
                    name: name.to_owned(),
                    image_path: None,
                })
                .collect()
        };
        assert!(launched_by(&chain(&[(10, "claude.exe"), (7, "cctg.exe")]), 7));
        assert!(launched_by(
            &chain(&[(10, "claude.exe"), (8, "cmd.exe"), (7, "cctg.exe")]),
            7
        ));
        // CCTG_RUN inherited from another session's terminal.
        assert!(!launched_by(
            &chain(&[
                (20, "claude.exe"),
                (19, "bash.exe"),
                (10, "claude.exe"),
                (7, "cctg.exe")
            ]),
            7
        ));
        assert!(!launched_by(&chain(&[(10, "claude.exe"), (3, "explorer.exe")]), 7));
        assert!(!launched_by(&chain(&[(7, "claude.exe")]), 7), "not itself");
    }

    #[test]
    fn the_environment_is_read_once() {''')
rep('''        write_request(
            &path,
            &Request {
                session_id: "5e55-ab".into(),
            },
        )
        .unwrap();
        let back: Request = serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
        assert_eq!(back.session_id, "5e55-ab");''', '''        write_request(
            &path,
            &Request {
                args: args(&["--resume", "5e55-ab"]),
            },
        )
        .unwrap();
        let back: Request = serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
        assert_eq!(back.args, ["--resume", "5e55-ab"]);''')
open(p, 'w', encoding='utf-8', newline='\n').write(s)
print('ok')
