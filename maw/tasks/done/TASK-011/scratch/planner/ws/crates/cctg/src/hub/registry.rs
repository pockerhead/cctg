//! Slot registry: which forum topic belongs to which `(device, folder,
//! ordinal)` slot and which session currently lives in it.
//!
//! Pure state and decisions, no IO: the `slots` actor feeds it events and
//! turns [`Registry::topic_work`] into Bot API calls. A slot owns its topic
//! forever; sessions succeed each other inside it. Nested runs and subagents
//! never get a slot, they only point at their parent's.
//!
//! Paths, folder names and titles are private: nothing here logs them.

use std::collections::{BTreeMap, HashSet};
use std::io::{self, Write};
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use transcript::telegram_len;

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

const FILE_NAME: &str = "registry.json";
const TEMP_NAME: &str = "registry.json.tmp";

/// Default icons, from `getForumTopicIconStickers` (2026-09-23, 112 stickers).
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

    /// Drops every id Telegram does not offer; returns the names of the
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
fn cut(text: &str, limit: usize) -> String {
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
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SubagentEntry {
    pub parent_session: String,
    #[serde(default)]
    pub slot: Option<SlotId>,
}

/// A topic call the actor should make. The slot is marked busy until the
/// matching `topic_*` result comes back.
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
    /// already holds; its previous slot if free; the slot of the previous
    /// session of the same claude process (`/clear`), which that session
    /// gives up; the first free slot of the folder; a new ordinal.
    fn allocate(
        &mut self,
        session: &str,
        host: &str,
        cwd: &str,
        claude_pid: Option<u32>,
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
        }
        let previous = claude_pid
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
        let kind = match self.sessions.get(session) {
            // A known session keeps what it was (a resume has no ancestor info
            // worth more than the first start).
            Some(entry) if parent_pid.is_none() => entry.kind.clone(),
            _ => self.nesting(&post.host, session, parent_pid),
        };
        let slot = match &kind {
            SessionKind::TopLevel => {
                let id = self.allocate(session, &post.host, &post.cwd, claude_pid);
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

    /// Handles one hook event.
    pub fn apply_hook(&mut self, post: &HookPost) -> Followup {
        let session = post.session_id.as_str();
        match &post.event {
            HookEvent::SessionStart {
                source,
                claude_pid,
                parent_claude_pid,
            } => {
                self.session_started(post, source.as_deref(), *claude_pid, *parent_claude_pid);
                Followup::default()
            }
            HookEvent::SessionEnd { .. } => {
                let Some(entry) = self.sessions.get_mut(session) else {
                    return Followup::default();
                };
                entry.ended = true;
                entry.waiting = false;
                if let Some(pid) = entry.claude_pid {
                    let key = pid_key(&entry.host, pid);
                    if self.pids.get(&key).map(String::as_str) == Some(session) {
                        self.pids.remove(&key);
                    }
                }
                self.touch();
                Followup::default()
            }
            HookEvent::UserPromptSubmit { .. } | HookEvent::Stop { .. } => {
                if !self.sessions.contains_key(session) {
                    // Its SessionStart was lost (hub down at the time).
                    self.session_started(post, None, None, None);
                }
                let entry = self.sessions.get_mut(session).expect("registered above");
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
                }
            }
            HookEvent::SubagentStart {
                agent_id,
                agent_type,
            }
            | HookEvent::SubagentStop {
                agent_id,
                agent_type,
                ..
            } => {
                // Claude Code's own internal agents have no type.
                if !agent_type.is_empty() {
                    let slot = self.sessions.get(session).and_then(|entry| entry.slot);
                    let entry = SubagentEntry {
                        parent_session: session.to_owned(),
                        slot,
                    };
                    if self.subagents.get(agent_id) != Some(&entry) {
                        self.subagents.insert(agent_id.clone(), entry);
                        self.dirty = true;
                    }
                }
                Followup::default()
            }
            HookEvent::SubagentHandback { .. } => Followup::default(),
        }
    }

    /// An agent registered. `false`: the session is unknown; the caller waits
    /// for its hook for a while and then calls [`Registry::adopt`].
    pub fn agent_connected(&mut self, session: &str, conn: u64) -> bool {
        match self.sessions.get_mut(session) {
            Some(entry) => {
                entry.agent = Some(conn);
                true
            }
            None => false,
        }
    }

    /// A session known only from its agent: taken as top-level.
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
            if let Some(text) = slot.pending_separator.take() {
                self.dirty = true;
                jobs.push(TopicJob::Separator {
                    slot: id,
                    thread_id,
                    text,
                });
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

    /// Records a successful (or not-modified) edit of `thread_id`.
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
        slot.failed = None;
        if let Some(name) = name {
            slot.applied_title = Some(name.to_owned());
        }
        if let Some(icon) = icon {
            slot.applied_icon = Some(icon.to_owned());
        }
        self.dirty = true;
    }

    /// A create or edit failed for another reason than a gone topic: the
    /// current name and icon are not tried again until they change or
    /// [`Registry::retry_failed`] runs.
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
    /// exactly one replacement.
    pub fn topic_invalid(&mut self, id: SlotId, thread_id: i64) {
        let slot = &mut self.slots[id.0];
        slot.busy = false;
        if slot.topic_id == Some(thread_id) {
            slot.topic_id = None;
            slot.applied_title = None;
            slot.applied_icon = None;
            slot.pending_separator = None;
            slot.failed = None;
            self.dirty = true;
        }
    }

    /// Releases a slot whose edit came back without a usable answer, keeping
    /// what is applied.
    pub fn release(&mut self, id: SlotId) {
        self.slots[id.0].busy = false;
    }

    pub fn retry_failed(&mut self) {
        for slot in &mut self.slots {
            slot.failed = None;
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
        if registry.sessions.values().any(|entry| bad_slot(entry.slot))
            || registry
                .subagents
                .values()
                .any(|agent| bad_slot(agent.slot))
        {
            return Err(LoadError::Invalid);
        }
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
        post(session, CWD, HookEvent::SessionEnd { reason: None })
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
                TopicJob::Separator { .. } => {}
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
        // Subagents: typed ones are recorded against the parent's slot.
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
        assert_eq!(
            registry.subagents.get("a1"),
            Some(&SubagentEntry {
                parent_session: A.to_owned(),
                slot: parent_slot
            })
        );
        assert!(!registry.subagents.contains_key("a2"));
        assert_eq!(creates(&settle(&mut registry, &mut topic)), 0);
        assert_eq!(registry.slots.len(), 1);
        // The parent's slot is still free for nobody else: nested runs do not hold it.
        assert_eq!(registry.slots[0].current_session.as_deref(), Some(A));
    }

    #[test]
    fn a_stale_parent_pid_pointing_at_itself_is_top_level() {
        let mut registry = Registry::default();
        registry.apply_hook(&start(A, CWD, Some(10), None));
        registry.apply_hook(&start_from(A, CWD, Some(10), Some(10), "resume"));
        assert_eq!(registry.sessions[A].kind, SessionKind::TopLevel);
        assert_eq!(registry.slots.len(), 1);
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
        let jobs = settle(&mut registry, &mut topic);
        assert_eq!(separators(&jobs), ["── session cccccccc · new ──"]);
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
        let offered: HashSet<String> = [ICON_ALIVE, ICON_DEAD, ICON_WAITING, ICON_NO_CHANNEL]
            .map(str::to_owned)
            .into();
        let mut checked = Icons::default();
        assert!(checked.keep_valid(&offered).is_empty());
        let mut partial: HashSet<String> = offered.clone();
        partial.remove(ICON_WAITING);
        assert_eq!(checked.keep_valid(&partial), ["waiting"]);
        assert_eq!(checked.waiting, None);
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
        // An agent of an unknown session is adopted as top-level.
        assert!(!registry.agent_connected(B, 2));
        let id = registry.adopt(B, "box", CWD);
        assert!(registry.agent_connected(B, 2));
        assert_eq!(registry.slots[id.0].ordinal, 2);
    }

    #[test]
    fn prompt_hooks_of_an_unknown_session_register_it_and_ask_for_a_title() {
        let mut registry = Registry::default();
        let followup = registry.apply_hook(&post(
            A,
            CWD,
            HookEvent::UserPromptSubmit { prompt_id: None },
        ));
        assert!(slot_of(&registry, A).is_some());
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
}
