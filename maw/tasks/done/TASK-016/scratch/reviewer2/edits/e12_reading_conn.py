import os, sys
sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
from ed import edit
WS = os.path.join(os.path.dirname(os.path.abspath(__file__)), '..', 'ws')
edit(os.path.join(WS, 'crates/cctg/src/hub/stream.rs'), [
('''    /// When the request in flight went out.
    pub reading: Option<Instant>,''', '''    /// The request in flight: the connection it went to and when.
    pub reading: Option<(u64, Instant)>,'''),
])
P = os.path.join(WS, 'crates/cctg/src/hub/slots.rs')
edit(P, [
('''/// A stream whose message Telegram did not take reads again after this.
const REWIND_AFTER: Duration = Duration::from_secs(5);
''', ''),
('''    /// Longest wait of a turn answer for the end of its turn in the
    /// transcript (the lines before it go first).
    pub hold_answer: Duration,''', '''    /// Longest wait of a turn answer for the end of its turn in the
    /// transcript (the lines before it go first).
    pub hold_answer: Duration,
    /// A stream whose message Telegram did not take reads again after this.
    pub stream_retry: Duration,'''),
('''            hold_answer: Duration::from_secs(5),''', '''            hold_answer: Duration::from_secs(5),
            stream_retry: Duration::from_secs(5),'''),
('''                let read = match live.reading {
                    Some(sent) => Some(sent + READ_TIMEOUT),''', '''                let read = match live.reading {
                    Some((_, sent)) => Some(sent + READ_TIMEOUT),'''),
('''                        self.on_chunk(&session, from, to, &lines, missing, more, reset);''',
 '''                        let chunk = Chunk {
                            from,
                            to,
                            lines: &lines,
                            missing,
                            more,
                            reset,
                        };
                        self.on_chunk(conn, &session, &chunk);'''),
('''            if let Some(sent) = live.reading
                && now >= sent + READ_TIMEOUT
            {''', '''            // A read that is late, or went to a connection that is no longer
            // the session's, is asked again.
            let conn = target.as_ref().map(|(conn, _)| *conn);
            if let Some((asked, sent)) = live.reading
                && (now >= sent + READ_TIMEOUT || conn != Some(asked))
            {'''),
('''            if asked {
                live.reading = Some(now);''', '''            if asked {
                live.reading = Some((conn, now));'''),
('''    /// One answered transcript read: its messages go to the topic in order,
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
    ) {''', '''    /// One answered transcript read: its messages go to the topic in order,
    /// its reactions out, a held answer after the lines of its turn.
    fn on_chunk(&mut self, conn: u64, session: &str, chunk: &Chunk<'_>) {
        let &Chunk {
            from,
            to,
            lines,
            missing,
            more,
            reset,
        } = chunk;'''),
('''        if live.reading.take().is_none() {
            debug!(
                session = short(session),
                "transcript chunk nobody asked for; dropped"
            );
            return;
        }''', '''        if live.reading.is_none_or(|(asked, _)| asked != conn) {
            debug!(
                session = short(session),
                "transcript chunk nobody asked for; dropped"
            );
            return;
        }
        live.reading = None;'''),
('''            live.rewind(offset, calls, Instant::now() + REWIND_AFTER);''',
 '''            live.rewind(offset, calls, Instant::now() + self.options.stream_retry);'''),
('''/// A job for the dispatch task.''', '''/// The fields of one `transcript_chunk`.
struct Chunk<'a> {
    from: u64,
    to: u64,
    lines: &'a [StreamLine],
    missing: bool,
    more: bool,
    reset: bool,
}

/// A job for the dispatch task.'''),
])
print('ok')
