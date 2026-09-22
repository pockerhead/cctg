use std::time::Instant;
use transcript::{SplitOptions, parse, render_full, split_for_telegram};

fn main() {
    let mut jsonl = String::new();
    for i in 0..1250 {
        jsonl.push_str(&format!("{{\"type\":\"user\",\"message\":{{\"content\":\"prompt {i} with some words\"}}}}\n"));
        jsonl.push_str(&format!("{{\"type\":\"assistant\",\"message\":{{\"stop_reason\":\"tool_use\",\"content\":[{{\"type\":\"tool_use\",\"id\":\"t{i}\",\"name\":\"Bash\",\"input\":{{\"command\":\"cargo test\"}}}}]}}}}\n"));
        jsonl.push_str(&format!("{{\"type\":\"user\",\"message\":{{\"content\":[{{\"type\":\"tool_result\",\"tool_use_id\":\"t{i}\",\"content\":\"{}\"}}]}}}}\n", "line of output\n".repeat(20)));
        jsonl.push_str(&format!("{{\"type\":\"assistant\",\"message\":{{\"stop_reason\":\"end_turn\",\"content\":[{{\"type\":\"text\",\"text\":\"{}\"}}]}}}}\n", format!("answer {i} ").repeat(10)));
    }
    let turns = parse(&jsonl);
    let t = Instant::now();
    let full = render_full(&turns);
    let r = t.elapsed();
    let t = Instant::now();
    let chunks = split_for_telegram(&full, SplitOptions::default());
    println!("turns={} full_bytes={} render={:?} split={:?} chunks={}", turns.len(), full.len(), r, t.elapsed(), chunks.chunks.len());
}
