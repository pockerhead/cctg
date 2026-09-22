use transcript::{Chunks, SplitLimits, TELEGRAM_TEXT_LIMIT, split_for_telegram, telegram_len};

fn split(text: &str) -> Chunks {
    split_for_telegram(text, SplitLimits::default())
}

fn assert_valid(text: &str, chunks: &Chunks) {
    for chunk in &chunks.chunks {
        assert!(telegram_len(chunk) <= TELEGRAM_TEXT_LIMIT);
        assert!(chunk.chars().count() <= TELEGRAM_TEXT_LIMIT);
        assert!(!chunk.trim().is_empty());
    }
    let strip = |s: &str| s.chars().filter(|c| !c.is_whitespace()).collect::<String>();
    assert_eq!(strip(&chunks.chunks.concat()), strip(text));
    assert_eq!(&split(text), chunks, "not deterministic");
}

#[test]
fn telegram_len_counts_utf16_units() {
    assert_eq!(telegram_len("abc"), 3);
    assert_eq!(telegram_len("я"), 1);
    assert_eq!(telegram_len("\u{1F680}"), 2);
    assert_eq!(
        telegram_len("\u{1F468}\u{200D}\u{1F469}\u{200D}\u{1F467}"),
        8
    );
}

#[test]
fn short_text_is_one_chunk() {
    let chunks = split("hello\n\nworld");
    assert_eq!(chunks.chunks, ["hello\n\nworld"]);
    assert!(!chunks.prefer_file);
    assert_eq!(split("").chunks, Vec::<String>::new());
    assert_eq!(split("\n\n  \n").chunks, Vec::<String>::new());
}

#[test]
fn exact_limit_fits_and_one_more_splits() {
    let exact = "a".repeat(TELEGRAM_TEXT_LIMIT);
    assert_eq!(split(&exact).chunks, vec![exact]);
    let over = "a".repeat(TELEGRAM_TEXT_LIMIT + 1);
    let chunks = split(&over);
    assert_eq!(chunks.chunks.len(), 2);
    assert_valid(&over, &chunks);
}

#[test]
fn prefers_paragraph_then_line_boundaries() {
    let para = "x".repeat(3000);
    let text = format!("{para}\n\n{para}\n\n{para}");
    let chunks = split(&text);
    assert_eq!(chunks.chunks, [para.clone(), para.clone(), para.clone()]);
    let lines = format!("{}\n{}", "y".repeat(3000), "z".repeat(3000));
    assert_eq!(split(&lines).chunks, ["y".repeat(3000), "z".repeat(3000)]);
}

#[test]
fn fifty_kb_block_is_deterministic_and_prefers_file() {
    let block = "0123456789".repeat(5_000);
    let chunks = split(&block);
    assert_eq!(chunks.chunks.len(), 13);
    assert!(chunks.prefer_file);
    assert_valid(&block, &chunks);
    let cyrillic = "ж".repeat(25_000); // 50 KB of UTF-8
    let chunks = split(&cyrillic);
    assert_eq!(chunks.chunks.len(), 7);
    assert!(chunks.prefer_file);
    assert_valid(&cyrillic, &chunks);
}

#[test]
fn emoji_on_the_boundary_is_never_cut() {
    let rocket = "\u{1F680}";
    let text = format!("{}{rocket}b", "a".repeat(TELEGRAM_TEXT_LIMIT - 1));
    let chunks = split(&text);
    assert_eq!(
        chunks.chunks,
        ["a".repeat(TELEGRAM_TEXT_LIMIT - 1), format!("{rocket}b")]
    );
    assert_valid(&text, &chunks);
}

#[test]
fn zwj_sequence_on_the_boundary_stays_whole() {
    let family = "\u{1F468}\u{200D}\u{1F469}\u{200D}\u{1F467}";
    for pad in TELEGRAM_TEXT_LIMIT - 8..TELEGRAM_TEXT_LIMIT {
        let text = format!("{}{family}{}", "a".repeat(pad), "b".repeat(10));
        let chunks = split(&text);
        assert_valid(&text, &chunks);
        assert!(
            chunks.chunks.iter().any(|chunk| chunk.contains(family)),
            "pad {pad}"
        );
    }
}

#[test]
fn only_emoji_line_splits_on_char_boundaries() {
    let text = "\u{1F600}".repeat(5_000);
    let chunks = split(&text);
    assert_valid(&text, &chunks);
    assert_eq!(chunks.chunks.len(), 3);
    assert_eq!(telegram_len(&chunks.chunks[0]), TELEGRAM_TEXT_LIMIT);
}

#[test]
fn tiny_limits_still_make_progress() {
    for chunk_len in [0, 1, 2, 3] {
        let limits = SplitLimits {
            chunk_len,
            max_chunks: 1,
        };
        let chunks = split_for_telegram("a\u{1F680}b c", limits);
        assert!(
            chunks
                .chunks
                .iter()
                .all(|chunk| telegram_len(chunk) <= chunk_len.max(2))
        );
        assert!(chunks.prefer_file);
    }
}

#[test]
fn threshold_is_configurable() {
    let text = format!("{}\n\n{}", "a".repeat(3000), "b".repeat(3000));
    let limits = |max_chunks| SplitLimits {
        chunk_len: TELEGRAM_TEXT_LIMIT,
        max_chunks,
    };
    assert!(split_for_telegram(&text, limits(1)).prefer_file);
    assert!(!split_for_telegram(&text, limits(2)).prefer_file);
}
