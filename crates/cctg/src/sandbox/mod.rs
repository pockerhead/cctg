//! Sandbox mode of a folder (TASK-087 part A): a session started in a marked
//! folder cannot read or write outside it.
//!
//! The mark lives on the device (`cctg sandbox on` -> [`marks`], it covers
//! the folder and everything inside). Before claude starts, the wrapper asks
//! cctg; for a marked folder cctg checks the device ([`preflight`]) and
//! writes a settings profile ([`profile`]): our `settings.json` plus the
//! strict Claude Code sandbox for Bash, the read block outside the working
//! directory and denials of the tools that lead out of the folder.
//!
//! What part A closes on Linux and macOS: commands of the Bash tool and
//! their children (the OS sandbox of Claude Code), reads and writes of the
//! file tools. What it does not: `!` and slash commands the owner types in
//! the terminal, user hooks and plugins, system directories, the network.
//! Native Windows is refused. Details for people: `docs/sandbox.md`.
//!
//! Modules take the environment and the file system through parameters
//! (a `var` closure, [`preflight::Probe`]), never `std::env` inside the
//! logic: tests run in parallel.

use std::io::Write;
use std::path::{Path, PathBuf};

pub mod cli;
pub mod console;
pub mod gate;
pub mod marks;
pub mod paths;
pub mod preflight;
pub mod profile;

/// `"1"`: this session runs with a sandbox profile (its `env` sets it).
pub const ACTIVE_VAR: &str = "CCTG_SANDBOX";

/// The home directory from `var` (Windows: `USERPROFILE`, then `HOME`).
pub fn home_dir_of(var: &impl Fn(&str) -> Option<String>) -> Option<PathBuf> {
    crate::device::home_dir(var)
}

/// `<home>/.cctg/sandbox`: marks (`folders.json`), the read allowlist
/// (`read-dirs`), temp dirs (`tmp/<hash>`) and gitconfig copies
/// (`git/<hash>`). Sandboxed commands never write here.
pub fn sandbox_home(home: &Path) -> PathBuf {
    home.join(".cctg").join("sandbox")
}

/// `<home>/.cctg/sandbox/folders.json`.
pub fn marks_file(home: &Path) -> PathBuf {
    sandbox_home(home).join("folders.json")
}

/// Claude Code's config dir: a non-empty `CLAUDE_CONFIG_DIR`, else
/// `<home>/.claude`.
pub fn claude_config_dir(var: &impl Fn(&str) -> Option<String>) -> Option<PathBuf> {
    match var("CLAUDE_CONFIG_DIR").filter(|value| !value.trim().is_empty()) {
        Some(config) => Some(PathBuf::from(config)),
        None => home_dir_of(var).map(|home| home.join(".claude")),
    }
}

/// Is this agent's session sandboxed? For cctg's own ways out of the folder
/// (console commands, `send_file`, the inbox).
#[derive(Debug, Clone)]
pub struct Guard {
    /// The session runs with a profile ([`ACTIVE_VAR`]).
    pub active: bool,
    /// The session folder.
    pub root: PathBuf,
    /// `None` without a home directory: then no mark can exist.
    pub marks_file: Option<PathBuf>,
}

impl Guard {
    /// Sandboxed when the profile says so or the folder is marked. A marks
    /// file that cannot be read counts as marked (fail closed).
    pub fn on(&self) -> bool {
        self.active
            || self
                .marks_file
                .as_deref()
                .is_some_and(|file| marks::covered(file, &self.root) != Ok(false))
    }

    /// `root`: an absolute `CLAUDE_PROJECT_DIR`, else the current directory.
    pub fn from_env() -> Self {
        let var = |name: &str| std::env::var(name).ok();
        let root = var("CLAUDE_PROJECT_DIR")
            .map(PathBuf::from)
            .filter(|dir| dir.is_absolute())
            .or_else(|| std::env::current_dir().ok())
            .unwrap_or_default();
        Self {
            active: var(ACTIVE_VAR).as_deref() == Some("1"),
            root: paths::canonical(&root).unwrap_or(root),
            marks_file: home_dir_of(&var).map(|home| marks_file(&home)),
        }
    }
}

/// Creates `dir` and its missing parents; new ones are private (0700 on
/// unix).
pub(crate) fn create_private_dir(dir: &Path) -> std::io::Result<()> {
    let mut builder = std::fs::DirBuilder::new();
    builder.recursive(true);
    #[cfg(unix)]
    std::os::unix::fs::DirBuilderExt::mode(&mut builder, 0o700);
    builder.create(dir)
}

/// Writes `bytes` to `path` through a temp file and a rename, only when the
/// file holds other bytes (an unchanged file keeps its time: `Worker::plan`
/// restarts a session whose settings file changed). The file is private
/// (0600 on unix). `Ok(true)` when it was written.
pub(crate) fn write_if_changed(path: &Path, bytes: &[u8]) -> std::io::Result<bool> {
    if std::fs::read(path).is_ok_and(|old| old == bytes) {
        return Ok(false);
    }
    let name = path
        .file_name()
        .ok_or_else(|| std::io::Error::other("no file name"))?
        .to_string_lossy();
    let tmp = path.with_file_name(format!("{name}.tmp-{}", std::process::id()));
    let written = (|| {
        let mut options = std::fs::OpenOptions::new();
        options.write(true).create_new(true);
        #[cfg(unix)]
        std::os::unix::fs::OpenOptionsExt::mode(&mut options, 0o600);
        let mut file = options.open(&tmp)?;
        file.write_all(bytes)?;
        file.sync_all()?;
        drop(file);
        std::fs::rename(&tmp, path)
    })();
    if written.is_err() {
        let _ = std::fs::remove_file(&tmp);
    }
    written.map(|()| true)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::hub::testdir::TempDir;

    fn vars<'a>(pairs: &'a [(&'a str, &'a str)]) -> impl Fn(&str) -> Option<String> + 'a {
        move |name| {
            pairs
                .iter()
                .find(|(key, _)| *key == name)
                .map(|(_, value)| (*value).to_owned())
        }
    }

    #[test]
    fn the_config_dir_is_the_variable_else_under_home() {
        let home_var = if cfg!(windows) { "USERPROFILE" } else { "HOME" };
        let home = if cfg!(windows) { r"C:\h" } else { "/h" };
        assert_eq!(
            claude_config_dir(&vars(&[(home_var, home)])),
            Some(Path::new(home).join(".claude"))
        );
        assert_eq!(
            claude_config_dir(&vars(&[(home_var, home), ("CLAUDE_CONFIG_DIR", " ")])),
            Some(Path::new(home).join(".claude")),
            "a blank value is unset"
        );
        assert_eq!(
            claude_config_dir(&vars(&[(home_var, home), ("CLAUDE_CONFIG_DIR", "/cfg")])),
            Some(PathBuf::from("/cfg"))
        );
        assert_eq!(claude_config_dir(&vars(&[])), None);
        assert_eq!(
            sandbox_home(Path::new(home)),
            Path::new(home).join(".cctg").join("sandbox")
        );
    }

    #[test]
    fn a_guard_is_on_by_profile_by_mark_or_by_a_broken_marks_file() {
        let dir = TempDir::new("sandbox-guard");
        let home = dir.path().join("home");
        let folder = dir.path().join("proj");
        std::fs::create_dir_all(&folder).unwrap();
        let file = marks_file(&home);
        let guard = |active| Guard {
            active,
            root: folder.clone(),
            marks_file: Some(file.clone()),
        };
        assert!(!guard(false).on(), "no marks file: not marked");
        assert!(guard(true).on());
        marks::add(&file, &folder).unwrap();
        assert!(guard(false).on());
        std::fs::write(&file, b"{broken").unwrap();
        assert!(guard(false).on(), "fail closed");
        let homeless = Guard {
            active: false,
            root: folder.clone(),
            marks_file: None,
        };
        assert!(!homeless.on());
    }

    #[test]
    fn a_file_is_written_only_when_its_bytes_change() {
        let dir = TempDir::new("sandbox-write");
        let path = dir.path().join("x.json");
        assert!(write_if_changed(&path, b"a").unwrap());
        assert!(!write_if_changed(&path, b"a").unwrap());
        assert!(write_if_changed(&path, b"b").unwrap());
        assert_eq!(std::fs::read(&path).unwrap(), b"b");
        let names: Vec<_> = std::fs::read_dir(dir.path())
            .unwrap()
            .map(|entry| entry.unwrap().file_name())
            .collect();
        assert_eq!(names.len(), 1, "no temp file left: {names:?}");
    }
}
