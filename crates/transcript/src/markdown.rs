//! Markdown of assistant text as Telegram HTML (`parse_mode: HTML`).
//!
//! A small converter for the subset models write: `**bold**` / `__bold__`, `*italic*` /
//! `_italic_`, `~~strike~~`, inline code, fenced code blocks (the language is kept), `[text](url)`
//! links, `#` headings (bold), `>` quotes. List markers, tables and rules stay as they are. All
//! other text is escaped (`&`, `<`, `>`). Every tag is closed where it is opened, so any piece
//! converted on its own is valid HTML; a code block cut between two messages is closed at the end
//! of the first and opened again at the start of the next.

use crate::split::{SplitOptions, TELEGRAM_TEXT_LIMIT, cut, telegram_len};

/// A code block language longer than this, or with other characters, is dropped.
const MAX_LANG: usize = 32;
/// The smallest cut tried when the HTML of a piece is over the limit; one char always fits.
const MIN_LIMIT: usize = 2;
/// Scan steps the inline markup of one line may take per byte (plus [`MIN_STEPS`]); past that the
/// rest of the line is plain text. Keeps pathological input (many unclosed openers) linear.
const STEPS_PER_BYTE: usize = 32;
const MIN_STEPS: usize = 1024;
/// Characters a backslash escapes: the ones this converter reads as markup.
const ESCAPABLE: &[u8] = b"\\`*_~[]#<>";

/// One Telegram message of markdown text.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HtmlChunk {
    /// The source slice: sent as plain text when Telegram refuses the HTML.
    pub text: String,
    /// The HTML of `text`, within `TELEGRAM_TEXT_LIMIT` by `telegram_len`.
    pub html: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HtmlSplit {
    /// Consecutive slices of the input with their HTML; slices with blank HTML are dropped.
    pub chunks: Vec<HtmlChunk>,
    /// True when `chunks.len() > max_chunks`: send the whole text as a document instead.
    pub prefer_file: bool,
}

/// An open fenced code block.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Fence {
    marker: u8,
    len: usize,
    lang: Option<String>,
}

/// `&`, `<` and `>` as HTML entities: plain text for `parse_mode: HTML`.
pub fn escape_html(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    for c in text.chars() {
        match c {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            _ => out.push(c),
        }
    }
    out
}

/// The whole of `text` as Telegram HTML, with no length limit.
pub fn markdown_to_html(text: &str) -> String {
    render(text, 0, &Closers::new(text), &mut None)
}

/// Cuts markdown `text` like [`crate::split_for_telegram`] and converts each slice, so that the
/// HTML of every slice is within the limit (the cut is made smaller until it is) and a code block
/// open at the end of one slice goes on in the next.
pub fn split_markdown_for_telegram(text: &str, options: SplitOptions) -> HtmlSplit {
    let closers = Closers::new(text);
    let mut chunks = Vec::new();
    let mut fence = None;
    let mut rest = text;
    while !rest.is_empty() {
        let mut limit = TELEGRAM_TEXT_LIMIT;
        let base = text.len() - rest.len();
        let (at, html, next) = loop {
            let at = cut(rest, limit);
            let mut next = fence.clone();
            let html = render(rest.get(..at).unwrap_or(rest), base, &closers, &mut next);
            let len = telegram_len(&html);
            if len <= TELEGRAM_TEXT_LIMIT || limit <= MIN_LIMIT {
                break (at, html, next);
            }
            limit = (limit * TELEGRAM_TEXT_LIMIT / len).clamp(MIN_LIMIT, limit - 1);
        };
        let (chunk, tail) = rest.split_at(at);
        if !html.trim().is_empty() {
            chunks.push(HtmlChunk {
                text: chunk.to_owned(),
                html,
            });
        }
        fence = next;
        rest = tail;
    }
    let prefer_file = chunks.len() > options.max_chunks;
    HtmlSplit {
        chunks,
        prefer_file,
    }
}

/// The lines of the whole message that can close a fenced code block (only backticks or only
/// tildes, at least three). A fence opens a block only when such a line comes later, so an
/// unclosed fence or a lone `~~~~` rule stays text instead of swallowing the rest.
struct Closers {
    /// Per marker (backtick, tilde): line offsets in order and, from each index on, the longest
    /// closing line.
    lines: [(Vec<usize>, Vec<usize>); 2],
}

impl Closers {
    fn new(text: &str) -> Self {
        let mut lines: [(Vec<usize>, Vec<usize>); 2] = Default::default();
        let mut offset = 0;
        for line in text.split('\n') {
            let body = line.trim().as_bytes();
            if let Some(slot) = body.first().and_then(|&b| marker_slot(b))
                && body.len() >= 3
                && body.iter().all(|&b| b == body[0])
            {
                lines[slot].0.push(offset);
                lines[slot].1.push(body.len());
            }
            offset += line.len() + 1;
        }
        for (_, longest) in &mut lines {
            for index in (1..longest.len()).rev() {
                longest[index - 1] = longest[index - 1].max(longest[index]);
            }
        }
        Self { lines }
    }

    /// True when a line starting after byte `offset` of the message closes `fence`.
    fn after(&self, offset: usize, fence: &Fence) -> bool {
        let Some(slot) = marker_slot(fence.marker) else {
            return false;
        };
        let (offsets, longest) = &self.lines[slot];
        let from = offsets.partition_point(|&at| at <= offset);
        longest.get(from).is_some_and(|&len| len >= fence.len)
    }
}

fn marker_slot(marker: u8) -> Option<usize> {
    match marker {
        b'`' => Some(0),
        b'~' => Some(1),
        _ => None,
    }
}

/// Converts `text`, which starts at byte `base` of the message, line by line. `fence` is the code
/// block open at its start and, on return, the one still open at its end (closed in the output
/// either way).
fn render(text: &str, base: usize, closers: &Closers, fence: &mut Option<Fence>) -> String {
    let mut out: Vec<String> = Vec::new();
    let mut code: Vec<&str> = Vec::new();
    let mut quote: Vec<&str> = Vec::new();
    let mut offset = base;
    for line in text.split('\n') {
        let at = offset;
        offset += line.len() + 1;
        let line = line.strip_suffix('\r').unwrap_or(line);
        if let Some(open) = fence.as_ref() {
            if closes(line, open) {
                push_code(&mut out, open, &code);
                code.clear();
                *fence = None;
            } else {
                code.push(line);
            }
            continue;
        }
        if let Some(content) = quoted(line) {
            quote.push(content);
            continue;
        }
        push_quote(&mut out, &mut quote);
        if let Some(open) = opening(line)
            && closers.after(at, &open)
        {
            *fence = Some(open);
            continue;
        }
        out.push(render_line(line));
    }
    push_quote(&mut out, &mut quote);
    if let Some(open) = fence.as_ref() {
        push_code(&mut out, open, &code);
    }
    out.join("\n")
}

fn opening(line: &str) -> Option<Fence> {
    let body = line.trim_start();
    let marker = *body.as_bytes().first()?;
    if marker != b'`' && marker != b'~' {
        return None;
    }
    let len = run(body.as_bytes(), 0);
    if len < 3 {
        return None;
    }
    let info = body.get(len..)?.trim();
    if marker == b'`' && info.contains('`') {
        return None;
    }
    let lang = info
        .split_whitespace()
        .next()
        .filter(|word| {
            word.len() <= MAX_LANG
                && word
                    .bytes()
                    .all(|b| b.is_ascii_alphanumeric() || b"_+#.-".contains(&b))
        })
        .map(str::to_owned);
    Some(Fence { marker, len, lang })
}

fn closes(line: &str, fence: &Fence) -> bool {
    let body = line.trim();
    body.len() >= fence.len && body.bytes().all(|b| b == fence.marker)
}

/// A code block; one with only blank lines is left out (Telegram refuses empty entities).
fn push_code(out: &mut Vec<String>, fence: &Fence, lines: &[&str]) {
    if lines.iter().all(|line| line.trim().is_empty()) {
        return;
    }
    let body = escape_html(&lines.join("\n"));
    out.push(match &fence.lang {
        Some(lang) => format!("<pre><code class=\"language-{lang}\">{body}</code></pre>"),
        None => format!("<pre>{body}</pre>"),
    });
}

/// The text of a quote line: `> text` or a bare `>`. `>= 5` is not a quote.
fn quoted(line: &str) -> Option<&str> {
    let rest = line.trim_start().strip_prefix('>')?;
    match rest.strip_prefix([' ', '\t']) {
        Some(text) => Some(text),
        None => rest.trim().is_empty().then_some(""),
    }
}

fn push_quote(out: &mut Vec<String>, quote: &mut Vec<&str>) {
    if quote.is_empty() {
        return;
    }
    let body = quote
        .iter()
        .map(|line| render_line(line))
        .collect::<Vec<_>>()
        .join("\n");
    if body.trim().is_empty() {
        out.extend(quote.iter().map(|_| "&gt;".to_owned()));
    } else {
        out.push(format!("<blockquote>{body}</blockquote>"));
    }
    quote.clear();
}

fn render_line(line: &str) -> String {
    let budget = &mut (STEPS_PER_BYTE * line.len() + MIN_STEPS);
    let body = line.trim_start();
    let (indent, body) = line.split_at(line.len() - body.len());
    if let Some(title) = heading(body) {
        return if title.is_empty() {
            String::new()
        } else {
            format!("<b>{}</b>", inline(title, budget))
        };
    }
    if is_rule(body) {
        return escape_html(line);
    }
    let (marker, rest) = body.split_at(list_marker(body));
    format!(
        "{}{}{}",
        escape_html(indent),
        escape_html(marker),
        inline(rest, budget)
    )
}

/// The title of an ATX heading (`# Title`, closing `#`s dropped).
fn heading(body: &str) -> Option<&str> {
    let level = run(body.as_bytes(), 0);
    if !body.starts_with('#') || level > 6 {
        return None;
    }
    let rest = body.get(level..)?;
    if !rest.is_empty() && !rest.starts_with([' ', '\t']) {
        return None;
    }
    let rest = rest.trim();
    let stripped = rest.trim_end_matches('#');
    Some(if stripped.is_empty() || stripped.ends_with([' ', '\t']) {
        stripped.trim_end()
    } else {
        rest
    })
}

fn is_rule(body: &str) -> bool {
    let marks: Vec<char> = body.chars().filter(|c| !c.is_whitespace()).collect();
    marks.len() >= 3
        && matches!(marks.first(), Some('-' | '*' | '_'))
        && marks.windows(2).all(|pair| pair[0] == pair[1])
}

/// Byte length of a list marker with its space (`- `, `* `, `+ `, `12. `, `3) `), or 0.
fn list_marker(body: &str) -> usize {
    let bytes = body.as_bytes();
    if bytes.len() >= 2 && matches!(bytes[0], b'-' | b'*' | b'+') && bytes[1] == b' ' {
        return 2;
    }
    let digits = bytes.iter().take_while(|b| b.is_ascii_digit()).count();
    if (1..=9).contains(&digits)
        && matches!(bytes.get(digits), Some(b'.' | b')'))
        && bytes.get(digits + 1) == Some(&b' ')
    {
        return digits + 2;
    }
    0
}

/// Takes `n` steps from `budget`; `None` (and an empty budget) when it has fewer.
fn spend(budget: &mut usize, n: usize) -> Option<()> {
    match budget.checked_sub(n) {
        Some(left) => {
            *budget = left;
            Some(())
        }
        None => {
            *budget = 0;
            None
        }
    }
}

/// Inline markup of one line. Positions it cuts at are always ASCII bytes, so char boundaries.
/// Once `budget` is spent, the rest of `s` is plain text.
fn inline(s: &str, budget: &mut usize) -> String {
    let bytes = s.as_bytes();
    let mut out = String::with_capacity(s.len());
    let (mut i, mut plain) = (0, 0);
    // End of the word the last escaping backslash was in, and whether that word is a path.
    let mut word = (0, false);
    while i < bytes.len() {
        if spend(budget, 1).is_none() {
            break;
        }
        let found = match bytes[i] {
            b'\\' => match bytes.get(i + 1) {
                Some(next) if ESCAPABLE.contains(next) => {
                    if i >= word.0 {
                        word = path_word(bytes, i);
                    }
                    // A path keeps its backslash; either way the next char is not markup.
                    let from = if word.1 { i } else { i + 1 };
                    Some((i + 2, escape_html(&s[from..i + 2])))
                }
                _ => None,
            },
            b'`' => code_span(s, i, budget),
            b'[' => link(s, i, budget),
            b'<' => autolink(s, i, budget),
            b'*' | b'_' | b'~' => emphasis(s, i, budget),
            _ => None,
        };
        match found {
            Some((end, html)) => {
                out.push_str(&escape_html(&s[plain..i]));
                out.push_str(&html);
                i = end;
                plain = end;
            }
            // A run of delimiters that opens nothing is text as a whole.
            None if matches!(bytes[i], b'`' | b'*' | b'_' | b'~') => i += run(bytes, i),
            None => i += 1,
        }
    }
    out.push_str(&escape_html(&s[plain..]));
    out
}

/// End of the whitespace-delimited word around byte `i`, and whether it is a Windows path: it has
/// a drive (`C:\`) or a backslash before a letter or digit (`C:\dev\_x`, `\\server\share`). In a
/// path a backslash is a separator, not an escape, so `C:\dev\_x` stays as written while `my\_var`
/// becomes `my_var`.
fn path_word(bytes: &[u8], i: usize) -> (usize, bool) {
    let start = bytes[..i]
        .iter()
        .rposition(u8::is_ascii_whitespace)
        .map_or(0, |at| at + 1);
    let end = bytes[i..]
        .iter()
        .position(u8::is_ascii_whitespace)
        .map_or(bytes.len(), |at| i + at);
    let word = &bytes[start..end];
    let separator = word
        .windows(2)
        .any(|pair| pair[0] == b'\\' && pair[1].is_ascii_alphanumeric());
    let drive = word
        .windows(3)
        .any(|w| w[0].is_ascii_alphabetic() && w[1] == b':' && w[2] == b'\\');
    (end, separator || drive)
}

/// Length of the run of the byte at `at`.
fn run(bytes: &[u8], at: usize) -> usize {
    bytes
        .get(at)
        .map_or(0, |&b| bytes[at..].iter().take_while(|&&x| x == b).count())
}

/// Start of the next run of exactly `n` backticks at or after `from`.
fn code_end(bytes: &[u8], from: usize, n: usize, budget: &mut usize) -> Option<usize> {
    let mut j = from;
    while j < bytes.len() {
        spend(budget, 1)?;
        if bytes[j] == b'`' {
            let r = run(bytes, j);
            if r == n {
                return Some(j);
            }
            j += r;
        } else {
            j += 1;
        }
    }
    None
}

fn code_span(s: &str, i: usize, budget: &mut usize) -> Option<(usize, String)> {
    let n = run(s.as_bytes(), i);
    let close = code_end(s.as_bytes(), i + n, n, budget)?;
    let content = &s[i + n..close];
    if content.trim().is_empty() {
        return None;
    }
    let content = match content.strip_prefix(' ').and_then(|c| c.strip_suffix(' ')) {
        Some(inner) if !inner.is_empty() => inner,
        _ => content,
    };
    Some((close + n, format!("<code>{}</code>", escape_html(content))))
}

/// `[text](url)`; parentheses inside the url count when balanced (`.../Rust_(language)`).
fn link(s: &str, i: usize, budget: &mut usize) -> Option<(usize, String)> {
    let bytes = s.as_bytes();
    let mut close = i + 1;
    while *bytes.get(close)? != b']' {
        spend(budget, 1)?;
        if bytes[close] == b'[' {
            return None;
        }
        close += 1;
    }
    let label = &s[i + 1..close];
    if bytes.get(close + 1) != Some(&b'(') {
        return None;
    }
    let (start, mut end, mut depth) = (close + 2, close + 2, 0usize);
    loop {
        spend(budget, 1)?;
        match *bytes.get(end)? {
            b'(' => depth += 1,
            b')' if depth == 0 => break,
            b')' => depth -= 1,
            _ => {}
        }
        end += 1;
    }
    let url = s[start..end].trim();
    if label.trim().is_empty() || url.contains(char::is_whitespace) || !allowed_url(url) {
        return None;
    }
    Some((
        end + 1,
        format!(
            "<a href=\"{}\">{}</a>",
            escape_attr(url),
            inline(label, budget)
        ),
    ))
}

/// `<https://...>`: the address as text (Telegram links it by itself).
fn autolink(s: &str, i: usize, budget: &mut usize) -> Option<(usize, String)> {
    let rest = s.get(i + 1..)?;
    if !(rest.starts_with("http://") || rest.starts_with("https://")) {
        return None;
    }
    let end = rest.find('>');
    spend(budget, end.map_or(rest.len(), |end| end + 1))?;
    let url = &rest[..end?];
    if url.contains(char::is_whitespace) || url.contains('<') {
        return None;
    }
    Some((i + 1 + url.len() + 1, escape_html(url)))
}

fn allowed_url(url: &str) -> bool {
    let lower = url.to_ascii_lowercase();
    ["http://", "https://", "tg://", "mailto:"]
        .iter()
        .any(|scheme| lower.starts_with(scheme) && lower.len() > scheme.len())
}

fn escape_attr(text: &str) -> String {
    escape_html(text).replace('"', "&quot;")
}

/// `**b**`, `__b__`, `*i*`, `_i_`, `~~s~~` opening at `i`. A run of three opens bold with italic
/// inside. `_` does not open or close inside a word (`snake_case`).
fn emphasis(s: &str, i: usize, budget: &mut usize) -> Option<(usize, String)> {
    let bytes = s.as_bytes();
    let c = bytes[i];
    let r = run(bytes, i);
    let (n, tag) = match (c, r) {
        (b'~', 1) => return None,
        (b'~', _) => (2, "s"),
        (_, 1) => (1, "i"),
        _ => (2, "b"),
    };
    let open_end = i + r;
    if s[open_end..].chars().next().is_none_or(char::is_whitespace) {
        return None;
    }
    if c == b'_'
        && s[..i]
            .chars()
            .next_back()
            .is_some_and(char::is_alphanumeric)
    {
        return None;
    }
    let close = closing(s, open_end, c, n, budget)?;
    let inner = &s[i + n..close];
    Some((
        close + n,
        format!("<{tag}>{}</{tag}>", inline(inner, budget)),
    ))
}

/// Start of the `n` closing delimiters `c` after `from`, skipping code spans and escapes.
fn closing(s: &str, from: usize, c: u8, n: usize, budget: &mut usize) -> Option<usize> {
    let bytes = s.as_bytes();
    let mut j = from;
    while j < bytes.len() {
        spend(budget, 1)?;
        let b = bytes[j];
        if b == b'\\' {
            j += 2;
        } else if b == b'`' {
            let r = run(bytes, j);
            j = code_end(bytes, j + r, r, budget).map_or(j + r, |end| end + r);
        } else if b == c {
            let r = run(bytes, j);
            let fits = if n == 1 { r == 1 || r >= 3 } else { r >= 2 };
            let before = s[..j].chars().next_back().is_some_and(char::is_whitespace);
            let word_after =
                c == b'_' && s[j + r..].chars().next().is_some_and(char::is_alphanumeric);
            if fits && !before && !word_after {
                return Some(j + r - n);
            }
            j += r;
        } else {
            j += 1;
        }
    }
    None
}
