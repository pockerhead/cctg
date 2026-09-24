# One-off edit script (TASK-027 implementer): adds `html` to Op::Send/Op::Stream
# constructions and `markdown` to hub stream Step::Send. Asserts every anchor once.
import sys

ROOT = sys.argv[1]
s = ""


def load(path):
    global s
    s = open(f"{ROOT}/{path}", encoding="utf-8").read()


def save(path):
    open(f"{ROOT}/{path}", "w", encoding="utf-8", newline="\n").write(s)


def rep(old, new, count=1):
    global s
    c = s.count(old)
    assert c == count, (old, c)
    s = s.replace(old, new)


load("crates/cctg/src/hub/slots.rs")
rep("use transcript::{SplitOptions, split_for_telegram};",
    "use transcript::{HtmlChunk, SplitOptions, split_for_telegram, split_markdown_for_telegram};")
rep("""                    text: buffer::resume_text(&session),
                    reply_markup,""", """                    text: buffer::resume_text(&session),
                    html: None,
                    reply_markup,""")
rep("""                text: prompt.text.clone(),
                reply_markup: Some(permissions::keyboard(&prompt.request_id)),""", """                text: prompt.text.clone(),
                html: None,
                reply_markup: Some(permissions::keyboard(&prompt.request_id)),""")
rep("""                } => Op::Send {
                    thread_id: Some(*thread_id),
                    text: text.clone(),
                    reply_markup: None,""", """                } => Op::Send {
                    thread_id: Some(*thread_id),
                    text: text.clone(),
                    html: None,
                    reply_markup: None,""")
rep("""fn message_op(thread_id: i64, text: String) -> Op {
    Op::Send {
        thread_id: Some(thread_id),
        text,
        reply_markup: None,""", """fn message_op(thread_id: i64, text: String) -> Op {
    Op::Send {
        thread_id: Some(thread_id),
        text,
        html: None,
        reply_markup: None,""")
rep("""                    Step::Send { text, merge } => {
                        let split = split_for_telegram(&text, SplitOptions::default());
                        let merge = merge && split.chunks.len() == 1;
                        for text in split.chunks {
                            queued += 1;
                            let op = Op::Stream {
                                thread_id,
                                text,
                                merge,""", """                    Step::Send {
                        text,
                        merge,
                        markdown,
                    } => {
                        let chunks = stream_chunks(&text, markdown);
                        let merge = merge && chunks.len() == 1;
                        for (text, html) in chunks {
                            queued += 1;
                            let op = Op::Stream {
                                thread_id,
                                text,
                                html,
                                merge,""")
rep("""        let split = split_for_telegram(text, SplitOptions::default());
        let ops: Vec<Op> = if split.prefer_file {""", """        let split = split_markdown_for_telegram(text, SplitOptions::default());
        let ops: Vec<Op> = if split.prefer_file {""")
rep("""            split
                .chunks
                .into_iter()
                .map(|chunk| message_op(thread_id, chunk))
                .collect()""", """            split
                .chunks
                .into_iter()
                .map(|chunk| Op::Send {
                    thread_id: Some(thread_id),
                    text: chunk.text,
                    html: Some(chunk.html),
                    reply_markup: None,
                    permission: false,
                })
                .collect()""")
rep("""    let split = split_for_telegram(&held.answer, SplitOptions::default());
    if split.prefer_file || split.chunks.len() > room {""", """    let split = split_markdown_for_telegram(&held.answer, SplitOptions::default());
    if split.prefer_file || split.chunks.len() > room {""")
rep("""    let op = |live: &mut Live, text| Op::Stream {
        thread_id,
        text,
        merge: false,""", """    let op = |live: &mut Live, chunk: HtmlChunk| Op::Stream {
        thread_id,
        text: chunk.text,
        html: Some(chunk.html),
        merge: false,""")
rep("""    for text in chunks {
        let message = op(live, text);""", """    for chunk in chunks {
        let message = op(live, chunk);""")
rep("""fn message_op(thread_id: i64, text: String) -> Op {""", """/// The messages of a stream line: markdown as HTML with its plain source,
/// anything else as plain text.
fn stream_chunks(text: &str, markdown: bool) -> Vec<(String, Option<String>)> {
    if markdown {
        split_markdown_for_telegram(text, SplitOptions::default())
            .chunks
            .into_iter()
            .map(|chunk| (chunk.text, Some(chunk.html)))
            .collect()
    } else {
        split_for_telegram(text, SplitOptions::default())
            .chunks
            .into_iter()
            .map(|text| (text, None))
            .collect()
    }
}

fn message_op(thread_id: i64, text: String) -> Op {""")
save("crates/cctg/src/hub/slots.rs")

load("crates/cctg/src/hub/commands.rs")
rep("""    let op = Op::Send {
        thread_id,
        text,
        reply_markup: None,""", """    let op = Op::Send {
        thread_id,
        text,
        html: None,
        reply_markup: None,""")
save("crates/cctg/src/hub/commands.rs")

load("crates/cctg/src/hub/stream.rs")
rep("""    /// A topic message; `merge`: a one-line tool call that may share a
    /// message with the next ones.
    Send { text: String, merge: bool },""", """    /// A topic message; `merge`: a one-line tool call that may share a
    /// message with the next ones; `markdown`: assistant text, sent as
    /// Telegram HTML.
    Send {
        text: String,
        merge: bool,
        markdown: bool,
    },""")
rep("""                steps.push(Step::Send {
                    text: format!("> {text}"),
                    merge: false,
                });""", """                steps.push(Step::Send {
                    text: format!("> {text}"),
                    merge: false,
                    markdown: false,
                });""")
rep("""                steps.push(Step::Send {
                    text: text.clone(),
                    merge: false,
                });""", """                steps.push(Step::Send {
                    text: text.clone(),
                    merge: false,
                    markdown: true,
                });""")
rep("""                steps.push(Step::Send {
                    text: format!("{THINKING} {text}"),
                    merge: true,
                });""", """                steps.push(Step::Send {
                    text: format!("{THINKING} {text}"),
                    merge: true,
                    markdown: true,
                });""")
rep("""    Step::Send { text, merge: true }
}""", """    Step::Send {
        text,
        merge: true,
        markdown: false,
    }
}""")
rep("""                Step::Send {
                    text: "> go".into(),
                    merge: false
                },
                Step::Send {
                    text: "• Bash: A ✓".into(),
                    merge: true
                },
                Step::Send {
                    text: "\\u{1F4AD} Checking cargo.".into(),
                    merge: true
                },""", """                Step::Send {
                    text: "> go".into(),
                    merge: false,
                    markdown: false,
                },
                Step::Send {
                    text: "• Bash: A ✓".into(),
                    merge: true,
                    markdown: false,
                },
                Step::Send {
                    text: "\\u{1F4AD} Checking cargo.".into(),
                    merge: true,
                    markdown: true,
                },""")
rep("""                Step::Send {
                    text: "• Bash: B ✓".into(),
                    merge: true
                },""", """                Step::Send {
                    text: "• Bash: B ✓".into(),
                    merge: true,
                    markdown: false,
                },""")
save("crates/cctg/src/hub/stream.rs")
print("ok")
