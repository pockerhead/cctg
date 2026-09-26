"""One-off edit of crates/cctg/src/hub/slots.rs for TASK-053 (compaction)."""
p = 'crates/cctg/src/hub/slots.rs'
s = open(p, encoding='utf-8').read()


def rep(a, b):
    global s
    assert s.count(a) == 1, a
    s = s.replace(a, b)


rep('''//! the prompt), and a written Esc is shown as sent, not as the turn's end.
''', '''//! the prompt), and a written Esc is shown as sent, not as the turn's end.
//! A compaction (TASK-053: the `PreCompact` hook, ended by the session's
//! `SessionStart` with `source: compact`) shows in the status message with
//! its minutes and gives the topic two silent lines: one when it begins, one
//! when it is done, with the context percentages before and after when the
//! status line sends the new one within [`COMPACT_NUMBERS_WAIT`]. One that
//! does not end (the session ends, [`COMPACT_MAX`] passes) leaves the status
//! with no line.
''')
rep('''/// Agents one update press asks at most:''', '''/// A compaction that has not ended after this is forgotten (TASK-053).
pub const COMPACT_MAX: Duration = Duration::from_secs(15 * 60);
/// The line of an ended compaction waits this long for the status line's
/// new context percentage, then goes without the percentages.
pub const COMPACT_NUMBERS_WAIT: Duration = Duration::from_secs(10);
/// Agents one update press asks at most:''')
rep('''    /// What live top-level sessions do, for their status messages.
    activity: HashMap<String, Activity>,
''', '''    /// What live top-level sessions do, for their status messages.
    activity: HashMap<String, Activity>,
    /// Compactions of live top-level sessions, running or ended and waiting
    /// for their numbers.
    compactions: HashMap<String, Compaction>,
''')
rep('''            activity: HashMap::new(),
            shown: HashMap::new(),''', '''            activity: HashMap::new(),
            compactions: HashMap::new(),
            shown: HashMap::new(),''')
rep('''            HookEvent::SubagentHandback { agent_id, message }
                if subagents::is_agent_id(agent_id) =>
            {
                self.reports.insert(agent_id.clone(), message.clone());
            }
            _ => {}
        }''', '''            HookEvent::SubagentHandback { agent_id, message }
                if subagents::is_agent_id(agent_id) =>
            {
                self.reports.insert(agent_id.clone(), message.clone());
            }
            HookEvent::PreCompact { trigger } => self.compact_started(session, trigger.as_deref()),
            HookEvent::SessionStart {
                source: Some(source),
                ..
            } if source == "compact" => self.compact_ended(session),
            HookEvent::StatusLine { context, .. } => self.compact_numbers(session, *context),
            _ => {}
        }''')
rep('''        self.activity
            .retain(|session, _| registry.is_live_top_level(session));
        self.started_agents''', '''        self.activity
            .retain(|session, _| registry.is_live_top_level(session));
        self.compactions
            .retain(|session, _| registry.is_live_top_level(session));
        self.started_agents''')
rep('''    /// What a live top-level session does, for its status message. Events of''', '''    /// `PreCompact` of the live current session of a slot with a topic: the
    /// status says so and the topic gets one line. A repeat while one runs
    /// changes nothing.
    fn compact_started(&mut self, session: &str, trigger: Option<&str>) {
        if self
            .compactions
            .get(session)
            .is_some_and(|compaction| compaction.done.is_none())
        {
            return;
        }
        let Some(slot) = self.current_slot(session) else {
            debug!(
                session = short(session),
                "compaction of a session that is not the live one of its slot; not shown"
            );
            return;
        };
        let Some(thread_id) = self.registry.slot(slot).and_then(|slot| slot.topic_id) else {
            return;
        };
        let auto = match trigger {
            Some("auto") => Some(true),
            Some("manual") => Some(false),
            _ => None,
        };
        let before = self
            .registry
            .sessions
            .get(session)
            .and_then(|entry| entry.metrics.as_ref())
            .and_then(|metrics| metrics.context);
        info!(session = short(session), ?auto, "compaction started");
        self.compactions.insert(
            session.to_owned(),
            Compaction {
                auto,
                started: Instant::now(),
                before,
                done: None,
            },
        );
        self.send_messages(vec![message_op(thread_id, status::compacting_line(auto))]);
    }

    /// `SessionStart` with `source: compact`: the compaction is done. Its
    /// line waits for the new context percentage when the session has a
    /// status line at all.
    fn compact_ended(&mut self, session: &str) {
        let has_numbers = self
            .registry
            .sessions
            .get(session)
            .is_some_and(|entry| entry.metrics.is_some());
        let now = Instant::now();
        let Some(compaction) = self
            .compactions
            .get_mut(session)
            .filter(|compaction| compaction.done.is_none())
        else {
            return;
        };
        let took = now.saturating_duration_since(compaction.started);
        info!(
            session = short(session),
            took_s = took.as_secs(),
            "compaction ended"
        );
        compaction.done = Some((took, now + COMPACT_NUMBERS_WAIT));
        if !has_numbers {
            self.compact_told(session, None);
        }
    }

    /// A status line: the percentage after an ended compaction, once it
    /// differs from the one before (a line sent before the end can still
    /// arrive after it).
    fn compact_numbers(&mut self, session: &str, context: Option<u32>) {
        let Some(context) = context else {
            return;
        };
        if self.compactions.get(session).is_some_and(|compaction| {
            compaction.done.is_some() && compaction.before != Some(context)
        }) {
            self.compact_told(session, Some(context));
        }
    }

    /// Sends the line of the ended compaction of `session` and forgets it.
    fn compact_told(&mut self, session: &str, after: Option<u32>) {
        let Some(compaction) = self.compactions.remove(session) else {
            return;
        };
        let Some((took, _)) = compaction.done else {
            return;
        };
        let Some(thread_id) = self
            .current_slot(session)
            .and_then(|slot| self.registry.slot(slot))
            .and_then(|slot| slot.topic_id)
        else {
            return;
        };
        let line = status::compacted_line(took, compaction.before, after);
        self.send_messages(vec![message_op(thread_id, line)]);
    }

    /// Compactions that never ended are forgotten; ended ones whose numbers
    /// did not come are told without them.
    fn check_compactions(&mut self, now: Instant) {
        self.compactions.retain(|session, compaction| {
            let lost = compaction.done.is_none() && now >= compaction.started + COMPACT_MAX;
            if lost {
                info!(
                    session = short(session),
                    "compaction did not end in time; forgotten"
                );
            }
            !lost
        });
        let late: Vec<String> = self
            .compactions
            .iter()
            .filter(|(_, compaction)| compaction.done.is_some_and(|(_, until)| now >= until))
            .map(|(session, _)| session.clone())
            .collect();
        for session in late {
            self.compact_told(&session, None);
        }
    }

    /// When the compactions need the actor next: the wait for numbers, the
    /// limit, and the next whole minute of a running one (its status).
    fn compaction_deadlines(&self, now: Instant) -> Vec<Instant> {
        self.compactions
            .values()
            .map(|compaction| match compaction.done {
                Some((_, until)) => until,
                None => {
                    let minutes = now.saturating_duration_since(compaction.started).as_secs() / 60;
                    let next_minute = compaction.started + Duration::from_secs((minutes + 1) * 60);
                    next_minute.min(compaction.started + COMPACT_MAX)
                }
            })
            .collect()
    }

    /// What a live top-level session does, for its status message. Events of''')
rep('''        let phase = status::phase(activity, ended, waiting);
        let keys = !ended''', '''        let compacting = self
            .compactions
            .get(session)
            .filter(|compaction| compaction.done.is_none());
        let phase = match compacting {
            Some(compaction) if !ended && !waiting => status::Phase::Compacting {
                auto: compaction.auto,
                minutes: now.saturating_duration_since(compaction.started).as_secs() / 60,
            },
            _ => status::phase(activity, ended, waiting),
        };
        let keys = !ended''')
rep('''        self.check_candidates();
        self.retry_bodies();
    }''', '''        self.check_candidates();
        self.retry_bodies();
        self.check_compactions(now);
    }''')
rep('''        let deadline = status.fold(deadline, Instant::min);
''', '''        let deadline = status.fold(deadline, Instant::min);
        let deadline = self
            .compaction_deadlines(now)
            .into_iter()
            .filter(|at| *at > now)
            .fold(deadline, Instant::min);
''')
rep('''/// A plain message without a sound.
fn message_op(''', '''/// A compaction of a live top-level session (TASK-053).
struct Compaction {
    /// `Some(true)`: auto, `Some(false)`: `/compact`, `None`: not known.
    auto: Option<bool>,
    started: Instant,
    /// The context percentage of the status line when it began.
    before: Option<u32>,
    /// Ended: how long it took, and until when its line waits for the new
    /// percentage.
    done: Option<(Duration, Instant)>,
}

/// A plain message without a sound.
fn message_op(''')
open(p, 'w', encoding='utf-8', newline='').write(s)
