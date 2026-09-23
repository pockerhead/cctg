//! QA TASK-009: independent acceptance tests against the public hub API.
use std::collections::VecDeque;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::{Duration, SystemTime};

use cctg::hub::api::{ApiError, Message};
use cctg::hub::commands::{self, Parsed, View, parse, serve};
use cctg::hub::config::Allowlist;
use cctg::hub::offset::OffsetStore;
use cctg::hub::scheduler::{BucketConfig, Delivery, Op, Outcome, Scheduler, Transport};
use cctg::hub::sessions::{LocateError, ProjectsDir, TranscriptLocator};
use cctg::hub::updates::{Inbound, Routed, UpdateSource, poll};
use serde_json::{Value, json};
use tokio::sync::mpsc;

const CHAT: i64 = -1000000000077;
const ALLOWED: i64 = 4242;
const S1: &str = "abcdef01-0000-4000-8000-000000000001";
const S2: &str = "abcdef02-0000-4000-8000-000000000002";

static SEQ: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);
fn tmp(name: &str) -> PathBuf {
    let n = SEQ.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
    let p = std::env::temp_dir().join(format!("qa009-{name}-{}-{n}", std::process::id()));
    let _ = std::fs::remove_dir_all(&p);
    std::fs::create_dir_all(&p).unwrap();
    p
}

fn set_age(path: &Path, secs: u64) {
    std::fs::File::options()
        .write(true)
        .open(path)
        .unwrap()
        .set_modified(SystemTime::now() - Duration::from_secs(secs))
        .unwrap();
}

fn session_lines(n: usize, answer_len: usize) -> String {
    let mut s = String::new();
    for i in 0..n {
        s.push_str(&json!({"type":"user","message":{"role":"user","content":format!("q{i}")}}).to_string());
        s.push('\n');
        s.push_str(&json!({"type":"assistant","message":{"role":"assistant","stop_reason":"end_turn",
            "content":[{"type":"text","text":format!("a{i} ").repeat(answer_len/4)}]}}).to_string());
        s.push('\n');
    }
    s
}

struct Fake {
    ops: Mutex<Vec<Op>>,
    script: Mutex<VecDeque<Option<ApiError>>>,
    always: Option<(i64, &'static str)>,
}
impl Fake {
    fn new(script: Vec<Option<ApiError>>, always: Option<(i64, &'static str)>) -> Arc<Self> {
        Arc::new(Self { ops: Mutex::new(vec![]), script: Mutex::new(script.into()), always })
    }
    fn ops(&self) -> Vec<Op> {
        self.ops.lock().unwrap().clone()
    }
}
impl Transport for Fake {
    async fn execute(&self, op: &Op) -> Delivery {
        self.ops.lock().unwrap().push(op.clone());
        if let Some(Some(e)) = self.script.lock().unwrap().pop_front() {
            return Err(e);
        }
        if let Some((code, d)) = self.always {
            return Err(ApiError::Telegram { code, description: d.to_owned() });
        }
        Ok(Outcome::Sent(Message::default()))
    }
}

async fn run_cmds(fake: &Arc<Fake>, root: &Path, texts: &[&str]) {
    let (sched, outbox) = Scheduler::new(fake.clone(), BucketConfig::default());
    let sched = tokio::spawn(sched.run());
    let (tx, rx) = mpsc::unbounded_channel();
    for t in texts {
        tx.send(Inbound { message_id: 1, thread_id: Some(9), text: Some((*t).into()) }).unwrap();
    }
    drop(tx);
    serve(rx, outbox, Arc::new(ProjectsDir::new(root.to_owned())), Some("bot".into())).await;
    sched.await.unwrap();
}

fn kinds(ops: &[Op]) -> Vec<String> {
    ops.iter()
        .map(|op| match op {
            Op::Send { text, .. } if text == "Не удалось отправить транскрипт." => "NOTICE".into(),
            Op::Send { .. } => "Send".into(),
            Op::SendDocument { .. } => "Doc".into(),
            other => format!("{other:?}"),
        })
        .collect()
}

fn one_project(jsonl: &str) -> PathBuf {
    let root = tmp("proj");
    let p = root.join("C--Users-secretname-dev-app");
    std::fs::create_dir_all(&p).unwrap();
    std::fs::write(p.join(format!("{S1}.jsonl")), jsonl).unwrap();
    root
}

// ---------- AC2 + fixer #1: delivery failure notice is one-shot ----------

#[tokio::test(start_paused = true)]
async fn persistent_failure_gives_one_notice_per_command_no_loop() {
    let root = one_project(&session_lines(1, 100));
    let fake = Fake::new(vec![], Some((400, "Bad Request: message thread not found")));
    run_cmds(&fake, &root, &["/brief", "/full", "/brief zz!"]).await;
    // command attempt + notice attempt each; usage reply fails too -> notice
    assert_eq!(kinds(&fake.ops()), ["Send", "NOTICE", "Send", "NOTICE", "Send", "NOTICE"]);
}

#[tokio::test(start_paused = true)]
async fn failure_then_recovery_next_command_works() {
    let root = one_project(&session_lines(1, 100));
    let fake = Fake::new(
        vec![Some(ApiError::Telegram { code: 403, description: "Forbidden".into() }), Some(ApiError::Telegram { code: 403, description: "Forbidden".into() })],
        None,
    );
    run_cmds(&fake, &root, &["/brief", "/full"]).await;
    let ops = fake.ops();
    assert_eq!(kinds(&ops), ["Send", "NOTICE", "Send"]);
    let body = transcript::render_full(transcript::last_prompts(
        &transcript::parse(&session_lines(1, 100)),
        1,
    ));
    assert!(matches!(&ops[2], Op::Send { text, .. } if *text == body));
}

#[tokio::test(start_paused = true)]
async fn too_long_everywhere_switches_once_and_stops() {
    // big but <= 4 chunks, so text path is taken
    let jsonl = session_lines(3, 3000);
    let root = one_project(&jsonl);
    let fake = Fake::new(vec![], Some((400, "Bad Request: message is too long")));
    run_cmds(&fake, &root, &["/brief 3", "/full 1"]).await;
    let k = kinds(&fake.ops());
    // Records what happens: expected [Send, Doc] per command, no loop.
    println!("too_long_everywhere ops: {k:?}");
    assert_eq!(k.iter().filter(|x| *x == "Doc").count(), 2);
    assert!(k.len() <= 6, "{k:?}");
}

#[tokio::test(start_paused = true)]
async fn too_long_on_third_chunk_rest_is_exact_tail() {
    let jsonl = session_lines(3, 3600);
    let root = one_project(&jsonl);
    let body = transcript::render_brief(transcript::last_prompts(&transcript::parse(&jsonl), 3));
    let split = transcript::split_for_telegram(&body, Default::default());
    assert!(split.chunks.len() >= 3 && !split.prefer_file, "{}", split.chunks.len());
    let too_long = || Some(ApiError::Telegram { code: 400, description: "Bad Request: message is too long".into() });
    let fake = Fake::new(vec![None, None, too_long()], None);
    run_cmds(&fake, &root, &["/brief 3"]).await;
    let ops = fake.ops();
    let mut got = String::new();
    for (i, op) in ops.iter().enumerate() {
        match op {
            Op::Send { text, .. } if i < 2 => got.push_str(text),
            Op::Send { .. } => {}
            Op::SendDocument { document, .. } => {
                got.push_str(&String::from_utf8(document.bytes.clone()).unwrap());
                assert!(document.file_name.ends_with(".txt"));
                assert!(!document.caption.clone().unwrap_or_default().contains("secretname"));
            }
            _ => panic!(),
        }
    }
    assert_eq!(kinds(&ops), ["Send", "Send", "Send", "Doc"]);
    assert_eq!(got, body);
}

#[tokio::test(start_paused = true)]
async fn huge_reply_is_one_document_equal_to_library() {
    let jsonl = session_lines(10, 5000);
    let root = one_project(&jsonl);
    let fake = Fake::new(vec![], None);
    run_cmds(&fake, &root, &["/full 10"]).await;
    let ops = fake.ops();
    assert_eq!(kinds(&ops), ["Doc"]);
    let body = transcript::render_full(transcript::last_prompts(&transcript::parse(&jsonl), 10));
    assert!(matches!(&ops[0], Op::SendDocument { document, thread_id } if document.bytes == body.as_bytes() && *thread_id == Some(9)));
}

#[tokio::test]
async fn stopped_scheduler_does_not_hang_worker() {
    let root = one_project(&session_lines(1, 100));
    let fake = Fake::new(vec![], None);
    let (sched, outbox) = Scheduler::new(fake.clone(), BucketConfig::default());
    drop(sched); // scheduler never runs
    let (tx, rx) = mpsc::unbounded_channel();
    tx.send(Inbound { message_id: 1, thread_id: None, text: Some("/brief".into()) }).unwrap();
    tx.send(Inbound { message_id: 2, thread_id: None, text: Some("/full".into()) }).unwrap();
    drop(tx);
    tokio::time::timeout(
        Duration::from_secs(10),
        serve(rx, outbox, Arc::new(ProjectsDir::new(root)), None),
    )
    .await
    .expect("worker finished despite a stopped scheduler");
}

// ---------- AC1 on the real library fixtures, order preserved ----------

#[tokio::test(start_paused = true)]
async fn fixtures_match_library_with_prefix_and_n() {
    let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../../../../../crates/transcript/tests/fixtures");
    let mut count = 0;
    for entry in std::fs::read_dir(&dir).unwrap().flatten() {
        let path = entry.path();
        if path.extension().and_then(|e| e.to_str()) != Some("jsonl") {
            continue;
        }
        let jsonl = std::fs::read_to_string(&path).unwrap();
        let root = one_project(&jsonl);
        let fake = Fake::new(vec![], None);
        run_cmds(&fake, &root, &["/full 5 abcdef01", "/brief 1", "/BRIEF@BOT 2 ABCDEF"]).await;
        let turns = transcript::parse(&jsonl);
        let want: Vec<String> = [
            transcript::render_full(transcript::last_prompts(&turns, 5)),
            transcript::render_brief(transcript::last_prompts(&turns, 1)),
            transcript::render_brief(transcript::last_prompts(&turns, 2)),
        ]
        .iter()
        .flat_map(|b| {
            if b.trim().is_empty() {
                vec![format!("EMPTY")]
            } else {
                transcript::split_for_telegram(b, Default::default()).chunks
            }
        })
        .collect();
        let got: Vec<String> = fake
            .ops()
            .into_iter()
            .map(|op| match op {
                Op::Send { text, .. } if text.contains("пока нечего показывать") => "EMPTY".into(),
                Op::Send { text, .. } => text,
                Op::SendDocument { document, .. } => String::from_utf8(document.bytes).unwrap(),
                _ => panic!(),
            })
            .collect();
        assert_eq!(got, want, "{}", path.file_name().unwrap().to_string_lossy());
        count += 1;
    }
    assert!(count >= 7, "{count}");
}

// ---------- AC6/AC7 resolver ----------

#[test]
fn resolver_newest_prefix_ambiguous_subagents() {
    let root = tmp("resolver");
    let a = root.join("C--Users-secretname-a");
    let b = root.join("C--Users-secretname-b");
    std::fs::create_dir_all(&a).unwrap();
    std::fs::create_dir_all(&b).unwrap();
    std::fs::write(a.join(format!("{S1}.jsonl")), "{\"type\":\"ai-title\",\"aiTitle\":\"Alpha\"}\n").unwrap();
    std::fs::write(b.join(format!("{S2}.jsonl")), "{}\n").unwrap();
    set_age(&a.join(format!("{S1}.jsonl")), 10);
    set_age(&b.join(format!("{S2}.jsonl")), 500);
    // newer subagent files with uuid names, and a symlink-like / dir entry
    let sub = b.join(S2).join("subagents");
    std::fs::create_dir_all(&sub).unwrap();
    std::fs::write(sub.join("ffffffff-0000-4000-8000-00000000000f.jsonl"), "{}\n").unwrap();
    std::fs::write(sub.join("agent-a1.jsonl"), "{}\n").unwrap();
    // uuid-named file directly in root (not a project) -> ignored
    std::fs::write(root.join("eeeeeeee-0000-4000-8000-00000000000e.jsonl"), "{}\n").unwrap();
    // uppercase uuid -> not a session
    std::fs::write(a.join("DDDDDDDD-0000-4000-8000-00000000000D.jsonl"), "{}\n").unwrap();

    let dir = ProjectsDir::new(root.clone());
    assert_eq!(dir.locate(None, None).unwrap().session_id, S1);
    assert_eq!(dir.locate(Some(5), Some("abcdef02")).unwrap().session_id, S2);
    assert_eq!(dir.locate(None, Some("ffff")), Err(LocateError::NoMatch));
    assert_eq!(dir.locate(None, Some("eeee")), Err(LocateError::NoMatch));
    assert_eq!(dir.locate(None, Some("dddd")), Err(LocateError::NoMatch));
    match dir.locate(None, Some("abcdef")) {
        Err(LocateError::Ambiguous(c)) => assert_eq!(c.len(), 2),
        other => panic!("{other:?}"),
    }
    // S1 made older: S2 becomes newest
    set_age(&a.join(format!("{S1}.jsonl")), 1000);
    assert_eq!(dir.locate(None, None).unwrap().session_id, S2);
}

#[tokio::test(start_paused = true)]
async fn ambiguous_reply_has_no_project_names() {
    let root = tmp("ambig");
    for (i, s) in [S1, S2].iter().enumerate() {
        let p = root.join(format!("C--Users-secretname-proj{i}"));
        std::fs::create_dir_all(&p).unwrap();
        std::fs::write(p.join(format!("{s}.jsonl")), format!("{{\"type\":\"ai-title\",\"aiTitle\":\"T{i}\"}}\n")).unwrap();
    }
    let fake = Fake::new(vec![], None);
    run_cmds(&fake, &root, &["/brief abcdef", "/full 1 ABCDEF0"]).await;
    let ops = fake.ops();
    assert_eq!(ops.len(), 2);
    for op in ops {
        let Op::Send { text, .. } = op else { panic!() };
        println!("ambiguous notice lines={}", text.lines().count());
        assert!(text.starts_with("Под это начало id подходят 2"));
        assert!(!text.contains("secretname") && !text.contains("C--") && !text.contains("proj"));
        assert!(text.contains("abcdef01") && text.contains("abcdef02"));
        assert!(text.contains("T0") && text.contains("T1"));
    }
}

// ---------- AC4 bad paths, polling continues ----------

#[tokio::test(start_paused = true)]
async fn bad_paths_notices_no_path() {
    let root = one_project(&session_lines(1, 10));
    let file = root.join("C--Users-secretname-dev-app").join(format!("{S1}.jsonl"));
    std::fs::remove_file(&file).unwrap();
    std::fs::create_dir_all(&file).unwrap(); // dir named like session -> skipped
    let fake = Fake::new(vec![], None);
    run_cmds(&fake, &root, &["/brief", "/brief 1 abc", "/full ../../etc", "/brief"]).await;
    let missing = tmp("gone").join("nope");
    run_cmds(&fake, &missing, &["/full"]).await;
    // invalid utf-8 + garbage transcript
    let root2 = one_project("");
    std::fs::write(
        root2.join("C--Users-secretname-dev-app").join(format!("{S1}.jsonl")),
        b"\xff\xfe garbage\n{\"type\":\"user\",\"message\":{\"content\":\"hi\"}}\n{\"type\":\"assistant\"",
    )
    .unwrap();
    run_cmds(&fake, &root2, &["/brief", "/full"]).await;
    let texts: Vec<String> = fake
        .ops()
        .into_iter()
        .map(|op| match op {
            Op::Send { text, .. } => text,
            _ => "DOC".into(),
        })
        .collect();
    println!("bad path replies: {}", texts.len());
    assert_eq!(texts.len(), 7);
    for t in &texts {
        assert!(!t.contains("secretname") && !t.contains("qa009") && !t.contains("\\") , "{t}");
    }
    assert_eq!(texts[0], "Сессий Claude Code пока нет.");
    assert_eq!(texts[1], "Нет сессии с таким началом id.");
    assert_eq!(texts[2], commands::USAGE);
    assert!(texts[4].starts_with("Каталог проектов Claude Code не найден"));
    assert!(texts[5].contains("hi"), "{}", texts[5]);
}

// ---------- parse edge cases ----------

#[test]
fn parse_edges() {
    let b = Some("bot");
    let cmd = |p: Parsed| match p {
        Parsed::Command(c) => format!("{:?}/{}/{:?}", c.view, c.prompts, c.session_prefix),
        Parsed::Usage => "usage".into(),
        Parsed::NotOurs => "not".into(),
    };
    assert_eq!(cmd(parse("/brief 100", b)), "Brief/100/None");
    assert_eq!(cmd(parse("/brief 2026", b)), "usage");
    assert_eq!(cmd(parse("/brief 3 2026", b)), "Brief/3/Some(\"2026\")");
    assert_eq!(cmd(parse("/full\n2", b)), "Full/2/None");
    assert_eq!(cmd(parse("/brief -1", b)), "Brief/3/Some(\"-1\")");
    assert_eq!(cmd(parse("/brief@bot", b)), "Brief/3/None");
    assert_eq!(cmd(parse("/brief@", b)), "not");
    assert_eq!(cmd(parse("/brief 99999999999999999999999", b)), "usage");
    assert!(matches!(parse("/Full", b), Parsed::Command(c) if c.view == View::Full));
}

// ---------- AC3 offset ----------

struct Tg {
    updates: Vec<Value>,
    served: Mutex<usize>,
}
impl UpdateSource for Tg {
    fn chat_id(&self) -> i64 {
        CHAT
    }
    async fn get_updates(&self, offset: Option<i64>, timeout: Duration) -> Result<Vec<Value>, ApiError> {
        *self.served.lock().unwrap() += 1;
        let b: Vec<Value> = self
            .updates
            .iter()
            .filter(|u| offset.is_none_or(|o| u["update_id"].as_i64().unwrap() >= o))
            .cloned()
            .collect();
        if b.is_empty() {
            tokio::time::sleep(timeout).await;
        }
        Ok(b)
    }
}

fn upd(id: i64, from: i64, text: &str) -> Value {
    json!({"update_id": id, "message": {"message_id": id, "date": 1, "text": text, "message_thread_id": 3, "is_topic_message": true,
        "from": {"id": from, "is_bot": false, "first_name": "x"},
        "chat": {"id": CHAT, "type": "supergroup", "is_forum": true}}})
}

async fn poll_for(updates: Vec<Value>, store: &OffsetStore, secs: u64) -> Vec<String> {
    let mut seen = vec![];
    let src = Tg { updates, served: Mutex::new(0) };
    let allow: Allowlist = [ALLOWED].into_iter().collect();
    let _ = tokio::time::timeout(
        Duration::from_secs(secs),
        poll(&src, &allow, store, |r| {
            if let Routed::Input(i) = r {
                seen.push(i.text.unwrap_or_default())
            }
        }),
    )
    .await;
    seen
}

#[tokio::test(start_paused = true)]
async fn restart_does_not_replay_and_stranger_is_ignored() {
    let d = tmp("off-restart");
    let s = OffsetStore::open(&d).unwrap();
    let batch = vec![upd(100, ALLOWED, "/brief"), upd(101, 999, "/full"), upd(102, ALLOWED, "/full 2")];
    // "crash" right after the first poll handled everything (timeout aborts)
    assert_eq!(poll_for(batch.clone(), &s, 1).await, ["/brief", "/full 2"]);
    assert_eq!(OffsetStore::open(&d).unwrap().load(), Some(103));
    // restart: Telegram still returns all (never confirmed) + a new one
    let mut again = batch.clone();
    again.push(upd(103, ALLOWED, "/brief 1"));
    let s2 = OffsetStore::open(&d).unwrap();
    assert_eq!(poll_for(again, &s2, 120).await, ["/brief 1"]);
    assert_eq!(s2.load(), Some(104));
}

#[tokio::test(start_paused = true)]
async fn stale_offset_is_ignored_fresh_is_used() {
    let d = tmp("off-stale");
    let s = OffsetStore::open(&d).unwrap();
    s.save(50).unwrap();
    set_age(&d.join("offset"), 25 * 3600);
    assert_eq!(s.load(), None);
    set_age(&d.join("offset"), 23 * 3600);
    assert_eq!(s.load(), Some(50));
    // future mtime (clock skew) still loads
    std::fs::File::options().write(true).open(d.join("offset")).unwrap()
        .set_modified(SystemTime::now() + Duration::from_secs(3600)).unwrap();
    assert_eq!(s.load(), Some(50));
    // negative / whitespace
    std::fs::write(d.join("offset"), " 77 \r\n").unwrap();
    assert_eq!(s.load(), Some(77));
    std::fs::write(d.join("offset"), "").unwrap();
    assert_eq!(s.load(), None);
}

/// Telegram reset ids below the saved offset, and ignores offsets above its ids.
struct ResetTg {
    updates: Vec<Value>,
    confirmed: Mutex<i64>,
    calls: Mutex<Vec<Option<i64>>>,
}
impl UpdateSource for ResetTg {
    fn chat_id(&self) -> i64 {
        CHAT
    }
    async fn get_updates(&self, offset: Option<i64>, timeout: Duration) -> Result<Vec<Value>, ApiError> {
        self.calls.lock().unwrap().push(offset);
        let conf = {
            let mut c = self.confirmed.lock().unwrap();
            if let Some(o) = offset.filter(|o| *o <= 100) {
                *c = (*c).max(o);
            }
            *c
        };
        let b: Vec<Value> = self.updates.iter().filter(|u| u["update_id"].as_i64().unwrap() >= conf).cloned().collect();
        if b.is_empty() {
            tokio::time::sleep(timeout).await;
        }
        Ok(b)
    }
}

#[tokio::test(start_paused = true)]
async fn telegram_id_reset_is_handled_once() {
    let d = tmp("off-reset");
    let s = OffsetStore::open(&d).unwrap();
    s.save(900_000).unwrap();
    let src = ResetTg { updates: vec![upd(3, ALLOWED, "/brief"), upd(4, ALLOWED, "/full")], confirmed: Mutex::new(0), calls: Mutex::new(vec![]) };
    let allow: Allowlist = [ALLOWED].into_iter().collect();
    let mut seen = vec![];
    let _ = tokio::time::timeout(Duration::from_secs(300), poll(&src, &allow, &s, |r| {
        if let Routed::Input(i) = r { seen.push(i.text.unwrap_or_default()) }
    })).await;
    assert_eq!(seen, ["/brief", "/full"]);
    assert_eq!(s.load(), Some(5));
    let calls = src.calls.lock().unwrap().clone();
    assert_eq!(calls[0], Some(900_000));
    assert!(calls[1..].iter().all(|c| *c == Some(5)), "{calls:?}");
}
