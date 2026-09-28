//! Markdown to Telegram HTML (TASK-027).

use transcript::{
    SplitOptions, TELEGRAM_TEXT_LIMIT, escape_html, markdown_to_html, rich_markdown,
    split_markdown_for_telegram, telegram_len,
};

/// A real model answer (TASK-004 implementer summary, from this repository).
const ANSWER: &str = include_str!("fixtures/answer_markdown.md");

/// Tags `parse_mode: HTML` takes that the converter writes.
const TAGS: [&str; 7] = ["b", "i", "s", "code", "pre", "a", "blockquote"];

/// Fails unless `html` is what Telegram's HTML parser takes: known tags only, every tag closed in
/// order, no raw `<`, `>` or `&` in text, only the four named entities.
fn assert_valid(html: &str) {
    let mut open: Vec<&str> = Vec::new();
    let mut rest = html;
    while let Some(at) = rest.find(['<', '>', '&']) {
        let tail = &rest[at..];
        if tail.starts_with('>') {
            panic!("raw > in {html:?}");
        }
        if tail.starts_with('&') {
            let entity = ["&lt;", "&gt;", "&amp;", "&quot;"]
                .into_iter()
                .find(|entity| tail.starts_with(entity))
                .unwrap_or_else(|| panic!("raw & in {html:?}"));
            rest = &tail[entity.len()..];
            continue;
        }
        let end = tail.find('>').expect("tag closed");
        let tag = &tail[1..end];
        assert!(!tag.contains('<'), "raw < in {html:?}");
        if let Some(name) = tag.strip_prefix('/') {
            assert_eq!(open.pop(), Some(name), "closing order in {html:?}");
        } else {
            let name = tag.split(' ').next().unwrap_or_default();
            assert!(TAGS.contains(&name), "tag {tag:?} in {html:?}");
            match name {
                "a" => assert!(tag.starts_with("a href=\"") && tag.ends_with('"')),
                "code" if tag != "code" => assert!(tag.starts_with("code class=\"language-")),
                _ => assert_eq!(tag, name),
            }
            open.push(name);
        }
        rest = &tail[end + 1..];
    }
    assert!(open.is_empty(), "unclosed {open:?} in {html:?}");
}

#[test]
fn inline_and_block_markup_becomes_telegram_html() {
    let cases = [
        (
            "**bold** and *it* and `code`",
            "<b>bold</b> and <i>it</i> and <code>code</code>",
        ),
        ("a < b && c > d", "a &lt; b &amp;&amp; c &gt; d"),
        (
            "snake_case_name and __init__",
            "snake_case_name and <b>init</b>",
        ),
        ("2 * 3 * 4", "2 * 3 * 4"),
        (
            "[docs](https://example.com/a?b=1&c=\"x\")",
            "<a href=\"https://example.com/a?b=1&amp;c=&quot;x&quot;\">docs</a>",
        ),
        ("[local](file.md)", "[local](file.md)"),
        ("`a<b>`", "<code>a&lt;b&gt;</code>"),
        ("``code with ` tick``", "<code>code with ` tick</code>"),
        ("\\*not italic\\*", "*not italic*"),
        ("~~gone~~", "<s>gone</s>"),
        ("***both***", "<b><i>both</i></b>"),
        ("*italic **bold** more*", "<i>italic <b>bold</b> more</i>"),
        ("**unclosed", "**unclosed"),
        ("# Title", "<b>Title</b>"),
        ("## Title ##", "<b>Title</b>"),
        ("#hashtag", "#hashtag"),
        ("- item **x**", "- item <b>x</b>"),
        ("* star item", "* star item"),
        ("1. first", "1. first"),
        (
            "> quote\n> *more*",
            "<blockquote>quote\n<i>more</i></blockquote>",
        ),
        ("---", "---"),
        (
            "```rust\nfn a() -> u8 {}\n```",
            "<pre><code class=\"language-rust\">fn a() -&gt; u8 {}</code></pre>",
        ),
        ("```\nx < y\n```", "<pre>x &lt; y</pre>"),
        ("```py\nprint(1)", "```py\nprint(1)"),
        ("<https://x.y/z>", "https://x.y/z"),
        ("\u{1F4AD} **thinking**", "\u{1F4AD} <b>thinking</b>"),
        (
            "[Request interrupted by user]",
            "[Request interrupted by user]",
        ),
    ];
    for (markdown, html) in cases {
        let got = markdown_to_html(markdown);
        assert_eq!(got, html, "{markdown:?}");
        assert_valid(&got);
    }
}

#[test]
fn a_real_answer_renders_as_valid_html() {
    let html = markdown_to_html(ANSWER);
    assert_valid(&html);
    for want in [
        "<b>TASK-004 — IMPL_SUMMARY</b>",
        "<b>1. Что сделано</b>",
        "<code>scratch/FINDINGS.md</code>",
        "<b>подтверждено</b>",
        "<b>Стенд 2.1.280, а не 2.1.278.</b>",
        "<pre>cargo test --workspace</pre>",
        "<pre><code class=\"language-sh\">claude mcp add",
        "python &lt;repo&gt;/maw/",
        "- таблица 4 режимов",
        "| файл | строк | что это |",
    ] {
        assert!(html.contains(want), "{want:?} not in\n{html}");
    }
    // Shell comments inside the code block keep their `#`.
    assert!(html.contains("\n# интерактивно, канал работает"));
    for markup in ["**", "```", "## "] {
        assert!(!html.contains(markup), "{markup:?} left in\n{html}");
    }
}

#[test]
fn long_markdown_splits_into_valid_chunks_within_the_limit() {
    let block = format!("```rust\n{}```\n\n", "let x = a < b && c;\n".repeat(150));
    let text = format!("{}{block}{}", ANSWER, "**tail** & more\n".repeat(400));
    let split = split_markdown_for_telegram(&text, SplitOptions { max_chunks: 20 });
    assert!(split.chunks.len() >= 3, "{}", split.chunks.len());
    assert!(!split.prefer_file);
    for chunk in &split.chunks {
        assert!(telegram_len(&chunk.html) <= TELEGRAM_TEXT_LIMIT);
        assert!(telegram_len(&chunk.text) <= TELEGRAM_TEXT_LIMIT);
        assert_valid(&chunk.html);
    }
    // Nothing is lost: the slices are the input in order.
    let joined: String = split.chunks.iter().map(|c| c.text.as_str()).collect();
    assert_eq!(joined, text);
    // The code block cut between chunks goes on as code in the next one.
    let cut = split
        .chunks
        .windows(2)
        .find(|pair| pair[0].html.contains("let x") && pair[1].html.contains("let x"))
        .expect("the code block spans two chunks");
    assert!(cut[0].html.ends_with("</code></pre>"), "{}", cut[0].html);
    assert!(
        cut[1]
            .html
            .starts_with("<pre><code class=\"language-rust\">let x"),
        "{}",
        cut[1].html
    );
}

#[test]
fn escaping_that_grows_the_text_makes_the_chunks_smaller() {
    let text = "<&>".repeat(3000);
    let split = split_markdown_for_telegram(&text, SplitOptions::default());
    assert!(split.chunks.len() >= 3);
    for chunk in &split.chunks {
        assert!(telegram_len(&chunk.html) <= TELEGRAM_TEXT_LIMIT);
        assert_eq!(chunk.html, escape_html(&chunk.text));
    }
    let joined: String = split.chunks.iter().map(|c| c.text.as_str()).collect();
    assert_eq!(joined, text);
}

#[test]
fn short_text_is_one_chunk_and_many_chunks_prefer_a_file() {
    let one = split_markdown_for_telegram("**hi**", SplitOptions::default());
    assert_eq!(one.chunks.len(), 1);
    assert_eq!(one.chunks[0].text, "**hi**");
    assert_eq!(one.chunks[0].html, "<b>hi</b>");
    let blank = split_markdown_for_telegram("```\n\n```\n", SplitOptions::default());
    assert!(blank.chunks.is_empty());
    let many = split_markdown_for_telegram(&"word ".repeat(5000), SplitOptions::default());
    assert!(many.prefer_file);
}

fn assert_cases(cases: &[(&str, &str)]) {
    for (markdown, html) in cases {
        let got = markdown_to_html(markdown);
        assert_eq!(&got, html, "{markdown:?}");
        assert_valid(&got);
    }
}

#[test]
fn windows_paths_keep_their_backslashes_outside_code() {
    assert_cases(&[
        (
            r"C:\Users\user\.claude\projects",
            r"C:\Users\user\.claude\projects",
        ),
        (r"see C:\dev\_x and y_ now", r"see C:\dev\_x and y_ now"),
        (r"D:\_build\*.rs *ok*", r"D:\_build\*.rs <i>ok</i>"),
        (r"\\server\share\_x", r"\\server\share\_x"),
        (
            r"`C:\Users\user` and C:\Users\user",
            r"<code>C:\Users\user</code> and C:\Users\user",
        ),
        // Outside a path a backslash still escapes markup.
        (r"my\_var and \*x\*", "my_var and *x*"),
        (r"\# not a heading", "# not a heading"),
        (r"a \. b", r"a \. b"),
    ]);
}

#[test]
fn links_keep_balanced_parentheses_in_the_url() {
    assert_cases(&[
        (
            "[wiki](https://en.wikipedia.org/wiki/Rust_(language))",
            "<a href=\"https://en.wikipedia.org/wiki/Rust_(language)\">wiki</a>",
        ),
        (
            "([a](https://x.y/z)) b",
            "(<a href=\"https://x.y/z\">a</a>) b",
        ),
        ("[a](https://x.y/(z", "[a](https://x.y/(z"),
    ]);
}

#[test]
fn a_fence_opens_a_block_only_when_it_closes_later() {
    assert_cases(&[
        (
            "~~~~~~~~\ntext **b**\nmore",
            "~~~~~~~~\ntext <b>b</b>\nmore",
        ),
        ("```\nno close *i*", "```\nno close <i>i</i>"),
        ("~~~\ncode\n~~~\n**after**", "<pre>code</pre>\n<b>after</b>"),
        // A shorter fence line does not close a longer one.
        ("````\n**x**\n```", "````\n<b>x</b>\n```"),
    ]);
    // The closing line may be in a later chunk: the block still opens.
    let text = format!("```\n{}```\nend", "x < y\n".repeat(1000));
    let split = split_markdown_for_telegram(&text, SplitOptions { max_chunks: 10 });
    assert!(split.chunks.len() >= 2);
    assert!(
        split.chunks[0].html.starts_with("<pre>x &lt; y"),
        "{}",
        split.chunks[0].html
    );
    for chunk in &split.chunks {
        assert_valid(&chunk.html);
    }
}

#[test]
fn a_quote_needs_a_space_after_the_marker() {
    assert_cases(&[
        (">= 5 items", "&gt;= 5 items"),
        ("> a\n>\n> b", "<blockquote>a\n\nb</blockquote>"),
        (">>x", "&gt;&gt;x"),
    ]);
}

#[test]
fn pathological_input_converts_in_bounded_work() {
    // Each of these made the delimiter search quadratic (unclosed openers, each scanning the
    // rest of the line): 60k chars on one line took about a second in debug.
    let inputs = [
        "*a ".repeat(20_000),
        "_a ".repeat(20_000),
        "**a ".repeat(15_000),
        "~~a ".repeat(15_000),
        "[".repeat(60_000),
        "[a](x".repeat(12_000),
        "<https://".repeat(7_000),
        "*a `b ".repeat(10_000),
        r"C:\x\_".repeat(10_000),
    ];
    for input in &inputs {
        assert_valid(&markdown_to_html(input));
        let split = split_markdown_for_telegram(input, SplitOptions::default());
        for chunk in &split.chunks {
            assert!(telegram_len(&chunk.html) <= TELEGRAM_TEXT_LIMIT);
            assert_valid(&chunk.html);
        }
        let joined: String = split.chunks.iter().map(|c| c.text.as_str()).collect();
        assert_eq!(&joined, input);
    }
    // Ordinary markup after a few unclosed openers still converts.
    let line = format!("{}**done**", "*open ".repeat(20));
    assert!(markdown_to_html(&line).ends_with("<b>done</b>"));
}

// rich_markdown (TASK-075)

#[test]
fn rich_markdown_escapes_every_lt_that_is_no_allowed_tag() {
    assert_eq!(rich_markdown("a Vec<String> b"), "a Vec&lt;String> b");
    assert_eq!(
        rich_markdown(r#"<channel source="cctg" message_id=5> and </div>"#),
        r#"&lt;channel source="cctg" message_id=5> and &lt;/div>"#
    );
    assert_eq!(rich_markdown("a < b"), "a &lt; b");
    // A valid tag by HTML syntax (`p` with attributes `and`, `j`) that Telegram would drop.
    assert_eq!(rich_markdown("if i<p and j>0"), "if i&lt;p and j>0");
    assert_eq!(
        rich_markdown(r#"<b onclick="x">y</b>"#),
        r#"&lt;b onclick="x">y</b>"#
    );
    assert_eq!(rich_markdown("<!-- c -->"), "&lt;!-- c -->");
    assert_eq!(rich_markdown("see <https://x.y>"), "see &lt;https://x.y>");
    assert_eq!(
        rich_markdown("<!DOCTYPE html> <?xml?>"),
        "&lt;!DOCTYPE html> &lt;?xml?>"
    );
    // A tag not closed on its line.
    assert_eq!(rich_markdown("<b\n>x"), "&lt;b\n>x");
}

#[test]
fn rich_markdown_keeps_complete_allowed_tags() {
    for text in [
        "a<br>b",
        "a<br/>b<br />c",
        "x<sup>2</sup>",
        "<details open><summary>s</summary>t</details>",
        "</b>",
        r#"<a href="https://x">y</a>"#,
        "<a href='https://x'>y</a>",
        "<blockquote expandable>q</blockquote>",
        r#"<code class="language-rust">x</code>"#,
        r#"<ol start=3 reversed><li>a</li></ol>"#,
        r#"<td align="right" colspan=2>c</td>"#,
        "<tg-spoiler>s</tg-spoiler> <h1>t</h1>",
    ] {
        assert_eq!(rich_markdown(text), text);
    }
}

#[test]
fn rich_markdown_leaves_code_alone() {
    for text in [
        "use `Vec<T>` here",
        "``a ` <b> `` and `x`",
        "```rust\nlet v: Vec<u8> = a < b;\n```\nafter",
        "~~~\n<div>\n~~~",
        "   ```\n<x>\n   ```",
        "````\n```\n<x>\n````",
    ] {
        assert_eq!(rich_markdown(text), text, "{text:?}");
    }
    // Four spaces: no fence here, so the `<` outside code is escaped.
    assert_eq!(rich_markdown("    ```\n<x>"), "    ```\n&lt;x>");
    // A closing run needs the same marker, at least as long, nothing after it.
    assert_eq!(
        rich_markdown("```\n~~~\n``` x\n<y>\n```\n<z>"),
        "```\n~~~\n``` x\n<y>\n```\n&lt;z>"
    );
    // A backtick after a backtick fence opener: a code span, not a fence.
    assert_eq!(rich_markdown("```a`b\n<x>"), "```a`b\n&lt;x>");
    // An unmatched backtick run is text: the `<` after it is escaped.
    assert_eq!(rich_markdown("a ` b <c"), "a ` b &lt;c");
}

#[test]
fn rich_markdown_closes_a_fence_left_open_at_the_end() {
    assert_eq!(
        rich_markdown("text\n```rust\nlet a = b<c>;"),
        "text\n```rust\nlet a = b<c>;\n```"
    );
    assert_eq!(rich_markdown("~~~~\n<x>\n"), "~~~~\n<x>\n~~~~");
}

#[test]
fn rich_markdown_backslash_lt_becomes_an_entity_other_escapes_stay() {
    assert_eq!(rich_markdown(r"\<String>"), "&lt;String>");
    assert_eq!(
        rich_markdown(r"\* not a list \_x\_ \$5"),
        r"\* not a list \_x\_ \$5"
    );
    assert_eq!(rich_markdown(r"\`<b>` x"), r"\`<b>` x");
}

#[test]
fn rich_markdown_without_lt_or_fence_is_the_input() {
    for text in [
        "",
        "plain text",
        "# Title\n\n| a | b |\n|---|:-:|\n| 1 | 2 |\n\n- one\n  - two\n    - three",
        "a > b & c &amp; **bold** $x$",
        "кириллица 🙂 ~~strike~~",
    ] {
        assert_eq!(rich_markdown(text), text);
    }
    assert_eq!(rich_markdown(ANSWER).replace("&lt;", "<"), ANSWER);
}

#[test]
fn rich_markdown_tag_names_are_lowercase_so_generics_stay_text() {
    assert_eq!(
        rich_markdown("hyper Request<B>, tower Service<S>, Foo<U>, Cow<A>, Wrap<P>, Vec<I> end"),
        "hyper Request&lt;B>, tower Service&lt;S>, Foo&lt;U>, Cow&lt;A>, Wrap&lt;P>, Vec&lt;I> end"
    );
    assert_eq!(rich_markdown("<H1>t</H1>"), "&lt;H1>t&lt;/H1>");
    assert_eq!(rich_markdown("<b>x</b> <i>y</i>"), "<b>x</b> <i>y</i>");
}

#[test]
fn rich_markdown_reads_fences_inside_list_items_and_quotes() {
    for (text, expected) in [
        // A fence opened on the marker line: code up to its closing line, prose after it.
        (
            "- ```rust\n  let a: Vec<u8>;\n  ```\n\nThen Vec<String> is returned.",
            "- ```rust\n  let a: Vec<u8>;\n  ```\n\nThen Vec&lt;String> is returned.",
        ),
        (
            "1. ```sh\n   echo <x>\n   ```\n2. Returns Option<T>.",
            "1. ```sh\n   echo <x>\n   ```\n2. Returns Option&lt;T>.",
        ),
        (
            "> ```\n> Vec<T>\n> ```\nafter Vec<U>",
            "> ```\n> Vec<T>\n> ```\nafter Vec&lt;U>",
        ),
        // The usual step with its code below it.
        (
            "1. Step\n   ```sh\n   cmd <x>\n   ```\n2. Next <y>",
            "1. Step\n   ```sh\n   cmd <x>\n   ```\n2. Next &lt;y>",
        ),
        // A fence ends with its container.
        (
            "> ```\n> Vec<T>\nafter Vec<U>",
            "> ```\n> Vec<T>\nafter Vec&lt;U>",
        ),
        (
            "- ```\n  a<b>\n\n  c<d>\nafter <e>",
            "- ```\n  a<b>\n\n  c<d>\nafter &lt;e>",
        ),
        // Left open at the end: closed inside its containers.
        ("- ```\n  Vec<T>", "- ```\n  Vec<T>\n  ```"),
        ("> ```\n> a<b\n", "> ```\n> a<b\n> ```"),
    ] {
        assert_eq!(rich_markdown(text), expected, "{text:?}");
    }
}

#[test]
fn rich_markdown_code_in_nested_containers_stays_code() {
    for text in [
        "- a\n   - b\n     ```\n     Vec<T>\n     ```\n",
        "- a\n  - b\n    ```\n    Vec<T>\n    ```",
        "- > ```\n  > Vec<T>\n  > ```",
        "> - x\n>   ```\n>   Vec<T>\n>   ```",
    ] {
        assert_eq!(rich_markdown(text), text, "{text:?}");
    }
    assert_eq!(
        rich_markdown("10. x\n    ```\n    Vec<T>\n    ```\nafter <z>"),
        "10. x\n    ```\n    Vec<T>\n    ```\nafter &lt;z>"
    );
}

#[test]
fn rich_markdown_code_spans_go_on_within_a_paragraph() {
    assert_eq!(
        rich_markdown("Run `cargo\ntest` then read Vec<String> and `x` here."),
        "Run `cargo\ntest` then read Vec&lt;String> and `x` here."
    );
    assert_eq!(
        rich_markdown("- run `a\n  b<c>` and d<e>"),
        "- run `a\n  b<c>` and d&lt;e>"
    );
    // Not across a blank line, a new item or a table row.
    assert_eq!(rich_markdown("a `b\n\nc<d> `e`"), "a `b\n\nc&lt;d> `e`");
    assert_eq!(rich_markdown("- a `b\n- c<d> `e`"), "- a `b\n- c&lt;d> `e`");
    assert_eq!(
        rich_markdown("| a ` | b |\n| c<d> ` | e |"),
        "| a ` | b |\n| c&lt;d> ` | e |"
    );
}
