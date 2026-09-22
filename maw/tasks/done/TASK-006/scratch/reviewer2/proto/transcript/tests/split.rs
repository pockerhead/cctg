use transcript::{
    SplitOptions, SplitResult, TELEGRAM_TEXT_LIMIT, split_for_telegram, telegram_len,
};

fn split(text: &str) -> SplitResult {
    split_for_telegram(text, SplitOptions::default())
}

fn utf16(text: &str) -> usize {
    text.encode_utf16().count()
}

/// Every chunk fits, is non-blank, and is the next slice of the input; only whitespace lies between
/// chunks and after the last one. For inputs without a whitespace run longer than the limit this means
/// `chunks.concat() == text` up to leading/trailing whitespace dropped with blank slices.
fn assert_valid(text: &str, result: &SplitResult) {
    let mut rest = text;
    for chunk in &result.chunks {
        assert!(utf16(chunk) <= TELEGRAM_TEXT_LIMIT);
        assert!(!chunk.trim().is_empty());
        let at = rest
            .find(chunk.as_str())
            .expect("chunk is a slice of the input");
        assert!(rest[..at].trim().is_empty(), "non-blank text dropped");
        rest = &rest[at + chunk.len()..];
    }
    assert!(rest.trim().is_empty(), "non-blank tail dropped");
    assert_eq!(&split(text), result, "not deterministic");
}

#[test]
fn telegram_len_counts_utf16_units() {
    for text in [
        "abc",
        "я",
        "\u{1F680}",
        "\u{1F468}\u{200D}\u{1F469}\u{200D}\u{1F467}",
    ] {
        assert_eq!(telegram_len(text), utf16(text));
    }
    assert_eq!(telegram_len("\u{1F680}"), 2);
}

#[test]
fn short_text_is_one_chunk() {
    let result = split("hello\n\nworld");
    assert_eq!(result.chunks, ["hello\n\nworld"]);
    assert!(!result.prefer_file);
    assert_eq!(split("").chunks, Vec::<String>::new());
    assert_eq!(split("\n\n  \n").chunks, Vec::<String>::new());
}

#[test]
fn exact_limit_fits_and_one_more_splits() {
    let exact = "a".repeat(TELEGRAM_TEXT_LIMIT);
    assert_eq!(split(&exact).chunks, vec![exact]);
    let over = "a".repeat(TELEGRAM_TEXT_LIMIT + 1);
    let result = split(&over);
    assert_eq!(
        result.chunks,
        ["a".repeat(TELEGRAM_TEXT_LIMIT), "a".to_owned()]
    );
    assert_valid(&over, &result);
}

#[test]
fn prefers_paragraph_then_line_then_space() {
    let para = "x".repeat(3000);
    let text = format!("{para}\n\n{para}\n\n{para}");
    let result = split(&text);
    assert_eq!(
        result.chunks,
        [format!("{para}\n\n"), format!("{para}\n\n"), para.clone()]
    );
    assert_eq!(result.chunks.concat(), text);
    // a paragraph break wins over a later line break in the same window
    let text = format!(
        "{}\n\n{}\n{}",
        "p".repeat(2500),
        "q".repeat(1000),
        "r".repeat(3000)
    );
    assert!(split(&text).chunks[0].ends_with("p\n\n"));
    let lines = format!("{}\n{}", "y".repeat(3000), "z".repeat(3000));
    assert_eq!(
        split(&lines).chunks,
        [format!("{}\n", "y".repeat(3000)), "z".repeat(3000)]
    );
    let words = format!("{} {}", "u".repeat(3000), "v".repeat(3000));
    assert_eq!(
        split(&words).chunks,
        [format!("{} ", "u".repeat(3000)), "v".repeat(3000)]
    );
    // a break in the first half of the window is not used: the chunk would be too small
    let early = format!("{}\n{}", "e".repeat(100), "f".repeat(6000));
    let result = split(&early);
    assert_eq!(utf16(&result.chunks[0]), TELEGRAM_TEXT_LIMIT);
    assert_valid(&early, &result);
}

#[test]
fn paragraph_separator_is_kept_exactly() {
    let text = format!("a\n\n{}\n{}", "b".repeat(3000), "c".repeat(3000));
    let result = split(&text);
    assert_eq!(result.chunks.concat(), text);
    assert_valid(&text, &result);
}

#[test]
fn blank_run_longer_than_the_limit_drops_only_whitespace() {
    let text = format!("a{}b", "\n".repeat(10_000));
    let result = split(&text);
    assert_valid(&text, &result);
    assert_eq!(
        result.chunks.first().map(|c| c.starts_with('a')),
        Some(true)
    );
    assert_eq!(result.chunks.last().map(|c| c.ends_with('b')), Some(true));
}

#[test]
fn fifty_kb_block_is_deterministic_and_prefers_file() {
    let block = "0123456789".repeat(5_000);
    let result = split(&block);
    assert_eq!(result.chunks.len(), 13);
    assert!(result.prefer_file);
    assert_eq!(result.chunks.concat(), block);
    assert_valid(&block, &result);
    let cyrillic = "ж".repeat(25_000); // 50 KB of UTF-8
    let result = split(&cyrillic);
    assert_eq!(result.chunks.len(), 7);
    assert!(result.prefer_file);
    assert_eq!(result.chunks.concat(), cyrillic);
    assert_valid(&cyrillic, &result);
}

#[test]
fn emoji_on_the_boundary_is_never_cut() {
    let rocket = "\u{1F680}";
    let text = format!("{}{rocket}b", "a".repeat(TELEGRAM_TEXT_LIMIT - 1));
    let result = split(&text);
    assert_eq!(
        result.chunks,
        ["a".repeat(TELEGRAM_TEXT_LIMIT - 1), format!("{rocket}b")]
    );
    assert_valid(&text, &result);
}

/// Places `cluster` at every offset around the limit and checks that it stays in one chunk.
fn assert_cluster_kept(cluster: &str) {
    let len = utf16(cluster);
    assert!(len < TELEGRAM_TEXT_LIMIT / 2);
    for pad in TELEGRAM_TEXT_LIMIT - len - 1..=TELEGRAM_TEXT_LIMIT {
        let text = format!("{}{cluster}{}", "a".repeat(pad), "b".repeat(10));
        let result = split(&text);
        assert_valid(&text, &result);
        assert_eq!(result.chunks.concat(), text);
        assert!(
            result.chunks.iter().any(|chunk| chunk.contains(cluster)),
            "pad {pad}: {cluster:?} was cut"
        );
    }
}

#[test]
fn grapheme_clusters_on_the_boundary_stay_whole() {
    // ZWJ family
    assert_cluster_kept("\u{1F468}\u{200D}\u{1F469}\u{200D}\u{1F467}");
    // long ZWJ sequence and a base with 24 combining marks: longer than any fixed look-back
    assert_cluster_kept(&("\u{1F469}\u{200D}".repeat(12) + "\u{1F680}"));
    assert_cluster_kept(&format!("a{}", "\u{0301}".repeat(24)));
    // regional-indicator flag, skin tone, keycap, Devanagari with a spacing mark
    assert_cluster_kept("\u{1F1FA}\u{1F1F8}");
    assert_cluster_kept("\u{1F44D}\u{1F3FD}");
    assert_cluster_kept("1\u{FE0F}\u{20E3}");
    assert_cluster_kept("\u{0928}\u{093F}");
}

#[test]
fn flags_after_an_odd_prefix_are_not_split() {
    let flag = "\u{1F1FA}\u{1F1F8}";
    let text = format!("a{}", flag.repeat(1500));
    let result = split(&text);
    assert_valid(&text, &result);
    for chunk in &result.chunks {
        let indicators = chunk
            .chars()
            .filter(|c| ('\u{1F1E6}'..='\u{1F1FF}').contains(c))
            .count();
        assert_eq!(indicators % 2, 0);
    }
}

#[test]
fn oversized_grapheme_falls_back_to_char_boundaries() {
    let text = format!("a{}", "\u{0301}".repeat(10_000));
    let result = split(&text);
    assert_valid(&text, &result);
    assert_eq!(result.chunks.concat(), text);
    assert_eq!(result.chunks.len(), 3);
}

#[test]
fn only_emoji_line_splits_on_char_boundaries() {
    let text = "\u{1F600}".repeat(5_000);
    let result = split(&text);
    assert_valid(&text, &result);
    assert_eq!(result.chunks.len(), 3);
    assert_eq!(utf16(&result.chunks[0]), TELEGRAM_TEXT_LIMIT);
}

#[test]
fn threshold_is_configurable() {
    let text = format!("{}\n\n{}", "a".repeat(3000), "b".repeat(3000));
    let options = |max_chunks| SplitOptions { max_chunks };
    assert_eq!(SplitOptions::default().max_chunks, 4);
    assert!(split_for_telegram(&text, options(1)).prefer_file);
    assert!(!split_for_telegram(&text, options(2)).prefer_file);
}
