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

/// The Windows sandbox install mark, as the checks need it (OS-independent so
/// `Fake` can carry one).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Installed {
    pub version: u32,
    pub slots: u32,
    /// The recorded owner SID is the current Windows user.
    pub owner_is_me: bool,
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

    /// Windows: the sandbox install mark, `None` when not installed. Other
    /// systems have none.
    fn sandbox_install(&self) -> Option<Installed> {
        None
    }

    /// Windows: an ancestor of `folder` up to `home` that a broad principal
    /// (Users, Authenticated Users, Everyone, a sandbox account…) may read or
    /// write, so neighbours are exposed. `None` when none is, or off Windows.
    fn shared_ancestor(&self, _folder: &Path, _home: &Path) -> Option<PathBuf> {
        None
    }

    /// Windows: the Git Bash `bash.exe` the sandbox launches, resolved like
    /// Claude Code does. `None` when none is found or off Windows.
    fn git_bash(&self) -> Option<PathBuf> {
        None
    }

    /// Windows: applies the folder's slot ACEs at session start (the slot must
    /// be live and the tree granted) and returns the slot number. `root` is
    /// the mark root, `gitconfig` the name-only copy, `read_dirs` the resolved
    /// program directories. The default is off-Windows only.
    fn windows_prepare(
        &self,
        _root: &Path,
        _gitconfig: &Path,
        _read_dirs: &[PathBuf],
    ) -> Result<u32, Refusal> {
        Err(Refusal::NotPrepared)
    }
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

    fn sandbox_install(&self) -> Option<Installed> {
        #[cfg(windows)]
        {
            let mark = super::win::read_mark()?;
            let me = super::win::current_user_sid().ok()?;
            Some(Installed {
                version: mark.version,
                slots: mark.slots,
                owner_is_me: mark.owner_sid.eq_ignore_ascii_case(&me),
            })
        }
        #[cfg(not(windows))]
        {
            None
        }
    }

    fn shared_ancestor(&self, _folder: &Path, _home: &Path) -> Option<PathBuf> {
        #[cfg(windows)]
        {
            super::win::acl::shared_ancestor(_folder, _home)
        }
        #[cfg(not(windows))]
        {
            None
        }
    }

    fn git_bash(&self) -> Option<PathBuf> {
        #[cfg(windows)]
        {
            real_git_bash(self)
        }
        #[cfg(not(windows))]
        {
            None
        }
    }

    fn windows_prepare(
        &self,
        _root: &Path,
        _gitconfig: &Path,
        _read_dirs: &[PathBuf],
    ) -> Result<u32, Refusal> {
        #[cfg(windows)]
        {
            real_windows_prepare(self, _root, _gitconfig, _read_dirs)
        }
        #[cfg(not(windows))]
        {
            Err(Refusal::NotPrepared)
        }
    }
}

/// The real Windows session-start preparation: check the slot is live and the
/// tree granted, re-stamp protected names, keep the gitconfig ACE, and sync
/// the `read-dirs` group grants.
#[cfg(windows)]
fn real_windows_prepare(
    probe: &dyn Probe,
    root: &Path,
    gitconfig: &Path,
    read_dirs: &[PathBuf],
) -> Result<u32, Refusal> {
    use super::win;
    let home = probe.home().ok_or(Refusal::NoHome)?;
    let home = probe.canonical(&home).unwrap_or(home);
    let win_dir = win::win_dir(&home);
    let mark = win::read_mark().ok_or(Refusal::SandboxNotInstalled)?;
    let root_str = root.to_string_lossy().to_string();
    let slot = win::slots::active_slot(&win_dir, &root_str)
        .map_err(|_| Refusal::NotPrepared)?
        .ok_or(Refusal::NotPrepared)?;
    let slot_sid = mark.slot_sid(slot).ok_or(Refusal::NotPrepared)?;
    if !win::acl::has_ace(root, slot_sid) {
        return Err(Refusal::NotPrepared);
    }
    let _ = win::acl::stamp_protected(root, slot_sid, false);
    if !win::acl::has_ace(gitconfig, slot_sid) {
        let _ = win::acl::grant_file(gitconfig, slot_sid, win::acl::FILE_READ);
    }
    // Sync the read-dir group grants under the slots lock.
    let desired: Vec<String> = read_dirs
        .iter()
        .map(|d| d.to_string_lossy().to_string())
        .collect();
    if let Ok(previous) = win::slots::read_dirs(&win_dir) {
        for dir in &desired {
            if !previous.iter().any(|p| p.eq_ignore_ascii_case(dir)) {
                let _ = win::acl::grant_tree_read(Path::new(dir), &mark.group_sid);
            }
        }
        for dir in &previous {
            if !desired.iter().any(|d| d.eq_ignore_ascii_case(dir)) {
                let _ = win::acl::revoke_tree_read(Path::new(dir), &mark.group_sid);
            }
        }
        let _ = win::slots::set_read_dirs(&win_dir, &desired);
    }
    Ok(slot)
}

/// Git Bash the way Claude Code finds it: `CLAUDE_CODE_GIT_BASH_PATH` when it
/// names an existing `bash.exe`/`sh.exe`, else from `git.exe` on `PATH` (the
/// Git root is the parent of its `cmd` or `bin` directory, then `<root>\bin\
/// bash.exe`). Never `%SystemRoot%\System32\bash.exe` (that runs WSL).
#[cfg(windows)]
fn real_git_bash(probe: &dyn Probe) -> Option<PathBuf> {
    let under_system = |path: &Path| {
        probe
            .var("SystemRoot")
            .map(PathBuf::from)
            .and_then(|root| probe.canonical(&root))
            .is_some_and(|root| {
                probe
                    .canonical(path)
                    .is_some_and(|resolved| paths::within(&root, &resolved))
            })
    };
    let named = |name: &Path| {
        name.file_name()
            .and_then(|n| n.to_str())
            .is_some_and(|n| n.eq_ignore_ascii_case("bash.exe") || n.eq_ignore_ascii_case("sh.exe"))
    };
    if let Some(configured) = probe
        .var("CLAUDE_CODE_GIT_BASH_PATH")
        .filter(|v| !v.trim().is_empty())
        .map(PathBuf::from)
        && probe.exists(&configured)
        && named(&configured)
        && !under_system(&configured)
    {
        return Some(configured);
    }
    let git = probe.which("git.exe")?;
    let git = probe.canonical(&git).unwrap_or(git);
    // <root>\cmd\git.exe or <root>\bin\git.exe -> <root>\bin\bash.exe.
    let root = git.parent()?.parent()?;
    let bash = root.join("bin").join("bash.exe");
    (probe.exists(&bash) && !under_system(&bash)).then_some(bash)
}

/// Why a folder of this device cannot be sandboxed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Refusal {
    UnsupportedOs,
    /// Windows: `cctg sandbox-install` has not run on this device.
    SandboxNotInstalled,
    /// Windows: the install is of an older schema; re-run `sandbox-install`.
    SandboxOutdated,
    /// Windows: another Windows user installed the sandbox on this machine.
    SandboxOtherOwner,
    /// Windows: no Git Bash outside the profile (or in `read-dirs`).
    NoGitBash,
    /// Windows: every slot account is taken; the count is `N`.
    NoFreeSlot(u32),
    /// Windows: the folder has no live slot or ACE; run `cctg sandbox on`.
    NotPrepared,
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
    /// The marks file cannot be read, is damaged or cannot be written
    /// (TASK-090, the menu switch).
    MarksFile,
    /// Windows: commands of the sandbox created protected names in the
    /// folder (`.git`, `.claude`, `.vscode`, `.idea`, shell configs...), this
    /// many; turning it off is left to the terminal, which lists them
    /// (TASK-090).
    SandboxWroteProtected(usize),
    /// Windows: the folder's permissions for the sandbox account could not
    /// be set; what failed, as a verb phrase (TASK-090).
    Permissions(&'static str),
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
    /// Windows: outside `%USERPROFILE%`, where neighbours are world-open.
    OutsideProfile,
    /// Windows: an ancestor is open to other accounts of the machine.
    SharedParent,
    /// Windows: inside or above another marked folder (one slot per tree).
    Nested,
}

impl fmt::Display for Refusal {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::UnsupportedOs => f.write_str("сэндбокс работает на Linux, macOS и Windows"),
            Self::SandboxNotInstalled => f.write_str(
                "сэндбокс на этом устройстве не установлен: выполните cctg sandbox-install \
                 (один запрос UAC)",
            ),
            Self::SandboxOutdated => {
                f.write_str("установка сэндбокса устарела: выполните cctg sandbox-install")
            }
            Self::SandboxOtherOwner => {
                f.write_str("сэндбокс на этом компьютере установлен другим пользователем Windows")
            }
            Self::NoGitBash => f.write_str(
                "нужен Git Bash (Git for Windows) вне профиля пользователя или в \
                 ~/.cctg/sandbox/read-dirs",
            ),
            Self::NoFreeSlot(n) => write!(
                f,
                "все {n} учёток сэндбокса заняты помеченными папками: снимите метку с ненужной \
                 или выполните cctg sandbox-install --slots M"
            ),
            Self::NotPrepared => f.write_str(
                "папка не подготовлена для сэндбокса: выполните в ней cctg sandbox on ещё раз",
            ),
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
            Self::MarksFile => f.write_str(
                "файл меток сэндбокса на устройстве не читается, повреждён или не \
                 записывается: cctg doctor",
            ),
            Self::SandboxWroteProtected(n) => write!(
                f,
                "команды из сэндбокса создали служебные файлы или папки (.git, .claude, \
                 .vscode, .idea и подобные: {n}); снимите сэндбокс в терминале: \
                 cctg sandbox off в этой папке покажет их"
            ),
            Self::Permissions(what) => write!(f, "не удалось {what}; нажмите ещё раз"),
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
            Self::OutsideProfile => {
                "папка вне профиля пользователя: на Windows соседние папки там открыты любой \
                 учётной записи"
            }
            Self::SharedParent => "каталог над папкой открыт другим учётным записям компьютера",
            Self::Nested => "папка внутри или над другой помеченной папкой",
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
/// start claude without the sandbox. For `cctg sandbox on` and doctor. On
/// Windows the `.cmd` wrapper (used from cmd/PowerShell) and the shell-prefix
/// shim must be the new ones too.
pub fn wrapper(probe: &dyn Probe) -> Result<(), Refusal> {
    let home = probe.home().ok_or(Refusal::NoHome)?;
    match probe.read(&home.join(WRAPPER)) {
        Ok(text) if text.contains("sandbox-check") => {}
        _ => return Err(Refusal::OldWrapper),
    }
    if probe.os() == Os::Windows {
        let cmd = match probe.read(&home.join(WRAPPER).with_extension("cmd")) {
            Ok(text) => text,
            Err(_) => return Err(Refusal::OldWrapper),
        };
        if !cmd.contains("sandbox-check") || !cmd.contains("--cmd") {
            return Err(Refusal::OldWrapper);
        }
        match probe.read(&super::win_shim_path(&home)) {
            Ok(text) if text.contains("sandbox-exec") => {}
            _ => return Err(Refusal::OldWrapper),
        }
    }
    Ok(())
}

fn os(probe: &dyn Probe) -> Result<(), Refusal> {
    match probe.os() {
        Os::Linux | Os::MacOs | Os::Windows => Ok(()),
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
    if probe.os() == Os::Windows {
        // The folder must lie strictly inside the profile: outside it,
        // neighbours are open to any account (icacls of C:\ grants Users and
        // Authenticated Users on new folders). The Home problem above already
        // caught the folder being the profile or above it.
        if !paths::within(&home, folder) {
            return bad(FolderProblem::OutsideProfile);
        }
        if probe.shared_ancestor(folder, &home).is_some() {
            return bad(FolderProblem::SharedParent);
        }
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
        Os::Windows => windows_tools(probe),
        Os::Other => os(probe),
    }
}

/// Windows tool checks: the sandbox must be installed, of this schema and this
/// user, with a Git Bash outside the profile (or in `read-dirs`).
fn windows_tools(probe: &dyn Probe) -> Result<(), Refusal> {
    let installed = probe
        .sandbox_install()
        .ok_or(Refusal::SandboxNotInstalled)?;
    if installed.version != super::SETUP_VERSION {
        return Err(Refusal::SandboxOutdated);
    }
    if !installed.owner_is_me {
        return Err(Refusal::SandboxOtherOwner);
    }
    let bash = probe.git_bash().ok_or(Refusal::NoGitBash)?;
    let home = probe.home().ok_or(Refusal::NoHome)?;
    let home = probe.canonical(&home).unwrap_or(home);
    let bash_resolved = probe.canonical(&bash).unwrap_or(bash);
    if paths::within(&home, &bash_resolved) && !read_dir_covers(probe, &home, &bash_resolved) {
        return Err(Refusal::NoGitBash);
    }
    Ok(())
}

/// Whether a directory in `~/.cctg/sandbox/read-dirs` (resolved leniently)
/// contains `path`. For a Git Bash that lives under the profile.
fn read_dir_covers(probe: &dyn Probe, home: &Path, path: &Path) -> bool {
    let text = match probe.read(&super::sandbox_home(home).join("read-dirs")) {
        Ok(text) => text,
        Err(_) => return false,
    };
    text.lines().any(|line| {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') {
            return false;
        }
        let dir = match line.strip_prefix('~') {
            Some("") => home.to_path_buf(),
            Some(rest) if rest.starts_with('/') => home.join(&rest[1..]),
            _ => PathBuf::from(line),
        };
        probe
            .canonical(&dir)
            .is_some_and(|dir| paths::within(&dir, path))
    })
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
        /// Windows: the install mark the checks see.
        pub installed: Option<Installed>,
        /// Windows: a shared ancestor of a folder, if the test sets one.
        pub shared_parent: Option<PathBuf>,
        /// Windows: the Git Bash path.
        pub git_bash: Option<PathBuf>,
        /// Windows: the slot `windows_prepare` returns.
        pub win_slot: u32,
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
                installed: None,
                shared_parent: None,
                git_bash: None,
                win_slot: 1,
            };
            fake.answer("claude", 0, "2.1.284 (Claude Code)\n");
            fake.answer("bwrap", 0, "");
            fake.files.insert(
                home.join(WRAPPER),
                "#!/bin/sh\np=$(cctg sandbox-check --settings s)\n".to_owned(),
            );
            fake
        }

        /// A Windows device that can sandbox: installed, ours, with a Git Bash
        /// outside the profile and the wrapper/shim in place.
        pub(crate) fn windows(home: &Path) -> Self {
            let mut fake = Self::linux(home);
            fake.os = Os::Windows;
            fake.tools.clear();
            fake.runs.remove("bwrap");
            fake.installed = Some(Installed {
                version: super::super::SETUP_VERSION,
                slots: 8,
                owner_is_me: true,
            });
            let bash = home
                .parent()
                .unwrap()
                .join("Git")
                .join("bin")
                .join("bash.exe");
            std::fs::create_dir_all(bash.parent().unwrap()).unwrap();
            std::fs::write(&bash, b"").unwrap();
            fake.git_bash = Some(bash);
            // The wrapper (sh + cmd) and the shim carry the new markers.
            fake.files.insert(
                home.join(WRAPPER).with_extension("cmd"),
                "sandbox-check --settings s --cmd\n".to_owned(),
            );
            fake.files.insert(
                super::super::win_shim_path(home),
                "cctg sandbox-exec \"$@\"\n".to_owned(),
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
        fn sandbox_install(&self) -> Option<Installed> {
            self.installed
        }
        fn shared_ancestor(&self, _folder: &Path, _home: &Path) -> Option<PathBuf> {
            self.shared_parent.clone()
        }
        fn git_bash(&self) -> Option<PathBuf> {
            self.git_bash.clone()
        }
        fn windows_prepare(
            &self,
            _root: &Path,
            _gitconfig: &Path,
            _read_dirs: &[PathBuf],
        ) -> Result<u32, Refusal> {
            Ok(self.win_slot)
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
    fn an_unsupported_system_is_refused_first() {
        let (_dir, home, folder) = home_and_folder("preflight-os");
        let mut fake = Fake::linux(&home);
        fake.os = Os::Other;
        assert_eq!(check(&fake, &folder), Err(Refusal::UnsupportedOs));
        assert!(Refusal::UnsupportedOs.to_string().contains("Windows"));
    }

    /// Windows: not installed -> SandboxNotInstalled; old schema, other owner
    /// and no Git Bash each surface; everything present -> Ready.
    #[test]
    fn windows_checks_the_install_and_git_bash() {
        let (_dir, home, folder) = home_and_folder("preflight-win");
        // Not installed.
        let mut bare = Fake::windows(&home);
        bare.installed = None;
        assert_eq!(check(&bare, &folder), Err(Refusal::SandboxNotInstalled));
        // Outdated schema.
        let mut old = Fake::windows(&home);
        old.installed = Some(Installed {
            version: super::super::SETUP_VERSION + 1,
            slots: 8,
            owner_is_me: true,
        });
        assert_eq!(check(&old, &folder), Err(Refusal::SandboxOutdated));
        // Another owner.
        let mut other = Fake::windows(&home);
        other.installed = Some(Installed {
            version: super::super::SETUP_VERSION,
            slots: 8,
            owner_is_me: false,
        });
        assert_eq!(check(&other, &folder), Err(Refusal::SandboxOtherOwner));
        // No Git Bash.
        let mut no_bash = Fake::windows(&home);
        no_bash.git_bash = None;
        assert_eq!(check(&no_bash, &folder), Err(Refusal::NoGitBash));
        // Everything present.
        let ready = Fake::windows(&home);
        assert!(
            check(&ready, &folder).is_ok(),
            "{:?}",
            check(&ready, &folder)
        );
        assert_eq!(wrapper(&ready), Ok(()));
    }

    /// Windows: a folder outside the profile, and one with a shared ancestor.
    #[test]
    fn windows_refuses_folders_outside_the_profile_or_shared() {
        let dir = TempDir::new("preflight-win-folder");
        let base = paths::canonical(dir.path()).unwrap();
        let home = base.join("home");
        let folder = home.join("proj");
        std::fs::create_dir_all(&folder).unwrap();
        let outside = base.join("elsewhere");
        std::fs::create_dir_all(&outside).unwrap();
        let fake = Fake::windows(&home);
        assert_eq!(
            check(&fake, &outside),
            Err(Refusal::BadFolder(FolderProblem::OutsideProfile))
        );
        let mut shared = Fake::windows(&home);
        shared.shared_parent = Some(home.clone());
        assert_eq!(
            check(&shared, &folder),
            Err(Refusal::BadFolder(FolderProblem::SharedParent))
        );
    }

    /// Windows: the `.cmd` wrapper and the shim must be current too.
    #[test]
    fn windows_wrapper_checks_the_cmd_and_shim() {
        let (_dir, home, folder) = home_and_folder("preflight-win-wrapper");
        let mut fake = Fake::windows(&home);
        assert!(check(&fake, &folder).is_ok());
        assert_eq!(wrapper(&fake), Ok(()));
        // An old .cmd without --cmd.
        fake.files.insert(
            home.join(WRAPPER).with_extension("cmd"),
            "run -- ...\n".to_owned(),
        );
        assert_eq!(wrapper(&fake), Err(Refusal::OldWrapper));
        // The shim missing.
        let mut no_shim = Fake::windows(&home);
        no_shim.files.remove(&super::super::win_shim_path(&home));
        assert_eq!(wrapper(&no_shim), Err(Refusal::OldWrapper));
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
