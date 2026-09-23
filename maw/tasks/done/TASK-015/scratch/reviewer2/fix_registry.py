# -*- coding: utf-8 -*-
# Reviewer-2 fixes in hub/registry.rs of ws/ (each replacement must match once).
import io, os
here = os.path.dirname(os.path.abspath(__file__))
p = os.path.join(here, 'ws', 'crates', 'cctg', 'src', 'hub', 'registry.rs')
s = io.open(p, encoding='utf-8', newline='').read()


def sub(old, new):
    global s
    assert s.count(old) == 1, old[:80]
    s = s.replace(old, new)


# Bound on durable subagent records.
sub('''/// Tries of one block send or edit before it is given up.
pub const MAX_BLOCK_ATTEMPTS: u32 = 5;
''', '''/// Tries of one block send or edit before it is given up.
pub const MAX_BLOCK_ATTEMPTS: u32 = 5;
/// Subagent records kept; beyond this the oldest settled one is forgotten
/// (a reply to its block then carries no `target_agent`).
pub const MAX_SUBAGENTS: usize = 1024;
''')

# SubagentEntry.seen orders the eviction.
sub('''    #[serde(default)]
    pub slot: Option<SlotId>,
    #[serde(default)]
    pub block: Block,
}
''', '''    #[serde(default)]
    pub slot: Option<SlotId>,
    #[serde(default)]
    pub block: Block,
    /// Registry sequence number of the confirmation; orders eviction.
    #[serde(default)]
    pub seen: u64,
}
''')

# Nested answer persisted in the block.
sub('''    #[serde(default)]
    pub sending: bool,
    #[serde(skip)]
    pub busy: bool,
''', '''    #[serde(default)]
    pub sending: bool,
    /// A nested run's last turn answer, shown when the run ends.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub answer: Option<String>,
    #[serde(skip)]
    pub busy: bool,
''')

# confirm_subagent: bounded.
sub('''    pub fn confirm_subagent(&mut self, agent_id: &str, parent: &str, header: String) -> bool {
        if self.subagents.contains_key(agent_id) {
            return false;
        }
        let Some(slot) = self
            .sessions
            .get(parent)
            .filter(|entry| entry.kind == SessionKind::TopLevel)
            .and_then(|entry| entry.slot)
        else {
            return false;
        };
        self.subagents.insert(
            agent_id.to_owned(),
            SubagentEntry {
                parent_session: parent.to_owned(),
                slot: Some(slot),
                block: Block::running(header),
            },
        );
        self.dirty = true;
        true
    }
''', '''    pub fn confirm_subagent(&mut self, agent_id: &str, parent: &str, header: String) -> bool {
        if self.subagents.contains_key(agent_id) {
            return false;
        }
        let Some(slot) = self
            .sessions
            .get(parent)
            .filter(|entry| entry.kind == SessionKind::TopLevel)
            .and_then(|entry| entry.slot)
        else {
            return false;
        };
        if self.subagents.len() >= MAX_SUBAGENTS {
            // The oldest block with nothing left to show goes; when every
            // block still works or waits for Telegram, the new one is refused.
            let settled = self
                .subagents
                .iter()
                .filter(|(_, entry)| {
                    !entry.block.running && entry.block.pending.is_none() && !entry.block.busy
                })
                .min_by_key(|(_, entry)| entry.seen)
                .map(|(id, _)| id.clone());
            let Some(oldest) = settled else {
                return false;
            };
            self.subagents.remove(&oldest);
        }
        let seen = self.touch();
        self.subagents.insert(
            agent_id.to_owned(),
            SubagentEntry {
                parent_session: parent.to_owned(),
                slot: Some(slot),
                block: Block::running(header),
                seen,
            },
        );
        true
    }

    /// Keeps a nested run's last answer for its end. `false`: not a nested
    /// run with a block.
    pub fn set_nested_answer(&mut self, session: &str, answer: &str) -> bool {
        let Some(block) = self
            .sessions
            .get_mut(session)
            .filter(|entry| matches!(entry.kind, SessionKind::Nested { .. }))
            .and_then(|entry| entry.block.as_mut())
        else {
            return false;
        };
        block.answer = Some(answer.to_owned());
        self.dirty = true;
        true
    }

    /// Takes the answer kept by [`Registry::set_nested_answer`].
    pub fn take_nested_answer(&mut self, session: &str) -> Option<String> {
        let answer = self.sessions.get_mut(session)?.block.as_mut()?.answer.take();
        if answer.is_some() {
            self.dirty = true;
        }
        answer
    }
''')

# lose_blocks: a lost nested run drops its kept answer.
sub('''        for key in lost {
            if let Some(header) = self.block(&key).map(|block| block.header.clone()) {
                self.show_block(&key, format!("{header}\\n{BLOCK_LOST}"), false);
            }
        }
''', '''        for key in lost {
            if let Some(header) = self.block(&key).map(|block| block.header.clone()) {
                self.show_block(&key, format!("{header}\\n{BLOCK_LOST}"), false);
            }
            if let BlockKey::Nested(session) = &key {
                self.take_nested_answer(session);
            }
        }
''')

# block_work: bounded.
sub('''    /// Block sends and edits that are due. A first send needs the slot's
    /// topic; a block whose first send was cut off by a restart is given up.
    pub fn block_work(&mut self) -> Vec<BlockJob> {''', '''    /// At most `limit` block sends and edits that are due; the rest wait in
    /// the registry. A first send needs the slot's topic; a block whose first
    /// send may have reached Telegram (`sending` without an answer, e.g. cut
    /// off by a restart) is never sent again: its text is dropped.
    pub fn block_work(&mut self, limit: usize) -> Vec<BlockJob> {''')
sub('''        let mut jobs = Vec::new();
        for key in keys {
            let topic = self
                .block_slot(&key)''', '''        let mut jobs = Vec::new();
        for key in keys {
            if jobs.len() >= limit {
                break;
            }
            let topic = self
                .block_slot(&key)''')

# block_send_unclear: the at-most-once tombstone for an in-process send.
sub('''    /// The agent id of the subagent block `message_id` in `thread_id`, when''', '''    /// A first send got no clear answer: Telegram may show it, so it is
    /// never sent again (at most once); the block keeps no text to show.
    pub fn block_send_unclear(&mut self, key: &BlockKey) {
        if let Some(block) = self.block_mut(key) {
            block.busy = false;
            block.pending = None;
            block.running = false;
            self.dirty = true;
        }
    }

    /// The agent id of the subagent block `message_id` in `thread_id`, when''')

# Legacy records on load.
sub('''            || duplicate_slot
        {
            return Err(LoadError::Invalid);
        }
        registry.after_restart();
        Ok(registry)''', '''            || duplicate_slot
        {
            return Err(LoadError::Invalid);
        }
        // Records written before subagents were matched to `Agent` calls have
        // no block header; they may be Claude Code's internal agents.
        registry
            .subagents
            .retain(|_, agent| !agent.block.header.is_empty());
        registry.after_restart();
        Ok(registry)''')

io.open(p, 'w', encoding='utf-8', newline='\n').write(s)
print('ok')
