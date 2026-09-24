//! What the worker agent does on the user's "Обновить" (TASK-040).
//!
//! Only the hub asks (`update`), and only after an allowlisted press. The
//! worker then, in this order:
//! - takes a newer binary when the file it was started from has other bytes
//!   than it runs: the shim ([`crate::shim`]) starts that file instead of it,
//!   Claude Code keeps running;
//! - restarts claude when the settings or MCP config it was started with
//!   changed afterwards (a running claude does not reload `--settings`,
//!   probe TASK-040 P1): it writes a request for `cctg run`
//!   ([`crate::run`]) with the whole relaunch command line
//!   ([`relaunch_args`]: `--resume <session>`, no prompt) and types
//!   `/exit`, and `cctg run` starts claude with it in the same window. All
//!   knowledge of claude's options lives here, in the updatable worker;
//!   `cctg run` only runs what the request says;
//! - otherwise answers that it is up to date.

use std::path::{Path, PathBuf};
use std::time::{Duration, UNIX_EPOCH};

use serde::{Deserialize, Serialize};

use crate::client;
use crate::keys::{self, ExitTyped};
use crate::proctree::Proc;
use crate::shim;

/// Set by `cctg run` for its claude: the pid of that `cctg run`.
pub const RUN_VAR: &str = "CCTG_RUN";
/// Set by `cctg run`: its claude arguments as a JSON array of strings.
pub const RUN_ARGS_VAR: &str = "CCTG_RUN_ARGS";

/// A restart request of a worker for `cctg run`: the arguments of the next
/// claude, as [`relaunch_args`] made them.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Request {
    pub args: Vec<String>,
}

/// `<state>/restart/<cctg run pid>.json`.
pub fn request_path(state_dir: &Path, run_pid: u32) -> PathBuf {
    state_dir.join("restart").join(format!("{run_pid}.json"))
}

/// A session id `claude --resume` can take and a file can carry: 1 to 64
/// ASCII letters, digits and dashes.
pub fn is_session_id(id: &str) -> bool {
    (1..=64).contains(&id.len()) && id.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-')
}

/// What the worker knows about its place: set once at start.
#[derive(Debug, Clone, Default)]
pub struct Worker {
    /// An earlier worker of this session answered `initialize`.
    pub resumed: bool,
    /// The shim's level (env [`shim::LEVEL_VAR`]); `None`: no shim.
    pub shim: Option<u32>,
    /// Unix seconds the shim started.
    pub shim_started: Option<u64>,
    /// The file the shim copied this worker from ([`shim::SOURCE_VAR`]), and
    /// the build this worker runs.
    pub exe: Option<PathBuf>,
    pub build: Option<String>,
    pub claude_pid: Option<u32>,
    /// `cctg run` pid and claude arguments (env [`RUN_VAR`], [`RUN_ARGS_VAR`]).
    /// The caller clears `run_pid` when that `cctg run` did not start this
    /// claude ([`launched_by`]).
    pub run_pid: Option<u32>,
    pub run_args: Vec<String>,
    pub state_dir: Option<PathBuf>,
    /// This build can type into the claude console.
    pub keys: bool,
}

/// What an `update` leads to.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Plan {
    Reload,
    Restart,
    ManualRestart,
    UpToDate,
    Failed,
}

impl Worker {
    /// From the environment; `exe`/`build` are read by the caller (a hash of
    /// the executable takes a few milliseconds).
    pub fn from_env(
        var: impl Fn(&str) -> Option<String>,
        claude_pid: Option<u32>,
        state_dir: Option<PathBuf>,
        keys: bool,
    ) -> Self {
        let number = |name: &str| var(name).and_then(|value| value.trim().parse::<u64>().ok());
        let small = |name: &str| number(name).and_then(|value| u32::try_from(value).ok());
        Self {
            resumed: var(shim::RESUMED_VAR).is_some(),
            shim: small(shim::LEVEL_VAR),
            shim_started: number(shim::STARTED_VAR),
            exe: None,
            build: None,
            claude_pid,
            run_pid: small(RUN_VAR),
            run_args: var(RUN_ARGS_VAR)
                .and_then(|json| serde_json::from_str(&json).ok())
                .unwrap_or_default(),
            state_dir,
            keys,
        }
    }

    /// The worker can hand over to a newer binary.
    pub fn self_update(&self) -> bool {
        self.shim.is_some() && self.exe.is_some() && self.build.is_some()
    }

    /// `cctg run` can start its claude again.
    pub fn restartable(&self) -> bool {
        self.run_pid.is_some() && self.keys && self.claude_pid.is_some() && self.state_dir.is_some()
    }

    /// Decides; blocking (hashes the executable, looks at config files).
    pub fn plan(&self) -> Plan {
        let (Some(exe), Some(build)) = (&self.exe, &self.build) else {
            return Plan::Failed;
        };
        if !self.self_update() {
            return Plan::Failed;
        }
        match client::build_of(exe) {
            Ok(disk) if disk != *build => return Plan::Reload,
            Ok(_) => {}
            // Mid-deploy (renamed away, not yet back): try again later.
            Err(_) => return Plan::Failed,
        }
        let changed = self
            .shim_started
            .is_some_and(|started| changed_since(&config_files(&self.run_args), started));
        match (changed, self.restartable()) {
            (false, _) => Plan::UpToDate,
            (true, true) => Plan::Restart,
            (true, false) => Plan::ManualRestart,
        }
    }

    /// Writes the request for `cctg run` and types `/exit`. The request is
    /// removed again when `/exit` was not sent.
    pub fn restart(&self, session_id: &str) -> ExitTyped {
        let (Some(state), Some(run_pid), Some(claude_pid)) =
            (&self.state_dir, self.run_pid, self.claude_pid)
        else {
            return ExitTyped::Failed;
        };
        if !is_session_id(session_id) {
            return ExitTyped::Failed;
        }
        let path = request_path(state, run_pid);
        let request = Request {
            args: relaunch_args(&self.run_args, session_id),
        };
        if write_request(&path, &request).is_err() {
            return ExitTyped::Failed;
        }
        let typed = keys::type_exit(claude_pid);
        if typed != ExitTyped::Sent {
            let _ = std::fs::remove_file(&path);
        }
        typed
    }

    /// Takes back a request whose `/exit` did not end claude.
    pub fn withdraw_request(&self) {
        if let (Some(state), Some(run_pid)) = (&self.state_dir, self.run_pid) {
            let _ = std::fs::remove_file(request_path(state, run_pid));
        }
    }
}

fn write_request(path: &Path, request: &Request) -> std::io::Result<()> {
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
    }
    let tmp = path.with_extension("json.tmp");
    std::fs::write(&tmp, serde_json::to_vec(request)?)?;
    std::fs::rename(&tmp, path)
}

/// `chain` is claude's process and its ancestors, claude first. `run_pid`
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

/// The files `--settings` and `--mcp-config` name in claude's arguments
/// (`--mcp-config` takes several). Inline JSON is not a file.
pub fn config_files(args: &[String]) -> Vec<PathBuf> {
    let mut files = Vec::new();
    let mut taking: Option<bool> = None;
    for arg in args {
        if let Some((flag, value)) = arg.split_once('=')
            && (flag == "--settings" || flag == "--mcp-config")
        {
            taking = None;
            push_file(&mut files, value);
            continue;
        }
        match arg.as_str() {
            "--settings" => taking = Some(false),
            "--mcp-config" => taking = Some(true),
            _ if arg.starts_with('-') => taking = None,
            value => {
                if let Some(many) = taking {
                    push_file(&mut files, value);
                    if !many {
                        taking = None;
                    }
                }
            }
        }
    }
    files
}

fn push_file(files: &mut Vec<PathBuf>, value: &str) {
    let value = value.trim();
    if !value.is_empty() && !value.starts_with('{') {
        files.push(PathBuf::from(value));
    }
}

/// Any of `files` was modified after unix second `started`.
pub fn changed_since(files: &[PathBuf], started: u64) -> bool {
    let started = UNIX_EPOCH + Duration::from_secs(started);
    files.iter().any(|file| {
        std::fs::metadata(file)
            .and_then(|meta| meta.modified())
            .is_ok_and(|modified| modified > started)
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::hub::testdir::TempDir;

    fn args(list: &[&str]) -> Vec<String> {
        list.iter().map(|arg| (*arg).to_owned()).collect()
    }

    #[test]
    fn config_files_come_from_settings_and_mcp_config() {
        let found = config_files(&args(&[
            "--mcp-config",
            "a.json",
            "b.json",
            "--settings",
            "s.json",
            "extra-prompt",
            "--settings={\"x\":1}",
            "--mcp-config=c.json",
            "--dangerously-load-development-channels",
            "server:cctg",
            "--settings",
            "{\"inline\":true}",
        ]));
        assert_eq!(
            found,
            ["a.json", "b.json", "s.json", "c.json"].map(PathBuf::from)
        );
        assert!(config_files(&[]).is_empty());
    }

    #[test]
    fn a_file_changed_after_the_start_needs_a_restart() {
        let dir = TempDir::new("update-changed");
        let file = dir.path().join("settings.json");
        std::fs::write(&file, "{}").unwrap();
        let now = std::time::SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_secs();
        assert!(changed_since(std::slice::from_ref(&file), now - 100));
        assert!(!changed_since(std::slice::from_ref(&file), now + 100));
        assert!(!changed_since(&[dir.path().join("missing")], 0));
    }

    fn worker(dir: &Path, exe: &Path) -> Worker {
        Worker {
            shim: Some(1),
            // Far ahead: nothing counts as changed.
            shim_started: Some(4_000_000_000),
            exe: Some(exe.to_owned()),
            build: Some(client::build_of(exe).unwrap()),
            claude_pid: Some(1),
            state_dir: Some(dir.to_owned()),
            keys: true,
            ..Worker::default()
        }
    }

    #[test]
    fn the_plan_follows_the_disk_the_config_and_cctg_run() {
        let dir = TempDir::new("update-plan");
        let exe = dir.path().join("cctg.exe");
        std::fs::write(&exe, "one").unwrap();
        let settings = dir.path().join("settings.json");
        std::fs::write(&settings, "{}").unwrap();
        let mut w = worker(dir.path(), &exe);
        assert_eq!(w.plan(), Plan::UpToDate);
        // Settings written after the shim started: restart, or say so.
        w.shim_started = Some(0);
        w.run_args = args(&["--settings", &settings.to_string_lossy()]);
        assert_eq!(w.plan(), Plan::ManualRestart, "not under cctg run");
        w.run_pid = Some(77);
        assert_eq!(w.plan(), Plan::Restart);
        // A newer binary goes first.
        std::fs::write(&exe, "two").unwrap();
        assert_eq!(w.plan(), Plan::Reload);
        // Mid-deploy or no shim: nothing.
        std::fs::remove_file(&exe).unwrap();
        assert_eq!(w.plan(), Plan::Failed);
        w.shim = None;
        assert_eq!(w.plan(), Plan::Failed);
    }

    #[test]
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
            (&["--settings={\"a\":1}", "fix"], &["--settings={\"a\":1}"]),
            (&["--debug", "api", "fix"], &["--debug", "api"]),
            (
                &["--add-dir", "a", "b", "-p"],
                &["--add-dir", "a", "b", "-p"],
            ),
            // `--resume` without an id followed by an option keeps it.
            (&["--resume", "--model", "haiku"], &["--model", "haiku"]),
            // A word right after the channels option is one more channel
            // for claude too, so it stays (docs/poc.md keeps a one-value
            // option last in the wrapper for this reason).
            (
                &[
                    "--dangerously-load-development-channels",
                    "server:cctg",
                    "fix",
                ],
                &[
                    "--dangerously-load-development-channels",
                    "server:cctg",
                    "fix",
                ],
            ),
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
        assert!(launched_by(
            &chain(&[(10, "claude.exe"), (7, "cctg.exe")]),
            7
        ));
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
        assert!(!launched_by(
            &chain(&[(10, "claude.exe"), (3, "explorer.exe")]),
            7
        ));
        assert!(!launched_by(&chain(&[(7, "claude.exe")]), 7), "not itself");
    }

    #[test]
    fn the_environment_is_read_once() {
        let env = |name: &str| match name {
            "CCTG_SHIM" => Some("1".to_owned()),
            "CCTG_SHIM_STARTED" => Some("1700000000".to_owned()),
            "CCTG_WORKER_RESUMED" => Some("1".to_owned()),
            "CCTG_RUN" => Some("4242".to_owned()),
            "CCTG_RUN_ARGS" => Some(r#"["--settings","s.json"]"#.to_owned()),
            _ => None,
        };
        let w = Worker::from_env(env, Some(9), Some(PathBuf::from("/state")), true);
        assert!(w.resumed);
        assert_eq!(w.shim, Some(1));
        assert_eq!(w.shim_started, Some(1_700_000_000));
        assert_eq!(w.run_pid, Some(4242));
        assert_eq!(w.run_args, ["--settings", "s.json"]);
        assert!(w.restartable());
        assert!(!w.self_update(), "no build yet");
        let bare = Worker::from_env(|_| None, Some(9), None, true);
        assert!(!bare.resumed && bare.shim.is_none() && !bare.restartable());
    }

    #[test]
    fn a_restart_request_names_the_session_and_only_a_valid_one() {
        let dir = TempDir::new("update-request");
        let path = request_path(dir.path(), 4242);
        assert!(path.ends_with("restart/4242.json"));
        write_request(
            &path,
            &Request {
                args: args(&["--resume", "5e55-ab"]),
            },
        )
        .unwrap();
        let back: Request = serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
        assert_eq!(back.args, ["--resume", "5e55-ab"]);
        for bad in ["", "a b", "../x", "x\"y", &"a".repeat(65)] {
            assert!(!is_session_id(bad), "{bad}");
        }
        assert!(is_session_id("5e551017-0000-4000-8000-000000000001"));
        let w = Worker {
            state_dir: Some(dir.path().to_owned()),
            run_pid: Some(4242),
            ..Worker::default()
        };
        w.withdraw_request();
        assert!(!path.exists());
        assert_eq!(w.restart("5e55"), ExitTyped::Failed, "no claude pid");
    }
}
