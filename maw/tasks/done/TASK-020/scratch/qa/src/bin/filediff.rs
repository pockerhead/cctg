// Per-file multiset line diff old vs new brief. Prints categories only, never content.
use std::collections::{BTreeMap, HashMap};
use std::path::{Path, PathBuf};
fn walk(dir: &Path, out: &mut Vec<PathBuf>) {
    let Ok(rd) = std::fs::read_dir(dir) else { return };
    for e in rd.flatten() { let p = e.path(); if p.is_dir() { walk(&p, out) } else if p.extension().is_some_and(|x| x == "jsonl") { out.push(p) } }
}
fn cat(l: &str) -> String {
    if l.is_empty() { "blank".into() } else if l == "в работе…" { "marker".into() } else if l.starts_with("> ") { "other-prompt".into() } else if l.trim_start().starts_with("<command-") { "raw-tag-line".into() } else if l.starts_with("> /") { "slash-prompt".into() } else { "OTHER".into() }
}
fn main() {
    let root = Path::new(&std::env::var("USERPROFILE").unwrap()).join(".claude").join("projects");
    let mut files = Vec::new(); walk(&root, &mut files);
    let mut c: BTreeMap<String, usize> = BTreeMap::new();
    for f in &files {
        let Ok(d) = std::fs::read_to_string(f) else { continue };
        for full in [false, true] {
            let (n, o) = if full { (transcript::render_full(&transcript::parse(&d)), transcript_old::render_full(&transcript_old::parse(&d))) }
                         else { (transcript::render_brief(&transcript::parse(&d)), transcript_old::render_brief(&transcript_old::parse(&d))) };
            if n == o { continue; }
            let mut m: HashMap<&str, i64> = HashMap::new();
            for l in n.lines() { *m.entry(l).or_default() += 1; }
            for l in o.lines() { *m.entry(l).or_default() -= 1; }
            for (l, v) in m { if v > 0 { *c.entry(format!("full={full} added {}", cat(l))).or_default() += v as usize; } if v < 0 { *c.entry(format!("full={full} removed {}", if full && l.starts_with("> <command-") {"raw-command".into()} else {cat(l)})).or_default() += (-v) as usize; } }
        }
    }
    for (k, v) in &c { println!("{v:6} {k}"); }
}
