import os, sys
sys.path.insert(0, os.path.dirname(__file__))
from ed import edit
WS = os.path.join(os.path.dirname(__file__), '..', 'ws')
edit(os.path.join(WS, 'crates/cctg/src/wire.rs'), [
(r'''    /// The answer to one `transcript_read`: the stream events of the complete
    /// lines in `from..to`. `missing`: the file is not there (yet). `more`:
    /// the file has complete lines past `to` that did not fit.
    TranscriptChunk {
        session_id: String,
        from: u64,
        to: u64,
        #[serde(default)]
        lines: Vec<StreamLine>,
        #[serde(default)]
        missing: bool,
        #[serde(default)]
        more: bool,
    },''', r'''    /// The answer to one `transcript_read`: the stream events of the complete
    /// lines in `from..to`. `missing`: the file is not there (yet). `more`:
    /// the file has complete lines past `to` that did not fit. `reset`: the
    /// file no longer continues at `from` (it is shorter, or `from` is not
    /// the end of a line): a new file, to be read from its start.
    TranscriptChunk {
        session_id: String,
        from: u64,
        to: u64,
        #[serde(default)]
        lines: Vec<StreamLine>,
        #[serde(default)]
        missing: bool,
        #[serde(default)]
        more: bool,
        #[serde(default)]
        reset: bool,
    },'''),
(r'''    Result {
        id: String,
        #[serde(default)]
        error: Option<String>,
    },
    /// A kind from a newer agent; skipped.''', r'''    Result {
        id: String,
        #[serde(default)]
        error: Option<String>,
    },
    /// The assistant text that ends a turn; its text comes from `Stop`.
    TurnEnd,
    /// A kind from a newer agent; skipped.'''),
(r'''                missing: false,
                more: true,
            },''', r'''                missing: false,
                more: true,
                reset: false,
            },'''),
])
