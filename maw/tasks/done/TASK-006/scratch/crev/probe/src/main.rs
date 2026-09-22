// Read-only probe over ~/.claude/projects: split invariants, thinking leakage, in-progress marker sanity.
use std::{fs, path::Path, time::Instant};
use transcript::*;

fn walk(dir: &Path, out: &mut Vec<std::path::PathBuf>) {
    if let Ok(rd) = fs::read_dir(dir) {
        for e in rd.flatten() {
            let p = e.path();
            if p.is_dir() { walk(&p, out) } else if p.extension().is_some_and(|x| x == "jsonl") { out.push(p) }
        }
    }
}

fn thinking_snippets(src: &str) -> Vec<String> {
    let mut v = Vec::new();
    for line in src.lines() {
        let Ok(j) = serde_json::from_str::<serde_json::Value>(line) else { continue };
        if let Some(arr) = j.pointer("/message/content").and_then(|c| c.as_array()) {
            for b in arr {
                if b.get("type").and_then(|t| t.as_str()) == Some("thinking") {
                    if let Some(t) = b.get("thinking").and_then(|t| t.as_str()) {
                        let t = t.trim();
                        // take a 60-char middle slice, skip short
                        let cs: Vec<char> = t.chars().collect();
                        if cs.len() >= 120 { v.push(cs[30..90].iter().collect()); }
                    }
                }
            }
        }
    }
    v
}

fn check_split(text: &str, tag: &str, path: &Path, bad: &mut usize) {
    let r = split_for_telegram(text, SplitOptions::default());
    let mut pos = 0usize;
    for c in &r.chunks {
        if telegram_len(c) > TELEGRAM_TEXT_LIMIT || c.trim().is_empty() { *bad += 1; eprintln!("BAD chunk {tag} {}", path.display()); }
        match text[pos..].find(c.as_str()) {
            Some(off) => { if !text[pos..pos+off].trim().is_empty() { *bad += 1; eprintln!("LOST text {tag} {}", path.display()); } pos += off + c.len(); }
            None => { *bad += 1; eprintln!("NOT SLICE {tag} {}", path.display()); }
        }
    }
    if !text[pos..].trim().is_empty() { *bad += 1; eprintln!("LOST tail {tag} {}", path.display()); }
}

fn main() {
    let home = std::env::var("USERPROFILE").unwrap();
    let mut files = Vec::new();
    walk(&Path::new(&home).join(".claude/projects"), &mut files);
    let (mut bad, mut leaks, mut marker_after_end, mut nfiles, mut bytes) = (0, 0, 0, 0, 0usize);
    let mut max_ms = (0u128, String::new());
    let mut marker_count = 0;
    for p in &files {
        let Ok(src) = fs::read_to_string(p) else { continue };
        nfiles += 1; bytes += src.len();
        let t0 = Instant::now();
        let turns = parse(&src);
        let b = render_brief(&turns);
        let f = render_full(&turns);
        let ms = t0.elapsed().as_millis();
        if ms > max_ms.0 { max_ms = (ms, format!("{} ({} KB)", p.display(), src.len()/1024)); }
        check_split(&b, "brief", p, &mut bad);
        check_split(&f, "full", p, &mut bad);
        for s in thinking_snippets(&src) {
            if b.contains(&s) || f.contains(&s) { leaks += 1; eprintln!("THINKING LEAK {} in_brief={}", p.display(), b.contains(&s)); locate(&src, &s); break; }
        }
        if let Some(at) = b.find("> This session is being continued from a previous conversation") { let r = split_for_telegram(&b, SplitOptions::default()); let without = b[..at].len(); eprintln!("COMPACT in brief: brief_len={} before_summary={} chunks={} prefer_file={}", telegram_len(&b), without, r.chunks.len(), r.prefer_file); }
        if b.ends_with("в работе…") {
            marker_count += 1;
            // is the last assistant text record end_turn and nothing but service/meta after it?
            let last_asst = turns.iter().rposition(|t| t.role == Role::Assistant && t.blocks.iter().any(|b| matches!(b, Block::Text(_) | Block::ToolUse{..})));
            if let Some(i) = last_asst {
                if turns[i].stop_reason.as_deref() == Some("end_turn") {
                    marker_after_end += 1;
                    if marker_after_end <= 15 {
                        let tail: Vec<String> = turns[i+1..].iter().map(|t| {
                            let first = t.blocks.iter().map(|b| match b { Block::Text(x) => format!("text:{}", x.chars().take(50).collect::<String>().replace('\n', " ")), Block::ToolUse{name,..} => format!("tool_use:{name}"), Block::ToolResult{..} => "tool_result".into() }).collect::<Vec<_>>().join(",");
                            format!("{:?} meta={} [{}]", t.role, t.is_meta, first)
                        }).collect();
                        eprintln!("MARKER AFTER end_turn {}\n   tail: {:?}", p.display(), tail);
                    }
                }
            }
        }
    }
    println!("files={nfiles} MB={} bad_split={bad} thinking_leaks={leaks} marker_files={marker_count} marker_after_end_turn={marker_after_end} slowest={}ms {}", bytes/1_000_000, max_ms.0, max_ms.1);
}
#[allow(dead_code)]
pub fn locate(src: &str, snippet: &str) {
    for (n, line) in src.lines().enumerate() {
        if line.contains(&snippet.replace('"', "\\\"").replace('\n', "\n")) || line.contains(snippet) {
            let j: serde_json::Value = serde_json::from_str(line).unwrap_or_default();
            let types: Vec<String> = j.pointer("/message/content").and_then(|c| c.as_array()).map(|a| a.iter().map(|b| b.get("type").and_then(|t| t.as_str()).unwrap_or("?").to_owned()).collect()).unwrap_or_default();
            eprintln!("  line {n}: type={} content_types={:?}", j.get("type").and_then(|t| t.as_str()).unwrap_or("?"), types);
        }
    }
}
