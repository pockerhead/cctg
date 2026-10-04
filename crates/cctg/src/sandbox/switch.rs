//! The core of `cctg sandbox on|off` without printing, for the agent's
//! switch from the bot menu (TASK-090). The CLI keeps its own flow and
//! prints itself; from here it takes only the Windows parts
//! ([`windows_on`], [`windows_owned`], [`windows_release`]).
//!
//! [`Refusal`] texts of this module reach Telegram: they name no path.
//! Whether the Windows parts run is decided by [`Probe::os`], not only by
//! the build: tests with a Linux probe on a Windows machine never reach the
//! real slot accounts and ACLs.

use std::path::Path;
#[cfg(windows)]
use std::path::PathBuf;

use super::marks::{self, Removed};
use super::preflight::{self, Os, Probe, Refusal};
use super::profile;
use crate::wire::SandboxState;

/// What a switch left the folder with.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Switched {
    /// The folder has its own mark.
    On,
    /// No mark covers the folder.
    Off,
    /// The mark of a folder above covers it, and it has none of its own.
    Inherited,
}

/// The folder's own mark is in `marks`.
fn own_mark(marks: &[std::path::PathBuf], folder: &Path) -> bool {
    let folder = marks::canonical(folder);
    marks.iter().any(|mark| marks::same(mark, &folder))
}

/// The sandbox state of `folder` for a session that runs with (`active`) or
/// without a profile. A damaged marks file asks for the sandbox and counts
/// as no folder above. Blocking.
pub fn state(marks_file: Option<&Path>, folder: &Path, active: bool) -> SandboxState {
    let Some(file) = marks_file else {
        return SandboxState {
            active,
            wanted: false,
            inherited: false,
        };
    };
    let wanted = marks::covered(file, folder) != Ok(false);
    let inherited = marks::load(file).is_ok_and(|marks| {
        marks::covering(&marks, &marks::canonical(folder)).is_some() && !own_mark(&marks, folder)
    });
    SandboxState {
        active,
        wanted,
        inherited,
    }
}

/// Marks `folder` as `cctg sandbox on` does: checks the device, on Windows
/// takes a slot and grants it the tree, writes the mark and, with
/// `base_settings` (cctg's `settings.json` of a `claude-cctg` session), the
/// folder's profile. A refused profile takes the mark (and the slot) back.
/// A folder already covered changes nothing. Blocking (minutes on Windows).
pub fn turn_on(
    probe: &dyn Probe,
    marks_file: &Path,
    folder: &Path,
    exe: &Path,
    base_settings: Option<&Path>,
) -> Result<Switched, Refusal> {
    match marks::covered(marks_file, folder) {
        Err(_) => return Err(Refusal::MarksFile),
        Ok(true) => {
            let marks = marks::load(marks_file).map_err(|_| Refusal::MarksFile)?;
            return Ok(if own_mark(&marks, folder) {
                Switched::On
            } else {
                Switched::Inherited
            });
        }
        Ok(false) => {}
    }
    preflight::check(probe, folder)?;
    preflight::wrapper(probe)?;
    let windows = probe.os() == Os::Windows;
    if windows {
        #[cfg(windows)]
        windows_on(
            marks_file,
            folder,
            probe.home().as_deref(),
            Some(exe),
            &mut |_| {},
        )
        .map_err(|error| match error {
            WinOnError::Nested => Refusal::BadFolder(preflight::FolderProblem::Nested),
            WinOnError::Refused(refusal) => refusal,
            WinOnError::GrantFailed => Refusal::Io("права учётки сэндбокса на папку"),
            WinOnError::StampFailed => Refusal::Io("защиту служебных файлов папки"),
        })?;
        #[cfg(not(windows))]
        return Err(Refusal::UnsupportedOs);
    }
    // Takes back what the Windows part did (nothing elsewhere).
    let release = || {
        #[cfg(windows)]
        if windows && let Some(home) = probe.home() {
            windows_release(folder, &home);
        }
    };
    if marks::add(marks_file, folder).is_err() {
        release();
        return Err(Refusal::MarksFile);
    }
    if let Some(base) = base_settings
        && let Err(refusal) = profile::prepare(probe, base, folder, exe)
    {
        let _ = marks::remove(marks_file, folder);
        release();
        return Err(refusal);
    }
    Ok(Switched::On)
}

/// Unmarks `folder` as `cctg sandbox off` does. A mark above stays (it is
/// taken off in that folder). On Windows it refuses while files the
/// sandbox's account created in protected places are there: the terminal
/// lists them. Blocking.
pub fn turn_off(probe: &dyn Probe, marks_file: &Path, folder: &Path) -> Result<Switched, Refusal> {
    let marks = marks::load(marks_file).map_err(|_| Refusal::MarksFile)?;
    if !own_mark(&marks, folder) {
        let above = marks::covering(&marks, &marks::canonical(folder)).is_some();
        return Ok(if above {
            Switched::Inherited
        } else {
            Switched::Off
        });
    }
    #[cfg(windows)]
    let home = if probe.os() == Os::Windows {
        let home = probe.home().ok_or(Refusal::NoHome)?;
        let owned = windows_owned(folder, &home);
        if !owned.is_empty() {
            return Err(Refusal::SandboxWroteProtected(owned.len()));
        }
        Some(home)
    } else {
        None
    };
    #[cfg(not(windows))]
    let _ = probe;
    match marks::remove(marks_file, folder) {
        Err(_) => return Err(Refusal::MarksFile),
        Ok(Removed::Ancestor(_)) => return Ok(Switched::Inherited),
        // Unmarked meanwhile from the terminal.
        Ok(Removed::None) => return Ok(Switched::Off),
        Ok(Removed::Exact) => {}
    }
    #[cfg(windows)]
    if let Some(home) = &home {
        windows_release(folder, home);
    }
    Ok(if marks::covered(marks_file, folder) != Ok(false) {
        Switched::Inherited
    } else {
        Switched::Off
    })
}

/// Progress of [`windows_on`].
#[cfg(windows)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WinStep {
    /// The grant of the tree begins.
    Granting,
    /// Files seen so far.
    Seen(usize),
}

/// Why [`windows_on`] did not prepare the folder.
#[cfg(windows)]
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum WinOnError {
    /// Another mark is inside or above the folder (one slot per tree).
    Nested,
    Refused(Refusal),
    /// The slot could not be granted the tree.
    GrantFailed,
    /// The protected names could not be stamped.
    StampFailed,
}

/// The Windows preparation of a mark: refuse a nested mark, take a slot,
/// make the folder dirs and grant the slot the tree (before the mark is
/// written, so a failure leaves no mark; the slot is freed again). `exe`,
/// when given, is made readable for the sandbox group. Returns how many
/// hard-linked files were denied write.
#[cfg(windows)]
pub fn windows_on(
    marks_file: &Path,
    folder: &Path,
    home: Option<&Path>,
    exe: Option<&Path>,
    step: &mut dyn FnMut(WinStep),
) -> Result<usize, WinOnError> {
    use super::{paths, win};
    // One slot per tree: no other mark inside or above.
    if let Ok(marks) = marks::load(marks_file) {
        for mark in &marks {
            if paths::within(folder, mark) && !paths::within(mark, folder)
                || paths::within(mark, folder) && !paths::within(folder, mark)
            {
                return Err(WinOnError::Nested);
            }
        }
    }
    let mark = win::read_mark().ok_or(WinOnError::Refused(Refusal::SandboxNotInstalled))?;
    let home = home.ok_or(WinOnError::Refused(Refusal::NoHome))?;
    let win_dir = win::win_dir(home);
    let folder_str = folder.to_string_lossy().to_string();
    let k = win::slots::take(&win_dir, mark.slots, &folder_str).map_err(WinOnError::Refused)?;
    let slot_sid = mark
        .slot_sid(k)
        .ok_or(WinOnError::Refused(Refusal::NotPrepared))?
        .to_owned();
    // A slot is consumed by `take`; anything that fails before the mark is
    // written frees it again (retire + best-effort revoke), so a failed
    // `sandbox on` never leaves an orphaned slot that `sandbox off` cannot
    // clean (no mark exists, so off takes the `None` branch).
    let unwind = |slot_sid: &str| {
        win::acl::revoke_folder(folder, slot_sid);
        let _ = win::slots::retire(&win_dir, &folder_str);
    };
    if let Err(refusal) = profile::folder_dirs(folder, true) {
        unwind(&slot_sid);
        return Err(WinOnError::Refused(refusal));
    }
    step(WinStep::Granting);
    let count = match win::acl::grant_folder(folder, &slot_sid, |seen| step(WinStep::Seen(seen))) {
        Ok(count) => count,
        Err(_) => {
            unwind(&slot_sid);
            return Err(WinOnError::GrantFailed);
        }
    };
    if win::acl::stamp_protected(folder, &slot_sid, true).is_err() {
        unwind(&slot_sid);
        return Err(WinOnError::StampFailed);
    }
    if let Some(exe) = exe {
        let _ = win::acl::grant_file(exe, &mark.group_sid, win::acl::GROUP_READ_EXEC);
    }
    Ok(count)
}

/// The protected names of `folder` that the account of its active slot
/// created (none without an install or a slot).
#[cfg(windows)]
pub fn windows_owned(folder: &Path, home: &Path) -> Vec<PathBuf> {
    use super::win;
    let folder_str = folder.to_string_lossy().to_string();
    let slot = win::slots::active_slot(&win::win_dir(home), &folder_str)
        .ok()
        .flatten();
    match (win::read_mark(), slot) {
        (Some(mark), Some(k)) => mark
            .slot_sid(k)
            .map(|slot_sid| win::acl::slot_owned_protected(folder, slot_sid))
            .unwrap_or_default(),
        _ => Vec::new(),
    }
}

/// Takes the slot's ACEs off `folder` (when it has an active slot) and
/// retires the slot: a new owner gets a fresh SID. Returns how many objects
/// kept an ACE.
#[cfg(windows)]
pub fn windows_release(folder: &Path, home: &Path) -> usize {
    use super::win;
    let win_dir = win::win_dir(home);
    let folder_str = folder.to_string_lossy().to_string();
    let slot = win::slots::active_slot(&win_dir, &folder_str)
        .ok()
        .flatten();
    let mut errors = 0;
    if let (Some(mark), Some(k)) = (win::read_mark(), slot)
        && let Some(slot_sid) = mark.slot_sid(k)
    {
        errors = win::acl::revoke_folder(folder, slot_sid);
    }
    let _ = win::slots::retire(&win_dir, &folder_str);
    errors
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::sandbox::preflight::tests::{Fake, home_and_folder};

    /// cctg's `settings.json` under `home`, and a cctg executable path.
    fn base_and_exe(home: &Path) -> (std::path::PathBuf, std::path::PathBuf) {
        let conf = home.join(".cctg").join("claude");
        std::fs::create_dir_all(&conf).unwrap();
        let settings = conf.join("settings.json");
        std::fs::write(&settings, b"{}").unwrap();
        (settings, home.join("cctg.exe"))
    }

    #[test]
    fn turning_on_writes_the_mark_and_the_profile_once() {
        let (_dir, home, folder) = home_and_folder("switch-on");
        let (settings, exe) = base_and_exe(&home);
        let file = crate::sandbox::marks_file(&home);
        let fake = Fake::linux(&home);
        assert_eq!(
            turn_on(&fake, &file, &folder, &exe, Some(&settings)),
            Ok(Switched::On)
        );
        assert_eq!(marks::covered(&file, &folder), Ok(true));
        assert!(profile::profile_path(&settings, &folder).is_file());
        let before = std::fs::read(&file).unwrap();
        assert_eq!(
            turn_on(&fake, &file, &folder, &exe, Some(&settings)),
            Ok(Switched::On),
            "already on"
        );
        assert_eq!(std::fs::read(&file).unwrap(), before);
        // A subfolder is covered by it: nothing written.
        let sub = folder.join("sub");
        std::fs::create_dir_all(&sub).unwrap();
        assert_eq!(
            turn_on(&fake, &file, &sub, &exe, Some(&settings)),
            Ok(Switched::Inherited)
        );
        assert_eq!(std::fs::read(&file).unwrap(), before);
    }

    #[test]
    fn a_session_not_started_by_the_wrapper_gets_the_mark_without_a_profile() {
        let (_dir, home, folder) = home_and_folder("switch-on-bare");
        let (settings, exe) = base_and_exe(&home);
        let file = crate::sandbox::marks_file(&home);
        assert_eq!(
            turn_on(&Fake::linux(&home), &file, &folder, &exe, None),
            Ok(Switched::On)
        );
        assert_eq!(marks::covered(&file, &folder), Ok(true));
        assert!(!profile::profile_path(&settings, &folder).exists());
    }

    #[test]
    fn a_refused_device_or_profile_leaves_no_mark() {
        let (_dir, home, folder) = home_and_folder("switch-refused");
        let (settings, exe) = base_and_exe(&home);
        let file = crate::sandbox::marks_file(&home);
        let mut bare = Fake::linux(&home);
        bare.tools.retain(|tool| *tool != "bwrap");
        assert_eq!(
            turn_on(&bare, &file, &folder, &exe, Some(&settings)),
            Err(Refusal::MissingTool("bwrap"))
        );
        assert_eq!(marks::covered(&file, &folder), Ok(false));
        // The profile is refused after the mark was written: taken back.
        let read_dirs = crate::sandbox::sandbox_home(&home).join("read-dirs");
        std::fs::create_dir_all(read_dirs.parent().unwrap()).unwrap();
        std::fs::write(&read_dirs, "relative/dir\n").unwrap();
        assert!(matches!(
            turn_on(&Fake::linux(&home), &file, &folder, &exe, Some(&settings)),
            Err(Refusal::BadReadDir(1))
        ));
        assert_eq!(marks::covered(&file, &folder), Ok(false));
    }

    #[test]
    fn turning_off_removes_only_the_folders_own_mark() {
        let (_dir, home, folder) = home_and_folder("switch-off");
        let file = crate::sandbox::marks_file(&home);
        let fake = Fake::linux(&home);
        let sub = folder.join("sub");
        std::fs::create_dir_all(&sub).unwrap();
        assert_eq!(turn_off(&fake, &file, &folder), Ok(Switched::Off), "none");
        marks::add(&file, &folder).unwrap();
        assert_eq!(turn_off(&fake, &file, &sub), Ok(Switched::Inherited));
        assert_eq!(
            marks::covered(&file, &folder),
            Ok(true),
            "the mark above stays"
        );
        marks::add(&file, &sub).unwrap();
        assert_eq!(turn_off(&fake, &file, &sub), Ok(Switched::Inherited));
        assert_eq!(marks::load(&file).unwrap(), vec![folder.clone()]);
        assert_eq!(turn_off(&fake, &file, &folder), Ok(Switched::Off));
        assert_eq!(marks::covered(&file, &folder), Ok(false));
    }

    #[test]
    fn a_damaged_marks_file_is_refused_and_left_alone() {
        let (_dir, home, folder) = home_and_folder("switch-damaged");
        let (settings, exe) = base_and_exe(&home);
        let file = crate::sandbox::marks_file(&home);
        std::fs::create_dir_all(file.parent().unwrap()).unwrap();
        std::fs::write(&file, b"{broken").unwrap();
        let fake = Fake::linux(&home);
        assert_eq!(
            turn_on(&fake, &file, &folder, &exe, Some(&settings)),
            Err(Refusal::MarksFile)
        );
        assert_eq!(turn_off(&fake, &file, &folder), Err(Refusal::MarksFile));
        assert_eq!(std::fs::read(&file).unwrap(), b"{broken");
        assert!(!file.with_file_name("folders.json.bak").exists());
    }

    #[test]
    fn the_state_tells_wanted_and_inherited() {
        let (_dir, home, folder) = home_and_folder("switch-state");
        let file = crate::sandbox::marks_file(&home);
        let sub = folder.join("sub");
        std::fs::create_dir_all(&sub).unwrap();
        let of = |active, wanted, inherited| SandboxState {
            active,
            wanted,
            inherited,
        };
        assert_eq!(state(None, &folder, true), of(true, false, false));
        assert_eq!(state(Some(&file), &folder, false), of(false, false, false));
        marks::add(&file, &folder).unwrap();
        assert_eq!(state(Some(&file), &folder, false), of(false, true, false));
        assert_eq!(state(Some(&file), &sub, true), of(true, true, true));
        marks::add(&file, &sub).unwrap();
        assert_eq!(state(Some(&file), &sub, false), of(false, true, false));
        std::fs::write(&file, b"{broken").unwrap();
        // The backup still names both: wanted; a damaged file names no
        // folder above.
        assert_eq!(state(Some(&file), &sub, false), of(false, true, false));
        std::fs::remove_file(file.with_file_name("folders.json.bak")).unwrap();
        assert_eq!(state(Some(&file), &sub, true), of(true, true, false));
    }

    #[test]
    fn the_new_refusals_name_no_path() {
        for refusal in [Refusal::MarksFile, Refusal::SandboxWroteProtected(2)] {
            let text = refusal.to_string();
            assert!(!text.contains('/') && !text.contains('\\'), "{text}");
        }
        assert!(
            Refusal::SandboxWroteProtected(2)
                .to_string()
                .contains("(2)")
        );
    }
}
