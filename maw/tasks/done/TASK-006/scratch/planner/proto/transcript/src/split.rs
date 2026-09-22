//! Splitting rendered text into Telegram-sized plain-text messages.

/// Bot API `sendMessage` text limit.
pub const TELEGRAM_TEXT_LIMIT: usize = 4096;

/// Length in UTF-16 code units. It never undercounts Unicode code points, so a chunk that fits by this
/// measure also fits a code-point limit.
pub fn telegram_len(text: &str) -> usize {
    text.chars().map(char::len_utf16).sum()
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SplitLimits {
    /// Maximum `telegram_len` of one chunk; values below 2 are treated as 2.
    pub chunk_len: usize,
    /// More chunks than this sets `Chunks::prefer_file`.
    pub max_chunks: usize,
}

impl Default for SplitLimits {
    fn default() -> Self {
        Self {
            chunk_len: TELEGRAM_TEXT_LIMIT,
            max_chunks: 4,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Chunks {
    /// Non-blank pieces in order, each within `chunk_len`.
    pub chunks: Vec<String>,
    /// True when `chunks.len() > max_chunks`: send the whole text as a document instead.
    pub prefer_file: bool,
}

/// Splits at blank lines first, then at line breaks, then inside a line.
pub fn split_for_telegram(text: &str, limits: SplitLimits) -> Chunks {
    let limit = limits.chunk_len.max(2);
    let mut packer = Packer {
        limit,
        chunks: Vec::new(),
        current: String::new(),
        current_len: 0,
    };
    for paragraph in text.split("\n\n") {
        let len = telegram_len(paragraph);
        if len <= limit {
            packer.add(paragraph, len, "\n\n");
            continue;
        }
        for line in paragraph.split('\n') {
            let len = telegram_len(line);
            if len <= limit {
                packer.add(line, len, "\n");
                continue;
            }
            let mut rest = line;
            while !rest.is_empty() {
                let (piece, tail) = rest.split_at(hard_cut(rest, limit));
                packer.add(piece, telegram_len(piece), "");
                rest = tail;
            }
        }
    }
    packer.flush();
    let prefer_file = packer.chunks.len() > limits.max_chunks;
    Chunks {
        chunks: packer.chunks,
        prefer_file,
    }
}

struct Packer {
    limit: usize,
    chunks: Vec<String>,
    current: String,
    current_len: usize,
}

impl Packer {
    fn add(&mut self, piece: &str, len: usize, separator: &str) {
        if piece.trim().is_empty() {
            return;
        }
        if self.current.is_empty() {
            self.start(piece, len);
        } else if self.current_len + separator.len() + len <= self.limit {
            self.current.push_str(separator);
            self.current.push_str(piece);
            self.current_len += separator.len() + len;
        } else {
            self.flush();
            self.start(piece, len);
        }
    }

    fn start(&mut self, piece: &str, len: usize) {
        self.current.push_str(piece);
        self.current_len = len;
    }

    fn flush(&mut self) {
        if !self.current.trim().is_empty() {
            self.chunks.push(std::mem::take(&mut self.current));
        }
        self.current.clear();
        self.current_len = 0;
    }
}

/// Byte index (> 0, a char boundary) where a line longer than `limit` is cut.
/// Prefers just after the last whitespace in the second half of the window, and never cuts next to
/// an emoji joiner or modifier when a safe point exists nearby.
fn hard_cut(line: &str, limit: usize) -> usize {
    let mut units = 0;
    let mut end = 0;
    for (index, c) in line.char_indices() {
        if units + c.len_utf16() > limit {
            break;
        }
        units += c.len_utf16();
        end = index + c.len_utf8();
    }
    if end >= line.len() {
        return line.len();
    }
    let window = &line[..end];
    if let Some((index, c)) = window.char_indices().rev().find(|(_, c)| c.is_whitespace())
        && index >= end / 2
    {
        return index + c.len_utf8();
    }
    let mut cut = end;
    for _ in 0..16 {
        let before = line[..cut].chars().next_back();
        let at = line[cut..].chars().next();
        if !(before.is_some_and(is_joiner) || at.is_some_and(is_joiner)) {
            return cut;
        }
        match before {
            Some(c) if cut > c.len_utf8() => cut -= c.len_utf8(),
            _ => break,
        }
    }
    end
}

/// Characters that glue to a neighbour: ZWJ, variation selectors, keycap, skin tones, tags, combining marks.
fn is_joiner(c: char) -> bool {
    matches!(c,
        '\u{200D}' | '\u{FE00}'..='\u{FE0F}' | '\u{20E3}' | '\u{1F3FB}'..='\u{1F3FF}'
        | '\u{E0020}'..='\u{E007F}' | '\u{0300}'..='\u{036F}')
}
