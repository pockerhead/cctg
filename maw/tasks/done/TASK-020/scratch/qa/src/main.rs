// QA TASK-020: differential brief/full over all real jsonl. Prints counts and command names only.
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

fn walk(dir: &Path, out: &mut Vec<PathBuf>) {
    let Ok(rd) = std::fs::read_dir(dir) else { return };
    for e in rd.flatten() {
        let p = e.path();
        if p.is_dir() { walk(&p, out); } else if p.extension().is_some_and(|x| x == "jsonl") { out.push(p); }
    }
}

fn first_tag(t: &str) -> &'static str {
    if t.starts_with("<command-name>") { "name-first" }
    else if t.starts_with("<command-message>") { "message-first" }
    else { "other" }
}

fn main() {
    let home = std::env::var("USERPROFILE").unwrap();
    let root = Path::new(&home).join(".claude").join("projects");
    let mut files = Vec::new();
    walk(&root, &mut files);
    let mut c: BTreeMap<String, usize> = BTreeMap::new();
    let mut names: BTreeMap<String, usize> = BTreeMap::new();
    let mut inc = |c: &mut BTreeMap<String, usize>, k: String| *c.entry(k).or_default() += 1;
    let (mut lines_total, mut file_diff_brief) = (0usize, 0usize);
    for f in &files {
        let Ok(data) = std::fs::read_to_string(f) else { inc(&mut c, "file-unreadable".into()); continue };
        // whole-file render: must not panic; count files whose brief differs
        let nb = transcript::render_brief(&transcript::parse(&data));
        let ob = transcript_old::render_brief(&transcript_old::parse(&data));
        let _ = transcript::render_full(&transcript::parse(&data));
        if nb != ob { file_diff_brief += 1; }
        for line in data.lines() {
            lines_total += 1;
            let Ok(v) = serde_json::from_str::<serde_json::Value>(line) else { continue };
            if v.get("type").and_then(|t| t.as_str()) != Some("user") { continue; }
            let meta = v.get("isMeta").and_then(|x| x.as_bool()).unwrap_or(false);
            let content = &v["message"]["content"];
            let texts: Vec<String> = match content {
                serde_json::Value::String(s) => vec![s.clone()],
                serde_json::Value::Array(a) => a.iter().filter(|b| b["type"] == "text").filter_map(|b| b["text"].as_str().map(str::to_owned)).collect(),
                _ => vec![],
            };
            let turns_new = transcript::parse(line);
            let turns_old = transcript_old::parse(line);
            let (nb, ob) = (transcript::render_brief(&turns_new), transcript_old::render_brief(&turns_old));
            let (nf, of) = (transcript::render_full(&turns_new), transcript_old::render_full(&turns_old));
            let has_cmd = texts.iter().any(|t| t.contains("<command-name>"));
            let shape = if matches!(content, serde_json::Value::String(_)) { "str" } else { "arr" };
            if has_cmd {
                let tag = texts.iter().map(|t| first_tag(t.trim())).next().unwrap_or("none");
                let key = format!("cmd-containing meta={meta} shape={shape} first={tag} brief_shown={} old_brief_shown={}", !nb.is_empty(), !ob.is_empty());
                inc(&mut c, key);
                if !meta && !nb.is_empty() {
                    // command name only: first token after "> "
                    let first = nb.lines().next().unwrap_or("");
                    let name = first.trim_start_matches("> ").split(' ').next().unwrap_or("").to_owned();
                    let multi = nb.lines().count() > 2;
                    inc(&mut names, format!("{name} [{tag}] brief_lines>2={multi} full_eq_brief={}", nf == nb));
                }
            }
            if nb != ob && !has_cmd { inc(&mut c, format!("UNEXPECTED brief diff (no command-name) meta={meta} shape={shape}")); }
            if nf != of && !has_cmd { inc(&mut c, format!("UNEXPECTED full diff (no command-name) meta={meta} shape={shape}")); }
            if nb != ob && has_cmd && meta { inc(&mut c, "UNEXPECTED brief diff on meta record".into()); }
            // service prefixes other than command ones: must stay hidden in brief
            for t in &texts {
                let t = t.trim();
                for p in ["<task-notification>", "<local-command-stdout>", "<local-command-stderr>", "<local-command-caveat>", "<bash-input>", "<bash-stdout>", "<bash-stderr>", "This session is being continued"] {
                    if t.starts_with(p) {
                        inc(&mut c, format!("service {p} meta={meta} brief_shown={} old_shown={}", !nb.is_empty(), !ob.is_empty()));
                    }
                }
                if !meta && t.starts_with("<command-message>") && !t.contains("<command-name>") {
                    inc(&mut c, format!("message-only (no name) brief_shown={}", !nb.is_empty()));
                }
            }
        }
    }
    println!("files={} lines={} files_with_brief_diff={}", files.len(), lines_total, file_diff_brief);
    for (k, v) in &c { println!("{v:6}  {k}"); }
    println!("--- command names (non-meta, shown) ---");
    for (k, v) in &names { println!("{v:6}  {k}"); }
}
