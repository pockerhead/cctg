# Production edits of slots.rs for TASK-017 (applied once to ws/).
import io, os
p = os.path.join(os.path.dirname(os.path.abspath(__file__)), 'ws', 'crates', 'cctg', 'src', 'hub', 'slots.rs')
s = io.open(p, encoding='utf-8', newline='').read()

def rep(old, new):
    global s
    assert s.count(old) == 1, old
    s = s.replace(old, new)

rep("""//! Topic messages from allowlisted users go to the agent of the slot's
//! current session with a non-blocking `try_send`; agent replies and the
//! final answer of each turn (from the `Stop` hook) go back to the session's
//! topic through the same dispatch task. At most
//! [`MAX_QUEUED_MESSAGES`] such messages wait for Telegram at a time, and a
//! slot gets each kind of notice at most once per `Options::notice_every`.
""", """//! Topic messages from allowlisted users go to the agent of the slot's
//! current session with a non-blocking `try_send`. A message no live agent
//! can take waits in its slot (see [`buffer`], kept in `registry.json`) and
//! goes, in order, to the first live top-level session of the slot whose
//! agent is bound; a dead slot with waiting messages shows one Resume button.
//! Agent replies and the final answer of each turn (from the `Stop` hook) go
//! back to the session's topic through the same dispatch task. At most
//! [`MAX_QUEUED_MESSAGES`] such messages wait for Telegram at a time, and a
//! slot gets the text-only notice at most once per `Options::notice_every`.
""")
rep("""use super::api::{ApiError, Document};
""", """use super::api::{ApiError, Document};
use super::buffer::{self, Parked, ResumeNote};
""")
rep("""    BlockJob, BlockKey, Icons, Registry, RegistryStore, SessionKind, SlotId, TopicJob, TopicView,
};""", """    BlockJob, BlockKey, Icons, Registry, RegistryStore, SessionKind, SlotId, SlotState, TopicJob,
    TopicView,
};""")
rep("""pub const OFFLINE_NOTICE: &str = "Сессия этой темы не на связи, сообщение не доставлено.";
""", "")
rep("""    /// A slot gets the same notice at most once per this long; a burst of
    /// messages to a dead session would eat the group's 20 messages a minute.
""", """    /// A slot gets the text-only notice at most once per this long; a burst
    /// of photos would eat the group's 20 messages a minute.
""")
rep("""    /// A button answer or the edit of an expired prompt.
    Callback(Option<Delivery>),
""", """    /// A button answer, the edit of an expired prompt or of a Resume
    /// message whose period ended.
    Callback(Option<Delivery>),
    /// The Resume message of `slot`'s offline period for `session`.
    Resume {
        slot: SlotId,
        session: String,
        delivery: Option<Delivery>,
    },
""")
rep("""    Callback,
    Block(BlockJob),
    Stream { session: String, number: u64 },
    Reaction,
}""", """    Callback,
    Resume { slot: SlotId, session: String },
    Block(BlockJob),
    Stream { session: String, number: u64 },
    Reaction,
}""")

# on_topic_message: park, then flush.
start = s.index("    /// Forwards a topic message to the agent of the slot's current session,")
end = s.index("    /// The running top-level session of `slot` and its agent connection.")
s = s[:start] + '''    /// Keeps a topic message in its slot and hands the slot's messages to
    /// the agent of its live current session, when there is one. General and
    /// topics that are not slots reach no agent and get no answer.
    fn on_topic_message(&mut self, input: Inbound) {
        let Some(thread_id) = input.thread_id else {
            debug!("message outside a topic; not forwarded");
            return;
        };
        let Some(slot) = self.registry.slot_by_topic(thread_id) else {
            debug!("message in a topic without a slot; not forwarded");
            return;
        };
        let Some(text) = input.text else {
            self.notify(slot, thread_id, TEXT_ONLY_NOTICE);
            return;
        };
        self.park(
            slot,
            Parked {
                message_id: input.message_id,
                thread_id,
                text,
                reply_to: input.reply_to,
            },
        );
        self.flush(slot);
    }

    /// Adds a message to the slot's buffer. A full buffer drops its oldest
    /// message and tells the topic once per offline period; a running
    /// session without an agent is told once per period too (a dead one
    /// gets its Resume message from [`Self::offer_resume`]).
    fn park(&mut self, slot: SlotId, parked: Parked) {
        let ordinal = self.ordinal(slot);
        let offline = self.live_agent(slot).is_none();
        let dead = self.registry.state(slot) == SlotState::Dead;
        let thread_id = parked.thread_id;
        let Some(entry) = self.registry.slot_mut(slot) else {
            return;
        };
        let dropped = entry.buffer.push(parked);
        let tell_overflow = dropped && !entry.buffer.overflow_told;
        let tell_queued = offline && !dead && !entry.buffer.queued_told;
        self.registry.dirty = true;
        if offline {
            info!(
                ordinal,
                "message kept for the slot until a session is on line"
            );
        }
        if dropped {
            debug!(ordinal, "slot buffer full; its oldest message dropped");
        }
        if tell_overflow && self.send_messages(vec![message_op(thread_id, buffer::OVERFLOW_NOTICE.to_owned())]) {
            info!(ordinal, "slot buffer full; the topic is told once");
            if let Some(entry) = self.registry.slot_mut(slot) {
                entry.buffer.overflow_told = true;
            }
        }
        if tell_queued && self.send_messages(vec![message_op(thread_id, buffer::QUEUED_NOTICE.to_owned())]) {
            if let Some(entry) = self.registry.slot_mut(slot) {
                entry.buffer.queued_told = true;
            }
        }
    }

    /// Hands the kept messages of `slot` to the agent of its live top-level
    /// current session, oldest first, until one does not fit the link
    /// queue (the rest wait for the next try). A message leaves the buffer
    /// when its link queue took it. An emptied buffer ends the slot's
    /// offline period: the Resume button goes away.
    fn flush(&mut self, slot: SlotId) {
        let Some((session, conn)) = self.live_agent(slot) else {
            return;
        };
        let ordinal = self.ordinal(slot);
        let mut handed = 0;
        while let Some(parked) = self
            .registry
            .slot(slot)
            .and_then(|entry| entry.buffer.messages.front())
            .cloned()
        {
            let inbound = self.inbound(&session, &parked);
            let sent = self
                .conns
                .get(&conn)
                .is_some_and(|bound| bound.to_agent.try_send(inbound).is_ok());
            if !sent {
                debug!(
                    ordinal,
                    session = short(&session),
                    "agent queue full or closed; messages stay in the slot"
                );
                break;
            }
            if let Some(entry) = self.registry.slot_mut(slot) {
                entry.buffer.messages.pop_front();
            }
            self.registry.dirty = true;
            handed += 1;
            if let Some(stream) = self
                .registry
                .sessions
                .get_mut(&session)
                .and_then(|entry| entry.stream.as_mut())
            {
                stream::receipt(stream, parked.message_id);
            }
            self.react(parked.message_id, stream::ACCEPTED);
            info!(
                ordinal,
                session = short(&session),
                "message forwarded to the session agent"
            );
        }
        let Some(entry) = self.registry.slot_mut(slot) else {
            return;
        };
        if !entry.buffer.messages.is_empty() || entry.buffer.is_idle() {
            return;
        }
        let period = entry.buffer.resume.is_some() || entry.buffer.queued_told;
        let note = entry.buffer.close();
        self.registry.dirty = true;
        if period {
            info!(
                ordinal,
                session = short(&session),
                handed,
                "kept messages handed to the slot's session; offline period over"
            );
        }
        if let Some(message_id) = note.and_then(|note| note.message_id) {
            self.drop_resume_button(message_id);
        }
    }

    /// The Inbound of a topic message for `session`: meta `chat_id`,
    /// `message_id`, `thread_id`, `reply_to_message_id` for an explicit
    /// reply, and `target_agent` for a reply to a block of its subagent.
    fn inbound(&self, session: &str, parked: &Parked) -> HubMsg {
        let mut meta = BTreeMap::from([
            ("chat_id".to_owned(), self.options.chat_id.to_string()),
            ("message_id".to_owned(), parked.message_id.to_string()),
            ("thread_id".to_owned(), parked.thread_id.to_string()),
        ]);
        if let Some(reply_to) = parked.reply_to {
            meta.insert("reply_to_message_id".to_owned(), reply_to.to_string());
            // A reply to a block of this session's subagent is for that
            // subagent; Claude forwards it (channel instructions).
            if let Some(agent_id) =
                self.registry
                    .subagent_of_message(parked.thread_id, reply_to, session)
            {
                meta.insert("target_agent".to_owned(), agent_id.to_owned());
            }
        }
        HubMsg::Inbound {
            content: parked.text.clone(),
            meta,
        }
    }

    /// Every slot with kept messages tries its live session again: the
    /// session or its agent may have come (back) since.
    fn flush_all(&mut self) {
        let waiting: Vec<SlotId> = (0..self.registry.slots.len())
            .map(SlotId)
            .filter(|&slot| {
                self.registry
                    .slot(slot)
                    .is_some_and(|entry| !entry.buffer.messages.is_empty())
            })
            .collect();
        for slot in waiting {
            self.flush(slot);
        }
    }

    /// A dead slot with kept messages gets one Resume message per offline
    /// period, sent at most once (a lost one is not sent again).
    fn offer_resume(&mut self) {
        for index in 0..self.registry.slots.len() {
            let slot = SlotId(index);
            let entry = &self.registry.slots[index];
            if entry.buffer.messages.is_empty()
                || entry.buffer.resume.is_some()
                || self.registry.state(slot) != SlotState::Dead
            {
                continue;
            }
            let (Some(thread_id), Some(session)) = (entry.topic_id, entry.current_session.clone())
            else {
                continue;
            };
            if self.queued_messages >= MAX_QUEUED_MESSAGES {
                return;
            }
            let reply_markup = buffer::callback_data(&session).map(buffer::keyboard);
            if reply_markup.is_none() {
                debug!(session = short(&session), "session id too long for a Resume button");
            }
            self.registry.slots[index].buffer.resume = Some(ResumeNote {
                session: session.clone(),
                message_id: None,
            });
            self.registry.dirty = true;
            self.queued_messages += 1;
            info!(
                ordinal = self.ordinal(slot),
                session = short(&session),
                "Resume button offered for the slot's kept messages"
            );
            self.hand_off(
                Work::Resume {
                    slot,
                    session: session.clone(),
                },
                Op::Send {
                    thread_id: Some(thread_id),
                    text: buffer::resume_text(&session),
                    reply_markup,
                    permission: false,
                },
            );
        }
    }

    /// The Resume message of a period that is over loses its button. One
    /// try: a press on a button that stayed gets a fitting answer anyway.
    fn drop_resume_button(&mut self, message_id: i64) {
        self.hand_off(
            Work::Callback,
            Op::Edit {
                message_id,
                text: buffer::RESUMED_TEXT.to_owned(),
                reply_markup: Some(permissions::no_keyboard()),
            },
        );
    }

    /// Telegram answered the Resume message of `slot` for `session`: its id
    /// is kept for the edit at the end of the period, or, when that period
    /// is over already, the button goes now.
    fn on_resume_done(&mut self, slot: SlotId, session: &str, delivery: Option<Delivery>) {
        self.queued_messages = self.queued_messages.saturating_sub(1);
        if self.queued_messages == 0 {
            self.overflow_warned = false;
        }
        let message_id = match &delivery {
            Some(Ok(Outcome::Sent(message))) if message.message_id != 0 => Some(message.message_id),
            Some(Err(error)) => {
                warn!(%error, "Resume message not delivered; offered again in the next offline period");
                None
            }
            _ => None,
        };
        let Some(message_id) = message_id else {
            return;
        };
        let waiting = self.registry.slot_mut(slot).and_then(|entry| {
            entry
                .buffer
                .resume
                .as_mut()
                .filter(|note| note.session == session && note.message_id.is_none())
        });
        match waiting {
            Some(note) => {
                note.message_id = Some(message_id);
                self.registry.dirty = true;
            }
            None => self.drop_resume_button(message_id),
        }
    }

    /// A Resume press: recorded for the ended current session of its slot
    /// (bringing it back is TASK-019), otherwise told why nothing happens.
    fn press_resume(&mut self, session: &str) -> &'static str {
        if self.registry.is_live_top_level(session) {
            return buffer::ANSWER_ALIVE;
        }
        let slot = self
            .registry
            .sessions
            .get(session)
            .and_then(|entry| entry.slot)
            .filter(|&slot| {
                self.registry
                    .slot(slot)
                    .is_some_and(|entry| entry.current_session.as_deref() == Some(session))
                    && self.registry.state(slot) == SlotState::Dead
            });
        let Some(slot) = slot else {
            debug!("Resume button of a session that is not its slot's ended one");
            return permissions::ANSWER_EXPIRED;
        };
        let ordinal = self.ordinal(slot);
        if let Some(entry) = self.registry.slot_mut(slot)
            && !entry.buffer.resume_asked
        {
            entry.buffer.resume_asked = true;
            self.registry.dirty = true;
        }
        info!(
            ordinal,
            session = short(session),
            "Resume asked in Telegram; starting a session from there is not available yet"
        );
        buffer::ANSWER_UNAVAILABLE
    }

''' + s[end:]

rep("""    fn press(&mut self, input: &CallbackInput) -> Option<&'static str> {
        let Some((behavior, request_id)) =""", """    fn press(&mut self, input: &CallbackInput) -> Option<&'static str> {
        if let Some(session) = input.data.as_deref().and_then(buffer::parse_callback) {
            return Some(self.press_resume(session));
        }
        let Some((behavior, request_id)) =""")
rep("""            Done::Block { job, delivery } => self.on_block_done(job, delivery),
            Done::Stream {""", """            Done::Resume {
                slot,
                session,
                delivery,
            } => self.on_resume_done(slot, &session, delivery),
            Done::Block { job, delivery } => self.on_block_done(job, delivery),
            Done::Stream {""")
rep("""    fn pump(&mut self) {
        let edits = Instant::now() >= self.grace_until;""", """    fn pump(&mut self) {
        self.flush_all();
        self.offer_resume();
        let edits = Instant::now() >= self.grace_until;""")
rep("""                Work::Callback => Done::Callback(delivery),
                Work::Block""", """                Work::Callback => Done::Callback(delivery),
                Work::Resume { slot, session } => Done::Resume {
                    slot,
                    session,
                    delivery,
                },
                Work::Block""")
rep("""    /// Hands pending topic work and prompts to the dispatch task, publishes
    /// the view and the snapshot to save.""", """    /// Hands kept messages to sessions that can take them now, offers Resume
    /// buttons, hands pending topic work and prompts to the dispatch task,
    /// publishes the view and the snapshot to save.""")
io.open(p, 'w', encoding='utf-8', newline='\n').write(s)
print('ok')
