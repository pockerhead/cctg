//! QA TASK-007: independent synthetic checks plus a read-only sweep over real transcripts.
//! Real content is never printed: only counts and file indices.

use serde_json::{Value, json};
use std::path::{Path, PathBuf};
use transcript::{
    Subagent, SubagentBody, SubagentInput, parse, parse_subagent_meta, render_brief,
    render_brief_with_subagents, render_full, render_full_with_subagents,
};

const MARK: &str = "в работе…";

fn rec(kind: &str, content: Value, side: bool, meta: bool, stop: Option<&str>) -> String {
    let mut r = json!({
        "type": kind, "isSidechain": side, "isMeta": meta,
        "message": {"role": kind, "content": content, "stop_reason": stop},
    });
    if kind == "user" {
        r["message"].as_object_mut().unwrap().remove("stop_reason");
    }
    format!("{r}\n")
}
fn user(text: &str, side: bool) -> String {
    rec("user", json!(text), side, false, None)
}
fn meta_user(text: &str, side: bool) -> String {
    rec("user", json!(text), side, true, None)
}
fn say(text: &str, side: bool, stop: Option<&str>) -> String {
    rec("assistant", json!([{"type":"text","text":text}]), side, false, stop)
}
fn think(text: &str, side: bool) -> String {
    rec("assistant", json!([{"type":"thinking","thinking":text,"signature":"SIG-QA"}]), side, false, None)
}
fn call(id: &str, name: &str, input: Value, side: bool) -> String {
    rec("assistant", json!([{"type":"tool_use","id":id,"name":name,"input":input}]), side, false, Some("tool_use"))
}
fn result(id: &str, text: &str, side: bool, agent: Option<&str>) -> String {
    let mut r = json!({
        "type":"user","isSidechain":side,
        "message":{"role":"user","content":[{"type":"tool_result","tool_use_id":id,"content":text}]},
    });
    if let Some(a) = agent {
        r["toolUseResult"] = json!({"agentId": a, "status": "async_launched"});
    }
    format!("{r}\n")
}

struct T {
    fails: usize,
}
impl T {
    fn check(&mut self, name: &str, ok: bool, detail: impl std::fmt::Display) {
        if ok {
            println!("PASS {name}");
        } else {
            self.fails += 1;
            println!("FAIL {name}: {detail}");
        }
    }
    fn note(&self, name: &str, detail: impl std::fmt::Display) {
        println!("NOTE {name}: {detail}");
    }
}

fn sub(id: &str, transcript: Option<&str>, last: Option<&str>, report: Option<&str>) -> Subagent {
    Subagent::new(SubagentInput {
        agent_id: id,
        agent_type: Some("Explore"),
        transcript,
        last_assistant_message: last,
        report,
        ..SubagentInput::default()
    })
}

fn parent_with_agent(prompt: &str) -> String {
    let mut p = String::new();
    p += &user("QA parent prompt", false);
    p += &think("QA-PARENT-THINKING", false);
    p += &call("tu1", "Agent", json!({"description":"Look around","prompt":prompt,"subagent_type":"Explore"}), false);
    p += &result("tu1", "Async agent launched.", false, Some("aqa1"));
    p += &say("QA parent answer", false, Some("end_turn"));
    p
}

fn synthetic(t: &mut T) {
    // subagent transcript: spawn prompt, meta reminder, thinking, tool with result, final text
    let mut s = String::new();
    s += &user("QA-SPAWN-PROMPT do the thing", true);
    s += &meta_user("<system-reminder>QA-REMINDER</system-reminder>", true);
    s += &think("QA-SUB-THINKING", true);
    s += &call("st1", "Bash", json!({"command":"QA-CMD-INPUT","description":"List files"}), true);
    s += &result("st1", "QA-RESULT-CONTENT", true, None);
    s += &say("QA final answer", true, Some("end_turn"));

    let parent = parse(&parent_with_agent("QA-SPAWN-PROMPT do the thing"));
    let block = sub("aqa1", Some(&s), Some("QA final answer"), None);
    t.check(
        "S1 body is Transcript when hook text equals final answer",
        matches!(block.body(), SubagentBody::Transcript(b) if b == "• Bash: List files\nQA final answer"),
        format!("{:?}", block.body()),
    );
    let brief = render_brief_with_subagents(&parent, std::slice::from_ref(&block));
    let full = render_full_with_subagents(&parent, std::slice::from_ref(&block));
    for hidden in ["QA-SUB-THINKING", "QA-PARENT-THINKING", "SIG-QA", "QA-REMINDER", "QA-CMD-INPUT", "QA-RESULT-CONTENT"] {
        t.check(&format!("S2 full parent hides {hidden}"), !full.contains(hidden), &full);
        t.check(&format!("S2 brief parent hides {hidden}"), !brief.contains(hidden), &brief);
        t.check(&format!("S2 block hides {hidden}"), !block.render().contains(hidden), block.render());
    }
    t.check("S3 spawn prompt absent from block", !block.render().contains("QA-SPAWN-PROMPT"), block.render());
    t.check("S3 spawn prompt absent from brief parent", !brief.contains("QA-SPAWN-PROMPT"), &brief);
    if full.contains("QA-SPAWN-PROMPT") {
        t.note("S3 full parent", "spawn prompt visible via the parent's own Agent input line (pre-existing render_full behavior)");
    }
    t.check(
        "S4 brief parent has one header + indented brief body",
        brief.contains("↳ Explore aqa1: Look around\n  • Bash: List files\n  QA final answer\nQA parent answer"),
        &brief,
    );
    t.check("S4 exactly one ↳", brief.matches('↳').count() == 1 && full.matches('↳').count() == 1, &full);
    t.check(
        "S5 full parent minus body == render_full",
        full.replace("  • Bash: List files\n  QA final answer\n", "") == render_full(&parent),
        &full,
    );
    t.check("S5 empty slice == old renderers", render_brief_with_subagents(&parent, &[]) == render_brief(&parent)
        && render_full_with_subagents(&parent, &[]) == render_full(&parent), "");

    // S6 mixed sidechain turns in the parent slice, including a sidechain Agent call
    let mut mixed = parent_with_agent("p");
    mixed += &user("QA-SIDE-USER", true);
    mixed += &call("sx", "Agent", json!({"description":"QA-SIDE-AGENT"}), true);
    mixed += &result("sx", "x", true, Some("aside"));
    mixed += &say("QA-SIDE-TEXT", true, Some("end_turn"));
    let mixed = parse(&mixed);
    for full_mode in [false, true] {
        let out = if full_mode {
            render_full_with_subagents(&mixed, std::slice::from_ref(&block))
        } else {
            render_brief_with_subagents(&mixed, std::slice::from_ref(&block))
        };
        t.check(
            &format!("S6 sidechain never at parent top level (full={full_mode})"),
            !out.contains("QA-SIDE") && !out.contains("aside"),
            &out,
        );
    }
    t.check("S6 plain render_brief still shows sidechain (unchanged API)", render_brief(&mixed).contains("QA-SIDE-TEXT"), "");

    // S7 fallback order, independent matrix
    let prompt_only = user("x", true);
    let unfinished: String = s.lines().take(5).map(|l| format!("{l}\n")).collect();
    let cases: Vec<(&str, Option<&str>, Option<&str>, Option<&str>, SubagentBody)> = vec![
        ("report wins over all", Some(" R "), Some(&s), Some("other"), SubagentBody::Report("R".into())),
        ("blank report falls through", Some(" \n\t"), Some(&s), None, SubagentBody::Transcript("• Bash: List files\nQA final answer".into())),
        ("newer hook text beats finished transcript", None, Some(&s), Some("newer"), SubagentBody::LastMessage("newer".into())),
        ("padded hook text matches", None, Some(&s), Some("  QA final answer \r\n"), SubagentBody::Transcript("• Bash: List files\nQA final answer".into())),
        ("suffix is not a match", None, Some(&s), Some("answer"), SubagentBody::LastMessage("answer".into())),
        ("unfinished + hook text", None, Some(&unfinished), Some("L"), SubagentBody::LastMessage("L".into())),
        ("unfinished alone", None, Some(&unfinished), None, SubagentBody::InProgress(format!("• Bash: List files\n{MARK}"))),
        ("no transcript + hook", None, None, Some("L"), SubagentBody::LastMessage("L".into())),
        ("garbage transcript", None, Some("not json\n{\n"), None, SubagentBody::Empty),
        ("garbage transcript + hook", None, Some("not json\n"), Some("L"), SubagentBody::LastMessage("L".into())),
        ("nothing", None, None, Some("   "), SubagentBody::Empty),
        ("prompt only", None, Some(&prompt_only), None, SubagentBody::InProgress(MARK.into())),
    ];
    for (name, report, tr, last, want) in cases {
        let got = sub("a", tr, last, report).body().clone();
        t.check(&format!("S7 {name}"), got == want, format!("{got:?} != {want:?}"));
    }
    // CRLF transcript as written on Windows by other tools
    let crlf = s.replace('\n', "\r\n");
    let got = sub("a", Some(&crlf), Some("QA final answer"), None).body().clone();
    t.check("S7 CRLF transcript", matches!(got, SubagentBody::Transcript(_)), format!("{got:?}"));

    // S8 meta variants
    let m = parse_subagent_meta("\u{feff}  {\"agentType\": 5, \"description\": \"  d\\n  e \"}  ");
    t.check("S8 non-string type -> None", m.agent_type.is_none(), format!("{m:?}"));
    let b = Subagent::new(SubagentInput { agent_id: "a1", agent_type: Some("Hook"), meta: Some("{\"agentType\":5,\"description\":\" d\\n e \"}"), ..Default::default() });
    t.check("S8 type falls back to hook, description one-lined", b.render() == "↳ Hook a1: d e", b.render());
    let b = Subagent::new(SubagentInput { agent_id: "a1", agent_type: Some("Hook"), meta: Some("{\"agentType\":\"Meta\"}"), ..Default::default() });
    t.check("S8 meta type wins", b.render() == "↳ Meta a1", b.render());
    for bad in ["", "{", "null", "[1]", "\"s\"", "{\"agentType\":\"  \"}", "\u{feff}"] {
        let b = Subagent::new(SubagentInput { agent_id: "a1", meta: Some(bad), ..Default::default() });
        t.check(&format!("S8 broken meta {bad:?}"), b.render() == "↳ agent a1", b.render());
    }
    // meta description wins over call description in the parent line
    let bm = Subagent::new(SubagentInput { agent_id: "aqa1", meta: Some("{\"agentType\":\"Plan\",\"description\":\"From meta\"}"), last_assistant_message: Some("ok"), ..Default::default() });
    let out = render_brief_with_subagents(&parent, &[bm]);
    t.check("S8 meta overrides call header in parent", out.contains("↳ Plan aqa1: From meta\n  ok") && !out.contains("Look around"), &out);

    // S9 nested Agent inside a subagent: shown as one line, not expanded
    let mut n = String::new();
    n += &user("spawn", true);
    n += &call("n1", "Agent", json!({"subagent_type":"Plan","description":"inner","prompt":"QA-INNER-PROMPT"}), true);
    n += &result("n1", "QA-INNER-RESULT", true, Some("ainner"));
    n += &say("outer done", true, Some("end_turn"));
    let nb = sub("a", Some(&n), None, None);
    t.check("S9 nested agent one line", nb.body().text() == "↳ Plan ainner: inner\nouter done", nb.body().text());

    // S10 behavior change of plain render_brief for exotic agent ids (fixer's one_line in agent_header)
    let long_id = "a".repeat(130);
    for (label, id) in [("whitespace id", "a1 \n b2"), ("130-char id", long_id.as_str())] {
        let mut p = String::new();
        p += &call("tz", "Agent", json!({"subagent_type":"X"}), false);
        p += &result("tz", "r", false, Some(id));
        let new = render_brief(&parse(&p));
        let old = transcript_main::render_brief(&transcript_main::parse(&p));
        if new != old {
            t.note(&format!("S10 render_brief differs from main for {label}"), format!("{:?} vs {:?}", new.lines().next(), old.lines().next()));
        } else {
            println!("PASS S10 render_brief unchanged for {label}");
        }
    }

    // S11 spawn prompt preceded by a meta record (not seen in real data, 538/538 prompt first)
    let mut e = String::new();
    e += &meta_user("<system-reminder>r</system-reminder>", true);
    e += &user("QA-LATE-PROMPT", true);
    e += &say("done", true, Some("end_turn"));
    let eb = sub("a", Some(&e), None, None);
    if eb.body().text().contains("QA-LATE-PROMPT") {
        t.note("S11 spawn prompt after a meta record leaks into the body", eb.body().text());
    } else {
        println!("PASS S11 late spawn prompt not in body");
    }

    // S12 all old fixtures: new == main for brief and full; with_subagents(&[]) == plain
    let dir = Path::new("C:/Users/user/dev/cctg/crates/transcript/tests/fixtures");
    for entry in std::fs::read_dir(dir).unwrap() {
        let p = entry.unwrap().path();
        if p.extension().and_then(|x| x.to_str()) != Some("jsonl") {
            continue;
        }
        let text = std::fs::read_to_string(&p).unwrap();
        let (n, o) = (parse(&text), transcript_main::parse(&text));
        let name = p.file_name().unwrap().to_string_lossy().to_string();
        t.check(&format!("S12 {name} brief==main"), render_brief(&n) == transcript_main::render_brief(&o), "");
        t.check(&format!("S12 {name} full==main"), render_full(&n) == transcript_main::render_full(&o), "");
        if !n.iter().any(|x| x.is_sidechain) {
            t.check(&format!("S12 {name} with_subagents(&[])"), render_brief_with_subagents(&n, &[]) == render_brief(&n) && render_full_with_subagents(&n, &[]) == render_full(&n), "");
        }
    }
}

fn thinking_texts(jsonl: &str) -> Vec<String> {
    let mut out = Vec::new();
    for line in jsonl.lines() {
        let Ok(v) = serde_json::from_str::<Value>(line) else { continue };
        if let Some(items) = v.pointer("/message/content").and_then(Value::as_array) {
            for it in items {
                if it.get("type").and_then(Value::as_str) == Some("thinking") {
                    if let Some(s) = it.get("thinking").and_then(Value::as_str) {
                        out.push(s.to_owned());
                    }
                }
            }
        }
    }
    out
}

fn spawn_prompt(jsonl: &str) -> Option<String> {
    let line = jsonl.lines().find(|l| !l.trim().is_empty())?;
    let v: Value = serde_json::from_str(line).ok()?;
    v.pointer("/message/content").and_then(Value::as_str).map(str::to_owned)
}

fn window(s: &str) -> Option<String> {
    let t: String = s.trim().chars().take(60).collect();
    (t.chars().count() >= 40).then_some(t)
}

fn real(t: &mut T) {
    let home = std::env::var("USERPROFILE").unwrap();
    let root = PathBuf::from(home).join(".claude").join("projects");
    let mut parents = Vec::new();
    for proj in std::fs::read_dir(&root).unwrap().flatten() {
        if let Ok(files) = std::fs::read_dir(proj.path()) {
            for f in files.flatten() {
                let p = f.path();
                if p.extension().and_then(|x| x.to_str()) == Some("jsonl") {
                    parents.push(p);
                }
            }
        }
    }
    let (mut same, mut diff_b, mut diff_f, mut diff_empty) = (0, 0, 0, 0);
    let (mut subs, mut think_hits, mut spawn_hits, mut body_think_hits, mut headers_ok, mut headers_bad) = (0, 0, 0, 0, 0, 0);
    let mut kinds = std::collections::BTreeMap::new();
    let mut headers_nested = 0;
    for (i, p) in parents.iter().enumerate() {
        let Ok(text) = std::fs::read_to_string(p) else { continue };
        let (n, o) = (parse(&text), transcript_main::parse(&text));
        let (nb, nf) = (render_brief(&n), render_full(&n));
        let mut ok = true;
        if nb != transcript_main::render_brief(&o) { diff_b += 1; ok = false; println!("DIFF brief parent#{i}"); }
        if nf != transcript_main::render_full(&o) { diff_f += 1; ok = false; println!("DIFF full parent#{i}"); }
        if render_brief_with_subagents(&n, &[]) != nb || render_full_with_subagents(&n, &[]) != nf { diff_empty += 1; ok = false; }
        if ok { same += 1; }
        // subagents of this session
        let dir = p.with_extension("").join("subagents");
        let Ok(entries) = std::fs::read_dir(&dir) else { continue };
        let mut blocks = Vec::new();
        let mut all_thinking = Vec::new();
        for e in entries.flatten() {
            let sp = e.path();
            let name = sp.file_name().unwrap().to_string_lossy().to_string();
            if !(name.starts_with("agent-") && name.ends_with(".jsonl")) { continue; }
            let id = name.trim_start_matches("agent-").trim_end_matches(".jsonl").to_owned();
            let Ok(st) = std::fs::read_to_string(&sp) else { continue };
            let meta = std::fs::read_to_string(sp.with_extension("meta.json")).ok();
            let b = Subagent::new(SubagentInput { agent_id: &id, meta: meta.as_deref(), transcript: Some(&st), ..Default::default() });
            subs += 1;
            let kind = match b.body() {
                SubagentBody::Report(_) => "Report", SubagentBody::Transcript(_) => "Transcript",
                SubagentBody::LastMessage(_) => "LastMessage", SubagentBody::InProgress(_) => "InProgress", SubagentBody::Empty => "Empty",
            };
            *kinds.entry(kind).or_insert(0) += 1;
            let rendered = b.render();
            if std::env::var("QA_EYE").ok().as_deref() == Some(&format!("{i}")) { println!("EYE-----
{}", rendered.lines().take(12).collect::<Vec<_>>().join("
")); }
            let th = thinking_texts(&st);
            for w in th.iter().filter_map(|x| window(x)) {
                if rendered.contains(&w) { body_think_hits += 1; }
            }
            if let Some(w) = spawn_prompt(&st).and_then(|x| window(&x)) {
                if b.body().text().contains(&w) { spawn_hits += 1; println!("SPAWN-IN-BODY parent#{i} sub {}", id.len()); }
            }
            all_thinking.extend(th);
            blocks.push(b);
        }
        if blocks.is_empty() { continue; }
        let full = render_full_with_subagents(&n, &blocks);
        let brief = render_brief_with_subagents(&n, &blocks);
        for w in all_thinking.iter().filter_map(|x| window(x)) {
            if brief.contains(&w) || (full.contains(&w) && !nf.contains(&w)) { think_hits += 1; println!("THINK-IN-PARENT parent#{i}"); }
        }
        // every linked subagent header appears once per Agent call
        for b in &blocks {
            let needle = format!(" {}", b.agent_id());
            let in_other = blocks.iter().any(|o| o.agent_id() != b.agent_id() && o.render().contains(&needle));
            if brief.contains(&needle) == nb.contains(&needle) { headers_ok += 1 } else if in_other { headers_nested += 1 } else { headers_bad += 1 }
        }
    }
    println!("REAL parents={} identical={same} brief_diff={diff_b} full_diff={diff_f} with_empty_diff={diff_empty}", parents.len());
    println!("REAL subagents={subs} kinds={kinds:?} thinking_in_block={body_think_hits} spawn_prompt_in_body={spawn_hits} new_thinking_in_parent={think_hits} header_link_ok={headers_ok} header_link_bad={headers_bad} nested_only={headers_nested}");
    t.check("R1 render_brief/full identical to main on all real parents", diff_b == 0 && diff_f == 0, format!("{diff_b}/{diff_f}"));
    t.check("R1 with_subagents(&[]) identical on all real parents", diff_empty == 0, diff_empty);
    t.check("R2 no thinking in any real block", body_think_hits == 0, body_think_hits);
    t.check("R2 no spawn prompt in any real body", spawn_hits == 0, spawn_hits);
    t.check("R2 no new thinking in parent views", think_hits == 0, think_hits);
}

fn main() {
    let mut t = T { fails: 0 };
    synthetic(&mut t);
    if std::env::args().any(|a| a == "--real") {
        real(&mut t);
    }
    println!("FAILURES {}", t.fails);
}
