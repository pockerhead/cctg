// Code-reviewer harness: compares transcript::parse against an independent
// serde_json::Value count on every real jsonl under ~/.claude/projects.
// Prints only counts and file names, never record content.
use serde_json::Value;
use std::path::{Path, PathBuf};

fn walk(dir: &Path, out: &mut Vec<PathBuf>) {
    if let Ok(rd) = std::fs::read_dir(dir) {
        for e in rd.flatten() {
            let p = e.path();
            if p.is_dir() { walk(&p, out) } else if p.extension().map_or(false, |x| x == "jsonl") { out.push(p) }
        }
    }
}

fn visible(item: &Value) -> bool {
    matches!(item.get("type").and_then(Value::as_str), Some("text" | "tool_use" | "tool_result"))
}

fn main() {
    let home = std::env::var("USERPROFILE").unwrap();
    let mut files = Vec::new();
    walk(&Path::new(&home).join(".claude").join("projects"), &mut files);
    let (mut tot_expected, mut tot_got, mut mismatches, mut bad_lines, mut thinking_leaks) = (0usize, 0usize, 0usize, 0usize, 0usize);
    let mut block_kinds = std::collections::BTreeMap::new();
    let mut mismatch_reasons = std::collections::BTreeMap::new();
    for f in &files {
        let Ok(src) = std::fs::read_to_string(f) else { continue };
        let mut expected = 0usize;
        let mut exp_blocks = 0usize;
        let mut thinking = Vec::new();
        for line in src.lines().filter(|l| !l.trim().is_empty()) {
            let v: Value = match serde_json::from_str(line) { Ok(v) => v, Err(e) => { bad_lines += 1; *mismatch_reasons.entry(format!("badline:{}", e.classify() as u8)).or_insert(0) += 1; continue } };
            let kind = v.get("type").and_then(Value::as_str).unwrap_or("");
            if kind != "user" && kind != "assistant" { continue }
            let content = v.pointer("/message/content");
            match content {
                Some(Value::String(_)) => { expected += 1; exp_blocks += 1; }
                Some(Value::Array(items)) => {
                    for it in items {
                        let k = it.get("type").and_then(Value::as_str).unwrap_or("<none>").to_string();
                        *block_kinds.entry(format!("{kind}:{k}")).or_insert(0usize) += 1;
                        if k == "thinking" { if let Some(t) = it.get("thinking").and_then(Value::as_str) { if t.len() > 40 { thinking.push(t[..t.char_indices().nth(40).map_or(t.len(), |x| x.0)].to_string()); } } }
                    }
                    let n = items.iter().filter(|i| visible(i)).count();
                    if n > 0 { expected += 1; exp_blocks += n; }
                    // detect visible-typed blocks that would fail typed decode
                    for it in items.iter().filter(|i| visible(i)) {
                        let bad = match it.get("type").and_then(Value::as_str) {
                            Some("text") => it.get("text").map_or(false, |t| !t.is_string()),
                            Some("tool_use") => ["id","name"].iter().any(|k| it.get(*k).map_or(false, |t| !t.is_string())),
                            Some("tool_result") => it.get("tool_use_id").map_or(false, |t| !t.is_string()),
                            _ => false,
                        };
                        if bad { *mismatch_reasons.entry("visible_block_wrong_typed_field".into()).or_insert(0) += 1; }
                    }
                }
                other => { *mismatch_reasons.entry(format!("content_shape:{}", match other { None => "missing", Some(Value::Null) => "null", _ => "other" })).or_insert(0) += 1; }
            }
        }
        let turns = transcript::parse(&src);
        let got_blocks: usize = turns.iter().map(|t| t.blocks.len()).sum();
        if turns.len() != expected || got_blocks != exp_blocks {
            mismatches += 1;
            println!("MISMATCH {} expected_turns={} got={} expected_blocks={} got_blocks={}", f.file_name().unwrap().to_string_lossy(), expected, turns.len(), exp_blocks, got_blocks);
        }
        let dbg = format!("{:?}", turns);
        for t in &thinking { if dbg.contains(t.as_str()) { thinking_leaks += 1;
            for (i, tu) in turns.iter().enumerate() { for b in &tu.blocks { let d = format!("{:?}", b); if d.contains(t.as_str()) {
                let kind = match b { transcript::Block::Text(_) => "Text", transcript::Block::ToolUse{name,..} => name.as_str(), transcript::Block::ToolResult{..} => "ToolResult" };
                println!("LEAK file={} turn={} role={:?} block={} sidechain={}", f.display(), i, tu.role, kind, tu.is_sidechain); } } } } }
        let _ = transcript::ai_title(&src);
        tot_expected += expected; tot_got += turns.len();
    }
    println!("files={} expected_turns={} got_turns={} mismatching_files={} bad_json_lines={} thinking_prefix_found_in_output={}", files.len(), tot_expected, tot_got, mismatches, bad_lines, thinking_leaks);
    println!("block kinds: {:?}", block_kinds);
    println!("notes: {:?}", mismatch_reasons);
}
