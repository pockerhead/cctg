//! Self-removing temporary directory for unit tests.
//!
//! The slots actor and its registry saver run for the life of the hub, so in
//! a test they outlive the test body: a save can still be renaming
//! `registry.json` into the directory while `Drop` removes it, and
//! `remove_dir_all` then fails on a directory that is not empty (TASK-018
//! found such leftovers after ordinary runs). `Drop` retries until the
//! directory is gone; after that nothing can recreate it, because
//! `RegistryStore::save` never creates directories. A test process that was
//! killed never drops its directories: the first `TempDir` of a later run
//! removes `cctg-test-*` directories older than [`STALE`].

use std::path::{Path, PathBuf};
use std::sync::Once;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;

const PREFIX: &str = "cctg-test-";
/// No test runs this long; an older directory belongs to a dead run.
const STALE: Duration = Duration::from_secs(60 * 60);

pub struct TempDir(PathBuf);

impl TempDir {
    pub fn new(name: &str) -> Self {
        static NEXT: AtomicUsize = AtomicUsize::new(0);
        static SWEEP: Once = Once::new();
        SWEEP.call_once(sweep_stale);
        let path = std::env::temp_dir().join(format!(
            "{PREFIX}{}-{}-{name}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        let _ = std::fs::remove_dir_all(&path);
        std::fs::create_dir_all(&path).expect("create temp dir");
        Self(path)
    }

    pub fn path(&self) -> &Path {
        &self.0
    }
}

impl Drop for TempDir {
    fn drop(&mut self) {
        for _ in 0..50 {
            let _ = std::fs::remove_dir_all(&self.0);
            if !self.0.exists() {
                return;
            }
            std::thread::sleep(Duration::from_millis(20));
        }
    }
}

fn sweep_stale() {
    let Ok(entries) = std::fs::read_dir(std::env::temp_dir()) else {
        return;
    };
    for entry in entries.filter_map(Result::ok) {
        let ours = entry
            .file_name()
            .to_str()
            .is_some_and(|name| name.starts_with(PREFIX));
        let stale = entry
            .metadata()
            .and_then(|meta| meta.modified())
            .ok()
            .and_then(|modified| modified.elapsed().ok())
            .is_some_and(|age| age > STALE);
        if ours && stale {
            let _ = std::fs::remove_dir_all(entry.path());
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A registry save in flight when the test body ends: its `.tmp` cannot be
    /// deleted while open (no `FILE_SHARE_DELETE`), then it is renamed to
    /// `registry.json`. This is the leak TASK-018 found; nothing may be left.
    #[cfg(windows)]
    #[test]
    fn a_save_in_flight_during_the_drop_leaves_no_directory() {
        use std::os::windows::fs::OpenOptionsExt;
        const SHARE_READ_WRITE: u32 = 0x1 | 0x2;
        let dir = TempDir::new("testdir-race");
        let path = dir.path().to_owned();
        let temp = path.join("registry.json.tmp");
        let file = std::fs::OpenOptions::new()
            .write(true)
            .create(true)
            .share_mode(SHARE_READ_WRITE)
            .open(&temp)
            .unwrap();
        let saver = std::thread::spawn(move || {
            std::thread::sleep(Duration::from_millis(100));
            drop(file);
            let _ = std::fs::rename(&temp, temp.with_file_name("registry.json"));
        });
        drop(dir);
        saver.join().unwrap();
        assert!(!path.exists(), "{} left behind", path.display());
    }
}
