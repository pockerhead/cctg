//! The `getUpdates` offset on disk, so a restarted hub does not handle an
//! update again.
//!
//! Telegram confirms an update only when `getUpdates` is next called with a
//! higher offset. Without this file, updates handled just before a crash or
//! restart come back on the first call after it.

use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime};

use tracing::warn;

const FILE_NAME: &str = "offset";
const TEMP_NAME: &str = "offset.tmp";
/// Telegram keeps an update at most 24 hours, so an offset saved earlier
/// guards nothing. It can hurt: after a week without updates Telegram restarts
/// ids at random, possibly below it.
const MAX_AGE: Duration = Duration::from_secs(24 * 60 * 60);

#[derive(Debug, Clone)]
pub struct OffsetStore {
    dir: PathBuf,
}

impl OffsetStore {
    /// Uses `dir`, creating it when missing.
    pub fn open(dir: &Path) -> io::Result<Self> {
        std::fs::create_dir_all(dir)?;
        Ok(Self {
            dir: dir.to_owned(),
        })
    }

    /// The saved offset. A missing file is `None`; an unreadable or garbled one
    /// is logged and also `None`, so the hub still starts. So is one saved
    /// more than `MAX_AGE` ago.
    pub fn load(&self) -> Option<i64> {
        let path = self.dir.join(FILE_NAME);
        let age = std::fs::metadata(&path)
            .and_then(|metadata| metadata.modified())
            .ok()
            .and_then(|modified| SystemTime::now().duration_since(modified).ok());
        if age.is_some_and(|age| age > MAX_AGE) {
            warn!("saved getUpdates offset is older than a day; starting without it");
            return None;
        }
        match std::fs::read_to_string(&path) {
            Ok(text) => {
                let offset = text.trim().parse().ok();
                if offset.is_none() {
                    warn!("saved getUpdates offset is not a number; starting without it");
                }
                offset
            }
            Err(error) if error.kind() == io::ErrorKind::NotFound => None,
            Err(error) => {
                warn!(kind = ?error.kind(), "cannot read the saved getUpdates offset; starting without it");
                None
            }
        }
    }

    /// Writes a temp file, flushes it to disk and renames it over the old one,
    /// so an interrupted save leaves the previous value.
    pub fn save(&self, offset: i64) -> io::Result<()> {
        let temp = self.dir.join(TEMP_NAME);
        let mut file = std::fs::File::create(&temp)?;
        writeln!(file, "{offset}")?;
        file.sync_all()?;
        drop(file);
        std::fs::rename(&temp, self.dir.join(FILE_NAME))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::hub::testdir::TempDir;

    #[test]
    fn saves_and_reloads_across_instances() {
        let dir = TempDir::new("offset-roundtrip");
        let state = dir.path().join("state");
        let store = OffsetStore::open(&state).unwrap();
        assert_eq!(store.load(), None);
        store.save(41).unwrap();
        store.save(42).unwrap();
        assert_eq!(OffsetStore::open(&state).unwrap().load(), Some(42));
        assert!(!state.join(TEMP_NAME).exists());
    }

    #[test]
    fn garbage_or_leftover_temp_file_does_not_break_start() {
        let dir = TempDir::new("offset-garbage");
        let store = OffsetStore::open(dir.path()).unwrap();
        std::fs::write(dir.path().join(FILE_NAME), "not a number").unwrap();
        assert_eq!(store.load(), None);

        // A crash between write and rename leaves only the temp file behind.
        store.save(7).unwrap();
        std::fs::write(dir.path().join(TEMP_NAME), "99").unwrap();
        assert_eq!(store.load(), Some(7));
        store.save(8).unwrap();
        assert_eq!(store.load(), Some(8));
    }

    #[test]
    fn an_offset_older_than_a_day_is_ignored() {
        let dir = TempDir::new("offset-stale");
        let store = OffsetStore::open(dir.path()).unwrap();
        store.save(9).unwrap();
        let file = std::fs::File::options()
            .write(true)
            .open(dir.path().join(FILE_NAME))
            .unwrap();
        file.set_modified(SystemTime::now() - MAX_AGE + Duration::from_secs(60))
            .unwrap();
        assert_eq!(store.load(), Some(9));
        file.set_modified(SystemTime::now() - MAX_AGE - Duration::from_secs(60))
            .unwrap();
        assert_eq!(store.load(), None);
    }
}
