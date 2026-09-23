//! The `getUpdates` offset on disk, so a restarted hub does not handle an
//! update again.
//!
//! Telegram confirms an update only when `getUpdates` is next called with a
//! higher offset. Without this file, updates handled just before a crash or
//! restart come back on the first call after it.

use std::io::{self, Write};
use std::path::{Path, PathBuf};

use tracing::warn;

const FILE_NAME: &str = "offset";
const TEMP_NAME: &str = "offset.tmp";

#[derive(Debug)]
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
    /// is logged and also `None`, so the hub still starts.
    pub fn load(&self) -> Option<i64> {
        match std::fs::read_to_string(self.dir.join(FILE_NAME)) {
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
}
