//! Hub side of the live transcript stream (TASK-016).
//!
//! The session's agent reads the transcript on its own device and answers
//! each `transcript_read` with the stream events of the complete lines it
//! found ([`crate::tail`]). Here those events become topic messages: a
//! terminal prompt as `> text`, assistant text before a tool call as is, and
//! one line per finished tool call (`• Bash: ... ✓`, `• Edit: ... ✗ error`)
//! in the order of the calls: a result that comes before the result of an
//! earlier call waits for it. The final answer of a turn comes from the
//! `Stop` hook, never from here; the end of a turn in the transcript only
//! lets a held answer go (see [`Live::held`]). A Telegram message the agent
//! handed to Claude turns 👀 -> ✍ when its own channel record shows up.
//!
//! Offsets: [`Stream::offset`] in the registry is the transcript byte up to
//! which every stream message was accepted by Telegram, and
//! [`Stream::calls`] the calls still open at that byte. Each read ends with a
//! barrier; it moves them only when every message before it was accepted.
//! A hub restart re-reads from there: messages in flight may come twice, a
//! line is never skipped. A message Telegram refuses (not 429, which the
//! scheduler retries) stops the commits; once nothing is in flight the stream
//! reads again from the last barrier.
//!
//! A released turn answer rides the stream as its messages (see
//! [`Live::sent_answer`]): it waits behind a refused line like the lines do,
//! and a rewind holds it again, so it shows after the lines of its turn.
//! An answer is paired with its turn end by the transcript byte of that turn
//! end ([`Held::end`]), not by count: a turn end read again after a rewind
//! lets only its own answer go, or nothing when that answer is in the topic
//! already ([`Live::turn_end`]).

use std::collections::VecDeque;
use std::time::Duration;

use tokio::time::Instant;

use super::registry::{PendingCall, Stream};
use crate::wire::StreamItem;

/// Tool calls of a turn waiting for their result, per session. A call past it
/// lets the oldest go: with its mark when its result is in, else unshown.
pub const MAX_CALLS: usize = 64;
/// Telegram messages per session that wait for ✍; the oldest keeps 👀.
pub const MAX_RECEIPTS: usize = 32;
/// Stream messages of a session waiting for Telegram; no further line is
/// taken while this many wait.
pub const MAX_WAITING: usize = 64;
/// Turn answers of a session held for their transcript turn end; the oldest
/// goes when one more comes.
pub const MAX_HELD: usize = 8;
/// Mark of a thinking message in the topic.
pub const THINKING: &str = "\u{1F4AD}";
/// Reaction for a message handed to the session's agent.
pub const ACCEPTED: &str = "👀";
/// Reaction for a message Claude took into work (its channel record is in the
/// transcript). Without U+FE0F, exactly as the Bot API lists it.
pub const WORKING: &str = "✍";

/// What one transcript line asks of the actor, in order.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Step {
    /// A topic message; `merge`: a one-line tool call that may share a
    /// message with the next ones.
    Send {
        text: String,
        merge: bool,
        format: Format,
    },
    /// Mark this Telegram message ✍.
    Working(i64),
    /// A turn ended here: a held answer may go now.
    TurnEnd,
    /// A prompt typed in the terminal starts a turn.
    NewTurn,
}

/// How a stream message is sent.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Format {
    Plain,
    /// Assistant text, sent as Telegram HTML.
    Markdown,
    /// A prompt typed in the terminal: the whole message monospace.
    Code,
}

/// Applies the items of one line to the calls still open and the receipts.
pub fn apply_line(
    calls: &mut Vec<PendingCall>,
    receipts: &mut Vec<i64>,
    items: &[StreamItem],
) -> Vec<Step> {
    let mut steps = Vec::new();
    for item in items {
        match item {
            StreamItem::Prompt { text } => {
                flush(calls, &mut steps);
                steps.push(Step::NewTurn);
                steps.push(Step::Send {
                    text: format!("> {text}"),
                    merge: false,
                    format: Format::Code,
                });
            }
            StreamItem::Note { text } => {
                flush(calls, &mut steps);
                steps.push(Step::Send {
                    text: text.clone(),
                    merge: false,
                    format: Format::Markdown,
                });
            }
            // Its own message; under the rate limit it joins its neighbours like a tool line.
            StreamItem::Thinking { text } => {
                flush(calls, &mut steps);
                steps.push(Step::Send {
                    text: format!("{THINKING} {text}"),
                    merge: true,
                    format: Format::Markdown,
                });
            }
            StreamItem::TurnEnd => {
                flush(calls, &mut steps);
                steps.push(Step::TurnEnd);
            }
            StreamItem::Channel { message_id } => {
                if let Some(at) = receipts.iter().position(|id| id == message_id) {
                    receipts.remove(at);
                    steps.push(Step::Working(*message_id));
                }
            }
            StreamItem::Call { id, line } => {
                if calls.iter().any(|known| known.id == *id) {
                    continue;
                }
                if calls.len() >= MAX_CALLS {
                    let oldest = calls.remove(0);
                    if oldest.done {
                        steps.push(finished(&oldest));
                    }
                    release_ready(calls, &mut steps);
                }
                calls.push(PendingCall {
                    id: id.clone(),
                    line: line.clone(),
                    done: false,
                    error: None,
                });
            }
            StreamItem::Result { id, error } => {
                if let Some(call) = calls
                    .iter_mut()
                    .find(|known| known.id == *id && !known.done)
                {
                    call.done = true;
                    call.error.clone_from(error);
                    release_ready(calls, &mut steps);
                }
            }
            StreamItem::Other => {}
        }
    }
    steps
}

/// Sends the finished calls at the head, in call order.
fn release_ready(calls: &mut Vec<PendingCall>, steps: &mut Vec<Step>) {
    let ready = calls.iter().take_while(|call| call.done).count();
    steps.extend(calls.drain(..ready).map(|call| finished(&call)));
}

/// A turn moved on: finished calls go in call order, calls that never got a
/// result are not shown (no ✓ for what did not finish).
fn flush(calls: &mut Vec<PendingCall>, steps: &mut Vec<Step>) {
    steps.extend(
        calls
            .drain(..)
            .filter(|call| call.done)
            .map(|call| finished(&call)),
    );
}

fn finished(call: &PendingCall) -> Step {
    let text = match call.error.as_deref() {
        None => format!("{} ✓", call.line),
        Some("") => format!("{} ✗", call.line),
        Some(error) => format!("{} ✗ {error}", call.line),
    };
    Step::Send {
        text,
        merge: true,
        format: Format::Plain,
    }
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
    let receipts = &stream.receipts;
    stream.parts.retain(|(key, _)| receipts.contains(key));
}

/// Messages handed to the session's agent as one inbound (TASK-048), in
/// order: the last one, whose id the inbound's `message_id` carries, waits
/// for ✍ like [`receipt`]; the others turn ✍ with it ([`take_parts`]).
pub fn receipt_parts(stream: &mut Stream, message_ids: &[i64]) {
    let Some((&key, others)) = message_ids.split_last() else {
        return;
    };
    receipt(stream, key);
    stream.parts.retain(|(known, _)| *known != key);
    if !others.is_empty() {
        stream.parts.push((key, others.to_vec()));
    }
}

/// The other messages of the burst whose receipt `key` turned ✍.
pub fn take_parts(stream: &mut Stream, key: i64) -> Vec<i64> {
    match stream.parts.iter().position(|(known, _)| *known == key) {
        Some(at) => stream.parts.remove(at).1,
        None => Vec::new(),
    }
}

/// A turn answer held until the stream lines before it are handed out.
#[derive(Debug)]
pub struct Held {
    pub thread_id: i64,
    /// Blank for a `Stop` without text: it only takes its turn end.
    pub answer: String,
    pub until: Instant,
    /// The transcript byte of its turn end, once paired with one.
    pub end: Option<u64>,
}

#[derive(Debug)]
enum Entry {
    Message {
        number: u64,
        state: Answer,
        /// The last message of a turn answer: held again if it is not in
        /// the topic when the stream rewinds.
        answer: Option<Held>,
    },
    /// Everything before it read: the offset and the calls open there.
    Barrier { to: u64, calls: Vec<PendingCall> },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Answer {
    Waiting,
    Accepted,
    Refused,
}

/// In-memory stream state of one session.
#[derive(Debug, Default)]
pub struct Live {
    /// Next byte to ask the agent for; ahead of `Stream::offset` while
    /// messages wait. `None`: the end of the file.
    pub read_at: Option<u64>,
    /// Calls open at `read_at`.
    pub calls: Vec<PendingCall>,
    /// The request in flight: the connection it went to and when.
    pub reading: Option<(u64, Instant)>,
    /// When to ask again.
    pub next_read: Option<Instant>,
    pub missing_warned: bool,
    /// The agent found the transcript at least once. Until then a missing
    /// file is normal: Claude Code writes it with the first prompt.
    pub file_seen: bool,
    pub reset_warned: bool,
    pub refused_warned: bool,
    /// Turn answers waiting for a turn end in the transcript, oldest first.
    pub held: VecDeque<Held>,
    /// Turn ends read that no `Stop` has taken yet (the file was quicker than
    /// the hook), by transcript byte, oldest first, at most [`MAX_HELD`]. A
    /// new prompt read after them does not clear them at once (their `Stop`
    /// may still be on its way to the actor); they lapse at `ends_until`.
    pub ends_unclaimed: VecDeque<u64>,
    pub ends_until: Option<Instant>,
    /// The next stream message is the first of this stream, or the first
    /// after a rewind: it lets the scheduler send the topic's stream again
    /// after a refused message (see `Op::Stream::restart`).
    pub restart: bool,
    /// Turn ends ahead of `read_at` whose answer went to the topic already:
    /// read again, they let nothing go and are not claimable.
    answered_ends: Vec<u64>,
    waiting: VecDeque<Entry>,
    next: u64,
}

impl Live {
    pub fn new(read_at: Option<u64>, calls: Vec<PendingCall>) -> Self {
        Self {
            read_at,
            calls,
            restart: true,
            ..Self::default()
        }
    }

    /// A new turn started in the transcript: turn ends no `Stop` has taken
    /// yet stay claimable until `until`, not longer.
    pub fn lapse_ends(&mut self, until: Instant) {
        if !self.ends_unclaimed.is_empty() {
            self.ends_until.get_or_insert(until);
        }
    }

    /// A `Stop` takes the oldest turn end read before it, if one is still
    /// claimable at `now`: its transcript byte.
    pub fn claim_end(&mut self, now: Instant) -> Option<u64> {
        if self.ends_until.is_some_and(|until| now >= until) {
            self.ends_unclaimed.clear();
        }
        let claimed = self.ends_unclaimed.pop_front();
        if self.ends_unclaimed.is_empty() {
            self.ends_until = None;
        }
        claimed
    }

    /// The turn end at transcript byte `end` was read: the held answer it
    /// lets go, if any. Its own answer (held again by a rewind) first, else
    /// the oldest held answer not paired with a later turn end. A turn end
    /// whose answer is in the topic already lets nothing go; one without an
    /// answer stays claimable for the next `Stop`.
    pub fn turn_end(&mut self, end: u64) -> Option<Held> {
        // The mark stays: another rewind may read this end again. A barrier
        // past it (`advance`) or a rewind beyond it drops it.
        if self.answered_ends.contains(&end) {
            return None;
        }
        let own = self.held.iter().position(|held| held.end == Some(end));
        let at = own.or_else(|| {
            self.held
                .front()
                .is_some_and(|held| held.end.is_none_or(|known| known <= end))
                .then_some(0)
        });
        if let Some(mut held) = at.and_then(|at| self.held.remove(at)) {
            held.end = Some(end);
            return Some(held);
        }
        if self.ends_unclaimed.len() < MAX_HELD {
            self.ends_unclaimed.push_back(end);
        }
        None
    }

    /// Held answers at the front whose turn end lies before `from`: the
    /// lines of their turn are in the topic, nothing reads that end again.
    pub fn overdue(&mut self, from: u64) -> Vec<Held> {
        let mut due = Vec::new();
        while self
            .held
            .front()
            .is_some_and(|held| held.end.is_some_and(|end| end <= from))
        {
            due.extend(self.held.pop_front());
        }
        due
    }

    /// `held` goes by its timeout ahead of a read of its turn end: that turn
    /// end, read later, must not let another answer go.
    pub fn answered_early(&mut self, held: &Held) {
        if let Some(end) = held.end
            && self.read_at.is_some_and(|at| at < end)
        {
            self.answered_ends.push(end);
        }
    }

    /// `held` does not ride the stream (blank, a file, or dropped): nothing
    /// records its turn end when it is accepted, so it is recorded now. A
    /// rewind that reads that end again lets nothing go there and leaves
    /// nothing claimable for the next `Stop`.
    pub fn answered_outside(&mut self, held: &Held) {
        if let Some(end) = held.end
            && !self.answered_ends.contains(&end)
        {
            self.answered_ends.push(end);
        }
    }

    /// A message goes to Telegram; its number.
    pub fn sent(&mut self) -> u64 {
        self.push_message(None)
    }

    /// The last message of turn answer `held` goes to Telegram; its number.
    pub fn sent_answer(&mut self, held: Held) -> u64 {
        self.push_message(Some(held))
    }

    fn push_message(&mut self, answer: Option<Held>) -> u64 {
        self.next += 1;
        self.waiting.push_back(Entry::Message {
            number: self.next,
            state: Answer::Waiting,
            answer,
        });
        self.next
    }

    /// Everything up to `to` is handed out; `calls` are open there. A
    /// barrier right after another replaces it (nothing lies between them),
    /// so idle reads while a message waits keep one barrier, not one each.
    pub fn barrier(&mut self, to: u64) {
        self.read_at = Some(to);
        let barrier = Entry::Barrier {
            to,
            calls: self.calls.clone(),
        };
        match self.waiting.back_mut() {
            Some(last @ Entry::Barrier { .. }) => *last = barrier,
            _ => self.waiting.push_back(barrier),
        }
    }

    /// Telegram answered message `number`; `accepted` false: it is not in
    /// the topic.
    pub fn answered(&mut self, number: u64, accepted: bool) {
        for entry in &mut self.waiting {
            if let Entry::Message {
                number: n, state, ..
            } = entry
                && *n == number
            {
                *state = if accepted {
                    Answer::Accepted
                } else {
                    Answer::Refused
                };
            }
        }
    }

    /// Drops the accepted head; the last barrier passed, if any: the offset
    /// and open calls to persist.
    pub fn advance(&mut self) -> Option<(u64, Vec<PendingCall>)> {
        let mut passed = None;
        loop {
            match self.waiting.front() {
                Some(Entry::Message {
                    state: Answer::Accepted,
                    ..
                }) => {}
                Some(Entry::Barrier { .. }) => {}
                _ => return passed,
            }
            match self.waiting.pop_front() {
                Some(Entry::Barrier { to, calls }) => {
                    // A rewind never goes back past `to` now.
                    self.answered_ends.retain(|&end| end > to);
                    passed = Some((to, calls));
                }
                // Until a barrier passes it, a rewind may read its turn end
                // again.
                Some(Entry::Message {
                    answer: Some(held), ..
                }) => self.answered_ends.extend(held.end),
                _ => {}
            }
        }
    }

    /// The numbers of the messages waiting for Telegram that carry an answer.
    #[cfg(test)]
    pub fn answer_numbers(&self) -> Vec<(u64, &str)> {
        self.waiting
            .iter()
            .filter_map(|entry| match entry {
                Entry::Message {
                    number,
                    answer: Some(held),
                    ..
                } => Some((*number, held.answer.as_str())),
                _ => None,
            })
            .collect()
    }

    /// Messages not answered yet.
    pub fn unanswered(&self) -> usize {
        self.waiting
            .iter()
            .filter(|entry| {
                matches!(
                    entry,
                    Entry::Message {
                        state: Answer::Waiting,
                        ..
                    }
                )
            })
            .count()
    }

    /// A refused message waits and nothing is in flight: time to read again
    /// from the last barrier.
    pub fn stuck(&self) -> bool {
        self.unanswered() == 0
            && self.waiting.iter().any(|entry| {
                matches!(
                    entry,
                    Entry::Message {
                        state: Answer::Refused,
                        ..
                    }
                )
            })
    }

    /// Starts over at the last barrier Telegram fully accepted, keeping the
    /// held answers and warnings. Answers sent since that barrier and not in
    /// the topic are held again, before those still held; the read of their
    /// own turn end lets them go after their lines again, and a turn end
    /// read again whose answer is in the topic lets nothing go. None goes by
    /// its timeout before `at + hold`; a read later than that lets it go
    /// ahead of its lines (the timeout bounds the wait).
    pub fn rewind(
        &mut self,
        offset: Option<u64>,
        calls: Vec<PendingCall>,
        at: Instant,
        hold: Duration,
    ) {
        let read_again = |end: &u64| offset.is_some_and(|offset| *end > offset);
        let mut held = VecDeque::new();
        let mut answered_ends: Vec<u64> = self.answered_ends.drain(..).filter(read_again).collect();
        for entry in std::mem::take(&mut self.waiting) {
            if let Entry::Message {
                state,
                answer: Some(answer),
                ..
            } = entry
            {
                if state == Answer::Accepted {
                    answered_ends.extend(answer.end.filter(read_again));
                } else {
                    held.push_back(answer);
                }
            }
        }
        // An answer that went by its timeout and is held again waits for its
        // own turn end again.
        answered_ends.retain(|end| !held.iter().any(|answer| answer.end == Some(*end)));
        held.append(&mut self.held);
        for answer in &mut held {
            answer.until = answer.until.max(at + hold);
        }
        // Unclaimed turn ends before the offset are not read again.
        let mut ends_unclaimed = std::mem::take(&mut self.ends_unclaimed);
        ends_unclaimed.retain(|end| !read_again(end));
        let ends_until = self.ends_until.filter(|_| !ends_unclaimed.is_empty());
        let refused_warned = self.refused_warned;
        let file_seen = self.file_seen;
        *self = Self::new(offset, calls);
        self.held = held;
        self.answered_ends = answered_ends;
        self.ends_unclaimed = ends_unclaimed;
        self.ends_until = ends_until;
        self.refused_warned = refused_warned;
        self.file_seen = file_seen;
        self.next_read = Some(at);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

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

    fn sends(steps: &[Step]) -> Vec<&str> {
        steps
            .iter()
            .filter_map(|step| match step {
                Step::Send { text, .. } => Some(text.as_str()),
                _ => None,
            })
            .collect()
    }

    /// Applies lines one by one, like the actor.
    fn run(calls: &mut Vec<PendingCall>, lines: &[Vec<StreamItem>]) -> Vec<Step> {
        let mut receipts = Vec::new();
        lines
            .iter()
            .flat_map(|items| apply_line(calls, &mut receipts, items))
            .collect()
    }

    #[test]
    fn results_out_of_call_order_wait_and_go_in_call_order() {
        let mut calls = Vec::new();
        let first = run(
            &mut calls,
            &[
                vec![StreamItem::Prompt { text: "go".into() }],
                vec![StreamItem::Note {
                    text: "Running.".into(),
                }],
                vec![call("a", "• Bash: A")],
                vec![call("b", "• Edit: B")],
                vec![result("b", Some("not found"))],
            ],
        );
        // B is done, but A was called first and is still running.
        assert_eq!(sends(&first), ["> go", "Running."]);
        let second = run(&mut calls, &[vec![result("a", None)]]);
        assert_eq!(sends(&second), ["• Bash: A ✓", "• Edit: B ✗ not found"]);
        assert!(
            second
                .iter()
                .all(|step| matches!(step, Step::Send { merge: true, .. }))
        );
        assert!(calls.is_empty());
    }

    /// TASK-025: thinking is its own message, marked, after the finished
    /// calls before it; like a tool line it may join neighbours under the limit.
    #[test]
    fn thinking_goes_as_its_own_message_in_turn_order() {
        let mut calls = Vec::new();
        let steps = run(
            &mut calls,
            &[
                vec![StreamItem::Prompt { text: "go".into() }],
                vec![call("a", "• Bash: A")],
                vec![result("a", None)],
                vec![call("b", "• Bash: B")],
                vec![StreamItem::Thinking {
                    text: "Checking cargo.".into(),
                }],
                vec![StreamItem::TurnEnd],
            ],
        );
        assert_eq!(
            steps,
            [
                Step::NewTurn,
                Step::Send {
                    text: "> go".into(),
                    merge: false,
                    format: Format::Code,
                },
                Step::Send {
                    text: "• Bash: A ✓".into(),
                    merge: true,
                    format: Format::Plain,
                },
                Step::Send {
                    text: "\u{1F4AD} Checking cargo.".into(),
                    merge: true,
                    format: Format::Markdown,
                },
                Step::TurnEnd,
            ]
        );
        assert!(calls.is_empty());
    }

    #[test]
    fn a_turn_end_lets_finished_calls_go_and_never_marks_an_unfinished_one() {
        let mut calls = Vec::new();
        let steps = run(
            &mut calls,
            &[
                vec![call("a", "• Bash: A"), call("b", "• Bash: B")],
                vec![result("b", None)],
                vec![StreamItem::TurnEnd],
            ],
        );
        assert_eq!(
            steps,
            [
                Step::Send {
                    text: "• Bash: B ✓".into(),
                    merge: true,
                    format: Format::Plain,
                },
                Step::TurnEnd
            ]
        );
        assert!(calls.is_empty());
        // A late result of the flushed call shows nothing.
        assert!(run(&mut calls, &[vec![result("a", None)]]).is_empty());
    }

    #[test]
    fn open_calls_are_bounded_without_blocking_later_ones() {
        let mut calls = Vec::new();
        let mut lines: Vec<Vec<StreamItem>> = (0..MAX_CALLS)
            .map(|n| vec![call(&format!("t{n}"), &format!("c{n}"))])
            .collect();
        // t1 finishes while t0 never does.
        lines.push(vec![result("t1", None)]);
        lines.push(vec![call("late", "late")]);
        let steps = run(&mut calls, &lines);
        assert_eq!(sends(&steps), ["c1 ✓"]);
        assert_eq!(calls.len(), MAX_CALLS - 1);
        assert_eq!(calls[0].id, "t2");
    }

    #[test]
    fn only_a_received_message_turns_to_working_and_only_once() {
        let mut stream = Stream::default();
        receipt(&mut stream, 7);
        receipt(&mut stream, 7);
        let steps = apply_line(
            &mut Vec::new(),
            &mut stream.receipts,
            &[
                StreamItem::Channel { message_id: 8 },
                StreamItem::Channel { message_id: 7 },
                StreamItem::Channel { message_id: 7 },
            ],
        );
        assert_eq!(steps, [Step::Working(7)]);
        assert!(stream.receipts.is_empty());
        for id in 0..(MAX_RECEIPTS as i64 + 5) {
            receipt(&mut stream, id);
        }
        assert_eq!(stream.receipts.len(), MAX_RECEIPTS);
        assert_eq!(stream.receipts[0], 5);
    }

    #[test]
    fn a_burst_waits_on_its_last_message_and_its_parts_go_with_it_once() {
        let mut stream = Stream::default();
        receipt_parts(&mut stream, &[1, 2, 3]);
        receipt_parts(&mut stream, &[4]);
        assert_eq!(stream.receipts, [3, 4]);
        assert_eq!(stream.parts, [(3, vec![1, 2])]);
        let steps = apply_line(
            &mut Vec::new(),
            &mut stream.receipts,
            &[StreamItem::Channel { message_id: 3 }],
        );
        assert_eq!(steps, [Step::Working(3)]);
        assert_eq!(take_parts(&mut stream, 3), [1, 2]);
        assert!(take_parts(&mut stream, 3).is_empty(), "only once");
        assert!(
            take_parts(&mut stream, 4).is_empty(),
            "a lone message has none"
        );
        // Parts leave with their receipt when newer ones push it out.
        receipt_parts(&mut stream, &[10, 11]);
        for id in 100..(100 + MAX_RECEIPTS as i64) {
            receipt(&mut stream, id);
        }
        assert!(stream.parts.is_empty());
    }

    #[test]
    fn the_offset_moves_only_over_accepted_messages() {
        let mut live = Live::new(Some(0), Vec::new());
        let a = live.sent();
        let b = live.sent();
        live.calls.push(PendingCall {
            id: "open".into(),
            line: "x".into(),
            done: false,
            error: None,
        });
        live.barrier(25);
        assert_eq!(live.unanswered(), 2);
        live.answered(b, true);
        assert_eq!(live.advance(), None);
        live.answered(a, true);
        let (offset, calls) = live.advance().unwrap();
        assert_eq!(offset, 25);
        assert_eq!(calls[0].id, "open");
        assert_eq!(live.unanswered(), 0);
        // A read without messages moves it at once.
        live.barrier(40);
        assert_eq!(live.advance().map(|(offset, _)| offset), Some(40));
    }

    #[test]
    fn a_refused_message_stops_the_offset_until_the_stream_rewinds() {
        let mut live = Live::new(Some(0), Vec::new());
        let a = live.sent();
        live.barrier(10);
        let b = live.sent();
        live.barrier(20);
        let c = live.sent();
        live.barrier(30);
        live.answered(a, true);
        assert_eq!(live.advance().map(|(offset, _)| offset), Some(10));
        live.answered(b, false);
        live.answered(c, true);
        assert_eq!(live.advance(), None, "nothing passes a refused message");
        assert!(live.stuck());
        live.rewind(Some(10), Vec::new(), Instant::now(), Duration::ZERO);
        assert_eq!(live.read_at, Some(10));
        assert!(!live.stuck());
        assert_eq!(live.unanswered(), 0);
    }

    #[test]
    fn a_rewind_holds_again_the_answers_not_in_the_topic_before_the_held_ones() {
        let now = Instant::now();
        let held = |answer: &str| Held {
            thread_id: 100,
            answer: answer.into(),
            until: now,
            end: None,
        };
        let mut live = Live::new(Some(0), Vec::new());
        let line = live.sent();
        let first = live.sent_answer(held("first"));
        live.barrier(10);
        let refused = live.sent();
        let second = live.sent_answer(held("second"));
        live.barrier(20);
        live.held.push_back(Held {
            until: now + Duration::from_secs(60),
            ..held("still held")
        });
        live.answered(line, true);
        live.answered(first, true);
        live.answered(refused, false);
        // Dropped unsent behind the refused line.
        live.answered(second, false);
        assert_eq!(live.advance().map(|(offset, _)| offset), Some(10));
        assert!(live.stuck());
        let at = now + Duration::from_secs(5);
        live.rewind(Some(10), Vec::new(), at, Duration::from_secs(1));
        let answers: Vec<(&str, Instant)> = live
            .held
            .iter()
            .map(|held| (held.answer.as_str(), held.until))
            .collect();
        assert_eq!(
            answers,
            [
                ("second", at + Duration::from_secs(1)),
                ("still held", now + Duration::from_secs(60)),
            ],
            "the accepted answer is not held again"
        );
        assert_eq!(live.unanswered(), 0);
        assert!(live.restart, "the next message restarts the stream");
    }

    /// TASK-023 review I1: an answer that went by its timeout before its
    /// turn end was read takes that turn end when it comes (nothing else
    /// goes there, nothing is left claimable); held again by a rewind it
    /// waits for that turn end again.
    #[test]
    fn an_answer_gone_by_its_timeout_keeps_its_turn_end() {
        let now = Instant::now();
        let held = |answer: &str| Held {
            thread_id: 100,
            answer: answer.into(),
            until: now,
            end: Some(40),
        };
        let mut live = Live::new(Some(0), Vec::new());
        let early = held("early");
        live.answered_early(&early);
        let number = live.sent_answer(early);
        live.answered(number, true);
        live.held.push_back(Held {
            end: None,
            ..held("next")
        });
        assert!(live.turn_end(40).is_none(), "its answer is in the topic");
        assert!(live.ends_unclaimed.is_empty());
        assert_eq!(live.held.len(), 1, "the next answer waits for its own end");

        let mut live = Live::new(Some(0), Vec::new());
        let early = held("early");
        live.answered_early(&early);
        let number = live.sent_answer(early);
        live.answered(number, false);
        assert!(live.stuck());
        live.rewind(Some(0), Vec::new(), now, Duration::ZERO);
        assert!(live.turn_end(20).is_none(), "not its turn end");
        assert_eq!(live.ends_unclaimed, [20]);
        assert_eq!(
            live.turn_end(40).map(|held| held.answer),
            Some("early".into())
        );
    }

    /// TASK-023 QA BUG-1: an answer that goes outside the stream (a file, a
    /// blank Stop) marks its turn end; a rewind that reads that end again
    /// lets nothing go and leaves nothing claimable.
    #[test]
    fn a_turn_end_answered_outside_the_stream_is_not_claimable_when_read_again() {
        let now = Instant::now();
        let mut live = Live::new(Some(0), Vec::new());
        live.held.push_back(Held {
            thread_id: 100,
            answer: "a file".into(),
            until: now,
            end: None,
        });
        let doc = live.turn_end(40).expect("its answer");
        live.answered_outside(&doc);
        live.answered_outside(&doc);
        let line = live.sent();
        live.barrier(80);
        live.answered(line, false);
        assert!(live.stuck());
        live.rewind(Some(0), Vec::new(), now, Duration::ZERO);
        live.held.push_back(Held {
            thread_id: 100,
            answer: "next".into(),
            until: now,
            end: None,
        });
        assert!(live.turn_end(40).is_none(), "answered already");
        assert!(live.ends_unclaimed.is_empty(), "nothing to claim");
        assert_eq!(live.held.len(), 1, "the next answer waits for its own end");
        assert_eq!(
            live.turn_end(70).map(|held| held.answer),
            Some("next".into())
        );
    }

    /// TASK-023 QA BUG-2: the turn end of an answer in the topic is read
    /// again by two rewinds to the same barrier; neither read leaves it
    /// claimable. Its mark goes once a barrier passes it.
    #[test]
    fn a_turn_end_read_again_by_two_rewinds_is_not_claimable() {
        let now = Instant::now();
        let mut live = Live::new(Some(0), Vec::new());
        live.held.push_back(Held {
            thread_id: 100,
            answer: "first".into(),
            until: now,
            end: None,
        });
        let first = live.turn_end(40).expect("its answer");
        let number = live.sent_answer(first);
        live.answered(number, true);
        for _ in 0..2 {
            let line = live.sent();
            live.barrier(80);
            live.answered(line, false);
            assert!(live.advance().is_none());
            assert!(live.stuck());
            live.rewind(Some(0), Vec::new(), now, Duration::ZERO);
            assert!(live.turn_end(40).is_none(), "answered already");
            assert!(live.ends_unclaimed.is_empty(), "nothing to claim");
        }
        let line = live.sent();
        live.barrier(80);
        live.answered(line, true);
        assert_eq!(live.advance().map(|(offset, _)| offset), Some(80));
        assert!(live.answered_ends.is_empty(), "a barrier past it drops it");
    }

    #[test]
    fn turn_ends_read_before_a_new_turn_stay_claimable_until_they_lapse() {
        use std::time::Duration;
        let now = Instant::now();
        let mut live = Live::new(Some(0), Vec::new());
        assert_eq!(live.claim_end(now), None, "nothing read");
        live.ends_unclaimed = VecDeque::from([10, 20]);
        live.lapse_ends(now + Duration::from_secs(5));
        // A later new turn does not push the lapse out.
        live.lapse_ends(now + Duration::from_secs(50));
        assert_eq!(live.claim_end(now + Duration::from_secs(1)), Some(10));
        assert_eq!(live.claim_end(now + Duration::from_secs(5)), None, "lapsed");
        assert!(live.ends_unclaimed.is_empty());
        assert_eq!(live.ends_until, None);
        // Without a new turn a turn end waits for its Stop however long.
        live.ends_unclaimed = VecDeque::from([30]);
        assert_eq!(live.claim_end(now + Duration::from_secs(3600)), Some(30));
        assert_eq!(live.claim_end(now + Duration::from_secs(3600)), None);
    }

    #[test]
    fn idle_reads_while_a_message_waits_keep_one_barrier() {
        let mut live = Live::new(Some(0), Vec::new());
        let a = live.sent();
        live.barrier(10);
        // 60 s of idle reads every 300 ms while Telegram makes `a` wait.
        for to in 0..200 {
            if to == 150 {
                live.calls.push(PendingCall {
                    id: "open".into(),
                    line: "x".into(),
                    done: false,
                    error: None,
                });
            }
            live.barrier(10 + to);
        }
        assert_eq!(live.waiting.len(), 2, "{:?}", live.waiting);
        assert_eq!(live.read_at, Some(209));
        assert_eq!(live.advance(), None);
        live.answered(a, true);
        let (offset, calls) = live.advance().unwrap();
        assert_eq!(offset, 209);
        assert_eq!(calls[0].id, "open");
        assert!(live.waiting.is_empty());
        // A message between two barriers keeps both.
        let b = live.sent();
        live.barrier(220);
        let c = live.sent();
        live.barrier(230);
        assert_eq!(live.waiting.len(), 4);
        live.answered(b, true);
        assert_eq!(live.advance().map(|(offset, _)| offset), Some(220));
        live.answered(c, true);
        assert_eq!(live.advance().map(|(offset, _)| offset), Some(230));
    }
}
