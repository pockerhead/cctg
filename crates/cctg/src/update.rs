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
//!
//! A restart waits while the claude console shows the agent view or a
//! working background agent (TASK-047): `/exit` would end them with the
//! session. The worker then answers `agents_running` and the hub asks again.
//!
//! Sandbox mode (TASK-087): `cctg run` keeps the first claude's arguments
//! for every restart, so the worker points them at the folder's sandbox
//! profile, or back at `settings.json`, on each restart
//! ([`crate::sandbox::profile::retarget`]). A session whose mode differs
//! from its folder's mark restarts on "Обновить"; a marked folder never
//! restarts without its profile (a refused preflight fails the restart).

use std::path::{Path, PathBuf};
use std::time::{Duration, UNIX_EPOCH};

use serde::{Deserialize, Serialize};

use crate::client;
use crate::keys::{self, Typed};
use crate::proctree::Proc;
use crate::sandbox::{self, marks, preflight, profile};
use crate::shim;

/// Set by `cctg run` for its claude: the pid of that `cctg run`.
pub const RUN_VAR: &str = "CCTG_RUN";
/// Set by `cctg run`: its claude arguments as a JSON array of strings.
pub const RUN_ARGS_VAR: &str = "CCTG_RUN_ARGS";
/// Unix seconds: a claude whose shim started earlier takes this build only
/// with a restart. Claude Code keeps what the first worker announced at
/// `initialize` (tools, `tools.listChanged` since TASK-032) for the whole
/// session. Move it forward with any change that reaches Claude Code only on
/// a restart.
pub const RESTART_SINCE: u64 = 1_790_318_757;

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
    /// Where the worker types into claude ([`keys::Target`]), if anywhere.
    pub console: Option<keys::Target>,
    /// This claude runs with a sandbox profile (env [`sandbox::ACTIVE_VAR`]).
    pub sandbox_active: bool,
    /// The session folder: an absolute `CLAUDE_PROJECT_DIR`, else the
    /// caller sets the current directory.
    pub folder: Option<PathBuf>,
    /// `<home>/.cctg/sandbox/folders.json`; `None` without a home.
    pub marks_file: Option<PathBuf>,
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
        console: Option<keys::Target>,
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
            console,
            sandbox_active: var(sandbox::ACTIVE_VAR).as_deref() == Some("1"),
            folder: var("CLAUDE_PROJECT_DIR")
                .map(PathBuf::from)
                .filter(|dir| dir.is_absolute())
                .map(|dir| PathBuf::from(crate::device::canonical_cwd(&dir.to_string_lossy()))),
            marks_file: sandbox::home_dir_of(&var).map(|home| sandbox::marks_file(&home)),
        }
    }

    /// The folder's mark asks for the sandbox. A marks file that cannot be
    /// read asks for it (fail closed). Blocking.
    pub fn wanted(&self) -> bool {
        match (&self.marks_file, &self.folder) {
            (Some(file), Some(folder)) => marks::covered(file, folder) != Ok(false),
            _ => false,
        }
    }

    /// The arguments this claude runs with: `run_args` are the first
    /// claude's, which a restart may have pointed at the profile or away.
    pub fn current_args(&self) -> Vec<String> {
        let current = if self.sandbox_active {
            self.folder.as_deref().and_then(|folder| {
                let base = profile::settings_base(&self.run_args)?;
                profile::retarget(&self.run_args, Some(&profile::profile_path(&base, folder)))
            })
        } else {
            profile::retarget(&self.run_args, None)
        };
        current.unwrap_or_else(|| self.run_args.clone())
    }

    /// The next claude's arguments: [`relaunch_args`], pointed at the
    /// folder's sandbox profile when its mark asks for one (written now by
    /// [`profile::prepare`]; `None` when the device refuses), else at
    /// `settings.json`. Blocking.
    pub fn restart_args(
        &self,
        session_id: &str,
        probe: &dyn preflight::Probe,
    ) -> Option<Vec<String>> {
        let args = relaunch_args(&self.run_args, session_id);
        if !self.wanted() {
            return Some(profile::retarget(&args, None).unwrap_or(args));
        }
        let folder = self.folder.as_deref()?;
        let base = profile::settings_base(&self.run_args)?;
        let exe = self.exe.as_deref()?;
        let path = profile::prepare(probe, &base, folder, exe).ok()?;
        profile::retarget(&args, Some(&path))
    }

    /// The worker can hand over to a newer binary.
    pub fn self_update(&self) -> bool {
        self.shim.is_some() && self.exe.is_some() && self.build.is_some()
    }

    /// `cctg run` can start its claude again.
    pub fn restartable(&self) -> bool {
        self.run_pid.is_some()
            && self.console.is_some()
            && self.claude_pid.is_some()
            && self.state_dir.is_some()
    }

    /// Decides; blocking (hashes the executable, looks at config files). A
    /// claude started before [`RESTART_SINCE`] restarts as after a settings
    /// change.
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
        let changed = self.wanted() != self.sandbox_active
            || self.shim_started.is_some_and(|started| {
                started < RESTART_SINCE
                    || changed_since(&config_files(&self.current_args()), started)
            });
        match (changed, self.restartable()) {
            (false, _) => Plan::UpToDate,
            (true, true) => Plan::Restart,
            (true, false) => Plan::ManualRestart,
        }
    }

    /// The claude console shows the agent view or a working background
    /// agent ([`keys::agents_on_screen`], TASK-047): no restart now. Blocking.
    pub fn agents_on_screen(&self) -> bool {
        self.console.as_ref().is_some_and(keys::agents_on_screen)
    }

    /// Writes the request for `cctg run` and types `/exit`. The request is
    /// removed again when `/exit` was not sent.
    pub fn restart(&self, session_id: &str) -> Typed {
        self.restart_with(session_id, &preflight::RealProbe)
    }

    /// [`Self::restart`] with the device seen through `probe`.
    pub fn restart_with(&self, session_id: &str, probe: &dyn preflight::Probe) -> Typed {
        let (Some(state), Some(run_pid), Some(console)) =
            (&self.state_dir, self.run_pid, &self.console)
        else {
            return Typed::Failed;
        };
        if !is_session_id(session_id) {
            return Typed::Failed;
        }
        let Some(args) = self.restart_args(session_id, probe) else {
            return Typed::Failed;
        };
        let path = request_path(state, run_pid);
        let request = Request { args };
        if write_request(&path, &request).is_err() {
            return Typed::Failed;
        }
        let typed = keys::type_exit(console);
        if typed != Typed::Sent {
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
            console: Some(keys::Target::Console(1)),
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
    fn a_claude_started_before_restart_since_restarts() {
        let dir = TempDir::new("update-plan-since");
        let exe = dir.path().join("cctg.exe");
        std::fs::write(&exe, "one").unwrap();
        let mut w = worker(dir.path(), &exe);
        w.run_pid = Some(77);
        w.shim_started = Some(RESTART_SINCE - 1);
        assert_eq!(w.plan(), Plan::Restart);
        w.run_pid = None;
        assert_eq!(w.plan(), Plan::ManualRestart);
        w.shim_started = Some(RESTART_SINCE);
        assert_eq!(w.plan(), Plan::UpToDate);
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
        let w = Worker::from_env(
            env,
            Some(9),
            Some(PathBuf::from("/state")),
            Some(keys::Target::Console(9)),
        );
        assert!(w.resumed);
        assert_eq!(w.shim, Some(1));
        assert_eq!(w.shim_started, Some(1_700_000_000));
        assert_eq!(w.run_pid, Some(4242));
        assert_eq!(w.run_args, ["--settings", "s.json"]);
        assert!(w.restartable());
        assert!(!w.self_update(), "no build yet");
        let bare = Worker::from_env(|_| None, Some(9), None, Some(keys::Target::Console(9)));
        assert!(!bare.resumed && bare.shim.is_none() && !bare.restartable());
    }

    /// A worker of a claude started by the wrapper in `home/proj`, with the
    /// settings and the exe `install.sh` would give it.
    fn sandbox_worker(home: &Path, exe: &Path, active: bool) -> Worker {
        let folder = home.join("proj");
        let conf = home.join(".cctg").join("claude");
        std::fs::create_dir_all(&folder).unwrap();
        std::fs::create_dir_all(&conf).unwrap();
        let settings = conf.join("settings.json");
        std::fs::write(&settings, "{}").unwrap();
        let folder = sandbox::paths::canonical(&folder).unwrap();
        let normal = args(&[
            "--mcp-config",
            "m.json",
            "--dangerously-load-development-channels",
            "server:cctg",
            "--settings",
            &settings.to_string_lossy(),
        ]);
        let run_args = if active {
            let path = profile::profile_path(&settings, &folder);
            profile::retarget(&normal, Some(&path)).unwrap()
        } else {
            normal
        };
        Worker {
            run_pid: Some(77),
            run_args,
            sandbox_active: active,
            folder: Some(folder),
            marks_file: Some(sandbox::marks_file(home)),
            ..worker(home, exe)
        }
    }

    #[test]
    fn a_mode_that_differs_from_the_mark_restarts() {
        let dir = TempDir::new("update-sandbox-plan");
        let home = sandbox::paths::canonical(dir.path()).unwrap();
        let exe = home.join("cctg.exe");
        std::fs::write(&exe, "one").unwrap();
        let w = sandbox_worker(&home, &exe, false);
        assert_eq!(w.plan(), Plan::UpToDate);
        marks::add(&sandbox::marks_file(&home), w.folder.as_deref().unwrap()).unwrap();
        assert!(w.wanted());
        assert_eq!(w.plan(), Plan::Restart, "marked, not sandboxed");
        let w = sandbox_worker(&home, &exe, true);
        assert_eq!(w.plan(), Plan::UpToDate, "marked and sandboxed");
        std::fs::write(sandbox::marks_file(&home), "{broken").unwrap();
        assert_eq!(w.plan(), Plan::UpToDate, "an unreadable mark counts as on");
        std::fs::remove_file(sandbox::marks_file(&home)).unwrap();
        assert_eq!(w.plan(), Plan::Restart, "sandboxed, no longer marked");
        let manual = Worker {
            run_pid: None,
            ..sandbox_worker(&home, &exe, true)
        };
        assert_eq!(manual.plan(), Plan::ManualRestart);
    }

    #[test]
    fn the_current_args_name_the_file_this_claude_reads() {
        let dir = TempDir::new("update-sandbox-args");
        let home = sandbox::paths::canonical(dir.path()).unwrap();
        let exe = home.join("cctg.exe");
        std::fs::write(&exe, "one").unwrap();
        let normal = sandbox_worker(&home, &exe, false);
        let settings = home.join(".cctg").join("claude").join("settings.json");
        assert_eq!(config_files(&normal.current_args())[1], settings);
        // First started normal, now sandboxed after a restart: the profile.
        let restarted = Worker {
            sandbox_active: true,
            ..sandbox_worker(&home, &exe, false)
        };
        let profile = profile::profile_path(&settings, restarted.folder.as_deref().unwrap());
        assert_eq!(config_files(&restarted.current_args())[1], profile);
        assert!(
            restarted
                .current_args()
                .contains(&"--strict-mcp-config".to_owned())
        );
        // And back: settings.json, no flags.
        let back = Worker {
            sandbox_active: false,
            ..sandbox_worker(&home, &exe, true)
        };
        assert_eq!(back.current_args(), normal.run_args);
    }

    #[test]
    fn a_restart_of_a_marked_folder_takes_its_profile_or_does_not_happen() {
        let dir = TempDir::new("update-sandbox-restart");
        let home = sandbox::paths::canonical(dir.path()).unwrap();
        let exe = home.join("cctg.exe");
        std::fs::write(&exe, "one").unwrap();
        let w = sandbox_worker(&home, &exe, false);
        let folder = w.folder.clone().unwrap();
        let ready = preflight::tests::Fake::linux(&home);
        // Not marked: the normal arguments.
        let plain = w.restart_args("5e55", &ready).unwrap();
        assert_eq!(plain, relaunch_args(&w.run_args, "5e55"));
        marks::add(&sandbox::marks_file(&home), &folder).unwrap();
        let args = w.restart_args("5e55", &ready).unwrap();
        let settings = home.join(".cctg").join("claude").join("settings.json");
        let path = profile::profile_path(&settings, &folder);
        let at = args.iter().position(|arg| arg == "--settings").unwrap();
        assert_eq!(args[at - 4..at], profile::FLAGS.map(str::to_owned));
        assert_eq!(args[at + 1], path.to_string_lossy());
        assert_eq!(args[args.len() - 2..], ["--resume", "5e55"]);
        assert!(path.is_file(), "prepare wrote the profile");
        // A device that refuses: no arguments, no request, nothing typed.
        let mut old = preflight::tests::Fake::linux(&home);
        old.answer("claude", 0, "2.1.1 (Claude Code)");
        assert_eq!(w.restart_args("5e55", &old), None);
        assert_eq!(w.restart_with("5e55", &old), Typed::Failed);
        assert!(!request_path(&home, 77).exists());
        // Sandboxed, mark gone: back to settings.json without the flags.
        std::fs::remove_file(sandbox::marks_file(&home)).unwrap();
        let sandboxed = sandbox_worker(&home, &exe, true);
        let args = sandboxed.restart_args("5e55", &ready).unwrap();
        assert!(
            !args
                .iter()
                .any(|arg| profile::FLAGS.contains(&arg.as_str()))
        );
        assert!(args.contains(&settings.to_string_lossy().into_owned()));
    }

    #[test]
    fn the_sandbox_part_of_the_environment() {
        let home = if cfg!(windows) { r"C:\h" } else { "/h" };
        let home_var = if cfg!(windows) { "USERPROFILE" } else { "HOME" };
        let project = if cfg!(windows) {
            r"C:\definitely
ot\here"
        } else {
            "/definitely/not/here"
        };
        let env = |name: &str| match name {
            "CCTG_SANDBOX" => Some("1".to_owned()),
            "CLAUDE_PROJECT_DIR" => Some(project.to_owned()),
            name if name == home_var => Some(home.to_owned()),
            _ => None,
        };
        let w = Worker::from_env(env, None, None, None);
        assert!(w.sandbox_active);
        assert_eq!(w.folder, Some(PathBuf::from(project)));
        assert_eq!(w.marks_file, Some(sandbox::marks_file(Path::new(home))));
        let bare = Worker::from_env(|_| None, None, None, None);
        assert!(!bare.sandbox_active && bare.folder.is_none() && bare.marks_file.is_none());
        assert!(!bare.wanted());
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
        assert_eq!(w.restart("5e55"), Typed::Failed, "no terminal");
    }
}
