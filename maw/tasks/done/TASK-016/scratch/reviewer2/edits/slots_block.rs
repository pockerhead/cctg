    fn release(&mut self, session: &str, held: Held) {
        if held.answer.trim().is_empty() {
            return;
        }
        if let Some(parts) = self.send_text(held.thread_id, session, &held.answer, "answer") {
            info!(session = short(session), parts, "turn answer queued");
        }
    }

    /// Sets the bot's reaction on a topic message; failures are only logged.
    /// At most [`MAX_REACTIONS`] wait for Telegram; one more is skipped.
    fn react(&mut self, message_id: i64, emoji: &str) {
        if self.reactions >= MAX_REACTIONS {
            debug!("too many reactions wait for Telegram; one skipped");
            return;
        }
        self.reactions += 1;
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
    /// answers that waited long enough or can no longer be matched, and
    /// forgets the streams of sessions that are over once nothing of theirs
    /// waits for Telegram.
    fn pump_streams(&mut self) {
        let now = Instant::now();
        let mut sessions: HashSet<String> = self
            .conns
            .values()
            .map(|bound| bound.session.clone())
            .collect();
        sessions.extend(self.streams.keys().cloned());
        for session in sessions {
            if self.current_slot(&session).is_none() {
                let Some(live) = self.streams.get_mut(&session) else {
                    continue;
                };
                let held: Vec<Held> = live.held.drain(..).collect();
                if live.unanswered() == 0 {
                    self.streams.remove(&session);
                }
                for held in held {
                    self.release(&session, held);
                }
                continue;
            }
            let target = self.stream_target(&session);
            if target.is_none() && !self.streams.contains_key(&session) {
                continue;
            }
            let (offset, calls) = self
                .registry
                .sessions
                .get(&session)
                .and_then(|entry| entry.stream.as_ref())
                .map(|stream| (stream.offset, stream.calls.clone()))
                .unwrap_or_default();
            let live = self
                .streams
                .entry(session.clone())
                .or_insert_with(|| Live::new(offset, calls));
            if let Some(sent) = live.reading
                && now >= sent + READ_TIMEOUT
            {
                debug!(
                    session = short(&session),
                    "transcript read not answered; asking again"
                );
                live.reading = None;
            }
            let mut released = Vec::new();
            while live
                .held
                .front()
                .is_some_and(|held| target.is_none() || now >= held.until)
            {
                released.extend(live.held.pop_front());
            }
            for held in released {
                self.release(&session, held);
            }
            let Some((conn, path)) = target else {
                continue;
            };
            let Some(live) = self.streams.get_mut(&session) else {
                continue;
            };
            let due = live.next_read.is_none_or(|at| now >= at);
            if live.reading.is_some()
                || !due
                || live.unanswered() >= stream::MAX_WAITING
                || self.queued_messages >= STREAM_QUEUE
            {
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
                live.reading = Some(now);
            } else {
                live.next_read = Some(now + self.options.stream_every);
            }
        }
    }

    /// One answered transcript read: its messages go to the topic in order,
    /// its reactions out, a held answer after the lines of its turn.
    #[allow(clippy::too_many_arguments)]
    fn on_chunk(
        &mut self,
        session: &str,
        from: u64,
        to: u64,
        lines: &[StreamLine],
        missing: bool,
        more: bool,
        reset: bool,
    ) {
        enum Action {
            Stream(u64, Op),
            Release,
            React(i64),
        }
        let now = Instant::now();
        let every = self.options.stream_every;
        let thread_id = self
            .current_slot(session)
            .and_then(|slot| self.registry.slot(slot))
            .and_then(|slot| slot.topic_id);
        let Some(live) = self.streams.get_mut(session) else {
            return;
        };
        if live.reading.take().is_none() {
            debug!(
                session = short(session),
                "transcript chunk nobody asked for; dropped"
            );
            return;
        }
        live.next_read = Some(now + every);
        let Some(thread_id) = thread_id else {
            return;
        };
        if live.read_at.is_some_and(|at| at != from) || to < from {
            debug!(
                session = short(session),
                "transcript chunk out of place; asking again"
            );
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
            // No turn end can come from a file that is not there.
            let held: Vec<Held> = live.held.drain(..).collect();
            for held in held {
                self.release(session, held);
            }
            return;
        }
        live.missing_warned = false;
        if reset {
            if !live.reset_warned {
                live.reset_warned = true;
                warn!(
                    session = short(session),
                    "session transcript was cut or replaced; the stream reads it again from its start"
                );
            }
            live.read_at = Some(0);
            live.calls.clear();
            live.next_read = Some(now);
            return;
        }
        live.reset_warned = false;
        let Some(stream) = self
            .registry
            .sessions
            .get_mut(session)
            .and_then(|entry| entry.stream.as_mut())
        else {
            return;
        };
        if stream.offset.is_none() {
            // The first read of a stream that starts at the end of the file.
            stream.offset = Some(from);
            self.registry.dirty = true;
        }
        let mut actions = Vec::new();
        let mut queued = self.queued_messages;
        let mut read_to = to;
        let mut stopped = false;
        for (index, line) in lines.iter().enumerate() {
            // The first line always goes (the read was asked with room); the
            // rest wait in the file while Telegram is behind.
            if index > 0 && (live.unanswered() >= stream::MAX_WAITING || queued >= STREAM_QUEUE) {
                read_to = lines[index - 1].end;
                stopped = true;
                break;
            }
            for step in stream::apply_line(&mut live.calls, &mut stream.receipts, &line.items) {
                match step {
                    Step::Send { text, merge } => {
                        let split = split_for_telegram(&text, SplitOptions::default());
                        let merge = merge && split.chunks.len() == 1;
                        for text in split.chunks {
                            queued += 1;
                            let op = Op::Stream {
                                thread_id,
                                text,
                                merge,
                            };
                            actions.push(Action::Stream(live.sent(), op));
                        }
                    }
                    Step::Working(message_id) => {
                        live.ends_unclaimed = 0;
                        self.registry.dirty = true;
                        actions.push(Action::React(message_id));
                    }
                    Step::NewTurn => live.ends_unclaimed = 0,
                    Step::TurnEnd if live.held.is_empty() => {
                        live.ends_unclaimed = live
                            .ends_unclaimed
                            .saturating_add(1)
                            .min(stream::MAX_HELD as u8);
                    }
                    Step::TurnEnd => actions.push(Action::Release),
                }
            }
        }
        live.barrier(read_to);
        if more || stopped {
            live.next_read = Some(now);
        }
        for action in actions {
            match action {
                Action::Stream(number, op) => {
                    self.queued_messages += 1;
                    self.hand_off(
                        Work::Stream {
                            session: session.to_owned(),
                            number,
                        },
                        op,
                    );
                }
                Action::Release => {
                    let held = self
                        .streams
                        .get_mut(session)
                        .and_then(|live| live.held.pop_front());
                    if let Some(held) = held {
                        self.release(session, held);
                    }
                }
                Action::React(message_id) => self.react(message_id, stream::WORKING),
            }
        }
        self.stream_answered(session);
    }

    /// Telegram answered stream message `number` of `session`.
    fn on_stream_done(&mut self, session: &str, number: u64, delivery: Option<Delivery>) {
        self.queued_messages = self.queued_messages.saturating_sub(1);
        if self.queued_messages == 0 {
            self.overflow_warned = false;
        }
        let accepted = match &delivery {
            Some(Ok(Outcome::Sent(_) | Outcome::Merged)) => true,
            // A message Telegram will never take (bad request, not a lost
            // topic) is skipped rather than sent again for ever.
            Some(result @ Err(ApiError::Telegram { code, .. }))
                if (400..500).contains(code) && !topic_gone(result) =>
            {
                warn!(
                    session = short(session),
                    "stream message refused by Telegram; skipped"
                );
                true
            }
            _ => false,
        };
        let Some(live) = self.streams.get_mut(session) else {
            return;
        };
        live.answered(number, accepted);
        if !accepted && !live.refused_warned {
            live.refused_warned = true;
            warn!(
                session = short(session),
                "stream message not delivered; the stream sends it again"
            );
        }
        self.stream_answered(session);
        let Some(live) = self.streams.get_mut(session) else {
            return;
        };
        if live.stuck() {
            let (offset, calls) = self
                .registry
                .sessions
                .get(session)
                .and_then(|entry| entry.stream.as_ref())
                .map(|stream| (stream.offset, stream.calls.clone()))
                .unwrap_or_default();
            live.rewind(offset, calls, Instant::now() + REWIND_AFTER);
        }
    }

    /// Moves the persisted offset and open calls to the last barrier whose
    /// messages Telegram has all accepted.
    fn stream_answered(&mut self, session: &str) {
        let Some(live) = self.streams.get_mut(session) else {
            return;
        };
        let Some((offset, calls)) = live.advance() else {
            return;
        };
        live.refused_warned = false;
        if let Some(stream) = self
            .registry
            .sessions
            .get_mut(session)
            .and_then(|entry| entry.stream.as_mut())
            && (stream.offset != Some(offset) || stream.calls != calls)
        {
            stream.offset = Some(offset);
            stream.calls = calls;
            self.registry.dirty = true;
        }
    }

