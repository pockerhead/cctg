import os, sys
sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
from ed import edit
WS = os.path.join(os.path.dirname(os.path.abspath(__file__)), '..', 'ws')
P = os.path.join(WS, 'crates/cctg/src/hub/slots.rs')
edit(P, [
('''use super::stream::{self, Held, Live};''', '''use super::stream::{self, Held, Live, Step};'''),
('''/// A transcript read the agent has not answered by then is asked again.
const READ_TIMEOUT: Duration = Duration::from_secs(10);''',
'''/// A transcript read the agent has not answered by then is asked again.
const READ_TIMEOUT: Duration = Duration::from_secs(10);
/// A stream whose message Telegram did not take reads again after this.
const REWIND_AFTER: Duration = Duration::from_secs(5);
/// The stream takes no new transcript line while this many messages of any
/// kind wait for Telegram: replies, turn answers and notices keep the rest of
/// [`MAX_QUEUED_MESSAGES`].
const STREAM_QUEUE: usize = MAX_QUEUED_MESSAGES / 2;
/// Reactions waiting for Telegram at a time; one more is skipped.
const MAX_REACTIONS: usize = 64;'''),
('''    /// Longest wait of a turn answer for the stream lines read after its `Stop`.
    pub hold_answer: Duration,''', '''    /// Longest wait of a turn answer for the end of its turn in the
    /// transcript (the lines before it go first).
    pub hold_answer: Duration,'''),
('''            hold_answer: Duration::from_millis(1500),''', '''            // The turn's last record shows up ~0.15 s after the answer.
            hold_answer: Duration::from_secs(5),'''),
('''    reaction_warned: bool,
    grace_until''', '''    reaction_warned: bool,
    /// Reactions handed out and not answered, at most [`MAX_REACTIONS`].
    reactions: usize,
    grace_until'''),
('''            reaction_warned: false,
''', '''            reaction_warned: false,
            reactions: 0,
'''),
('''                let read = match live.reading {
                    Some((_, sent)) => Some(sent + READ_TIMEOUT),
                    None => live.next_read,
                };
                read.into_iter()
                    .chain(live.held.as_ref().map(|held| held.until))''', '''                let read = match live.reading {
                    Some(sent) => Some(sent + READ_TIMEOUT),
                    None => live.next_read,
                };
                read.into_iter()
                    .chain(live.held.front().map(|held| held.until))'''),
('''                        missing,
                        more,
                    } if session_id == session => {
                        self.on_chunk(&session, from, to, &lines, missing, more);
                    }''', '''                        missing,
                        more,
                        reset,
                    } if session_id == session => {
                        self.on_chunk(&session, from, to, &lines, missing, more, reset);
                    }'''),
('''    fn on_turn_answer(&mut self, session: &str, answer: &str) {
        if answer.trim().is_empty() {
            return;
        }''', '''    fn on_turn_answer(&mut self, session: &str, answer: &str) {
        // A streamed session's blank answer still takes its turn end.
        let streamed = self.stream_target(session).is_some();
        if answer.trim().is_empty() && !streamed {
            return;
        }'''),
('''        if let Some(live) = self.streams.get_mut(session) {
            // The lines of this turn that are not read yet go first.
            let earlier = live.held.take();
            let after = live.requests + 1;
            live.held = Some(Held {
                thread_id,
                answer: answer.to_owned(),
                after,
                until: Instant::now() + self.options.hold_answer,
            });
            live.next_read = Some(Instant::now());
            if let Some(earlier) = earlier {
                self.release(session, earlier);
            }
            return;
        }''', '''        if let Some(live) = self.streams.get_mut(session).filter(|_| streamed) {
            if live.ends_unclaimed > 0 {
                // Its turn end is read already: the lines before it are out.
                live.ends_unclaimed -= 1;
            } else {
                // The lines of this turn up to its end in the transcript go
                // first, for at most `hold_answer`.
                let now = Instant::now();
                let oldest = (live.held.len() >= stream::MAX_HELD)
                    .then(|| live.held.pop_front())
                    .flatten();
                live.held.push_back(Held {
                    thread_id,
                    answer: answer.to_owned(),
                    until: now + self.options.hold_answer,
                });
                live.next_read = Some(now);
                if let Some(oldest) = oldest {
                    self.release(session, oldest);
                }
                return;
            }
        }
        if answer.trim().is_empty() {
            return;
        }'''),
('''            Done::Stream { session, number } => {
                if let Some(live) = self.streams.get_mut(&session) {
                    live.answered(number);
                    self.stream_answered(&session);
                }
            }
            Done::Reaction(delivery) => match delivery {''', '''            Done::Stream {
                session,
                number,
                delivery,
            } => self.on_stream_done(&session, number, delivery),
            Done::Reaction(delivery) => {
                self.reactions = self.reactions.saturating_sub(1);
                self.on_reaction_done(delivery);
            }
        }
    }

    fn on_reaction_done(&mut self, delivery: Option<Delivery>) {
        match delivery {'''),
('''                Work::Stream { session, number } => Done::Stream { session, number },''',
 '''                Work::Stream { session, number } => Done::Stream {
                    session,
                    number,
                    delivery,
                },'''),
('''    /// A stream message of `session`, by its number in [`Live`].
    Stream {
        session: String,
        number: u64,
    },''', '''    /// A stream message of `session`, by its number in [`Live`].
    Stream {
        session: String,
        number: u64,
        delivery: Option<Delivery>,
    },'''),
])
print('ok')
