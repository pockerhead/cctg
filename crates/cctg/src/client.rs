//! Which cctg build this process runs (TASK-040, TASK-035).
//!
//! The build is what the hub compares to call an agent outdated. It is the
//! source the binary was built from, so a hub in a Linux container and a
//! Windows client of one commit run the same build:
//! - the commit given to the build (`CCTG_BUILD_ID`, CI and Docker) or the
//!   git commit of a clean checkout: that value as it is;
//! - a checkout with local changes: `<commit>-dirty.<sha256 of the
//!   executable>`, since two such builds of one commit can differ;
//! - no git at all: the sha256 of the executable, lowercase hex.
//!
//! [`build_of`] (the file hash) stays what the worker agent compares with
//! the file on disk to find a newer binary ([`crate::update`]).
//! `CARGO_PKG_VERSION` stays `0.1.0` across builds; it is only shown.

use std::io::Read;
use std::path::Path;

pub const VERSION: &str = env!("CARGO_PKG_VERSION");
/// Baked in by `build.rs`: the commit, `<commit>-dirty`, or empty.
pub const SOURCE: &str = env!("CCTG_SOURCE");
/// What `cctg --version` prints after the name.
pub const LONG_VERSION: &str = concat!(env!("CARGO_PKG_VERSION"), " (", env!("CCTG_SOURCE"), ")");

/// sha256 of the file at `path`, lowercase hex. Reads it in 64 KiB pieces.
pub fn build_of(path: &Path) -> std::io::Result<String> {
    let mut file = std::fs::File::open(path)?;
    let mut context = aws_lc_rs::digest::Context::new(&aws_lc_rs::digest::SHA256);
    let mut buf = vec![0u8; 64 << 10];
    loop {
        let read = match file.read(&mut buf) {
            Ok(0) => break,
            Ok(read) => read,
            Err(error) if error.kind() == std::io::ErrorKind::Interrupted => continue,
            Err(error) => return Err(error),
        };
        context.update(&buf[..read]);
    }
    Ok(context
        .finish()
        .as_ref()
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect())
}

/// The build from the baked `source` and the executable's hash, which is
/// asked for only when `source` alone does not name the build.
pub fn identity(source: &str, file_hash: impl FnOnce() -> Option<String>) -> Option<String> {
    if source.is_empty() {
        file_hash()
    } else if source.ends_with("-dirty") {
        file_hash().map(|hash| format!("{source}.{hash}"))
    } else {
        Some(source.to_owned())
    }
}

/// The build of the executable at `path` as its own process reports it
/// (built from this same source). `None` when a hash is needed and the
/// file cannot be read.
pub fn build_id_of(path: &Path) -> Option<String> {
    identity(SOURCE, || build_of(path).ok())
}

/// The build of this process, `None` when it needs the executable's hash
/// and the file cannot be read.
pub fn own_build() -> Option<String> {
    identity(SOURCE, || {
        let exe = std::env::current_exe().ok()?;
        build_of(&exe).ok()
    })
}

/// The first 8 characters, plus `-dirty` and the first 4 of the file hash
/// for a build with local changes (two such builds of one commit differ),
/// for logs and messages.
pub fn short(build: &str) -> String {
    let head = build.get(..8).unwrap_or(build);
    match build.split_once("-dirty") {
        Some((_, rest)) => {
            let hash: String = rest
                .strip_prefix('.')
                .unwrap_or_default()
                .chars()
                .take(4)
                .collect();
            if hash.is_empty() {
                format!("{head}-dirty")
            } else {
                format!("{head}-dirty.{hash}")
            }
        }
        None => head.to_owned(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::hub::testdir::TempDir;

    const COMMIT: &str = "0123456789abcdef0123456789abcdef01234567";

    #[test]
    fn a_file_hash_is_the_sha256_of_the_file() {
        let dir = TempDir::new("client-build");
        let path = dir.path().join("x.bin");
        std::fs::write(&path, b"abc").unwrap();
        assert_eq!(
            build_of(&path).unwrap(),
            "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
        );
        std::fs::write(&path, vec![7u8; 200_000]).unwrap();
        let big = build_of(&path).unwrap();
        assert_eq!(big.len(), 64);
        assert_eq!(
            build_of(&path).unwrap(),
            big,
            "the same bytes, the same build"
        );
        assert!(build_of(&dir.path().join("missing")).is_err());
        assert_eq!(short(&big), &big[..8]);
        assert_eq!(short("abc"), "abc");
        assert!(own_build().is_some());
    }

    #[test]
    fn one_commit_is_one_build_whatever_the_file() {
        // A Linux hub and a Windows client of one commit: different files,
        // one build; the hash is never even asked for.
        let linux = identity(COMMIT, || panic!("no hash for a clean commit"));
        let windows = identity(COMMIT, || panic!("no hash for a clean commit"));
        assert_eq!(linux.as_deref(), Some(COMMIT));
        assert_eq!(linux, windows);
        assert_ne!(identity("fedcba98", || None), linux, "another commit");
        assert_eq!(short(COMMIT), "01234567");
    }

    #[test]
    fn local_changes_and_no_git_fall_back_to_the_file() {
        let dirty = format!("{COMMIT}-dirty");
        let a = identity(&dirty, || Some("aa".repeat(32))).unwrap();
        let b = identity(&dirty, || Some("bb".repeat(32))).unwrap();
        assert_ne!(a, b, "two dirty builds of one commit differ");
        assert!(a.starts_with(&dirty));
        assert_eq!(short(&a), "01234567-dirty.aaaa");
        assert_eq!(short(&b), "01234567-dirty.bbbb", "shown apart too");
        assert_eq!(short(&dirty), "01234567-dirty");
        assert_eq!(identity(&dirty, || None), None);
        assert_eq!(
            identity("", || Some("cc".repeat(32))),
            Some("cc".repeat(32))
        );
        assert_eq!(identity("", || None), None);
    }

    #[test]
    fn the_baked_source_is_plain_text() {
        assert!(
            SOURCE
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'-' | b'_')),
            "{SOURCE}"
        );
        assert!(SOURCE.len() <= 64 + "-dirty".len());
        assert!(LONG_VERSION.starts_with(VERSION));
    }
}
