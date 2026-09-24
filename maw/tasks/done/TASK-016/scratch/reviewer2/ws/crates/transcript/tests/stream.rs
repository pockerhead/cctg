use transcript::{StreamEvent, stream_events};

const FINAL_ANSWER: &str = include_str!("fixtures/final_answer.jsonl");
const STREAM: &str = include_str!("fixtures/stream.jsonl");

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
            // The final text only marks the end of the turn (thinking of the
            // same response does not).
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
            StreamEvent::Prompt("[Request interrupted by user]".to_owned()),
        ]
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
