// QA probe (TASK-006). Independent checks: splitter fuzz vs grapheme_indices, render edge cases,
// and a read-only pass over ~/.claude/projects. Prints only counts for real data, never content.
use std::{
    collections::HashSet,
    fs,
    path::{Path, PathBuf},
    time::Instant,
};
use transcript::*;
use unicode_segmentation::UnicodeSegmentation;

struct Rng(u64);
impl Rng {
    fn next(&mut self) -> u64 {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0 << 17;
        self.0
    }
    fn below(&mut self, n: u64) -> u64 {
        self.next() % n
    }
}

const PIECES: &[&str] = &[
    "a", "word ", "б", "\n", "\n\n", "\r\n", " ", "\t", "🚀", "👨‍👩‍👧‍👦", "🇷🇺", "🇺",
    "e\u{301}\u{302}\u{303}", "👍🏽", "1\u{fe0f}\u{20e3}", "क्षि", "한",
    "\u{1100}\u{1161}\u{11a8}", "\u{200d}", "\u{301}", "   ", "x\u{301}", "\u{a0}",
];

/// Violations of the splitter invariants for `text`.
fn check(text: &str, opts: SplitOptions) -> Vec<String> {
    let mut errs = vec![];
    let r = split_for_telegram(text, opts);
    if r != split_for_telegram(text, opts) {
        errs.push("nondeterministic".into());
    }
    let bounds: HashSet<usize> = text
        .grapheme_indices(true)
        .map(|(i, _)| i)
        .chain([text.len()])
        .collect();
    let max_g = text
        .graphemes(true)
        .map(|g| g.encode_utf16().count())
        .max()
        .unwrap_or(0);
    let mut pos = 0usize;
    for c in &r.chunks {
        let n = c.encode_utf16().count();
        if n > 4096 {
            errs.push(format!("chunk len {n}"));
        }
        if c.trim().is_empty() {
            errs.push("blank chunk".into());
        }
        let Some(off) = text[pos..].find(c.as_str()) else {
            errs.push("not a slice".into());
            break;
        };
        if !text[pos..pos + off].trim().is_empty() {
            errs.push("lost text".into());
        }
        let start = pos + off;
        let end = start + c.len();
        if max_g <= 4096 && (!bounds.contains(&start) || !bounds.contains(&end)) {
            errs.push(format!("grapheme cut at {start}..{end}"));
        }
        pos = end;
    }
    if !text[pos..].trim().is_empty() {
        errs.push("lost tail".into());
    }
    if r.prefer_file != (r.chunks.len() > opts.max_chunks) {
        errs.push("prefer_file".into());
    }
    errs
}

fn fuzz() {
    let mut rng = Rng(0x9e3779b97f4a7c15);
    let mut fails = 0;
    let mut total_chunks = 0;
    for case in 0..3000 {
        let target = 1000 + rng.below(20000) as usize;
        let mut s = String::new();
        let bias = rng.below(PIECES.len() as u64 + 3) as usize;
        while s.len() < target {
            let p = if bias < PIECES.len() && rng.below(4) != 0 {
                PIECES[bias]
            } else {
                PIECES[rng.below(PIECES.len() as u64) as usize]
            };
            s.push_str(p);
        }
        let errs = check(&s, SplitOptions::default());
        total_chunks += split_for_telegram(&s, SplitOptions::default()).chunks.len();
        if !errs.is_empty() {
            fails += 1;
            if fails < 5 {
                println!("FUZZ case {case} bias {bias}: {:?}", &errs[..errs.len().min(3)]);
            }
        }
    }
    let mut sweep_fail = 0;
    for p in PIECES {
        for k in 4080..4100 {
            for tail in ["", "z", " z", "\n\n"] {
                let s = format!("{}{}{}{}", "a".repeat(k), p.repeat(3), tail, "b".repeat(50));
                let e = check(&s, SplitOptions::default());
                if !e.is_empty() {
                    sweep_fail += 1;
                    if sweep_fail < 5 {
                        println!("SWEEP {p:?} {k} {tail:?}: {e:?}");
                    }
                }
                let s2 = format!("{} {}{}{}", "a".repeat(3000), "a".repeat(k - 3000), p.repeat(3), tail);
                let e = check(&s2, SplitOptions::default());
                if !e.is_empty() {
                    sweep_fail += 1;
                    if sweep_fail < 5 {
                        println!("SWEEP2 {p:?} {k}: {e:?}");
                    }
                }
            }
        }
    }
    let odd = format!("🇺{}", "🇷🇺".repeat(3000));
    let e = check(&odd, SplitOptions::default());
    if !e.is_empty() {
        println!("RI odd: {:?}", &e[..e.len().min(3)]);
        sweep_fail += 1;
    }
    let huge = format!("x{}y", "\u{301}".repeat(9000));
    let r = split_for_telegram(&huge, SplitOptions::default());
    let ok = r.chunks.iter().all(|c| c.encode_utf16().count() <= 4096) && r.chunks.concat() == huge;
    println!("oversized grapheme chunks={} ok={ok}", r.chunks.len());
    println!(
        "empty={:?} ws={:?} max0_prefer={:?}",
        split_for_telegram("", SplitOptions::default()),
        split_for_telegram(" \n\t ", SplitOptions::default()),
        split_for_telegram("hi", SplitOptions { max_chunks: 0 }).prefer_file
    );
    let emoji50 = "a".repeat(4095) + &"🚀".repeat(12000);
    let r1 = split_for_telegram(&emoji50, SplitOptions::default());
    let r2 = split_for_telegram(&emoji50, SplitOptions::default());
    println!(
        "50KB emoji: bytes={} chunks={} prefer_file={} det={} first_len={} second_starts_with_rocket={}",
        emoji50.len(),
        r1.chunks.len(),
        r1.prefer_file,
        r1 == r2,
        r1.chunks[0].encode_utf16().count(),
        r1.chunks[1].starts_with('🚀')
    );
    println!("fuzz fails={fails}/3000 total_chunks={total_chunks} sweep_fails={sweep_fail}");
}

fn line(v: serde_json::Value) -> String {
    v.to_string() + "\n"
}
fn asst(content: serde_json::Value, stop: serde_json::Value) -> String {
    line(serde_json::json!({"type":"assistant","message":{"content":content,"stop_reason":stop}}))
}
fn user(content: serde_json::Value, meta: bool) -> String {
    line(serde_json::json!({"type":"user","isMeta":meta,"message":{"content":content}}))
}

fn render_cases() {
    use serde_json::json;
    let secret = "SECRETTHINK";
    let mut j = user(json!("hello"), false);
    j += &asst(json!([{"type":"thinking","thinking":secret,"signature":"x"}]), json!(null));
    j += &asst(json!([{"type":"redacted_thinking","data":secret}]), json!(null));
    j += &asst(json!([{"type":"thinking","thinking":secret},{"type":"text","text":"answer"}]), json!("end_turn"));
    j += &asst(json!([{"type":"text","thinking":secret}]), json!("end_turn"));
    j += &asst(json!([{"type":"text","text":null,"thinking":secret}]), json!("end_turn"));
    j += &asst(json!([{"type":"thinking","text":secret}]), json!("end_turn"));
    j += &user(json!([{"type":"tool_result","tool_use_id":"t","content":[{"type":"thinking","thinking":secret}]}]), false);
    let t = parse(&j);
    let (b, f) = (render_brief(&t), render_full(&t));
    println!("thinking leak brief={} full={}", b.contains(secret), f.contains(secret));
    println!("--- thinking case brief:\n{b}\n--- full:\n{f}\n---");

    let mut j = user(json!("go"), false);
    j += &asst(json!([{"type":"tool_use","id":"a1","name":"Bash","input":{"description":"line1\nline2\r\nline3","command":"x"}}]), json!("tool_use"));
    j += &asst(json!([{"type":"tool_use","id":"a2","name":"Agent","input":{"subagent_type":"Ex\nplore","description":"d\ne"}}]), json!("tool_use"));
    j += &line(json!({"type":"user","toolUseResult":{"agentId":"id\nwith\nnewline"},"message":{"content":[{"type":"tool_result","tool_use_id":"a2","content":"r"}]}}));
    j += &asst(json!([{"type":"tool_use","id":"a3","name":"Na\nme","input":{}}]), json!("tool_use"));
    j += &asst(json!([{"type":"tool_use","id":"a4","name":"Read","input":{"file_path":"\u{2028}p\u{2029}q\u{85}r"}}]), json!("tool_use"));
    let b = render_brief(&parse(&j));
    println!("--- tool lines brief ({} lines; expected 1 prompt + 4 calls + marker = 6):\n{b}\n---", b.lines().count());

    let tail = |j: &str| {
        let t = parse(j);
        (render_brief(&t).ends_with("в работе…"), render_full(&t).ends_with("в работе…"))
    };
    let base = user(json!("q"), false);
    let done = asst(json!([{"type":"text","text":"done"}]), json!("end_turn"));
    println!("tail tool_use text: {:?}", tail(&(base.clone() + &asst(json!([{"type":"text","text":"let me check"}]), json!("tool_use")))));
    println!("tail end_turn: {:?}", tail(&(base.clone() + &done)));
    println!("tail null text, no tool after: {:?}", tail(&(base.clone() + &asst(json!([{"type":"text","text":"streaming"}]), json!(null)))));
    println!("tail max_tokens text: {:?}", tail(&(base.clone() + &asst(json!([{"type":"text","text":"cut"}]), json!("max_tokens")))));
    let slash = base.clone() + &done + &user(json!("<command-message>foo</command-message>\n<command-name>/foo</command-name>\n<command-args>6</command-args>"), false);
    println!("tail end_turn then typed slash command: {:?} brief_shows_slash={}", tail(&slash), render_brief(&parse(&slash)).contains("/foo"));
    let empty_ch = base.clone() + &done + &user(json!("<channel source=\"cctg\"></channel>"), true);
    println!("tail end_turn then empty channel msg: {:?} brief={:?}", tail(&empty_ch), render_brief(&parse(&empty_ch)));
    println!("tail thinking only after prompt: {:?}", tail(&(base.clone() + &asst(json!([{"type":"thinking","thinking":"x"}]), json!(null)))));

    let j = base.clone()
        + &asst(json!([{"type":"text","text":"INTERMEDIATE"}]), json!("tool_use"))
        + &asst(json!([{"type":"tool_use","id":"b","name":"Read","input":{"file_path":"f"}}]), json!("tool_use"))
        + &user(json!([{"type":"tool_result","tool_use_id":"b","content":"RESULTBODY"}]), false)
        + &asst(json!([{"type":"text","text":"FINAL"}]), json!("end_turn"));
    let t = parse(&j);
    let (b, f) = (render_brief(&t), render_full(&t));
    println!(
        "brief: inter={} final={} result={} input={} marker={} | full: inter={} result={} input={}",
        b.contains("INTERMEDIATE"), b.contains("FINAL"), b.contains("RESULTBODY"), b.contains("{\"file_path\""), b.contains("в работе"),
        f.contains("INTERMEDIATE"), f.contains("RESULTBODY"), f.contains("{\"file_path\"")
    );
    for sr in [json!(null), json!(1), json!({"a":1}), json!(["end_turn"]), json!(true)] {
        let t = parse(&asst(json!([{"type":"text","text":"x"}]), sr.clone()));
        print!("stop {sr} -> n={} {:?}; ", t.len(), t.first().map(|t| t.stop_reason.clone()));
    }
    println!();
    let mixed = asst(json!([{"type":"text","text":5},{"type":"text","text":"ok"},{"type":"tool_use","id":null,"name":null,"input":null}]), json!("end_turn"));
    let t = parse(&mixed);
    println!("per-item tolerance: turns={} blocks={}", t.len(), t.first().map(|t| t.blocks.len()).unwrap_or(0));
    // slices render independently and concatenate
    let whole = base.clone() + &done + &user(json!("q2"), false) + &asst(json!([{"type":"text","text":"a2"}]), json!("end_turn"));
    let t = parse(&whole);
    println!("slice concat equal: {}", render_brief(&t) == format!("{}\n\n{}", render_brief(&t[..2]), render_brief(&t[2..])));
}

fn walk(dir: &Path, out: &mut Vec<PathBuf>) {
    if let Ok(rd) = fs::read_dir(dir) {
        for e in rd.flatten() {
            let p = e.path();
            if p.is_dir() {
                walk(&p, out)
            } else if p.extension().is_some_and(|x| x == "jsonl") {
                out.push(p)
            }
        }
    }
}

fn thinking_snips(src: &str) -> Vec<String> {
    let mut v = vec![];
    for l in src.lines() {
        let Ok(j) = serde_json::from_str::<serde_json::Value>(l) else { continue };
        if let Some(a) = j.pointer("/message/content").and_then(|c| c.as_array()) {
            for b in a {
                if b.get("type").and_then(|t| t.as_str()) == Some("thinking") {
                    if let Some(t) = b.get("thinking").and_then(|t| t.as_str()) {
                        let cs: Vec<char> = t.trim().chars().collect();
                        if cs.len() >= 200 {
                            v.push(cs[100..180].iter().collect());
                        }
                    }
                }
            }
        }
    }
    v
}

fn real() {
    let home = std::env::var("USERPROFILE").unwrap_or_default();
    let mut files = vec![];
    walk(&Path::new(&home).join(".claude").join("projects"), &mut files);
    let (mut bad, mut leaks, mut chunks, mut prefer, mut markers, mut bytes) = (0, 0, 0usize, 0, 0, 0usize);
    let mut slowest = (0u128, 0usize);
    let mut marker_after_end_turn = 0;
    let mut slash_hidden = 0;
    for p in &files {
        let Ok(src) = fs::read_to_string(p) else { continue };
        bytes += src.len();
        let t0 = Instant::now();
        let t = parse(&src);
        let b = render_brief(&t);
        let f = render_full(&t);
        let ms = t0.elapsed().as_millis();
        if ms > slowest.0 {
            slowest = (ms, src.len());
        }
        for text in [&b, &f] {
            let e = check(text, SplitOptions::default());
            if !e.is_empty() {
                bad += 1;
                println!("REAL split violation {:?}", &e[..e.len().min(2)]);
            }
            let r = split_for_telegram(text, SplitOptions::default());
            chunks += r.chunks.len();
            if r.prefer_file {
                prefer += 1;
            }
        }
        for s in &thinking_snips(&src) {
            if b.contains(s.as_str()) || f.contains(s.as_str()) {
                leaks += 1;
                let in_visible_block = t.iter().any(|turn| {
                    turn.blocks.iter().any(|bl| match bl {
                        Block::Text(x) => x.contains(s.as_str()),
                        Block::ToolResult { content, .. } => content.contains(s.as_str()),
                        Block::ToolUse { input, .. } => input.to_string().contains(s.as_str()),
                    })
                });
                println!("thinking hit: snippet also present in a parsed non-thinking block = {in_visible_block}; roles = {:?}",
                    t.iter().filter(|turn| turn.blocks.iter().any(|bl| matches!(bl, Block::Text(x) if x.contains(s.as_str())))).map(|turn| (turn.role, turn.is_meta)).collect::<Vec<_>>());
            }
        }
        if b.ends_with("в работе…") {
            markers += 1;
            if t.last().map(|x| (x.role, x.stop_reason.as_deref())) == Some((Role::Assistant, Some("end_turn"))) {
                marker_after_end_turn += 1;
            }
        }
        for turn in &t {
            if turn.role == Role::User && !turn.is_meta {
                for bl in &turn.blocks {
                    if let Block::Text(x) = bl {
                        if x.trim_start().starts_with("<command-") && x.contains("<command-name>/") {
                            slash_hidden += 1;
                        }
                    }
                }
            }
        }
    }
    println!(
        "REAL files={} MB={} split_violations={bad} thinking_snippet_hits={leaks} chunks={chunks} prefer_file={prefer} brief_markers={markers} marker_after_final_end_turn={marker_after_end_turn} typed_slash_commands_hidden_in_brief={slash_hidden} slowest_file_ms={} ({} bytes)",
        files.len(),
        bytes / 1_000_000,
        slowest.0,
        slowest.1
    );
}

fn main() {
    let arg = std::env::args().nth(1).unwrap_or_default();
    if arg.is_empty() || arg == "fuzz" {
        fuzz();
    }
    if arg.is_empty() || arg == "render" {
        render_cases();
    }
    if arg.is_empty() || arg == "real" {
        real();
    }
}
