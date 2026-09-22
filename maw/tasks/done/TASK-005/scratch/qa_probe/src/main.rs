// QA probe (TASK-005): synthetic acceptance checks + read-only real-data scan.
// Real-data mode prints only counts and file names, never record content.
use serde_json::{json, Value};
use std::cell::Cell;
use std::path::{Path, PathBuf};
use transcript::{ai_title, parse, Block, Role};

thread_local!(static FAILS: Cell<usize> = const { Cell::new(0) });
fn check(name: &str, ok: bool) {
    println!("{} {}", if ok { "PASS" } else { "FAIL" }, name);
    if !ok {
        FAILS.with(|f| f.set(f.get() + 1));
    }
}
fn texts(src: &str) -> Vec<String> {
    parse(src)
        .into_iter()
        .flat_map(|t| t.blocks)
        .map(|b| match b {
            Block::Text(s) => format!("T:{s}"),
            Block::ToolUse { id, name, .. } => format!("U:{id}:{name}"),
            Block::ToolResult { tool_use_id, content, is_error, agent_id } => {
                format!("R:{tool_use_id}:{content}:{is_error}:{agent_id:?}")
            }
        })
        .collect()
}
fn rec(kind: &str, content: Value) -> String {
    json!({"type": kind, "message": {"content": content}}).to_string()
}

fn synthetic() {
    // 1. per-item tolerance after the null fix: other wrong types still drop only the item
    for bad in [
        json!({"type":"text","text":5}),
        json!({"type":"text","text":true}),
        json!({"type":"text","text":{}}),
        json!({"type":"text","text":["a"]}),
        json!({"type":"tool_use","id":5,"name":"Bash"}),
        json!({"type":"tool_use","id":"x","name":[]}),
        json!({"type":"tool_result","tool_use_id":{}}),
    ] {
        let src = rec("assistant", json!([bad.clone(), {"type":"text","text":"keep"}]));
        check(&format!("wrong-typed item dropped alone: {bad}"), texts(&src) == vec!["T:keep"]);
    }
    let src = rec(
        "assistant",
        json!([{"type":"text","text":null},{"type":"tool_use","id":null,"name":null,"input":null},{"type":"tool_result","tool_use_id":null,"content":null,"is_error":null}]),
    );
    check("nulls become defaults", texts(&src) == vec!["T:", "U::", "R:::false:None"]);

    // 2. thinking leak attempts
    let secret = "QA-SECRET-THINK";
    let attempts = vec![
        rec("assistant", json!([{"type":"thinking","thinking":secret,"text":secret,"signature":secret}])),
        rec("assistant", json!([{"type":"redacted_thinking","data":secret}])),
        rec("assistant", json!([{"type":"text","text":null,"thinking":secret}])),
        rec("assistant", json!([{"type":"text","text":"ok","thinking":secret,"signature":secret}])),
        rec(
            "user",
            json!([{"type":"tool_result","tool_use_id":"t","content":[{"type":"thinking","thinking":secret,"text":secret},{"type":"text","text":"r"}]}]),
        ),
        rec("assistant", json!([{"type":"Thinking","thinking":secret}])),
        rec("assistant", json!([{"type":"THINKING","text":secret}])),
        json!({"type":"assistant","thinking":secret,"message":{"thinking":secret,"content":[{"type":"text","text":"x"}]}}).to_string(),
        json!({"type":"ai-title","aiTitle":"t","message":{"content":secret}}).to_string(),
    ];
    for (i, a) in attempts.iter().enumerate() {
        let out = format!("{:?}{:?}", parse(a), ai_title(a));
        check(&format!("thinking leak attempt #{i}"), !out.contains(secret));
    }

    // 3. record type allowlist is exact
    for k in ["User", "ASSISTANT", "system", "attachment", "summary", "ai-title", "user ", ""] {
        check(&format!("type {k:?} skipped"), parse(&rec(k, json!("hello"))).is_empty());
    }

    // 4. neighbours survive: unknown record, unknown block, truncated tail
    let src = [
        rec("user", json!("a")),
        json!({"type":"weird-new-type","x":1}).to_string(),
        rec("assistant", json!([{"type":"mystery","z":1},{"type":"text","text":"b"}])),
        "{\"type\":\"assistant\",\"message\":{\"content\":[{\"type\":\"te".to_string(),
    ]
    .join("\n");
    check("neighbours survive + truncated tail", texts(&src) == vec!["T:a", "T:b"]);
    let src = format!("{}\n{{\"type\":\"user\",\"message\":{{\"content\":\"При", rec("user", json!("a")));
    check("truncated multibyte tail", texts(&src) == vec!["T:a"]);

    // 5. empty / ignored-only
    check("empty", parse("").is_empty() && ai_title("").is_none());
    check("ws only", parse(" \r\n\t\n").is_empty());
    let ign = [
        json!({"type":"mode"}),
        json!({"type":"file-history-snapshot","snapshot":{}}),
        json!({"type":"ai-title","aiTitle":"X"}),
        json!({"type":"attachment","message":{"content":"att"}}),
    ]
    .iter()
    .map(|v| v.to_string())
    .collect::<Vec<_>>()
    .join("\n");
    check("ignored only", parse(&ign).is_empty());

    // 6. both content shapes, isMeta flag
    let src = [
        json!({"type":"user","isMeta":true,"message":{"content":"meta s"}}).to_string(),
        rec("user", json!([{"type":"text","text":"arr"}])),
        rec("assistant", json!("astr")),
        rec("assistant", json!([{"type":"text","text":"aarr"}])),
    ]
    .join("\n");
    let t = parse(&src);
    check(
        "shapes+meta",
        t.len() == 4
            && t[0].is_meta
            && !t[1].is_meta
            && t[2].role == Role::Assistant
            && texts(&src) == vec!["T:meta s", "T:arr", "T:astr", "T:aarr"],
    );
    check("empty string content -> one Text(\"\")", texts(&rec("user", json!(""))) == vec!["T:"]);

    // 7. ai_title first wins
    let src = [
        json!({"type":"ai-title","aiTitle":""}).to_string(),
        json!({"type":"ai-title","aiTitle":"First"}).to_string(),
        json!({"type":"ai-title","aiTitle":"Second"}).to_string(),
    ]
    .join("\n");
    check("ai_title first non-empty", ai_title(&src).as_deref() == Some("First") && parse(&src).is_empty());

    // 8. fuzz: LCG random bytes + fixture mutation
    let fixtures = [
        include_str!("../../../../../../../crates/transcript/tests/fixtures/tool_use_result.jsonl"),
        include_str!("../../../../../../../crates/transcript/tests/fixtures/thinking_ai_title.jsonl"),
    ];
    let mut s: u64 = 0x9E37_79B9_7F4A_7C15;
    let mut next = || {
        s = s.wrapping_mul(6364136223846793005).wrapping_add(1442695040888963407);
        (s >> 33) as u32
    };
    let alphabet = b"{}[]\":,\\\n0anull";
    let r = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        for i in 0..20000usize {
            let bytes: Vec<u8> = if i % 2 == 0 {
                (0..(next() % 300)).map(|_| (next() % 256) as u8).collect()
            } else {
                let mut b = fixtures[(i / 2) % 2].as_bytes().to_vec();
                for _ in 0..(1 + next() % 8) {
                    let p = next() as usize % b.len();
                    b[p] = alphabet[next() as usize % alphabet.len()];
                }
                b
            };
            let st = String::from_utf8_lossy(&bytes);
            let _ = ai_title(&st);
            let out = format!("{:?}", parse(&st));
            if out.contains("SECRET-THINKING-MARKER") || out.contains("SECRET-SIGNATURE-MARKER") {
                println!("mutated fixture leaked marker at iter {i}");
                return false;
            }
        }
        true
    }));
    check("20000 fuzz/mutation inputs: no panic, no thinking marker in output", matches!(r, Ok(true)));
}

fn walk(dir: &Path, out: &mut Vec<PathBuf>) {
    if let Ok(rd) = std::fs::read_dir(dir) {
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
fn collect_secret(v: &Value, out: &mut Vec<String>) {
    match v {
        Value::String(s) if s.chars().count() >= 24 => out.push(s.clone()),
        Value::Array(a) => a.iter().for_each(|x| collect_secret(x, out)),
        Value::Object(o) => o.iter().filter(|(k, _)| *k != "type").for_each(|(_, x)| collect_secret(x, out)),
        _ => {}
    }
}
fn real() {
    let home = std::env::var("USERPROFILE").unwrap_or_default();
    let mut files = Vec::new();
    walk(&Path::new(&home).join(".claude").join("projects"), &mut files);
    let (mut hits, mut secrets_total, mut turns_total, mut panics, mut titles) = (0usize, 0usize, 0usize, 0usize, 0usize);
    for f in &files {
        let Ok(src) = std::fs::read_to_string(f) else { continue };
        let mut secrets = Vec::new();
        for line in src.lines() {
            let Ok(v) = serde_json::from_str::<Value>(line) else { continue };
            if let Some(Value::Array(items)) = v.pointer("/message/content") {
                for it in items {
                    if matches!(it.get("type").and_then(Value::as_str), Some("thinking" | "redacted_thinking")) {
                        collect_secret(it, &mut secrets)
                    }
                }
            }
        }
        let Ok((turns, title)) = std::panic::catch_unwind(|| (parse(&src), ai_title(&src))) else {
            panics += 1;
            continue;
        };
        if title.is_some() {
            titles += 1
        }
        turns_total += turns.len();
        secrets_total += secrets.len();
        for sct in &secrets {
            let probe: String = sct.chars().take(60).collect();
            for (i, t) in turns.iter().enumerate() {
                for b in &t.blocks {
                    let (k, s) = match b {
                        Block::Text(s) => ("Text", s.clone()),
                        Block::ToolUse { input, id, name } => ("ToolUse", format!("{id}{name}{input}")),
                        Block::ToolResult { content, tool_use_id, .. } => ("ToolResult", format!("{tool_use_id}{content}")),
                    };
                    if s.contains(probe.as_str()) {
                        hits += 1;
                        println!(
                            "HIT file={} turn={i} role={:?} block={k} meta={} sidechain={}",
                            f.file_name().unwrap_or_default().to_string_lossy(),
                            t.role,
                            t.is_meta,
                            t.is_sidechain
                        );
                    }
                }
            }
        }
    }
    println!(
        "real: files={} turns={} files_with_title={} thinking_strings={} hits={} panics={}",
        files.len(),
        turns_total,
        titles,
        secrets_total,
        hits,
        panics
    );
}

fn main() {
    synthetic();
    if std::env::args().any(|a| a == "--real") {
        real()
    }
    let f = FAILS.with(|f| f.get());
    println!("FAILS={f}");
    std::process::exit(if f == 0 { 0 } else { 1 });
}
