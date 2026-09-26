//! Bakes the build's source identity into `CCTG_SOURCE` (TASK-035), read by
//! `cctg::client`:
//! - `CCTG_BUILD_ID` of the build environment when set (CI and the Docker
//!   build pass the commit, so the hub image and the client binaries of one
//!   commit carry the same value);
//! - else the git commit of this checkout, with `-dirty` when a file under
//!   `crates/`, `Cargo.toml` or `Cargo.lock` differs from it;
//! - else empty (no git, no variable): the executable's hash stands in.
//!
//! Never fails the build for want of git.
//!
//! Also bakes `CCTG_RELEASE` (TASK-050): the release tag CI and the Docker
//! build pass for a tag build, empty otherwise. A hub with a tag offers its
//! clients that release's binary on ⬆️ Обновить.

use std::path::{Path, PathBuf};
use std::process::Command;

const ID_VAR: &str = "CCTG_BUILD_ID";
const RELEASE_VAR: &str = "CCTG_RELEASE";

fn main() {
    println!("cargo::rerun-if-env-changed={ID_VAR}");
    println!("cargo::rerun-if-env-changed={RELEASE_VAR}");
    println!("cargo::rerun-if-changed=build.rs");
    let manifest = PathBuf::from(std::env::var_os("CARGO_MANIFEST_DIR").unwrap_or_default());
    let source = match std::env::var(ID_VAR) {
        Ok(id) if !id.trim().is_empty() => checked(ID_VAR, id.trim()),
        _ => from_git(&manifest).unwrap_or_default(),
    };
    println!("cargo::rustc-env=CCTG_SOURCE={source}");
    let release = match std::env::var(RELEASE_VAR) {
        Ok(tag) if !tag.trim().is_empty() => checked(RELEASE_VAR, tag.trim()),
        _ => String::new(),
    };
    println!("cargo::rustc-env=CCTG_RELEASE={release}");
}

/// A given id or tag goes into logs, the agent link, Telegram texts and
/// download URLs: short and plain, or the build stops.
fn checked(var: &str, id: &str) -> String {
    let plain = id
        .bytes()
        .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'-' | b'_'));
    assert!(
        plain && id.len() <= 64,
        "{var} must be 1-64 characters of [A-Za-z0-9._-]"
    );
    id.to_owned()
}

fn git(manifest: &Path, args: &[&str]) -> Option<String> {
    let output = Command::new("git")
        .current_dir(manifest)
        .args(args)
        .output()
        .ok()?;
    output
        .status
        .success()
        .then(|| String::from_utf8_lossy(&output.stdout).trim().to_owned())
}

fn from_git(manifest: &Path) -> Option<String> {
    let commit = git(manifest, &["rev-parse", "HEAD"])?;
    if commit.len() != 40 || !commit.bytes().all(|byte| byte.is_ascii_hexdigit()) {
        return None;
    }
    watch_git(manifest);
    let status = git(
        manifest,
        &[
            "status",
            "--porcelain",
            "--untracked-files=normal",
            "--",
            ":/crates",
            ":/Cargo.toml",
            ":/Cargo.lock",
        ],
    )?;
    Some(if status.is_empty() {
        commit
    } else {
        format!("{commit}-dirty")
    })
}

/// Runs again when HEAD moves or a source changes. Only existing paths: a
/// missing one would make cargo run this script on every build.
fn watch_git(manifest: &Path) {
    let git_path = |name: &str| {
        git(
            manifest,
            &["rev-parse", "--path-format=absolute", "--git-path", name],
        )
        .map(PathBuf::from)
    };
    let mut watched: Vec<PathBuf> = ["HEAD", "packed-refs"]
        .iter()
        .filter_map(|name| git_path(name))
        .collect();
    if let Some(head_ref) = git(manifest, &["symbolic-ref", "-q", "HEAD"]) {
        watched.extend(git_path(&head_ref));
    }
    let root = manifest.join("..").join("..");
    watched.extend([
        manifest.join("src"),
        manifest.join("Cargo.toml"),
        root.join("crates").join("transcript").join("src"),
        root.join("crates").join("transcript").join("Cargo.toml"),
        root.join("Cargo.toml"),
        root.join("Cargo.lock"),
    ]);
    for path in watched.into_iter().filter(|path| path.exists()) {
        println!("cargo::rerun-if-changed={}", path.display());
    }
}
