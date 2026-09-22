const LIB: &str = include_str!("../src/lib.rs");
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
fn library_source_has_no_io_or_panicking_calls() {
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
        assert!(!LIB.contains(forbidden), "src/lib.rs contains {forbidden}");
    }
    assert!(!contains_identifier(
        &code_without_line_comments(LIB),
        "unsafe"
    ));
}

#[test]
fn library_has_only_serde_dependencies() {
    let deps = MANIFEST.split("[dependencies]").nth(1).unwrap_or_default();
    let names: Vec<&str> = deps
        .lines()
        .map(str::trim)
        .take_while(|line| !line.starts_with('['))
        .filter(|line| !line.is_empty() && !line.starts_with('#'))
        .filter_map(|line| line.split(['.', '=', ' ']).next())
        .collect();
    assert_eq!(names, ["serde", "serde_json"]);
}
