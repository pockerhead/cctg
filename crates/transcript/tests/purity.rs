const SOURCES: [(&str, &str); 5] = [
    ("lib.rs", include_str!("../src/lib.rs")),
    ("render.rs", include_str!("../src/render.rs")),
    ("split.rs", include_str!("../src/split.rs")),
    ("stream.rs", include_str!("../src/stream.rs")),
    ("subagent.rs", include_str!("../src/subagent.rs")),
];
const MANIFEST: &str = include_str!("../Cargo.toml");

fn code_without_line_comments(source: &str) -> String {
    source
        .lines()
        .map(|line| line.split_once("//").map_or(line, |(code, _)| code))
        .collect::<Vec<_>>()
        .join("\n")
}

fn contains_identifier(source: &str, identifier: &str) -> bool {
    source
        .split(|c: char| !c.is_ascii_alphanumeric() && c != '_')
        .any(|token| token == identifier)
}

#[test]
fn every_source_file_is_scanned() {
    let dir = concat!(env!("CARGO_MANIFEST_DIR"), "/src");
    let mut found: Vec<String> = std::fs::read_dir(dir)
        .unwrap()
        .map(|entry| entry.unwrap().file_name().to_string_lossy().into_owned())
        .collect();
    found.sort();
    let mut scanned: Vec<&str> = SOURCES.iter().map(|(name, _)| *name).collect();
    scanned.sort();
    assert_eq!(found, scanned);
}

#[test]
fn library_source_has_no_io_or_panicking_calls() {
    for (name, source) in SOURCES {
        assert_no_forbidden(name, source);
    }
}

fn assert_no_forbidden(name: &str, source: &str) {
    for forbidden in [
        "std::fs",
        "std::io",
        "std::net",
        "std::process",
        "std::env",
        "std::thread",
        "File::",
        "print!",
        "println!",
        "eprint!",
        "eprintln!",
        "dbg!",
        ".unwrap()",
        ".expect(",
        "panic!",
        "unreachable!",
        "todo!",
        "unimplemented!",
        "#[cfg(test)]",
    ] {
        assert!(
            !source.contains(forbidden),
            "src/{name} contains {forbidden}"
        );
    }
    assert!(!contains_identifier(
        &code_without_line_comments(source),
        "unsafe"
    ));
}

#[test]
fn library_has_only_pure_dependencies() {
    let deps = MANIFEST.split("[dependencies]").nth(1).unwrap_or_default();
    let names: Vec<&str> = deps
        .lines()
        .map(str::trim)
        .take_while(|line| !line.starts_with('['))
        .filter(|line| !line.is_empty() && !line.starts_with('#'))
        .filter_map(|line| line.split(['.', '=', ' ']).next())
        .collect();
    assert_eq!(names, ["serde", "serde_json", "unicode-segmentation"]);
}
