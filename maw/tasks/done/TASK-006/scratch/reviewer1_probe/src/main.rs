use transcript::{
    parse, render_brief, render_full, split_for_telegram, telegram_len, SplitLimits,
    TELEGRAM_TEXT_LIMIT,
};

fn boundary_splits_cluster(chunks: &[String], cluster: &str) -> bool {
    !chunks.iter().any(|chunk| chunk.contains(cluster))
}

fn main() {
    let limits = SplitLimits::default();

    let combining_cluster = format!("a{}", "\u{0301}".repeat(24));
    let combining_input = format!(
        "{}{}tail",
        "x".repeat(TELEGRAM_TEXT_LIMIT - 18),
        combining_cluster
    );
    let combining = split_for_telegram(&combining_input, limits);
    println!(
        "combining_split={} lens={:?}",
        boundary_splits_cluster(&combining.chunks, &combining_cluster),
        combining.chunks.iter().map(|s| telegram_len(s)).collect::<Vec<_>>()
    );

    let zwj_cluster = (0..12).map(|_| "👩\u{200d}").collect::<String>() + "🚀";
    let zwj_input = format!(
        "{}{}tail",
        "x".repeat(TELEGRAM_TEXT_LIMIT - 30),
        zwj_cluster
    );
    let zwj = split_for_telegram(&zwj_input, limits);
    println!(
        "zwj_split={} lens={:?}",
        boundary_splits_cluster(&zwj.chunks, &zwj_cluster),
        zwj.chunks.iter().map(|s| telegram_len(s)).collect::<Vec<_>>()
    );

    let long_line = "z".repeat(50 * 1024);
    let long = split_for_telegram(&long_line, limits);
    println!(
        "50k_chunks={} max_len={} prefer_file={}",
        long.chunks.len(),
        long.chunks.iter().map(|s| telegram_len(s)).max().unwrap_or_default(),
        long.prefer_file
    );

    let astral_boundary = format!("{}🚀b", "a".repeat(TELEGRAM_TEXT_LIMIT - 1));
    let astral = split_for_telegram(&astral_boundary, limits);
    println!("astral_chunks={:?}", astral.chunks.iter().map(|s| telegram_len(s)).collect::<Vec<_>>());

    let service = r#"{"type":"user","message":{"content":"<task-notification>done</task-notification>"}}"#;
    let service_turns = parse(service);
    println!("service_brief={:?}", render_brief(&service_turns));
    println!("service_full={:?}", render_full(&service_turns));
}
