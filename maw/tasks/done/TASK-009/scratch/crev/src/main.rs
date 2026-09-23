// Read-only probe: prints counts, sizes and timings, never content or paths.
use std::time::Instant;
use cctg::hub::commands::{prepare, Prepared, TranscriptCommand, View, parse, Parsed};
use cctg::hub::sessions::{ProjectsDir, TranscriptLocator, LocateError};
use transcript::{split_for_telegram, SplitOptions};

fn main() {
    let root = std::path::PathBuf::from(std::env::var("USERPROFILE").unwrap()).join(".claude").join("projects");
    let dir = ProjectsDir::new(root.clone());
    let t = Instant::now();
    let found = dir.locate(None, None);
    println!("locate(None): ok={} in {:?}", found.is_ok(), t.elapsed());
    // ambiguity on single hex char
    for p in ["0", "a", "-", "00000000-0000"] {
        match dir.locate(None, Some(p)) {
            Err(LocateError::Ambiguous(c)) => println!("prefix {p:?}: ambiguous {}", c.len()),
            Err(e) => println!("prefix {p:?}: {e}"),
            Ok(_) => println!("prefix {p:?}: one"),
        }
    }
    // is any located file under a subagents dir?
    for view in [View::Brief, View::Full] {
        for n in [1usize, 3, 100] {
            let cmd = TranscriptCommand { view, prompts: n, session_prefix: None };
            let t = Instant::now();
            let p = prepare(&dir, None, &cmd);
            let el = t.elapsed();
            match p {
                Prepared::Transcript(r) => {
                    let s = split_for_telegram(&r.body, SplitOptions::default());
                    println!("{view:?} n={n}: body {} bytes, {} chunks, prefer_file={} in {el:?}", r.body.len(), s.chunks.len(), s.prefer_file);
                    let lost = s.chunks.concat() != r.body;
                    println!("   concat!=body: {lost}");
                }
                Prepared::Notice(t) => println!("{view:?} n={n}: notice len {}", t.len()),
            }
        }
    }
    // largest top-level file across all projects: find via walking 2 levels
    let mut biggest: Option<(u64, String)> = None;
    for pr in std::fs::read_dir(&root).unwrap().flatten() {
        if let Ok(fs) = std::fs::read_dir(pr.path()) { for f in fs.flatten() {
            let n = f.file_name().to_string_lossy().into_owned();
            if n.ends_with(".jsonl") && n.len()==42 { let l = f.metadata().map(|m| m.len()).unwrap_or(0);
              if biggest.as_ref().is_none_or(|b| l > b.0) { biggest = Some((l, n[..36].to_owned())); } }
        }}
    }
    if let Some((len, id)) = biggest {
        let cmd = TranscriptCommand { view: View::Full, prompts: 100, session_prefix: Some(id.clone()) };
        let t = Instant::now();
        let p = prepare(&dir, None, &cmd);
        match p { Prepared::Transcript(r) => println!("biggest {len} bytes: full100 body {} bytes in {:?}", r.body.len(), t.elapsed()),
                  Prepared::Notice(n) => println!("biggest {len}: notice len {}", n.len()) }
        let cmd = TranscriptCommand { view: View::Brief, prompts: 3, session_prefix: Some(id) };
        let t = Instant::now();
        if let Prepared::Transcript(r) = prepare(&dir, None, &cmd) { println!("biggest brief3 body {} in {:?}", r.body.len(), t.elapsed()); }
    }
    for s in ["/brief 20260923", "/brief 1 20260923", "/brief\u{a0}5", "/brief\n2", "/brief 5e5", "/full 1 --", "/brief@ 3"] {
        println!("{:?} -> {}", s, match parse(s, Some("cctg_bot")) { Parsed::Command(c) => format!("cmd n={} p={:?}", c.prompts, c.session_prefix), Parsed::Usage => "usage".into(), Parsed::NotOurs => "notours".into() });
    }
}
