//! Folders in sandbox mode: `<home>/.cctg/sandbox/folders.json`,
//! `{"version":1,"folders":["/abs/canonical/folder", ...]}`.
//!
//! A mark covers its folder and everything inside it: a session started in
//! a subfolder of a marked project is sandboxed too. Errors never quote the
//! file.
//!
//! A missing or blank file marks nothing. Every save also writes
//! `folders.json.bak`. A file that is there but damaged (review finding 4)
//! must not change folders it never named, and must never unmark one it
//! did, so [`covered`] decides a folder this way:
//! - marked when a whole path in the damaged text, or the backup, covers it;
//! - unmarked when neither does and nothing may be missing: the text ends
//!   as a JSON object ends (`}`) or the backup can be read;
//! - undecided (`Err`: callers fail closed) when the text is cut off with no
//!   backup, when the cut falls inside a path this folder starts with, and
//!   when the file cannot be read at all.

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

/// The marked folders; none when the file does not exist or is blank.
pub fn load(file: &Path) -> Result<Vec<PathBuf>, MarksError> {
    match std::fs::read(file) {
        Ok(bytes) => parse(&bytes),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(Vec::new()),
        Err(_) => Err(MarksError::Unreadable),
    }
}

fn parse(bytes: &[u8]) -> Result<Vec<PathBuf>, MarksError> {
    if bytes.iter().all(u8::is_ascii_whitespace) {
        return Ok(Vec::new());
    }
    let marks: MarksFile = serde_json::from_slice(bytes).map_err(|_| MarksError::Unparsable)?;
    if marks.version != VERSION {
        return Err(MarksError::Version);
    }
    Ok(marks.folders)
}

/// `folders.json.bak`: the last save, again.
fn backup_of(file: &Path) -> PathBuf {
    let name = file
        .file_name()
        .map(|name| name.to_string_lossy().into_owned())
        .unwrap_or_default();
    file.with_file_name(format!("{name}.bak"))
}

/// Is `folder` (resolved here) marked or inside a marked folder? A damaged
/// file is decided as the module docs say.
pub fn covered(file: &Path, folder: &Path) -> Result<bool, MarksError> {
    let folder = canonical(folder);
    let bytes = match std::fs::read(file) {
        Ok(bytes) => bytes,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(false),
        Err(_) => return Err(MarksError::Unreadable),
    };
    match parse(&bytes) {
        Ok(marks) => Ok(covering(&marks, &folder).is_some()),
        Err(error) => damaged_covers(file, &bytes, &folder).ok_or(error),
    }
}

/// The file is there and cannot be used as it is (unreadable, damaged, of
/// another version).
pub fn damaged(file: &Path) -> bool {
    match std::fs::read(file) {
        Ok(bytes) => parse(&bytes).is_err(),
        Err(error) => error.kind() != std::io::ErrorKind::NotFound,
    }
}

/// [`covered`] for a damaged `bytes`; `None`: cannot be told.
fn damaged_covers(file: &Path, bytes: &[u8], folder: &Path) -> Option<bool> {
    let text = String::from_utf8_lossy(bytes);
    let (whole_strings, cut_string) = strings_in(&text);
    let named = whole_strings.iter().any(|named| {
        let named = Path::new(named);
        named.is_absolute() && paths::within(named, folder)
    });
    let backup = std::fs::read(backup_of(file))
        .ok()
        .and_then(|bytes| parse(&bytes).ok());
    let in_backup = backup
        .as_deref()
        .is_some_and(|marks| covering(marks, folder).is_some());
    if named || in_backup {
        return Some(true);
    }
    let folder_text = folder.to_string_lossy();
    let fold = |text: &str| {
        if cfg!(any(windows, target_os = "macos")) {
            text.to_lowercase()
        } else {
            text.to_owned()
        }
    };
    if cut_string.is_some_and(|cut| fold(&folder_text).starts_with(&fold(&cut))) {
        return None;
    }
    let ends_whole = text.trim_end().ends_with('}');
    (ends_whole || backup.is_some()).then_some(false)
}

/// The JSON strings of `text` (escapes undone), and the last one when the
/// text ends inside it.
fn strings_in(text: &str) -> (Vec<String>, Option<String>) {
    let mut whole = Vec::new();
    let mut open: Option<String> = None;
    let mut chars = text.chars();
    while let Some(c) = chars.next() {
        match open.as_mut() {
            None if c == '"' => open = Some(String::new()),
            None => {}
            Some(_) if c == '"' => whole.extend(open.take()),
            Some(string) if c == '\\' => {
                if let Some(escaped) = chars.next() {
                    string.push(match escaped {
                        'n' => '\n',
                        't' => '\t',
                        other => other,
                    });
                }
            }
            Some(string) => string.push(c),
        }
    }
    (whole, open)
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
        .and_then(|_| super::write_if_changed(&backup_of(file), &bytes))
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

        let mut names: Vec<_> = std::fs::read_dir(file.parent().unwrap())
            .unwrap()
            .map(|entry| entry.unwrap().file_name())
            .collect();
        names.sort();
        assert_eq!(
            names,
            ["folders.json", "folders.json.bak"].map(std::ffi::OsString::from)
        );
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

    /// Review finding 4: an empty file is no marks; a damaged one still
    /// marks every folder it names and leaves the others unmarked; only a
    /// cut-off file with no backup, or one that cannot be read, marks all.
    #[test]
    fn a_damaged_file_marks_only_what_it_names() {
        let dir = TempDir::new("marks-damaged");
        let base = paths::canonical(dir.path()).unwrap();
        let file = base.join("sandbox").join("folders.json");
        let proj = base.join("proj");
        let other = base.join("other");
        std::fs::create_dir_all(proj.join("sub")).unwrap();
        std::fs::create_dir_all(&other).unwrap();
        std::fs::create_dir_all(file.parent().unwrap()).unwrap();
        std::fs::write(&file, b"  \n").unwrap();
        assert_eq!(load(&file), Ok(Vec::new()), "blank: no marks");
        assert_eq!(covered(&file, &proj), Ok(false));
        let named = serde_json::to_string(&proj).unwrap();
        // Valid JSON is damaged by a trailing comma.
        std::fs::write(&file, format!("{{\"version\":1,\"folders\":[{named},],}}")).unwrap();
        assert_eq!(covered(&file, &proj), Ok(true));
        assert_eq!(covered(&file, &proj.join("sub")), Ok(true));
        assert_eq!(covered(&file, &other), Ok(false), "not named: as before");
        assert!(damaged(&file));
        // Cut off: what came after the cut is unknown.
        std::fs::write(&file, format!("{{\"version\":1,\"folders\":[{named}")).unwrap();
        assert_eq!(covered(&file, &proj), Ok(true));
        assert!(
            covered(&file, &other).is_err(),
            "anything may have followed"
        );
        // Cut off inside the one path: a folder it may have named.
        let cut = &named[..named.len() - 3];
        std::fs::write(&file, format!("{{\"version\":1,\"folders\":[{cut}")).unwrap();
        assert!(covered(&file, &proj).is_err());
        // With a backup of the last save the cut file is decided by it.
        std::fs::remove_file(&file).unwrap();
        add(&file, &proj).unwrap();
        std::fs::write(&file, b"{\"version\":1,\"fol").unwrap();
        assert_eq!(covered(&file, &proj), Ok(true));
        assert_eq!(covered(&file, &other), Ok(false));
        // A file that cannot be read: every folder may be marked.
        std::fs::remove_file(&file).unwrap();
        std::fs::create_dir_all(&file).unwrap();
        assert_eq!(covered(&file, &other), Err(MarksError::Unreadable));
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
