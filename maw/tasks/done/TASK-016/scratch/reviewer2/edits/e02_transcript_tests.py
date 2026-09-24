import os, sys
sys.path.insert(0, os.path.dirname(__file__))
from ed import edit
WS = os.path.join(os.path.dirname(__file__), '..', 'ws')
edit(os.path.join(WS, 'crates/transcript/tests/stream.rs'), [
(r'''            call("toolu_demo31", "• Bash: Run workspace tests"),
            ok("toolu_demo31"),
            // A Telegram message: its id, never its text.
            StreamEvent::Channel { message_id: 7 },
            call("toolu_demo32", "↳ Explore: Explore crate"),
            ok("toolu_demo32"),
        ]''', r'''            call("toolu_demo31", "• Bash: Run workspace tests"),
            ok("toolu_demo31"),
            // The final text only marks the end of the turn (thinking of the
            // same response does not).
            StreamEvent::TurnEnd,
            // A Telegram message: its id, never its text.
            StreamEvent::Channel { message_id: 7 },
            call("toolu_demo32", "↳ Explore: Explore crate"),
            ok("toolu_demo32"),
            StreamEvent::TurnEnd,
        ]'''),
(r'''#[test]
fn a_partial_or_foreign_line_gives_nothing() {''', r'''#[test]
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
fn a_partial_or_foreign_line_gives_nothing() {'''),
])
