import os, sys
sys.path.insert(0, os.path.dirname(__file__))
from ed import edit
WS = os.path.join(os.path.dirname(__file__), '..', 'ws')
edit(os.path.join(WS, 'crates/transcript/src/stream.rs'), [
(r'''    /// The result of a tool call; `error` is set for a failed call and holds
    /// its first line (possibly empty).
    Result { id: String, error: Option<String> },
}''', r'''    /// The result of a tool call; `error` is set for a failed call and holds
    /// its first line (possibly empty).
    Result { id: String, error: Option<String> },
    /// Assistant text that ends a turn (`stop_reason` set and not
    /// `tool_use`). Its text is not carried: the `Stop` hook sends it.
    TurnEnd,
}'''),
(r'''pub fn stream_events(line: &str) -> Vec<StreamEvent> {
    let line = line.trim();''', r'''pub fn stream_events(line: &str) -> Vec<StreamEvent> {
    // A UTF-8 BOM is not JSON whitespace; a file written with one keeps it.
    let line = line.trim().trim_start_matches('\u{feff}');'''),
(r'''    let mut events = Vec::new();
    for block in &turn.blocks {''', r'''    let mut events = Vec::new();
    let mut answer = false;
    for block in &turn.blocks {'''),
(r'''            (Role::Assistant, Block::Text(text)) => {
                let text = text.trim();
                if turn.stop_reason.as_deref() == Some("tool_use") && !text.is_empty() {
                    events.push(StreamEvent::Note(text.to_owned()));
                }
            }''', r'''            (Role::Assistant, Block::Text(text)) => {
                let text = text.trim();
                match turn.stop_reason.as_deref() {
                    Some("tool_use") if !text.is_empty() => {
                        events.push(StreamEvent::Note(text.to_owned()));
                    }
                    Some("tool_use") | None => {}
                    Some(_) => answer = true,
                }
            }'''),
(r'''        }
    }
    events
}''', r'''        }
    }
    if answer {
        events.push(StreamEvent::TurnEnd);
    }
    events
}'''),
(r'''/// `message_id` of a `<channel ...>` opening tag, when it is all digits.
fn channel_message_id(text: &str) -> Option<i64> {
    let id = channel_attribute(text, "message_id")?;''', r'''/// `message_id` of a `<channel source="cctg" ...>` opening tag, when it is
/// all digits. Another channel server's tag never counts, whatever its ids.
fn channel_message_id(text: &str) -> Option<i64> {
    if channel_attribute(text, "source")? != SOURCE {
        return None;
    }
    let id = channel_attribute(text, "message_id")?;'''),
(r'''#[derive(Default, Deserialize)]
#[serde(default)]
struct RawAttachmentRecord {''', r'''/// The `source` of cctg's channel tags: the server name cctg is registered
/// under (`cctg agent-install`, `docs/poc.md`).
const SOURCE: &str = "cctg";

#[derive(Default, Deserialize)]
#[serde(default)]
struct RawAttachmentRecord {'''),
])
