//! Read-only QA probe over the real ~/.claude/projects. Prints counts, sizes,
//! booleans and timings only; never content, ids or paths.
use std::io::Read;
use std::time::Instant;

use cctg::hub::commands::{Prepared, TranscriptCommand, View, prepare};
use cctg::hub::sessions::{LocateError, ProjectsDir, TranscriptLocator};
use serde_json::Value;
use transcript::{SplitOptions, TELEGRAM_TEXT_LIMIT, split_for_telegram, telegram_len};

fn thinking_texts(jsonl: &str) -> Vec<String> {
    let mut out = vec![];
    for line in jsonl.lines() {
        let Ok(v) = serde_json::from_str::<Value>(line) else { continue };
        if let Some(arr) = v.pointer("/message/content").and_then(Value::as_array) {
            for b in arr {
                if b["type"] == "thinking" {
                    if let Some(t) = b["thinking"].as_str() {
                        let t = t.trim();
                        if t.chars().count() >= 60 {
                            out.push(t.chars().take(60).collect());
                        }
                    }
                }
            }
        }
    }
    out
}

fn main() {
    let root = std::path::PathBuf::from(std::env::var("USERPROFILE").unwrap())
        .join(".claude")
        .join("projects");
    let dir = ProjectsDir::new(root.clone());
    let t = Instant::now();
    let newest = dir.locate(None, None).unwrap();
    println!("locate(None) {:?}; newest under subagents: {}", t.elapsed(),
        newest.path.components().any(|c| c.as_os_str() == "subagents"));

    // All top-level sessions via single-hex-char prefixes.
    let mut all = vec![];
    let mut leak = false;
    for c in "0123456789abcdef".chars() {
        match dir.locate(None, Some(&c.to_string())) {
            Ok(l) => all.push(l),
            Err(LocateError::Ambiguous(v)) => {
                all.extend(v);
            }
            Err(_) => {}
        }
    }
    println!("top-level sessions: {}", all.len());
    println!("any located under subagents: {}",
        all.iter().any(|l| l.path.components().any(|c| c.as_os_str() == "subagents")));

    // Ambiguous notices through the real prepare(): no project dir names.
    let mut amb = 0;
    for c in "0123456789abcdef".chars() {
        let cmd = TranscriptCommand { view: View::Brief, prompts: 1, session_prefix: Some(c.to_string()) };
        if let Prepared::Notice(text) = prepare(&dir, None, &cmd) {
            if text.starts_with("Под это") {
                amb += 1;
                for l in &all {
                    if text.contains(&l.project) { leak = true; }
                }
                if text.contains("C--") || text.contains(":\\") { leak = true; }
                if telegram_len(&text) > TELEGRAM_TEXT_LIMIT { println!("ambiguous notice over limit!"); }
            }
        }
    }
    println!("ambiguous notices checked: {amb}; project name leaked: {leak}");

    // ai-title coverage within first 64 KiB vs whole file.
    let (mut anywhere, mut head) = (0, 0);
    for l in &all {
        let Ok(bytes) = std::fs::read(&l.path) else { continue };
        let s = String::from_utf8_lossy(&bytes);
        if transcript::ai_title(&s).is_some() { anywhere += 1; }
        let mut h = vec![];
        let _ = std::fs::File::open(&l.path).unwrap().take(64 * 1024).read_to_end(&mut h);
        if transcript::ai_title(&String::from_utf8_lossy(&h)).is_some() { head += 1; }
    }
    println!("ai-title present: anywhere {anywhere}, within 64KiB {head} (of {})", all.len());

    // Render checks on up to 40 sessions.
    let mut sorted = all.clone();
    sorted.sort_by_key(|l| std::fs::metadata(&l.path).map(|m| m.len()).unwrap_or(0));
    let step = (sorted.len() / 40).max(1);
    let (mut checked, mut concat_bad, mut over, mut think_leak, mut docs, mut notices) = (0, 0, 0, 0, 0, 0);
    let mut worst = std::time::Duration::ZERO;
    for l in sorted.iter().step_by(step).chain(sorted.last()) {
        let jsonl = String::from_utf8_lossy(&std::fs::read(&l.path).unwrap()).into_owned();
        let thinking = thinking_texts(&jsonl);
        for view in [View::Brief, View::Full] {
            for n in [1usize, 3, 100] {
                let cmd = TranscriptCommand { view, prompts: n, session_prefix: Some(l.session_id.clone()) };
                let t = Instant::now();
                let p = prepare(&dir, None, &cmd);
                worst = worst.max(t.elapsed());
                checked += 1;
                match p {
                    Prepared::Transcript(r) => {
                        let turns = transcript::parse(&jsonl);
                        let s = transcript::last_prompts(&turns, n);
                        let want = match view { View::Brief => transcript::render_brief(s), View::Full => transcript::render_full(s) };
                        if want != r.body { concat_bad += 1; }
                        let sp = split_for_telegram(&r.body, SplitOptions::default());
                        if sp.prefer_file { docs += 1; }
                        if sp.chunks.concat() != r.body { concat_bad += 1; }
                        if sp.chunks.iter().any(|c| telegram_len(c) > TELEGRAM_TEXT_LIMIT) { over += 1; }
                        for th in thinking.iter().filter(|th| r.body.contains(th.as_str())) {
                            // Is it also in a non-thinking block of the parsed turns?
                            let elsewhere = turns.iter().flat_map(|t| &t.blocks).any(|b| match b {
                                transcript::Block::Text(x) => x.contains(th.as_str()),
                                transcript::Block::ToolUse { input, .. } => input.to_string().contains(th.as_str()) || input.as_object().is_some_and(|o| o.values().any(|v| v.as_str().is_some_and(|s| s.contains(th.as_str())))),
                                transcript::Block::ToolResult { content, .. } => content.contains(th.as_str()),
                            });
                            println!("  thinking prefix found in body; also present in a non-thinking block: {elsewhere}");
                            if !elsewhere { think_leak += 1; }
                        }
                    }
                    Prepared::Notice(_) => notices += 1,
                }
            }
        }
    }
    println!("renders: {checked}, mismatches: {concat_bad}, chunk over limit: {over}, thinking leaked: {think_leak}, documents: {docs}, notices: {notices}, worst {:?}", worst);
}
