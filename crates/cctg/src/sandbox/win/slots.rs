//! The slot map `~/.cctg/sandbox/win/slots.json`: which sandbox account
//! belongs to which marked folder, and the program-directory read allowlist.
//! Only the real user reads or writes it (its directory carries a DENY for the
//! `cctg-sandbox` group). Every read-modify-write holds an exclusive lock on
//! `slots.lock` so two `cctg sandbox on` runs never take the same slot.
//!
//! One account per marked folder (a slot). A slot moves to another folder only
//! after the account is recreated with a new SID (`sandbox-install`), so a
//! forgotten ACE of the old folder never grants the new owner access.

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::sandbox::preflight::Refusal;
use crate::sandbox::{create_private_dir, write_if_changed};

const VERSION: u32 = 1;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub enum State {
    #[serde(rename = "active")]
    Active,
    #[serde(rename = "retired")]
    Retired,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct Slot {
    slot: u32,
    state: State,
    /// The mark root this slot serves, canonical.
    folder: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct SlotsFile {
    version: u32,
    #[serde(default)]
    slots: Vec<Slot>,
    #[serde(default)]
    read_dirs: Vec<String>,
}

impl Default for SlotsFile {
    fn default() -> Self {
        Self {
            version: VERSION,
            slots: Vec::new(),
            read_dirs: Vec::new(),
        }
    }
}

fn file(dir: &Path) -> PathBuf {
    dir.join("slots.json")
}

fn lockfile(dir: &Path) -> PathBuf {
    dir.join("slots.lock")
}

/// Same folder, without case (canonical spellings from one source).
fn same(a: &str, b: &str) -> bool {
    a.eq_ignore_ascii_case(b) || a.to_lowercase() == b.to_lowercase()
}

/// Read-modify-write `slots.json` under an exclusive lock. `f` sees the parsed
/// file and returns its result plus whether the file changed; a changed file
/// is written back. A missing file is an empty map; a corrupt one fails
/// closed with [`Refusal::Io`].
fn with_lock<T>(
    dir: &Path,
    f: impl FnOnce(&mut SlotsFile) -> Result<T, Refusal>,
) -> Result<T, Refusal> {
    create_private_dir(dir).map_err(|_| Refusal::Io("~/.cctg/sandbox/win"))?;
    let lock = std::fs::OpenOptions::new()
        .create(true)
        .write(true)
        .truncate(false)
        .open(lockfile(dir))
        .map_err(|_| Refusal::Io("~/.cctg/sandbox/win/slots.lock"))?;
    lock.lock()
        .map_err(|_| Refusal::Io("~/.cctg/sandbox/win/slots.lock"))?;
    let result = (|| {
        let mut model = match std::fs::read(file(dir)) {
            Ok(bytes) => serde_json::from_slice::<SlotsFile>(&bytes)
                .map_err(|_| Refusal::Io("slots.json"))?,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => SlotsFile::default(),
            Err(_) => return Err(Refusal::Io("slots.json")),
        };
        if model.version != VERSION {
            return Err(Refusal::Io("slots.json"));
        }
        let before = serde_json::to_vec(&model).ok();
        let value = f(&mut model)?;
        let after = serde_json::to_vec_pretty(&model).map_err(|_| Refusal::Io("slots.json"))?;
        if before.as_deref() != Some(after.as_slice()) {
            write_if_changed(&file(dir), &after).map_err(|_| Refusal::Io("slots.json"))?;
        }
        Ok(value)
    })();
    let _ = lock.unlock();
    result
}

/// The slot serving `folder`: the active one if any, else a retired one of the
/// same folder revived, else the first fresh number in `1..=max`. Fresh means
/// no entry names it. `Refusal::NoFreeSlot(max)` when every number is taken by
/// another folder.
pub fn take(dir: &Path, max: u32, folder: &str) -> Result<u32, Refusal> {
    with_lock(dir, |model| {
        if let Some(entry) = model.slots.iter_mut().find(|s| same(&s.folder, folder)) {
            entry.state = State::Active;
            return Ok(entry.slot);
        }
        let used: Vec<u32> = model.slots.iter().map(|s| s.slot).collect();
        let fresh = (1..=max).find(|k| !used.contains(k));
        match fresh {
            Some(k) => {
                model.slots.push(Slot {
                    slot: k,
                    state: State::Active,
                    folder: folder.to_owned(),
                });
                Ok(k)
            }
            None => Err(Refusal::NoFreeSlot(max)),
        }
    })
}

/// Marks `folder`'s slot retired (kept in the map so its number is not reused
/// until the account is recreated). No-op when the folder has no slot.
pub fn retire(dir: &Path, folder: &str) -> Result<(), Refusal> {
    with_lock(dir, |model| {
        if let Some(entry) = model.slots.iter_mut().find(|s| same(&s.folder, folder)) {
            entry.state = State::Retired;
        }
        Ok(())
    })
}

/// The active slot number of `folder`, if any.
pub fn active_slot(dir: &Path, folder: &str) -> Result<Option<u32>, Refusal> {
    with_lock(dir, |model| {
        Ok(model
            .slots
            .iter()
            .find(|s| same(&s.folder, folder) && s.state == State::Active)
            .map(|s| s.slot))
    })
}

/// Removes `folder`'s slot entry entirely (used by uninstall bookkeeping and
/// after an account is recreated).
pub fn forget(dir: &Path, folder: &str) -> Result<(), Refusal> {
    with_lock(dir, |model| {
        model.slots.retain(|s| !same(&s.folder, folder));
        Ok(())
    })
}

/// A summary for `cctg doctor`: `(active, retired, fresh)` counts against
/// `max`.
pub fn counts(dir: &Path, max: u32) -> Result<(u32, u32, u32), Refusal> {
    with_lock(dir, |model| {
        let active = model
            .slots
            .iter()
            .filter(|s| s.state == State::Active)
            .count() as u32;
        let retired = model
            .slots
            .iter()
            .filter(|s| s.state == State::Retired)
            .count() as u32;
        let used = model.slots.len() as u32;
        Ok((active, retired, max.saturating_sub(used)))
    })
}

/// The recorded read allowlist directories.
pub fn read_dirs(dir: &Path) -> Result<Vec<String>, Refusal> {
    with_lock(dir, |model| Ok(model.read_dirs.clone()))
}

/// Replaces the recorded read allowlist.
pub fn set_read_dirs(dir: &Path, dirs: &[String]) -> Result<(), Refusal> {
    with_lock(dir, |model| {
        model.read_dirs = dirs.to_vec();
        Ok(())
    })
}

/// Every folder that still holds a slot (active or retired), for uninstall.
pub fn folders(dir: &Path) -> Result<Vec<(u32, String, bool)>, Refusal> {
    with_lock(dir, |model| {
        Ok(model
            .slots
            .iter()
            .map(|s| (s.slot, s.folder.clone(), s.state == State::Active))
            .collect())
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::hub::testdir::TempDir;

    #[test]
    fn take_reactivate_retire_and_exhaust() {
        let tmp = TempDir::new("slots");
        let dir = tmp.path().join("win");
        assert_eq!(take(&dir, 2, "C:/a").unwrap(), 1);
        assert_eq!(take(&dir, 2, "C:/a").unwrap(), 1, "same folder, same slot");
        assert_eq!(take(&dir, 2, "C:/b").unwrap(), 2);
        assert_eq!(active_slot(&dir, "C:/a").unwrap(), Some(1));
        assert_eq!(active_slot(&dir, "c:/A").unwrap(), Some(1), "case-fold");
        assert_eq!(take(&dir, 2, "C:/c"), Err(Refusal::NoFreeSlot(2)));
        retire(&dir, "C:/a").unwrap();
        assert_eq!(active_slot(&dir, "C:/a").unwrap(), None);
        assert_eq!(take(&dir, 2, "C:/a").unwrap(), 1, "retired revives");
        // A retired slot still holds its number.
        retire(&dir, "C:/b").unwrap();
        assert_eq!(take(&dir, 2, "C:/c"), Err(Refusal::NoFreeSlot(2)));
        forget(&dir, "C:/b").unwrap();
        assert_eq!(
            take(&dir, 2, "C:/c").unwrap(),
            2,
            "forgotten frees the number"
        );
    }

    #[test]
    fn a_corrupt_file_fails_closed() {
        let tmp = TempDir::new("slots-corrupt");
        let dir = tmp.path().join("win");
        create_private_dir(&dir).unwrap();
        std::fs::write(file(&dir), b"{not json").unwrap();
        assert_eq!(take(&dir, 8, "C:/a"), Err(Refusal::Io("slots.json")));
        assert_eq!(active_slot(&dir, "C:/a"), Err(Refusal::Io("slots.json")));
    }

    #[test]
    fn read_dirs_round_trip() {
        let tmp = TempDir::new("slots-readdirs");
        let dir = tmp.path().join("win");
        assert!(read_dirs(&dir).unwrap().is_empty());
        set_read_dirs(&dir, &["C:/tools".to_owned()]).unwrap();
        assert_eq!(read_dirs(&dir).unwrap(), vec!["C:/tools".to_owned()]);
    }
}
