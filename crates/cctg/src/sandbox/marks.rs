//! Folders in sandbox mode: `<home>/.cctg/sandbox/folders.json`,
//! `{"version":1,"folders":["/abs/canonical/folder", ...]}`.
//!
//! A mark covers its folder and everything inside it: a session started in
//! a subfolder of a marked project is sandboxed too. Callers treat a file
//! that cannot be read as "marked" (fail closed); errors never quote the
//! file.

use std::fmt;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use super::paths;

const VERSION: u32 = 1;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MarksError {
    /// The file exists and cannot be read.
    Unreadable,
    /// Not the JSON of a marks file.
    Unparsable,
    /// A marks file of another version.
    Version,
    /// Writing it failed.
    Write,
}

impl fmt::Display for MarksError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::Unreadable => "the sandbox marks file cannot be read",
            Self::Unparsable => "the sandbox marks file is damaged (contents are not shown)",
            Self::Version => "the sandbox marks file is of another cctg version",
            Self::Write => "the sandbox marks file cannot be written",
        })
    }
}

impl std::error::Error for MarksError {}

#[derive(Serialize, Deserialize)]
struct MarksFile {
    version: u32,
    #[serde(default)]
    folders: Vec<PathBuf>,
}

/// What [`remove`] did.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Removed {
    /// The folder's own mark is gone.
    Exact,
    /// The folder is covered by the mark of this folder above it; nothing
    /// changed.
    Ancestor(PathBuf),
    /// No mark covers the folder.
    None,
}

/// The marked folders; none when the file does not exist.
pub fn load(file: &Path) -> Result<Vec<PathBuf>, MarksError> {
    let bytes = match std::fs::read(file) {
        Ok(bytes) => bytes,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(_) => return Err(MarksError::Unreadable),
    };
    let marks: MarksFile = serde_json::from_slice(&bytes).map_err(|_| MarksError::Unparsable)?;
    if marks.version != VERSION {
        return Err(MarksError::Version);
    }
    Ok(marks.folders)
}

/// Is `folder` (resolved here) marked or inside a marked folder?
pub fn covered(file: &Path, folder: &Path) -> Result<bool, MarksError> {
    let marks = load(file)?;
    Ok(covering(&marks, &canonical(folder)).is_some())
}

/// The mark that covers `folder` (canonical), if any.
pub fn covering<'a>(marks: &'a [PathBuf], folder: &Path) -> Option<&'a Path> {
    marks
        .iter()
        .map(PathBuf::as_path)
        .find(|mark| paths::within(mark, folder))
}

/// Marks `folder` (stored resolved). `Ok(false)`: it already was. A file that
/// cannot be read is left alone.
pub fn add(file: &Path, folder: &Path) -> Result<bool, MarksError> {
    let mut marks = load(file)?;
    let folder = canonical(folder);
    if marks.iter().any(|mark| same(mark, &folder)) {
        return Ok(false);
    }
    marks.push(folder);
    save(file, marks)?;
    Ok(true)
}

/// Removes the mark of `folder` itself; a mark above it stays (the user
/// turns it off there).
pub fn remove(file: &Path, folder: &Path) -> Result<Removed, MarksError> {
    let mut marks = load(file)?;
    let folder = canonical(folder);
    let before = marks.len();
    marks.retain(|mark| !same(mark, &folder));
    if marks.len() != before {
        save(file, marks)?;
        return Ok(Removed::Exact);
    }
    Ok(match covering(&marks, &folder) {
        Some(mark) => Removed::Ancestor(mark.to_path_buf()),
        None => Removed::None,
    })
}

fn same(a: &Path, b: &Path) -> bool {
    paths::within(a, b) && paths::within(b, a)
}

/// The spelling marks are kept in ([`crate::device::canonical_cwd`]).
fn canonical(folder: &Path) -> PathBuf {
    match folder.to_str() {
        Some(text) => PathBuf::from(crate::device::canonical_cwd(text)),
        None => paths::canonical(folder).unwrap_or_else(|| folder.to_path_buf()),
    }
}

fn save(file: &Path, folders: Vec<PathBuf>) -> Result<(), MarksError> {
    let bytes = serde_json::to_vec_pretty(&MarksFile {
        version: VERSION,
        folders,
    })
    .map_err(|_| MarksError::Write)?;
    if let Some(dir) = file.parent() {
        super::create_private_dir(dir).map_err(|_| MarksError::Write)?;
    }
    super::write_if_changed(file, &bytes)
        .map(|_| ())
        .map_err(|_| MarksError::Write)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::hub::testdir::TempDir;

    #[test]
    fn a_mark_covers_its_folder_and_everything_inside() {
        let dir = TempDir::new("marks");
        let file = dir.path().join("home").join("folders.json");
        let proj = paths::canonical(dir.path()).unwrap().join("proj");
        let sub = proj.join("sub");
        let other = paths::canonical(dir.path()).unwrap().join("proj2");
        std::fs::create_dir_all(&sub).unwrap();
        std::fs::create_dir_all(&other).unwrap();

        assert_eq!(covered(&file, &proj), Ok(false), "no file: nothing marked");
        assert_eq!(add(&file, &proj), Ok(true));
        assert_eq!(add(&file, &proj), Ok(false));
        assert_eq!(covered(&file, &proj), Ok(true));
        assert_eq!(covered(&file, &sub), Ok(true));
        assert_eq!(
            covered(&file, &other),
            Ok(false),
            "a name prefix is not a parent"
        );
        assert_eq!(
            covered(&file, &sub.join("..").join("..").join("proj2")),
            Ok(false)
        );
        assert_eq!(load(&file).unwrap(), vec![proj.clone()]);

        assert_eq!(remove(&file, &sub), Ok(Removed::Ancestor(proj.clone())));
        assert_eq!(remove(&file, &other), Ok(Removed::None));
        assert_eq!(remove(&file, &proj), Ok(Removed::Exact));
        assert_eq!(covered(&file, &sub), Ok(false));

        let names: Vec<_> = std::fs::read_dir(file.parent().unwrap())
            .unwrap()
            .map(|entry| entry.unwrap().file_name())
            .collect();
        assert_eq!(names, vec![std::ffi::OsString::from("folders.json")]);
    }

    #[test]
    fn a_broken_file_is_an_error_and_is_left_alone() {
        let dir = TempDir::new("marks-broken");
        let file = dir.path().join("folders.json");
        let proj = dir.path().join("proj");
        std::fs::create_dir_all(&proj).unwrap();
        std::fs::write(&file, b"{\"version\":1,\"folders\":[\"/secret/path\"").unwrap();
        assert_eq!(load(&file), Err(MarksError::Unparsable));
        assert_eq!(covered(&file, &proj), Err(MarksError::Unparsable));
        assert_eq!(add(&file, &proj), Err(MarksError::Unparsable));
        assert!(
            std::fs::read(&file).unwrap().ends_with(b"path\""),
            "untouched"
        );
        assert!(!MarksError::Unparsable.to_string().contains("secret"));

        std::fs::write(&file, b"{\"version\":2,\"folders\":[]}").unwrap();
        assert_eq!(load(&file), Err(MarksError::Version));
        std::fs::write(&file, b"{\"folders\":[]}").unwrap();
        assert_eq!(load(&file), Err(MarksError::Unparsable), "no version");
        std::fs::write(&file, b"{\"version\":1}").unwrap();
        assert_eq!(load(&file), Ok(Vec::new()));
    }

    #[cfg(unix)]
    #[test]
    fn the_marks_file_is_private() {
        use std::os::unix::fs::PermissionsExt;
        let dir = TempDir::new("marks-mode");
        let file = dir.path().join("sandbox").join("folders.json");
        add(&file, dir.path()).unwrap();
        let mode = |path: &Path| std::fs::metadata(path).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode(&file), 0o600);
        assert_eq!(mode(file.parent().unwrap()), 0o700);
    }
}
