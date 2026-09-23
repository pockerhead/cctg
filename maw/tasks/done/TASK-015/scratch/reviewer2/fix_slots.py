# -*- coding: utf-8 -*-
# Reviewer-2 fixes in hub/slots.rs and hub/subagents.rs of ws/.
import io, os
here = os.path.dirname(os.path.abspath(__file__))
hub = os.path.join(here, 'ws', 'crates', 'cctg', 'src', 'hub')
s = ''


def sub(old, new, count=1):
    global s
    assert s.count(old) == count, old[:100]
    s = s.replace(old, new)


# ---------------------------------------------------------------- slots.rs
p = os.path.join(hub, 'slots.rs')
s = io.open(p, encoding='utf-8', newline='').read()

sub('''pub const MAX_QUEUED_MESSAGES: usize = 256;
''', '''pub const MAX_QUEUED_MESSAGES: usize = 256;
/// Block sends and edits handed to the dispatch task and not answered yet;
/// the rest wait in the registry.
pub const MAX_BLOCK_JOBS: usize = 16;
/// Subagent files read at a time for block texts (each up to 64 MiB).
const MAX_BODY_READS: usize = 2;
''')

sub('''    /// The finished text of a subagent block.
    Body {
        key: BlockKey,
        text: String,
    },''', '''    /// The finished text of a subagent block.
    Body {
        agent_id: String,
        text: String,
    },''')

sub('''    reports: Reports,
    /// Last turn answer of each nested run with a block, for its end.
    nested_answers: HashMap<String, String>,
    grace_until: Instant,''', '''    reports: Reports,
    /// Block texts to read from subagent files, the newest stop per agent.
    bodies_waiting: BTreeMap<String, BodyInput>,
    /// Agents whose files are being read, at most [`MAX_BODY_READS`].
    bodies_reading: HashSet<String>,
    /// Block jobs handed out and not answered, at most [`MAX_BLOCK_JOBS`].
    block_jobs: usize,
    grace_until: Instant,''')

sub('''            reports: Reports::default(),
            nested_answers: HashMap::new(),
            grace_until: now + options.grace,''', '''            reports: Reports::default(),
            bodies_waiting: BTreeMap::new(),
            bodies_reading: HashSet::new(),
            block_jobs: 0,
            grace_until: now + options.grace,''')

sub('''        {
            if self.has_nested_block(session) {
                self.nested_answers
                    .insert(session.to_owned(), answer.clone());
            }
            self.on_turn_answer(session, answer);
        }''', '''        {
            // Kept in the registry, so a restart before the run ends keeps it.
            self.registry.set_nested_answer(session, answer);
            self.on_turn_answer(session, answer);
        }''')

sub('''    fn has_nested_block(&self, session: &str) -> bool {
        self.registry.sessions.get(session).is_some_and(|entry| {
            matches!(entry.kind, SessionKind::Nested { .. }) && entry.block.is_some()
        })
    }

''', '')

sub('''        for session in ended {
            let answer = self.nested_answers.remove(session);
            let key = BlockKey::Nested(session.clone());''', '''        for session in ended {
            let answer = self.registry.take_nested_answer(session);
            let key = BlockKey::Nested(session.clone());''')

sub('''        info!(
            agent = short(agent_id),
            session = short(&candidate.session),
            "subagent block opened"
        );
        if let Some(stop) = candidate.stop {
            self.read_body(agent_id, stop);
        }
    }''', '''        info!(
            agent = short(agent_id),
            session = short(&candidate.session),
            "subagent block opened"
        );
        match candidate.stop {
            Some(stop) => self.read_body(agent_id, stop),
            // Matched only after its session ended: no stop will finish it.
            None if self
                .registry
                .sessions
                .get(&candidate.session)
                .is_some_and(|entry| entry.ended) =>
            {
                self.registry.lose_blocks(&[candidate.session]);
            }
            None => {}
        }
    }''')

sub('''    /// Reads the finished subagent's files off the actor; the text comes
    /// back as [`Done::Body`].
    fn read_body(&mut self, agent_id: &str, stop: Stopped) {''', '''    /// Reads the finished subagent's files off the actor; the text comes
    /// back as [`Done::Body`]. A newer stop of the same agent replaces one
    /// still waiting; see [`Self::start_body_reads`].
    fn read_body(&mut self, agent_id: &str, stop: Stopped) {''')

sub('''            report: self.reports.take(agent_id),
            last: stop.last,
        };
        let key = BlockKey::Agent(agent_id.to_owned());
        let done = self.done_tx.clone();
        tokio::spawn(async move {
            let text = tokio::task::spawn_blocking(move || subagents::read_body(&input))
                .await
                .unwrap_or_default();
            let _ = done.send(Done::Body { key, text });
        });
    }''', '''            report: self.reports.take(agent_id),
            last: stop.last,
        };
        self.bodies_waiting.insert(agent_id.to_owned(), input);
        self.start_body_reads();
    }

    /// Starts waiting reads, at most [`MAX_BODY_READS`] at a time and one per
    /// agent. A read that ends while a newer stop of its agent waits is
    /// stale and dropped ([`Done::Body`]), so the newest stop always wins.
    fn start_body_reads(&mut self) {
        while self.bodies_reading.len() < MAX_BODY_READS {
            let Some(agent_id) = self
                .bodies_waiting
                .keys()
                .find(|agent_id| !self.bodies_reading.contains(*agent_id))
                .cloned()
            else {
                return;
            };
            let Some(input) = self.bodies_waiting.remove(&agent_id) else {
                return;
            };
            self.bodies_reading.insert(agent_id.clone());
            let done = self.done_tx.clone();
            tokio::spawn(async move {
                let text = tokio::task::spawn_blocking(move || subagents::read_body(&input))
                    .await
                    .unwrap_or_default();
                let _ = done.send(Done::Body { agent_id, text });
            });
        }
    }''')

sub('''            Done::Body { key, text } => {
                if !text.is_empty() {
                    self.finish_block(key, text);
                }
            }''', '''            Done::Body { agent_id, text } => {
                self.bodies_reading.remove(&agent_id);
                // A newer stop of this agent waits: this text is stale.
                if !text.is_empty() && !self.bodies_waiting.contains_key(&agent_id) {
                    self.finish_block(BlockKey::Agent(agent_id), text);
                }
                self.start_body_reads();
            }''')

sub('''    fn on_block_done(&mut self, job: BlockJob, delivery: Option<Delivery>) {
        let (key, text, message_id) = match (&job, &delivery) {''', '''    fn on_block_done(&mut self, job: BlockJob, delivery: Option<Delivery>) {
        self.block_jobs = self.block_jobs.saturating_sub(1);
        let (key, text, message_id) = match (&job, &delivery) {''')

sub('''            (BlockJob::Send { key, .. } | BlockJob::Edit { key, .. }, _) => {
                if self.registry.block_failed(key) {''', '''            // Only a refusal proves Telegram has no such message; anything
            // else may have been shown, and a first send goes at most once.
            (BlockJob::Send { key, .. }, delivery) if !send_refused(delivery.as_ref()) => {
                warn!("block message may have been sent without an answer; not sent again");
                self.registry.block_send_unclear(key);
                return;
            }
            (BlockJob::Send { key, .. } | BlockJob::Edit { key, .. }, _) => {
                if self.registry.block_failed(key) {''')

sub('''        for job in self.registry.block_work() {
            let op = match &job {''', '''        let room = MAX_BLOCK_JOBS.saturating_sub(self.block_jobs);
        for job in self.registry.block_work(room) {
            self.block_jobs += 1;
            let op = match &job {''')

sub('''/// The topic was deleted in Telegram.
fn topic_gone(delivery: &Delivery) -> bool {''', '''/// Telegram refused a send, or it never left this machine: no message exists.
fn send_refused(delivery: Option<&Delivery>) -> bool {
    match delivery {
        Some(Err(ApiError::Telegram { .. })) => true,
        Some(Err(ApiError::Http(error))) => error.is_connect(),
        _ => false,
    }
}

/// The topic was deleted in Telegram.
fn topic_gone(delivery: &Delivery) -> bool {''')

# tests: the registry API change
sub('''        for (job, message) in registry.block_work().into_iter().zip([700, 701]) {''',
    '''        for (job, message) in registry
            .block_work(usize::MAX)
            .into_iter()
            .zip([700, 701])
        {''')
sub('''            assert_eq!(registry.block_work().len(), 1);''',
    '''            assert_eq!(registry.block_work(usize::MAX).len(), 1);''')
sub('''        assert_eq!(busy, 16);''', '''        assert_eq!(busy, MAX_BLOCK_JOBS);''')

io.open(p, 'w', encoding='utf-8', newline='\n').write(s)

# ------------------------------------------------------------ subagents.rs
p = os.path.join(hub, 'subagents.rs')
s = io.open(p, encoding='utf-8', newline='').read()

sub('''/// Handed-back reports kept until their subagent stops.
pub const MAX_REPORTS: usize = 256;
''', '''/// Handed-back reports kept until their subagent stops.
pub const MAX_REPORTS: usize = 256;
/// `Agent` calls and results one session index keeps; the oldest go first.
pub const MAX_INDEX_ENTRIES: usize = 1024;
/// A call's `subagent_type` and `description` are kept up to this many
/// UTF-16 units (the header shows 120 characters).
const MAX_CALL_FIELD: usize = 256;
''')

sub('''                        let field =
                            |key: &str| input.get(key).and_then(Value::as_str).map(str::to_owned);''',
    '''                        let field = |key: &str| {
                            input
                                .get(key)
                                .and_then(Value::as_str)
                                .map(|text| cut(text, MAX_CALL_FIELD))
                        };''')

sub('''/// The `Agent` calls of one session's transcript found so far.
#[derive(Debug, Default)]
pub struct AgentIndex {
    path: String,
    offset: u64,
    calls: HashMap<String, AgentCall>,
    links: HashMap<String, String>,
}''', '''/// The `Agent` calls of one session's transcript found so far, at most
/// [`MAX_INDEX_ENTRIES`] calls and as many results.
#[derive(Debug, Default)]
pub struct AgentIndex {
    path: String,
    offset: u64,
    calls: HashMap<String, AgentCall>,
    links: HashMap<String, String>,
    /// Insertion order of `calls` and `links`, for eviction.
    call_order: VecDeque<String>,
    link_order: VecDeque<String>,
}''')

sub('''        self.offset = scan.offset;
        self.calls.extend(scan.calls);
        self.links.extend(scan.links);
    }''', '''        self.offset = scan.offset;
        for (id, call) in scan.calls {
            if self.calls.insert(id.clone(), call).is_none() {
                self.call_order.push_back(id);
            }
        }
        for (agent_id, id) in scan.links {
            if self.links.insert(agent_id.clone(), id).is_none() {
                self.link_order.push_back(agent_id);
            }
        }
        while self.call_order.len() > MAX_INDEX_ENTRIES {
            if let Some(oldest) = self.call_order.pop_front() {
                self.calls.remove(&oldest);
            }
        }
        while self.link_order.len() > MAX_INDEX_ENTRIES {
            if let Some(oldest) = self.link_order.pop_front() {
                self.links.remove(&oldest);
            }
        }
    }''')

io.open(p, 'w', encoding='utf-8', newline='\n').write(s)
print('ok')
