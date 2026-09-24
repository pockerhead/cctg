//! Which cctg build this process runs (TASK-040).
//!
//! The build is the sha256 of the executable file, as lowercase hex: two
//! processes run the same build exactly when their files had the same bytes.
//! `CARGO_PKG_VERSION` stays `0.1.0` across builds, so the hub compares
//! builds, not versions; the version is only shown and logged.

use std::io::Read;
use std::path::Path;

pub const VERSION: &str = env!("CARGO_PKG_VERSION");

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

/// The build of this process's executable, `None` when it cannot be read.
pub fn own_build() -> Option<String> {
    let exe = std::env::current_exe().ok()?;
    build_of(&exe).ok()
}

/// The first 8 hex digits, for logs and messages.
pub fn short(build: &str) -> &str {
    build.get(..8).unwrap_or(build)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::hub::testdir::TempDir;

    #[test]
    fn a_build_is_the_sha256_of_the_file() {
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
        assert!(own_build().is_some_and(|build| build.len() == 64));
    }
}
