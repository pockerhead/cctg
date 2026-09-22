//! Runs Subagent::new over every real subagent transcript (no report, no last message) and reports
//! body kinds, UTF-16 sizes and timings. Evidence only; never copied into the repo.
use std::{collections::BTreeMap, fs, path::PathBuf, time::Instant};
use transcript::{Subagent, SubagentBody, SubagentInput, telegram_len};

fn main() {
    let root = PathBuf::from(std::env::var("USERPROFILE").unwrap()).join(".claude").join("projects");
    let mut kinds: BTreeMap<&str, usize> = BTreeMap::new();
    let mut sizes = Vec::new();
    let mut over = 0;
    let mut n = 0;
    let mut parts: Vec<(usize, usize, usize)> = Vec::new();
    let start = Instant::now();
    for project in fs::read_dir(&root).unwrap().flatten() {
        for session in fs::read_dir(project.path()).into_iter().flatten().flatten() {
            let dir = session.path().join("subagents");
            for file in fs::read_dir(&dir).into_iter().flatten().flatten() {
                let path = file.path();
                let name = path.file_name().unwrap().to_string_lossy().into_owned();
                if !name.ends_with(".jsonl") { continue; }
                let id = name.trim_start_matches("agent-").trim_end_matches(".jsonl").to_owned();
                let jsonl = fs::read_to_string(&path).unwrap_or_default();
                let meta = fs::read_to_string(path.with_extension("meta.json")).ok();
                let block = Subagent::new(SubagentInput {
                    agent_id: &id,
                    meta: meta.as_deref(),
                    transcript: Some(&jsonl),
                    ..SubagentInput::default()
                });
                let kind = match block.body() {
                    SubagentBody::Report(_) => "report",
                    SubagentBody::Transcript(_) => "transcript",
                    SubagentBody::LastMessage(_) => "last",
                    SubagentBody::InProgress(_) => "in_progress",
                    SubagentBody::Empty => "empty",
                };
                *kinds.entry(kind).or_default() += 1;
                let len = telegram_len(&block.render());
                if len > 4096 { over += 1; }
                sizes.push(len);
                let text = block.body().text();
                let tools = text.lines().filter(|l| l.starts_with("• ") || l.starts_with("↳ ")).count();
                let tool_chars: usize = text.lines().filter(|l| l.starts_with("• ") || l.starts_with("↳ ")).map(|l| telegram_len(l) + 1).sum();
                parts.push((tools, tool_chars, len - tool_chars.min(len)));
                if !block.render().starts_with("↳ ") { println!("BAD header {id}"); }
                n += 1;
            }
        }
    }
    sizes.sort();
    let mut t: Vec<usize> = parts.iter().map(|p| p.0).collect(); t.sort();
    let mut tc: Vec<usize> = parts.iter().map(|p| p.1).collect(); tc.sort();
    let mut rest: Vec<usize> = parts.iter().map(|p| p.2).collect(); rest.sort();
    println!("tool lines p50 {} p90 {} max {}", t[n/2], t[n*9/10], t[n-1]);
    println!("tool-line utf16 p50 {} p90 {} max {}", tc[n/2], tc[n*9/10], tc[n-1]);
    println!("non-tool utf16 (header+texts+prompts) p50 {} p90 {} max {} over4096 {}", rest[n/2], rest[n*9/10], rest[n-1], rest.iter().filter(|&&r| r > 4096).count());
    println!("files {n} in {:?}", start.elapsed());
    println!("body kinds {kinds:?}");
    println!("block utf16 len p50 {} p90 {} max {} over4096 {over}", sizes[n / 2], sizes[n * 9 / 10], sizes[n - 1]);
}
