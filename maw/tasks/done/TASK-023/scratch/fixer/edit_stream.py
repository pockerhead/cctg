# Fixer edit script (TASK-023 I1): pairs held answers with their turn end by transcript byte.
p = 'C:/Users/user/dev/cctg/crates/cctg/src/hub/stream.rs'
s = open(p, encoding='utf-8').read()


def rep(a, b, cnt=1):
    global s
    assert s.count(a) == cnt, (a[:80], s.count(a))
    s = s.replace(a, b)


rep("""//! A released turn answer rides the stream as its messages (see
//! [`Live::sent_answer`]): it waits behind a refused line like the lines do,
//! and a rewind holds it again, so it shows after the lines of its turn.
""", """//! A released turn answer rides the stream as its messages (see
//! [`Live::sent_answer`]): it waits behind a refused line like the lines do,
//! and a rewind holds it again, so it shows after the lines of its turn.
//! An answer is paired with its turn end by the transcript byte of that turn
//! end ([`Held::end`]), not by count: a turn end read again after a rewind
//! lets only its own answer go, or nothing when that answer is in the topic
//! already ([`Live::turn_end`]).
""")
rep("""    pub answer: String,
    pub until: Instant,
}""", """    pub answer: String,
    pub until: Instant,
    /// The transcript byte of its turn end, once paired with one.
    pub end: Option<u64>,
}""")
rep("""    /// Turn ends read that no `Stop` has taken yet (the file was quicker than
    /// the hook). A new prompt read after them does not clear them at once
    /// (their `Stop` may still be on its way to the actor); they lapse at
    /// `ends_until`.
    pub ends_unclaimed: u8,""", """    /// Turn ends read that no `Stop` has taken yet (the file was quicker than
    /// the hook), by transcript byte, oldest first, at most [`MAX_HELD`]. A
    /// new prompt read after them does not clear them at once (their `Stop`
    /// may still be on its way to the actor); they lapse at `ends_until`.
    pub ends_unclaimed: VecDeque<u64>,""")
rep("""    pub restart: bool,
    waiting: VecDeque<Entry>,""", """    pub restart: bool,
    /// Turn ends ahead of `read_at` whose answer went to the topic already:
    /// read again, they let nothing go and are not claimable.
    answered_ends: Vec<u64>,
    waiting: VecDeque<Entry>,""")
rep("""    pub fn lapse_ends(&mut self, until: Instant) {
        if self.ends_unclaimed > 0 {""", """    pub fn lapse_ends(&mut self, until: Instant) {
        if !self.ends_unclaimed.is_empty() {""")
rep("""    /// A `Stop` takes the oldest turn end read before it, if one is still
    /// claimable at `now`.
    pub fn claim_end(&mut self, now: Instant) -> bool {
        if self.ends_until.is_some_and(|until| now >= until) {
            self.ends_unclaimed = 0;
        }
        let claimed = self.ends_unclaimed > 0;
        self.ends_unclaimed = self.ends_unclaimed.saturating_sub(1);
        if self.ends_unclaimed == 0 {
            self.ends_until = None;
        }
        claimed
    }
""", """    /// A `Stop` takes the oldest turn end read before it, if one is still
    /// claimable at `now`: its transcript byte.
    pub fn claim_end(&mut self, now: Instant) -> Option<u64> {
        if self.ends_until.is_some_and(|until| now >= until) {
            self.ends_unclaimed.clear();
        }
        let claimed = self.ends_unclaimed.pop_front();
        if self.ends_unclaimed.is_empty() {
            self.ends_until = None;
        }
        claimed
    }

    /// The turn end at transcript byte `end` was read: the held answer it
    /// lets go, if any. Its own answer (held again by a rewind) first, else
    /// the oldest held answer not paired with a later turn end. A turn end
    /// whose answer is in the topic already lets nothing go; one without an
    /// answer stays claimable for the next `Stop`.
    pub fn turn_end(&mut self, end: u64) -> Option<Held> {
        if let Some(at) = self.answered_ends.iter().position(|&known| known == end) {
            self.answered_ends.swap_remove(at);
            return None;
        }
        let own = self.held.iter().position(|held| held.end == Some(end));
        let at = own.or_else(|| {
            self.held
                .front()
                .is_some_and(|held| held.end.is_none_or(|known| known <= end))
                .then_some(0)
        });
        if let Some(mut held) = at.and_then(|at| self.held.remove(at)) {
            held.end = Some(end);
            return Some(held);
        }
        if self.ends_unclaimed.len() < MAX_HELD {
            self.ends_unclaimed.push_back(end);
        }
        None
    }

    /// Held answers at the front whose turn end lies before `from`: the
    /// lines of their turn are in the topic, nothing reads that end again.
    pub fn overdue(&mut self, from: u64) -> Vec<Held> {
        let mut due = Vec::new();
        while self
            .held
            .front()
            .is_some_and(|held| held.end.is_some_and(|end| end <= from))
        {
            due.extend(self.held.pop_front());
        }
        due
    }

    /// `held` goes by its timeout ahead of a read of its turn end: that turn
    /// end, read later, must not let another answer go.
    pub fn answered_early(&mut self, held: &Held) {
        if let Some(end) = held.end
            && self.read_at.is_some_and(|at| at < end)
        {
            self.answered_ends.push(end);
        }
    }
""")
rep("""    /// Starts over at the last barrier Telegram fully accepted, keeping the
    /// held answers and warnings. Answers sent since that barrier and not in
    /// the topic are held again, before those still held; the read at `at`
    /// lets them go after their lines again. None goes by its timeout before
    /// `at + hold`, so an answer whose turn end is not read again waits at
    /// most that long.
    pub fn rewind(
        &mut self,
        offset: Option<u64>,
        calls: Vec<PendingCall>,
        at: Instant,
        hold: Duration,
    ) {
        let mut held: VecDeque<Held> = std::mem::take(&mut self.waiting)
            .into_iter()
            .filter_map(|entry| match entry {
                Entry::Message {
                    state: Answer::Waiting | Answer::Refused,
                    answer,
                    ..
                } => answer,
                _ => None,
            })
            .collect();
        held.append(&mut self.held);
        for answer in &mut held {
            answer.until = answer.until.max(at + hold);
        }
        let refused_warned = self.refused_warned;
        let file_seen = self.file_seen;
        *self = Self::new(offset, calls);
        self.held = held;
        self.refused_warned = refused_warned;
        self.file_seen = file_seen;
        self.next_read = Some(at);
    }""", """    /// Starts over at the last barrier Telegram fully accepted, keeping the
    /// held answers and warnings. Answers sent since that barrier and not in
    /// the topic are held again, before those still held; the read of their
    /// own turn end lets them go after their lines again, and a turn end
    /// read again whose answer is in the topic lets nothing go. None goes by
    /// its timeout before `at + hold`; a read later than that lets it go
    /// ahead of its lines (the timeout bounds the wait).
    pub fn rewind(
        &mut self,
        offset: Option<u64>,
        calls: Vec<PendingCall>,
        at: Instant,
        hold: Duration,
    ) {
        let read_again = |end: &u64| offset.is_some_and(|offset| *end > offset);
        let mut held = VecDeque::new();
        let mut answered_ends: Vec<u64> =
            self.answered_ends.drain(..).filter(read_again).collect();
        for entry in std::mem::take(&mut self.waiting) {
            if let Entry::Message {
                state,
                answer: Some(answer),
                ..
            } = entry
            {
                if state == Answer::Accepted {
                    answered_ends.extend(answer.end.filter(read_again));
                } else {
                    held.push_back(answer);
                }
            }
        }
        held.append(&mut self.held);
        for answer in &mut held {
            answer.until = answer.until.max(at + hold);
        }
        // Unclaimed turn ends before the offset are not read again.
        let mut ends_unclaimed = std::mem::take(&mut self.ends_unclaimed);
        ends_unclaimed.retain(|end| !read_again(end));
        let ends_until = self.ends_until.filter(|_| !ends_unclaimed.is_empty());
        let refused_warned = self.refused_warned;
        let file_seen = self.file_seen;
        *self = Self::new(offset, calls);
        self.held = held;
        self.answered_ends = answered_ends;
        self.ends_unclaimed = ends_unclaimed;
        self.ends_until = ends_until;
        self.refused_warned = refused_warned;
        self.file_seen = file_seen;
        self.next_read = Some(at);
    }""")
rep("""            answer: answer.into(),
            until: now,
        };""", """            answer: answer.into(),
            until: now,
            end: None,
        };""")
rep("""        assert!(!live.claim_end(now), "nothing read");
        live.ends_unclaimed = 2;
        live.lapse_ends(now + Duration::from_secs(5));
        // A later new turn does not push the lapse out.
        live.lapse_ends(now + Duration::from_secs(50));
        assert!(live.claim_end(now + Duration::from_secs(1)));
        assert!(!live.claim_end(now + Duration::from_secs(5)), "lapsed");
        assert_eq!((live.ends_unclaimed, live.ends_until), (0, None));
        // Without a new turn a turn end waits for its Stop however long.
        live.ends_unclaimed = 1;
        assert!(live.claim_end(now + Duration::from_secs(3600)));
        assert!(!live.claim_end(now + Duration::from_secs(3600)));""", """        assert_eq!(live.claim_end(now), None, "nothing read");
        live.ends_unclaimed = VecDeque::from([10, 20]);
        live.lapse_ends(now + Duration::from_secs(5));
        // A later new turn does not push the lapse out.
        live.lapse_ends(now + Duration::from_secs(50));
        assert_eq!(live.claim_end(now + Duration::from_secs(1)), Some(10));
        assert_eq!(live.claim_end(now + Duration::from_secs(5)), None, "lapsed");
        assert!(live.ends_unclaimed.is_empty());
        assert_eq!(live.ends_until, None);
        // Without a new turn a turn end waits for its Stop however long.
        live.ends_unclaimed = VecDeque::from([30]);
        assert_eq!(live.claim_end(now + Duration::from_secs(3600)), Some(30));
        assert_eq!(live.claim_end(now + Duration::from_secs(3600)), None);""")
open(p, 'w', encoding='utf-8', newline='').write(s)
print("ok")
