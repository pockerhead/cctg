import os, sys
sys.path.insert(0, os.path.dirname(__file__))
from ed import edit
WS = os.path.join(os.path.dirname(__file__), '..', 'ws')
edit(os.path.join(WS, 'crates/cctg/src/hub/registry.rs'), [
(r'''    /// Transcript bytes whose stream messages Telegram has answered (the
    /// next read after a restart starts here). `None`: nothing read yet;
    /// the first read starts at the end of the file, so a session resumed
    /// from before the hub knew it does not replay its history.
    #[serde(default)]
    pub offset: Option<u64>,
    /// Tool calls read before `offset` whose line waits for its result.
    #[serde(default)]
    pub calls: Vec<PendingCall>,''', r'''    /// Transcript bytes whose stream messages Telegram has accepted (the
    /// next read after a restart starts here). `None`: nothing read yet;
    /// the first read starts at the end of the file, so a session resumed
    /// from before the hub knew it does not replay its history.
    #[serde(default)]
    pub offset: Option<u64>,
    /// Tool calls of the turn at `offset` whose line is not sent yet: no
    /// result yet, or an earlier call has none.
    #[serde(default)]
    pub calls: Vec<PendingCall>,'''),
(r'''#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PendingCall {
    pub id: String,
    /// The `/brief` line of the call.
    pub line: String,
    /// End of the line with its result, once read; the call is forgotten
    /// when `offset` passes it.
    #[serde(default)]
    pub result_end: Option<u64>,
}''', r'''#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PendingCall {
    pub id: String,
    /// The `/brief` line of the call.
    pub line: String,
    /// Its result is in.
    #[serde(default)]
    pub done: bool,
    /// The first line of a failed result.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}'''),
])
