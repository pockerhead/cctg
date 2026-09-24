# Integration test edits for TASK-017 (applied once to ws/).
import io, os
ws = os.path.join(os.path.dirname(os.path.abspath(__file__)), 'ws', 'crates', 'cctg', 'tests')
def edit(name, pairs):
    p = os.path.join(ws, name)
    s = io.open(p, encoding='utf-8', newline='').read()
    for old, new in pairs:
        assert s.count(old) == 1, (name, old)
        s = s.replace(old, new)
    io.open(p, 'w', encoding='utf-8', newline='\n').write(s)

edit('message_logs.rs', [
 ("//! Log capture for topic messages, agent replies and turn answers: their\n//! text and the sender's user id never reach the logs.",
  "//! Log capture for topic messages (also kept ones), agent replies and turn\n//! answers: their text and the sender's user id never reach the logs, and\n//! the kept message's `registry.json` carries no user id."),
 ("use cctg::hub::slots::{Control, OFFLINE_NOTICE, Options, Slots};", "use cctg::hub::buffer::QUEUED_NOTICE;\nuse cctg::hub::slots::{Control, Options, Slots};"),
 ("    // No agent yet: the user gets the offline notice.", "    // No agent yet: the message is kept and the user is told."),
 ("                .any(|op| matches!(op, Op::Send { text, .. } if text == OFFLINE_NOTICE));",
  "                .any(|op| matches!(op, Op::Send { text, .. } if text == QUEUED_NOTICE));"),
 ("        .expect(\"offline notice in time\");", "        .expect(\"queued notice in time\");\n    let saved = std::fs::read_to_string(state.join(\"registry.json\")).unwrap_or_default();\n    assert!(saved.contains(&offline_text), \"the kept message is saved\");\n    assert!(!saved.contains(&USER.to_string()), \"no user id in the saved buffer\");"),
 ("""    tokio::time::sleep(Duration::from_millis(300)).await;
    control
        .send(topic_message(2, &inbound_text))
        .expect("control");
    let got = tokio::time::timeout(Duration::from_secs(30), to_agent_rx.recv())
        .await
        .expect("inbound in time");""", """    // The agent takes the kept message first.
    let got = tokio::time::timeout(Duration::from_secs(30), to_agent_rx.recv())
        .await
        .expect("kept message in time");
    assert!(
        matches!(got, Some(HubMsg::Inbound { ref content, .. }) if *content == offline_text),
        "{got:?}"
    );
    control
        .send(topic_message(2, &inbound_text))
        .expect("control");
    let got = tokio::time::timeout(Duration::from_secs(30), to_agent_rx.recv())
        .await
        .expect("inbound in time");"""),
 ("        \"message for a session that is not on line\",", "        \"message kept for the slot until a session is on line\",\n        \"kept messages handed to the slot's session\","),
])

edit('overflow_logs.rs', [
 ("        // Every message to the dead session asks for a notice.", "        // Every photo asks for a text-only notice."),
 ("                text: Some(\"x\".into()),", "                text: None,"),
 ("    // The session has no agent: every message becomes an offline notice.", "    // Photos: every one becomes a text-only notice."),
])
print('ok')
