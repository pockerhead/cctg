use transcript::{StreamEvent, THINKING_LIMIT, parse, render_brief, render_full, stream_events};

const FINAL_ANSWER: &str = include_str!("fixtures/final_answer.jsonl");
const STREAM: &str = include_str!("fixtures/stream.jsonl");
const STREAM_THINKING: &str = include_str!("fixtures/stream_thinking.jsonl");

fn events(jsonl: &str) -> Vec<StreamEvent> {
    jsonl.lines().flat_map(stream_events).collect()
}

fn call(id: &str, line: &str) -> StreamEvent {
    StreamEvent::Call {
        id: id.to_owned(),
        line: line.to_owned(),
    }
}

fn ok(id: &str) -> StreamEvent {
    StreamEvent::Result {
        id: id.to_owned(),
        error: None,
    }
}

#[test]
fn a_turn_streams_its_prompt_notes_and_calls_but_not_its_final_answer() {
    assert_eq!(
        events(FINAL_ANSWER),
        [
            StreamEvent::Prompt("Check the build \u{1F680} and explain.".to_owned()),
            StreamEvent::Note("Let me run the tests first.".to_owned()),
            call("toolu_demo31", "• Bash: Run workspace tests"),
            ok("toolu_demo31"),
            // Thinking of the answering response is shown; only the final
            // text marks the end of the turn.
            StreamEvent::Thinking("SECRET-THINKING-MARKER final".to_owned()),
            StreamEvent::TurnEnd,
            // A Telegram message: its id, never its text.
            StreamEvent::Channel { message_id: 7 },
            call("toolu_demo32", "↳ Explore: Explore crate"),
            ok("toolu_demo32"),
            StreamEvent::TurnEnd,
        ]
    );
}

#[test]
fn queued_channel_messages_errors_and_service_records() {
    assert_eq!(
        events(STREAM),
        [
            StreamEvent::Prompt("/model opus".to_owned()),
            StreamEvent::Note("Editing the file now.".to_owned()),
            call("toolu_s1", "• Edit: src/lib.rs"),
            StreamEvent::Result {
                id: "toolu_s1".to_owned(),
                error: Some("String to replace not found in file.".to_owned()),
            },
            // Only the opening tag counts, not an id-looking body.
            StreamEvent::Channel { message_id: 12 },
            // Queue operations, a typed queued prompt, sidechain records, text
            // with no tool call after it (the answer comes from the Stop hook)
            // and a channel tag without a numeric id give nothing.
            // An interrupt is Claude Code's note, not a prompt.
            StreamEvent::Note("[Request interrupted by user]".to_owned()),
        ]
    );
    let for_tool = "{\"type\":\"user\",\"message\":{\"role\":\"user\",\"content\":[{\"type\":\"text\",\"text\":\"[Request interrupted by user for tool use]\"}]}}";
    assert_eq!(
        stream_events(for_tool),
        [StreamEvent::Note(
            "[Request interrupted by user for tool use]".to_owned()
        )]
    );
}

#[test]
fn only_cctg_channel_records_match_and_a_bom_line_still_counts() {
    let channel = |source: &str| {
        format!(
            "{{\"type\":\"user\",\"isMeta\":true,\"message\":{{\"role\":\"user\",\"content\":\"<channel source=\\\"{source}\\\" message_id=\\\"42\\\">hi</channel>\"}}}}"
        )
    };
    assert_eq!(
        stream_events(&channel("cctg")),
        [StreamEvent::Channel { message_id: 42 }]
    );
    for foreign in ["webhook", "plugin:fakechat:fakechat", "cctg2", ""] {
        assert!(stream_events(&channel(foreign)).is_empty(), "{foreign}");
    }
    let queued = "{\"type\":\"attachment\",\"attachment\":{\"type\":\"queued_command\",\"prompt\":\"<channel source=\\\"webhook\\\" message_id=\\\"42\\\">x</channel>\"}}";
    assert!(stream_events(queued).is_empty());
    let prompt = "{\"type\":\"user\",\"message\":{\"role\":\"user\",\"content\":\"first\"}}";
    assert_eq!(
        stream_events(&format!("\u{feff}{prompt}\r\n")),
        [StreamEvent::Prompt("first".to_owned())]
    );
}

#[test]
fn a_partial_or_foreign_line_gives_nothing() {
    let whole = FINAL_ANSWER.lines().nth(2).unwrap();
    assert!(stream_events(&whole[..whole.len() / 2]).is_empty());
    assert!(stream_events("").is_empty());
    assert!(stream_events("{\"type\":\"mode\",\"mode\":\"x\"}").is_empty());
    assert!(stream_events("[1,2]").is_empty());
}

/// TASK-025: visible thinking goes in turn order, trimmed and cut short;
/// empty (signature-only), redacted and sidechain thinking give nothing; a
/// call of cctg's own `reply` tool gives no line.
#[test]
fn thinking_streams_in_order_and_the_reply_call_does_not() {
    let long = "мысль ".repeat(300);
    let long = long.trim_end();
    assert!(STREAM_THINKING.contains(long));
    assert!(long.chars().count() > THINKING_LIMIT);
    let cut: String = long.chars().take(THINKING_LIMIT).collect();
    let cut = format!("{}\u{2026}", cut.trim_end());
    assert_eq!(
        events(STREAM_THINKING),
        [
            StreamEvent::Prompt("Which cargo processes are running?".to_owned()),
            StreamEvent::Thinking("Проверю, какие процессы cargo запущены.".to_owned()),
            StreamEvent::Note("Sending a status.".to_owned()),
            // No call line for `mcp__cctg__reply`; its result names a call
            // the hub never saw and shows nothing.
            ok("toolu_reply1"),
            call("toolu_bash1", "• Bash: List cargo processes"),
            ok("toolu_bash1"),
            StreamEvent::Thinking(cut.clone()),
            StreamEvent::TurnEnd,
        ]
    );
    assert_eq!(cut.chars().count(), THINKING_LIMIT + 1, "{cut}");
    // Another server's reply tool is a call like any other.
    let foreign = r#"{"type":"assistant","message":{"role":"assistant","stop_reason":"tool_use","content":[{"type":"tool_use","id":"t","name":"mcp__plugin_telegram_telegram__reply","input":{}}]}}"#;
    assert!(matches!(
        stream_events(foreign).as_slice(),
        [StreamEvent::Call { .. }]
    ));
    // `/brief` and `/full` never show thinking.
    let turns = parse(STREAM_THINKING);
    for shown in [render_brief(&turns), render_full(&turns)] {
        assert!(!shown.contains("процессы cargo"), "{shown}");
        assert!(!shown.contains("мысль"), "{shown}");
    }
}

#[test]
fn a_thinking_cut_never_splits_a_grapheme() {
    // A family emoji: one grapheme of five code points.
    let family = "\u{1F468}\u{200D}\u{1F469}\u{200D}\u{1F467}";
    let text = family.repeat(THINKING_LIMIT + 5);
    let line = format!(
        r#"{{"type":"assistant","message":{{"role":"assistant","stop_reason":"tool_use","content":[{{"type":"thinking","thinking":"{text}","signature":"s"}}]}}}}"#
    );
    assert_eq!(
        stream_events(&line),
        [StreamEvent::Thinking(format!(
            "{}\u{2026}",
            family.repeat(THINKING_LIMIT)
        ))]
    );
    // Whitespace-only thinking and a user record holding a thinking block
    // give nothing.
    let blank = r#"{"type":"assistant","message":{"role":"assistant","content":[{"type":"thinking","thinking":"  \n ","signature":"s"}]}}"#;
    assert!(stream_events(blank).is_empty());
    let user = r#"{"type":"user","message":{"role":"user","content":[{"type":"thinking","thinking":"x"}]}}"#;
    assert!(stream_events(user).is_empty());
}

#[test]
fn a_call_line_from_hook_input_matches_the_streamed_line() {
    // The status message shows a running call with the line the stream
    // shows when it ends (TASK-029): from the tool name and input alone.
    let input = serde_json::json!({ "command": "cargo test", "description": "Run the  tests\n" });
    assert_eq!(
        transcript::call_line("Bash", &input),
        "• Bash: Run the tests"
    );
    let agent = serde_json::json!({ "subagent_type": "Explore", "description": "find it" });
    assert_eq!(transcript::call_line("Agent", &agent), "↳ Explore: find it");
    assert_eq!(
        transcript::call_line("Read", &serde_json::Value::Null),
        "• Read"
    );
}

#[test]
fn terminal_bang_commands_and_local_command_output_stream_as_prompt_and_code() {
    let listing = (1..20)
        .map(|n| format!("line {n}"))
        .collect::<Vec<_>>()
        .join("\n");
    assert_eq!(
        events(include_str!("fixtures/console_commands.jsonl")),
        [
            StreamEvent::Prompt("! echo hi".to_owned()),
            StreamEvent::Note("```\nhi\n```".to_owned()),
            StreamEvent::Prompt("! git status".to_owned()),
            // stderr counts as output.
            StreamEvent::Note("```\nfatal: not a git repository\n```".to_owned()),
            // No output, no note.
            StreamEvent::Prompt("! true".to_owned()),
            StreamEvent::Prompt("/cost".to_owned()),
            // Colours dropped.
            StreamEvent::Note("```\nTotal cost: $0.12\nTotal duration: 3m\n```".to_owned()),
            StreamEvent::Prompt("! cat notes.md".to_owned()),
            // 20 lines at most; the fence is longer than any backtick run.
            StreamEvent::Note(format!("````\n```rust\n{listing}\n\u{2026}\n````")),
        ]
    );
}

#[test]
fn a_local_command_written_as_a_system_record_streams_as_prompt_and_code() {
    // Claude Code 2.1.282 writes `/context` and its output as `system`
    // records with `subtype: local_command`.
    let jsonl = concat!(
        r#"{"type":"system","subtype":"local_command","content":"<command-name>/context</command-name>\n            <command-message>context</command-message>\n            <command-args></command-args>","isSidechain":false,"isMeta":false}"#,
        "\n",
        r#"{"type":"system","subtype":"local_command","content":"<local-command-stdout> \u001b[1mContext Usage\u001b[22m\n142.8k/1m tokens (14%)</local-command-stdout>","isSidechain":false,"isMeta":false}"#,
        "\n",
        r#"{"type":"system","subtype":"local_command","content":"<local-command-stdout>hidden</local-command-stdout>","isSidechain":true}"#,
        "\n",
        r#"{"type":"system","subtype":"turn_duration","content":"<local-command-stdout>no</local-command-stdout>"}"#,
    );
    assert_eq!(
        events(jsonl),
        [
            StreamEvent::Prompt("/context".to_owned()),
            StreamEvent::Note("```\nContext Usage\n142.8k/1m tokens (14%)\n```".to_owned()),
        ]
    );
}
