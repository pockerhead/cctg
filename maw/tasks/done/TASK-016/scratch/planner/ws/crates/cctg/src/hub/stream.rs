//! Hub side of the live transcript stream (TASK-016).
//!
//! The session's agent reads the transcript on its own device and answers
//! each `transcript_read` with the stream events of the complete lines it
//! found ([`crate::tail`]). Here those events become topic messages: a
//! terminal prompt as `> text`, assistant text before a tool call as is, and
//! one line per tool call once its result is in (`• Bash: ... ✓`,
//! `• Edit: ... ✗ error`). The final answer of a turn comes from the `Stop`
//! hook, never from here. A Telegram message the agent handed to Claude
//! turns 👀 -> ✍ when its own channel record shows up.
//!
//! Offsets: [`Stream::offset`] in the registry is the transcript byte up to
//! which every message is answered by Telegram, so a hub restart re-reads
//! only what may not have arrived (at most the messages in flight come
//! twice) and never skips what was written meanwhile. [`Live`] keeps the
//! in-memory read position ahead of it and the messages still waiting.

use std::collections::VecDeque;

use tokio::time::Instant;

use super::registry::{PendingCall, Stream};
use crate::wire::{StreamItem, StreamLine};

/// Tool calls waiting for their result, per session; the oldest is dropped
/// (its result then shows nothing).
pub const MAX_CALLS: usize = 64;
/// Telegram messages per session that wait for ✍.
pub const MAX_RECEIPTS: usize = 32;
/// Stream messages of a session waiting for Telegram; no further read until
/// fewer wait. One chunk adds at most [`crate::tail::MAX_CHUNK_LINES`] lines.
pub const MAX_WAITING: usize = 64;
/// Reaction for a message handed to the session's agent.
pub const ACCEPTED: &str = "👀";
/// Reaction for a message Claude took into work (its channel record is in the
/// transcript). Without U+FE0F, exactly as the Bot API lists it.
pub const WORKING: &str = "✍";

/// A message for the topic from the line that ends at `end`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Out {
    pub end: u64,
    pub text: String,
    /// A tool call line: may share a message with the next ones.
    pub merge: bool,
}

/// What one chunk asks of the actor.
#[derive(Debug, Default, PartialEq, Eq)]
pub struct Applied {
    pub messages: Vec<Out>,
    /// Telegram messages to mark ✍.
    pub working: Vec<i64>,
}

/// Applies the lines of one chunk, read in order from the stream's position,
/// to the persisted state.
pub fn apply(stream: &mut Stream, lines: &[StreamLine]) -> Applied {
    let answered = stream.offset.unwrap_or(0);
    let mut applied = Applied::default();
    for line in lines {
        for item in &line.items {
            let out = |text: String, merge: bool| Out {
                end: line.end,
                text,
                merge,
            };
            match item {
                StreamItem::Prompt { text } => {
                    applied.messages.push(out(format!("> {text}"), false));
                }
                StreamItem::Note { text } => applied.messages.push(out(text.clone(), false)),
                StreamItem::Channel { message_id } => {
                    if let Some(at) = stream.receipts.iter().position(|id| id == message_id) {
                        stream.receipts.remove(at);
                        applied.working.push(*message_id);
                    }
                }
                StreamItem::Call { id, line: call } => {
                    if !stream.calls.iter().any(|known| known.id == *id) {
                        if stream.calls.len() >= MAX_CALLS {
                            stream.calls.remove(0);
                        }
                        stream.calls.push(PendingCall {
                            id: id.clone(),
                            line: call.clone(),
                            result_end: None,
                        });
                    }
                }
                StreamItem::Result { id, error } => {
                    // A result read again after a restart counts as long as
                    // Telegram never answered its message.
                    let Some(call) = stream.calls.iter_mut().find(|known| {
                        known.id == *id && known.result_end.is_none_or(|end| end > answered)
                    }) else {
                        continue;
                    };
                    call.result_end = Some(line.end);
                    let text = match error.as_deref() {
                        None => format!("{} ✓", call.line),
                        Some("") => format!("{} ✗", call.line),
                        Some(error) => format!("{} ✗ {error}", call.line),
                    };
                    applied.messages.push(out(text, true));
                }
                StreamItem::Other => {}
            }
        }
    }
    applied
}

/// A message handed to the session's agent now shows 👀 and waits for ✍.
pub fn receipt(stream: &mut Stream, message_id: i64) {
    if stream.receipts.contains(&message_id) {
        return;
    }
    if stream.receipts.len() >= MAX_RECEIPTS {
        stream.receipts.remove(0);
    }
    stream.receipts.push(message_id);
}

/// Everything up to `offset` is answered: calls whose result is before it
/// are done.
pub fn answered_up_to(stream: &mut Stream, offset: u64) {
    stream.offset = Some(offset);
    stream
        .calls
        .retain(|call| call.result_end.is_none_or(|end| end > offset));
}

/// A turn answer held until the stream lines before it are handed out.
#[derive(Debug)]
pub struct Held {
    pub thread_id: i64,
    pub answer: String,
    /// Goes out after the chunk of this request number.
    pub after: u64,
    pub until: Instant,
}

/// In-memory stream state of one session.
#[derive(Debug, Default)]
pub struct Live {
    /// Next byte to ask the agent for; ahead of `Stream::offset` while
    /// messages wait. `None`: the end of the file.
    pub read_at: Option<u64>,
    /// The request in flight: its number and when it went out.
    pub reading: Option<(u64, Instant)>,
    /// Requests sent so far.
    pub requests: u64,
    /// When to ask again.
    pub next_read: Option<Instant>,
    pub missing_warned: bool,
    pub held: Option<Held>,
    /// Handed out, oldest first: (number, end, answered). A read's end is an
    /// answered entry of its own, so lines without messages move the offset.
    waiting: VecDeque<(u64, u64, bool)>,
    next: u64,
}

impl Live {
    pub fn new(read_at: Option<u64>) -> Self {
        Self {
            read_at,
            ..Self::default()
        }
    }

    /// A message of the line ending at `end` goes to Telegram; its number.
    pub fn sent(&mut self, end: u64) -> u64 {
        self.next += 1;
        self.waiting.push_back((self.next, end, false));
        self.next
    }

    /// A chunk was read up to `to`.
    pub fn read_up_to(&mut self, to: u64) {
        self.next += 1;
        self.waiting.push_back((self.next, to, true));
    }

    /// Telegram answered message `number`.
    pub fn answered(&mut self, number: u64) {
        if let Some(entry) = self.waiting.iter_mut().find(|entry| entry.0 == number) {
            entry.2 = true;
        }
    }

    /// Drops the answered head; the new answered offset if it moved.
    pub fn advance(&mut self) -> Option<u64> {
        let mut offset = None;
        while let Some(&(_, end, true)) = self.waiting.front() {
            offset = Some(end);
            self.waiting.pop_front();
        }
        offset
    }

    /// Messages not answered yet.
    pub fn unanswered(&self) -> usize {
        self.waiting.iter().filter(|entry| !entry.2).count()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn line(end: u64, items: Vec<StreamItem>) -> StreamLine {
        StreamLine { end, items }
    }

    fn call(id: &str, line: &str) -> StreamItem {
        StreamItem::Call {
            id: id.into(),
            line: line.into(),
        }
    }

    fn result(id: &str, error: Option<&str>) -> StreamItem {
        StreamItem::Result {
            id: id.into(),
            error: error.map(str::to_owned),
        }
    }

    fn texts(applied: &Applied) -> Vec<(u64, &str, bool)> {
        applied
            .messages
            .iter()
            .map(|out| (out.end, out.text.as_str(), out.merge))
            .collect()
    }

    #[test]
    fn a_call_line_goes_out_with_its_result_in_call_order() {
        let mut stream = Stream {
            offset: Some(0),
            ..Stream::default()
        };
        let first = apply(
            &mut stream,
            &[
                line(10, vec![StreamItem::Prompt { text: "go".into() }]),
                line(
                    20,
                    vec![StreamItem::Note {
                        text: "Running.".into(),
                    }],
                ),
                line(30, vec![call("t1", "• Bash: one")]),
                line(40, vec![call("t2", "• Edit: f.rs")]),
                line(50, vec![result("t1", None)]),
            ],
        );
        assert_eq!(
            texts(&first),
            [
                (10, "> go", false),
                (20, "Running.", false),
                (50, "• Bash: one ✓", true)
            ]
        );
        let second = apply(
            &mut stream,
            &[line(
                60,
                vec![result("t2", Some("not found")), result("t3", None)],
            )],
        );
        assert_eq!(texts(&second), [(60, "• Edit: f.rs ✗ not found", true)]);
        answered_up_to(&mut stream, 60);
        assert!(stream.calls.is_empty());
    }

    #[test]
    fn a_result_read_again_after_a_restart_goes_again_only_if_unanswered() {
        let mut stream = Stream {
            offset: Some(0),
            ..Stream::default()
        };
        apply(&mut stream, &[line(10, vec![call("t1", "• Bash: a")])]);
        apply(&mut stream, &[line(20, vec![result("t1", None)])]);
        // The hub died before Telegram answered: the offset stayed at 10.
        answered_up_to(&mut stream, 10);
        let again = apply(&mut stream, &[line(20, vec![result("t1", None)])]);
        assert_eq!(texts(&again), [(20, "• Bash: a ✓", true)]);
        answered_up_to(&mut stream, 20);
        let late = apply(&mut stream, &[line(20, vec![result("t1", None)])]);
        assert!(late.messages.is_empty());
    }

    #[test]
    fn only_a_received_message_turns_to_working_and_only_once() {
        let mut stream = Stream::default();
        receipt(&mut stream, 7);
        receipt(&mut stream, 7);
        let applied = apply(
            &mut stream,
            &[line(
                5,
                vec![
                    StreamItem::Channel { message_id: 8 },
                    StreamItem::Channel { message_id: 7 },
                    StreamItem::Channel { message_id: 7 },
                ],
            )],
        );
        assert_eq!(applied.working, [7]);
        assert!(stream.receipts.is_empty());
        for id in 0..(MAX_RECEIPTS as i64 + 5) {
            receipt(&mut stream, id);
        }
        assert_eq!(stream.receipts.len(), MAX_RECEIPTS);
        assert_eq!(stream.receipts[0], 5);
    }

    #[test]
    fn the_offset_moves_only_over_answered_messages() {
        let mut live = Live::new(Some(0));
        let a = live.sent(10);
        let b = live.sent(20);
        live.read_up_to(25);
        assert_eq!(live.unanswered(), 2);
        live.answered(b);
        assert_eq!(live.advance(), None);
        live.answered(a);
        assert_eq!(live.advance(), Some(25));
        assert_eq!(live.unanswered(), 0);
        // A read without messages moves it at once.
        live.read_up_to(40);
        assert_eq!(live.advance(), Some(40));
    }

    #[test]
    fn pending_calls_are_bounded() {
        let mut stream = Stream::default();
        let lines: Vec<StreamLine> = (0..(MAX_CALLS as u64 + 3))
            .map(|n| line(n + 1, vec![call(&format!("t{n}"), "• X")]))
            .collect();
        apply(&mut stream, &lines);
        assert_eq!(stream.calls.len(), MAX_CALLS);
        assert_eq!(stream.calls[0].id, "t3");
    }
}
