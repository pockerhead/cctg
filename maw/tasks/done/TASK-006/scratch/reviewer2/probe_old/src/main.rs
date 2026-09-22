use transcript::{SplitLimits, TELEGRAM_TEXT_LIMIT, split_for_telegram};

fn main() {
    let l = SplitLimits::default();
    // regional-indicator flag: 1 ASCII unit + flags => cut lands between the two halves of a flag
    let flag = "\u{1F1FA}\u{1F1F8}";
    let t = format!("a{}", flag.repeat(1500));
    let c = split_for_telegram(&t, l);
    let first = &c.chunks[0];
    let ri = first.chars().filter(|ch| ('\u{1F1E6}'..='\u{1F1FF}').contains(ch)).count();
    println!("flag_split={} (regional indicators in first chunk: {ri})", ri % 2 == 1);
    // combining run and long zwj from reviewer1
    let comb = format!("a{}", "\u{0301}".repeat(24));
    let t = format!("{}{}tail", "x".repeat(TELEGRAM_TEXT_LIMIT - 18), comb);
    let c = split_for_telegram(&t, l);
    println!("combining_split={}", !c.chunks.iter().any(|s| s.contains(&comb)));
    // paragraph separator loss: "a\n\n" + long paragraph of two lines
    let t = format!("a\n\n{}\n{}", "b".repeat(3000), "c".repeat(3000));
    let c = split_for_telegram(&t, l);
    println!("para_sep_lost={} first_chunk_starts={:?}", c.chunks.concat() != t && !c.chunks[0].starts_with("a\n\n"), &c.chunks[0][..4]);
    // whitespace-only run longer than the limit
    let t = format!("a{}b", "\n".repeat(5000));
    let c = split_for_telegram(&t, l);
    println!("long_blank_run chunks={:?}", c.chunks);
}
