//! Slot registry: which forum topic belongs to which `(device, folder,
//! ordinal)` slot and which session currently lives in it.
//!
//! Pure state and decisions, no IO: the `slots` actor feeds it events and
//! turns [`Registry::topic_work`] into Bot API calls. A slot owns its topic
//! forever; sessions succeed each other inside it. Nested runs and subagents
//! never get a slot, they only point at their parent's.
//!
//! Paths, folder names and titles are private: nothing here logs them.

use std::collections::{BTreeMap, BTreeSet, HashSet};
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};
use transcript::telegram_len;

use super::buffer::Buffer;
use crate::wire::{HookEvent, HookPost};

pub const VERSION: u32 = 1;
/// Telegram limit for a topic name. Measured in UTF-16 units, which is never
/// less than the character count.
pub const MAX_TITLE: usize = 128;
/// A host name longer than this is cut, so `[host]` never eats the title.
const MAX_HOST: usize = 32;
/// Ended sessions that no slot shows any more are forgotten beyond this count.
pub const MAX_SESSIONS: usize = 1024;
const SHORT_ID: usize = 8;
const CLEAR_HANDOFF_TTL: Duration = Duration::from_secs(60);
/// A session started this recently is never ended by a device's list of live
/// claude pids: its SessionStart can overtake the POST of a list taken before
/// its process existed. A hook sends within 0.8 s of its snapshot.
const REAP_GRACE: Duration = Duration::from_secs(5);

const FILE_NAME: &str = "registry.json";
const TEMP_NAME: &str = "registry.json.tmp";

/// Preferred icons, from `getForumTopicIconStickers` (2026-09-23, 112
/// stickers). Used only while Telegram offers them, see [`Icons::from_offered`].
pub const ICON_ALIVE: &str = "5312016608254762256"; // ⚡️
pub const ICON_DEAD: &str = "5408906741125490282"; // 🏁
pub const ICON_WAITING: &str = "5377316857231450742"; // ❓
pub const ICON_NO_CHANNEL: &str = "5357121491508928442"; // 👀

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SlotState {
    Alive,
    Dead,
    Waiting,
    NoChannel,
}

/// `icon_custom_emoji_id` per state; `None` leaves the icon alone.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Icons {
    pub alive: Option<String>,
    pub dead: Option<String>,
    pub waiting: Option<String>,
    pub no_channel: Option<String>,
}

impl Default for Icons {
    fn default() -> Self {
        Self {
            alive: Some(ICON_ALIVE.to_owned()),
            dead: Some(ICON_DEAD.to_owned()),
            waiting: Some(ICON_WAITING.to_owned()),
            no_channel: Some(ICON_NO_CHANNEL.to_owned()),
        }
    }
}

impl Icons {
    pub fn for_state(&self, state: SlotState) -> Option<&str> {
        match state {
            SlotState::Alive => self.alive.as_deref(),
            SlotState::Dead => self.dead.as_deref(),
            SlotState::Waiting => self.waiting.as_deref(),
            SlotState::NoChannel => self.no_channel.as_deref(),
        }
    }

    /// Icons from the ids `getForumTopicIconStickers` offers: the preferred
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
}

/// Strips the `\\?\` prefix, turns `\` into `/` and drops trailing
/// separators. Keeps the original case.
fn lexical(cwd: &str) -> String {
    let unprefixed = if let Some(rest) = cwd
        .get(..8)
        .filter(|head| head.eq_ignore_ascii_case(r"\\?\UNC\"))
        .and_then(|_| cwd.get(8..))
    {
        format!(r"\\{rest}")
    } else {
        cwd.strip_prefix(r"\\?\").unwrap_or(cwd).to_owned()
    };
    let mut path = unprefixed.replace('\\', "/");
    while path.len() > 1 && path.ends_with('/') {
        path.pop();
    }
    path
}

fn is_windows_path(path: &str) -> bool {
    let bytes = path.as_bytes();
    (bytes.len() >= 2 && bytes[0].is_ascii_alphabetic() && bytes[1] == b':')
        || path.starts_with("//")
}

/// Slot identity of a folder: lexical only, case-folded for Windows paths
/// (drive letter or UNC). Symlinks, junctions and 8.3 names are resolved by
/// the reporting device, never here: the hub cannot see another machine.
pub fn folder_key(cwd: &str) -> String {
    let path = lexical(cwd);
    if is_windows_path(&path) {
        path.to_lowercase()
    } else {
        path
    }
}

/// Last component of `cwd` as it was spelled, for the topic title.
pub fn folder_name(cwd: &str) -> String {
    let path = lexical(cwd);
    match path.rsplit('/').next() {
        Some(last) if !last.is_empty() => last.to_owned(),
        _ => path,
    }
}

fn short(session_id: &str) -> &str {
    session_id
        .char_indices()
        .nth(SHORT_ID)
        .map_or(session_id, |(end, _)| &session_id[..end])
}

/// Control characters (newlines included) become spaces, runs of whitespace one.
fn one_line(text: &str) -> String {
    text.split(|c: char| c.is_whitespace() || c.is_control())
        .filter(|word| !word.is_empty())
        .collect::<Vec<_>>()
        .join(" ")
}

/// At most `limit` UTF-16 units; a cut text ends with `…`.
pub(crate) fn cut(text: &str, limit: usize) -> String {
    if telegram_len(text) <= limit {
        return text.to_owned();
    }
    if limit == 0 {
        return String::new();
    }
    let mut out = String::new();
    let mut used = 1; // the ellipsis
    for c in text.chars() {
        if used + c.len_utf16() > limit {
            break;
        }
        used += c.len_utf16();
        out.push(c);
    }
    out.push('…');
    out
}

/// `[host] folder #N · label`, at most [`MAX_TITLE`] UTF-16 units. `#N` only
/// for `ordinal > 1`. The label goes first when space runs out, then the
/// folder; `[host]` and `#N` always stay.
pub fn topic_title(host: &str, folder: &str, ordinal: u32, label: Option<&str>) -> String {
    let head = format!("[{}]", cut(&one_line(host), MAX_HOST));
    let suffix = if ordinal > 1 {
        format!(" #{ordinal}")
    } else {
        String::new()
    };
    let folder_room = MAX_TITLE - telegram_len(&head) - 1 - telegram_len(&suffix);
    let mut title = format!("{head} {}{suffix}", cut(&one_line(folder), folder_room));
    if let Some(label) = label.map(one_line).filter(|label| !label.is_empty()) {
        let room = MAX_TITLE.saturating_sub(telegram_len(&title) + telegram_len(" · "));
        if room > 1 {
            title.push_str(" · ");
            title.push_str(&cut(&label, room));
        }
    }
    title
}

/// Last line of a block whose subagent or nested run is still working.
pub const BLOCK_RUNNING: &str = "в работе…";
/// Last line of a block whose session ended before its result arrived.
pub const BLOCK_LOST: &str = "итог не получен";
/// Tries of one block send or edit before it is given up.
pub const MAX_BLOCK_ATTEMPTS: u32 = 5;
/// Subagent records kept; beyond this the oldest settled one is forgotten
/// (a reply to its block then carries no `target_agent`).
pub const MAX_SUBAGENTS: usize = 1024;

/// `<type> <short id>` of a block: the type from a subagent header
/// `↳ <type> <id>[: <description>]`, `nested` for a nested run.
fn block_label(key: &BlockKey, header: &str) -> String {
    match key {
        BlockKey::Agent(id) => {
            let kind = header
                .strip_prefix("↳ ")
                .and_then(|rest| rest.split_once(&format!(" {id}")))
                .map_or("agent", |(kind, _)| kind);
            format!("{kind} {}", short(id))
        }
        BlockKey::Nested(session) => format!("nested {}", short(session)),
    }
}

/// `⇣ nested <short id>`, the header of a nested run's block.
pub fn nested_header(session_id: &str) -> String {
    format!("⇣ nested {}", short(session_id))
}

pub fn separator(session_id: &str, resumed: bool) -> String {
    let how = if resumed { "resumed" } else { "new" };
    format!("── session {} · {how} ──", short(session_id))
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct SlotId(pub usize);

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Slot {
    pub host: String,
    /// [`folder_key`] of the folder: the identity.
    pub folder_key: String,
    /// [`folder_name`] as first spelled: the display.
    pub folder_name: String,
    pub ordinal: u32,
    #[serde(default)]
    pub topic_id: Option<i64>,
    #[serde(default)]
    pub current_session: Option<String>,
    /// Name and icon Telegram shows now, as far as the hub knows.
    #[serde(default)]
    pub applied_title: Option<String>,
    #[serde(default)]
    pub applied_icon: Option<String>,
    /// Separator to post once the topic exists.
    #[serde(default)]
    pub pending_separator: Option<String>,
    /// Topic messages no session of the slot could take yet (TASK-017).
    #[serde(default, skip_serializing_if = "Buffer::is_idle")]
    pub buffer: Buffer,
    /// A topic call for this slot is in flight.
    #[serde(skip)]
    pub busy: bool,
    /// Name and icon a call already failed with; not retried until they
    /// change or [`Registry::retry_failed`] runs.
    #[serde(skip)]
    pub failed: Option<(String, Option<String>)>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum SessionKind {
    TopLevel,
    /// A nested `claude -p`. `parent: None` is "nested, parent unknown":
    /// still never a slot of its own.
    Nested {
        parent: Option<String>,
    },
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SessionEntry {
    pub host: String,
    pub kind: SessionKind,
    /// Own slot for a top-level session, the parent's slot for a nested one.
    #[serde(default)]
    pub slot: Option<SlotId>,
    #[serde(default)]
    pub transcript_path: String,
    #[serde(default)]
    pub claude_pid: Option<u32>,
    #[serde(default)]
    pub title: Option<String>,
    #[serde(default)]
    pub ended: bool,
    /// Registry sequence number of the last event; orders pruning.
    #[serde(default)]
    pub seen: u64,
    /// Agent connection number; connections do not survive a hub restart.
    #[serde(skip)]
    pub agent: Option<u64>,
    #[serde(skip)]
    pub waiting: bool,
    /// The `⇣ nested` block of a nested run in its parent's topic.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub block: Option<Block>,
    /// The live transcript stream of a top-level session (TASK-016).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub stream: Option<Stream>,
}

/// What survives a restart of a session's transcript stream.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Stream {
    /// Transcript bytes whose stream messages Telegram has accepted (the
    /// next read after a restart starts here). `None`: nothing read yet;
    /// the first read starts at the end of the file, so a session resumed
    /// from before the hub knew it does not replay its history.
    #[serde(default)]
    pub offset: Option<u64>,
    /// Tool calls of the turn at `offset` whose line is not sent yet: no
    /// result yet, or an earlier call has none.
    #[serde(default)]
    pub calls: Vec<PendingCall>,
    /// Telegram messages handed to the session's agent that show 👀 and wait
    /// for their channel record to turn ✍.
    #[serde(default)]
    pub receipts: Vec<i64>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
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
}

/// A subagent matched to an `Agent` call of its parent: it has a block in
/// the topic of the parent's slot.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SubagentEntry {
    pub parent_session: String,
    #[serde(default)]
    pub slot: Option<SlotId>,
    #[serde(default)]
    pub block: Block,
    /// Registry sequence number of the confirmation; orders eviction.
    #[serde(default)]
    pub seen: u64,
}

/// One collapsed message in a topic: a subagent or a nested run. The
/// registry keeps the text Telegram should show until Telegram took it, so a
/// restart neither loses nor repeats it.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Block {
    /// `↳ <type> <id>[: <description>]` or `⇣ nested <id>`.
    #[serde(default)]
    pub header: String,
    /// The topic the message went to.
    #[serde(default)]
    pub thread_id: Option<i64>,
    #[serde(default)]
    pub message_id: Option<i64>,
    /// Text to send or edit to; `None` once Telegram shows it.
    #[serde(default)]
    pub pending: Option<String>,
    /// The block still ends with [`BLOCK_RUNNING`].
    #[serde(default)]
    pub running: bool,
    /// The first send is out without an answer. Found so after a restart,
    /// the block is never sent again: Telegram may have it already.
    #[serde(default)]
    pub sending: bool,
    /// A nested run's last turn answer, shown when the run ends.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub answer: Option<String>,
    /// The "finished" reply to this block was handed out; never again.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub notified: bool,
    #[serde(skip)]
    pub busy: bool,
    /// Waits for the retry tick.
    #[serde(skip)]
    pub failed: bool,
    #[serde(skip)]
    pub attempts: u32,
}

impl Block {
    fn running(header: String) -> Self {
        Self {
            pending: Some(format!("{header}\n{BLOCK_RUNNING}")),
            header,
            running: true,
            ..Self::default()
        }
    }
}

/// A short reply to a block that has just shown its final text, so the
/// topic gets a notification (an edit gives none).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BlockNotice {
    pub thread_id: i64,
    pub reply_to: i64,
    pub text: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum BlockKey {
    /// By agent id.
    Agent(String),
    /// By the nested run's session id.
    Nested(String),
}

/// A block message the actor should send or edit. At most one per block is
/// in flight.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum BlockJob {
    Send {
        key: BlockKey,
        thread_id: i64,
        text: String,
    },
    Edit {
        key: BlockKey,
        message_id: i64,
        text: String,
    },
}

/// A topic call the actor should make. At most one per slot is in flight: the
/// slot is marked busy until the matching `topic_*` result comes back.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TopicJob {
    Create {
        slot: SlotId,
        name: String,
        icon: Option<String>,
    },
    Edit {
        slot: SlotId,
        thread_id: i64,
        /// `None` keeps the current name or icon.
        name: Option<String>,
        icon: Option<String>,
    },
    Separator {
        slot: SlotId,
        thread_id: i64,
        text: String,
    },
}

/// What a hook event or an agent registration asks of the actor.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Followup {
    /// Read the ai-title from this transcript for this session.
    pub read_title: Option<(String, String)>,
    /// Sessions ended or pruned by this transition. The slots actor uses the
    /// ids to close prompts even when pruning removed the registry entries.
    pub ended_sessions: Vec<String>,
    /// The part of `ended_sessions` ended because their claude process was
    /// missing from the device's list of live pids.
    pub reaped: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Registry {
    pub version: u32,
    #[serde(default)]
    pub seq: u64,
    #[serde(default)]
    pub slots: Vec<Slot>,
    #[serde(default)]
    pub sessions: BTreeMap<String, SessionEntry>,
    #[serde(default)]
    pub subagents: BTreeMap<String, SubagentEntry>,
    /// `"<host>/<claude pid>"` -> session id; filled on SessionStart, cleared
    /// on SessionEnd (pids are reused).
    #[serde(default)]
    pub pids: BTreeMap<String, String>,
    /// Slot released by a just-ended `/clear`, keyed by host/pid. This is
    /// deliberately transient: pids are reused and the matching SessionStart
    /// follows immediately.
    #[serde(skip)]
    recent_clears: BTreeMap<String, (SlotId, Instant)>,
    /// Sessions whose SessionStart came within [`REAP_GRACE`]. Transient: a
    /// hub restart takes longer than the grace.
    #[serde(skip)]
    recent_starts: BTreeMap<String, Instant>,
    /// Set by every change that must reach `registry.json`.
    #[serde(skip)]
    pub dirty: bool,
}

impl Default for Registry {
    fn default() -> Self {
        Self {
            version: VERSION,
            seq: 0,
            slots: Vec::new(),
            sessions: BTreeMap::new(),
            subagents: BTreeMap::new(),
            pids: BTreeMap::new(),
            recent_clears: BTreeMap::new(),
            recent_starts: BTreeMap::new(),
            dirty: false,
        }
    }
}

fn pid_key(host: &str, pid: u32) -> String {
    format!("{host}/{pid}")
}

impl Registry {
    fn touch(&mut self) -> u64 {
        self.seq += 1;
        self.dirty = true;
        self.seq
    }

    pub fn slot(&self, id: SlotId) -> Option<&Slot> {
        self.slots.get(id.0)
    }

    /// The caller sets [`Registry::dirty`] when it changes a saved field.
    pub fn slot_mut(&mut self, id: SlotId) -> Option<&mut Slot> {
        self.slots.get_mut(id.0)
    }

    pub fn slot_by_topic(&self, thread_id: i64) -> Option<SlotId> {
        self.slots
            .iter()
            .position(|slot| slot.topic_id == Some(thread_id))
            .map(SlotId)
    }

    /// A slot is free when no session that is still running holds it.
    fn is_free(&self, id: SlotId) -> bool {
        match self.slots[id.0].current_session.as_deref() {
            None => true,
            Some(session) => self.sessions.get(session).is_none_or(|entry| entry.ended),
        }
    }

    pub fn state(&self, id: SlotId) -> SlotState {
        let entry = self.slots[id.0]
            .current_session
            .as_deref()
            .and_then(|session| self.sessions.get(session));
        match entry {
            None => SlotState::Dead,
            Some(entry) if entry.ended => SlotState::Dead,
            Some(entry) if entry.agent.is_none() => SlotState::NoChannel,
            Some(entry) if entry.waiting => SlotState::Waiting,
            Some(_) => SlotState::Alive,
        }
    }

    pub fn desired_title(&self, id: SlotId) -> String {
        let slot = &self.slots[id.0];
        let label = slot.current_session.as_deref().map(|session| {
            self.sessions
                .get(session)
                .and_then(|entry| entry.title.clone())
                .unwrap_or_else(|| short(session).to_owned())
        });
        topic_title(
            &slot.host,
            &slot.folder_name,
            slot.ordinal,
            label.as_deref(),
        )
    }

    /// The slot for a new top-level session, in this order: the slot it
    /// already holds; its previous slot if free; on `/clear` (`clear_pid`),
    /// the slot of the previous session of the same claude process, which
    /// that session gives up; the first free slot of the folder; a new
    /// ordinal.
    fn allocate(
        &mut self,
        session: &str,
        host: &str,
        cwd: &str,
        clear_pid: Option<u32>,
        clear_slot: Option<SlotId>,
    ) -> SlotId {
        let key = folder_key(cwd);
        let same_folder = |slot: &Slot| slot.host == host && slot.folder_key == key;
        if let Some(own) = self.sessions.get(session).and_then(|entry| entry.slot) {
            let slot = &self.slots[own.0];
            if same_folder(slot)
                && (slot.current_session.as_deref() == Some(session) || self.is_free(own))
            {
                return own;
            }
            // Known limitation (TASK-011 review I6): a session resumed from
            // another folder takes a slot there, and its old slot keeps
            // showing it (busy, same icon) until its SessionEnd.
        }
        if let Some(id) = clear_slot
            && self.slots.get(id.0).is_some_and(same_folder)
            && self.is_free(id)
        {
            return id;
        }
        let previous = clear_pid
            .and_then(|pid| self.pids.get(&pid_key(host, pid)))
            .filter(|previous| previous.as_str() != session)
            .cloned();
        if let Some(previous) = previous
            && let Some(id) = self.sessions.get(&previous).and_then(|entry| entry.slot)
            && same_folder(&self.slots[id.0])
            && self.slots[id.0].current_session.as_deref() == Some(previous.as_str())
        {
            if let Some(entry) = self.sessions.get_mut(&previous) {
                entry.ended = true;
                entry.waiting = false;
            }
            return id;
        }
        let mut ordinals: Vec<(u32, SlotId)> = self
            .slots
            .iter()
            .enumerate()
            .filter(|(_, slot)| same_folder(slot))
            .map(|(index, slot)| (slot.ordinal, SlotId(index)))
            .collect();
        ordinals.sort();
        if let Some(&(_, id)) = ordinals.iter().find(|(_, id)| self.is_free(*id)) {
            return id;
        }
        let ordinal = ordinals.last().map_or(1, |(ordinal, _)| ordinal + 1);
        self.slots.push(Slot {
            host: host.to_owned(),
            folder_key: key,
            folder_name: folder_name(cwd),
            ordinal,
            topic_id: None,
            current_session: None,
            applied_title: None,
            applied_icon: None,
            pending_separator: None,
            buffer: Buffer::default(),
            busy: false,
            failed: None,
        });
        SlotId(self.slots.len() - 1)
    }

    fn occupy(&mut self, id: SlotId, session: &str, resumed: bool) {
        let slot = &mut self.slots[id.0];
        match slot.current_session.as_deref() {
            Some(current) if current == session => {}
            Some(_) => {
                slot.pending_separator = Some(separator(session, resumed));
                slot.current_session = Some(session.to_owned());
            }
            None => slot.current_session = Some(session.to_owned()),
        }
    }

    /// Nesting from the pid the device reports as the next claude ancestor.
    /// An ancestor the registry does not know is "nested, parent unknown".
    fn nesting(&self, host: &str, parent_pid: Option<u32>) -> SessionKind {
        let Some(pid) = parent_pid else {
            return SessionKind::TopLevel;
        };
        SessionKind::Nested {
            parent: self.pids.get(&pid_key(host, pid)).cloned(),
        }
    }

    fn take_clear_slot(&mut self, host: &str, pid: u32, cwd: &str) -> Option<SlotId> {
        let now = Instant::now();
        self.recent_clears.retain(|_, (_, expires)| *expires > now);
        let (slot, _) = self.recent_clears.remove(&pid_key(host, pid))?;
        let wanted_folder = folder_key(cwd);
        self.slots
            .get(slot.0)
            .is_some_and(|entry| entry.host == host && entry.folder_key == wanted_folder)
            .then_some(slot)
    }

    /// Registers a session start. Top-level sessions get a slot, nested ones
    /// only a reference to the parent's.
    pub fn session_started(
        &mut self,
        post: &HookPost,
        source: Option<&str>,
        claude_pid: Option<u32>,
        parent_pid: Option<u32>,
    ) -> SlotOrParent {
        let session = post.session_id.as_str();
        if parent_pid.is_some_and(|pid| {
            self.pids.get(&pid_key(&post.host, pid)).map(String::as_str) == Some(session)
        }) {
            // The ancestor is this very session: a stale CLAUDE_PID on the
            // device, or a nested resume of the parent's own id. It proves
            // nothing about top-level: nested with an unknown parent, and the
            // session's existing record stays as it is.
            return SlotOrParent::Parent(None);
        }
        if parent_pid.is_some()
            && self
                .sessions
                .get(session)
                .is_some_and(|entry| entry.kind == SessionKind::TopLevel)
        {
            // A nested invocation may resume an id that is also a real
            // top-level session. Route this invocation to its parent without
            // rewriting the durable identity or releasing its own slot.
            let slot = parent_pid
                .and_then(|pid| self.pids.get(&pid_key(&post.host, pid)))
                .and_then(|parent| self.sessions.get(parent))
                .and_then(|entry| entry.slot);
            return SlotOrParent::Parent(slot);
        }
        let clear = source == Some("clear");
        let clear_slot = match (source, claude_pid) {
            (Some("clear"), Some(pid)) => self.take_clear_slot(&post.host, pid, &post.cwd),
            (_, Some(pid)) => {
                self.recent_clears.remove(&pid_key(&post.host, pid));
                None
            }
            _ => None,
        };
        if !clear
            && let Some(previous) = claude_pid
                .and_then(|pid| self.pids.get(&pid_key(&post.host, pid)))
                .filter(|previous| previous.as_str() != session)
                .cloned()
            && let Some(entry) = self.sessions.get_mut(&previous)
        {
            // A reused pid: that process is gone and its SessionEnd was lost.
            // Only `/clear` hands its slot on; this start takes the first free.
            entry.ended = true;
            entry.waiting = false;
        }
        let kind = match self.sessions.get(session) {
            // A known session keeps what it was (a resume has no ancestor info
            // worth more than the first start).
            Some(entry) if parent_pid.is_none() => entry.kind.clone(),
            _ => self.nesting(&post.host, parent_pid),
        };
        let slot = match &kind {
            SessionKind::TopLevel => {
                let clear_pid = claude_pid.filter(|_| clear);
                let id = self.allocate(session, &post.host, &post.cwd, clear_pid, clear_slot);
                self.occupy(id, session, source == Some("resume"));
                Some(id)
            }
            SessionKind::Nested { parent } => parent
                .as_deref()
                .and_then(|parent| self.sessions.get(parent))
                .and_then(|entry| entry.slot),
        };
        let seen = self.touch();
        let entry = self
            .sessions
            .entry(session.to_owned())
            .or_insert_with(|| SessionEntry {
                host: post.host.clone(),
                kind: kind.clone(),
                slot,
                transcript_path: String::new(),
                claude_pid: None,
                title: None,
                ended: false,
                seen,
                agent: None,
                waiting: false,
                block: None,
                stream: None,
            });
        entry.kind = kind.clone();
        entry.slot = slot;
        entry.ended = false;
        entry.waiting = false;
        entry.seen = seen;
        if !post.transcript_path.is_empty() {
            entry.transcript_path.clone_from(&post.transcript_path);
        }
        if claude_pid.is_some() {
            entry.claude_pid = claude_pid;
        }
        if kind == SessionKind::TopLevel && entry.stream.is_none() {
            // A new transcript is streamed from its first byte; any other
            // start of a session the stream never saw begins at the end.
            let fresh = matches!(source, Some("startup" | "clear"));
            entry.stream = Some(Stream {
                offset: fresh.then_some(0),
                ..Stream::default()
            });
        }
        if let SessionKind::Nested { parent: Some(_) } = &kind
            && slot.is_some()
        {
            // One block per nested run, however often it starts.
            match &mut entry.block {
                None => entry.block = Some(Block::running(nested_header(session))),
                Some(block) if !block.running => {
                    block.pending = Some(format!("{}\n{BLOCK_RUNNING}", block.header));
                    block.running = true;
                }
                Some(_) => {}
            }
        }
        if let Some(pid) = claude_pid {
            self.pids
                .insert(pid_key(&post.host, pid), session.to_owned());
        }
        self.prune();
        match kind {
            SessionKind::TopLevel => {
                SlotOrParent::Own(slot.expect("top-level sessions get a slot"))
            }
            SessionKind::Nested { .. } => SlotOrParent::Parent(slot),
        }
    }

    /// Handles one hook event. A `SessionStart`/`SessionEnd` that brings its
    /// device's live claude pids first ends that host's sessions whose
    /// process is gone ([`Self::reap`]), so a start takes their slots.
    pub fn apply_hook(&mut self, post: &HookPost) -> Followup {
        let reaped = self.reap(post);
        let mut followup = self.apply_event(post);
        for session in &reaped {
            if !followup.ended_sessions.contains(session) {
                followup.ended_sessions.push(session.clone());
            }
        }
        followup.reaped = reaped;
        followup
    }

    /// Ends the sessions of `post.host`, top-level and nested, that have a
    /// `claude_pid` missing from `post.live_claude_pids`: their process died
    /// without a SessionEnd (window closed, killed, crashed). The end is the
    /// one a SessionEnd makes, without the `/clear` hand-on. Untouched: the
    /// posting session, other hosts, sessions without a pid, and starts within
    /// [`REAP_GRACE`]. A list without the posting process's own pid is not a
    /// view of this device's claudes and ends nothing.
    fn reap(&mut self, post: &HookPost) -> Vec<String> {
        let own = match &post.event {
            HookEvent::SessionStart { claude_pid, .. }
            | HookEvent::SessionEnd { claude_pid, .. } => *claude_pid,
            _ => None,
        };
        let Some(live) = post
            .live_claude_pids
            .as_ref()
            .filter(|live| own.is_some_and(|pid| live.contains(&pid)))
        else {
            return Vec::new();
        };
        let live: HashSet<u32> = live.iter().copied().collect();
        let now = Instant::now();
        self.recent_starts
            .retain(|_, started| now.duration_since(*started) < REAP_GRACE);
        let dead: Vec<(String, u32)> = self
            .sessions
            .iter()
            .filter(|(id, entry)| {
                !entry.ended
                    && entry.host == post.host
                    && id.as_str() != post.session_id
                    && !self.recent_starts.contains_key(id.as_str())
            })
            .filter_map(|(id, entry)| {
                entry
                    .claude_pid
                    .filter(|pid| !live.contains(pid))
                    .map(|pid| (id.clone(), pid))
            })
            .collect();
        for (session, pid) in &dead {
            if let Some(entry) = self.sessions.get_mut(session) {
                entry.ended = true;
                entry.waiting = false;
                entry.agent = None;
            }
            let key = pid_key(&post.host, *pid);
            if self.pids.get(&key) == Some(session) {
                self.pids.remove(&key);
            }
        }
        if !dead.is_empty() {
            self.touch();
        }
        dead.into_iter().map(|(session, _)| session).collect()
    }

    /// Every start so far is past [`REAP_GRACE`].
    #[cfg(test)]
    pub(crate) fn forget_recent_starts(&mut self) {
        self.recent_starts.clear();
    }

    fn apply_event(&mut self, post: &HookPost) -> Followup {
        let session = post.session_id.as_str();
        match &post.event {
            HookEvent::SessionStart {
                source,
                claude_pid,
                parent_claude_pid,
            } => {
                let before: Vec<(String, bool)> = self
                    .sessions
                    .iter()
                    .map(|(id, entry)| (id.clone(), entry.ended))
                    .collect();
                self.session_started(post, source.as_deref(), *claude_pid, *parent_claude_pid);
                let now = Instant::now();
                self.recent_starts
                    .retain(|_, started| now.duration_since(*started) < REAP_GRACE);
                self.recent_starts.insert(session.to_owned(), now);
                let ended_sessions = before
                    .into_iter()
                    .filter_map(|(id, was_ended)| match self.sessions.get(&id) {
                        None => Some(id),
                        Some(entry) if !was_ended && entry.ended => Some(id),
                        Some(_) => None,
                    })
                    .collect();
                Followup {
                    ended_sessions,
                    ..Followup::default()
                }
            }
            HookEvent::SessionEnd { reason, claude_pid } => {
                let Some(entry) = self.sessions.get_mut(session) else {
                    return Followup::default();
                };
                if claude_pid.is_some()
                    && entry.claude_pid.is_some()
                    && *claude_pid != entry.claude_pid
                {
                    // A nested `claude -p --resume <id>` of this session ended,
                    // not the session's own run.
                    return Followup::default();
                }
                entry.ended = true;
                entry.waiting = false;
                // The run's agent goes with it: a resume starts a new claude
                // and a new agent, and the old link, still open for a moment,
                // must not take the slot's kept messages (TASK-017). After
                // `/clear` the agent follows its pid (`Slots::follow_pid`).
                entry.agent = None;
                // A nested entry's slot is its parent's: never handed on.
                let clear = entry
                    .claude_pid
                    .zip(entry.slot)
                    .filter(|_| entry.kind == SessionKind::TopLevel)
                    .map(|(pid, slot)| (pid_key(&entry.host, pid), slot));
                if let Some(pid) = entry.claude_pid {
                    let key = pid_key(&entry.host, pid);
                    if self.pids.get(&key).map(String::as_str) == Some(session) {
                        self.pids.remove(&key);
                    }
                }
                if let Some((key, slot)) = clear {
                    // Taken already: the next session's SessionStart came first.
                    let pid_free = !self.pids.contains_key(&key);
                    self.recent_clears.remove(&key);
                    if reason.as_deref() == Some("clear") && pid_free {
                        self.recent_clears
                            .insert(key, (slot, Instant::now() + CLEAR_HANDOFF_TTL));
                    }
                }
                self.touch();
                Followup {
                    ended_sessions: vec![session.to_owned()],
                    ..Followup::default()
                }
            }
            HookEvent::UserPromptSubmit { .. } | HookEvent::Stop { .. } => {
                // Only a SessionStart tells top-level from nested: a session
                // whose start was never seen gets nothing until its next one.
                let Some(entry) = self.sessions.get_mut(session) else {
                    return Followup::default();
                };
                entry.waiting = false;
                if entry.transcript_path.is_empty() && !post.transcript_path.is_empty() {
                    entry.transcript_path.clone_from(&post.transcript_path);
                    self.dirty = true;
                }
                let wants_title = entry.title.is_none()
                    && entry.kind == SessionKind::TopLevel
                    && !entry.transcript_path.is_empty();
                Followup {
                    read_title: wants_title
                        .then(|| (session.to_owned(), entry.transcript_path.clone())),
                    ..Followup::default()
                }
            }
            // A subagent is recorded only once the slots actor matched it to
            // an `Agent` call of its parent ([`Registry::confirm_subagent`]):
            // Claude Code's internal agents must leave no trace.
            HookEvent::SubagentStart { .. }
            | HookEvent::SubagentStop { .. }
            | HookEvent::SubagentHandback { .. } => Followup::default(),
        }
    }

    /// An agent registered. `false`: the session is unknown; the caller keeps
    /// the agent waiting for its SessionStart and never adopts it (without a
    /// SessionStart a nested run cannot be told from a top-level one). The
    /// agent of a nested run is never bound: it is not a channel of any slot.
    pub fn agent_connected(&mut self, session: &str, conn: u64) -> bool {
        match self.sessions.get_mut(session) {
            Some(entry) if entry.kind == SessionKind::TopLevel => {
                entry.agent = Some(conn);
                true
            }
            _ => false,
        }
    }

    /// The running top-level session of a claude process, if the registry
    /// knows one. After `/clear` this is the new session of the same process.
    pub fn live_session_of_pid(&self, host: &str, pid: u32) -> Option<&str> {
        let session = self.pids.get(&pid_key(host, pid))?;
        self.sessions
            .get(session)
            .filter(|entry| !entry.ended && entry.kind == SessionKind::TopLevel)
            .map(|_| session.as_str())
    }

    /// A known top-level session that has not ended.
    pub fn is_live_top_level(&self, session: &str) -> bool {
        self.sessions
            .get(session)
            .is_some_and(|entry| !entry.ended && entry.kind == SessionKind::TopLevel)
    }

    pub fn agent_disconnected(&mut self, session: &str, conn: u64) {
        if let Some(entry) = self.sessions.get_mut(session)
            && entry.agent == Some(conn)
        {
            entry.agent = None;
            entry.waiting = false;
        }
    }

    pub fn set_waiting(&mut self, session: &str, waiting: bool) {
        if let Some(entry) = self.sessions.get_mut(session) {
            entry.waiting = waiting;
        }
    }

    pub fn set_title(&mut self, session: &str, title: &str) {
        let title = one_line(title);
        if let Some(entry) = self.sessions.get_mut(session)
            && !title.is_empty()
            && entry.title.as_deref() != Some(title.as_str())
        {
            entry.title = Some(title);
            self.dirty = true;
        }
    }

    /// After a restart no agent is connected and nothing is in flight.
    pub fn after_restart(&mut self) {
        for entry in self.sessions.values_mut() {
            entry.agent = None;
            entry.waiting = false;
        }
        for slot in &mut self.slots {
            slot.busy = false;
            slot.failed = None;
        }
        for block in self.blocks_mut() {
            block.busy = false;
            block.failed = false;
            block.attempts = 0;
        }
    }

    fn blocks_mut(&mut self) -> impl Iterator<Item = &mut Block> {
        let agents = self.subagents.values_mut().map(|entry| &mut entry.block);
        let nested = self
            .sessions
            .values_mut()
            .filter_map(|entry| entry.block.as_mut());
        agents.chain(nested)
    }

    pub fn block(&self, key: &BlockKey) -> Option<&Block> {
        match key {
            BlockKey::Agent(id) => self.subagents.get(id).map(|entry| &entry.block),
            BlockKey::Nested(session) => self.sessions.get(session)?.block.as_ref(),
        }
    }

    fn block_mut(&mut self, key: &BlockKey) -> Option<&mut Block> {
        match key {
            BlockKey::Agent(id) => self.subagents.get_mut(id).map(|entry| &mut entry.block),
            BlockKey::Nested(session) => self.sessions.get_mut(session)?.block.as_mut(),
        }
    }

    pub fn block_slot(&self, key: &BlockKey) -> Option<SlotId> {
        match key {
            BlockKey::Agent(id) => self.subagents.get(id)?.slot,
            BlockKey::Nested(session) => self.sessions.get(session)?.slot,
        }
    }

    /// Records a subagent matched to an `Agent` call of `parent`, with a
    /// running block. `false`: it has a block already (a repeated hook, a
    /// restart), or the parent is not a top-level session with a slot.
    pub fn confirm_subagent(&mut self, agent_id: &str, parent: &str, header: String) -> bool {
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
        let answer = self
            .sessions
            .get_mut(session)?
            .block
            .as_mut()?
            .answer
            .take();
        if answer.is_some() {
            self.dirty = true;
        }
        answer
    }

    /// The block shall show `text`; `running` says whether it still works.
    pub fn show_block(&mut self, key: &BlockKey, text: String, running: bool) {
        if let Some(block) = self.block_mut(key) {
            block.pending = Some(text);
            block.running = running;
            block.failed = false;
            block.attempts = 0;
            self.dirty = true;
        }
    }

    /// Running blocks that belong to `ended` sessions end as [`BLOCK_LOST`]:
    /// the subagents of those sessions, the nested runs themselves and the
    /// nested runs whose parent is among them. A later result still wins.
    pub fn lose_blocks(&mut self, ended: &[String]) {
        if ended.is_empty() {
            return;
        }
        let ended: HashSet<&str> = ended.iter().map(String::as_str).collect();
        let mut lost = Vec::new();
        for (id, entry) in &self.subagents {
            if entry.block.running && ended.contains(entry.parent_session.as_str()) {
                lost.push(BlockKey::Agent(id.clone()));
            }
        }
        for (id, entry) in &self.sessions {
            let parent_ended = matches!(&entry.kind, SessionKind::Nested { parent: Some(parent) }
                if ended.contains(parent.as_str()));
            if entry.block.as_ref().is_some_and(|block| block.running)
                && (ended.contains(id.as_str()) || parent_ended)
            {
                lost.push(BlockKey::Nested(id.clone()));
            }
        }
        for key in lost {
            if let Some(header) = self.block(&key).map(|block| block.header.clone()) {
                self.show_block(&key, format!("{header}\n{BLOCK_LOST}"), false);
            }
            if let BlockKey::Nested(session) = &key {
                self.take_nested_answer(session);
            }
        }
    }

    /// At most `limit` block sends and edits that are due; the rest wait in
    /// the registry. A first send needs the slot's topic; a block whose first
    /// send may have reached Telegram (`sending` without an answer, e.g. cut
    /// off by a restart) is never sent again: its text is dropped.
    pub fn block_work(&mut self, limit: usize) -> Vec<BlockJob> {
        let keys: Vec<BlockKey> = self
            .subagents
            .iter()
            .filter(|(_, entry)| entry.block.pending.is_some())
            .map(|(id, _)| BlockKey::Agent(id.clone()))
            .chain(
                self.sessions
                    .iter()
                    .filter(|(_, entry)| {
                        entry
                            .block
                            .as_ref()
                            .is_some_and(|block| block.pending.is_some())
                    })
                    .map(|(id, _)| BlockKey::Nested(id.clone())),
            )
            .collect();
        let mut jobs = Vec::new();
        for key in keys {
            if jobs.len() >= limit {
                break;
            }
            let topic = self
                .block_slot(&key)
                .and_then(|slot| self.slot(slot))
                .and_then(|slot| slot.topic_id);
            let Some(block) = self.block_mut(&key) else {
                continue;
            };
            if block.busy || block.failed {
                continue;
            }
            let Some(text) = block.pending.clone() else {
                continue;
            };
            match block.message_id {
                Some(message_id) => {
                    block.busy = true;
                    jobs.push(BlockJob::Edit {
                        key,
                        message_id,
                        text,
                    });
                }
                None if block.sending => {
                    block.pending = None;
                    block.running = false;
                    self.dirty = true;
                }
                None => {
                    let Some(thread_id) = topic else {
                        continue;
                    };
                    block.busy = true;
                    block.sending = true;
                    block.thread_id = Some(thread_id);
                    self.dirty = true;
                    jobs.push(BlockJob::Send {
                        key,
                        thread_id,
                        text,
                    });
                }
            }
        }
        jobs
    }

    /// Telegram shows `text`; `message_id` is set for a first send. The
    /// first time a block shows its final text, the reply that announces it.
    pub fn block_done(
        &mut self,
        key: &BlockKey,
        text: &str,
        message_id: Option<i64>,
    ) -> Option<BlockNotice> {
        let block = self.block_mut(key)?;
        block.busy = false;
        block.sending = false;
        block.attempts = 0;
        if message_id.is_some() {
            block.message_id = message_id;
        }
        if block.pending.as_deref() == Some(text) {
            block.pending = None;
        }
        self.dirty = true;
        let block = self.block(key)?;
        if block.notified || block.running || block.pending.is_some() {
            return None;
        }
        let (thread_id, reply_to) = (block.thread_id?, block.message_id?);
        let lost = text
            == format!(
                "{}
{BLOCK_LOST}",
                block.header
            );
        let label = block_label(key, &block.header);
        self.block_mut(key)?.notified = true;
        Some(BlockNotice {
            thread_id,
            reply_to,
            text: if lost {
                format!("✗ {label} {BLOCK_LOST}")
            } else {
                format!("✓ {label} закончил")
            },
        })
    }

    /// A send or edit failed: tried again on the retry tick, given up after
    /// [`MAX_BLOCK_ATTEMPTS`]. `true` when it was given up now.
    pub fn block_failed(&mut self, key: &BlockKey) -> bool {
        let Some(block) = self.block_mut(key) else {
            return false;
        };
        block.busy = false;
        block.sending = false;
        block.attempts += 1;
        let given_up = block.attempts >= MAX_BLOCK_ATTEMPTS;
        if given_up {
            block.pending = None;
            block.attempts = 0;
        } else {
            block.failed = true;
        }
        self.dirty = true;
        given_up
    }

    /// A first send got no clear answer: Telegram may show it, so it is
    /// never sent again (at most once); the block keeps no text to show.
    pub fn block_send_unclear(&mut self, key: &BlockKey) {
        if let Some(block) = self.block_mut(key) {
            block.busy = false;
            block.pending = None;
            block.running = false;
            self.dirty = true;
        }
    }

    /// The agent id of the subagent block `message_id` in `thread_id`, when
    /// that subagent belongs to `session`.
    pub fn subagent_of_message(
        &self,
        thread_id: i64,
        message_id: i64,
        session: &str,
    ) -> Option<&str> {
        self.subagents
            .iter()
            .find(|(_, entry)| {
                entry.parent_session == session
                    && entry.block.message_id == Some(message_id)
                    && entry.block.thread_id == Some(thread_id)
            })
            .map(|(id, _)| id.as_str())
    }

    /// Forgets the oldest ended sessions that no slot shows, beyond
    /// [`MAX_SESSIONS`], with their subagents.
    fn prune(&mut self) {
        if self.sessions.len() <= MAX_SESSIONS {
            return;
        }
        let current: HashSet<&str> = self
            .slots
            .iter()
            .filter_map(|slot| slot.current_session.as_deref())
            .collect();
        let mut candidates: Vec<(u64, String)> = self
            .sessions
            .iter()
            .filter(|(id, entry)| entry.ended && !current.contains(id.as_str()))
            .map(|(id, entry)| (entry.seen, id.clone()))
            .collect();
        candidates.sort();
        let excess = self.sessions.len() - MAX_SESSIONS;
        let gone: HashSet<String> = candidates
            .into_iter()
            .take(excess)
            .map(|(_, id)| id)
            .collect();
        self.sessions.retain(|id, _| !gone.contains(id));
        self.subagents
            .retain(|_, agent| !gone.contains(&agent.parent_session));
        self.pids.retain(|_, session| !gone.contains(session));
        self.dirty = true;
    }

    /// Topic calls needed to make Telegram match the registry. Marks the
    /// slots busy. `edits`: false during the start-up grace, when agents are
    /// still reconnecting and icons would flicker.
    pub fn topic_work(&mut self, icons: &Icons, edits: bool) -> Vec<TopicJob> {
        let mut jobs = Vec::new();
        for index in 0..self.slots.len() {
            let id = SlotId(index);
            if self.slots[index].busy {
                continue;
            }
            let name = self.desired_title(id);
            let icon = icons.for_state(self.state(id)).map(str::to_owned);
            let wanted = (name.clone(), icon.clone());
            let slot = &mut self.slots[index];
            if slot.failed.as_ref() == Some(&wanted) {
                continue;
            }
            let Some(thread_id) = slot.topic_id else {
                slot.busy = true;
                jobs.push(TopicJob::Create {
                    slot: id,
                    name,
                    icon,
                });
                continue;
            };
            // One call per slot at a time; the separator stays pending until
            // Telegram took it.
            if let Some(text) = slot.pending_separator.clone() {
                slot.busy = true;
                jobs.push(TopicJob::Separator {
                    slot: id,
                    thread_id,
                    text,
                });
                continue;
            }
            if !edits {
                continue;
            }
            let slot = &mut self.slots[index];
            let new_name = (slot.applied_title.as_deref() != Some(name.as_str())).then_some(name);
            let new_icon = icon.filter(|icon| slot.applied_icon.as_deref() != Some(icon.as_str()));
            if new_name.is_some() || new_icon.is_some() {
                slot.busy = true;
                jobs.push(TopicJob::Edit {
                    slot: id,
                    thread_id,
                    name: new_name,
                    icon: new_icon,
                });
            }
        }
        jobs
    }

    pub fn topic_created(&mut self, id: SlotId, thread_id: i64, name: &str, icon: Option<&str>) {
        let slot = &mut self.slots[id.0];
        slot.busy = false;
        slot.failed = None;
        slot.topic_id = Some(thread_id);
        slot.applied_title = Some(name.to_owned());
        slot.applied_icon = icon.map(str::to_owned);
        // A new topic starts with its first session: nothing to separate.
        slot.pending_separator = None;
        self.dirty = true;
    }

    /// Records a successful (or not-modified) edit of `thread_id`. A result
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
        slot.failed = None;
        if let Some(name) = name {
            slot.applied_title = Some(name.to_owned());
        }
        if let Some(icon) = icon {
            slot.applied_icon = Some(icon.to_owned());
        }
        self.dirty = true;
    }

    /// The separator `text` reached `thread_id`. A stale result changes nothing.
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
    /// changes or [`Registry::retry_failed`] runs.
    pub fn topic_failed(&mut self, id: SlotId, icons: &Icons) {
        let wanted = (
            self.desired_title(id),
            icons.for_state(self.state(id)).map(str::to_owned),
        );
        let slot = &mut self.slots[id.0];
        slot.busy = false;
        slot.failed = Some(wanted);
    }

    /// Telegram says `thread_id` is gone. Only the slot still bound to that
    /// topic forgets it, once; the next [`Registry::topic_work`] creates
    /// exactly one replacement. A late report of an old topic changes
    /// nothing, not even `busy`: the replacement may be in flight.
    pub fn topic_invalid(&mut self, id: SlotId, thread_id: i64) {
        let slot = &mut self.slots[id.0];
        if slot.topic_id == Some(thread_id) {
            slot.busy = false;
            slot.topic_id = None;
            slot.applied_title = None;
            slot.applied_icon = None;
            slot.pending_separator = None;
            slot.failed = None;
            self.dirty = true;
        }
    }

    pub fn retry_failed(&mut self) {
        for slot in &mut self.slots {
            slot.failed = None;
        }
        for block in self.blocks_mut() {
            block.failed = false;
        }
    }

    /// `thread_id -> (session id, transcript path)` of each slot's current session.
    pub fn topic_view(&self) -> TopicView {
        self.slots
            .iter()
            .filter_map(|slot| {
                let thread_id = slot.topic_id?;
                let session = slot.current_session.clone()?;
                let path = self
                    .sessions
                    .get(&session)
                    .map(|entry| entry.transcript_path.clone())
                    .unwrap_or_default();
                Some((thread_id, (session, path)))
            })
            .collect()
    }
}

/// `thread_id -> (session id, transcript path)`, what `/brief` in a topic reads.
pub type TopicView = BTreeMap<i64, (String, String)>;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SlotOrParent {
    Own(SlotId),
    /// Nested run: the parent's slot, if the parent is known.
    Parent(Option<SlotId>),
}

#[derive(Debug, thiserror::Error)]
pub enum LoadError {
    #[error("cannot read {FILE_NAME} ({0:?})")]
    Read(io::ErrorKind),
    /// No serde text: it would quote registry contents (private paths).
    #[error("{FILE_NAME} is not a valid registry; fix or move it away")]
    Invalid,
    #[error("{FILE_NAME} has version {0}, this hub reads version {VERSION}")]
    Version(u32),
}

/// `registry.json` in the hub state directory.
#[derive(Debug, Clone)]
pub struct RegistryStore {
    dir: PathBuf,
}

impl RegistryStore {
    pub fn open(dir: &Path) -> io::Result<Self> {
        std::fs::create_dir_all(dir)?;
        Ok(Self {
            dir: dir.to_owned(),
        })
    }

    /// An empty registry when there is no file. A file that does not parse
    /// is an error: starting empty would create a second topic per folder.
    pub fn load(&self) -> Result<Registry, LoadError> {
        let bytes = match std::fs::read(self.dir.join(FILE_NAME)) {
            Ok(bytes) => bytes,
            Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(Registry::default()),
            Err(error) => return Err(LoadError::Read(error.kind())),
        };
        let mut registry: Registry =
            serde_json::from_slice(&bytes).map_err(|_| LoadError::Invalid)?;
        if registry.version != VERSION {
            return Err(LoadError::Version(registry.version));
        }
        let slots = registry.slots.len();
        let bad_slot = |slot: Option<SlotId>| slot.is_some_and(|id| id.0 >= slots);
        let mut topic_ids = BTreeSet::new();
        let mut identities = BTreeSet::new();
        let duplicate_slot = registry.slots.iter().any(|slot| {
            !identities.insert((&slot.host, &slot.folder_key, slot.ordinal))
                || slot
                    .topic_id
                    .is_some_and(|topic_id| !topic_ids.insert(topic_id))
        });
        if registry.sessions.values().any(|entry| bad_slot(entry.slot))
            || registry
                .subagents
                .values()
                .any(|agent| bad_slot(agent.slot))
            || duplicate_slot
        {
            return Err(LoadError::Invalid);
        }
        // Records written before subagents were matched to `Agent` calls have
        // no block header; they may be Claude Code's internal agents.
        registry
            .subagents
            .retain(|_, agent| !agent.block.header.is_empty());
        registry.after_restart();
        Ok(registry)
    }

    pub fn encode(registry: &Registry) -> Vec<u8> {
        serde_json::to_vec_pretty(registry)
            .expect("the registry has string keys and always serializes")
    }

    /// Temp file, fsync, rename: an interrupted save leaves the previous file.
    pub fn save(&self, bytes: &[u8]) -> io::Result<()> {
        let temp = self.dir.join(TEMP_NAME);
        let mut file = std::fs::File::create(&temp)?;
        file.write_all(bytes)?;
        file.sync_all()?;
        drop(file);
        std::fs::rename(&temp, self.dir.join(FILE_NAME))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::hub::testdir::TempDir;

    const A: &str = "aaaaaaaa-0000-4000-8000-000000000001";
    const B: &str = "bbbbbbbb-0000-4000-8000-000000000002";
    const C: &str = "cccccccc-0000-4000-8000-000000000003";
    const N: &str = "dddddddd-0000-4000-8000-000000000004";
    const CWD: &str = r"C:\Work\Project";

    fn post(session: &str, cwd: &str, event: HookEvent) -> HookPost {
        HookPost::new(
            "box".into(),
            session.into(),
            cwd.into(),
            format!("/t/{session}.jsonl"),
            event,
        )
    }

    fn start(session: &str, cwd: &str, pid: Option<u32>, parent: Option<u32>) -> HookPost {
        start_from(session, cwd, pid, parent, "startup")
    }

    fn start_from(
        session: &str,
        cwd: &str,
        pid: Option<u32>,
        parent: Option<u32>,
        source: &str,
    ) -> HookPost {
        post(
            session,
            cwd,
            HookEvent::SessionStart {
                source: Some(source.into()),
                claude_pid: pid,
                parent_claude_pid: parent,
            },
        )
    }

    fn end(session: &str) -> HookPost {
        end_by(session, None)
    }

    fn end_by(session: &str, pid: Option<u32>) -> HookPost {
        post(
            session,
            CWD,
            HookEvent::SessionEnd {
                reason: None,
                claude_pid: pid,
            },
        )
    }

    fn clear_end(session: &str) -> HookPost {
        post(
            session,
            CWD,
            HookEvent::SessionEnd {
                reason: Some("clear".into()),
                claude_pid: None,
            },
        )
    }

    /// `post` as sent with the device's live claude pids.
    fn listing(mut post: HookPost, live: &[u32]) -> HookPost {
        post.live_claude_pids = Some(live.to_vec());
        post
    }

    fn slot_of(registry: &Registry, session: &str) -> Option<SlotId> {
        registry.sessions[session].slot
    }

    /// Runs `topic_work` and answers every job as Telegram would, numbering
    /// new topics from 100. Returns the jobs.
    fn settle(registry: &mut Registry, next_topic: &mut i64) -> Vec<TopicJob> {
        let jobs = registry.topic_work(&Icons::default(), true);
        for job in &jobs {
            match job {
                TopicJob::Create { slot, name, icon } => {
                    registry.topic_created(*slot, *next_topic, name, icon.as_deref());
                    *next_topic += 1;
                }
                TopicJob::Edit {
                    slot,
                    thread_id,
                    name,
                    icon,
                } => registry.topic_edited(*slot, *thread_id, name.as_deref(), icon.as_deref()),
                TopicJob::Separator {
                    slot,
                    thread_id,
                    text,
                } => registry.topic_separated(*slot, *thread_id, text),
            }
        }
        jobs
    }

    fn creates(jobs: &[TopicJob]) -> usize {
        jobs.iter()
            .filter(|job| matches!(job, TopicJob::Create { .. }))
            .count()
    }

    fn separators(jobs: &[TopicJob]) -> Vec<String> {
        jobs.iter()
            .filter_map(|job| match job {
                TopicJob::Separator { text, .. } => Some(text.clone()),
                _ => None,
            })
            .collect()
    }

    #[test]
    fn folder_key_normalizes_windows_spellings_only() {
        let key = folder_key(r"C:\Work\Project");
        for spelling in [
            r"c:/work/project/",
            r"\\?\C:\Work\Project",
            r"C:\WORK\PROJECT\\",
            "C:/Work/Project",
        ] {
            assert_eq!(folder_key(spelling), key, "{spelling}");
        }
        assert_eq!(key, "c:/work/project");
        assert_eq!(
            folder_key(r"\\?\UNC\Server\Share\Dir"),
            folder_key(r"\\server\share\dir\")
        );
        assert_eq!(folder_key(r"\\Server\Share"), "//server/share");
        // POSIX paths are case-sensitive.
        assert_ne!(folder_key("/home/u/App"), folder_key("/home/u/app"));
        assert_eq!(folder_key("/home/u/app/"), "/home/u/app");
        assert_eq!(folder_key("/"), "/");
        assert_eq!(folder_key(r"C:\"), "c:");
    }

    #[test]
    fn folder_name_keeps_the_original_spelling() {
        assert_eq!(folder_name(r"C:\Work\Project"), "Project");
        assert_eq!(folder_name(r"\\?\C:\Work\MyApp\"), "MyApp");
        assert_eq!(folder_name("/home/u/App/"), "App");
        assert_eq!(folder_name(r"C:\"), "C:");
        assert_eq!(folder_name("/"), "/");
    }

    #[test]
    fn titles_fit_and_keep_host_and_ordinal() {
        assert_eq!(topic_title("box", "app", 1, None), "[box] app");
        assert_eq!(
            topic_title("box", "app", 2, Some("aaaaaaaa")),
            "[box] app #2 · aaaaaaaa"
        );
        assert_eq!(
            topic_title("box", "app", 1, Some("Fix\nthe  bug")),
            "[box] app · Fix the bug"
        );
        let long = "я".repeat(300);
        let emoji = "😀".repeat(200);
        for (host, folder, label) in [
            ("box", long.as_str(), Some(long.as_str())),
            ("box", "app", Some(emoji.as_str())),
            (long.as_str(), long.as_str(), Some("t")),
            ("box", emoji.as_str(), None),
        ] {
            for ordinal in [1, 2, 37, u32::MAX] {
                let title = topic_title(host, folder, ordinal, label);
                assert!(
                    telegram_len(&title) <= MAX_TITLE,
                    "{} {title}",
                    telegram_len(&title)
                );
                assert!(title.starts_with('['));
                if host == "box" {
                    assert!(title.starts_with("[box] "), "{title}");
                }
                if ordinal > 1 {
                    assert!(title.contains(&format!(" #{ordinal}")), "{title}");
                }
            }
        }
        // The label is cut before the folder.
        let title = topic_title("box", "app", 3, Some(&long));
        assert!(title.starts_with("[box] app #3 · я"), "{title}");
        assert!(title.ends_with('…'));
        assert_eq!(telegram_len(&title), MAX_TITLE);
    }

    #[test]
    fn short_id_becomes_the_ai_title() {
        let mut registry = Registry::default();
        registry.apply_hook(&start(A, CWD, Some(10), None));
        let id = slot_of(&registry, A).unwrap();
        assert_eq!(registry.desired_title(id), "[box] Project · aaaaaaaa");
        registry.set_title(A, "Slot registry\ndesign");
        assert_eq!(
            registry.desired_title(id),
            "[box] Project · Slot registry design"
        );
    }

    #[test]
    fn a_session_after_a_dead_one_reuses_the_slot_with_one_separator() {
        let mut registry = Registry::default();
        let mut topic = 100;
        registry.apply_hook(&start(A, CWD, Some(10), None));
        assert_eq!(creates(&settle(&mut registry, &mut topic)), 1);
        registry.apply_hook(&end(A));
        let jobs = settle(&mut registry, &mut topic);
        assert!(separators(&jobs).is_empty());
        registry.apply_hook(&start(B, r"c:\work\project\", Some(11), None));
        let jobs = settle(&mut registry, &mut topic);
        assert_eq!(creates(&jobs), 0);
        assert_eq!(separators(&jobs), ["── session bbbbbbbb · new ──"]);
        assert_eq!(slot_of(&registry, A), slot_of(&registry, B));
        assert_eq!(registry.slots.len(), 1);
        // Nothing more on the next pass.
        assert!(separators(&settle(&mut registry, &mut topic)).is_empty());
    }

    #[test]
    fn two_starts_after_a_death_take_the_old_slot_and_one_new_ordinal() {
        let mut registry = Registry::default();
        let mut topic = 100;
        registry.apply_hook(&start(A, CWD, Some(10), None));
        settle(&mut registry, &mut topic);
        registry.apply_hook(&end(A));
        // B and C both start before any topic call goes out.
        registry.apply_hook(&start(B, CWD, Some(11), None));
        registry.apply_hook(&start(C, CWD, Some(12), None));
        let jobs = settle(&mut registry, &mut topic);
        assert_eq!(creates(&jobs), 1, "{jobs:?}");
        assert_eq!(separators(&jobs), ["── session bbbbbbbb · new ──"]);
        assert_eq!(slot_of(&registry, B), slot_of(&registry, A));
        assert_eq!(registry.slots[slot_of(&registry, C).unwrap().0].ordinal, 2);
        let jobs = settle(&mut registry, &mut topic);
        assert_eq!(creates(&jobs), 0);
        assert!(separators(&jobs).is_empty());
        assert_eq!(registry.slots.len(), 2);
    }

    #[test]
    fn concurrent_sessions_get_new_ordinals_and_free_slots_are_reused_first() {
        let mut registry = Registry::default();
        let mut topic = 100;
        for (session, pid) in [(A, 10), (B, 11), (C, 12)] {
            registry.apply_hook(&start(session, CWD, Some(pid), None));
        }
        let jobs = settle(&mut registry, &mut topic);
        let names: Vec<String> = jobs
            .iter()
            .filter_map(|job| match job {
                TopicJob::Create { name, .. } => Some(name.clone()),
                _ => None,
            })
            .collect();
        assert_eq!(
            names,
            [
                "[box] Project · aaaaaaaa",
                "[box] Project #2 · bbbbbbbb",
                "[box] Project #3 · cccccccc"
            ]
        );
        // #2 dies; the next session takes #2, not a new #4.
        registry.apply_hook(&end(B));
        registry.apply_hook(&start(N, CWD, Some(13), None));
        let jobs = settle(&mut registry, &mut topic);
        assert_eq!(creates(&jobs), 0);
        assert_eq!(registry.slots[slot_of(&registry, N).unwrap().0].ordinal, 2);
        // Another folder or another host is another slot.
        registry.apply_hook(&start("e1", r"C:\Work\Other", Some(14), None));
        let mut other_host = start("e2", CWD, Some(15), None);
        other_host.host = "laptop".into();
        registry.apply_hook(&other_host);
        assert_eq!(creates(&settle(&mut registry, &mut topic)), 2);
        assert_eq!(registry.slots.len(), 5);
    }

    #[test]
    fn folder_spellings_share_one_slot() {
        let mut registry = Registry::default();
        registry.apply_hook(&start(A, r"C:\Work\Project", Some(10), None));
        registry.apply_hook(&end(A));
        registry.apply_hook(&start(B, r"\\?\C:\Work\Project", Some(11), None));
        registry.apply_hook(&end(B));
        registry.apply_hook(&start(C, "c:/work/project/", Some(12), None));
        assert_eq!(registry.slots.len(), 1);
        assert_eq!(registry.slots[0].folder_name, "Project");
    }

    #[test]
    fn nested_runs_and_subagents_get_no_slot() {
        let mut registry = Registry::default();
        let mut topic = 100;
        registry.apply_hook(&start(A, CWD, Some(10), None));
        settle(&mut registry, &mut topic);
        let parent_slot = slot_of(&registry, A);
        // Nested run in another folder: still the parent's slot.
        registry.apply_hook(&start(N, r"C:\Elsewhere", Some(20), Some(10)));
        assert_eq!(
            registry.sessions[N].kind,
            SessionKind::Nested {
                parent: Some(A.to_owned())
            }
        );
        assert_eq!(slot_of(&registry, N), parent_slot);
        // A nested run of the nested run points at the same slot.
        registry.apply_hook(&start(C, CWD, Some(30), Some(20)));
        assert_eq!(slot_of(&registry, C), parent_slot);
        // Ancestor not in the registry: nested, parent unknown, no slot.
        registry.apply_hook(&start(B, CWD, Some(40), Some(99)));
        assert_eq!(
            registry.sessions[B].kind,
            SessionKind::Nested { parent: None }
        );
        assert_eq!(slot_of(&registry, B), None);
        // Subagent hooks alone record nothing; a confirmed match records the
        // subagent against the parent's slot, once.
        for (agent, kind) in [("a1", "Explore"), ("a2", "")] {
            registry.apply_hook(&post(
                A,
                CWD,
                HookEvent::SubagentStart {
                    agent_id: agent.into(),
                    agent_type: kind.into(),
                },
            ));
        }
        assert!(registry.subagents.is_empty());
        assert!(registry.confirm_subagent("a1", A, "↳ Explore a1".into()));
        assert!(!registry.confirm_subagent("a1", A, "↳ Explore a1".into()));
        assert_eq!(registry.subagents["a1"].slot, parent_slot);
        assert_eq!(registry.subagents["a1"].parent_session, A);
        // A nested run's subagents get no block of their own.
        assert!(!registry.confirm_subagent("a3", N, "↳ Explore a3".into()));
        assert_eq!(creates(&settle(&mut registry, &mut topic)), 0);
        assert_eq!(registry.slots.len(), 1);
        // The parent's slot is still free for nobody else: nested runs do not hold it.
        assert_eq!(registry.slots[0].current_session.as_deref(), Some(A));
    }

    /// Answers every block job as Telegram would, numbering messages from 500.
    fn settle_blocks(registry: &mut Registry, next_message: &mut i64) -> Vec<BlockJob> {
        let jobs = registry.block_work(usize::MAX);
        for job in &jobs {
            match job {
                BlockJob::Send { key, text, .. } => {
                    registry.block_done(key, text, Some(*next_message));
                    *next_message += 1;
                }
                BlockJob::Edit { key, text, .. } => {
                    registry.block_done(key, text, None);
                }
            }
        }
        jobs
    }

    #[test]
    fn a_nested_run_gets_one_block_in_its_parents_topic() {
        let mut registry = Registry::default();
        let mut topic = 100;
        let mut message = 500;
        registry.apply_hook(&start(A, CWD, Some(10), None));
        // The parent's topic does not exist yet: the block waits for it.
        registry.apply_hook(&start(N, CWD, Some(20), Some(10)));
        assert!(registry.block_work(usize::MAX).is_empty());
        settle(&mut registry, &mut topic);
        let jobs = settle_blocks(&mut registry, &mut message);
        assert_eq!(
            jobs,
            [BlockJob::Send {
                key: BlockKey::Nested(N.to_owned()),
                thread_id: 100,
                text: format!("⇣ nested dddddddd\n{BLOCK_RUNNING}"),
            }]
        );
        // Another start of the same run keeps its one block.
        registry.apply_hook(&start_from(N, CWD, Some(21), Some(10), "resume"));
        assert!(settle_blocks(&mut registry, &mut message).is_empty());
        // Parent unknown: no block anywhere.
        registry.apply_hook(&start(B, CWD, Some(40), Some(99)));
        assert!(registry.sessions[B].block.is_none());
        // Ending the parent ends the running block deterministically.
        registry.lose_blocks(&[A.to_owned()]);
        let jobs = settle_blocks(&mut registry, &mut message);
        assert_eq!(
            jobs,
            [BlockJob::Edit {
                key: BlockKey::Nested(N.to_owned()),
                message_id: 500,
                text: format!("⇣ nested dddddddd\n{BLOCK_LOST}"),
            }]
        );
        assert_eq!(creates(&settle(&mut registry, &mut topic)), 0);
        assert_eq!(registry.slots.len(), 1);
    }

    #[test]
    fn blocks_survive_a_restart_without_a_second_send() {
        let dir = TempDir::new("registry-blocks");
        let store = RegistryStore::open(dir.path()).unwrap();
        let mut registry = Registry::default();
        let mut topic = 100;
        let mut message = 500;
        registry.apply_hook(&start(A, CWD, Some(10), None));
        settle(&mut registry, &mut topic);
        for agent in ["a1", "a2", "a3"] {
            registry.confirm_subagent(agent, A, format!("↳ Explore {agent}"));
        }
        // a1 and a2 were shown; a3's first send was out when the hub stopped.
        let jobs = registry.block_work(usize::MAX);
        assert_eq!(jobs.len(), 3);
        for job in &jobs[..2] {
            let BlockJob::Send { key, text, .. } = job else {
                panic!("{job:?}");
            };
            registry.block_done(key, text, Some(message));
            message += 1;
        }
        registry.show_block(
            &BlockKey::Agent("a1".into()),
            "↳ Explore a1\ndone".into(),
            false,
        );
        store.save(&RegistryStore::encode(&registry)).unwrap();

        let mut registry = store.load().unwrap();
        assert_eq!(registry.subagents.len(), 3);
        assert!(!registry.confirm_subagent("a2", A, "↳ Explore a2".into()));
        // Only a1's pending edit goes out; a3 is never sent again.
        let jobs = settle_blocks(&mut registry, &mut message);
        assert_eq!(
            jobs,
            [BlockJob::Edit {
                key: BlockKey::Agent("a1".into()),
                message_id: 500,
                text: "↳ Explore a1\ndone".into(),
            }]
        );
        assert!(settle_blocks(&mut registry, &mut message).is_empty());
        assert_eq!(registry.subagents["a3"].block.message_id, None);
        // The unfinished a2 ends as lost when its session is found ended.
        registry.apply_hook(&end(A));
        registry.lose_blocks(&[A.to_owned()]);
        let jobs = settle_blocks(&mut registry, &mut message);
        assert_eq!(
            jobs,
            [BlockJob::Edit {
                key: BlockKey::Agent("a2".into()),
                message_id: 501,
                text: format!("↳ Explore a2\n{BLOCK_LOST}"),
            }]
        );
    }

    /// [`settle_blocks`], returning the notices of the answered jobs.
    fn settle_notices(registry: &mut Registry, next_message: &mut i64) -> Vec<BlockNotice> {
        let mut notices = Vec::new();
        for job in registry.block_work(usize::MAX) {
            let notice = match &job {
                BlockJob::Send { key, text, .. } => {
                    *next_message += 1;
                    registry.block_done(key, text, Some(*next_message - 1))
                }
                BlockJob::Edit { key, text, .. } => registry.block_done(key, text, None),
            };
            notices.extend(notice);
        }
        notices
    }

    fn notice(reply_to: i64, text: &str) -> BlockNotice {
        BlockNotice {
            thread_id: 100,
            reply_to,
            text: text.to_owned(),
        }
    }

    #[test]
    fn a_finished_block_is_announced_once_by_a_reply() {
        let dir = TempDir::new("registry-block-notice");
        let store = RegistryStore::open(dir.path()).unwrap();
        let mut registry = Registry::default();
        let mut topic = 100;
        let mut message = 500;
        registry.apply_hook(&start(A, CWD, Some(10), None));
        settle(&mut registry, &mut topic);
        let a1 = BlockKey::Agent("a0000000000000001".into());
        registry.confirm_subagent(
            "a0000000000000001",
            A,
            "↳ Explore a0000000000000001: look".into(),
        );
        // Running: no notice.
        assert!(settle_notices(&mut registry, &mut message).is_empty());
        registry.show_block(&a1, "↳ Explore a0000000000000001: look\ndone".into(), false);
        assert_eq!(
            settle_notices(&mut registry, &mut message),
            [notice(500, "✓ Explore a0000000 закончил")]
        );
        // A later edit of the same block: no second notice.
        registry.show_block(
            &a1,
            "↳ Explore a0000000000000001: look\nlater".into(),
            false,
        );
        assert!(settle_notices(&mut registry, &mut message).is_empty());

        // A nested run: once, even when it runs and ends again.
        registry.apply_hook(&start(N, CWD, Some(20), Some(10)));
        assert!(settle_notices(&mut registry, &mut message).is_empty());
        let nested = BlockKey::Nested(N.to_owned());
        registry.show_block(&nested, "⇣ nested dddddddd\nanswer".into(), false);
        assert_eq!(
            settle_notices(&mut registry, &mut message),
            [notice(501, "✓ nested dddddddd закончил")]
        );
        registry.apply_hook(&start_from(N, CWD, Some(21), Some(10), "resume"));
        registry.show_block(&nested, "⇣ nested dddddddd\nagain".into(), false);
        assert!(settle_notices(&mut registry, &mut message).is_empty());

        // Finished before its first send: the send itself is announced.
        registry.confirm_subagent("a2", A, "↳ Plan a2".into());
        registry.show_block(
            &BlockKey::Agent("a2".into()),
            "↳ Plan a2\ndone".into(),
            false,
        );
        assert_eq!(
            settle_notices(&mut registry, &mut message),
            [notice(502, "✓ Plan a2 закончил")]
        );

        // A tombstone (first send without an answer) is never announced.
        registry.confirm_subagent("a3", A, "↳ Explore a3".into());
        let a3 = BlockKey::Agent("a3".into());
        assert_eq!(registry.block_work(usize::MAX).len(), 1);
        registry.block_send_unclear(&a3);
        registry.show_block(&a3, "↳ Explore a3\ndone".into(), false);
        assert!(settle_notices(&mut registry, &mut message).is_empty());

        // a4 is shown running when the hub stops; after the restart it is
        // lost and announced so, while a1 is not announced again.
        registry.confirm_subagent("a4", A, "↳ Explore a4".into());
        assert!(settle_notices(&mut registry, &mut message).is_empty());
        store.save(&RegistryStore::encode(&registry)).unwrap();
        let mut registry = store.load().unwrap();
        assert!(registry.subagents["a0000000000000001"].block.notified);
        registry.show_block(
            &a1,
            "↳ Explore a0000000000000001: look\nagain".into(),
            false,
        );
        registry.apply_hook(&end(A));
        registry.lose_blocks(&[A.to_owned()]);
        assert_eq!(
            settle_notices(&mut registry, &mut message),
            [notice(503, "✗ Explore a4 итог не получен")]
        );
    }

    #[test]
    fn a_failed_block_waits_for_the_tick_and_is_given_up_in_the_end() {
        let mut registry = Registry::default();
        let mut topic = 100;
        registry.apply_hook(&start(A, CWD, Some(10), None));
        settle(&mut registry, &mut topic);
        registry.confirm_subagent("a1", A, "↳ Explore a1".into());
        let key = BlockKey::Agent("a1".into());
        for attempt in 1..=MAX_BLOCK_ATTEMPTS {
            assert_eq!(
                registry.block_work(usize::MAX).len(),
                1,
                "attempt {attempt}"
            );
            let given_up = registry.block_failed(&key);
            assert_eq!(given_up, attempt == MAX_BLOCK_ATTEMPTS);
            // Not again before the tick.
            assert!(registry.block_work(usize::MAX).is_empty());
            registry.retry_failed();
        }
        assert!(registry.block_work(usize::MAX).is_empty());
        assert_eq!(registry.subagents["a1"].block.pending, None);
    }

    #[test]
    fn a_legacy_subagent_record_is_dropped_on_load() {
        let dir = TempDir::new("registry-legacy-subagent");
        let store = RegistryStore::open(dir.path()).unwrap();
        let mut registry = Registry::default();
        let mut topic = 100;
        registry.apply_hook(&start(A, CWD, Some(10), None));
        settle(&mut registry, &mut topic);
        registry.confirm_subagent("a1", A, "↳ Explore a1".into());
        registry.confirm_subagent("a2", A, "↳ Explore a2".into());
        // TASK-011 wrote `{ parent_session, slot }` for every typed hook.
        let mut json: serde_json::Value =
            serde_json::from_slice(&RegistryStore::encode(&registry)).unwrap();
        json["subagents"]["a2"]
            .as_object_mut()
            .unwrap()
            .remove("block");
        store.save(&serde_json::to_vec(&json).unwrap()).unwrap();
        let loaded = store.load().unwrap();
        assert_eq!(loaded.subagents.keys().collect::<Vec<_>>(), ["a1"]);
    }

    #[test]
    fn an_unclear_first_send_is_never_repeated() {
        let mut registry = Registry::default();
        let mut topic = 100;
        registry.apply_hook(&start(A, CWD, Some(10), None));
        settle(&mut registry, &mut topic);
        registry.confirm_subagent("a1", A, "↳ Explore a1".into());
        let key = BlockKey::Agent("a1".into());
        assert_eq!(registry.block_work(usize::MAX).len(), 1);
        registry.block_send_unclear(&key);
        // Neither its result nor its session's end sends it again.
        registry.show_block(&key, "↳ Explore a1\ndone".into(), false);
        assert!(registry.block_work(usize::MAX).is_empty());
        registry.retry_failed();
        registry.lose_blocks(&[A.to_owned()]);
        assert!(registry.block_work(usize::MAX).is_empty());
        let block = &registry.subagents["a1"].block;
        assert!(block.sending && block.pending.is_none() && block.message_id.is_none());
    }

    #[test]
    fn block_work_hands_out_at_most_its_limit() {
        let mut registry = Registry::default();
        let mut topic = 100;
        registry.apply_hook(&start(A, CWD, Some(10), None));
        settle(&mut registry, &mut topic);
        for agent in ["a1", "a2", "a3"] {
            registry.confirm_subagent(agent, A, format!("↳ Explore {agent}"));
        }
        assert_eq!(registry.block_work(2).len(), 2);
        assert_eq!(registry.block_work(2).len(), 1);
        assert!(registry.block_work(2).is_empty());
    }

    #[test]
    fn subagent_records_are_bounded_oldest_settled_first() {
        let mut registry = Registry::default();
        let mut topic = 100;
        let mut message = 500;
        registry.apply_hook(&start(A, CWD, Some(10), None));
        settle(&mut registry, &mut topic);
        let id = |i: usize| format!("a{i}");
        for i in 0..MAX_SUBAGENTS {
            assert!(registry.confirm_subagent(&id(i), A, format!("↳ Explore {}", id(i))));
        }
        settle_blocks(&mut registry, &mut message);
        // Every block still works: a new one is refused, none is dropped.
        assert!(!registry.confirm_subagent("new", A, "↳ Explore new".into()));
        // a5 and a3 finished (in that order of confirmation: a3 is older).
        for i in [5, 3] {
            let key = BlockKey::Agent(id(i));
            registry.show_block(&key, "done".into(), false);
        }
        settle_blocks(&mut registry, &mut message);
        assert!(registry.confirm_subagent("new", A, "↳ Explore new".into()));
        assert_eq!(registry.subagents.len(), MAX_SUBAGENTS);
        assert!(!registry.subagents.contains_key("a3"));
        assert!(registry.subagents.contains_key("a5"));
    }

    #[test]
    fn a_nested_answer_is_kept_in_the_registry_until_the_run_ends() {
        let dir = TempDir::new("registry-nested-answer");
        let store = RegistryStore::open(dir.path()).unwrap();
        let mut registry = Registry::default();
        registry.apply_hook(&start(A, CWD, Some(10), None));
        registry.apply_hook(&start(N, CWD, Some(20), Some(10)));
        assert!(!registry.set_nested_answer(A, "top-level answers are turn answers"));
        assert!(registry.set_nested_answer(N, "first"));
        assert!(registry.set_nested_answer(N, "last"));
        store.save(&RegistryStore::encode(&registry)).unwrap();
        let mut registry = store.load().unwrap();
        assert_eq!(registry.take_nested_answer(N).as_deref(), Some("last"));
        assert_eq!(registry.take_nested_answer(N), None);
        // A run lost with its parent keeps no answer.
        registry.set_nested_answer(N, "late");
        registry.lose_blocks(&[A.to_owned()]);
        assert_eq!(registry.take_nested_answer(N), None);
    }

    #[test]
    fn a_reply_finds_only_its_own_sessions_subagent_block() {
        let mut registry = Registry::default();
        let mut topic = 100;
        let mut message = 500;
        registry.apply_hook(&start(A, CWD, Some(10), None));
        settle(&mut registry, &mut topic);
        registry.confirm_subagent("a1", A, "↳ Explore a1".into());
        settle_blocks(&mut registry, &mut message);
        assert_eq!(registry.subagent_of_message(100, 500, A), Some("a1"));
        assert_eq!(registry.subagent_of_message(100, 500, B), None);
        assert_eq!(registry.subagent_of_message(101, 500, A), None);
        assert_eq!(registry.subagent_of_message(100, 501, A), None);
    }

    #[test]
    fn a_parent_pid_that_is_the_session_itself_is_nested_unknown_parent() {
        let mut registry = Registry::default();
        let mut topic = 100;
        registry.apply_hook(&start(A, CWD, Some(10), None));
        settle(&mut registry, &mut topic);
        let before = registry.sessions[A].clone();
        let slots_before = registry.slots.clone();
        // A second start of A whose next claude ancestor is A's own process.
        for (pid, parent) in [(20, 10), (10, 10)] {
            let outcome = registry.session_started(
                &start_from(A, r"C:\Elsewhere", Some(pid), Some(parent), "resume"),
                Some("resume"),
                Some(pid),
                Some(parent),
            );
            assert_eq!(outcome, SlotOrParent::Parent(None));
        }
        // No slot, and A's own record and slot are left alone.
        assert_eq!(registry.sessions[A].kind, SessionKind::TopLevel);
        assert_eq!(registry.sessions[A].slot, before.slot);
        assert_eq!(registry.sessions[A].claude_pid, Some(10));
        assert!(!registry.pids.contains_key("box/20"));
        assert_eq!(registry.slots, slots_before);
        let jobs = settle(&mut registry, &mut topic);
        assert_eq!(creates(&jobs), 0);
        assert!(separators(&jobs).is_empty());
        // A plain restart of A still finds its own slot.
        registry.apply_hook(&end(A));
        registry.apply_hook(&start_from(A, CWD, Some(30), None, "resume"));
        assert_eq!(registry.sessions[A].kind, SessionKind::TopLevel);
        assert_eq!(registry.sessions[A].slot, before.slot);
    }

    #[test]
    fn the_pid_of_a_cleared_process_names_the_new_session() {
        let mut registry = Registry::default();
        registry.apply_hook(&start(A, CWD, Some(10), None));
        assert_eq!(registry.live_session_of_pid("box", 10), Some(A));
        assert!(registry.is_live_top_level(A));
        registry.apply_hook(&clear_end(A));
        assert_eq!(registry.live_session_of_pid("box", 10), None);
        assert!(!registry.is_live_top_level(A));
        registry.apply_hook(&start_from(B, CWD, Some(10), None, "clear"));
        assert_eq!(registry.live_session_of_pid("box", 10), Some(B));
        assert_eq!(registry.live_session_of_pid("other", 10), None);
        // A nested run's own pid never names a channel session.
        registry.apply_hook(&start(N, CWD, Some(20), Some(10)));
        assert_eq!(registry.live_session_of_pid("box", 20), None);
        assert!(!registry.is_live_top_level(N));
    }

    #[test]
    fn the_agent_of_a_nested_run_is_never_bound() {
        let mut registry = Registry::default();
        registry.apply_hook(&start(A, CWD, Some(10), None));
        registry.apply_hook(&start(N, CWD, Some(20), Some(10)));
        assert!(!registry.agent_connected(N, 5));
        assert_eq!(registry.sessions[N].agent, None);
        assert!(registry.agent_connected(A, 6));
    }

    #[test]
    fn clear_in_one_process_stays_in_its_slot() {
        let mut registry = Registry::default();
        let mut topic = 100;
        registry.apply_hook(&start(A, CWD, Some(10), None)); // #1
        registry.apply_hook(&start(B, CWD, Some(11), None)); // #2
        settle(&mut registry, &mut topic);
        registry.apply_hook(&end(A)); // #1 free now
        // `/clear` in B's process: SessionStart of C arrives before B's SessionEnd.
        registry.apply_hook(&start_from(C, CWD, Some(11), None, "clear"));
        assert_eq!(slot_of(&registry, C), slot_of(&registry, B));
        assert!(registry.sessions[B].ended);
        // B's late SessionEnd hands nothing on: the pid is C's now.
        registry.apply_hook(&clear_end(B));
        assert!(registry.recent_clears.is_empty());
        let jobs = settle(&mut registry, &mut topic);
        assert_eq!(separators(&jobs), ["── session cccccccc · new ──"]);
    }

    #[test]
    fn clear_end_before_start_stays_in_its_slot() {
        let mut registry = Registry::default();
        let mut topic = 100;
        registry.apply_hook(&start(A, CWD, Some(10), None)); // #1
        registry.apply_hook(&start(B, CWD, Some(11), None)); // #2
        settle(&mut registry, &mut topic);
        registry.apply_hook(&end(A)); // #1 is the first free slot
        let b_slot = slot_of(&registry, B).unwrap();

        registry.apply_hook(&clear_end(B));
        registry.apply_hook(&start_from(C, CWD, Some(11), None, "clear"));

        assert_eq!(slot_of(&registry, C), Some(b_slot));
        let jobs = settle(&mut registry, &mut topic);
        assert_eq!(separators(&jobs), ["── session cccccccc · new ──"]);
    }

    #[test]
    fn a_clear_hand_off_is_only_for_a_clear_start_in_time() {
        let setup = |b_end: HookPost| {
            let mut registry = Registry::default();
            let mut topic = 100;
            registry.apply_hook(&start(A, CWD, Some(10), None)); // #1
            registry.apply_hook(&start(B, CWD, Some(11), None)); // #2
            settle(&mut registry, &mut topic);
            registry.apply_hook(&end(A));
            registry.apply_hook(&b_end);
            registry
        };
        let c_ordinal =
            |registry: &Registry| registry.slots[slot_of(registry, C).unwrap().0].ordinal;

        // A plain SessionEnd hands nothing on: the first free slot.
        let mut registry = setup(end(B));
        registry.apply_hook(&start_from(C, CWD, Some(11), None, "clear"));
        assert_eq!(c_ordinal(&registry), 1);

        // Not a clear start (the pid was reused): the first free slot.
        let mut registry = setup(clear_end(B));
        registry.apply_hook(&start_from(C, CWD, Some(11), None, "startup"));
        assert_eq!(c_ordinal(&registry), 1);

        // Expired.
        let mut registry = setup(clear_end(B));
        for (_, expires) in registry.recent_clears.values_mut() {
            *expires = Instant::now();
        }
        registry.apply_hook(&start_from(C, CWD, Some(11), None, "clear"));
        assert_eq!(c_ordinal(&registry), 1);

        // In time: its own slot.
        let mut registry = setup(clear_end(B));
        registry.apply_hook(&start_from(C, CWD, Some(11), None, "clear"));
        assert_eq!(c_ordinal(&registry), 2);
    }

    #[test]
    fn nested_start_of_a_known_top_level_does_not_take_its_slot() {
        let mut registry = Registry::default();
        let mut topic = 100;
        registry.apply_hook(&start(A, CWD, Some(10), None));
        registry.apply_hook(&start(B, r"C:\Work\Other", Some(11), None));
        settle(&mut registry, &mut topic);
        let b_slot = slot_of(&registry, B);
        registry.apply_hook(&end(B));
        settle(&mut registry, &mut topic);

        // `claude -p --resume B` from a Bash tool of A.
        let nested = registry.session_started(
            &start_from(B, r"C:\Work\Other", Some(20), Some(10), "resume"),
            Some("resume"),
            Some(20),
            Some(10),
        );
        assert_eq!(nested, SlotOrParent::Parent(slot_of(&registry, A)));
        assert_eq!(registry.sessions[B].kind, SessionKind::TopLevel);
        assert_eq!(slot_of(&registry, B), b_slot);
        assert!(
            settle(&mut registry, &mut topic).is_empty(),
            "no topic work"
        );
        registry.apply_hook(&end(B)); // the nested run ends

        registry.apply_hook(&start_from(B, r"C:\Work\Other", Some(30), None, "resume"));
        assert_eq!(registry.sessions[B].kind, SessionKind::TopLevel);
        assert_eq!(slot_of(&registry, B), b_slot);
    }

    #[test]
    fn the_end_of_a_nested_resume_of_a_live_session_does_not_end_it() {
        const OTHER: &str = r"C:\Work\Other";
        let mut registry = Registry::default();
        let mut topic = 100;
        registry.apply_hook(&start(A, CWD, Some(10), None));
        registry.apply_hook(&start(B, OTHER, Some(20), None));
        settle(&mut registry, &mut topic);
        let b_slot = slot_of(&registry, B).unwrap();

        // `claude -p --resume B` from a Bash tool of A, then its SessionEnd.
        registry.apply_hook(&start_from(B, OTHER, Some(30), Some(10), "resume"));
        registry.apply_hook(&end_by(B, Some(30)));
        assert!(!registry.sessions[B].ended);
        assert_ne!(registry.state(b_slot), SlotState::Dead);

        // B still runs: a new session in its folder gets `#2`.
        registry.apply_hook(&start(C, OTHER, Some(50), None));
        let c_slot = slot_of(&registry, C).unwrap();
        assert_ne!(c_slot, b_slot);
        assert_eq!(registry.slots[c_slot.0].ordinal, 2);

        // B's own end still ends it.
        registry.apply_hook(&end_by(B, Some(20)));
        assert!(registry.sessions[B].ended);
    }

    #[test]
    fn the_end_of_a_nested_resume_of_the_parent_itself_does_not_end_it() {
        let mut registry = Registry::default();
        let mut topic = 100;
        registry.apply_hook(&start(A, CWD, Some(10), None));
        settle(&mut registry, &mut topic);
        let a_slot = slot_of(&registry, A).unwrap();

        // `claude -p --resume A` from A's own Bash: the ancestor is A.
        registry.apply_hook(&start_from(A, CWD, Some(30), Some(10), "resume"));
        registry.apply_hook(&end_by(A, Some(30)));
        registry.apply_hook(&post(
            A,
            CWD,
            HookEvent::Stop {
                prompt_id: None,
                last_assistant_message: None,
            },
        ));
        assert!(!registry.sessions[A].ended);
        assert!(
            settle(&mut registry, &mut topic).is_empty(),
            "no icon change"
        );

        registry.apply_hook(&start(C, CWD, Some(50), None));
        assert_ne!(slot_of(&registry, C), Some(a_slot));
    }

    #[test]
    fn a_session_end_ends_a_session_whose_start_had_no_pid() {
        let mut registry = Registry::default();
        registry.apply_hook(&start(A, CWD, None, None));
        registry.apply_hook(&end_by(A, Some(10)));
        assert!(registry.sessions[A].ended);
    }

    #[test]
    fn a_reused_pid_after_a_lost_end_takes_the_first_free_slot() {
        let mut registry = Registry::default();
        let mut topic = 100;
        registry.apply_hook(&start(A, CWD, Some(1), None)); // #1
        registry.apply_hook(&start(B, CWD, Some(2), None)); // #2
        settle(&mut registry, &mut topic);
        let b_slot = slot_of(&registry, B).unwrap();
        registry.apply_hook(&end(A));
        // B crashed without a SessionEnd; Windows gives pid 2 to a new claude.
        registry.apply_hook(&start(C, CWD, Some(2), None));

        assert_eq!(registry.slots[slot_of(&registry, C).unwrap().0].ordinal, 1);
        assert!(registry.sessions[B].ended);
        assert_eq!(registry.state(b_slot), SlotState::Dead);
    }

    #[test]
    fn a_start_takes_the_slot_of_a_session_whose_process_died() {
        let mut registry = Registry::default();
        let mut topic = 100;
        registry.apply_hook(&start(A, CWD, Some(1), None));
        settle(&mut registry, &mut topic);
        let a_slot = slot_of(&registry, A).unwrap();
        registry.forget_recent_starts();
        // A's window was closed: no SessionEnd, pid 1 is gone.
        let followup = registry.apply_hook(&listing(start(C, CWD, Some(5), None), &[5, 9]));

        assert_eq!(followup.reaped, [A]);
        assert_eq!(followup.ended_sessions, [A]);
        assert!(registry.sessions[A].ended);
        assert!(!registry.pids.contains_key(&pid_key("box", 1)));
        assert_eq!(slot_of(&registry, C), Some(a_slot));
        assert_eq!(registry.slots[a_slot.0].ordinal, 1);
        assert_eq!(registry.slots.len(), 1, "no #2");
        assert_eq!(registry.state(a_slot), SlotState::NoChannel);
    }

    #[test]
    fn only_dead_pids_of_the_reporting_host_end_sessions() {
        const L: &str = "11111111-0000-4000-8000-000000000011";
        const R: &str = "22222222-0000-4000-8000-000000000022";
        const P: &str = "33333333-0000-4000-8000-000000000033";
        let mut registry = Registry::default();
        registry.apply_hook(&start(A, CWD, Some(1), None)); // dead
        registry.apply_hook(&start(L, CWD, Some(2), None)); // alive
        registry.apply_hook(&start(N, CWD, Some(3), Some(2))); // nested in L, dead
        registry.apply_hook(&start(P, CWD, None, None)); // no pid
        let mut far = start(R, CWD, Some(1), None); // pid 1 of another device
        far.host = "far".into();
        registry.apply_hook(&far);
        assert_eq!(
            registry.sessions[N].kind,
            SessionKind::Nested {
                parent: Some(L.into())
            }
        );
        registry.forget_recent_starts();
        let followup =
            registry.apply_hook(&listing(start(C, "/elsewhere", Some(5), None), &[2, 5]));

        let mut reaped = followup.reaped.clone();
        reaped.sort();
        assert_eq!(reaped, [A, N]);
        for (session, ended) in [
            (A, true),
            (N, true),
            (L, false),
            (P, false),
            (R, false),
            (C, false),
        ] {
            assert_eq!(registry.sessions[session].ended, ended, "{session}");
        }
        assert_eq!(
            registry.pids.get(&pid_key("far", 1)).map(String::as_str),
            Some(R)
        );
        assert_eq!(
            registry.pids.get(&pid_key("box", 2)).map(String::as_str),
            Some(L)
        );
    }

    #[test]
    fn a_start_within_the_grace_is_not_ended_by_an_older_list() {
        let mut registry = Registry::default();
        registry.apply_hook(&start(A, CWD, Some(1), None));
        // B's list was taken before A's process existed; A's start came first.
        let followup = registry.apply_hook(&listing(start(B, CWD, Some(2), None), &[2]));
        assert!(followup.reaped.is_empty());
        assert!(!registry.sessions[A].ended);
        assert_eq!(registry.slots[slot_of(&registry, B).unwrap().0].ordinal, 2);
    }

    #[test]
    fn a_list_ends_nothing_unless_it_shows_the_reporting_process() {
        let mut registry = Registry::default();
        registry.apply_hook(&start(A, CWD, Some(1), None));
        registry.forget_recent_starts();
        // No list, a list without the own pid, no own pid, a turn event.
        registry.apply_hook(&start(B, CWD, Some(2), None));
        registry.apply_hook(&listing(start(C, "/c", Some(3), None), &[2]));
        registry.apply_hook(&listing(start(N, "/n", None, None), &[2, 3]));
        let stop = post(
            B,
            CWD,
            HookEvent::Stop {
                prompt_id: None,
                last_assistant_message: None,
            },
        );
        registry.apply_hook(&listing(stop, &[2, 3]));
        assert!(!registry.sessions[A].ended);
        // A SessionEnd with a list does end it.
        registry.forget_recent_starts();
        let followup = registry.apply_hook(&listing(end_by(B, Some(2)), &[2, 3]));
        assert_eq!(followup.reaped, [A]);
        let mut ended = followup.ended_sessions.clone();
        ended.sort();
        assert_eq!(ended, [A, B]);
        assert!(!registry.sessions[C].ended);
    }

    #[test]
    fn a_resume_in_a_new_process_is_not_ended_by_its_own_list() {
        let mut registry = Registry::default();
        let mut topic = 100;
        registry.apply_hook(&start(A, CWD, Some(1), None));
        settle(&mut registry, &mut topic);
        let a_slot = slot_of(&registry, A).unwrap();
        registry.forget_recent_starts();
        // `claude --resume A` after A's process was killed.
        let followup =
            registry.apply_hook(&listing(start_from(A, CWD, Some(7), None, "resume"), &[7]));
        assert!(followup.reaped.is_empty());
        assert!(!registry.sessions[A].ended);
        assert_eq!(slot_of(&registry, A), Some(a_slot));
        assert_eq!(registry.sessions[A].claude_pid, Some(7));
    }

    #[test]
    fn rapid_session_changes_keep_only_the_latest_separator() {
        let mut registry = Registry::default();
        let mut topic = 100;
        registry.apply_hook(&start(A, CWD, Some(10), None));
        settle(&mut registry, &mut topic);
        registry.apply_hook(&end(A));
        registry.apply_hook(&start(B, CWD, Some(11), None));
        registry.apply_hook(&end(B));
        registry.apply_hook(&start(C, CWD, Some(12), None));

        let jobs = registry.topic_work(&Icons::default(), true);
        assert_eq!(separators(&jobs), ["── session cccccccc · new ──"]);
    }

    #[test]
    fn a_new_transcript_streams_from_its_start_and_a_resume_keeps_its_offset() {
        let mut registry = Registry::default();
        registry.apply_hook(&start("s1", "/w", Some(1), None));
        let offset = |registry: &Registry, id: &str| {
            registry.sessions[id]
                .stream
                .as_ref()
                .map(|stream| stream.offset)
        };
        assert_eq!(offset(&registry, "s1"), Some(Some(0)));
        registry
            .sessions
            .get_mut("s1")
            .unwrap()
            .stream
            .as_mut()
            .unwrap()
            .offset = Some(420);
        registry.apply_hook(&end("s1"));
        registry.apply_hook(&start_from("s1", "/w", Some(2), None, "resume"));
        assert_eq!(offset(&registry, "s1"), Some(Some(420)));
        // A resume the hub never saw starts at the end of the file.
        registry.apply_hook(&start_from("s2", "/w", Some(3), None, "resume"));
        assert_eq!(offset(&registry, "s2"), Some(None));
        // Nested runs are not streamed.
        registry.apply_hook(&start("s3", "/w", Some(4), Some(3)));
        assert_eq!(offset(&registry, "s3"), None);
        let bytes = RegistryStore::encode(&registry);
        let back: Registry = serde_json::from_slice(&bytes).unwrap();
        assert_eq!(offset(&back, "s1"), Some(Some(420)));
    }

    #[test]
    fn a_resumed_session_returns_to_its_slot() {
        let mut registry = Registry::default();
        let mut topic = 100;
        registry.apply_hook(&start(A, CWD, Some(10), None)); // #1
        registry.apply_hook(&start(B, CWD, Some(11), None)); // #2
        settle(&mut registry, &mut topic);
        registry.apply_hook(&end(A));
        registry.apply_hook(&end(B));
        registry.apply_hook(&start_from(B, CWD, Some(12), None, "resume"));
        assert_eq!(registry.slots[slot_of(&registry, B).unwrap().0].ordinal, 2);
        // Same session again in its slot: no separator.
        assert!(separators(&settle(&mut registry, &mut topic)).is_empty());
        registry.apply_hook(&end(B));
        registry.apply_hook(&start(C, CWD, Some(13), None));
        registry.apply_hook(&end(C));
        registry.apply_hook(&start_from(B, CWD, Some(14), None, "resume"));
        // C took #1; B's #2 is free, B goes back there.
        assert_eq!(registry.slots[slot_of(&registry, B).unwrap().0].ordinal, 2);
    }

    #[test]
    fn states_map_to_icons_from_the_list() {
        let icons = Icons::default();
        let mut registry = Registry::default();
        let mut topic = 100;
        registry.apply_hook(&start(A, CWD, Some(10), None));
        let id = slot_of(&registry, A).unwrap();
        assert_eq!(registry.state(id), SlotState::NoChannel);
        settle(&mut registry, &mut topic);
        assert!(registry.agent_connected(A, 7));
        assert_eq!(registry.state(id), SlotState::Alive);
        registry.set_waiting(A, true);
        assert_eq!(registry.state(id), SlotState::Waiting);
        registry.apply_hook(&post(
            A,
            CWD,
            HookEvent::Stop {
                prompt_id: None,
                last_assistant_message: None,
            },
        ));
        assert_eq!(registry.state(id), SlotState::Alive);
        registry.agent_disconnected(A, 6); // an older connection: ignored
        assert_eq!(registry.state(id), SlotState::Alive);
        registry.agent_disconnected(A, 7);
        assert_eq!(registry.state(id), SlotState::NoChannel);
        registry.apply_hook(&end(A));
        assert_eq!(registry.state(id), SlotState::Dead);
        let jobs = registry.topic_work(&icons, true);
        assert_eq!(
            jobs,
            [TopicJob::Edit {
                slot: id,
                thread_id: 100,
                name: None,
                icon: Some(ICON_DEAD.to_owned())
            }]
        );
    }

    #[test]
    fn icons_come_from_the_offered_set() {
        let preferred = [ICON_ALIVE, ICON_DEAD, ICON_WAITING, ICON_NO_CHANNEL].map(str::to_owned);
        let (icons, substituted) = Icons::from_offered(preferred.clone()).unwrap();
        assert_eq!(icons, Icons::default());
        assert!(substituted.is_empty());

        // The waiting and dead icons are gone: the smallest spare ids stand in.
        let offered = [
            ICON_ALIVE.to_owned(),
            ICON_NO_CHANNEL.to_owned(),
            "900".to_owned(),
            "800".to_owned(),
            "700".to_owned(),
            String::new(),
        ];
        let (icons, substituted) = Icons::from_offered(offered.clone()).unwrap();
        assert_eq!(substituted, ["dead", "waiting"]);
        assert_eq!(icons.alive.as_deref(), Some(ICON_ALIVE));
        assert_eq!(icons.dead.as_deref(), Some("700"));
        assert_eq!(icons.waiting.as_deref(), Some("800"));
        assert_eq!(icons.no_channel.as_deref(), Some(ICON_NO_CHANNEL));
        let chosen: HashSet<String> = [icons.alive, icons.dead, icons.waiting, icons.no_channel]
            .into_iter()
            .map(Option::unwrap)
            .collect();
        assert_eq!(chosen.len(), 4);
        assert!(chosen.iter().all(|id| offered.contains(id)));

        // Too few distinct usable ids: an error, never an unchecked id.
        assert_eq!(
            Icons::from_offered(["1", "2", "2", ""].map(str::to_owned)),
            Err(IconError::TooFew(2))
        );
        assert_eq!(Icons::from_offered(Vec::new()), Err(IconError::TooFew(0)));
    }

    #[test]
    fn a_gone_topic_is_replaced_exactly_once() {
        let mut registry = Registry::default();
        let mut topic = 100;
        registry.apply_hook(&start(A, CWD, Some(10), None));
        registry.apply_hook(&start(B, CWD, Some(11), None));
        settle(&mut registry, &mut topic); // topics 100 and 101
        let id = slot_of(&registry, A).unwrap();
        registry.topic_invalid(id, 100);
        registry.topic_invalid(id, 100); // a second failure report of the same topic
        let jobs = registry.topic_work(&Icons::default(), true);
        assert_eq!(creates(&jobs), 1);
        registry.topic_created(id, 200, "x", None);
        registry.topic_invalid(id, 100); // late report of the old topic
        assert_eq!(registry.slots[id.0].topic_id, Some(200));
        assert_eq!(creates(&registry.topic_work(&Icons::default(), true)), 0);
        // The other slot keeps its topic.
        assert_eq!(
            registry.slots[slot_of(&registry, B).unwrap().0].topic_id,
            Some(101)
        );
    }

    #[test]
    fn a_late_gone_report_during_a_replacement_changes_nothing() {
        let mut registry = Registry::default();
        let mut topic = 100;
        registry.apply_hook(&start(A, CWD, Some(10), None));
        settle(&mut registry, &mut topic);
        let id = slot_of(&registry, A).unwrap();
        // `/clear`: a separator and a title edit are both due.
        registry.apply_hook(&start_from(B, CWD, Some(10), None, "clear"));
        let jobs = registry.topic_work(&Icons::default(), true);
        assert_eq!(jobs.len(), 1, "one call per slot: {jobs:?}");
        assert_eq!(separators(&jobs).len(), 1);
        registry.topic_invalid(id, 100);
        let jobs = registry.topic_work(&Icons::default(), true);
        assert_eq!(creates(&jobs), 1);
        // Late reports about topic 100 while the replacement is in flight.
        registry.topic_invalid(id, 100);
        registry.topic_edited(id, 100, Some("x"), None);
        registry.topic_separated(id, 100, "x");
        assert!(registry.slots[id.0].busy);
        assert!(registry.topic_work(&Icons::default(), true).is_empty());
        registry.topic_created(id, 200, "y", None);
        let mut total = 1;
        for _ in 0..3 {
            total += creates(&settle(&mut registry, &mut topic));
        }
        assert_eq!(total, 1);
        assert_eq!(registry.slots[id.0].topic_id, Some(200));
    }

    #[test]
    fn a_separator_stays_pending_until_it_is_delivered() {
        let mut registry = Registry::default();
        let mut topic = 100;
        registry.apply_hook(&start(A, CWD, Some(10), None));
        settle(&mut registry, &mut topic);
        registry.apply_hook(&end(A));
        settle(&mut registry, &mut topic);
        registry.apply_hook(&start(B, CWD, Some(11), None));
        let id = slot_of(&registry, B).unwrap();
        let text = "── session bbbbbbbb · new ──";
        let jobs = registry.topic_work(&Icons::default(), true);
        assert_eq!(separators(&jobs), [text]);
        assert_eq!(jobs.len(), 1, "the edit waits for the separator: {jobs:?}");
        // In flight, it is still what a save writes.
        assert_eq!(
            registry.slots[id.0].pending_separator.as_deref(),
            Some(text)
        );
        assert!(registry.topic_work(&Icons::default(), true).is_empty());
        // Refused: kept, not repeated until the retry.
        registry.topic_failed(id, &Icons::default());
        assert_eq!(
            registry.slots[id.0].pending_separator.as_deref(),
            Some(text)
        );
        assert!(registry.topic_work(&Icons::default(), true).is_empty());
        registry.retry_failed();
        let jobs = registry.topic_work(&Icons::default(), true);
        assert_eq!(separators(&jobs), [text]);
        registry.topic_separated(id, 100, text);
        assert_eq!(registry.slots[id.0].pending_separator, None);
        let jobs = registry.topic_work(&Icons::default(), true);
        assert!(separators(&jobs).is_empty());
        assert!(
            matches!(&jobs[..], [TopicJob::Edit { name: Some(name), .. }] if name == "[box] Project · bbbbbbbb")
        );
    }

    #[test]
    fn a_failed_call_is_not_repeated_until_retry() {
        let mut registry = Registry::default();
        registry.apply_hook(&start(A, CWD, Some(10), None));
        let jobs = registry.topic_work(&Icons::default(), true);
        let TopicJob::Create { slot, .. } = jobs[0].clone() else {
            panic!("create expected");
        };
        assert!(
            registry.topic_work(&Icons::default(), true).is_empty(),
            "busy"
        );
        registry.topic_failed(slot, &Icons::default());
        assert!(registry.topic_work(&Icons::default(), true).is_empty());
        registry.retry_failed();
        assert_eq!(creates(&registry.topic_work(&Icons::default(), true)), 1);
    }

    #[test]
    fn edits_wait_for_the_grace_but_creations_do_not() {
        let mut registry = Registry::default();
        let mut topic = 100;
        registry.apply_hook(&start(A, CWD, Some(10), None));
        settle(&mut registry, &mut topic);
        registry.apply_hook(&end(A));
        assert!(registry.topic_work(&Icons::default(), false).is_empty());
        registry.apply_hook(&start(B, CWD, Some(11), None));
        registry.apply_hook(&start(C, CWD, Some(12), None));
        let jobs = registry.topic_work(&Icons::default(), false);
        // The separator for B and the topic for C, no edit.
        assert_eq!(separators(&jobs).len(), 1);
        assert_eq!(creates(&jobs), 1);
        assert_eq!(jobs.len(), 2);
    }

    #[test]
    fn a_hook_only_session_is_bound_by_its_agent_later() {
        let mut registry = Registry::default();
        let mut topic = 100;
        registry.apply_hook(&start(A, CWD, Some(10), None));
        let jobs = settle(&mut registry, &mut topic);
        assert!(
            matches!(&jobs[0], TopicJob::Create { icon, .. } if icon.as_deref() == Some(ICON_NO_CHANNEL))
        );
        assert!(registry.agent_connected(A, 1));
        let jobs = settle(&mut registry, &mut topic);
        assert_eq!(creates(&jobs), 0);
        assert_eq!(registry.slots.len(), 1);
        // An agent of an unknown session is not adopted.
        assert!(!registry.agent_connected(B, 2));
        assert!(!registry.sessions.contains_key(B));
        assert!(registry.topic_work(&Icons::default(), true).is_empty());
    }

    #[test]
    fn prompt_hooks_of_an_unknown_session_create_nothing() {
        let mut registry = Registry::default();
        for event in [
            HookEvent::UserPromptSubmit { prompt_id: None },
            HookEvent::Stop {
                prompt_id: None,
                last_assistant_message: None,
            },
        ] {
            let followup = registry.apply_hook(&post(A, CWD, event));
            assert_eq!(followup, Followup::default());
        }
        assert!(registry.sessions.is_empty());
        assert!(registry.slots.is_empty());
        assert!(registry.topic_work(&Icons::default(), true).is_empty());
        // Known and top-level: the prompt hook asks for the title once.
        registry.apply_hook(&start(A, CWD, Some(10), None));
        let followup = registry.apply_hook(&post(
            A,
            CWD,
            HookEvent::UserPromptSubmit { prompt_id: None },
        ));
        assert_eq!(
            followup.read_title,
            Some((A.to_owned(), format!("/t/{A}.jsonl")))
        );
        registry.set_title(A, "t");
        let followup = registry.apply_hook(&post(
            A,
            CWD,
            HookEvent::Stop {
                prompt_id: None,
                last_assistant_message: None,
            },
        ));
        assert_eq!(followup.read_title, None);
        // SessionEnd of an unknown session creates nothing.
        registry.apply_hook(&end(B));
        assert!(!registry.sessions.contains_key(B));
    }

    #[test]
    fn old_ended_sessions_are_pruned_but_current_ones_stay() {
        let mut registry = Registry::default();
        registry.apply_hook(&start(A, CWD, Some(1), None));
        registry.apply_hook(&end(A));
        for i in 0..MAX_SESSIONS + 5 {
            let session = format!("s{i:05}");
            registry.apply_hook(&start(&session, CWD, Some(1000 + i as u32), None));
            registry.apply_hook(&end(&session));
        }
        assert!(registry.sessions.len() <= MAX_SESSIONS);
        let current = registry.slots[0].current_session.clone().unwrap();
        assert!(registry.sessions.contains_key(&current));
        assert!(!registry.sessions.contains_key(A));
    }

    #[test]
    fn saves_are_atomic_and_round_trip() {
        let dir = TempDir::new("registry-store");
        let store = RegistryStore::open(dir.path()).unwrap();
        assert_eq!(store.load().unwrap(), Registry::default());
        let mut registry = Registry::default();
        let mut topic = 100;
        registry.apply_hook(&start(A, CWD, Some(10), None));
        settle(&mut registry, &mut topic);
        store.save(&RegistryStore::encode(&registry)).unwrap();

        // An interrupted save: the temp file was written (or half written),
        // the rename never happened. The previous file is what loads.
        std::fs::write(
            dir.path().join(TEMP_NAME),
            b"{\"version\":1,\"slots\":[{\"ho",
        )
        .unwrap();
        let mut loaded = store.load().unwrap();
        loaded.dirty = registry.dirty;
        assert!(loaded.recent_starts.is_empty(), "never saved");
        loaded.recent_starts.clone_from(&registry.recent_starts);
        assert_eq!(loaded, registry);
        assert_eq!(loaded.slots[0].topic_id, Some(100));

        store
            .save(&RegistryStore::encode(&Registry::default()))
            .unwrap();
        assert_eq!(store.load().unwrap(), Registry::default());
        assert!(!dir.path().join(TEMP_NAME).exists());
    }

    #[test]
    fn a_restart_forgets_connections_but_keeps_slots() {
        let dir = TempDir::new("registry-restart");
        let store = RegistryStore::open(dir.path()).unwrap();
        let mut registry = Registry::default();
        registry.apply_hook(&start(A, CWD, Some(10), None));
        registry.agent_connected(A, 3);
        registry.set_waiting(A, true);
        registry.slots[0].busy = true;
        store.save(&RegistryStore::encode(&registry)).unwrap();
        let loaded = store.load().unwrap();
        assert_eq!(loaded.sessions[A].agent, None);
        assert!(!loaded.sessions[A].waiting);
        assert!(!loaded.slots[0].busy);
        assert_eq!(loaded.state(SlotId(0)), SlotState::NoChannel);
    }

    #[test]
    fn a_slot_buffer_survives_a_restart_and_an_older_file_loads_without_one() {
        use crate::hub::buffer::{Parked, ResumeNote};
        let dir = TempDir::new("registry-buffer");
        let store = RegistryStore::open(dir.path()).unwrap();
        let mut registry = Registry::default();
        registry.apply_hook(&start(A, CWD, Some(10), None));
        registry.apply_hook(&start(B, CWD, Some(11), None));
        // An idle buffer is not written: the file looks like a TASK-011 one.
        let idle = String::from_utf8(RegistryStore::encode(&registry)).unwrap();
        assert!(!idle.contains("\"buffer\""), "{idle}");
        std::fs::write(dir.path().join(FILE_NAME), &idle).unwrap();
        assert!(
            store
                .load()
                .unwrap()
                .slots
                .iter()
                .all(|slot| slot.buffer.is_idle())
        );

        let buffer = &mut registry.slots[0].buffer;
        buffer.push(Parked {
            message_id: 5,
            thread_id: 100,
            text: "kept".into(),
            reply_to: Some(4),
            quote: None,
            forwarded: false,
        });
        buffer.overflow_told = true;
        buffer.resume = Some(ResumeNote {
            session: A.into(),
            number: 1,
            message_id: Some(900),
        });
        store.save(&RegistryStore::encode(&registry)).unwrap();
        let loaded = store.load().unwrap();
        assert_eq!(loaded.slots[0].buffer, registry.slots[0].buffer);
        assert!(loaded.slots[1].buffer.is_idle());
    }

    #[test]
    fn a_broken_or_foreign_file_refuses_to_load_without_quoting_it() {
        let dir = TempDir::new("registry-broken");
        let store = RegistryStore::open(dir.path()).unwrap();
        let secret_path = r"C:\Users\private-name\dev";
        std::fs::write(
            dir.path().join(FILE_NAME),
            format!(
                "{{\"version\":1,\"slots\":\"{}",
                secret_path.replace('\\', "\\\\")
            ),
        )
        .unwrap();
        let error = store.load().unwrap_err();
        assert!(matches!(error, LoadError::Invalid));
        assert!(!error.to_string().contains("private-name"));
        std::fs::write(dir.path().join(FILE_NAME), b"{\"version\":2}").unwrap();
        assert!(matches!(store.load(), Err(LoadError::Version(2))));
        std::fs::write(
            dir.path().join(FILE_NAME),
            b"{\"version\":1,\"sessions\":{\"s\":{\"host\":\"h\",\"kind\":{\"kind\":\"top_level\"},\"slot\":3}}}",
        )
        .unwrap();
        assert!(matches!(store.load(), Err(LoadError::Invalid)));
    }

    #[test]
    fn duplicate_topic_ids_and_slot_identities_refuse_to_load() {
        let dir = TempDir::new("registry-duplicates");
        let store = RegistryStore::open(dir.path()).unwrap();
        let mut registry = Registry::default();
        registry.apply_hook(&start(A, CWD, Some(10), None));
        registry.apply_hook(&start(B, CWD, Some(11), None));
        registry.slots[0].topic_id = Some(100);
        registry.slots[1].topic_id = Some(100);
        store.save(&RegistryStore::encode(&registry)).unwrap();
        assert!(matches!(store.load(), Err(LoadError::Invalid)));

        registry.slots[1].topic_id = Some(101);
        registry.slots[1].ordinal = registry.slots[0].ordinal;
        store.save(&RegistryStore::encode(&registry)).unwrap();
        assert!(matches!(store.load(), Err(LoadError::Invalid)));
    }
}
