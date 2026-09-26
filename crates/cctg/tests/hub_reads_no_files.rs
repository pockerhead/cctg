//! The hub opens no file of a session's machine (TASK-034): outside its own
//! state (`registry.json` through `RegistryStore`, the `getUpdates` offset,
//! the device list and join codes (TASK-045), the `.env` it starts with) and its test helpers, no file API appears in
//! the non-test code of `src/hub/` (every file, also in subfolders). Session
//! files are read by the session's agent (`crate::reads`, `crate::tail`),
//! which the hub only asks over the link.

use std::path::{Path, PathBuf};

/// Files that keep the hub's own state; they may touch files anywhere.
const OWN_STATE: [&str; 4] = ["config.rs", "devices.rs", "offset.rs", "testdir.rs"];

/// Items that keep the hub's own state inside other files: the file and the
/// first line of the item, which ends at its matching closing brace.
const OWN_STATE_ITEMS: [(&str, &str); 1] = [("registry.rs", "impl RegistryStore {")];

/// Whole identifiers that open, list or probe files (`fs` also catches
/// `use std::{fs as x}` and `tokio::fs`), and the crate modules that read
/// files for the agent and the hooks.
const FORBIDDEN_NAMES: [&str; 17] = [
    "fs",
    "OpenOptions",
    "read_dir",
    "read_to_string",
    "canonicalize",
    "metadata",
    "symlink_metadata",
    "exists",
    "try_exists",
    "is_file",
    "is_dir",
    "is_symlink",
    "read_link",
    "tail",
    "spool",
    "proctree",
    "device",
];

/// Paths matched as text: `File::open`/`create` (the hub's `api::File` is a
/// Telegram type).
const FORBIDDEN_PATHS: [&str; 1] = ["File::"];

/// Crate modules that also read files, with the items of theirs the hub may
/// name (pure: byte math, names, queue room); constants (`UPPER_CASE`) are
/// always fine. Anything else after `reads::`/`files::`, a brace import or
/// an alias of the module is a file read the guard cannot follow.
const READERS: [(&str, &[&str]); 2] = [
    ("reads", &[]),
    (
        "files",
        &[
            "chunks",
            "room",
            "is_photo",
            "default_name",
            "clean_name",
            "date",
            "Assembly",
            "Broken",
        ],
    ),
];

/// The code before the file's `#[cfg(test)]` module.
fn production(source: &str) -> &str {
    source
        .find("#[cfg(test)]\nmod tests")
        .map_or(source, |at| &source[..at])
}

/// `source` with comments removed and the insides of string and char
/// literals blanked, newlines kept: what is left is code.
fn code_only(source: &str) -> String {
    let chars: Vec<char> = source.chars().collect();
    let mut out = String::with_capacity(source.len());
    let blank = |c: char| if c == '\n' { '\n' } else { ' ' };
    let mut i = 0;
    while i < chars.len() {
        let c = chars[i];
        let next = chars.get(i + 1).copied();
        if c == '/' && next == Some('/') {
            while i < chars.len() && chars[i] != '\n' {
                i += 1;
            }
        } else if c == '/' && next == Some('*') {
            let mut depth = 0;
            while i < chars.len() {
                if chars[i] == '/' && chars.get(i + 1) == Some(&'*') {
                    depth += 1;
                    i += 2;
                } else if chars[i] == '*' && chars.get(i + 1) == Some(&'/') {
                    depth -= 1;
                    i += 2;
                    if depth == 0 {
                        break;
                    }
                } else {
                    out.push(blank(chars[i]));
                    i += 1;
                }
            }
        } else if c == '"' || (c == 'r' && matches!(next, Some('"' | '#')) && raw_start(&chars, i))
        {
            // A string: `"..."` with escapes, or `r#"..."#` without.
            let raw = c == 'r';
            let mut hashes = 0;
            if raw {
                i += 1; // the `r`
                while chars.get(i) == Some(&'#') {
                    hashes += 1;
                    i += 1;
                }
            }
            i += 1; // the opening quote
            out.push('"');
            while i < chars.len() {
                if !raw && chars[i] == '\\' {
                    out.push(' ');
                    out.push(blank(chars.get(i + 1).copied().unwrap_or(' ')));
                    i += 2;
                    continue;
                }
                if chars[i] == '"' && (0..hashes).all(|h| chars.get(i + 1 + h) == Some(&'#')) {
                    i += 1 + hashes;
                    break;
                }
                out.push(blank(chars[i]));
                i += 1;
            }
            out.push('"');
        } else if c == '\'' && char_literal_len(&chars, i).is_some() {
            let len = char_literal_len(&chars, i).unwrap_or(1);
            out.push_str("' '");
            i += len;
        } else {
            out.push(c);
            i += 1;
        }
    }
    out
}

/// An `r` at `i` starts a raw string (not the end of an identifier).
fn raw_start(chars: &[char], i: usize) -> bool {
    let before = i.checked_sub(1).map(|b| chars[b]);
    let ident_before = before.is_some_and(|b| b.is_alphanumeric() || b == '_');
    // `br"..."` is a raw byte string.
    let byte = before == Some('b') && i.checked_sub(2).is_none_or(|b| !chars[b].is_alphanumeric());
    let mut j = i + 1;
    while chars.get(j) == Some(&'#') {
        j += 1;
    }
    (!ident_before || byte) && chars.get(j) == Some(&'"')
}

/// The length of the char literal starting at `i` (`'x'`, `'\n'`,
/// `'\u{..}'`), or `None` for a lifetime or label.
fn char_literal_len(chars: &[char], i: usize) -> Option<usize> {
    match chars.get(i + 1)? {
        '\\' => {
            let end = (i + 2..chars.len().min(i + 14)).find(|&j| chars[j] == '\'')?;
            Some(end - i + 1)
        }
        _ => (chars.get(i + 2) == Some(&'\'')).then_some(3),
    }
}

/// `code` with the items named in [`OWN_STATE_ITEMS`] for file `name`
/// blanked, lines kept.
fn without_own_items(name: &str, code: &str) -> String {
    let mut code = code.to_owned();
    for (file, header) in OWN_STATE_ITEMS {
        if file != name {
            continue;
        }
        let Some(start) = code.find(header) else {
            continue;
        };
        let mut depth = 0;
        let mut end = code.len();
        for (at, c) in code[start..].char_indices() {
            match c {
                '{' => depth += 1,
                '}' => {
                    depth -= 1;
                    if depth == 0 {
                        end = start + at + 1;
                        break;
                    }
                }
                _ => {}
            }
        }
        let blanked: String = code[start..end]
            .chars()
            .map(|c| if c == '\n' { '\n' } else { ' ' })
            .collect();
        code.replace_range(start..end, &blanked);
    }
    code
}

fn is_ident(c: char) -> bool {
    c.is_alphanumeric() || c == '_'
}

/// Whether the use of reader module `module` at the start of `rest` (the
/// text right after the word) names only what the hub may use.
fn reader_use_is_pure(allowed: &[&str], rest: &str) -> bool {
    let rest = rest.trim_start();
    if let Some(path) = rest.strip_prefix("::") {
        let path = path.trim_start();
        let item: String = path.chars().take_while(|&c| is_ident(c)).collect();
        let constant = !item.is_empty()
            && item
                .chars()
                .all(|c| c.is_ascii_uppercase() || c.is_ascii_digit() || c == '_');
        return constant || allowed.contains(&item.as_str());
    }
    // `use crate::files as x;` would hide every later use.
    let word: String = rest.chars().take_while(|&c| is_ident(c)).collect();
    word != "as"
}

/// `line: text` of every production line of file `name` that touches files.
fn offending(name: &str, source: &str) -> Vec<String> {
    let code = without_own_items(name, &code_only(production(source)));
    let lines: Vec<&str> = source.lines().collect();
    let mut found = Vec::new();
    let mut flag = |at: usize| {
        let number = code[..at].matches('\n').count();
        let text = format!(
            "{}: {}",
            number + 1,
            lines.get(number).map_or("", |l| l.trim())
        );
        if !found.contains(&text) {
            found.push(text);
        }
    };
    let mut start = None;
    for (at, c) in code.char_indices().chain([(code.len(), ' ')]) {
        match (is_ident(c), start) {
            (true, None) => start = Some(at),
            (false, Some(from)) => {
                start = None;
                let word = &code[from..at];
                if FORBIDDEN_NAMES.contains(&word) {
                    flag(from);
                }
                let before = code[..from].chars().next_back();
                let a_field = before == Some('.');
                if let Some((_, allowed)) = READERS.iter().find(|(module, _)| *module == word)
                    && !a_field
                    && !reader_use_is_pure(allowed, &code[at..])
                {
                    flag(from);
                }
            }
            _ => {}
        }
    }
    for path in FORBIDDEN_PATHS {
        for (at, _) in code.match_indices(path) {
            flag(at);
        }
    }
    found
}

/// Every `.rs` file under `dir`, as (path relative to `dir`, source).
fn sources(dir: &Path) -> Vec<(String, String)> {
    let mut found = Vec::new();
    let mut todo: Vec<PathBuf> = vec![dir.to_owned()];
    while let Some(folder) = todo.pop() {
        for entry in std::fs::read_dir(&folder).expect("source folder") {
            let path = entry.expect("entry").path();
            if path.is_dir() {
                todo.push(path);
            } else if path.extension().is_some_and(|ext| ext == "rs") {
                let name = path
                    .strip_prefix(dir)
                    .expect("inside")
                    .to_string_lossy()
                    .replace('\\', "/");
                let source = std::fs::read_to_string(&path)
                    .expect("source")
                    .replace("\r\n", "\n");
                found.push((name, source));
            }
        }
    }
    found
}

#[test]
fn hub_sources_read_no_session_files() {
    let dir = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("src")
        .join("hub");
    let mut checked = 0;
    let mut found = Vec::new();
    for (name, source) in sources(&dir) {
        if OWN_STATE.contains(&name.as_str()) {
            continue;
        }
        checked += 1;
        found.extend(
            offending(&name, &source)
                .into_iter()
                .map(|at| format!("{name}:{at}")),
        );
    }
    assert!(checked >= 15, "only {checked} hub sources found");
    assert!(
        found.is_empty(),
        "file access in the hub:\n{}",
        found.join("\n")
    );
}

#[test]
fn the_guard_sees_a_file_read() {
    for read in [
        "let _ = std::fs::read(\"x\");",
        "use std::{fs as disk};",
        "let _ = tokio::fs::read(p).await;",
        "let _ = File::open(p);",
        "let _ = Path::new(p).exists();",
        "let _ = p.metadata();",
        "let _ = crate::reads::answer(None, s, ask);",
        "crate::spool::replay(s);",
        // Imports that hide the reader behind another name.
        "use crate::reads::{answer}; let _ = answer(None, s, ask);",
        "use crate::reads::*;",
        "use crate::files::{save as keep};",
        "use crate::files as f;",
        "use crate::{files as f};",
        "let _ = files::save(w, n, b, t);",
        "let _ = crate::files :: read_upload(p);",
        // A `//` inside a string does not hide the rest of the line.
        "let u = (\"//\", std::fs::read(p));",
        "let u = (r#\"a \"// b\"#, std::fs::read(p));",
        "let c = '\"'; let _ = std::fs::read(p);",
        // An empty string ends at its second quote (TASK-045).
        "let e = \"\"; let _ = std::fs::read(p);",
    ] {
        let source = format!("fn f() {{ {read} }}\n#[cfg(test)]\nmod tests {{}}\n");
        assert_eq!(offending("slots.rs", &source).len(), 1, "{read}");
    }
    // A string over lines hides nothing after it.
    let multi = "fn f() {\n let s = \"a\n // b\";\n let _ = std::fs::read(p);\n}\n";
    assert_eq!(
        offending("slots.rs", multi),
        ["4: let _ = std::fs::read(p);"]
    );
    // `registry.rs` may touch files only inside `RegistryStore`.
    let registry = "impl RegistryStore {\n    fn save(&self) { std::fs::write(p, b); }\n}\n\
                    fn other() { std::fs::write(p, b); }\n";
    assert_eq!(
        offending("registry.rs", registry),
        ["4: fn other() { std::fs::write(p, b); }"]
    );
    assert_eq!(offending("slots.rs", registry).len(), 2);
    // Tests, comments, strings and the hub's own names are not file reads.
    let clean = "use super::api::File;\nuse crate::files;\n\
                 const M: usize = crate::reads::MAX_TEXT; // std::fs\n\
                 fn f(x: &File) { let _ = files::chunks(1, b); let _ = files::MAX_UPLOAD; }\n\
                 fn g(&mut self) { self.reads.insert(1); let files = true; let _ = \"std::fs\"; }\n\
                 struct C { reads: bool, files: bool }\n\
                 fn e() -> [&'static str; 2] { [\"\", \"a device list\"] }\n\
                 fn h<'a>(x: &'a str) -> char { '\\'' }\n\
                 /* std::fs::read(p) */\n\
                 #[cfg(test)]\nmod tests { fn g() { std::fs::read(\"x\"); } }\n";
    assert_eq!(offending("slots.rs", clean), Vec::<String>::new());
}

#[test]
fn every_hub_source_is_found_in_subfolders_too() {
    let dir = std::env::temp_dir().join(format!("cctg-guard-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(dir.join("inner")).unwrap();
    std::fs::write(dir.join("a.rs"), "fn a() {}\n").unwrap();
    std::fs::write(dir.join("inner").join("b.rs"), "fn b() {}\n").unwrap();
    std::fs::write(dir.join("notes.txt"), "").unwrap();
    let mut names: Vec<String> = sources(&dir).into_iter().map(|(name, _)| name).collect();
    names.sort();
    let _ = std::fs::remove_dir_all(&dir);
    assert_eq!(names, ["a.rs", "inner/b.rs"]);
}
