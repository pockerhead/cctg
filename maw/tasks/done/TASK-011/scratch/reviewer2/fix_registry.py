# Applies the reviewer-2 registry fixes to reviewer2/ws (idempotence not needed: run once).
import os
HERE = os.path.dirname(os.path.abspath(__file__))
p = os.path.join(HERE, 'ws', 'crates', 'cctg', 'src', 'hub', 'registry.rs')
s = open(p, encoding='utf-8').read()


def rep(old, new):
    global s
    assert s.count(old) == 1, old[:100]
    s = s.replace(old, new, 1)


rep('use std::collections::{BTreeMap, HashSet};', 'use std::collections::{BTreeMap, BTreeSet, HashSet};')
rep('''/// Default icons, from `getForumTopicIconStickers` (2026-09-23, 112 stickers).''',
    '''/// Preferred icons, from `getForumTopicIconStickers` (2026-09-23, 112
/// stickers). Used only while Telegram offers them, see [`Icons::from_offered`].''')
rep('''    /// Drops every id Telegram does not offer; returns the names of the
    /// dropped states.
    pub fn keep_valid(&mut self, valid: &HashSet<String>) -> Vec<&'static str> {
        let mut dropped = Vec::new();
        for (name, icon) in [
            ("alive", &mut self.alive),
            ("dead", &mut self.dead),
            ("waiting", &mut self.waiting),
            ("no_channel", &mut self.no_channel),
        ] {
            if icon.as_ref().is_some_and(|id| !valid.contains(id)) {
                *icon = None;
                dropped.push(name);
            }
        }
        dropped
    }
}''', '''    /// Icons from the ids `getForumTopicIconStickers` offers: the preferred
    /// id of a state when it is offered, otherwise the smallest offered id
    /// that no state prefers. Returns the names of the substituted states.
    /// Fewer than four usable ids is an error: an id Telegram does not offer
    /// is never sent.
    pub fn from_offered(
        offered: impl IntoIterator<Item = String>,
    ) -> Result<(Self, Vec<&'static str>), IconError> {
        let offered: BTreeSet<String> = offered.into_iter().filter(|id| !id.is_empty()).collect();
        let preferred = [
            ("alive", ICON_ALIVE),
            ("dead", ICON_DEAD),
            ("waiting", ICON_WAITING),
            ("no_channel", ICON_NO_CHANNEL),
        ];
        let too_few = IconError::TooFew(offered.len());
        let mut spare = offered
            .iter()
            .filter(|id| !preferred.iter().any(|(_, wanted)| wanted == id));
        let mut chosen = Vec::new();
        let mut substituted = Vec::new();
        for (state, wanted) in preferred {
            if offered.contains(wanted) {
                chosen.push(Some(wanted.to_owned()));
            } else {
                let id = spare.next().ok_or_else(|| too_few.clone())?;
                chosen.push(Some(id.clone()));
                substituted.push(state);
            }
        }
        let [alive, dead, waiting, no_channel] =
            <[Option<String>; 4]>::try_from(chosen).map_err(|_| too_few)?;
        let icons = Self {
            alive,
            dead,
            waiting,
            no_channel,
        };
        Ok((icons, substituted))
    }
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum IconError {
    #[error("getForumTopicIconStickers offered {0} icons; four distinct usable ones are needed")]
    TooFew(usize),
}''')

rep('''/// A topic call the actor should make. The slot is marked busy until the
/// matching `topic_*` result comes back.''', '''/// A topic call the actor should make. At most one per slot is in flight: the
/// slot is marked busy until the matching `topic_*` result comes back.''')

rep('''    /// Nesting from the pid the device reports as the next claude ancestor.
    fn nesting(&self, host: &str, session: &str, parent_pid: Option<u32>) -> SessionKind {
        let Some(pid) = parent_pid else {
            return SessionKind::TopLevel;
        };
        match self.pids.get(&pid_key(host, pid)) {
            // A parent that is the session itself is a stale CLAUDE_PID on the
            // device, not nesting.
            Some(parent) if parent == session => SessionKind::TopLevel,
            Some(parent) => SessionKind::Nested {
                parent: Some(parent.clone()),
            },
            None => SessionKind::Nested { parent: None },
        }
    }''', '''    /// Nesting from the pid the device reports as the next claude ancestor.
    /// An ancestor the registry does not know is "nested, parent unknown".
    fn nesting(&self, host: &str, parent_pid: Option<u32>) -> SessionKind {
        let Some(pid) = parent_pid else {
            return SessionKind::TopLevel;
        };
        SessionKind::Nested {
            parent: self.pids.get(&pid_key(host, pid)).cloned(),
        }
    }''')
rep('''        let session = post.session_id.as_str();
        let kind = match self.sessions.get(session) {
            // A known session keeps what it was (a resume has no ancestor info
            // worth more than the first start).
            Some(entry) if parent_pid.is_none() => entry.kind.clone(),
            _ => self.nesting(&post.host, session, parent_pid),
        };''', '''        let session = post.session_id.as_str();
        if parent_pid.is_some_and(|pid| {
            self.pids.get(&pid_key(&post.host, pid)).map(String::as_str) == Some(session)
        }) {
            // The ancestor is this very session: a stale CLAUDE_PID on the
            // device, or a nested resume of the parent's own id. It proves
            // nothing about top-level: nested with an unknown parent, and the
            // session's existing record stays as it is.
            return SlotOrParent::Parent(None);
        }
        let kind = match self.sessions.get(session) {
            // A known session keeps what it was (a resume has no ancestor info
            // worth more than the first start).
            Some(entry) if parent_pid.is_none() => entry.kind.clone(),
            _ => self.nesting(&post.host, parent_pid),
        };''')
rep('''            HookEvent::UserPromptSubmit { .. } | HookEvent::Stop { .. } => {
                if !self.sessions.contains_key(session) {
                    // Its SessionStart was lost (hub down at the time).
                    self.session_started(post, None, None, None);
                }
                let entry = self.sessions.get_mut(session).expect("registered above");''',
    '''            HookEvent::UserPromptSubmit { .. } | HookEvent::Stop { .. } => {
                // Only a SessionStart tells top-level from nested: a session
                // whose start was never seen gets nothing until its next one.
                let Some(entry) = self.sessions.get_mut(session) else {
                    return Followup::default();
                };''')
rep('''    /// An agent registered. `false`: the session is unknown; the caller waits
    /// for its hook for a while and then calls [`Registry::adopt`].''',
    '''    /// An agent registered. `false`: the session is unknown; the caller keeps
    /// the agent waiting for its SessionStart and never adopts it (without a
    /// SessionStart a nested run cannot be told from a top-level one).''')
rep('''    /// A session known only from its agent: taken as top-level.
    pub fn adopt(&mut self, session: &str, host: &str, cwd: &str) -> SlotId {
        let post = HookPost::new(
            host.to_owned(),
            session.to_owned(),
            cwd.to_owned(),
            String::new(),
            HookEvent::SessionStart {
                source: None,
                claude_pid: None,
                parent_claude_pid: None,
            },
        );
        match self.session_started(&post, None, None, None) {
            SlotOrParent::Own(id) => id,
            SlotOrParent::Parent(_) => unreachable!("no ancestor info means top-level"),
        }
    }

''', '')
rep('''            if let Some(text) = slot.pending_separator.take() {
                self.dirty = true;
                jobs.push(TopicJob::Separator {
                    slot: id,
                    thread_id,
                    text,
                });
            }''', '''            // One call per slot at a time; the separator stays pending until
            // Telegram took it.
            if let Some(text) = slot.pending_separator.clone() {
                slot.busy = true;
                jobs.push(TopicJob::Separator {
                    slot: id,
                    thread_id,
                    text,
                });
                continue;
            }''')
rep('''    /// Records a successful (or not-modified) edit of `thread_id`.
    pub fn topic_edited(
        &mut self,
        id: SlotId,
        thread_id: i64,
        name: Option<&str>,
        icon: Option<&str>,
    ) {
        let slot = &mut self.slots[id.0];
        slot.busy = false;
        if slot.topic_id != Some(thread_id) {
            return;
        }
        slot.failed = None;''', '''    /// Records a successful (or not-modified) edit of `thread_id`. A result
    /// for a topic the slot no longer has is stale and changes nothing.
    pub fn topic_edited(
        &mut self,
        id: SlotId,
        thread_id: i64,
        name: Option<&str>,
        icon: Option<&str>,
    ) {
        let slot = &mut self.slots[id.0];
        if slot.topic_id != Some(thread_id) {
            return;
        }
        slot.busy = false;
        slot.failed = None;''')
rep('''    /// A create or edit failed for another reason than a gone topic: the
    /// current name and icon are not tried again until they change or
    /// [`Registry::retry_failed`] runs.''', '''    /// The separator `text` reached `thread_id`. A stale result changes nothing.
    pub fn topic_separated(&mut self, id: SlotId, thread_id: i64, text: &str) {
        let slot = &mut self.slots[id.0];
        if slot.topic_id != Some(thread_id) {
            return;
        }
        slot.busy = false;
        slot.failed = None;
        if slot.pending_separator.as_deref() == Some(text) {
            slot.pending_separator = None;
            self.dirty = true;
        }
    }

    /// A create, edit or separator failed for another reason than a gone
    /// topic: the slot's work is not tried again until its name or icon
    /// changes or [`Registry::retry_failed`] runs.''')
rep('''    /// Telegram says `thread_id` is gone. Only the slot still bound to that
    /// topic forgets it, once; the next [`Registry::topic_work`] creates
    /// exactly one replacement.
    pub fn topic_invalid(&mut self, id: SlotId, thread_id: i64) {
        let slot = &mut self.slots[id.0];
        slot.busy = false;
        if slot.topic_id == Some(thread_id) {
            slot.topic_id = None;''', '''    /// Telegram says `thread_id` is gone. Only the slot still bound to that
    /// topic forgets it, once; the next [`Registry::topic_work`] creates
    /// exactly one replacement. A late report of an old topic changes
    /// nothing, not even `busy`: the replacement may be in flight.
    pub fn topic_invalid(&mut self, id: SlotId, thread_id: i64) {
        let slot = &mut self.slots[id.0];
        if slot.topic_id == Some(thread_id) {
            slot.busy = false;
            slot.topic_id = None;''')
rep('''    /// Releases a slot whose edit came back without a usable answer, keeping
    /// what is applied.''', '''    /// Releases a slot whose call came back without an answer (the scheduler
    /// stopped), keeping what is applied and what is pending.''')
open(p, 'w', encoding='utf-8', newline='\n').write(s)
print('ok')
