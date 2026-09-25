"""TASK-047 implementer: one-shot edit of crates/cctg/src/hub/slots.rs (kept for traceability)."""
p = 'C:/Users/user/dev/cctg/crates/cctg/src/hub/slots.rs'
s = open(p, encoding='utf-8').read()


def rep(old, new):
    global s
    assert s.count(old) == 1, old[:120]
    s = s.replace(old, new)


rep('''/// An update press is forgotten this long after it came.
pub const UPDATE_WAIT: Duration = Duration::from_secs(120);
''', '''/// An update press is forgotten this long after it came.
pub const UPDATE_WAIT: Duration = Duration::from_secs(120);
/// A restart held back by background agents or the agent view on the
/// terminal (TASK-047) is asked again after this.
pub const UPDATE_RETRY: Duration = Duration::from_secs(30);
/// The channel message a session gets from the hub once its next agent is
/// bound, after a client restart cut off its work (TASK-047).
pub const CONTINUE_TEXT: &str = "Клиент cctg обновлён, и сессия была перезапущена посреди работы. \\
Продолжи с того места, где остановился.";
/// PROBE-DEPENDENT (TASK-047): the sentence of [`CONTINUE_TEXT`] about
/// background agents the restart ended. Whether such an agent goes on from
/// its transcript after `SendMessage` or must be started again is decided by
/// a live probe; until then the text covers both.
pub const CONTINUE_AGENTS_TEXT: &str =
    "Если работали фоновые агенты, проверь их и при необходимости запусти заново.";
''')

rep('''    /// A restart held back because a turn began: its agent was not released
    /// and gives the `update` up by itself; that answer is only noted.
    held: Option<(u64, u64)>,
}
''', '''    /// A restart held back because a turn began: its agent was not released
    /// and gives the `update` up by itself; that answer is only noted.
    held: Option<(u64, u64)>,
    /// A restart held back by background agents (TASK-047) is asked again
    /// then.
    retry_at: Option<Instant>,
    /// The topic was told that the restart waits for background agents.
    agents_told: bool,
    /// ⏹ was written into the session's console while the press waited: a
    /// restart cuts off that work.
    interrupted: bool,
}
''')

rep('''                told: false,
                held: None,
            },
        );
        if self.busy(session) {''', '''                told: false,
                held: None,
                retry_at: None,
                agents_told: false,
                interrupted: false,
            },
        );
        if self.busy(session) {''')

rep('''            if ask.sent.is_some() || ask.held.is_some() || self.busy(&session) {
                continue;
            }''', '''            if ask.sent.is_some()
                || ask.held.is_some()
                || ask.retry_at.is_some_and(|at| now < at)
                || self.busy(&session)
            {
                continue;
            }''')

rep('''            info!(
                conn,
                session = short(session),
                "restart held back: a turn runs"
            );
            return;
        }
''', '''            info!(
                conn,
                session = short(session),
                "restart held back: a turn runs"
            );
            return;
        }
        if outcome == UpdateOutcome::AgentsRunning
            && let Some(ask) = self.updates.get_mut(session).filter(|ask| {
                ask.sent == Some((update_id, conn)) || ask.left == Some((update_id, conn))
            })
        {
            // The terminal shows background agents or the agent view
            // (TASK-047): no `/exit` now. Asked again later, like a press
            // waiting for a turn: this round does not count, the press is
            // kept, the topic is told once.
            let now = Instant::now();
            ask.sent = None;
            ask.left = None;
            ask.rounds = ask.rounds.saturating_sub(1);
            ask.until = now + UPDATE_WAIT;
            ask.retry_at = Some(now + UPDATE_RETRY);
            let tell = !std::mem::replace(&mut ask.agents_told, true);
            info!(
                conn,
                session = short(session),
                "restart held back: background agents"
            );
            self.keep_agent(conn, session);
            if tell
                && let Some(slot) = self.current_slot(session)
                && let Some(thread_id) = self.registry.slot(slot).and_then(|slot| slot.topic_id)
            {
                self.notify(slot, thread_id, status::UPDATE_AGENTS_NOTICE);
            }
            return;
        }
''')

rep('''            if let Some(ask) = self.updates.get_mut(session).filter(|_| asked) {
                ask.sent = None;
                ask.left = Some((update_id, conn));
            }
''', '''            if outcome == UpdateOutcome::Restarting && self.restart_cuts_work(session) {
                if let Some(entry) = self.registry.sessions.get_mut(session) {
                    entry.restart_interrupted = true;
                    self.registry.dirty = true;
                }
                info!(
                    session = short(session),
                    "the restart cuts off work; the session is told after it"
                );
            }
            if let Some(ask) = self.updates.get_mut(session).filter(|_| asked) {
                ask.sent = None;
                ask.left = Some((update_id, conn));
            }
''')

rep('''        if asked {
            self.updates.remove(session);
        }
        if self.conns.get(&conn).is_some_and(|bound| bound.leaving) {
            if let Some(bound) = self.conns.get_mut(&conn) {
                bound.leaving = false;
            }
            if self.registry.agent_connected(session, conn) {
                info!(conn, session = short(session), "leaving agent stays");
            }
        }
        let notice = match outcome {''', '''        if asked {
            self.updates.remove(session);
        }
        self.keep_agent(conn, session);
        let notice = match outcome {''')

rep('''            UpdateOutcome::DraftInInput => status::DRAFT_NOTICE,
            _ => status::UPDATE_FAILED_NOTICE,
        };''', '''            UpdateOutcome::DraftInInput => status::DRAFT_NOTICE,
            UpdateOutcome::AgentsRunning => status::UPDATE_AGENTS_NOTICE,
            _ => status::UPDATE_FAILED_NOTICE,
        };''')

rep('''    /// The session works: a turn or a call runs and no Esc went in yet.
    fn busy(&self, session: &str) -> bool {''', '''    /// A leaving agent `conn` of `session` whose update did not go through
    /// after all is bound back; a restart it announced did not happen.
    fn keep_agent(&mut self, conn: u64, session: &str) {
        let Some(bound) = self.conns.get_mut(&conn).filter(|bound| bound.leaving) else {
            return;
        };
        bound.leaving = false;
        if let Some(entry) = self
            .registry
            .sessions
            .get_mut(session)
            .filter(|entry| entry.restart_interrupted)
        {
            entry.restart_interrupted = false;
            self.registry.dirty = true;
        }
        if self.registry.agent_connected(session, conn) {
            info!(conn, session = short(session), "leaving agent stays");
        }
    }

    /// A restart of `session` now cuts off work: a turn or a call runs
    /// (stopped by Esc or not), or ⏹ went in while the update press waited.
    fn restart_cuts_work(&self, session: &str) -> bool {
        self.updates.get(session).is_some_and(|ask| ask.interrupted)
            || self.activity.get(session).is_some_and(Activity::working)
    }

    /// Sessions a client restart cut off get one channel message once the
    /// agent of their next run is bound (TASK-047).
    fn send_continuations(&mut self) {
        let waiting: Vec<String> = self
            .registry
            .sessions
            .iter()
            .filter(|(_, entry)| entry.restart_interrupted)
            .map(|(session, _)| session.clone())
            .collect();
        for session in waiting {
            let Some(slot) = self.current_slot(&session) else {
                continue;
            };
            let Some((_, conn)) = self.live_agent(slot).filter(|(live, _)| *live == session) else {
                continue;
            };
            let mut meta =
                BTreeMap::from([("chat_id".to_owned(), self.options.chat_id.to_string())]);
            if let Some(thread_id) = self.registry.slot(slot).and_then(|slot| slot.topic_id) {
                meta.insert("thread_id".to_owned(), thread_id.to_string());
            }
            let inbound = HubMsg::Inbound {
                content: format!("{CONTINUE_TEXT} {CONTINUE_AGENTS_TEXT}"),
                meta,
            };
            let sent = self
                .conns
                .get(&conn)
                .is_some_and(|bound| bound.to_agent.try_send(inbound).is_ok());
            if !sent {
                continue;
            }
            if let Some(entry) = self.registry.sessions.get_mut(&session) {
                entry.restart_interrupted = false;
                self.registry.dirty = true;
            }
            info!(
                conn,
                session = short(&session),
                "session told to go on after the restart"
            );
        }
    }

    /// The session works: a turn or a call runs and no Esc went in yet.
    fn busy(&self, session: &str) -> bool {''')

rep('''        if written {
            info!(conn, session = short(&ask.session), "Esc written");
            if let Some(activity) = self.activity.get_mut(&ask.session) {
                activity.interrupt_written();
            }''', '''        if written {
            info!(conn, session = short(&ask.session), "Esc written");
            if let Some(activity) = self.activity.get_mut(&ask.session) {
                activity.interrupt_written();
            }
            if let Some(update) = self.updates.get_mut(&ask.session) {
                update.interrupted = true;
            }''')

rep('''        self.check_hook_asks();
        self.flush_all();''', '''        self.check_hook_asks();
        self.send_continuations();
        self.flush_all();''')

rep('''        let deadline = status.fold(deadline, Instant::min);
        let deadline = self
            .reads''', '''        let deadline = status.fold(deadline, Instant::min);
        let deadline = self
            .updates
            .values()
            .filter_map(|ask| ask.retry_at)
            .filter(|at| *at > now)
            .fold(deadline, Instant::min);
        let deadline = self
            .reads''')
open(p, 'w', encoding='utf-8', newline='').write(s)
print('ok')
