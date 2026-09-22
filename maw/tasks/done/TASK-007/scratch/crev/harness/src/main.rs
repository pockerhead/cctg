// Read-only probe over ~/.claude/projects. Prints only counts, never content.
use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};
use transcript::*;

fn walk(dir: &Path, out: &mut Vec<PathBuf>) {
    let Ok(rd) = fs::read_dir(dir) else { return };
    for e in rd.flatten() {
        let p = e.path();
        if p.is_dir() { walk(&p, out) } else if p.extension().is_some_and(|x| x == "jsonl") { out.push(p) }
    }
}

fn thinking_texts(jsonl: &str) -> Vec<String> {
    let mut v = vec![];
    for line in jsonl.lines() {
        let Ok(val) = serde_json::from_str::<serde_json::Value>(line) else { continue };
        if let Some(arr) = val.pointer("/message/content").and_then(|c| c.as_array()) {
            for b in arr {
                if b.get("type").and_then(|t| t.as_str()) == Some("thinking") {
                    if let Some(t) = b.get("thinking").and_then(|t| t.as_str()) {
                        // take a distinctive line of >= 40 chars
                        for l in t.lines() { let l = l.trim(); if l.chars().count() >= 40 { v.push(l.to_owned()); break; } }
                    }
                }
            }
        }
    }
    v
}

fn main() {
    let home = std::env::var("USERPROFILE").unwrap();
    let root = PathBuf::from(home).join(".claude").join("projects");
    let mut files = vec![];
    walk(&root, &mut files);
    let (subs, parents): (Vec<_>, Vec<_>) = files.into_iter().partition(|p| p.to_string_lossy().contains("subagents"));
    let mut kinds: HashMap<&str, usize> = HashMap::new();
    let mut thinking_leak = 0; let mut meta_type = 0; let mut header_agent = 0;
    let mut by_parent: HashMap<PathBuf, Vec<Subagent>> = HashMap::new();
    for p in &subs {
        let Ok(text) = fs::read_to_string(p) else { continue };
        let meta = fs::read_to_string(p.with_extension("meta.json")).ok();
        let id = p.file_stem().unwrap().to_string_lossy().trim_start_matches("agent-").to_owned();
        let s = Subagent::new(SubagentInput { agent_id: &id, meta: meta.as_deref(), transcript: Some(&text), ..Default::default() });
        let k = match s.body() { SubagentBody::Report(_) => "Report", SubagentBody::Transcript(_) => "Transcript", SubagentBody::LastMessage(_) => "Last", SubagentBody::InProgress(_) => "InProgress", SubagentBody::Empty => "Empty" };
        *kinds.entry(k).or_default() += 1;
        {
            let turns = parse(&text);
            let last = turns.iter().rev().filter(|t| t.role == Role::Assistant).flat_map(|t| t.blocks.iter().rev()).find_map(|b| match b { Block::Text(t) if !t.trim().is_empty() => Some(t.clone()), _ => None });
            if let Some(last) = last {
                let s2 = Subagent::new(SubagentInput { agent_id: &id, meta: meta.as_deref(), transcript: Some(&text), last_assistant_message: Some(&last), ..Default::default() });
                let k2 = match s2.body() { SubagentBody::Transcript(_) => "T", SubagentBody::LastMessage(_) => "L", SubagentBody::InProgress(_) => "I", _ => "O" };
                *kinds.entry(k2).or_default() += 1;
                if k2 == "L" {
                    let b = s.body().text();
                    let tail: Vec<String> = b.lines().rev().take(3).map(|l| l.chars().take(3).collect::<String>()).collect();
                    let tl = last.trim();
                    let pos = b.rfind(tl);
                    let after = pos.map(|p| b[p+tl.len()..].lines().count());
                    // records after last assistant text record
                    let idx = turns.iter().rposition(|t| t.role == Role::Assistant && t.blocks.iter().any(|b| matches!(b, Block::Text(x) if x.trim()==tl)));
                    let after_types: Vec<String> = idx.map(|i| turns[i+1..].iter().map(|t| format!("{:?}:{}", t.role, t.blocks.iter().map(|b| match b { Block::Text(_) => "T".to_string(), Block::ToolUse{name,..} => format!("U({name})"), Block::ToolResult{..} => "R".into() }).collect::<Vec<_>>().join(","))).collect()).unwrap_or_default();
                    eprintln!("L-case: tail_prefixes={tail:?} found={} lines_after={after:?} same_turn_blocks={:?} after={after_types:?}", pos.is_some(), idx.map(|i| turns[i].blocks.len()));
                }
            }
            if matches!(s.body(), SubagentBody::InProgress(_)) { let last_line = text.lines().last().unwrap_or(""); let v: serde_json::Value = serde_json::from_str(last_line).unwrap_or_default(); eprintln!("inprogress: type={:?} stop={:?} mtime_age_s={:?}", v.get("type"), v.pointer("/message/stop_reason"), fs::metadata(p).and_then(|m| m.modified()).ok().and_then(|t| t.elapsed().ok()).map(|d| d.as_secs())); }
        }
        {
            let turns = parse(&text);
            let first_user = turns.iter().find(|t| t.role == Role::User && t.blocks.iter().any(|b| matches!(b, Block::Text(_))));
            let meta_first = first_user.map(|t| t.is_meta);
            let b = s.body().text();
            let prompt_in_body = first_user.and_then(|t| t.blocks.iter().find_map(|b| match b { Block::Text(x) => Some(x.trim().lines().next().unwrap_or("").to_owned()), _ => None })).filter(|l| l.chars().count() > 30).is_some_and(|l| b.contains(&l));
            if b.starts_with("> ") || prompt_in_body || meta_first == Some(true) { eprintln!("spawn: starts_prompt={} prompt_in_body={} first_user_meta={:?}", b.starts_with("> "), prompt_in_body, meta_first); }
            let over = telegram_len(&s.render()) > TELEGRAM_TEXT_LIMIT; if over { *kinds.entry("over4096").or_default() += 1; }
        }
        let r = s.render();
        if r.starts_with("↳ agent ") { header_agent += 1 } else { meta_type += 1 }
        for t in thinking_texts(&text) { if r.contains(&t) { thinking_leak += 1; break; } }
        // parent session file: <dir>/<session>/subagents/agent-x.jsonl -> <dir>/<session>.jsonl
        let sess = p.parent().unwrap().parent().unwrap();
        by_parent.entry(sess.with_extension("jsonl")).or_default().push(s);
    }
    println!("subagents={} kinds={:?} header_from_type={} header_agent={} thinking_leak={}", subs.len(), kinds, meta_type, header_agent, thinking_leak);
    let (mut brief_eq, mut full_eq, mut n, mut sidechain_parents, mut strip_eq, mut embedded, mut par_think_leak) = (0,0,0,0,0,0,0);
    for p in &parents {
        let Ok(text) = fs::read_to_string(p) else { continue };
        let turns = parse(&text);
        n += 1;
        if turns.iter().any(|t| t.is_sidechain) { sidechain_parents += 1; }
        if render_brief(&turns) == render_brief_with_subagents(&turns, &[]) { brief_eq += 1 }
        if render_full(&turns) == render_full_with_subagents(&turns, &[]) { full_eq += 1 }
        if let Some(subs) = by_parent.get(p) {
            let full = render_full_with_subagents(&turns, subs);
            let plain = render_full(&turns);
            embedded += full.matches('↳').count().min(1);
            // strip body lines: remove body text blocks that were inserted
            let mut stripped = full.clone();
            for s in subs { let b = s.body().text(); if b.is_empty() { continue }
                let ind: String = b.lines().map(|l| format!("  {l}")).collect::<Vec<_>>().join("\n");
                stripped = stripped.replace(&format!("\n{ind}"), ""); }
            // header may differ from plain (meta overrides): normalise by comparing line counts
            if stripped.lines().count() == plain.lines().count() { strip_eq += 1 }
            let sub_text: String = subs.iter().map(|s| s.body().text()).collect();
            let _ = sub_text;
            for t in thinking_texts(&text) { if full.contains(&t) { par_think_leak += 1; let in_plain = plain.contains(&t); let in_body = subs.iter().any(|s| s.body().text().contains(&t)); let occ_raw = text.matches(&t).count(); eprintln!("leak: in_plain={in_plain} in_body={in_body} raw_occurrences={occ_raw} len={}", t.chars().count()); break; } }
        }
    }
    println!("parents={} brief_eq={} full_eq={} sidechain_parents={} with_subs_embedded={} strip_linecount_eq={} parent_thinking_leak={}", n, brief_eq, full_eq, sidechain_parents, embedded, strip_eq, par_think_leak);
}
