//! Splitting rendered text into Telegram-sized plain-text messages.

use unicode_segmentation::GraphemeCursor;

/// Bot API `sendMessage` text limit: "1-4096 characters after entities parsing".
pub const TELEGRAM_TEXT_LIMIT: usize = 4096;

/// Length in UTF-16 code units, the project's measure for `TELEGRAM_TEXT_LIMIT`.
/// The Bot API does not name the unit of that limit. UTF-16 length is never below the code-point count,
/// so a text within the limit by this measure fits either reading; emoji-heavy chunks get smaller.
pub fn telegram_len(text: &str) -> usize {
    text.chars().map(char::len_utf16).sum()
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SplitOptions {
    /// More chunks than this sets `SplitResult::prefer_file`.
    pub max_chunks: usize,
}

impl Default for SplitOptions {
    fn default() -> Self {
        Self { max_chunks: 4 }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SplitResult {
    /// Consecutive slices of the input, each within `TELEGRAM_TEXT_LIMIT`; whitespace-only slices are dropped.
    pub chunks: Vec<String>,
    /// True when `chunks.len() > max_chunks`: send the whole text as a document instead.
    pub prefer_file: bool,
}

/// Cuts `text` into consecutive slices. Each cut is at the last blank line, else line break, else
/// whitespace in the second half of the chunk, else grapheme boundary; a single grapheme longer than
/// the limit is cut at a char boundary.
pub fn split_for_telegram(text: &str, options: SplitOptions) -> SplitResult {
    let mut chunks = Vec::new();
    let mut rest = text;
    while !rest.is_empty() {
        let (chunk, tail) = rest.split_at(cut(rest, TELEGRAM_TEXT_LIMIT));
        if !chunk.trim().is_empty() {
            chunks.push(chunk.to_owned());
        }
        rest = tail;
    }
    let prefer_file = chunks.len() > options.max_chunks;
    SplitResult {
        chunks,
        prefer_file,
    }
}

/// Byte length (> 0, a char boundary) of the next chunk of non-empty `text` within `limit` UTF-16
/// units (`limit >= 2`, so one char always fits). Scans one chunk's worth of chars; grapheme rules
/// are only consulted at the few candidate cut points.
pub(crate) fn cut(text: &str, limit: usize) -> usize {
    let (mut units, mut fit) = (0, text.len());
    let (mut paragraph, mut line, mut space) = (0, 0, 0);
    let mut previous = '\0';
    for (index, c) in text.char_indices() {
        if units + c.len_utf16() > limit {
            fit = index;
            break;
        }
        units += c.len_utf16();
        let end = index + c.len_utf8();
        if c == '\n' {
            if previous == '\n' {
                paragraph = end;
            }
            line = end;
        } else if c.is_whitespace() {
            space = end;
        }
        if c != '\r' {
            previous = c;
        }
    }
    if fit == text.len() {
        return fit;
    }
    [paragraph, line, space]
        .into_iter()
        .find(|&cut| cut > fit / 2 && is_grapheme_boundary(text, cut))
        .or_else(|| last_grapheme_boundary(text, fit))
        .unwrap_or(fit)
}

fn is_grapheme_boundary(text: &str, offset: usize) -> bool {
    GraphemeCursor::new(offset, text.len(), true)
        .is_boundary(text, 0)
        .unwrap_or(false)
}

/// The last extended grapheme boundary in `1..=offset`; `None` when one grapheme spans it all.
fn last_grapheme_boundary(text: &str, offset: usize) -> Option<usize> {
    if is_grapheme_boundary(text, offset) {
        return Some(offset);
    }
    GraphemeCursor::new(offset, text.len(), true)
        .prev_boundary(text, 0)
        .ok()
        .flatten()
        .filter(|&boundary| boundary > 0)
}
