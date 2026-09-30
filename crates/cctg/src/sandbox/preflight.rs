//! Can this device sandbox a folder? Checked before a mark is written and
//! before every sandboxed start (`failIfUnavailable` would otherwise end
//! the session, TASK-087 F13).
//!
//! The first failing check is the answer ([`Refusal`]). Texts are Russian
//! and name no project path (they may reach Telegram); the CLI prints the
//! path itself. Everything the checks look at comes through [`Probe`].

use std::fmt;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use serde_json::Value;

use super::paths;
use crate::hub::config::STATE_VAR;

/// The Claude Code version the sandbox docs and probes were checked on.
pub const MIN_CLAUDE: (u64, u64, u64) = (2, 1, 284);
const CLAUDE_TIMEOUT: Duration = Duration::from_secs(10);
const BWRAP_TIMEOUT: Duration = Duration::from_secs(3);
/// The bubblewrap run Claude Code needs: user, pid and network namespaces.
const BWRAP_PROBE: &[&str] = &[
    "--ro-bind",
    "/",
    "/",
    "--dev",
    "/dev",
    "--proc",
    "/proc",
    "--unshare-user",
    "--unshare-pid",
    "--unshare-net",
    "--die-with-parent",
    "true",
];
const SANDBOX_EXEC: &str = "/usr/bin/sandbox-exec";
/// The wrapper `install.sh` writes, under the home directory.
const WRAPPER: &str = ".local/bin/claude-cctg";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Os {
    Linux,
    MacOs,
    Windows,
    Other,
}

impl Os {
    pub fn host() -> Self {
        if cfg!(target_os = "linux") {
            Self::Linux
        } else if cfg!(target_os = "macos") {
            Self::MacOs
        } else if cfg!(windows) {
            Self::Windows
        } else {
            Self::Other
        }
    }
}

/// A program run to its end.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Ran {
    /// `None`: ended by a signal.
    pub code: Option<i32>,
    pub stdout: String,
}

/// What the checks look at; [`RealProbe`] is the device.
pub trait Probe {
    fn os(&self) -> Os;
    /// An environment variable.
    fn var(&self, name: &str) -> Option<String>;
    /// `program` on `PATH`.
    fn which(&self, program: &str) -> Option<PathBuf>;
    /// Runs `program` with no stdin; `None` when it cannot start or does not
    /// end within `timeout` (then it is killed).
    fn run(&self, program: &str, args: &[&str], timeout: Duration) -> Option<Ran>;
    fn read(&self, path: &Path) -> std::io::Result<String>;
    fn exists(&self, path: &Path) -> bool;
    /// [`paths::canonical`].
    fn canonical(&self, path: &Path) -> Option<PathBuf>;
    fn home(&self) -> Option<PathBuf> {
        super::home_dir_of(&|name| self.var(name))
    }
    /// Linux: Claude Code's temp shared by all projects of this user,
    /// `/tmp/claude-<uid>`, and the names in it now. `None` elsewhere.
    fn claude_temp(&self) -> Option<(String, Vec<String>)>;
}

pub struct RealProbe;

impl Probe for RealProbe {
    fn os(&self) -> Os {
        Os::host()
    }

    fn var(&self, name: &str) -> Option<String> {
        std::env::var(name).ok()
    }

    fn which(&self, program: &str) -> Option<PathBuf> {
        let path = std::env::var_os("PATH")?;
        std::env::split_paths(&path)
            .map(|dir| dir.join(program))
            .find(|candidate| candidate.is_file())
    }

    fn run(&self, program: &str, args: &[&str], timeout: Duration) -> Option<Ran> {
        let mut child = Command::new(program)
            .args(args)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
            .ok()?;
        let mut stdout = child.stdout.take()?;
        // The pipe is drained on its own thread, so a chatty program cannot
        // block on a full pipe while we wait.
        let reader = std::thread::spawn(move || {
            let mut out = Vec::new();
            let _ = stdout.by_ref().take(64 << 10).read_to_end(&mut out);
            out
        });
        let deadline = Instant::now() + timeout;
        let status = loop {
            match child.try_wait() {
                Ok(Some(status)) => break Some(status),
                Ok(None) if Instant::now() < deadline => {
                    std::thread::sleep(Duration::from_millis(20));
                }
                _ => break None,
            }
        };
        let Some(status) = status else {
            let _ = child.kill();
            let _ = child.wait();
            return None;
        };
        let out = reader.join().ok()?;
        Some(Ran {
            code: status.code(),
            stdout: String::from_utf8_lossy(&out).into_owned(),
        })
    }

    fn read(&self, path: &Path) -> std::io::Result<String> {
        std::fs::read_to_string(path)
    }

    fn exists(&self, path: &Path) -> bool {
        path.exists()
    }

    fn canonical(&self, path: &Path) -> Option<PathBuf> {
        paths::canonical(path)
    }

    #[cfg(target_os = "linux")]
    fn claude_temp(&self) -> Option<(String, Vec<String>)> {
        // SAFETY: getuid has no preconditions and cannot fail.
        let uid = unsafe { libc::getuid() };
        let root = format!("/tmp/claude-{uid}"); // profile::CLAUDE_TEMP
        let names = std::fs::read_dir(&root)
            .map(|entries| {
                entries
                    .flatten()
                    .filter_map(|entry| entry.file_name().into_string().ok())
                    .collect()
            })
            .unwrap_or_default();
        Some((root, names))
    }

    #[cfg(not(target_os = "linux"))]
    fn claude_temp(&self) -> Option<(String, Vec<String>)> {
        None
    }
}

/// Why a folder of this device cannot be sandboxed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Refusal {
    WindowsNotYet,
    UnsupportedOs,
    NoHome,
    BadFolder(FolderProblem),
    /// The version claude reported (`X.Y.Z`), if it reported one.
    ClaudeTooOld(Option<String>),
    /// `bwrap`, `socat` or `sandbox-exec`.
    MissingTool(&'static str),
    NoUserNamespaces,
    UserSettingsUnreadable,
    /// Keys of the user's `settings.json` that widen the sandbox.
    UserSettingsWiden(Vec<&'static str>),
    ConfigDirRelocated,
    OldWrapper,
    /// Line of `~/.cctg/sandbox/read-dirs` (from 1).
    BadReadDir(usize),
    /// Something under `<folder>/.cctg` is a link or not a directory.
    FolderLink,
    /// The base `settings.json` of cctg is not a JSON object.
    BaseSettings,
    /// A file of the sandbox could not be written or read; which one.
    Io(&'static str),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FolderProblem {
    NotAbsolute,
    NotUtf8,
    /// `*`, `?` or `[`: with them the read block does not reach commands.
    Glob,
    /// `'`, a line break or another control character.
    Quote,
    NotCanonical,
    Root,
    /// The home directory or above it.
    Home,
    /// In or above `~/.cctg`, claude's config dir or cctg's state dir.
    Private,
}

impl fmt::Display for Refusal {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::WindowsNotYet => f.write_str("на Windows сэндбокс появится в следующей задаче"),
            Self::UnsupportedOs => f.write_str("сэндбокс работает только на Linux и macOS"),
            Self::NoHome => f.write_str("не найден домашний каталог (HOME)"),
            Self::BadFolder(problem) => write!(f, "эту папку нельзя запереть: {problem}"),
            Self::ClaudeTooOld(found) => {
                let (major, minor, patch) = MIN_CLAUDE;
                write!(f, "нужен Claude Code {major}.{minor}.{patch} или новее")?;
                match found {
                    Some(version) => write!(f, ", найден {version}"),
                    None => f.write_str(", версию claude узнать не удалось"),
                }
            }
            Self::MissingTool(tool) => write!(f, "не найдена программа {tool}"),
            Self::NoUserNamespaces => f.write_str(
                "bubblewrap не может создать user namespace (на Ubuntu 24.04 это запрещает \
                 AppArmor, см. docs/sandbox.md)",
            ),
            Self::UserSettingsUnreadable => {
                f.write_str("не читается settings.json в каталоге настроек claude")
            }
            Self::UserSettingsWiden(keys) => write!(
                f,
                "settings.json в каталоге настроек claude расширяет сэндбокс, уберите: {}",
                keys.join(", ")
            ),
            Self::ConfigDirRelocated => {
                f.write_str("задан CLAUDE_CONFIG_DIR: с ним сэндбокс cctg пока не работает")
            }
            Self::OldWrapper => f.write_str(
                "обёртка claude-cctg старая и запустит claude без сэндбокса: \
                 перезапустите install.sh",
            ),
            Self::BadReadDir(line) => write!(
                f,
                "~/.cctg/sandbox/read-dirs, строка {line}: нужен существующий абсолютный \
                 каталог программ, не домашний, не каталог настроек и не папка сессии"
            ),
            Self::FolderLink => {
                f.write_str(".cctg в папке сессии подменён ссылкой или файлом; удалите его")
            }
            Self::BaseSettings => f.write_str("~/.cctg/claude/settings.json повреждён"),
            Self::Io(what) => write!(f, "не удалось записать {what}"),
        }
    }
}

impl fmt::Display for FolderProblem {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::NotAbsolute => "путь не абсолютный",
            Self::NotUtf8 => "путь не в UTF-8",
            Self::Glob => "в пути есть *, ? или [",
            Self::Quote => "в пути есть ', перевод строки или управляющий символ",
            Self::NotCanonical => "путь не канонический (ссылка или папки нет)",
            Self::Root => "это корень диска",
            Self::Home => "это домашний каталог или каталог над ним",
            Self::Private => "это служебный каталог cctg или claude",
        })
    }
}

impl std::error::Error for Refusal {}

/// The device can sandbox; `claude` is the version found.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Ready {
    pub claude: String,
}

/// Every check for `folder`, in order: OS, folder, claude, OS tools, user
/// settings, `CLAUDE_CONFIG_DIR`. The wrapper is checked by [`wrapper`].
pub fn check(probe: &dyn Probe, folder: &Path) -> Result<Ready, Refusal> {
    os(probe)?;
    check_folder(probe, folder)?;
    device(probe)
}

/// The checks without a folder (for `cctg doctor`).
pub fn device(probe: &dyn Probe) -> Result<Ready, Refusal> {
    os(probe)?;
    let claude = claude_version(probe)?;
    os_tools(probe)?;
    user_settings(probe)?;
    if probe
        .var("CLAUDE_CONFIG_DIR")
        .is_some_and(|dir| !dir.trim().is_empty())
    {
        return Err(Refusal::ConfigDirRelocated);
    }
    Ok(Ready { claude })
}

/// The installed `claude-cctg` asks `cctg sandbox-check`: an older one would
/// start claude without the sandbox. For `cctg sandbox on` and doctor.
pub fn wrapper(probe: &dyn Probe) -> Result<(), Refusal> {
    let home = probe.home().ok_or(Refusal::NoHome)?;
    match probe.read(&home.join(WRAPPER)) {
        Ok(text) if text.contains("sandbox-check") => Ok(()),
        _ => Err(Refusal::OldWrapper),
    }
}

fn os(probe: &dyn Probe) -> Result<(), Refusal> {
    match probe.os() {
        Os::Linux | Os::MacOs => Ok(()),
        Os::Windows => Err(Refusal::WindowsNotYet),
        Os::Other => Err(Refusal::UnsupportedOs),
    }
}

fn check_folder(probe: &dyn Probe, folder: &Path) -> Result<(), Refusal> {
    let bad = |problem| Err(Refusal::BadFolder(problem));
    if !folder.is_absolute() {
        return bad(FolderProblem::NotAbsolute);
    }
    let Some(text) = folder.to_str() else {
        return bad(FolderProblem::NotUtf8);
    };
    if text.contains(['*', '?', '[']) {
        return bad(FolderProblem::Glob);
    }
    if text.chars().any(|c| c == '\'' || c.is_control()) {
        return bad(FolderProblem::Quote);
    }
    let resolved = probe.canonical(folder);
    if !resolved.is_some_and(|resolved| same(&resolved, folder)) {
        return bad(FolderProblem::NotCanonical);
    }
    if folder.parent().is_none() {
        return bad(FolderProblem::Root);
    }
    let home = probe.home().ok_or(Refusal::NoHome)?;
    let home = probe.canonical(&home).unwrap_or(home);
    if paths::within(folder, &home) {
        return bad(FolderProblem::Home);
    }
    if private_dirs(probe, &home)
        .iter()
        .any(|dir| related(dir, folder))
    {
        return bad(FolderProblem::Private);
    }
    Ok(())
}

/// `~/.cctg`, claude's config dir and an absolute `CCTG_STATE_DIR`, resolved
/// where they exist.
pub(crate) fn private_dirs(probe: &dyn Probe, home: &Path) -> Vec<PathBuf> {
    let mut dirs = vec![home.join(".cctg")];
    dirs.extend(super::claude_config_dir(&|name| probe.var(name)));
    dirs.extend(
        probe
            .var(STATE_VAR)
            .map(PathBuf::from)
            .filter(|dir| dir.is_absolute()),
    );
    dirs.into_iter()
        .map(|dir| probe.canonical(&dir).unwrap_or(dir))
        .collect()
}

/// One is inside the other (or they are one).
pub(crate) fn related(a: &Path, b: &Path) -> bool {
    paths::within(a, b) || paths::within(b, a)
}

fn same(a: &Path, b: &Path) -> bool {
    paths::within(a, b) && paths::within(b, a)
}

fn claude_version(probe: &dyn Probe) -> Result<String, Refusal> {
    let program = probe
        .var(crate::run::CLAUDE_VAR)
        .filter(|value| !value.trim().is_empty())
        .unwrap_or_else(|| "claude".to_owned());
    let found = probe
        .run(&program, &["--version"], CLAUDE_TIMEOUT)
        .filter(|ran| ran.code == Some(0))
        .and_then(|ran| parse_version(&ran.stdout));
    match found {
        Some((version, text)) if version >= MIN_CLAUDE => Ok(text),
        Some((_, text)) => Err(Refusal::ClaudeTooOld(Some(text))),
        None => Err(Refusal::ClaudeTooOld(None)),
    }
}

/// `2.1.284 (Claude Code)` -> `(2, 1, 284)` and `"2.1.284"`.
fn parse_version(stdout: &str) -> Option<((u64, u64, u64), String)> {
    let token = stdout.split_whitespace().next()?;
    let mut parts = token.split('.').map(|part| part.parse::<u64>().ok());
    let version = (parts.next()??, parts.next()??, parts.next()??);
    parts.next().is_none().then(|| (version, token.to_owned()))
}

/// No WSL interop check: probe P10 (WSL2 5.15, Claude Code 2.1.285,
/// bubblewrap 0.6.1, interop enabled under both binfmt names) showed that
/// `cmd.exe` does not run from inside the sandbox (plan decision: the check
/// goes).
fn os_tools(probe: &dyn Probe) -> Result<(), Refusal> {
    match probe.os() {
        Os::Linux => {
            for tool in ["bwrap", "socat"] {
                if probe.which(tool).is_none() {
                    return Err(Refusal::MissingTool(tool));
                }
            }
            let ran = probe.run("bwrap", BWRAP_PROBE, BWRAP_TIMEOUT);
            if ran.is_none_or(|ran| ran.code != Some(0)) {
                return Err(Refusal::NoUserNamespaces);
            }
            Ok(())
        }
        Os::MacOs if probe.exists(Path::new(SANDBOX_EXEC)) => Ok(()),
        Os::MacOs => Err(Refusal::MissingTool("sandbox-exec")),
        Os::Windows | Os::Other => os(probe),
    }
}

/// Arrays of the user's settings merge with the profile and `--settings`
/// cannot take them back (docs settings, "lists merge"). The booleans that
/// weaken the sandbox are forced off by the profile instead
/// (`profile::overlay`).
const WIDENING: &[(&str, &[&str])] = &[
    (
        "permissions.additionalDirectories",
        &["permissions", "additionalDirectories"],
    ),
    (
        "sandbox.filesystem.allowWrite",
        &["sandbox", "filesystem", "allowWrite"],
    ),
    (
        "sandbox.filesystem.allowRead",
        &["sandbox", "filesystem", "allowRead"],
    ),
    ("sandbox.excludedCommands", &["sandbox", "excludedCommands"]),
    (
        "sandbox.network.allowUnixSockets",
        &["sandbox", "network", "allowUnixSockets"],
    ),
    // macOS: Mach services commands may look up (review finding 2).
    (
        "sandbox.network.allowMachLookup",
        &["sandbox", "network", "allowMachLookup"],
    ),
];

fn user_settings(probe: &dyn Probe) -> Result<(), Refusal> {
    let Some(config) = super::claude_config_dir(&|name| probe.var(name)) else {
        return Err(Refusal::NoHome);
    };
    let text = match probe.read(&config.join("settings.json")) {
        Ok(text) => text,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(_) => return Err(Refusal::UserSettingsUnreadable),
    };
    let settings: Value =
        serde_json::from_str(&text).map_err(|_| Refusal::UserSettingsUnreadable)?;
    if !settings.is_object() {
        return Err(Refusal::UserSettingsUnreadable);
    }
    let mut keys: Vec<&'static str> = WIDENING
        .iter()
        .filter(|(_, path)| {
            let value = path.iter().try_fold(&settings, |value, key| value.get(key));
            value.is_some_and(|value| !empty(value))
        })
        .map(|(name, _)| *name)
        .collect();
    let edit_rule = settings
        .pointer("/permissions/allow")
        .and_then(Value::as_array)
        .is_some_and(|rules| rules.iter().filter_map(Value::as_str).any(widens_writes));
    if edit_rule {
        keys.push("permissions.allow (Edit/Write/NotebookEdit с путём)");
    }
    if keys.is_empty() {
        Ok(())
    } else {
        Err(Refusal::UserSettingsWiden(keys))
    }
}

/// `null`, `[]`, `{}` and `""` add nothing; any other value might.
fn empty(value: &Value) -> bool {
    match value {
        Value::Null => true,
        Value::Array(items) => items.is_empty(),
        Value::Object(map) => map.is_empty(),
        Value::String(text) => text.is_empty(),
        _ => false,
    }
}

/// `Edit(~/x)`, `Write(/y/**)`, `NotebookEdit(z)`: a path the sandbox would
/// let commands write (docs settings-reference, `allowWrite`).
fn widens_writes(rule: &str) -> bool {
    let rule = rule.trim();
    ["Edit(", "Write(", "NotebookEdit("].iter().any(|tool| {
        rule.strip_prefix(tool)
            .and_then(|rest| rest.strip_suffix(')'))
            .is_some_and(|spec| !spec.trim().is_empty())
    })
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use crate::hub::testdir::TempDir;
    use std::collections::HashMap;

    /// A device as the test says, with the real file system for paths.
    pub(crate) struct Fake {
        pub os: Os,
        pub vars: HashMap<String, String>,
        pub tools: Vec<&'static str>,
        /// Command line or program name -> its answer; absent: cannot start.
        pub runs: HashMap<String, Ran>,
        pub files: HashMap<PathBuf, String>,
        pub unreadable: Vec<PathBuf>,
        pub claude_temp: Option<(String, Vec<String>)>,
    }

    impl Fake {
        /// A Linux device that can sandbox, home at `home`.
        pub(crate) fn linux(home: &Path) -> Self {
            let home_var = if cfg!(windows) { "USERPROFILE" } else { "HOME" };
            let mut fake = Self {
                os: Os::Linux,
                vars: HashMap::from([(home_var.to_owned(), home.display().to_string())]),
                tools: vec!["bwrap", "socat"],
                runs: HashMap::new(),
                files: HashMap::new(),
                unreadable: Vec::new(),
                claude_temp: None,
            };
            fake.answer("claude", 0, "2.1.284 (Claude Code)\n");
            fake.answer("bwrap", 0, "");
            fake.files.insert(
                home.join(WRAPPER),
                "#!/bin/sh\np=$(cctg sandbox-check --settings s)\n".to_owned(),
            );
            fake
        }

        pub(crate) fn answer(&mut self, program: &str, code: i32, stdout: &str) {
            self.runs.insert(
                program.to_owned(),
                Ran {
                    code: Some(code),
                    stdout: stdout.to_owned(),
                },
            );
        }
    }

    impl Probe for Fake {
        fn os(&self) -> Os {
            self.os
        }
        fn var(&self, name: &str) -> Option<String> {
            self.vars.get(name).cloned()
        }
        fn which(&self, program: &str) -> Option<PathBuf> {
            self.tools
                .contains(&program)
                .then(|| PathBuf::from("/usr/bin").join(program))
        }
        /// The answer for the whole command line, else for the program.
        fn run(&self, program: &str, args: &[&str], _timeout: Duration) -> Option<Ran> {
            let line = std::iter::once(program)
                .chain(args.iter().copied())
                .collect::<Vec<_>>()
                .join(" ");
            self.runs
                .get(&line)
                .or_else(|| self.runs.get(program))
                .cloned()
        }
        fn read(&self, path: &Path) -> std::io::Result<String> {
            if self.unreadable.iter().any(|p| p == path) {
                return Err(std::io::Error::from(std::io::ErrorKind::PermissionDenied));
            }
            self.files
                .get(path)
                .cloned()
                .ok_or_else(|| std::io::Error::from(std::io::ErrorKind::NotFound))
        }
        fn exists(&self, path: &Path) -> bool {
            self.files.contains_key(path)
        }
        fn canonical(&self, path: &Path) -> Option<PathBuf> {
            paths::canonical(path)
        }
        fn claude_temp(&self) -> Option<(String, Vec<String>)> {
            self.claude_temp.clone()
        }
    }

    /// A canonical home with a project folder in it.
    pub(crate) fn home_and_folder(name: &str) -> (TempDir, PathBuf, PathBuf) {
        let dir = TempDir::new(name);
        let home = paths::canonical(dir.path()).unwrap().join("home");
        let folder = home.join("proj");
        std::fs::create_dir_all(&folder).unwrap();
        (dir, home, folder)
    }

    #[test]
    fn a_ready_linux_and_mac() {
        let (_dir, home, folder) = home_and_folder("preflight-ok");
        let mut fake = Fake::linux(&home);
        assert_eq!(
            check(&fake, &folder),
            Ok(Ready {
                claude: "2.1.284".to_owned()
            })
        );
        assert_eq!(wrapper(&fake), Ok(()));
        fake.os = Os::MacOs;
        fake.tools.clear();
        fake.runs.remove("bwrap");
        fake.files
            .insert(PathBuf::from(SANDBOX_EXEC), String::new());
        fake.answer("claude", 0, "2.2.0 (Claude Code)");
        assert_eq!(
            check(&fake, &folder).map(|ready| ready.claude),
            Ok("2.2.0".into())
        );
        fake.files.remove(Path::new(SANDBOX_EXEC));
        assert_eq!(
            check(&fake, &folder),
            Err(Refusal::MissingTool("sandbox-exec"))
        );
    }

    #[test]
    fn other_systems_are_refused_first() {
        let (_dir, home, folder) = home_and_folder("preflight-os");
        let mut fake = Fake::linux(&home);
        fake.os = Os::Windows;
        assert_eq!(check(&fake, &folder), Err(Refusal::WindowsNotYet));
        assert_eq!(device(&fake), Err(Refusal::WindowsNotYet));
        assert!(Refusal::WindowsNotYet.to_string().contains("Windows"));
        fake.os = Os::Other;
        assert_eq!(check(&fake, &folder), Err(Refusal::UnsupportedOs));
    }

    #[test]
    fn folders_that_cannot_be_locked() {
        let (dir, home, folder) = home_and_folder("preflight-folder");
        let fake = Fake::linux(&home);
        let refused = |path: &Path| match check(&fake, path) {
            Err(Refusal::BadFolder(problem)) => Some(problem),
            _ => None,
        };
        assert_eq!(refused(Path::new("proj")), Some(FolderProblem::NotAbsolute));
        assert_eq!(refused(&home.join("a*b")), Some(FolderProblem::Glob));
        assert_eq!(refused(&home.join("a?b")), Some(FolderProblem::Glob));
        assert_eq!(refused(&home.join("a[b]")), Some(FolderProblem::Glob));
        assert_eq!(refused(&home.join("it's")), Some(FolderProblem::Quote));
        assert_eq!(refused(&home.join("a\nb")), Some(FolderProblem::Quote));
        assert_eq!(
            refused(&home.join("missing")),
            Some(FolderProblem::NotCanonical)
        );
        assert_eq!(
            refused(&folder.join("..").join("proj")),
            Some(FolderProblem::NotCanonical)
        );
        let root = home.ancestors().last().unwrap();
        assert_eq!(refused(root), Some(FolderProblem::Root));
        assert_eq!(refused(&home), Some(FolderProblem::Home));
        assert_eq!(refused(home.parent().unwrap()), Some(FolderProblem::Home));
        let cctg = home.join(".cctg").join("x");
        let claude = home.join(".claude").join("projects");
        std::fs::create_dir_all(&cctg).unwrap();
        std::fs::create_dir_all(&claude).unwrap();
        assert_eq!(refused(&cctg), Some(FolderProblem::Private));
        assert_eq!(refused(&home.join(".cctg")), Some(FolderProblem::Private));
        assert_eq!(refused(&claude), Some(FolderProblem::Private));

        let state = paths::canonical(dir.path()).unwrap().join("state");
        std::fs::create_dir_all(state.join("inner")).unwrap();
        let mut fake = Fake::linux(&home);
        fake.vars
            .insert(STATE_VAR.to_owned(), state.display().to_string());
        assert_eq!(
            check(&fake, &state.join("inner")),
            Err(Refusal::BadFolder(FolderProblem::Private))
        );
        assert_eq!(
            check(&fake, &state),
            Err(Refusal::BadFolder(FolderProblem::Private))
        );
        assert!(check(&fake, &folder).is_ok());
    }

    #[cfg(unix)]
    #[test]
    fn a_linked_folder_is_not_canonical() {
        let (_dir, home, folder) = home_and_folder("preflight-link");
        std::os::unix::fs::symlink(&folder, home.join("link")).unwrap();
        assert_eq!(
            check(&Fake::linux(&home), &home.join("link")),
            Err(Refusal::BadFolder(FolderProblem::NotCanonical))
        );
    }

    #[test]
    fn claude_must_be_new_enough() {
        let (_dir, home, folder) = home_and_folder("preflight-claude");
        let mut fake = Fake::linux(&home);
        fake.answer("claude", 0, "2.1.283 (Claude Code)\n");
        assert_eq!(
            check(&fake, &folder),
            Err(Refusal::ClaudeTooOld(Some("2.1.283".to_owned())))
        );
        fake.answer("claude", 0, "garbage");
        assert_eq!(check(&fake, &folder), Err(Refusal::ClaudeTooOld(None)));
        fake.answer("claude", 1, "2.1.290");
        assert_eq!(check(&fake, &folder), Err(Refusal::ClaudeTooOld(None)));
        fake.runs.remove("claude");
        assert_eq!(check(&fake, &folder), Err(Refusal::ClaudeTooOld(None)));
        fake.answer("my-claude", 0, "3.0.0 (Claude Code)");
        fake.vars
            .insert(crate::run::CLAUDE_VAR.to_owned(), "my-claude".to_owned());
        assert!(
            check(&fake, &folder).is_ok(),
            "CCTG_CLAUDE names the program"
        );
        assert_eq!(parse_version("2.1.284.1"), None);
        assert_eq!(parse_version("v2.1.284"), None);
    }

    #[test]
    fn linux_needs_bubblewrap_socat_and_namespaces() {
        let (_dir, home, folder) = home_and_folder("preflight-linux");
        let mut fake = Fake::linux(&home);
        fake.tools = vec!["socat"];
        assert_eq!(check(&fake, &folder), Err(Refusal::MissingTool("bwrap")));
        fake.tools = vec!["bwrap"];
        assert_eq!(check(&fake, &folder), Err(Refusal::MissingTool("socat")));
        fake.tools = vec!["bwrap", "socat"];
        fake.answer("bwrap", 1, "");
        assert_eq!(check(&fake, &folder), Err(Refusal::NoUserNamespaces));
        fake.runs.remove("bwrap");
        assert_eq!(check(&fake, &folder), Err(Refusal::NoUserNamespaces));
    }

    #[test]
    fn user_settings_that_widen_the_sandbox_are_named() {
        let (_dir, home, folder) = home_and_folder("preflight-settings");
        let mut fake = Fake::linux(&home);
        let settings = home.join(".claude").join("settings.json");
        let mut with = |json: &str| {
            fake.files.insert(settings.clone(), json.to_owned());
            check(&fake, &folder)
        };
        assert!(with(r#"{"permissions":{"allow":["Bash(ls)","Edit"]}}"#).is_ok());
        assert!(with(r#"{"sandbox":{"filesystem":{"allowWrite":[]}}}"#).is_ok());
        for (json, key) in [
            (
                r#"{"permissions":{"additionalDirectories":["/secret/dir"]}}"#,
                "permissions.additionalDirectories",
            ),
            (
                r#"{"sandbox":{"filesystem":{"allowWrite":["/secret/dir"]}}}"#,
                "sandbox.filesystem.allowWrite",
            ),
            (
                r#"{"sandbox":{"filesystem":{"allowRead":["/secret/dir"]}}}"#,
                "sandbox.filesystem.allowRead",
            ),
            (
                r#"{"sandbox":{"excludedCommands":["docker"]}}"#,
                "sandbox.excludedCommands",
            ),
            (
                r#"{"sandbox":{"network":{"allowUnixSockets":["/secret/sock"]}}}"#,
                "sandbox.network.allowUnixSockets",
            ),
            (
                r#"{"permissions":{"allow":["Edit(~/secret/dir/**)"]}}"#,
                "permissions.allow (Edit/Write/NotebookEdit с путём)",
            ),
            (
                r#"{"permissions":{"allow":["Write(/secret/dir)"]}}"#,
                "permissions.allow (Edit/Write/NotebookEdit с путём)",
            ),
        ] {
            let refusal = with(json).unwrap_err();
            assert_eq!(refusal, Refusal::UserSettingsWiden(vec![key]), "{json}");
            assert!(!refusal.to_string().contains("secret"), "{refusal}");
        }
        assert_eq!(
            with(
                r#"{"permissions":{"additionalDirectories":["/a"]},"sandbox":{"excludedCommands":["x"]}}"#
            ),
            Err(Refusal::UserSettingsWiden(vec![
                "permissions.additionalDirectories",
                "sandbox.excludedCommands"
            ]))
        );
        // Review finding 2: an array that cannot be overridden is refused.
        assert_eq!(
            with(r#"{"sandbox":{"network":{"allowMachLookup":["com.example.*"]}}}"#),
            Err(Refusal::UserSettingsWiden(vec![
                "sandbox.network.allowMachLookup"
            ]))
        );
        assert_eq!(with("{broken"), Err(Refusal::UserSettingsUnreadable));
        assert_eq!(with("[]"), Err(Refusal::UserSettingsUnreadable));
    }

    #[test]
    fn a_relocated_config_dir_and_an_old_wrapper_are_refused() {
        let (_dir, home, folder) = home_and_folder("preflight-config");
        let mut fake = Fake::linux(&home);
        let elsewhere = home.join("cfg");
        fake.vars.insert(
            "CLAUDE_CONFIG_DIR".to_owned(),
            elsewhere.display().to_string(),
        );
        assert_eq!(check(&fake, &folder), Err(Refusal::ConfigDirRelocated));
        fake.vars.remove("CLAUDE_CONFIG_DIR");
        assert!(check(&fake, &folder).is_ok());

        fake.files.insert(
            home.join(WRAPPER),
            "#!/bin/sh\nexec cctg run -- --settings s \"$@\"\n".to_owned(),
        );
        assert_eq!(wrapper(&fake), Err(Refusal::OldWrapper));
        fake.files.remove(&home.join(WRAPPER));
        assert_eq!(wrapper(&fake), Err(Refusal::OldWrapper));
        assert!(
            check(&fake, &folder).is_ok(),
            "check never asks the wrapper"
        );
    }
}
