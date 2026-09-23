    /// Remembers a relayed permission request; [`Self::send_prompts`] puts it
    /// into the topic of the session's own slot. Unlike a reply it needs no
    /// "current session of the slot" check: a session that ended has its
    /// prompts closed (see [`Self::close_ended_prompts`]).
    fn on_permission_request(&mut self, conn: u64, request: PermissionRequest) {
        let Some(bound) = self.conns.get(&conn) else {
            return;
        };
        if !is_request_id(&request.request_id) {
            debug!(
                conn,
                "permission request without a valid request id; dropped"
            );
            return;
        }
        let session = bound.session.clone();
        let prompt = Prompt::new(
            conn,
            bound.host.clone(),
            bound.claude_pid,
            session.clone(),
            &request,
        );
        match self.prompts.open(prompt) {
            Opened::Added { expired, .. } => {
                info!(
                    conn,
                    session = short(&session),
                    "permission request queued for the topic"
                );
                if let Some(gone) = expired {
                    self.expire(gone);
                }
            }
            Opened::Duplicate => debug!(conn, "permission request already shown; not repeated"),
            Opened::Full => warn!(
                conn,
                session = short(&session),
                "too many permission prompts wait for an answer; this one only in the terminal"
            ),
        }
        self.sync_waiting(&session);
    }

    /// An open prompt the full book let go: its buttons go away. One try: a
    /// press on buttons that stayed only answers "expired".
    fn expire(&mut self, gone: Prompt) {
        info!(
            session = short(&gone.session),
            "oldest open permission prompt expired to make room"
        );
        if let Some(message_id) = gone.message_id {
            self.hand_off(
                Work::Callback,
                Op::Edit {
                    message_id,
                    text: permissions::ANSWER_EXPIRED.to_owned(),
                    reply_markup: Some(permissions::no_keyboard()),
                },
            );
        }
        self.sync_waiting(&gone.session);
    }

    /// The waiting icon of `session` shows whether it has a prompt that
    /// still counts (see [`Prompt::waits`]).
    fn sync_waiting(&mut self, session: &str) {
        let waiting = self.prompts.waiting(session);
        self.registry.set_waiting(session, waiting);
    }

    /// Ends an active prompt: its final edit goes out on the next pump.
    fn finish(&mut self, key: u64, state: State) {
        let Some(session) = self.prompts.get(key).map(|prompt| prompt.session.clone()) else {
            return;
        };
        if self.prompts.finish(key, state) {
            self.sync_waiting(&session);
        }
    }

    /// Closes the active prompts of every session the registry marks ended:
    /// by its SessionEnd, by `/clear` or by a new session on a reused pid.
    /// A SessionEnd the registry ignored (a nested resume) closes nothing.
    fn close_ended_prompts(&mut self) {
        for key in self.prompts.active() {
            let Some(session) = self.prompts.get(key).map(|prompt| prompt.session.clone()) else {
                continue;
            };
            if self
                .registry
                .sessions
                .get(&session)
                .is_some_and(|entry| entry.ended)
            {
                info!(
                    session = short(&session),
                    "permission prompt closed: its session ended"
                );
                self.finish(key, State::Closed);
            }
        }
    }

    /// Hands every active prompt whose session's slot has a topic to
    /// Telegram, on the permission lane. Prompts are never counted against
    /// [`MAX_QUEUED_MESSAGES`]; [`permissions::MAX_PROMPTS`] bounds them.
    fn send_prompts(&mut self) {
        for key in self.prompts.unsent() {
            let Some(prompt) = self.prompts.get(key) else {
                continue;
            };
            let thread_id = self
                .registry
                .sessions
                .get(&prompt.session)
                .and_then(|entry| entry.slot)
                .and_then(|slot| self.registry.slot(slot))
                .and_then(|slot| slot.topic_id);
            let Some(thread_id) = thread_id else {
                continue;
            };
            let op = Op::Send {
                thread_id: Some(thread_id),
                text: prompt.text.clone(),
                reply_markup: Some(permissions::keyboard(&prompt.request_id)),
                permission: true,
            };
            if let Some(prompt) = self.prompts.get_mut(key) {
                prompt.sent = true;
            }
            self.hand_off(Work::Permission(key), op);
        }
    }

    /// Hands out the final edits that are due: the decision or the end of
    /// the session, always without buttons.
    fn send_prompt_edits(&mut self) {
        for key in self.prompts.due_edits() {
            let Some(prompt) = self.prompts.get_mut(key) else {
                continue;
            };
            let (Some(message_id), Some(text)) = (prompt.message_id, prompt.final_text()) else {
                continue;
            };
            prompt.edit = Edit::InFlight;
            self.hand_off(
                Work::PromptEdit(key),
                Op::Edit {
                    message_id,
                    text,
                    reply_markup: Some(permissions::no_keyboard()),
                },
            );
        }
    }

    /// Answers every button press at once; the final edit follows when the
    /// prompt ends.
    fn on_callback(&mut self, input: CallbackInput) {
        let answer = self.press(&input);
        self.hand_off(
            Work::Callback,
            Op::AnswerCallback {
                query_id: input.query_id,
                text: answer.map(str::to_owned),
            },
        );
    }

    /// The first press on an open prompt fixes the answer for good; later
    /// presses only push the same verdict again. Buttons that are not
    /// permission buttons get an empty answer.
    fn press(&mut self, input: &CallbackInput) -> Option<&'static str> {
        let Some((behavior, request_id)) =
            input.data.as_deref().and_then(permissions::parse_callback)
        else {
            debug!("button press that is not a permission answer");
            return None;
        };
        let expired = Some(permissions::ANSWER_EXPIRED);
        let Some(message_id) = input.message_id else {
            return expired;
        };
        let Some(key) = self.prompts.by_message(message_id) else {
            debug!("button of a prompt this hub does not know");
            return expired;
        };
        let Some(prompt) = self
            .prompts
            .get_mut(key)
            .filter(|prompt| prompt.request_id == request_id)
        else {
            return expired;
        };
        match prompt.state {
            State::Closed => expired,
            State::Selected { .. } | State::Decided(_) => {
                debug!(
                    session = short(&prompt.session),
                    "prompt already answered; no second verdict"
                );
                self.push_verdict(key);
                Some(permissions::ANSWER_DECIDED)
            }
            State::Open => {
                prompt.state = State::Selected {
                    behavior,
                    verdict_id: crate::wire::random_u64(),
                };
                info!(
                    session = short(&prompt.session),
                    ?behavior,
                    "permission answer chosen in Telegram"
                );
                if self.push_verdict(key) {
                    Some(permissions::answer(behavior))
                } else {
                    Some(permissions::ANSWER_OFFLINE)
                }
            }
        }
    }

    /// Hands the fixed answer of a selected prompt to an agent of its
    /// session. An agent that acknowledges verdicts decides the prompt with
    /// its ack; for an older agent the hand-off is all there is to know.
    /// `false`: no agent of the session could take it now.
    fn push_verdict(&mut self, key: u64) -> bool {
        let Some(prompt) = self.prompts.get(key) else {
            return false;
        };
        let State::Selected {
            behavior,
            verdict_id,
        } = prompt.state
        else {
            return false;
        };
        let Some((conn, bound)) = self
            .verdict_conn(prompt)
            .and_then(|conn| self.conns.get(&conn).map(|bound| (conn, bound)))
        else {
            info!(
                session = short(&prompt.session),
                "permission answer for an agent that is not on line; it waits"
            );
            return false;
        };
        let verdict = HubMsg::PermissionVerdict {
            request_id: prompt.request_id.clone(),
            behavior,
            verdict_id: bound.acks.then_some(verdict_id),
        };
        if bound.to_agent.try_send(verdict).is_err() {
            warn!(
                conn,
                "agent queue full or closed; the permission answer waits"
            );
            return false;
        }
        info!(
            conn,
            session = short(&prompt.session),
            ?behavior,
            "permission verdict forwarded to the session agent"
        );
        if !bound.acks {
            self.finish(key, State::Decided(behavior));
        }
        true
    }

    /// Selected prompts go again to their session's agents: after a link
    /// came back (`session`) or on the retry tick (all). The agent drops a
    /// verdict id it already passed on and acks it again.
    fn push_selected(&mut self, session: Option<&str>) {
        for key in self.prompts.selected() {
            let wanted = self
                .prompts
                .get(key)
                .is_some_and(|prompt| session.is_none_or(|session| prompt.session == session));
            if wanted {
                self.push_verdict(key);
            }
        }
    }

    /// The agent took the verdict: the prompt is decided.
    fn on_verdict_ack(&mut self, conn: u64, verdict_id: u64) {
        let Some(key) = self.prompts.by_verdict(verdict_id) else {
            debug!(conn, "ack of a verdict nothing waits for");
            return;
        };
        let Some(prompt) = self.prompts.get(key) else {
            return;
        };
        let State::Selected { behavior, .. } = prompt.state else {
            return;
        };
        if self
            .conns
            .get(&conn)
            .is_none_or(|bound| bound.session != prompt.session)
        {
            debug!(conn, "verdict ack from an agent of another session; ignored");
            return;
        }
        info!(
            conn,
            session = short(&prompt.session),
            "permission verdict taken by the session agent"
        );
        self.finish(key, State::Decided(behavior));
    }

    /// An agent bound to the prompt's session: the one that relayed it, or
    /// after a link drop the newest connection of the same claude process.
    /// A connection that follows another session (after `/clear`) or another
    /// process on a reused pid never qualifies.
    fn verdict_conn(&self, prompt: &Prompt) -> Option<u64> {
        let serves = |bound: &Conn| bound.session == prompt.session;
        if self.conns.get(&prompt.conn).is_some_and(serves) {
            return Some(prompt.conn);
        }
        let pid = prompt.claude_pid?;
        self.conns
            .iter()
            .filter(|(_, bound)| {
                serves(bound) && bound.host == prompt.host && bound.claude_pid == Some(pid)
            })
            .map(|(conn, _)| *conn)
            .max()
    }

