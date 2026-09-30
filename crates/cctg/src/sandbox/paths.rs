//! Is a path inside the session folder, and is it one of the folder's
//! protected files? Pure functions over the file system: symlinks and
//! junctions are resolved, `..` never escapes lexically.
//!
//! Case: macOS and Windows compare folder names without case (their usual
//! file systems do), Linux with case. Protected names are compared without
//! case everywhere (`.GiT` must not pass on a case-insensitive disk).

use std::path::{Component, Path, PathBuf, Prefix};

/// The host's usual file systems ignore case.
const FOLDS_CASE: bool = cfg!(any(windows, target_os = "macos"));

/// A protected tree when it is the first component under the folder.
const PROTECTED_DIRS: &[&str] = &[".claude", ".git", ".vscode", ".idea"];
/// A protected file or directory right in the folder: the list of the
/// Claude Code sandbox (docs sandboxing, "always write-protected").
const PROTECTED_TOP: &[&str] = &[
    ".mcp.json",
    ".gitconfig",
    ".bashrc",
    ".bash_profile",
    ".bash_login",
    ".profile",
    ".zshrc",
    ".zshenv",
    ".zprofile",
    ".zlogin",
    "head",
    "config",
    "hooks",
    "objects",
    "refs",
];

/// `std::fs::canonicalize` without Windows' `\\?\` where a plain spelling
/// exists.
pub fn canonical(path: &Path) -> Option<PathBuf> {
    let resolved = std::fs::canonicalize(path).ok()?;
    Some(match resolved.into_os_string().into_string() {
        Ok(text) => PathBuf::from(crate::device::strip_verbatim(&text)),
        Err(raw) => PathBuf::from(raw),
    })
}

/// Where `path` (relative: to `root`) really is: the deepest existing
/// ancestor resolved, plus the missing rest, which may hold plain names
/// only. `None` when that is not certain (`..` or `.` in the missing rest,
/// a dangling link, no access).
pub fn resolve(root: &Path, path: &Path) -> Option<PathBuf> {
    let full = if path.is_absolute() {
        path.to_path_buf()
    } else {
        root.join(path)
    };
    let mut missing = Vec::new();
    let mut existing = full.as_path();
    while std::fs::symlink_metadata(existing).is_err() {
        match existing.components().next_back()? {
            Component::Normal(name) => missing.push(name.to_owned()),
            _ => return None,
        }
        existing = existing.parent()?;
    }
    let mut resolved = canonical(existing)?;
    resolved.extend(missing.iter().rev());
    Some(resolved)
}

/// `path` resolves to `root` or below it.
pub fn contains(root: &Path, path: &Path) -> bool {
    match (canonical(root), resolve(root, path)) {
        (Some(root), Some(path)) => within(&root, &path),
        _ => false,
    }
}

/// Lexically: `path` is `base` or below it, component by component, with
/// the host's case rule. Both should be canonical.
pub fn within(base: &Path, path: &Path) -> bool {
    below(base, path, FOLDS_CASE).is_some()
}

/// The components of `path` under `base`, `None` when it is not under it.
fn below<'a>(base: &Path, path: &'a Path, fold: bool) -> Option<Vec<Component<'a>>> {
    let mut rest = path.components();
    for part in base.components() {
        if !same(part, rest.next()?, fold) {
            return None;
        }
    }
    Some(rest.collect())
}

fn same(a: Component<'_>, b: Component<'_>, fold: bool) -> bool {
    if !fold {
        return a == b;
    }
    key(a).to_lowercase() == key(b).to_lowercase()
}

/// One spelling of a component: `C:` and `c:`, `\\srv\share` and
/// `\\?\UNC\srv\share` are one prefix.
fn key(component: Component<'_>) -> String {
    match component {
        Component::Prefix(prefix) => match prefix.kind() {
            Prefix::Disk(letter) | Prefix::VerbatimDisk(letter) => format!("{}:", letter as char),
            Prefix::UNC(server, share) | Prefix::VerbatimUNC(server, share) => format!(
                r"\\{}\{}",
                server.to_string_lossy(),
                share.to_string_lossy()
            ),
            _ => prefix.as_os_str().to_string_lossy().into_owned(),
        },
        other => other.as_os_str().to_string_lossy().into_owned(),
    }
}

/// A file the gate must not let a tool write although it is in the folder:
/// `.claude`, `.git`, `.vscode`, `.idea` trees and the top-level names of
/// [`PROTECTED_TOP`]. Inside `.claude/worktrees/<name>/` the same rule
/// applies to the rest of the path (a worktree is a working folder of its
/// own). A path outside the folder is not "protected" ([`contains`] says
/// no); one that cannot be resolved is (fail closed).
// Probe P9: worktrees cannot be created in the sandbox; the rule stays for
// a worktree made outside it.
pub fn protected(root: &Path, path: &Path) -> bool {
    let (Some(root), Some(path)) = (canonical(root), resolve(root, path)) else {
        return true;
    };
    let Some(rest) = below(&root, &path, FOLDS_CASE) else {
        return false;
    };
    let names: Vec<String> = rest
        .iter()
        .map(|part| part.as_os_str().to_string_lossy().to_lowercase())
        .collect();
    let names = match names.as_slice() {
        [claude, worktrees, _, rest @ ..]
            if claude == ".claude" && worktrees == "worktrees" && !rest.is_empty() =>
        {
            rest
        }
        all => all,
    };
    match names {
        [] => false,
        [first, ..] if PROTECTED_DIRS.contains(&first.as_str()) => true,
        [only] => PROTECTED_TOP.contains(&only.as_str()),
        _ => false,
    }
}

/// A plan file of plan mode: a `*.md` right in `<config_dir>/plans`, and
/// when it exists, a plain file (no link out).
// Probe P8: plan mode is not available in the profile (see gate.rs).
pub fn plan_file(config_dir: &Path, path: &Path) -> bool {
    if !path.is_absolute() {
        return false;
    }
    let (Some(parent), Some(name)) = (path.parent(), path.file_name()) else {
        return false;
    };
    let markdown = Path::new(name)
        .extension()
        .is_some_and(|ext| ext.eq_ignore_ascii_case("md"));
    let in_plans = match (canonical(parent), canonical(&config_dir.join("plans"))) {
        (Some(parent), Some(plans)) => within(&plans, &parent) && within(&parent, &plans),
        _ => false,
    };
    markdown
        && in_plans
        && match std::fs::symlink_metadata(path) {
            Ok(meta) => meta.file_type().is_file(),
            Err(error) => error.kind() == std::io::ErrorKind::NotFound,
        }
}

/// Two metadata describe one file (device and inode).
#[cfg(unix)]
pub fn same_file(a: &std::fs::Metadata, b: &std::fs::Metadata) -> bool {
    use std::os::unix::fs::MetadataExt;
    a.dev() == b.dev() && a.ino() == b.ino()
}

/// Opens `path` for reading only when it lies in `root`: opened first
/// (without blocking on a pipe), then its resolved path must be inside
/// `root` and name the very file that is open. Reading from the handle, a
/// caller gains nothing from a swap of the path for a link in between.
#[cfg(unix)]
pub fn open_inside(root: &Path, path: &Path) -> Option<std::fs::File> {
    use std::os::unix::fs::OpenOptionsExt;
    let file = std::fs::OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NONBLOCK)
        .open(path)
        .ok()?;
    let opened = file.metadata().ok()?;
    let resolved = canonical(path)?;
    let named = std::fs::metadata(&resolved).ok()?;
    (contains(root, &resolved) && same_file(&opened, &named)).then_some(file)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::hub::testdir::TempDir;

    /// A canonical folder `proj` with `src/a.rs`, next to `outside/marker`.
    fn tree(name: &str) -> (TempDir, PathBuf) {
        let dir = TempDir::new(name);
        let root = canonical(dir.path()).unwrap().join("proj");
        std::fs::create_dir_all(root.join("src")).unwrap();
        std::fs::write(root.join("src").join("a.rs"), b"").unwrap();
        std::fs::create_dir_all(dir.path().join("outside")).unwrap();
        std::fs::write(dir.path().join("outside").join("marker"), b"x").unwrap();
        (dir, root)
    }

    #[test]
    fn contains_resolves_before_it_compares() {
        let (_dir, root) = tree("paths-contains");
        assert!(contains(&root, &root.join("src").join("a.rs")));
        assert!(contains(&root, &root));
        assert!(contains(&root, Path::new("src/a.rs")), "relative to root");
        assert!(
            contains(&root, &root.join("src").join("new.rs")),
            "a new file"
        );
        assert!(contains(&root, &root.join("new").join("deeper").join("x")));
        assert!(!contains(
            &root,
            &root.join("..").join("outside").join("marker")
        ));
        assert!(!contains(&root, Path::new("../outside/new")));
        assert!(
            contains(&root, &root.join("src").join("..").join("src").join("b.rs")),
            "`..` inside the existing part is resolved"
        );
        assert!(
            !contains(&root, &root.join("missing").join("..").join("..").join("x")),
            "`..` after a missing name is never guessed"
        );
        // Windows removes `..` lexically before it looks at the disk, so
        // there `missing/..` is simply the folder.
        #[cfg(unix)]
        assert_eq!(
            resolve(&root, &root.join("missing").join("..").join("x")),
            None
        );
        assert!(!contains(&root.join("missing-root"), &root));
    }

    #[test]
    fn case_follows_the_host() {
        let base = if cfg!(windows) {
            r"C:\Work\Proj"
        } else {
            "/Work/Proj"
        };
        let lower = if cfg!(windows) {
            r"c:\work\proj\x"
        } else {
            "/work/proj/x"
        };
        assert_eq!(within(Path::new(base), Path::new(lower)), FOLDS_CASE);
        assert!(within(Path::new(base), &Path::new(base).join("x")));
        assert!(!within(
            Path::new(base),
            &Path::new(base).with_file_name("Proj2")
        ));
        assert!(!within(&Path::new(base).join("x"), Path::new(base)));
    }

    #[cfg(windows)]
    #[test]
    fn windows_spellings_of_one_place_match() {
        assert!(within(Path::new(r"C:\a"), Path::new("C:/a/b")));
        assert!(within(
            Path::new(r"\\srv\share\a"),
            Path::new(r"\\SRV\share\a\b")
        ));
        assert!(within(
            Path::new(r"\\srv\share\a"),
            Path::new(r"\\?\UNC\srv\share\a\b")
        ));
        assert!(within(Path::new(r"C:\a"), Path::new(r"\\?\C:\a\b")));
        assert!(!within(Path::new(r"C:\a"), Path::new(r"D:\a\b")));
    }

    #[test]
    fn protected_names_are_the_config_and_vcs_files() {
        let (_dir, root) = tree("paths-protected");
        let yes = [
            ".claude/settings.json",
            ".CLAUDE/x",
            ".git/config",
            ".GiT/hooks/pre-commit",
            ".vscode/tasks.json",
            ".idea/x.xml",
            ".mcp.json",
            ".bashrc",
            ".zshenv",
            "HEAD",
            "config",
            "hooks",
            ".claude/worktrees/wt/.git",
            ".claude/worktrees/wt/.claude/x",
            ".claude/worktrees/wt",
            ".claude/worktrees",
        ];
        for path in yes {
            assert!(protected(&root, Path::new(path)), "{path}");
        }
        let no = [
            "src/a.rs",
            "src/.git/x",
            "src/config",
            "README.md",
            ".claude/worktrees/wt/src/a.rs",
            ".claude/worktrees/wt/config.toml",
            ".gitignore",
        ];
        for path in no {
            assert!(!protected(&root, Path::new(path)), "{path}");
        }
        assert!(
            !protected(&root, Path::new("../outside/.git")),
            "outside: not ours"
        );
        assert!(
            protected(&root, Path::new("missing/../.git")),
            "unresolvable"
        );
    }

    #[test]
    fn a_plan_file_is_a_markdown_file_right_in_plans() {
        let dir = TempDir::new("paths-plans");
        let config = canonical(dir.path()).unwrap().join("claude");
        let plans = config.join("plans");
        std::fs::create_dir_all(plans.join("sub")).unwrap();
        assert!(plan_file(&config, &plans.join("a.md")));
        std::fs::write(plans.join("b.md"), b"# plan").unwrap();
        assert!(plan_file(&config, &plans.join("b.md")));
        assert!(!plan_file(&config, &plans.join("sub").join("a.md")));
        assert!(!plan_file(&config, &plans.join("a.txt")));
        assert!(!plan_file(&config, &plans.join("..").join("a.md")));
        assert!(!plan_file(&config, Path::new("plans/a.md")));
        assert!(!plan_file(&config, &config.join("a.md")));
    }

    #[cfg(unix)]
    #[test]
    fn links_are_followed_to_where_they_lead() {
        use std::os::unix::fs::symlink;
        let (dir, root) = tree("paths-links");
        let outside = canonical(dir.path()).unwrap().join("outside");
        symlink(&outside, root.join("out-dir")).unwrap();
        symlink(outside.join("marker"), root.join("out-file")).unwrap();
        symlink(root.join("src"), root.join("in-dir")).unwrap();
        symlink(outside.join("gone"), root.join("dangling")).unwrap();
        assert!(!contains(&root, &root.join("out-dir").join("marker")));
        assert!(!contains(&root, &root.join("out-dir").join("new")));
        assert!(!contains(&root, &root.join("out-file")));
        assert!(contains(&root, &root.join("in-dir").join("a.rs")));
        assert!(!contains(&root, &root.join("dangling")), "a dangling link");
        symlink(&root, root.join("src").join("up")).unwrap();
        assert!(!protected(&root, &root.join("src").join("up").join("src")));
        assert!(protected(&root, &root.join("src").join("up").join(".git")));

        let config = canonical(dir.path()).unwrap().join("claude");
        std::fs::create_dir_all(config.join("plans")).unwrap();
        symlink(outside.join("marker"), config.join("plans").join("x.md")).unwrap();
        assert!(!plan_file(&config, &config.join("plans").join("x.md")));

        let a = std::fs::metadata(root.join("in-dir").join("a.rs")).unwrap();
        let b = std::fs::metadata(root.join("src").join("a.rs")).unwrap();
        let c = std::fs::metadata(outside.join("marker")).unwrap();
        assert!(same_file(&a, &b));
        assert!(!same_file(&a, &c));

        assert!(open_inside(&root, &root.join("src").join("a.rs")).is_some());
        assert!(open_inside(&root, &root.join("in-dir").join("a.rs")).is_some());
        assert!(open_inside(&root, &root.join("out-file")).is_none());
        assert!(open_inside(&root, &root.join("out-dir").join("marker")).is_none());
        assert!(open_inside(&root, &outside.join("marker")).is_none());
        assert!(open_inside(&root, &root.join("missing")).is_none());
        let fifo = root.join("pipe");
        let name = std::ffi::CString::new(fifo.to_str().unwrap()).unwrap();
        // SAFETY: a valid NUL-terminated path; mkfifo only creates the node.
        assert_eq!(unsafe { libc::mkfifo(name.as_ptr(), 0o600) }, 0);
        let pipe = open_inside(&root, &fifo).expect("a pipe opens without blocking");
        assert!(!pipe.metadata().unwrap().is_file());
    }
}
