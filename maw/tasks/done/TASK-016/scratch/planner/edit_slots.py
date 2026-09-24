import os
here = os.path.dirname(os.path.abspath(__file__))
p = os.path.join(here, 'ws/crates/cctg/src/hub/slots.rs')
s = open(p, encoding='utf-8').read()


def rep(a, b):
    global s
    assert s.count(a) == 1, a[:90]
    s = s.replace(a, b)


rep("""//! Logs carry short session ids, slot ordinals and fixed text; never a path,
//! a folder, a title or message text.
""", """//! The live transcript stream (see [`stream`]): the agent of the slot's
//! current session reads its transcript on request and the actor turns the
//! events into topic messages (terminal prompts, text before tool calls, one
//! line per finished tool call) on the stream lane of the scheduler, after the
//! session separator. A turn's answer waits for the lines read after its
//! `Stop` (bounded by `Options::hold_answer`). A message handed to an agent
//! gets 👀, and ✍ once its own channel record shows up in the transcript.
//!
//! Logs carry short session ids, slot ordinals and fixed text; never a path,
//! a folder, a title or message text.
""")
rep("""use super::scheduler::{Delivery, Op, Outbox, Outcome};
""", """use super::scheduler::{Delivery, Op, Outbox, Outcome};
use super::stream::{self, Held, Live};
""")
rep("""use crate::wire::{AgentMsg, HookEvent, HookPost, HubMsg, PermissionRequest};
""", """use crate::wire::{AgentMsg, HookEvent, HookPost, HubMsg, PermissionRequest, StreamLine};
""")
rep("""const MAX_BODY_READS: usize = 2;
""", """const MAX_BODY_READS: usize = 2;
/// A transcript read the agent has not answered by then is asked again.
const READ_TIMEOUT: Duration = Duration::from_secs(10);
""")
rep("""    /// First pause before the parent transcript is looked at again; it doubles.
    pub recheck_after: Duration,
}
""", """    /// First pause before the parent transcript is looked at again; it doubles.
    pub recheck_after: Duration,
    /// How often the agent of a streamed session is asked for new lines.
    pub stream_every: Duration,
    /// Longest wait of a turn answer for the stream lines read after its `Stop`.
    pub hold_answer: Duration,
}
""")
rep("""            correlate_for: Duration::from_secs(60),
            recheck_after: Duration::from_secs(1),
        }""", """            correlate_for: Duration::from_secs(60),
            recheck_after: Duration::from_secs(1),
            // Records show up in the jsonl ~0.1-0.3 s after their timestamp
            // (TASK-016 measurement); a read is one small link round trip.
            stream_every: Duration::from_millis(300),
            hold_answer: Duration::from_millis(1500),
        }""")
rep("""    Block {
        job: BlockJob,
        delivery: Option<Delivery>,
    },
}

/// A job for the dispatch task.""", """    Block {
        job: BlockJob,
        delivery: Option<Delivery>,
    },
    /// A stream message of `session`, by its number in [`Live`].
    Stream {
        session: String,
        number: u64,
    },
    Reaction(Option<Delivery>),
}

/// A job for the dispatch task.""")
rep("""    Callback,
    Block(BlockJob),
}""", """    Callback,
    Block(BlockJob),
    Stream { session: String, number: u64 },
    Reaction,
}""")
rep("""    /// It acknowledges permission verdicts ([`crate::wire::Register::verdict_ack`]).
    acks: bool,
}""", """    /// It acknowledges permission verdicts ([`crate::wire::Register::verdict_ack`]).
    acks: bool,
    /// It answers transcript reads ([`crate::wire::Register::transcript_reads`]).
    reads: bool,
}""")
rep("""    /// Block jobs handed out and not answered, at most [`MAX_BLOCK_JOBS`].
    block_jobs: usize,
""", """    /// Block jobs handed out and not answered, at most [`MAX_BLOCK_JOBS`].
    block_jobs: usize,
    /// Live transcript streams by session.
    streams: HashMap<String, Live>,
    reaction_warned: bool,
""")
rep("""            block_jobs: 0,
            grace_until: now + options.grace,""", """            block_jobs: 0,
            streams: HashMap::new(),
            reaction_warned: false,
            grace_until: now + options.grace,""")
rep("""        // A session whose transcript is being read wakes the actor anyway.
        self.candidates
            .next_due(|session| self.indexing.contains(session))
            .map_or(deadline, |due| deadline.min(due))
    }""", """        // A session whose transcript is being read wakes the actor anyway.
        let deadline = self
            .candidates
            .next_due(|session| self.indexing.contains(session))
            .map_or(deadline, |due| deadline.min(due));
        self.streams
            .values()
            .flat_map(|live| {
                let read = match live.reading {
                    Some((_, sent)) => Some(sent + READ_TIMEOUT),
                    None => live.next_read,
                };
                read.into_iter().chain(live.held.as_ref().map(|held| held.until))
            })
            .fold(deadline, Instant::min)
    }""")
rep("""                        to_agent,
                        acks: register.verdict_ack,
                    },
                );""", """                        to_agent,
                        acks: register.verdict_ack,
                        reads: register.transcript_reads,
                    },
                );""")
rep("""                    AgentMsg::Reply { text } => self.on_reply_for(conn, &session, &text),
                    _ => debug!(conn, "agent message not routed yet"),""", """                    AgentMsg::Reply { text } => self.on_reply_for(conn, &session, &text),
                    AgentMsg::TranscriptChunk {
                        session_id,
                        from,
                        to,
                        lines,
                        missing,
                        more,
                    } if session_id == session => {
                        self.on_chunk(&session, from, to, &lines, missing, more);
                    }
                    _ => debug!(conn, "agent message not routed"),""")
# topic message: reaction and receipt
rep("""        if sent {
            // Delivered: the next failure starts a new episode and is told at once.
            self.notices.remove(&(slot, OFFLINE_NOTICE));""", """        if sent {
            // Delivered: the next failure starts a new episode and is told at once.
            self.notices.remove(&(slot, OFFLINE_NOTICE));
            if let Some(stream) = self
                .registry
                .sessions
                .get_mut(&session)
                .and_then(|entry| entry.stream.as_mut())
            {
                stream::receipt(stream, input.message_id);
                self.registry.dirty = true;
            }
            self.react(input.message_id, stream::ACCEPTED);""")
# turn answer hold
rep("""        if let Some(parts) = self.send_text(thread_id, session, answer, "answer") {
            info!(
                ordinal,
                session = short(session),
                parts,
                "turn answer queued"
            );
        }
    }""", """        if let Some(live) = self.streams.get_mut(session) {
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
        }
        if let Some(parts) = self.send_text(thread_id, session, answer, "answer") {
            info!(
                ordinal,
                session = short(session),
                parts,
                "turn answer queued"
            );
        }
    }

    fn release(&mut self, session: &str, held: Held) {
        if let Some(parts) = self.send_text(held.thread_id, session, &held.answer, "answer") {
            info!(session = short(session), parts, "turn answer queued");
        }
    }

    /// Sets the bot's reaction on a topic message; failures are only logged.
    fn react(&self, message_id: i64, emoji: &str) {
        self.hand_off(
            Work::Reaction,
            Op::React {
                message_id,
                emoji: emoji.to_owned(),
            },
        );
    }

    /// Where the stream of `session` goes, when it may go on now: the live
    /// top-level current session of a slot whose topic exists and whose
    /// session separator is out, with a bound agent that reads transcripts.
    fn stream_target(&self, session: &str) -> Option<(u64, String)> {
        let slot = self.current_slot(session)?;
        let slot = self.registry.slot(slot)?;
        if slot.topic_id.is_none() || slot.pending_separator.is_some() {
            return None;
        }
        let entry = self.registry.sessions.get(session)?;
        entry.stream.as_ref()?;
        if entry.transcript_path.is_empty() {
            return None;
        }
        let conn = entry.agent?;
        let bound = self.conns.get(&conn)?;
        (bound.reads && bound.session == session).then(|| (conn, entry.transcript_path.clone()))
    }

    /// Asks the agents of streamed sessions for new lines, releases held
    /// answers that waited long enough and forgets the streams of sessions
    /// that are over once nothing of theirs waits for Telegram.
    fn pump_streams(&mut self) {
        let now = Instant::now();
        let mut sessions: HashSet<String> = self
            .conns
            .values()
            .filter(|bound| bound.reads)
            .map(|bound| bound.session.clone())
            .collect();
        sessions.extend(self.streams.keys().cloned());
        for session in sessions {
            if self.current_slot(&session).is_none() {
                let Some(live) = self.streams.get_mut(&session) else {
                    continue;
                };
                let held = live.held.take();
                let idle = live.unanswered() == 0;
                if let Some(held) = held {
                    self.release(&session, held);
                }
                if idle {
                    self.streams.remove(&session);
                }
                continue;
            }
            let target = self.stream_target(&session);
            if target.is_none() && !self.streams.contains_key(&session) {
                continue;
            }
            let offset = self
                .registry
                .sessions
                .get(&session)
                .and_then(|entry| entry.stream.as_ref())
                .and_then(|stream| stream.offset);
            let live = self
                .streams
                .entry(session.clone())
                .or_insert_with(|| Live::new(offset));
            if let Some((_, sent)) = live.reading
                && now >= sent + READ_TIMEOUT
            {
                debug!(session = short(&session), "transcript read not answered; asking again");
                live.reading = None;
            }
            let released = live
                .held
                .as_ref()
                .is_some_and(|held| now >= held.until || target.is_none())
                .then(|| live.held.take())
                .flatten();
            if let Some(held) = released {
                self.release(&session, held);
            }
            let Some((conn, path)) = target else {
                continue;
            };
            let Some(live) = self.streams.get_mut(&session) else {
                continue;
            };
            let due = live.next_read.is_none_or(|at| now >= at);
            if live.reading.is_some() || !due || live.unanswered() >= stream::MAX_WAITING {
                continue;
            }
            let read = HubMsg::TranscriptRead {
                session_id: session.clone(),
                path,
                from: live.read_at,
            };
            let asked = self
                .conns
                .get(&conn)
                .is_some_and(|bound| bound.to_agent.try_send(read).is_ok());
            if asked {
                live.requests += 1;
                live.reading = Some((live.requests, now));
            } else {
                live.next_read = Some(now + self.options.stream_every);
            }
        }
    }

    /// One answered transcript read: its messages go to the topic in order,
    /// its reactions out, a held answer after them.
    fn on_chunk(
        &mut self,
        session: &str,
        from: u64,
        to: u64,
        lines: &[StreamLine],
        missing: bool,
        more: bool,
    ) {
        let now = Instant::now();
        let every = self.options.stream_every;
        let thread_id = self
            .current_slot(session)
            .and_then(|slot| self.registry.slot(slot))
            .and_then(|slot| slot.topic_id);
        let Some(live) = self.streams.get_mut(session) else {
            return;
        };
        let Some((number, _)) = live.reading.take() else {
            debug!(session = short(session), "transcript chunk nobody asked for; dropped");
            return;
        };
        live.next_read = Some(now + every);
        let Some(thread_id) = thread_id else {
            return;
        };
        if live.read_at.is_some_and(|at| at != from) || to < from {
            debug!(session = short(session), "transcript chunk out of place; asking again");
            live.next_read = Some(now);
            return;
        }
        if missing {
            if !live.missing_warned {
                live.missing_warned = true;
                warn!(
                    session = short(session),
                    "session transcript not found; the stream waits for it"
                );
            }
        } else {
            live.missing_warned = false;
            let Some(stream) = self
                .registry
                .sessions
                .get_mut(session)
                .and_then(|entry| entry.stream.as_mut())
            else {
                return;
            };
            if stream.offset.is_none() {
                stream.offset = Some(from);
            }
            let applied = stream::apply(stream, lines);
            self.registry.dirty = true;
            for out in applied.messages {
                let split = split_for_telegram(&out.text, SplitOptions::default());
                let merge = out.merge && split.chunks.len() == 1;
                for text in split.chunks {
                    let number = live.sent(out.end);
                    self.dispatch_stream(session, number, thread_id, text, merge);
                }
            }
            live.read_at = Some(to);
            live.read_up_to(to);
            if more {
                live.next_read = Some(now);
            }
            for message_id in applied.working {
                self.react(message_id, stream::WORKING);
            }
            self.stream_answered(session);
        }
        let Some(live) = self.streams.get_mut(session) else {
            return;
        };
        if live.held.as_ref().is_some_and(|held| number >= held.after)
            && let Some(held) = live.held.take()
        {
            self.release(session, held);
        }
    }

    fn dispatch_stream(&self, session: &str, number: u64, thread_id: i64, text: String, merge: bool) {
        self.hand_off(
            Work::Stream {
                session: session.to_owned(),
                number,
            },
            Op::Stream {
                thread_id,
                text,
                merge,
            },
        );
    }

    /// Moves the persisted offset over what Telegram has answered.
    fn stream_answered(&mut self, session: &str) {
        let Some(offset) = self.streams.get_mut(session).and_then(Live::advance) else {
            return;
        };
        if let Some(stream) = self
            .registry
            .sessions
            .get_mut(session)
            .and_then(|entry| entry.stream.as_mut())
        {
            stream::answered_up_to(stream, offset);
            self.registry.dirty = true;
        }
    }""")
rep("""            Done::Block { job, delivery } => self.on_block_done(job, delivery),
        }
    }""", """            Done::Block { job, delivery } => self.on_block_done(job, delivery),
            Done::Stream { session, number } => {
                if let Some(live) = self.streams.get_mut(&session) {
                    live.answered(number);
                    self.stream_answered(&session);
                }
            }
            Done::Reaction(delivery) => match delivery {
                Some(Err(error)) if !self.reaction_warned => {
                    self.reaction_warned = true;
                    warn!(%error, "cannot set a message reaction; later failures are not logged");
                }
                Some(Err(error)) => debug!(%error, "message reaction not set"),
                _ => {}
            },
        }
    }""")
rep("""        self.send_prompts();
        self.send_prompt_edits();
        let view = self.registry.topic_view();""", """        self.send_prompts();
        self.send_prompt_edits();
        self.pump_streams();
        let view = self.registry.topic_view();""")
rep("""                Work::Block(job) => Done::Block { job, delivery },
            });""", """                Work::Block(job) => Done::Block { job, delivery },
                Work::Stream { session, number } => Done::Stream { session, number },
                Work::Reaction => Done::Reaction(delivery),
            });""")
open(p, 'w', encoding='utf-8', newline='\n').write(s)
print('ok')
