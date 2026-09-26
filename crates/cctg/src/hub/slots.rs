//! Slot actor: the one owner of the [`Registry`].
//!
//! It drains agent and hook ingress and forum service messages, updates the
//! registry and turns [`Registry::topic_work`] into scheduler jobs. It never
//! awaits anything but its inputs: jobs go to a dispatch task over an
//! unbounded channel (that task waits for room in the scheduler queue), and
//! every answer comes back as a [`Done`] message, so ingress keeps draining
//! while Telegram is slow or the scheduler honours a `retry_after`. The
//! registry keeps at most one topic call per slot in flight, so the dispatch
//! queue holds at most one topic job per slot plus service-message deletes.
//! Saving goes to a separate task that writes the latest snapshot.
//!
//! Topic messages from allowlisted users go to the agent of the slot's
//! current session with a non-blocking `try_send`. A message no live agent
//! can take waits in its slot (see [`buffer`], kept in `registry.json`) and
//! goes, in order, to the first live top-level session of the slot whose
//! agent is bound; a dead slot with waiting messages shows one Resume button.
//! Agent replies and the final answer of each turn (from the `Stop` hook) go
//! back to the session's topic through the same dispatch task. At most
//! [`MAX_QUEUED_MESSAGES`] such messages wait for Telegram at a time, and a
//! slot gets the text-only notice at most once per `Options::notice_every`.
//!
//! Permission requests become prompts with Allow/Deny buttons in the topic of
//! the requesting session's own slot (see [`permissions`]). They bypass the
//! message cap and ride the scheduler's permission lane. The first press
//! fixes the answer; the verdict goes only to an agent of the requesting
//! session, and again after a link drop until that agent acknowledges it.
//! Later presses only get "already decided". The end of the session closes
//! its open prompts; every ended prompt loses its buttons, retried on the
//! tick.
//!
//! A `PermissionRequest` hook asks too (see [`Slots::permission_asks`]):
//! Claude Code does not relay every dialog through the channel. The ask waits
//! [`TWIN_WINDOW`] for the channel request of the same session and tool (it
//! may also have come just before); with one, the hook gets no decision and
//! the channel's prompt stays the only one. Without one, the ask becomes a
//! prompt of its own whose first press goes straight back to the waiting
//! hook. It gets no decision when [`HOOK_ANSWER_WAIT`] runs out, the hook
//! goes away, the session ends or the hub stops; its buttons go away then.
//!
//! A question of Claude's `AskUserQuestion` (TASK-038, see
//! [`Slots::question_asks`] and [`questions`]) comes from its `PreToolUse`
//! hook: one message in the topic of the session's slot with a button per
//! option, ✏️ Другое and ⌨ В терминале. Answers of every question go back
//! to the waiting hook; ⌨, [`QUESTION_WAIT`], a hook that goes away, the end
//! of the session or the hub's stop give it no decision (the terminal dialog
//! opens). After ✏️ Другое the next text in the topic, and at any time a
//! reply to the question message, is the answer to the current question and
//! does not go to the session. The channel's `permission_request` and the
//! `PermissionRequest` hook of `AskUserQuestion` get no Allow/Deny buttons.
//!
//! Subagents and nested runs get no topic: each gets one collapsed block
//! message in the topic of its parent's slot (see [`subagents`]). A typed
//! subagent hook opens a block only once the parent's transcript shows the
//! `Agent` call that launched it; the block is edited to its result on
//! `SubagentStop`. A nested `claude -p` gets one `⇣ nested` block. A reply to
//! a subagent block reaches the parent's agent with meta `target_agent`.
//!
//! Session files (TASK-034): the hub reads none. The agent of a session
//! reads them on request (`session_read`, see [`crate::reads`]): the
//! ai-title for the topic name, the parent transcript's `Agent` calls, a
//! finished subagent's files, and `/brief`/`/full` for the command worker
//! ([`commands::TranscriptAsk`]). A read has [`Options::read_wait`] for an
//! answer (each piece of a text starts the wait again) and fails when its
//! agent's link closes or the agent leaves for an update. A session with no
//! agent that reads (ended, headless, nested, an agent too old) keeps its
//! short-id title, opens a subagent block only on its stop, from the hook
//! data alone, and `/brief` answers why it cannot show the transcript.
//!
//! The live transcript stream (see [`stream`]): the agent of the slot's
//! current session reads its transcript on request and the actor turns the
//! events into topic messages (terminal prompts, text before tool calls, one
//! line per finished tool call) on the stream lane of the scheduler, after the
//! session separator. A turn's answer waits for the lines read after its
//! `Stop` (bounded by `Options::hold_answer`). A message handed to an agent
//! gets 👀, and ✍ once its own channel record shows up in the transcript.
//!
//! The status message (see [`status`], TASK-029): each slot with a topic gets
//! one message, pinned once, that says what its current session does (from
//! prompts, `Stop`, the tool hooks, tool results and interrupt notes in the
//! stream, permission prompts and the session's end) and shows the numbers
//! of its status line. It is edited at most once per `Options::status_every`,
//! except right after a button press. These periodic edits are background
//! edits for the scheduler (TASK-054): they take only the edit budget other
//! edits leave, round-robin over the slots. An edit that shows a ⏹ press is
//! foreground, may go while a refresh of the message still waits (the
//! scheduler lets it take that refresh's place), and the ⏹ question gets its
//! whole wait from when Telegram shows it. Its ⏹ button asks the session's agent
//! to write Esc into the claude console; only an agent that announced
//! `console_keys` for the live current session of that very slot is asked,
//! never while a permission prompt of that session waits (Esc would answer
//! the prompt), and a written Esc is shown as sent, not as the turn's end.
//! A compaction (TASK-053: the `PreCompact` hook, ended by the session's
//! `SessionStart` with `source: compact`) shows in the status message with
//! its minutes and gives the topic two silent lines: one when it begins, one
//! when it is done, with the context percentages before and after when the
//! status line sends the new one within [`COMPACT_NUMBERS_WAIT`]. One that
//! does not end (the session ends, [`COMPACT_MAX`] passes) leaves the status
//! with no line. A cancelled or failed one never sends `SessionStart`: a
//! prompt, a tool start, a `Stop` or a written Esc of that session after the
//! `PreCompact` ends its status at once, and a late `SessionStart(compact)`
//! within [`COMPACT_GRACE`] after that still gives the done line (the hooks
//! are separate POSTs and can arrive in either order).
//!
//! Client updates (TASK-040): an agent registers with its cctg build. A
//! live current session whose agent runs another build than the hub (or is
//! too old to say) is outdated: its topic gets one warning per hub build with
//! ⬆️ Обновить, and its status message a line and the same button. Nothing
//! updates without a press. A press (allowlisted, like every callback) asks
//! the agent with `update` once no turn runs; an agent that hands over or
//! restarts claude leaves (it is unbound at once and gets `released` behind
//! what was queued for it, later messages wait in the slot), and the next
//! agent of the session is asked again, at most [`UPDATE_ROUNDS`] times
//! within [`UPDATE_WAIT`]; the last answer ends in one notice. A press never
//! expires while a turn runs (the topic is told once), and a restart whose
//! answer finds a turn begun meanwhile is not released: no `/exit` goes into
//! a turn; the press is asked again after it. Likewise no `/exit` goes in
//! while the terminal shows background agents or the agent view (TASK-047):
//! the agent answers `agents_running`, the topic is told once and the press
//! is asked again every [`UPDATE_RETRY`]. The hub's own view counts too: a
//! session with a subagent whose `SubagentStart` came and `SubagentStop` did
//! not (and that is still a candidate or has a block, at most
//! [`AGENT_MAX_AGE`]) gets no `update` at all, with the same notice; the
//! press goes on after the last stop. A restart that cuts off work (a turn stopped by ⏹ while the press
//! waited, or still running, or subagents the hub sees running) is followed
//! by one channel message into the session once its next agent is bound
//! ([`continue_text`], with the agent ids of those subagents).
//!
//! Console commands (TASK-043, see [`console`]): a topic message that starts
//! with `!` or with a slash command the hub does not serve goes, instead of
//! the slot's buffer, to the agent of the slot's live session to be typed
//! into its claude console, only when that agent announced
//! `console_commands` and no turn runs and no permission prompt waits;
//! otherwise, and when the agent answers that a draft was in the way or
//! typing failed, the message gets a short answer. A typed command gets 👀.
//!
//! Files (TASK-032): a topic message with a file waits in its slot like any
//! other, as a reference to the Telegram file. When the slot's live agent
//! can take files, the download task ([`fetch`], see [`Slots::fetch_files`])
//! fetches it and hands it to the agent in chunks; the slot's later
//! messages wait behind it, and it leaves the slot once all its chunks are
//! in the link queue (a closed link keeps it for the next agent). A file
//! over the Bot API's 20 MB, a failed download, or an agent too old for
//! files each give the topic a notice (the old agent still gets the
//! caption). The other way, `file_offer` from the agent of a slot's live
//! current session is accepted while less than [`MAX_FILE_BYTES`] of
//! files wait; its chunks are put together here and the file goes to the
//! topic as a photo (a JPEG, PNG or WebP of at most 10 MB) or a document,
//! and the agent learns what Telegram did.
//!
//! Bursts (TASK-048): Telegram hands a forward of several messages and the
//! user's note on it as separate messages within a fraction of a second. A
//! text message for a live session waits in the slot until no new one came
//! for `Options::gather_quiet`, at most `Options::gather_max` after the
//! first, and the waiting texts go as one inbound, in order, each marked as
//! it would be alone, [`buffer::PART_SEPARATOR`] between them. Every one
//! gets 👀, and all turn ✍ with the channel record of the last one, whose id
//! the inbound's `message_id` carries (`message_ids` lists them all). One
//! inbound has one addressee and one reply target: a burst is cut where the
//! explicit reply changes (to a subagent's block, to another message, or
//! none). A file or a console command ends the burst: the burst goes first,
//! then the file; a command is refused as busy while messages wait in the
//! slot or texts went less than `Options::inbound_settle` ago. Messages kept
//! while the slot had no live session, or left behind by a full link queue
//! before the burst began, go one by one as before, without waiting for it.
//!
//! Channel off (TASK-052): Claude Code drops channel messages silently when
//! it was started without the channel flag or its dialog was not confirmed.
//! Texts handed to a streamed session while no turn runs expect a sign that
//! Claude took them: a channel record in the stream, a turn in the stream or
//! a `UserPromptSubmit`, `PreToolUse` or `Stop` hook. When the stream has
//! read the transcript to its end `Options::channel_wait` after the hand-over
//! and none came, the topic gets [`CHANNEL_OFF_NOTICE`], once per session
//! until one of its channel records shows up.
//!
//! Logs carry short session ids, slot ordinals and fixed text; never a path,
//! a folder, a title, message text, a file name or a caption.

use std::collections::{BTreeMap, HashMap, HashSet, VecDeque};
use std::sync::Arc;
use std::time::{Duration, Instant as StdInstant};

use tokio::sync::{mpsc, oneshot, watch};
use tokio::task::JoinHandle;
use tokio::time::{Instant, sleep_until};
use tracing::{debug, info, warn};
use transcript::{
    HtmlChunk, SplitOptions, escape_html, split_for_telegram, split_markdown_for_telegram,
};

use super::api::{ApiError, Document};
use super::buffer::{self, Attachment, Parked, ResumeNote};
use super::commands::{self, Prepared, TranscriptAsk, Unavailable};
use super::console;
use super::fetch::{self, Fetch, Fetched};
use super::ingress::{AgentEvent, MAX_PERMISSION_WAITS, PermissionAsk, QuestionAsk};
use super::permissions::{self, Edit, Opened, Prompt, Prompts, State};
use super::questions::{self, Asks};
use super::registry::{
    BlockJob, BlockKey, Icons, Registry, RegistryStore, SessionKind, SlotId, SlotState,
    StatusMessage, TopicJob, cut,
};
use super::scheduler::{Delivery, Op, Outbox, Outcome};
use super::status::{self, Activity, Buttons, Press};
use super::stream::{self, Format, Held, Live, Step};
use super::subagents::{
    self, AgentCall, AgentIndex, BodyInput, Candidates, Reports, Scan, Stopped,
};
use super::updates::{CallbackInput, Inbound};
use crate::channel::is_request_id;
use crate::files;
use crate::wire::{
    AgentMsg, Answered, Behavior, Client, CommandOutcome, ConsoleKey, FileChunk, FileOutcome,
    FilePart, HookEvent, HookPost, HubMsg, MAX_ALBUM, PermissionPost, PermissionRequest,
    QUESTION_TOOL, SessionAnswer, SessionAsk, StreamItem, StreamLine, UpdateOutcome,
};

/// The longest text one session read may bring, as the agent sends at most
/// ([`crate::reads::MAX_TEXT`]); an agent that sends more is cut off.
const MAX_READ_TEXT: usize = crate::reads::MAX_TEXT;
/// A session read the agent has not answered by then has failed.
pub const READ_WAIT: Duration = Duration::from_secs(20);
const SAVE_RETRY_WAITS: [Duration; 2] = [Duration::from_millis(100), Duration::from_millis(500)];
const SHORT_ID: usize = 8;
/// Reply and turn-answer chunks and notices waiting for Telegram; beyond this
/// a new reply, answer or notice is dropped whole (the group allows ~20 messages a minute anyway).
pub const MAX_QUEUED_MESSAGES: usize = 256;
/// Block sends and edits handed to the dispatch task and not answered yet;
/// the rest wait in the registry.
pub const MAX_BLOCK_JOBS: usize = 16;
/// Subagent block texts asked of agents at a time.
const MAX_BODY_READS: usize = 2;
/// A block text whose read was cut by a lost link or a late answer waits
/// this long for the parent's next agent (a TASK-040 swap), then shows the
/// hook data alone.
const BODY_RETRY_WAIT: Duration = Duration::from_secs(60);
/// While a block text waits for the next agent, the actor looks this often.
const BODY_RETRY_CHECK: Duration = Duration::from_secs(1);
/// How soon an agent whose `bound` found its queue full is told again.
const BOUND_RETRY: Duration = Duration::from_secs(1);
/// Title reads of one agent that may fail in a row before its session's
/// title is no longer asked of it (no transcript to find, a refusing
/// agent); the next agent of the session is asked again.
const MAX_TITLE_FAILURES: u32 = 4;
/// A transcript read the agent has not answered by then is asked again.
const READ_TIMEOUT: Duration = Duration::from_secs(10);
/// The stream takes no new transcript line while this many messages of any
/// kind wait for Telegram: replies, turn answers and notices keep the rest of
/// [`MAX_QUEUED_MESSAGES`].
const STREAM_QUEUE: usize = MAX_QUEUED_MESSAGES / 2;
/// Reactions waiting for Telegram at a time; one more is skipped.
const MAX_REACTIONS: usize = 64;
/// How far apart a `PermissionRequest` hook and the channel request of the
/// same session and tool may arrive, either way, to count as one request.
pub const TWIN_WINDOW: Duration = Duration::from_millis(1500);
/// A hook prompt nobody answered by then gives its hook no decision, before
/// Claude Code's own ~1:44 automatic refusal and the hook's 100 s timeout.
pub const HOOK_ANSWER_WAIT: Duration = Duration::from_secs(90);
/// While hooks wait, the actor looks this often whether one went away.
const HOOK_CHECK_EVERY: Duration = Duration::from_secs(1);
/// A question nobody answered by then goes to the terminal: its hook gets
/// no decision, under the hook's own 310 s and the 330 s the settings give it.
pub const QUESTION_WAIT: Duration = Duration::from_secs(300);
/// A question's `PermissionRequest` hook this soon after a question hook of
/// its session follows that hook's "no decision" (the settings give the
/// question hook 330 s); later, the session's client has no question hook.
const QUESTION_HOOK_WINDOW: Duration = Duration::from_secs(340);
/// Channel requests remembered for a hook that comes after them.
const MAX_RELAYED: usize = 64;
/// Topic messages held while a question's message id is not known yet
/// (see [`Slots::hold`]); beyond this they go on at once.
const MAX_HELD: usize = 64;
/// Bytes of files from sessions held at a time, being received or waiting
/// for Telegram (one file of Telegram's largest size); an offer beyond gets
/// `busy`.
pub const MAX_FILE_BYTES: u64 = files::MAX_UPLOAD;
/// An accepted file from an agent that got no chunk for this long is
/// dropped at the next offer: the agent gave up waiting for `accepted`, or
/// the offer came late over a new link.
pub const UPLOAD_IDLE: Duration = Duration::from_secs(60);
/// Files of kept messages waiting for the download task.
const FETCH_QUEUE: usize = 64;
/// Hand-overs of one kept file cut by a closing agent link before it is
/// dropped with [`buffer::LINK_LOST_NOTICE`] (a link too slow for a chunk
/// line would otherwise fetch and cut it again forever).
const MAX_LINK_LOSSES: u32 = 3;
/// Telegram shows at most this much of a caption.
const CAPTION_LIMIT: usize = 1024;
/// A status message is edited at most this often (edits have no published
/// limit, but a busy session changes every second).
pub const STATUS_EVERY: Duration = Duration::from_secs(5);
/// A compaction that has not ended after this is forgotten (TASK-053).
pub const COMPACT_MAX: Duration = Duration::from_secs(15 * 60);
/// The line of an ended compaction waits this long for the status line's
/// new context percentage, then goes without the percentages.
pub const COMPACT_NUMBERS_WAIT: Duration = Duration::from_secs(10);
/// After the session showed activity while a compaction ran, its
/// `SessionStart(compact)` is still taken this long (TASK-053).
pub const COMPACT_GRACE: Duration = Duration::from_secs(30);
/// Agents one update press asks at most: the first, the one after a
/// hand-over, the one after a claude restart.
pub const UPDATE_ROUNDS: u8 = 3;
/// An update press is forgotten this long after it came.
pub const UPDATE_WAIT: Duration = Duration::from_secs(120);
/// A restart held back by background agents or the agent view on the
/// terminal (TASK-047) is asked again after this.
pub const UPDATE_RETRY: Duration = Duration::from_secs(30);
/// The channel message a session gets from the hub once its next agent is
/// bound, after a client restart cut off its work (TASK-047).
pub const CONTINUE_TEXT: &str = "Клиент cctg обновлён, и сессия была перезапущена посреди работы. \
Продолжи с того места, где остановился.";
/// Follows [`CONTINUE_TEXT`] when the restart stopped background subagents,
/// before their agent ids. Probe TASK-047 (2.1.282): after `--resume` such
/// an agent goes on from its transcript on a `SendMessage` to its agentId;
/// by its name it is not reachable.
pub const CONTINUE_AGENTS_TEXT: &str = "Перезапуск остановил фоновых субагентов с agentId:";
/// Ends the continuation after the agent ids.
pub const CONTINUE_AGENTS_HOW: &str = "Возобнови каждого через SendMessage, указав в to его agentId \
(не имя: по имени агент недоступен); он продолжит по своему транскрипту с места остановки.";
/// A subagent that started and did not stop holds updates back at most this
/// long: a lost `SubagentStop` must not hold them forever (TASK-047).
pub const AGENT_MAX_AGE: Duration = Duration::from_secs(6 * 60 * 60);
/// A burst of topic messages for a live session goes once no new one came
/// for this long (TASK-048).
pub const GATHER_QUIET: Duration = Duration::from_secs(1);
/// A burst goes at most this long after its first message, however steady
/// the stream.
pub const GATHER_MAX: Duration = Duration::from_secs(3);
/// Content of one burst inbound at most (the first message always goes);
/// the rest of the burst follows in the next one. A link line holds
/// [`crate::wire::MAX_LINE`], and a JSON escape takes up to 6 bytes per byte.
const MAX_GATHER_BYTES: usize = 128 * 1024;
/// A console command for a session that got topic texts less than this long
/// ago is refused as busy: the turn those texts start is not known to the
/// hub at once (whether `UserPromptSubmit` fires for a channel message is not
/// verified), and a command typed into it would mix with it.
pub const INBOUND_SETTLE: Duration = Duration::from_secs(3);
/// Topic texts handed to a session with no turn running that show no sign
/// of being taken within this long mean its channel is off (TASK-052).
pub const CHANNEL_WAIT: Duration = Duration::from_secs(20);
/// Told once per session when its messages go nowhere (TASK-052).
pub const CHANNEL_OFF_NOTICE: &str = "Сообщение передано в сессию, но Claude его не получил: \
похоже, канал cctg в этой сессии не включён (claude запущен без флага \
--dangerously-load-development-channels, то есть не через claude-cctg, или при запуске не \
подтверждён диалог development channels). Выйдите из claude (/exit), запустите claude-cctg \
--continue в той же папке и подтвердите диалог при запуске. Сообщения, отправленные до этого, \
пришлите ещё раз.";

/// The channel message after a restart that cut off work or stopped the
/// background subagents `agents` (TASK-047).
pub fn continue_text(agents: &[String]) -> String {
    if agents.is_empty() {
        return CONTINUE_TEXT.to_owned();
    }
    format!(
        "{CONTINUE_TEXT} {CONTINUE_AGENTS_TEXT} {}. {CONTINUE_AGENTS_HOW}",
        agents.join(", ")
    )
}

/// A call of a session that starts or ends this long after one of its
/// permission prompts came in means the prompt was answered in the terminal
/// (the tool hooks reach the hub ~0.1 s after the call).
pub const PROMPT_SETTLE: Duration = Duration::from_secs(2);

#[derive(Debug, Clone)]
pub struct Options {
    pub icons: Icons,
    /// The forum supergroup, passed to Claude as `chat_id` meta.
    pub chat_id: i64,
    /// The bot may delete service messages (`can_delete_messages`).
    pub can_delete: bool,
    /// A slot gets the text-only notice at most once per this long; a burst
    /// of photos would eat the group's 20 messages a minute.
    pub notice_every: Duration,
    /// After start, topic edits wait this long: agents are reconnecting.
    pub grace: Duration,
    /// Failed topic calls are tried again this often.
    pub retry_every: Duration,
    /// How long a subagent hook waits for its `Agent` call to appear in the
    /// parent transcript; its stop starts the wait again.
    pub correlate_for: Duration,
    /// First pause before the parent transcript is looked at again; it doubles.
    pub recheck_after: Duration,
    /// How often the agent of a streamed session is asked for new lines.
    pub stream_every: Duration,
    /// Longest wait of a turn answer for the end of its turn in the
    /// transcript (the lines before it go first).
    pub hold_answer: Duration,
    /// A stream whose message Telegram did not take reads again after this.
    pub stream_retry: Duration,
    /// A hook prompt nobody answered by then gives its hook no decision
    /// ([`HOOK_ANSWER_WAIT`]).
    pub hook_answer_wait: Duration,
    /// A question nobody answered by then goes to the terminal
    /// ([`QUESTION_WAIT`]).
    pub question_wait: Duration,
    /// Slots get a status message, edited at most this often; `None`: no
    /// status messages ([`STATUS_EVERY`] in the hub).
    pub status_every: Option<Duration>,
    /// The bot may pin messages (`can_pin_messages`).
    pub can_pin: bool,
    /// [`PROMPT_SETTLE`].
    pub prompt_settle: Duration,
    /// The hub's own build ([`crate::client`]); `None`: agents are never
    /// outdated.
    pub build: Option<String>,
    /// The release tag the hub was built for ([`crate::client::release`]),
    /// sent with `update` to an outdated agent, which downloads that
    /// release's binary (TASK-050); `None`: the agent looks only at its disk.
    pub release: Option<String>,
    /// A session read the agent has not answered by then has failed
    /// ([`READ_WAIT`]); each piece of a text starts the wait again.
    pub read_wait: Duration,
    /// A text message for a live session waits this long for the next one
    /// and they go as one inbound ([`GATHER_QUIET`] in the hub); `ZERO`:
    /// each goes at once.
    pub gather_quiet: Duration,
    /// A burst goes at most this long after its first message
    /// ([`GATHER_MAX`] in the hub).
    pub gather_max: Duration,
    /// [`INBOUND_SETTLE`] in the hub; `ZERO`: a command right after a text
    /// is typed.
    pub inbound_settle: Duration,
    /// [`CHANNEL_WAIT`] in the hub; `ZERO`: the channel is never reported
    /// off.
    pub channel_wait: Duration,
}

/// A burst of topic messages of a slot being gathered into one inbound
/// (TASK-048); its messages wait in the slot's buffer.
#[derive(Debug, Clone, Copy)]
struct Gather {
    /// Its first message; texts kept in front of it are older and do not
    /// wait for it.
    start: i64,
    first: Instant,
    /// The burst goes then, or at once when that passed (a file or a
    /// command ended it, or the link queue had no room).
    due: Instant,
}

/// The kept file of a slot on its way to an agent.
#[derive(Debug, Clone, Copy)]
struct Fetching {
    transfer_id: u64,
    message_id: i64,
    /// The link it goes to: it counts only while that is still the
    /// slot's live agent.
    conn: u64,
}

/// What became of a kept file on its way to the agent.
enum FileStep {
    /// It waits in the slot: being fetched, or no room now.
    Wait,
    /// It left the slot; `delivered`: something of it reached the agent.
    Gone { delivered: bool },
}

/// A file from an agent being received.
struct Upload {
    transfer_id: u64,
    name: String,
    caption: Option<String>,
    /// The files of an album offer, in order (TASK-059); empty for one file.
    parts: Vec<FilePart>,
    assembly: files::Assembly,
    /// When it was accepted or its last chunk came.
    touched: Instant,
}

/// A `file_offer` as it came.
struct Offer {
    transfer_id: u64,
    name: String,
    size: u64,
    caption: Option<String>,
    parts: Vec<FilePart>,
}

/// An offer's parts, if any, are 2 to [`MAX_ALBUM`] files of 1 byte to
/// [`files::MAX_UPLOAD`] each that add up to its size.
fn album_fits(parts: &[FilePart], size: u64) -> bool {
    parts.is_empty()
        || ((2..=MAX_ALBUM).contains(&parts.len())
            && parts
                .iter()
                .all(|part| part.size > 0 && part.size <= files::MAX_UPLOAD)
            && parts.iter().map(|part| part.size).sum::<u64>() == size)
}

impl Default for Options {
    fn default() -> Self {
        Self {
            icons: Icons::default(),
            chat_id: 0,
            can_delete: true,
            notice_every: Duration::from_secs(60),
            // Longer than the agent's 30 s maximum reconnect backoff.
            grace: Duration::from_secs(45),
            retry_every: Duration::from_secs(60),
            correlate_for: Duration::from_secs(60),
            recheck_after: Duration::from_secs(1),
            // Records show up in the jsonl ~0.1-0.3 s after their timestamp
            // (TASK-016 measurement); a read is one small link round trip.
            stream_every: Duration::from_millis(300),
            // The turn's last record shows up ~0.15 s after the answer.
            hold_answer: Duration::from_secs(5),
            stream_retry: Duration::from_secs(5),
            hook_answer_wait: HOOK_ANSWER_WAIT,
            question_wait: QUESTION_WAIT,
            status_every: None,
            can_pin: true,
            prompt_settle: PROMPT_SETTLE,
            build: None,
            release: None,
            read_wait: READ_WAIT,
            gather_quiet: Duration::ZERO,
            gather_max: Duration::ZERO,
            inbound_settle: Duration::ZERO,
            channel_wait: Duration::ZERO,
        }
    }
}

/// From the update poll.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Control {
    /// A `forum_topic_edited` service message.
    TopicEdited {
        thread_id: Option<i64>,
        message_id: i64,
    },
    /// A message from an allowlisted user that is not a command.
    Message(Inbound),
    /// A button press from an allowlisted user.
    Callback(CallbackInput),
    /// A `pinned_message` service message (`message_id`) about message
    /// `pinned`, sent by this bot.
    Pinned { message_id: i64, pinned: i64 },
    /// The hub stops: [`Slots::run`] handles what already came in, writes
    /// the registry and returns.
    Stop,
}

#[derive(Debug)]
enum Done {
    Topic {
        job: TopicJob,
        /// `None`: the scheduler stopped.
        delivery: Option<Delivery>,
    },
    Delete(Option<Delivery>),
    /// A reply chunk or a notice.
    Message(Option<Delivery>),
    /// A permission prompt, by its key in [`Prompts`].
    Permission {
        key: u64,
        delivery: Option<Delivery>,
    },
    /// The final edit of a prompt, by its key.
    PromptEdit {
        key: u64,
        delivery: Option<Delivery>,
    },
    /// A question message sent as `version`, by its key in [`Asks`].
    Question {
        key: u64,
        version: u64,
        delivery: Option<Delivery>,
    },
    /// An edit of a question message to `version`.
    QuestionEdit {
        key: u64,
        version: u64,
        delivery: Option<Delivery>,
    },
    /// A button answer, the edit of an expired prompt or of a Resume
    /// message whose period ended.
    Callback(Option<Delivery>),
    /// The Resume message of `slot` sent as number `number`.
    Resume {
        slot: SlotId,
        number: u64,
        delivery: Option<Delivery>,
    },
    Block {
        job: BlockJob,
        delivery: Option<Delivery>,
    },
    /// A stream message of `session`, by its number in [`Live`].
    Stream {
        session: String,
        number: u64,
        delivery: Option<Delivery>,
    },
    Reaction(Option<Delivery>),
    Status {
        slot: SlotId,
        job: StatusJob,
        delivery: Option<Delivery>,
    },
    /// The file of the message at the front of `slot` went to its agent,
    /// or did not.
    Fetched {
        slot: SlotId,
        transfer_id: u64,
        outcome: Fetched,
    },
    /// A file of an agent's `send_file`.
    File {
        conn: u64,
        transfer_id: u64,
        size: u64,
        delivery: Option<Delivery>,
    },
    /// A message of an album offer.
    Album {
        conn: u64,
        transfer_id: u64,
        size: u64,
        parts: Vec<usize>,
        delivery: Option<Delivery>,
    },
}

/// A call about a slot's status message; at most one per slot in flight,
/// except a ⏹ edit next to a waiting refresh.
#[derive(Debug, Clone)]
enum StatusJob {
    Create {
        thread_id: i64,
        text: String,
        keyboard: serde_json::Value,
    },
    Edit {
        message_id: i64,
        text: String,
        keyboard: serde_json::Value,
    },
    Pin {
        message_id: i64,
    },
}

/// What the actor knows of a slot's status message beyond the registry.
#[derive(Debug, Default)]
struct Shown {
    /// The text and keyboard Telegram shows, as far as known.
    content: Option<(String, serde_json::Value)>,
    /// [`StatusJob`]s in flight: one, or two when a ⏹ edit went after a
    /// refresh that still waits for the edit budget (the scheduler lets the
    /// newer one replace it).
    in_flight: u8,
    /// The next edit shows a ⏹ press: it goes as a foreground edit, also
    /// while a refresh of the message waits.
    urgent: bool,
    /// No edit before this (`Options::status_every` after the last one).
    next_at: Option<Instant>,
    /// A first ⏹ press of this session waits for its second until then.
    confirm: Option<(String, Instant)>,
    /// Pinning failed; not tried again in this run.
    pin_failed: bool,
    /// A failed send is tried again after this.
    retry_at: Option<Instant>,
    /// A failed send was warned about; the next warn waits for a success.
    send_warned: bool,
}

/// An Esc an agent was asked to write: the slot, session and connection it
/// was asked for. An answer counts only while all three still hold.
struct KeyAsk {
    slot: SlotId,
    session: String,
    conn: u64,
    until: Instant,
}

/// A command an agent was asked to type, like [`KeyAsk`], plus the topic
/// message it came from.
struct CommandAsk {
    slot: SlotId,
    session: String,
    conn: u64,
    thread_id: i64,
    message_id: i64,
    until: Instant,
}

/// The fields of one `transcript_chunk`.
struct Chunk<'a> {
    from: u64,
    to: u64,
    lines: &'a [StreamLine],
    missing: bool,
    more: bool,
    reset: bool,
}

/// A `session_read` out to an agent (TASK-034).
struct Pending {
    conn: u64,
    /// No answer by then: the read failed.
    until: Instant,
    purpose: Purpose,
}

/// What a session read is for.
enum Purpose {
    /// `/brief` or `/full`; `text` gathers the pieces.
    Command {
        ask: TranscriptAsk,
        session: String,
        text: String,
    },
    /// The ai-title of `session`, asked of agent `conn`.
    Title {
        session: String,
        path: String,
        conn: u64,
    },
    /// The `Agent` calls of `session`'s transcript from `from`.
    Calls {
        session: String,
        path: String,
        from: u64,
    },
    /// A finished subagent's block; `text` gathers the pieces.
    Body { input: BodyInput, text: String },
}

/// The messages of an album offer on their way to Telegram (TASK-059).
struct Album {
    /// Per file of the offer: whether Telegram took it.
    sent: Vec<bool>,
    /// Messages not answered yet.
    left: usize,
}

/// A job for the dispatch task.
#[derive(Debug)]
enum Work {
    Topic(TopicJob),
    Delete,
    Message,
    Permission(u64),
    PromptEdit(u64),
    Question {
        key: u64,
        version: u64,
    },
    QuestionEdit {
        key: u64,
        version: u64,
    },
    Callback,
    Resume {
        slot: SlotId,
        number: u64,
    },
    Block(BlockJob),
    Stream {
        session: String,
        number: u64,
    },
    Reaction,
    Status {
        slot: SlotId,
        job: StatusJob,
    },
    File {
        conn: u64,
        transfer_id: u64,
        size: u64,
    },
    /// One message of an album offer: the files `parts` of it (TASK-059).
    Album {
        conn: u64,
        transfer_id: u64,
        size: u64,
        parts: Vec<usize>,
    },
}

fn short(session_id: &str) -> &str {
    session_id
        .char_indices()
        .nth(SHORT_ID)
        .map_or(session_id, |(end, _)| &session_id[..end])
}

fn telegram_error(delivery: &Delivery, needles: &[&str]) -> bool {
    match delivery {
        Err(ApiError::Telegram {
            code: 400,
            description,
        }) => {
            let description = description.to_ascii_lowercase();
            needles.iter().any(|needle| description.contains(needle))
        }
        _ => false,
    }
}

/// Telegram refused a send, or it never left this machine: no message exists.
fn send_refused(delivery: Option<&Delivery>) -> bool {
    match delivery {
        Some(Err(ApiError::Telegram { code, .. })) => (400..500).contains(code),
        Some(Err(ApiError::Http(error))) => error.is_connect(),
        _ => false,
    }
}

/// The topic was deleted in Telegram.
fn topic_gone(delivery: &Delivery) -> bool {
    telegram_error(
        delivery,
        &[
            "topic_id_invalid",
            "topic_deleted",
            "message thread not found",
        ],
    )
}

/// `editForumTopic` with the name and icon it already has.
fn not_modified(delivery: &Delivery) -> bool {
    telegram_error(delivery, &["topic_not_modified"])
}

/// Why a session read that did not bring its text failed.
fn failure(answer: &SessionAnswer) -> Unavailable {
    match answer {
        SessionAnswer::Missing => Unavailable::Missing,
        SessionAnswer::Refused => Unavailable::Refused,
        SessionAnswer::Unreadable => Unavailable::Unreadable,
        SessionAnswer::TooLarge => Unavailable::TooLarge,
        _ => Unavailable::Failed,
    }
}

/// A `PermissionRequest` hook waiting for its channel twin.
struct HookAsk {
    post: PermissionPost,
    answer: oneshot::Sender<Option<Behavior>>,
    /// No twin by then: the ask becomes a prompt.
    show_at: Instant,
    /// Gives up then (`Options::hook_answer_wait` after it came).
    until: Instant,
}

/// The waiting hook of a hook prompt.
struct Waiter {
    answer: oneshot::Sender<Option<Behavior>>,
    until: Instant,
}

/// The waiting hook of a question.
struct QuestionWaiter {
    answer: oneshot::Sender<Option<Vec<Answered>>>,
    until: Instant,
}

/// One registered agent connection.
struct Conn {
    /// The session it is bound to or waits for.
    session: String,
    /// Binding changes by actor time. Frames carry their read time, so a
    /// frame read before `/clear` keeps the old session if consumed later.
    bindings: Vec<(StdInstant, String)>,
    host: String,
    claude_pid: Option<u32>,
    to_agent: mpsc::Sender<HubMsg>,
    /// It acknowledges permission verdicts ([`crate::wire::Register::verdict_ack`]).
    acks: bool,
    /// It answers transcript reads ([`crate::wire::Register::transcript_reads`]).
    reads: bool,
    /// It presses console keys ([`crate::wire::Register::console_keys`]).
    keys: bool,
    /// It types console commands ([`crate::wire::Register::console_commands`]).
    commands: bool,
    /// Its build and update abilities ([`crate::wire::Register::client`]).
    client: Option<Client>,
    /// It takes files ([`crate::wire::Register::files`]).
    files: bool,
    /// It reads its session's files ([`crate::wire::Register::session_reads`]).
    session_reads: bool,
    /// It passes status line numbers on and is told its session
    /// ([`crate::wire::Register::status_lines`]).
    status_lines: bool,
    /// Its last `bound` found the queue full; told again on a tick.
    untold: bool,
    /// It is leaving after an update answer: bound to nothing, never
    /// rebound by its claude pid.
    leaving: bool,
}

/// An update press of a session, until its last answer.
struct UpdateAsk {
    /// The `update` in flight: its id and connection.
    sent: Option<(u64, u64)>,
    /// The last `update` whose agent answered that it leaves: a later answer
    /// of it (a refused restart) still ends the press, and the next round
    /// never goes to that agent.
    left: Option<(u64, u64)>,
    /// Agents asked so far.
    rounds: u8,
    /// Forgotten then; pushed on while a turn runs.
    until: Instant,
    /// The topic was told that the press waits for a long turn.
    told: bool,
    /// A restart held back because a turn began: its agent was not released
    /// and gives the `update` up by itself; that answer is only noted.
    held: Option<(u64, u64)>,
    /// A restart held back by background agents (TASK-047) is asked again
    /// then.
    retry_at: Option<Instant>,
    /// The topic was told that the restart waits for background agents.
    agents_told: bool,
    /// ⏹ was written into the session's console while the press waited: a
    /// restart cuts off that work.
    interrupted: bool,
}

pub struct Slots {
    registry: Registry,
    dispatch: mpsc::UnboundedSender<(Work, Op)>,
    options: Options,
    saver: watch::Sender<Option<Arc<Vec<u8>>>>,
    /// The save task; awaited on [`Control::Stop`].
    save_task: Option<JoinHandle<()>>,
    conns: HashMap<u64, Conn>,
    /// Agents of sessions no SessionStart announced yet: session -> conn.
    pending: HashMap<String, u64>,
    reading: HashSet<String>,
    /// Transcript bytes already scanned for an ai-title: session -> (path, offset).
    scanned: HashMap<String, (String, u64)>,
    delete_warned: bool,
    /// Messages handed to the dispatch task and not answered yet.
    queued_messages: usize,
    overflow_warned: bool,
    /// Resume messages sent by this run; numbers their notes.
    resume_sends: u64,
    /// When a slot last got a notice of a kind.
    notices: HashMap<(SlotId, &'static str), Instant>,
    prompts: Prompts,
    /// `PermissionRequest` hooks, see [`Slots::permission_asks`].
    asks: Option<mpsc::Receiver<PermissionAsk>>,
    /// `/brief` and `/full`, see [`Slots::transcript_asks`].
    transcript_asks: Option<mpsc::Receiver<TranscriptAsk>>,
    /// Session reads out to agents, by read id.
    reads: HashMap<u64, Pending>,
    /// Hooks waiting for their channel twin, oldest first.
    hook_asks: Vec<HookAsk>,
    /// Hooks waiting for a press, by the key of their prompt.
    hook_waiters: HashMap<u64, Waiter>,
    /// Recent channel requests: (arrival, session, tool name).
    relayed: VecDeque<(Instant, String, String)>,
    /// `AskUserQuestion` hooks, see [`Slots::question_asks`].
    question_asks: Option<mpsc::Receiver<QuestionAsk>>,
    /// Questions shown or to be shown.
    questions: Asks,
    /// Topic messages waiting for a question's message id, oldest first.
    held: VecDeque<Inbound>,
    /// Hooks waiting for the answers, by the key of their question.
    question_waiters: HashMap<u64, QuestionWaiter>,
    /// When each session's question hook last asked; pruned after
    /// [`QUESTION_HOOK_WINDOW`].
    question_hooks: HashMap<String, Instant>,
    /// Typed subagents not yet matched to an `Agent` call of their parent.
    candidates: Candidates,
    /// Subagents of live top-level sessions that started and did not stop
    /// yet: agent id -> (session, when the entry stops counting). See
    /// [`Slots::agents_running`] (TASK-047).
    started_agents: HashMap<String, (String, Instant)>,
    /// `Agent` calls per session transcript, read incrementally.
    indexes: HashMap<String, AgentIndex>,
    /// Sessions whose transcript is being read for `Agent` calls.
    indexing: HashSet<String>,
    reports: Reports,
    /// Block texts to read from subagent files, the newest stop per agent.
    bodies_waiting: BTreeMap<String, BodyInput>,
    /// Agents whose files are being read, at most [`MAX_BODY_READS`].
    bodies_reading: HashSet<String>,
    /// Block texts whose read was cut by a lost link or a late answer,
    /// waiting until then for the parent's next agent ([`BODY_RETRY_WAIT`]).
    bodies_parked: HashMap<String, (BodyInput, Instant)>,
    /// Agents whose block text was asked again once already.
    bodies_retried: HashSet<String>,
    /// Title reads that failed in a row, by session: the agent asked, how
    /// many ([`MAX_TITLE_FAILURES`]).
    title_failures: HashMap<String, (u64, u32)>,
    /// Block jobs handed out and not answered, at most [`MAX_BLOCK_JOBS`].
    block_jobs: usize,
    /// Live transcript streams by session.
    streams: HashMap<String, Live>,
    reaction_warned: bool,
    /// Reactions handed out and not answered, at most [`MAX_REACTIONS`].
    reactions: usize,
    /// What live top-level sessions do, for their status messages.
    activity: HashMap<String, Activity>,
    /// Compactions of live top-level sessions, running or ended and waiting
    /// for their numbers.
    compactions: HashMap<String, Compaction>,
    /// Status messages by slot.
    shown: HashMap<SlotId, Shown>,
    /// Keys agents were asked to press, by key id.
    key_asks: HashMap<u64, KeyAsk>,
    /// Commands agents were asked to type, by command id.
    command_asks: HashMap<u64, CommandAsk>,
    /// Update presses by session.
    updates: HashMap<String, UpdateAsk>,
    /// The download task, once [`Slots::fetch_files`] started it.
    fetcher: Option<mpsc::Sender<fetch::Job>>,
    /// The kept file of a slot on its way to the agent.
    fetching: HashMap<SlotId, Fetching>,
    /// Hand-overs of the slot's front file (by message id) cut by a closed
    /// link, in a row.
    link_losses: HashMap<SlotId, (i64, u32)>,
    /// Bursts of topic messages being gathered, by slot.
    gathers: HashMap<SlotId, Gather>,
    /// When topic texts last went to a session, within
    /// `Options::inbound_settle`.
    handed_at: HashMap<String, Instant>,
    /// Streamed sessions that got topic texts while no turn ran, since
    /// when, until a sign that Claude took them (see [`Self::check_channel`]).
    unseen: HashMap<String, Instant>,
    /// Sessions whose topic got [`CHANNEL_OFF_NOTICE`]; told again only
    /// after one of their channel records showed up.
    channel_off_told: HashSet<String>,
    /// Transfers to agents of this run.
    transfers: u64,
    /// Files coming from agents, one per connection.
    uploads: HashMap<u64, Upload>,
    /// Bytes of [`Self::uploads`] and of files waiting for Telegram.
    file_bytes: u64,
    /// Album offers waiting for Telegram, by `(conn, transfer_id)`.
    albums: HashMap<(u64, u64), Album>,
    pin_warned: bool,
    grace_until: Instant,
    next_retry: Instant,
    done_tx: mpsc::UnboundedSender<Done>,
    done_rx: Option<mpsc::UnboundedReceiver<Done>>,
}

impl Slots {
    /// Starts the save and dispatch tasks.
    pub fn new(
        mut registry: Registry,
        store: RegistryStore,
        outbox: Outbox,
        options: Options,
    ) -> Self {
        // Blocks left running by sessions that ended meanwhile end now; a
        // result that still comes replaces the mark.
        let ended: Vec<String> = registry
            .sessions
            .iter()
            .filter(|(_, entry)| entry.ended)
            .map(|(id, _)| id.clone())
            .collect();
        registry.lose_blocks(&ended);
        let (saver, saves) = watch::channel(None);
        let save_task = tokio::spawn(save_loop(store, saves));
        let (done_tx, done_rx) = mpsc::unbounded_channel();
        let (dispatch, work) = mpsc::unbounded_channel();
        tokio::spawn(dispatch_loop(outbox, work, done_tx.clone()));
        let now = Instant::now();
        Self {
            registry,
            dispatch,
            saver,
            save_task: Some(save_task),
            conns: HashMap::new(),
            pending: HashMap::new(),
            reading: HashSet::new(),
            scanned: HashMap::new(),
            delete_warned: false,
            queued_messages: 0,
            overflow_warned: false,
            resume_sends: 0,
            notices: HashMap::new(),
            prompts: Prompts::default(),
            asks: None,
            transcript_asks: None,
            reads: HashMap::new(),
            hook_asks: Vec::new(),
            hook_waiters: HashMap::new(),
            relayed: VecDeque::new(),
            question_asks: None,
            questions: Asks::default(),
            held: VecDeque::new(),
            question_waiters: HashMap::new(),
            question_hooks: HashMap::new(),
            candidates: Candidates::default(),
            started_agents: HashMap::new(),
            indexes: HashMap::new(),
            indexing: HashSet::new(),
            reports: Reports::default(),
            bodies_waiting: BTreeMap::new(),
            bodies_reading: HashSet::new(),
            bodies_parked: HashMap::new(),
            bodies_retried: HashSet::new(),
            title_failures: HashMap::new(),
            block_jobs: 0,
            streams: HashMap::new(),
            reaction_warned: false,
            reactions: 0,
            activity: HashMap::new(),
            compactions: HashMap::new(),
            shown: HashMap::new(),
            key_asks: HashMap::new(),
            command_asks: HashMap::new(),
            updates: HashMap::new(),
            fetcher: None,
            fetching: HashMap::new(),
            link_losses: HashMap::new(),
            gathers: HashMap::new(),
            handed_at: HashMap::new(),
            unseen: HashMap::new(),
            channel_off_told: HashSet::new(),
            transfers: 0,
            uploads: HashMap::new(),
            file_bytes: 0,
            albums: HashMap::new(),
            pin_warned: false,
            grace_until: now + options.grace,
            next_retry: now + options.retry_every,
            done_tx,
            done_rx: Some(done_rx),
            options,
        }
    }

    /// Starts the task that downloads the files of kept messages with
    /// `fetch` and hands them to agents; call before [`Self::run`]. Without
    /// it a file from the topic gets the failed-download notice.
    pub fn fetch_files<F: Fetch>(&mut self, fetch: Arc<F>) {
        let (jobs, jobs_rx) = mpsc::channel(FETCH_QUEUE);
        let done = self.done_tx.clone();
        tokio::spawn(fetch::serve(
            fetch,
            jobs_rx,
            move |slot, transfer_id, outcome| {
                let _ = done.send(Done::Fetched {
                    slot,
                    transfer_id,
                    outcome,
                });
            },
        ));
        self.fetcher = Some(jobs);
    }

    /// The channel for `PermissionRequest` hooks
    /// ([`super::ingress::serve_hooks_and_permissions`]); call before
    /// [`Self::run`]. Without it the actor gets no hook asks.
    pub fn permission_asks(&mut self) -> mpsc::Sender<PermissionAsk> {
        let (asks, asks_rx) = mpsc::channel(MAX_PERMISSION_WAITS);
        self.asks = Some(asks_rx);
        asks
    }

    /// The channel for `AskUserQuestion` hooks
    /// ([`super::ingress::serve_hooks_and_asks`]); call before
    /// [`Self::run`]. Without it the actor gets no questions.
    pub fn question_asks(&mut self) -> mpsc::Sender<QuestionAsk> {
        let (asks, asks_rx) = mpsc::channel(MAX_PERMISSION_WAITS);
        self.question_asks = Some(asks_rx);
        asks
    }

    /// The channel for `/brief` and `/full` ([`commands::Asks`]); call before
    /// [`Self::run`]. Every ask is answered once: with the text its
    /// session's agent rendered, or with a notice.
    pub fn transcript_asks(&mut self) -> mpsc::Sender<TranscriptAsk> {
        let (asks, asks_rx) = mpsc::channel(16);
        self.transcript_asks = Some(asks_rx);
        asks
    }

    /// Runs until [`Control::Stop`]; a closed input channel is just no
    /// longer polled. On stop, hook posts and agent frames already queued are
    /// handled and the last registry snapshot is on disk before it returns;
    /// waiting `PermissionRequest` and question hooks get no decision.
    pub async fn run(
        mut self,
        mut agents: mpsc::Receiver<AgentEvent>,
        mut hooks: mpsc::Receiver<HookPost>,
        mut control: mpsc::UnboundedReceiver<Control>,
    ) {
        let mut done = self.done_rx.take().expect("run once");
        let mut asks = self.asks.take();
        let mut question_asks = self.question_asks.take();
        let mut transcript_asks = self.transcript_asks.take();
        self.pump();
        loop {
            let deadline = self.next_deadline();
            let ask = async {
                match asks.as_mut() {
                    Some(asks) => asks.recv().await,
                    None => std::future::pending().await,
                }
            };
            let question_ask = async {
                match question_asks.as_mut() {
                    Some(asks) => asks.recv().await,
                    None => std::future::pending().await,
                }
            };
            let transcript_ask = async {
                match transcript_asks.as_mut() {
                    Some(asks) => asks.recv().await,
                    None => std::future::pending().await,
                }
            };
            tokio::select! {
                Some(event) = agents.recv() => self.on_agent(event),
                Some(post) = hooks.recv() => self.on_hook(&post),
                Some(ask) = ask => self.on_permission_ask(ask),
                Some(ask) = question_ask => self.on_question_ask(ask),
                Some(ask) = transcript_ask => self.on_transcript_ask(ask),
                Some(control) = control.recv() => {
                    if control == Control::Stop {
                        break;
                    }
                    self.on_control(control);
                }
                Some(finished) = done.recv() => self.on_done(finished),
                () = sleep_until(deadline) => self.on_tick(),
            }
            self.pump();
        }
        // New posts and frames now fail at ingress (a hook gets 503 and
        // spools it) instead of being accepted and lost with the receiver.
        hooks.close();
        agents.close();
        while let Ok(post) = hooks.try_recv() {
            self.on_hook(&post);
        }
        while let Ok(event) = agents.try_recv() {
            self.on_agent(event);
        }
        // Waiting hooks get no decision (their answers are dropped), and
        // their prompts lose the buttons if Telegram still takes the edit.
        drop(asks);
        drop(question_asks);
        self.hook_asks.clear();
        for key in self.hook_waiters.keys().copied().collect::<Vec<_>>() {
            self.finish(key, State::Expired);
        }
        for key in self.question_waiters.keys().copied().collect::<Vec<_>>() {
            self.end_question(key, questions::State::Expired);
        }
        // Messages held for a question's id go on (into the slot buffer).
        self.release_held();
        self.pump();
        let save_task = self.save_task.take();
        // Closes the save channel: the task writes the last snapshot and ends.
        drop(self);
        if let Some(task) = save_task {
            let _ = task.await;
        }
        info!("slot registry saved");
    }

    fn next_deadline(&self) -> Instant {
        let deadline = if self.grace_until > Instant::now() {
            self.next_retry.min(self.grace_until)
        } else {
            self.next_retry
        };
        // A session whose transcript is being read wakes the actor anyway.
        let deadline = self
            .candidates
            .next_due(|session| self.indexing.contains(session))
            .map_or(deadline, |due| deadline.min(due));
        let hooks = self.hook_asks.iter().map(|ask| ask.show_at.min(ask.until));
        let waiters = self.hook_waiters.values().map(|waiter| waiter.until);
        let questions = self.question_waiters.values().map(|waiter| waiter.until);
        let mut deadline = hooks
            .chain(waiters)
            .chain(questions)
            .fold(deadline, Instant::min);
        if !self.hook_asks.is_empty()
            || !self.hook_waiters.is_empty()
            || !self.question_waiters.is_empty()
        {
            deadline = deadline.min(Instant::now() + HOOK_CHECK_EVERY);
        }
        // A throttled edit, a confirmation that runs out and a failed status
        // send wake the actor; past times are not waited for again.
        let now = Instant::now();
        let status = self
            .shown
            .values()
            .flat_map(|shown| {
                [
                    shown.next_at,
                    shown.confirm.as_ref().map(|(_, until)| *until),
                    shown.retry_at,
                ]
            })
            .flatten()
            .filter(|at| *at > now);
        let deadline = status.fold(deadline, Instant::min);
        let deadline = self
            .compaction_deadlines(now)
            .into_iter()
            .filter(|at| *at > now)
            .fold(deadline, Instant::min);
        let deadline = self
            .updates
            .values()
            .filter_map(|ask| ask.retry_at)
            .filter(|at| *at > now)
            .fold(deadline, Instant::min);
        // A press held by subagents goes on when they reach their age limit.
        let deadline = self
            .started_agents
            .values()
            .filter(|(session, _)| self.updates.contains_key(session))
            .map(|(_, until)| *until)
            .filter(|at| *at > now)
            .fold(deadline, Instant::min);
        let deadline = self
            .reads
            .values()
            .map(|pending| pending.until)
            .fold(deadline, Instant::min);
        let deadline = if self.bodies_parked.is_empty() {
            deadline
        } else {
            deadline.min(now + BODY_RETRY_CHECK)
        };
        let deadline = if self.conns.values().any(|bound| bound.untold) {
            deadline.min(now + BOUND_RETRY)
        } else {
            deadline
        };
        let deadline = self
            .gathers
            .values()
            .map(|gather| gather.due)
            .filter(|at| *at > now)
            .fold(deadline, Instant::min);
        self.streams
            .values()
            .flat_map(|live| {
                let read = match live.reading {
                    Some((_, sent)) => Some(sent + READ_TIMEOUT),
                    None => live.next_read,
                };
                read.into_iter()
                    .chain(live.held.front().map(|held| held.until))
            })
            .fold(deadline, Instant::min)
    }

    fn on_agent(&mut self, event: AgentEvent) {
        match event {
            AgentEvent::Registered {
                conn,
                register,
                to_agent,
            } => {
                let session = self.agent_session(&register);
                if let Some(hub) = self.options.build.as_deref() {
                    let agent = register.client.as_ref().map(|client| client.build.as_str());
                    if agent != Some(hub) {
                        info!(
                            conn,
                            session = short(&session),
                            agent = agent.map_or_else(|| "none".to_owned(), crate::client::short),
                            hub = crate::client::short(hub),
                            "agent runs another cctg build"
                        );
                    }
                }
                if self.registry.agent_connected(&session, conn) {
                    info!(
                        conn,
                        session = short(&session),
                        "agent bound to its session"
                    );
                } else {
                    debug!(
                        conn,
                        session = short(&session),
                        "agent of an unknown session waits for its SessionStart"
                    );
                    self.pending.insert(session.clone(), conn);
                }
                self.conns.insert(
                    conn,
                    Conn {
                        session: session.clone(),
                        bindings: vec![(StdInstant::now(), session.clone())],
                        host: register.host,
                        claude_pid: register.claude_pid,
                        to_agent,
                        acks: register.verdict_ack,
                        reads: register.transcript_reads,
                        keys: register.console_keys,
                        commands: register.console_commands,
                        client: register.client,
                        files: register.files,
                        session_reads: register.session_reads,
                        status_lines: register.status_lines,
                        untold: false,
                        leaving: false,
                    },
                );
                self.tell_bound(conn);
                // A link that came back takes the answers that wait for it.
                self.push_selected(Some(&session));
                self.sync_waiting(&session);
            }
            AgentEvent::Message {
                conn,
                received_at,
                msg,
            } => {
                if !self.conns.contains_key(&conn) {
                    return;
                }
                let session = self.session_at(conn, received_at);
                match msg {
                    AgentMsg::PermissionRequest(request) => {
                        self.on_permission_request(conn, &session, request);
                    }
                    AgentMsg::PermissionAck { verdict_id } => {
                        self.on_verdict_ack(conn, &session, verdict_id);
                    }
                    AgentMsg::Reply { text } => self.on_reply_for(conn, &session, &text),
                    AgentMsg::TranscriptChunk {
                        session_id,
                        from,
                        to,
                        lines,
                        missing,
                        more,
                        reset,
                    } if session_id == session => {
                        let chunk = Chunk {
                            from,
                            to,
                            lines: &lines,
                            missing,
                            more,
                            reset,
                        };
                        self.on_chunk(conn, &session, &chunk);
                    }
                    AgentMsg::ConsoleKeyWritten { key_id, written } => {
                        self.on_key_written(conn, &session, key_id, written);
                    }
                    AgentMsg::ConsoleCommandTyped {
                        command_id,
                        outcome,
                        panel,
                    } => self.on_command_typed(conn, &session, command_id, outcome, panel),
                    AgentMsg::UpdateAnswer { update_id, outcome } => {
                        self.on_update_answer(conn, &session, update_id, outcome);
                    }
                    AgentMsg::FileOffer {
                        transfer_id,
                        name,
                        size,
                        caption,
                        parts,
                    } => {
                        let offer = Offer {
                            transfer_id,
                            name,
                            size,
                            caption,
                            parts,
                        };
                        self.on_file_offer(conn, &session, offer);
                    }
                    AgentMsg::FileChunk(chunk) => self.on_file_chunk(conn, &session, &chunk),
                    AgentMsg::SessionAnswer { read_id, answer } => {
                        self.on_session_answer(conn, read_id, answer);
                    }
                    AgentMsg::StatusLine {
                        session_id,
                        model,
                        effort,
                        context,
                        five_hour,
                        seven_day,
                    } if session_id == session => {
                        let Some(host) = self.conns.get(&conn).map(|bound| bound.host.clone())
                        else {
                            return;
                        };
                        // The same event `cctg statusline` posts without an
                        // agent (TASK-058).
                        let numbers = HookEvent::StatusLine {
                            model,
                            effort,
                            context,
                            five_hour,
                            seven_day,
                        };
                        let post =
                            HookPost::new(host, session, String::new(), String::new(), numbers);
                        self.on_hook(&post);
                    }
                    _ => debug!(conn, "agent message not routed"),
                }
            }
            AgentEvent::Disconnected { conn } => {
                self.fail_reads_of(conn);
                if let Some(upload) = self.uploads.remove(&conn) {
                    self.file_bytes = self.file_bytes.saturating_sub(upload.assembly.size());
                    info!(conn, "agent link closed during a file; the file is dropped");
                }
                // An update it did not answer goes to the session's next agent.
                for ask in self.updates.values_mut() {
                    if ask.sent.is_some_and(|(_, to)| to == conn) {
                        ask.sent = None;
                    }
                    if ask.held.is_some_and(|(_, to)| to == conn) {
                        ask.held = None;
                    }
                }
                if let Some(gone) = self.conns.remove(&conn) {
                    let session = gone.session;
                    let bound = self
                        .registry
                        .sessions
                        .get(&session)
                        .is_some_and(|entry| entry.agent == Some(conn));
                    self.registry.agent_disconnected(&session, conn);
                    if self.pending.get(&session) == Some(&conn) {
                        self.pending.remove(&session);
                        if let Some(heir) = self.heir(&session) {
                            self.pending.insert(session.clone(), heir);
                        }
                    }
                    if bound {
                        self.rebind(&session, conn);
                    }
                }
            }
        }
    }

    /// Tells an agent that passes status line numbers on which session
    /// `conn` is bound to now (TASK-058); a full queue marks it `untold`
    /// and the tick tells it again ([`BOUND_RETRY`]).
    fn tell_bound(&mut self, conn: u64) {
        let Some(bound) = self.conns.get_mut(&conn).filter(|bound| bound.status_lines) else {
            return;
        };
        let told = HubMsg::Bound {
            session_id: bound.session.clone(),
        };
        bound.untold = match bound.to_agent.try_send(told) {
            Ok(()) => false,
            Err(mpsc::error::TrySendError::Full(_)) => {
                debug!(conn, "agent queue full; its session told later");
                true
            }
            Err(mpsc::error::TrySendError::Closed(_)) => false,
        };
    }

    /// The newest connection still open for `session` that belongs to the
    /// session's current claude process (any, when the session's pid is
    /// unknown) and is not leaving after an update answer.
    fn heir(&self, session: &str) -> Option<u64> {
        let run_pid = self
            .registry
            .sessions
            .get(session)
            .and_then(|entry| entry.claude_pid);
        self.conns
            .iter()
            .filter(|(_, bound)| {
                !bound.leaving
                    && bound.session == session
                    && (run_pid.is_none() || bound.claude_pid == run_pid)
            })
            .map(|(conn, _)| *conn)
            .max()
    }

    /// The bound connection `gone` of a running session closed while an
    /// older one of the same run is still open (a short-lived second agent,
    /// TASK-042): the session goes back to that one instead of showing
    /// "no channel".
    fn rebind(&mut self, session: &str, gone: u64) {
        if !self.registry.is_live_top_level(session) {
            return;
        }
        let Some(heir) = self.heir(session) else {
            return;
        };
        if self.registry.agent_connected(session, heir) {
            info!(
                conn = heir,
                gone,
                session = short(session),
                "agent bound again after a newer link of its session closed"
            );
            self.push_selected(Some(session));
            self.sync_waiting(session);
        }
    }

    /// The session a registering agent belongs to. Its env session id, unless
    /// that session is over and the registry knows a newer session of the
    /// same claude process: after `/clear` Claude Code keeps the channel
    /// server running with the old id (TASK-013).
    fn agent_session(&self, register: &crate::wire::Register) -> String {
        if !self.registry.is_live_top_level(&register.session_id)
            && let Some(current) = register
                .claude_pid
                .and_then(|pid| self.registry.live_session_of_pid(&register.host, pid))
        {
            return current.to_owned();
        }
        register.session_id.clone()
    }

    /// Session to which `conn` was bound when a frame was read. Frames from
    /// one connection arrive in order, so older binding history is no longer
    /// needed after the corresponding frame has been consumed.
    fn session_at(&mut self, conn: u64, received_at: StdInstant) -> String {
        let bound = self.conns.get_mut(&conn).expect("known connection");
        let index = bound
            .bindings
            .iter()
            .rposition(|(at, _)| *at <= received_at)
            .unwrap_or(0);
        let session = bound.bindings[index].1.clone();
        if index > 0 {
            bound.bindings.drain(..index);
        }
        session
    }

    /// Moves the agents of a claude process to the session that process now
    /// runs. Nothing moves when the pid names no running top-level session.
    fn follow_pid(&mut self, host: &str, pid: u32) {
        let Some(session) = self
            .registry
            .live_session_of_pid(host, pid)
            .map(str::to_owned)
        else {
            return;
        };
        // Only the newest connection of the process moves: a stale duplicate
        // (the agent reconnected) must not win the new session by map order.
        let mover = self
            .conns
            .iter()
            .filter(|(_, bound)| {
                !bound.leaving
                    && bound.host == host
                    && bound.claude_pid == Some(pid)
                    && bound.session != session
            })
            .map(|(conn, _)| *conn)
            .max();
        let Some(conn) = mover else {
            return;
        };
        let Some(bound) = self.conns.get_mut(&conn) else {
            return;
        };
        let rebound_at = StdInstant::now();
        let old = std::mem::replace(&mut bound.session, session.clone());
        bound.bindings.push((rebound_at, session.clone()));
        self.registry.agent_disconnected(&old, conn);
        if self.pending.get(&old) == Some(&conn) {
            self.pending.remove(&old);
        }
        if self.registry.agent_connected(&session, conn) {
            info!(
                conn,
                from = short(&old),
                session = short(&session),
                "agent follows its claude process to a new session"
            );
        }
        self.tell_bound(conn);
    }

    fn on_hook(&mut self, post: &HookPost) {
        let followup = self.registry.apply_hook(post);
        let session = post.session_id.as_str();
        if matches!(post.event, HookEvent::SessionEnd { .. }) {
            self.scanned.remove(session);
            self.title_failures.remove(session);
        }
        for gone in &followup.reaped {
            info!(
                session = short(gone),
                "session ended: its claude process is gone"
            );
            self.scanned.remove(gone);
            self.title_failures.remove(gone);
        }
        if self.registry.sessions.contains_key(session)
            && let Some(conn) = self.pending.remove(session)
        {
            self.registry.agent_connected(session, conn);
        }
        if let HookEvent::SessionStart {
            claude_pid: Some(pid),
            ..
        } = post.event
        {
            self.follow_pid(&post.host, pid);
        }
        if matches!(
            post.event,
            HookEvent::Stop { .. } | HookEvent::UserPromptSubmit { .. }
        ) {
            self.prompts.quiet(session);
        }
        if matches!(
            post.event,
            HookEvent::Stop { .. }
                | HookEvent::UserPromptSubmit { .. }
                | HookEvent::ToolStart { .. }
        ) {
            // A turn runs or ran: texts handed to the session were taken or
            // wait in Claude Code's queue behind it.
            self.unseen.remove(session);
        }
        if let HookEvent::Stop {
            last_assistant_message: Some(answer),
            ..
        } = &post.event
        {
            // Kept in the registry, so a restart before the run ends keeps it.
            self.registry.set_nested_answer(session, answer);
            self.on_turn_answer(session, answer);
        }
        match &post.event {
            HookEvent::SubagentStart {
                agent_id,
                agent_type,
            } => self.on_subagent(session, agent_id, agent_type, None),
            HookEvent::SubagentStop {
                agent_id,
                agent_type,
                agent_transcript_path,
                last_assistant_message,
            } => {
                let stop = Stopped {
                    agent_type: agent_type.clone(),
                    agent_path: agent_transcript_path.clone().unwrap_or_default(),
                    last: last_assistant_message.clone(),
                };
                self.on_subagent(session, agent_id, agent_type, Some(stop));
            }
            HookEvent::SubagentHandback { agent_id, message }
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
            HookEvent::UserPromptSubmit { .. }
            | HookEvent::ToolStart { .. }
            | HookEvent::Stop { .. } => self.compact_settled(session),
            _ => {}
        }
        self.track_activity(session, &post.event);
        self.track_agents(session, &post.event);
        // Every way a session ends (its SessionEnd, `/clear`, a reused pid,
        // the reaper) drops what it did.
        let registry = &self.registry;
        self.activity
            .retain(|session, _| registry.is_live_top_level(session));
        self.compactions
            .retain(|session, _| registry.is_live_top_level(session));
        self.started_agents
            .retain(|_, (session, _)| registry.is_live_top_level(session));
        self.unseen
            .retain(|session, _| registry.is_live_top_level(session));
        self.channel_off_told
            .retain(|session| registry.is_live_top_level(session));
        self.close_prompts(&followup.ended_sessions);
        self.end_blocks(&followup.ended_sessions);
        if let Some((session, path)) = followup.read_title {
            self.read_title(session, path);
        }
    }

    /// `PreCompact` of the live current session of a slot with a topic: the
    /// status says so and the topic gets one line. A repeat while one runs
    /// changes nothing; the line of an ended one that still waits for its
    /// numbers goes first, without them.
    fn compact_started(&mut self, session: &str, trigger: Option<&str>) {
        match self.compactions.get(session) {
            Some(compaction) if compaction.done.is_none() && compaction.settled.is_none() => {
                return;
            }
            Some(compaction) if compaction.done.is_some() => self.compact_told(session, None),
            _ => {}
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
                settled: None,
                done: None,
            },
        );
        self.send_messages(vec![message_op(thread_id, status::compacting_line(auto))]);
    }

    /// `SessionStart` with `source: compact`: the compaction is done, also
    /// when activity came first, within [`COMPACT_GRACE`]. Its line waits for
    /// the new context percentage when one was known before.
    fn compact_ended(&mut self, session: &str) {
        // The last percentage before the end: a status line sent while it
        // ran is still the old context.
        let last = self
            .registry
            .sessions
            .get(session)
            .and_then(|entry| entry.metrics.as_ref())
            .and_then(|metrics| metrics.context);
        let now = Instant::now();
        let Some(compaction) = self.compactions.get_mut(session).filter(|compaction| {
            compaction.done.is_none()
                && compaction
                    .settled
                    .is_none_or(|settled| now < settled + COMPACT_GRACE)
        }) else {
            return;
        };
        let took = now.saturating_duration_since(compaction.started);
        info!(
            session = short(session),
            took_s = took.as_secs(),
            "compaction ended"
        );
        compaction.done = Some((took, now + COMPACT_NUMBERS_WAIT));
        compaction.before = last.or(compaction.before);
        if compaction.before.is_none() {
            self.compact_told(session, None);
        }
    }

    /// Activity of `session` after its `PreCompact`: a compaction that did
    /// not end yet was cancelled or failed, or its end is late. The status
    /// stops showing it at once; its end is still taken for a while.
    fn compact_settled(&mut self, session: &str) {
        if let Some(compaction) = self
            .compactions
            .get_mut(session)
            .filter(|compaction| compaction.done.is_none() && compaction.settled.is_none())
        {
            debug!(
                session = short(session),
                "activity while a compaction ran; its status ends"
            );
            compaction.settled = Some(Instant::now());
        }
    }

    /// A status line: the percentage after an ended compaction, once it is
    /// below the one before (a line sent before the end can still arrive
    /// after it, and a compaction never grows the context).
    fn compact_numbers(&mut self, session: &str, context: Option<u32>) {
        let Some(context) = context else {
            return;
        };
        if self.compactions.get(session).is_some_and(|compaction| {
            compaction.done.is_some() && compaction.before.is_some_and(|before| context < before)
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

    /// Compactions that never ended (in time, or within the grace after the
    /// session moved on) are forgotten; ended ones whose numbers did not come
    /// are told without them.
    fn check_compactions(&mut self, now: Instant) {
        self.compactions.retain(|session, compaction| {
            let lost = compaction.done.is_none()
                && (now >= compaction.started + COMPACT_MAX
                    || compaction
                        .settled
                        .is_some_and(|settled| now >= settled + COMPACT_GRACE));
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
            .map(|compaction| match (compaction.done, compaction.settled) {
                (Some((_, until)), _) => until,
                (None, Some(settled)) => settled + COMPACT_GRACE,
                (None, None) => {
                    let minutes = now.saturating_duration_since(compaction.started).as_secs() / 60;
                    let next_minute = compaction.started + Duration::from_secs((minutes + 1) * 60);
                    next_minute.min(compaction.started + COMPACT_MAX)
                }
            })
            .collect()
    }

    /// What a live top-level session does, for its status message. Events of
    /// other sessions (nested, unknown, ended) are not kept.
    fn track_activity(&mut self, session: &str, event: &HookEvent) {
        if !self.registry.is_live_top_level(session) {
            return;
        }
        let activity = self.activity.entry(session.to_owned()).or_default();
        match event {
            HookEvent::UserPromptSubmit { .. } => activity.prompt(),
            HookEvent::Stop { .. } => activity.stop(),
            HookEvent::ToolStart { tool_use_id, line } => activity.tool_start(tool_use_id, line),
            HookEvent::ToolEnd { tool_use_id } => activity.tool_end(tool_use_id),
            // The numbers live in the registry (`SessionEntry::metrics`).
            _ => return,
        }
        if matches!(
            event,
            HookEvent::ToolStart { .. } | HookEvent::ToolEnd { .. }
        ) {
            // The turn moved on: a prompt of it that settled was answered in
            // the terminal (Telegram answers are seen). Stream lines do not
            // count: they can lag the prompt by any time.
            let settle = self.options.prompt_settle;
            self.prompts.quiet_settled(session, Instant::now(), settle);
            self.sync_waiting(session);
        }
    }

    /// Typed subagents a live top-level session started and did not stop
    /// yet, for [`Self::agents_running`] (TASK-047). Entries past
    /// [`AGENT_MAX_AGE`] are dropped on the next start.
    fn track_agents(&mut self, session: &str, event: &HookEvent) {
        match event {
            HookEvent::SubagentStart {
                agent_id,
                agent_type,
            } if self.registry.is_live_top_level(session)
                && !agent_type.trim().is_empty()
                && subagents::is_agent_id(agent_id) =>
            {
                let now = Instant::now();
                self.started_agents.retain(|_, (_, until)| now < *until);
                self.started_agents
                    .insert(agent_id.clone(), (session.to_owned(), now + AGENT_MAX_AGE));
            }
            HookEvent::SubagentStop { agent_id, .. }
                if self
                    .started_agents
                    .get(agent_id)
                    .is_some_and(|(owner, _)| owner == session) =>
            {
                self.started_agents.remove(agent_id);
            }
            _ => {}
        }
    }

    /// The subagents of `session` the hub sees running, by agent id: a
    /// `SubagentStart` came and no `SubagentStop`, less than
    /// [`AGENT_MAX_AGE`] ago, and the agent is still a candidate or was
    /// matched to an `Agent` call of the session (Claude Code's internal
    /// agents never are, and their stops never reach the hub).
    fn agents_running(&self, session: &str) -> Vec<String> {
        let now = Instant::now();
        let mut agents: Vec<String> = self
            .started_agents
            .iter()
            .filter(|(agent_id, (owner, until))| {
                owner == session
                    && now < *until
                    && (self.candidates.contains(agent_id)
                        || self
                            .registry
                            .subagents
                            .get(*agent_id)
                            .is_some_and(|entry| entry.parent_session == session))
            })
            .map(|(agent_id, _)| agent_id.clone())
            .collect();
        agents.sort();
        agents
    }

    /// Ended nested runs show their last answer, also when the block was
    /// marked lost because the parent ended first; every other block still
    /// running in those sessions ends as lost. The `Agent` calls of ended
    /// sessions are forgotten unless a candidate or a running read needs them.
    fn end_blocks(&mut self, ended: &[String]) {
        for session in ended {
            let answer = self.registry.take_nested_answer(session);
            let key = BlockKey::Nested(session.clone());
            let Some(header) = self
                .registry
                .block(&key)
                .filter(|block| block.running || answer.is_some())
                .map(|block| block.header.clone())
            else {
                continue;
            };
            info!(session = short(session), "nested run block finished");
            self.finish_block(key, subagents::nested_text(&header, answer.as_deref()));
        }
        self.registry.lose_blocks(ended);
        for session in ended {
            if !self.indexing.contains(session) && self.candidates.of_session(session).is_empty() {
                self.indexes.remove(session);
            }
        }
    }

    /// A typed subagent hook. A known subagent's stop finishes its block;
    /// an unknown one waits as a candidate until the parent's transcript
    /// shows its `Agent` call. Subagents of nested runs get no block.
    fn on_subagent(
        &mut self,
        session: &str,
        agent_id: &str,
        agent_type: &str,
        stop: Option<Stopped>,
    ) {
        if agent_type.trim().is_empty() || !subagents::is_agent_id(agent_id) {
            return;
        }
        if let Some(entry) = self.registry.subagents.get(agent_id) {
            if entry.parent_session == session
                && let Some(stop) = stop
            {
                self.read_body(agent_id, stop);
            }
            return;
        }
        let own_slot = self
            .registry
            .sessions
            .get(session)
            .is_some_and(|entry| entry.kind == SessionKind::TopLevel && entry.slot.is_some());
        if !own_slot {
            debug!(
                agent = short(agent_id),
                "subagent of a session without a slot of its own; no block"
            );
            return;
        }
        self.candidates.seen(
            agent_id,
            session,
            agent_type,
            stop,
            Instant::now(),
            self.options.correlate_for,
        );
        self.check_candidates();
    }

    /// Reads the transcripts of sessions with a candidate due, from where
    /// the last read stopped, one read per session at a time.
    /// Without an agent that reads the session's files, a candidate whose
    /// stop came gets its block from the hook data alone.
    fn check_candidates(&mut self) {
        for session in self.candidates.due_sessions(Instant::now()) {
            if self.indexing.contains(&session) {
                continue;
            }
            let path = self
                .registry
                .sessions
                .get(&session)
                .map(|entry| entry.transcript_path.clone())
                .unwrap_or_default();
            let Some(conn) = self.reader(&session) else {
                self.open_from_stops(&session);
                self.match_candidates(&session);
                continue;
            };
            if path.is_empty() {
                self.match_candidates(&session);
                continue;
            }
            let from = self
                .indexes
                .get(&session)
                .map_or(0, |index| index.resume_at(&path));
            self.ask_calls(conn, &session, path, from);
        }
    }

    /// Asks agent `conn` for the `Agent` calls of `session` from `from`.
    fn ask_calls(&mut self, conn: u64, session: &str, path: String, from: u64) {
        self.indexing.insert(session.to_owned());
        let ask = SessionAsk::Calls { from };
        let purpose = Purpose::Calls {
            session: session.to_owned(),
            path: path.clone(),
            from,
        };
        self.ask_read(conn, session, ask, purpose);
    }

    /// `Agent` calls found in a session's transcript: they are indexed and
    /// the session's candidates looked up.
    fn on_scan(&mut self, session: &str, scan: Scan) {
        self.indexing.remove(session);
        self.indexes
            .entry(session.to_owned())
            .or_default()
            .merge(scan);
        self.match_candidates(session);
        // Forget what no candidate and no running block needs.
        if self.candidates.of_session(session).is_empty()
            && !self.registry.is_live_top_level(session)
        {
            self.indexes.remove(session);
        }
    }

    /// Opens the block of each candidate of `session` whose stop came, from
    /// the hooks alone: no agent reads the parent transcript to tell a real
    /// subagent (the hook kept only stops that left subagent files).
    fn open_from_stops(&mut self, session: &str) {
        for (agent_id, candidate) in self.candidates.take_stopped(session) {
            let Some(stop) = candidate.stop else {
                continue;
            };
            let header = subagents::header(&agent_id, Some(&stop.agent_type), None);
            if !self
                .registry
                .confirm_subagent(&agent_id, session, header.clone())
            {
                continue;
            }
            info!(
                agent = short(&agent_id),
                session = short(session),
                "subagent block opened from its stop"
            );
            let input = BodyInput {
                agent_id: agent_id.clone(),
                agent_type: Some(stop.agent_type),
                report: self.reports.take(&agent_id),
                last: stop.last,
                header: Some(header),
                ..BodyInput::default()
            };
            let text = subagents::body_text(&input, None, None);
            self.finish_block(BlockKey::Agent(agent_id), text);
        }
    }

    /// Opens the block of each candidate of `session` whose `Agent` call is
    /// known now; the others wait longer or are dropped at their window's end.
    fn match_candidates(&mut self, session: &str) {
        let now = Instant::now();
        for agent_id in self.candidates.of_session(session) {
            let call = self
                .indexes
                .get(session)
                .and_then(|index| index.call(&agent_id))
                .cloned();
            match call {
                Some(call) => self.confirm(&agent_id, call),
                None => {
                    if self
                        .candidates
                        .missed(&agent_id, now, self.options.recheck_after)
                    {
                        debug!(
                            agent = short(&agent_id),
                            "subagent never matched an Agent call of its parent; no block"
                        );
                    }
                }
            }
        }
    }

    fn confirm(&mut self, agent_id: &str, call: AgentCall) {
        let Some(candidate) = self.candidates.take(agent_id) else {
            return;
        };
        let header = subagents::header(
            agent_id,
            call.subagent_type
                .as_deref()
                .or(Some(candidate.agent_type.as_str())),
            call.description.as_deref(),
        );
        if !self
            .registry
            .confirm_subagent(agent_id, &candidate.session, header)
        {
            return;
        }
        info!(
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
    }

    /// Reads the finished subagent's files off the actor; the text comes
    /// back as [`Done::Body`]. A newer stop of the same agent replaces one
    /// still waiting; see [`Self::start_body_reads`].
    fn read_body(&mut self, agent_id: &str, stop: Stopped) {
        let Some(entry) = self.registry.subagents.get(agent_id) else {
            return;
        };
        let call = self
            .indexes
            .get(&entry.parent_session)
            .and_then(|index| index.call(agent_id))
            .cloned()
            .unwrap_or_default();
        let input = BodyInput {
            agent_id: agent_id.to_owned(),
            agent_type: call.subagent_type.or(Some(stop.agent_type)),
            description: call.description,
            agent_path: stop.agent_path,
            report: self.reports.take(agent_id),
            last: stop.last,
            header: Some(entry.block.header.clone()).filter(|header| !header.is_empty()),
        };
        self.bodies_parked.remove(agent_id);
        self.bodies_retried.remove(agent_id);
        self.bodies_waiting.insert(agent_id.to_owned(), input);
        self.start_body_reads();
    }

    /// Starts waiting reads, at most [`MAX_BODY_READS`] at a time and one per
    /// agent. A read that ends while a newer stop of its agent waits is
    /// stale and dropped ([`Done::Body`]), so the newest stop always wins.
    /// A handed-back report, or a parent without an agent that reads,
    /// makes the text from the hook data alone, at once.
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
            let parent = self
                .registry
                .subagents
                .get(&agent_id)
                .map(|entry| entry.parent_session.clone());
            let reader = parent
                .as_deref()
                .and_then(|parent| self.reader(parent))
                .filter(|_| input.report.is_none() && !input.agent_path.is_empty());
            let (Some(conn), Some(parent)) = (reader, parent) else {
                let text = subagents::body_text(&input, None, None);
                self.finish_block(BlockKey::Agent(agent_id), text);
                continue;
            };
            self.bodies_reading.insert(agent_id.clone());
            let ask = SessionAsk::Subagent {
                agent_id: agent_id.clone(),
                agent_type: input.agent_type.clone(),
                description: input.description.clone(),
                header: input.header.clone(),
                last: input.last.clone(),
            };
            let purpose = Purpose::Body {
                input,
                text: String::new(),
            };
            self.ask_read(conn, &parent, ask, purpose);
        }
    }

    /// A subagent's block text came, from its agent or made here: shown
    /// unless a newer stop of the same agent waits.
    fn body_done(&mut self, agent_id: String, text: String) {
        self.bodies_reading.remove(&agent_id);
        self.bodies_retried.remove(&agent_id);
        if !text.is_empty() && !self.bodies_waiting.contains_key(&agent_id) {
            self.finish_block(BlockKey::Agent(agent_id), text);
        }
        self.start_body_reads();
    }

    /// Shows the final `text` in the block; a text too long for one
    /// message is cut there and sent whole as a file after it.
    fn finish_block(&mut self, key: BlockKey, text: String) {
        let (shown, whole) = subagents::fit(text);
        if let Some(whole) = whole {
            let thread_id = self
                .registry
                .block_slot(&key)
                .and_then(|slot| self.registry.slot(slot))
                .and_then(|slot| slot.topic_id);
            if let Some(thread_id) = thread_id {
                let (kind, id) = match &key {
                    BlockKey::Agent(id) => ("subagent", id),
                    BlockKey::Nested(id) => ("nested", id),
                };
                self.send_messages(vec![Op::SendDocument {
                    thread_id: Some(thread_id),
                    document: Document {
                        file_name: format!("{kind}-{}.txt", short(id)),
                        bytes: whole.into_bytes(),
                        caption: None,
                    },
                    notify: false,
                }]);
            }
        }
        self.registry.show_block(&key, shown, false);
    }

    /// Asks the session's agent for the first ai-title past the last scan.
    /// Without an agent that reads, the title stays the short id.
    fn read_title(&mut self, session: String, path: String) {
        let Some(conn) = self.reader(&session) else {
            return;
        };
        let failed = self.title_failures.get(&session);
        if failed.is_some_and(|&(asked, count)| asked == conn && count >= MAX_TITLE_FAILURES) {
            return;
        }
        if !self.reading.insert(session.clone()) {
            return;
        }
        // Only the tail written since the last scan of the same file.
        let from = match self.scanned.get(&session) {
            Some((scanned_path, offset)) if *scanned_path == path => *offset,
            _ => 0,
        };
        let ask = SessionAsk::Title { from };
        let purpose = Purpose::Title {
            session: session.clone(),
            path,
            conn,
        };
        self.ask_read(conn, &session, ask, purpose);
    }

    fn on_title(&mut self, session: String, path: String, title: Option<String>, scanned: u64) {
        self.reading.remove(&session);
        self.title_failures.remove(&session);
        match title {
            Some(title) => {
                self.scanned.remove(&session);
                self.registry.set_title(&session, &title);
            }
            // A session that ended during the scan is not scanned again.
            None if self
                .registry
                .sessions
                .get(&session)
                .is_some_and(|entry| !entry.ended) =>
            {
                self.scanned.insert(session, (path, scanned));
            }
            None => {}
        }
    }

    /// The agent that reads `session`'s files: bound to it, still open, not
    /// leaving, and it announced `session_reads`.
    fn reader(&self, session: &str) -> Option<u64> {
        let conn = self.registry.sessions.get(session)?.agent?;
        let bound = self.conns.get(&conn)?;
        (bound.session_reads && !bound.leaving && bound.session == session).then_some(conn)
    }

    /// Sends `ask` for the files of `session` to agent `conn`, which finds
    /// them in its own project folder. A link queue that is full or gone
    /// fails the read at once ([`Self::read_failed`]).
    fn ask_read(&mut self, conn: u64, session: &str, ask: SessionAsk, purpose: Purpose) {
        // Random, not counted: an agent sends answers it could not write
        // after its next registration, maybe to a restarted hub.
        let read_id = crate::wire::random_u64();
        let read = HubMsg::SessionRead {
            read_id,
            session_id: session.to_owned(),
            ask,
        };
        let sent = self
            .conns
            .get(&conn)
            .is_some_and(|bound| bound.to_agent.try_send(read).is_ok());
        if !sent {
            self.read_failed(purpose, Unavailable::LinkLost);
            return;
        }
        let until = Instant::now() + self.options.read_wait;
        self.reads.insert(
            read_id,
            Pending {
                conn,
                until,
                purpose,
            },
        );
    }

    /// One answer to session read `read_id`. Only the agent asked counts.
    fn on_session_answer(&mut self, conn: u64, read_id: u64, answer: SessionAnswer) {
        let Some(pending) = self.reads.get_mut(&read_id).filter(|p| p.conn == conn) else {
            debug!(conn, "session answer nobody waits for");
            return;
        };
        if let (
            Purpose::Command { text, .. } | Purpose::Body { text, .. },
            SessionAnswer::Text { text: piece, more },
        ) = (&mut pending.purpose, &answer)
        {
            if text.len() + piece.len() > MAX_READ_TEXT {
                if let Some(pending) = self.reads.remove(&read_id) {
                    warn!(conn, "session read longer than allowed; dropped");
                    self.read_failed(pending.purpose, Unavailable::TooLarge);
                }
                return;
            }
            text.push_str(piece);
            if *more {
                pending.until = Instant::now() + self.options.read_wait;
                return;
            }
        }
        let Some(pending) = self.reads.remove(&read_id) else {
            return;
        };
        match (pending.purpose, answer) {
            (Purpose::Command { ask, session, text }, SessionAnswer::Text { .. }) => {
                let _ = ask
                    .answer
                    .send(commands::transcript_reply(&ask.command, &session, text));
            }
            (Purpose::Body { input, text }, SessionAnswer::Text { .. }) => {
                self.body_done(input.agent_id, text);
            }
            (Purpose::Title { session, path, .. }, SessionAnswer::Title { title, scanned }) => {
                self.on_title(session, path, title, scanned);
            }
            (
                Purpose::Calls { session, path, .. },
                SessionAnswer::Calls {
                    offset,
                    calls,
                    links,
                    more,
                },
            ) => {
                let scan = Scan {
                    path: path.clone(),
                    offset,
                    calls: calls
                        .into_iter()
                        .map(|call| {
                            let found = AgentCall {
                                subagent_type: call.subagent_type,
                                description: call.description,
                            };
                            (call.id, found)
                        })
                        .collect(),
                    links: links
                        .into_iter()
                        .map(|link| (link.agent_id, link.tool_use_id))
                        .collect(),
                };
                match self.reader(&session).filter(|_| more) {
                    // Lines past the batch: indexed so far, read on at once.
                    Some(conn) => {
                        self.indexes.entry(session.clone()).or_default().merge(scan);
                        self.ask_calls(conn, &session, path, offset);
                    }
                    None => self.on_scan(&session, scan),
                }
            }
            (purpose, answer) => self.read_failed(purpose, failure(&answer)),
        }
    }

    /// A session read that brought nothing: the command gets a notice, a
    /// title waits for its next turn (not after [`MAX_TITLE_FAILURES`] in a
    /// row of one agent). An `Agent` call scan the agent cannot give opens
    /// the blocks of stopped subagents from the hooks, as without an agent
    /// that reads; a lost link or a late answer is only a miss. A block text
    /// cut by a lost link or a late answer waits once for the parent's next
    /// agent ([`Self::retry_bodies`]); else it comes from the hook data alone.
    fn read_failed(&mut self, purpose: Purpose, why: Unavailable) {
        let passing = matches!(why, Unavailable::LinkLost | Unavailable::NoAnswer);
        match purpose {
            Purpose::Command { ask, session, .. } => {
                info!(session = short(&session), ?why, "transcript read failed");
                let _ = ask.answer.send(commands::unavailable(why, &session));
            }
            Purpose::Title { session, conn, .. } => {
                self.reading.remove(&session);
                let failed = self.title_failures.entry(session).or_insert((conn, 0));
                if failed.0 != conn {
                    *failed = (conn, 0);
                }
                failed.1 += 1;
            }
            Purpose::Calls {
                session,
                path,
                from,
            } => {
                if !passing {
                    self.open_from_stops(&session);
                }
                self.on_scan(&session, Scan::nothing(path, from));
            }
            Purpose::Body { input, .. } => {
                debug!(
                    agent = short(&input.agent_id),
                    ?why,
                    "subagent files not read"
                );
                let agent_id = input.agent_id.clone();
                if passing
                    && !self.bodies_waiting.contains_key(&agent_id)
                    && self.bodies_retried.insert(agent_id.clone())
                {
                    self.bodies_reading.remove(&agent_id);
                    let until = Instant::now() + BODY_RETRY_WAIT;
                    self.bodies_parked.insert(agent_id, (input, until));
                    self.start_body_reads();
                    return;
                }
                let text = subagents::body_text(&input, None, None);
                self.body_done(agent_id, text);
            }
        }
    }

    /// Block texts parked by [`Self::read_failed`] go to the parent's agent
    /// that reads now (the next one after a TASK-040 swap); once the parent
    /// ended or [`BODY_RETRY_WAIT`] passed, they come from the hook data.
    fn retry_bodies(&mut self) {
        if self.bodies_parked.is_empty() {
            return;
        }
        let now = Instant::now();
        let agents: Vec<String> = self.bodies_parked.keys().cloned().collect();
        for agent_id in agents {
            let parent = self
                .registry
                .subagents
                .get(&agent_id)
                .map(|entry| entry.parent_session.clone());
            let ready = parent
                .as_deref()
                .is_some_and(|parent| self.reader(parent).is_some());
            let gone = parent
                .as_deref()
                .is_none_or(|parent| !self.registry.is_live_top_level(parent));
            let late = self
                .bodies_parked
                .get(&agent_id)
                .is_some_and(|(_, until)| *until <= now);
            if !(ready || gone || late) {
                continue;
            }
            let Some((input, _)) = self.bodies_parked.remove(&agent_id) else {
                continue;
            };
            if ready {
                self.bodies_waiting.entry(agent_id).or_insert(input);
            } else {
                let text = subagents::body_text(&input, None, None);
                self.body_done(agent_id, text);
            }
        }
        self.start_body_reads();
    }

    /// The reads out to agent `conn` fail: its link closed, or it leaves.
    fn fail_reads_of(&mut self, conn: u64) {
        let ids: Vec<u64> = self
            .reads
            .iter()
            .filter(|(_, pending)| pending.conn == conn)
            .map(|(id, _)| *id)
            .collect();
        for id in ids {
            if let Some(pending) = self.reads.remove(&id) {
                self.read_failed(pending.purpose, Unavailable::LinkLost);
            }
        }
    }

    /// `/brief` or `/full`: the session's agent renders it, or the ask gets
    /// the notice why it cannot.
    fn on_transcript_ask(&mut self, ask: TranscriptAsk) {
        let prefix = ask.command.session_prefix.as_deref();
        let session = match commands::resolve(&self.registry, ask.thread_id, prefix) {
            Ok(session) => session,
            Err(notice) => {
                let _ = ask.answer.send(Prepared::Notice(notice));
                return;
            }
        };
        let conn = match self.transcript_reader(&session) {
            Ok(conn) => conn,
            Err(why) => {
                info!(session = short(&session), ?why, "transcript not available");
                let _ = ask.answer.send(commands::unavailable(why, &session));
                return;
            }
        };
        let read = SessionAsk::Render {
            view: ask.command.view.wire(),
            prompts: u32::try_from(ask.command.prompts).unwrap_or(u32::MAX),
        };
        let purpose = Purpose::Command {
            ask,
            session: session.clone(),
            text: String::new(),
        };
        self.ask_read(conn, &session, read, purpose);
    }

    /// The agent that renders `/brief` of `session`, or why there is none.
    fn transcript_reader(&self, session: &str) -> Result<u64, Unavailable> {
        let entry = self
            .registry
            .sessions
            .get(session)
            .ok_or(Unavailable::NoAgent)?;
        if matches!(entry.kind, SessionKind::Nested { .. }) {
            return Err(Unavailable::Nested);
        }
        if entry.ended {
            return Err(Unavailable::Ended);
        }
        let bound = entry.agent.and_then(|conn| self.conns.get(&conn));
        if !bound.is_some_and(|bound| !bound.leaving && bound.session == session) {
            return Err(Unavailable::NoAgent);
        }
        let conn = self.reader(session).ok_or(Unavailable::OldAgent)?;
        if entry.transcript_path.is_empty() {
            return Err(Unavailable::NoTranscript);
        }
        Ok(conn)
    }

    fn on_control(&mut self, control: Control) {
        let (thread_id, message_id) = match control {
            Control::TopicEdited {
                thread_id,
                message_id,
            } => (thread_id, message_id),
            Control::Message(input) => return self.on_topic_message(input),
            Control::Callback(input) => return self.on_callback(input),
            Control::Pinned { message_id, pinned } => return self.on_pinned(message_id, pinned),
            // Handled by `run`.
            Control::Stop => return,
        };
        if !self.options.can_delete
            || thread_id
                .and_then(|t| self.registry.slot_by_topic(t))
                .is_none()
        {
            return;
        }
        self.hand_off(Work::Delete, Op::Delete { message_id });
    }

    /// The service message about pinning a slot's status message goes, like
    /// `forum_topic_edited`; other pins are the users' business.
    fn on_pinned(&mut self, message_id: i64, pinned: i64) {
        if self.options.can_delete && self.status_slot(pinned).is_some() {
            self.hand_off(Work::Delete, Op::Delete { message_id });
        }
    }

    /// The slot whose status message is `message_id`.
    fn status_slot(&self, message_id: i64) -> Option<SlotId> {
        self.registry
            .slots
            .iter()
            .position(|slot| {
                slot.status
                    .is_some_and(|status| status.message_id == message_id)
            })
            .map(SlotId)
    }

    /// Keeps a topic message in its slot and hands the slot's messages to
    /// the agent of its live current session, when there is one. General and
    /// topics that are not slots reach no agent and get no answer.
    fn on_topic_message(&mut self, input: Inbound) {
        let Some(thread_id) = input.thread_id else {
            debug!("message outside a topic; not forwarded");
            return;
        };
        let Some(input) = self.hold(thread_id, input) else {
            return;
        };
        // An answer to an open question never goes to the session.
        if let Some(text) = input.text.as_deref()
            && !input.forwarded
            && self.answer_question(thread_id, input.reply_to, text)
        {
            return;
        }
        let Some(slot) = self.registry.slot_by_topic(thread_id) else {
            debug!("message in a topic without a slot; not forwarded");
            return;
        };
        let (text, file) = match (input.text, input.media) {
            (Some(text), _) => (text, None),
            (None, Some(media)) => {
                let size = media.file.size.unwrap_or_default();
                if size > files::MAX_DOWNLOAD {
                    info!(
                        ordinal = self.ordinal(slot),
                        kind = media.file.kind.as_str(),
                        size,
                        "file from the topic larger than a bot may download"
                    );
                    self.notify(slot, thread_id, buffer::TOO_BIG_NOTICE);
                    return;
                }
                (media.caption.unwrap_or_default(), Some(media.file))
            }
            (None, None) => {
                self.notify(slot, thread_id, buffer::UNSUPPORTED_NOTICE);
                return;
            }
        };
        // A forward is someone else's words and a caption goes with a file:
        // neither is a command.
        if !input.forwarded
            && file.is_none()
            && let Some(command) = console::classify(&text)
        {
            // The burst gathered before the command goes first.
            self.end_gather(slot);
            self.flush(slot);
            self.on_console_command(slot, thread_id, input.message_id, command);
            return;
        }
        if file.is_some() {
            self.end_gather(slot);
        } else {
            self.gather(slot, input.message_id);
        }
        self.park(
            slot,
            Parked {
                message_id: input.message_id,
                thread_id,
                text,
                reply_to: input.reply_to,
                quote: input.quote,
                forwarded: input.forwarded,
                file,
                from_name: input.from_name,
            },
        );
        self.flush(slot);
    }

    /// Text message `message_id` for the live session of `slot` joins the
    /// burst being gathered there or starts one; a burst already due is not
    /// held back again. No burst without `Options::gather_quiet` or a live
    /// agent.
    fn gather(&mut self, slot: SlotId, message_id: i64) {
        let quiet = self.options.gather_quiet;
        if quiet.is_zero() || self.live_agent(slot).is_none() {
            return;
        }
        let now = Instant::now();
        let max = self.options.gather_max;
        match self.gathers.get_mut(&slot) {
            Some(gather) if gather.due <= now => {}
            Some(gather) => gather.due = (now + quiet).min(gather.first + max),
            None => {
                self.gathers.insert(
                    slot,
                    Gather {
                        start: message_id,
                        first: now,
                        due: now + quiet.min(max),
                    },
                );
            }
        }
    }

    /// A file or a command comes after the burst of `slot`: it goes now.
    fn end_gather(&mut self, slot: SlotId) {
        if let Some(gather) = self.gathers.get_mut(&slot) {
            gather.due = gather.due.min(Instant::now());
        }
    }

    /// Adds a message to the slot's buffer. A full buffer drops its oldest
    /// message and tells the topic once per offline period; a running
    /// session without an agent is told once per period too (a dead one
    /// gets its Resume message from [`Self::offer_resume`]).
    fn park(&mut self, slot: SlotId, parked: Parked) {
        let ordinal = self.ordinal(slot);
        let offline = self.live_agent(slot).is_none();
        let dead = self.registry.state(slot) == SlotState::Dead;
        let thread_id = parked.thread_id;
        let Some(entry) = self.registry.slot_mut(slot) else {
            return;
        };
        // The file on its way to the agent is not the one an overflow drops.
        let dropped = entry.buffer.push(parked, self.fetching.contains_key(&slot));
        let tell_overflow = dropped && !entry.buffer.overflow_told;
        let tell_queued = offline && !dead && !entry.buffer.queued_told;
        self.registry.dirty = true;
        if offline {
            info!(
                ordinal,
                "message kept for the slot until a session is on line"
            );
        }
        if dropped {
            debug!(ordinal, "slot buffer full; its oldest message dropped");
        }
        if tell_overflow
            && self.send_messages(vec![message_op(
                thread_id,
                buffer::OVERFLOW_NOTICE.to_owned(),
            )])
        {
            info!(ordinal, "slot buffer full; the topic is told once");
            if let Some(entry) = self.registry.slot_mut(slot) {
                entry.buffer.overflow_told = true;
            }
        }
        if tell_queued
            && self.send_messages(vec![message_op(
                thread_id,
                buffer::QUEUED_NOTICE.to_owned(),
            )])
            && let Some(entry) = self.registry.slot_mut(slot)
        {
            entry.buffer.queued_told = true;
        }
    }

    /// Hands the kept messages of `slot` to the agent of its live top-level
    /// current session, oldest first, until one does not fit the link
    /// queue (the rest wait for the next try). A message leaves the buffer
    /// when its link queue took it. The texts of a burst wait until it is
    /// due and go as one inbound. An emptied buffer ends the slot's offline
    /// period: the Resume button goes away.
    fn flush(&mut self, slot: SlotId) {
        let Some((session, conn)) = self.live_agent(slot) else {
            // What waits for a session that comes back goes one by one.
            self.gathers.remove(&slot);
            return;
        };
        let ordinal = self.ordinal(slot);
        let mut handed = 0;
        while let Some(parked) = self
            .registry
            .slot(slot)
            .and_then(|entry| entry.buffer.messages.front())
            .cloned()
        {
            // The messages that leave the slot now; `delivered`: they
            // reached the agent.
            let (taken, delivered) = match parked.file.clone() {
                Some(file) => match self.hand_file(slot, &session, conn, &parked, file) {
                    FileStep::Wait => break,
                    FileStep::Gone { delivered } => (vec![parked], delivered),
                },
                None => {
                    let parts = match self.gathers.get(&slot) {
                        // Kept before the burst began (the link queue had
                        // no room): it goes alone as soon as there is room.
                        Some(gather) if self.before_burst(slot, gather.start) => vec![parked],
                        Some(gather) if gather.due > Instant::now() => break,
                        Some(_) => self.burst(slot),
                        None => vec![parked],
                    };
                    let inbound = HubMsg::Inbound {
                        content: buffer::burst_content(&parts),
                        meta: self.burst_meta(&session, &parts),
                    };
                    let sent = self
                        .conns
                        .get(&conn)
                        .is_some_and(|bound| bound.to_agent.try_send(inbound).is_ok());
                    if !sent {
                        debug!(
                            ordinal,
                            session = short(&session),
                            "agent queue full or closed; messages stay in the slot"
                        );
                        break;
                    }
                    self.mark_handed(&session);
                    self.expect_taken(&session);
                    (parts, true)
                }
            };
            let Some(entry) = self.registry.slot_mut(slot) else {
                return;
            };
            entry.buffer.messages.drain(..taken.len());
            // The burst is out once no text of it is left at the front (a
            // burst too big for one inbound goes on with the next).
            if entry
                .buffer
                .messages
                .front()
                .is_none_or(|next| next.file.is_some())
            {
                self.gathers.remove(&slot);
            }
            self.registry.dirty = true;
            if !delivered {
                continue;
            }
            handed += taken.len();
            let ids: Vec<i64> = taken.iter().map(|parked| parked.message_id).collect();
            if let Some(stream) = self
                .registry
                .sessions
                .get_mut(&session)
                .and_then(|entry| entry.stream.as_mut())
            {
                stream::receipt_parts(stream, &ids);
            }
            for &id in &ids {
                self.react(id, stream::ACCEPTED);
            }
            if ids.len() > 1 {
                info!(
                    ordinal,
                    session = short(&session),
                    parts = ids.len(),
                    "messages forwarded to the session agent as one"
                );
            } else {
                info!(
                    ordinal,
                    session = short(&session),
                    "message forwarded to the session agent"
                );
            }
        }
        let Some(entry) = self.registry.slot_mut(slot) else {
            return;
        };
        if !entry.buffer.messages.is_empty() || entry.buffer.is_idle() {
            return;
        }
        let period = entry.buffer.resume.is_some() || entry.buffer.queued_told;
        let note = entry.buffer.close();
        self.registry.dirty = true;
        if period {
            info!(
                ordinal,
                session = short(&session),
                handed,
                "kept messages handed to the slot's session; offline period over"
            );
        }
        if let Some(message_id) = note.and_then(|note| note.message_id) {
            self.drop_resume_button(message_id);
        }
    }

    /// The kept file at the front of `slot` for the agent `conn` of its
    /// live session: handed to the download task (it waits in the slot
    /// until [`Self::on_fetched`]), or, for an agent that takes no files,
    /// dropped with a notice while its caption goes as text.
    fn hand_file(
        &mut self,
        slot: SlotId,
        session: &str,
        conn: u64,
        parked: &Parked,
        file: Attachment,
    ) -> FileStep {
        if self.fetching.contains_key(&slot) {
            return FileStep::Wait;
        }
        let Some(bound) = self.conns.get(&conn) else {
            return FileStep::Wait;
        };
        let ordinal = self.ordinal(slot);
        let kind = file.kind.as_str();
        if !bound.files {
            if !parked.text.is_empty()
                && bound
                    .to_agent
                    .try_send(self.inbound(session, parked))
                    .is_err()
            {
                return FileStep::Wait;
            }
            info!(ordinal, kind, "agent takes no files; the file is dropped");
            self.notify(slot, parked.thread_id, buffer::OLD_AGENT_NOTICE);
            return FileStep::Gone {
                delivered: !parked.text.is_empty(),
            };
        }
        if bound.to_agent.is_closed() {
            return FileStep::Wait;
        }
        let Some(fetcher) = &self.fetcher else {
            warn!(
                ordinal,
                kind, "no download task; a file from the topic is dropped"
            );
            self.notify(slot, parked.thread_id, buffer::FETCH_FAILED_NOTICE);
            return FileStep::Gone { delivered: false };
        };
        let transfer_id = self.transfers + 1;
        let job = fetch::Job {
            slot,
            transfer_id,
            file,
            content: parked.content(),
            meta: self.inbound_meta(session, parked),
            to_agent: bound.to_agent.clone(),
        };
        if fetcher.try_send(job).is_err() {
            debug!(ordinal, "download queue full; the file waits in the slot");
            return FileStep::Wait;
        }
        self.transfers = transfer_id;
        self.fetching.insert(
            slot,
            Fetching {
                transfer_id,
                message_id: parked.message_id,
                conn,
            },
        );
        info!(
            ordinal,
            session = short(session),
            kind,
            "file of a kept message being fetched for the session agent"
        );
        FileStep::Wait
    }

    /// The download task is done with the kept file of `slot`: it leaves
    /// the slot, and the topic hears of a file that did not go. It stays
    /// for the next agent when its link closed first (up to
    /// [`MAX_LINK_LOSSES`] times in a row) or when it went to a link that
    /// is no longer the slot's live agent (its session ended meanwhile:
    /// TASK-017, the old link never takes the slot's kept messages).
    fn on_fetched(&mut self, slot: SlotId, transfer_id: u64, outcome: Fetched) {
        let Some(fetching) = self
            .fetching
            .get(&slot)
            .copied()
            .filter(|fetching| fetching.transfer_id == transfer_id)
        else {
            return;
        };
        self.fetching.remove(&slot);
        let message_id = fetching.message_id;
        let ordinal = self.ordinal(slot);
        let to_live = self
            .live_agent(slot)
            .is_some_and(|(_, conn)| conn == fetching.conn);
        match outcome {
            Fetched::Handed { .. } if !to_live => {
                debug!(
                    ordinal,
                    "file handed to a link that is no longer the slot's agent; it stays in the slot"
                );
                // A newer agent of the slot may be waiting for it.
                self.flush(slot);
                return;
            }
            Fetched::LinkClosed => {
                let losses = match self.link_losses.get(&slot) {
                    Some(&(id, losses)) if id == message_id => losses + 1,
                    _ => 1,
                };
                if losses < MAX_LINK_LOSSES {
                    self.link_losses.insert(slot, (message_id, losses));
                    debug!(
                        ordinal,
                        losses,
                        "agent link closed before the file was handed over; it stays in the slot"
                    );
                    self.flush(slot);
                    return;
                }
                warn!(
                    ordinal,
                    losses, "agent link closed during every hand-over of a file; it is dropped"
                );
            }
            _ => {}
        }
        self.link_losses.remove(&slot);
        // Unless it left the slot meanwhile.
        let front = self
            .registry
            .slot(slot)
            .and_then(|entry| entry.buffer.messages.front())
            .map(|parked| (parked.message_id, parked.thread_id));
        let Some((_, thread_id)) = front.filter(|(id, _)| *id == message_id) else {
            return;
        };
        if let Some(entry) = self.registry.slot_mut(slot) {
            entry.buffer.messages.pop_front();
        }
        self.registry.dirty = true;
        match outcome {
            Fetched::Handed { size } => {
                info!(
                    ordinal,
                    size, "file of a kept message handed to the session agent"
                );
                if let Some((session, _)) = self.live_agent(slot)
                    && let Some(stream) = self
                        .registry
                        .sessions
                        .get_mut(&session)
                        .and_then(|entry| entry.stream.as_mut())
                {
                    stream::receipt(stream, message_id);
                }
                self.react(message_id, stream::ACCEPTED);
            }
            Fetched::TooBig => self.notify(slot, thread_id, buffer::TOO_BIG_NOTICE),
            Fetched::Failed => self.notify(slot, thread_id, buffer::FETCH_FAILED_NOTICE),
            Fetched::LinkClosed => self.notify(slot, thread_id, buffer::LINK_LOST_NOTICE),
        }
        // The next kept message goes, or the offline period ends.
        self.flush(slot);
    }

    /// A `file_offer` from `conn`: accepted when its session is the live
    /// one of a slot with a topic, the size is one Telegram takes and less
    /// than [`MAX_FILE_BYTES`] would wait. A new offer drops an unfinished
    /// file of the same link and any file idle for [`UPLOAD_IDLE`].
    fn on_file_offer(&mut self, conn: u64, frame_session: &str, offer: Offer) {
        let Offer {
            transfer_id,
            name,
            size,
            caption,
            parts,
        } = offer;
        if let Some(old) = self.uploads.remove(&conn) {
            self.file_bytes = self.file_bytes.saturating_sub(old.assembly.size());
            debug!(
                conn,
                "an unfinished file of the agent is dropped for its next offer"
            );
        }
        let now = Instant::now();
        let idle: Vec<u64> = self
            .uploads
            .iter()
            .filter(|(_, upload)| now.duration_since(upload.touched) >= UPLOAD_IDLE)
            .map(|(&conn, _)| conn)
            .collect();
        for idle in idle {
            if let Some(upload) = self.uploads.remove(&idle) {
                self.file_bytes = self.file_bytes.saturating_sub(upload.assembly.size());
                info!(conn = idle, "file from an agent stopped coming; dropped");
                self.answer_file(idle, upload.transfer_id, FileOutcome::Failed);
            }
        }
        let target = self
            .live_reply_slot(conn, frame_session)
            .filter(|(_, slot)| {
                self.registry
                    .slot(*slot)
                    .is_some_and(|entry| entry.topic_id.is_some())
            });
        let outcome = if target.is_none() {
            FileOutcome::NoTopic
        } else if size == 0 || size > files::MAX_UPLOAD || !album_fits(&parts, size) {
            FileOutcome::Failed
        } else if self.file_bytes + size > MAX_FILE_BYTES
            || self.queued_messages >= MAX_QUEUED_MESSAGES
        {
            FileOutcome::Busy
        } else {
            FileOutcome::Accepted
        };
        info!(
            conn,
            size,
            files = parts.len().max(1),
            ?outcome,
            "file offered by an agent"
        );
        if outcome == FileOutcome::Accepted {
            self.file_bytes += size;
            self.uploads.insert(
                conn,
                Upload {
                    transfer_id,
                    name,
                    caption,
                    parts,
                    assembly: files::Assembly::new(size),
                    touched: Instant::now(),
                },
            );
        }
        self.answer_file(conn, transfer_id, outcome);
    }

    /// A chunk of the file `conn` is sending; the complete file goes to the
    /// topic of its session, checked again (the session may have ended or
    /// left its slot meanwhile).
    fn on_file_chunk(&mut self, conn: u64, frame_session: &str, chunk: &FileChunk) {
        let Some(upload) = self
            .uploads
            .get_mut(&conn)
            .filter(|upload| upload.transfer_id == chunk.transfer_id)
        else {
            debug!(conn, "chunk of a file not being received; dropped");
            return;
        };
        upload.touched = Instant::now();
        let broken = match upload.assembly.push(chunk) {
            Ok(false) => return,
            Ok(true) => None,
            Err(error) => Some(error),
        };
        let Some(upload) = self.uploads.remove(&conn) else {
            return;
        };
        let size = upload.assembly.size();
        if let Some(error) = broken {
            self.file_bytes = self.file_bytes.saturating_sub(size);
            warn!(conn, %error, "file from an agent broken; dropped");
            self.answer_file(conn, upload.transfer_id, FileOutcome::Failed);
            return;
        }
        let target = self
            .live_reply_slot(conn, frame_session)
            .and_then(|(_, slot)| {
                let thread_id = self.registry.slot(slot)?.topic_id?;
                Some((slot, thread_id))
            });
        let refused = match target {
            None => Some(FileOutcome::NoTopic),
            Some(_) if self.queued_messages >= MAX_QUEUED_MESSAGES => Some(FileOutcome::Busy),
            Some(_) => None,
        };
        let (Some((slot, thread_id)), None) = (target, refused) else {
            self.file_bytes = self.file_bytes.saturating_sub(size);
            let outcome = refused.unwrap_or(FileOutcome::NoTopic);
            info!(conn, size, ?outcome, "file from an agent not sent");
            self.answer_file(conn, upload.transfer_id, outcome);
            return;
        };
        if !upload.parts.is_empty() {
            self.send_album(conn, slot, thread_id, upload);
            return;
        }
        let bytes = upload.assembly.into_bytes();
        let photo = size <= files::MAX_PHOTO && files::is_photo(&bytes);
        let document = Document {
            file_name: files::clean_name(&upload.name, "file"),
            bytes,
            caption: upload.caption.map(|caption| cut(&caption, CAPTION_LIMIT)),
        };
        let thread_id = Some(thread_id);
        let op = if photo {
            Op::SendPhoto {
                thread_id,
                document,
                notify: false,
            }
        } else {
            Op::SendDocument {
                thread_id,
                document,
                notify: false,
            }
        };
        self.queued_messages += 1;
        info!(
            ordinal = self.ordinal(slot),
            size, photo, "file from the session queued for its topic"
        );
        self.hand_off(
            Work::File {
                conn,
                transfer_id: upload.transfer_id,
                size,
            },
            op,
        );
    }

    /// Telegram answered a file of `conn`: the agent hears whether it went.
    fn on_file_done(&mut self, conn: u64, transfer_id: u64, size: u64, delivery: Option<Delivery>) {
        self.queued_messages = self.queued_messages.saturating_sub(1);
        if self.queued_messages == 0 {
            self.overflow_warned = false;
        }
        self.file_bytes = self.file_bytes.saturating_sub(size);
        let outcome = match delivery {
            Some(Ok(_)) => {
                info!(conn, size, "file from the session sent to its topic");
                FileOutcome::Sent
            }
            Some(Err(error)) => {
                warn!(%error, size, "file from the session not delivered");
                FileOutcome::Failed
            }
            None => {
                warn!(size, "file from the session got no answer");
                FileOutcome::Failed
            }
        };
        self.answer_file(conn, transfer_id, outcome);
    }

    /// The files of a complete album offer (TASK-059) go to the topic:
    /// the pictures as a photo album, then the rest as a document album
    /// (Telegram never mixes the two); a kind with one file goes alone, as
    /// with `path`. The caption goes on the first file; without one each
    /// photo shows its file name (TASK-051). One message token per message.
    fn send_album(&mut self, conn: u64, slot: SlotId, thread_id: i64, upload: Upload) {
        let size = upload.assembly.size();
        let bytes = upload.assembly.into_bytes();
        let mut photos = Vec::new();
        let mut others = Vec::new();
        let mut offset = 0usize;
        for (index, part) in upload.parts.iter().enumerate() {
            let end = offset + part.size as usize;
            let bytes = bytes[offset..end].to_vec();
            offset = end;
            let photo = part.size <= files::MAX_PHOTO && files::is_photo(&bytes);
            let file_name = files::clean_name(&part.name, "file");
            let caption = match &upload.caption {
                Some(_) => None,
                None if photo => Some(cut(&file_name, CAPTION_LIMIT)),
                None => None,
            };
            let document = Document {
                file_name,
                bytes,
                caption,
            };
            if photo {
                photos.push((index, document));
            } else {
                others.push((index, document));
            }
        }
        let mut ops: Vec<(Vec<usize>, u64, Op)> = Vec::new();
        for (group, photo) in [(photos, true), (others, false)] {
            if group.is_empty() {
                continue;
            }
            let bytes = group.iter().map(|(_, doc)| doc.bytes.len() as u64).sum();
            let (parts, mut items): (Vec<usize>, Vec<Document>) = group.into_iter().unzip();
            if ops.is_empty()
                && let (Some(caption), Some(first)) = (&upload.caption, items.first_mut())
            {
                first.caption = Some(cut(caption, CAPTION_LIMIT));
            }
            let thread_id = Some(thread_id);
            let op = match items.len() {
                1 => {
                    let document = items.remove(0);
                    if photo {
                        Op::SendPhoto {
                            thread_id,
                            document,
                            notify: false,
                        }
                    } else {
                        Op::SendDocument {
                            thread_id,
                            document,
                            notify: false,
                        }
                    }
                }
                _ => Op::SendAlbum {
                    thread_id,
                    items,
                    photos: photo,
                    notify: false,
                },
            };
            ops.push((parts, bytes, op));
        }
        // `album_fits` checked that the sizes add up to the bytes.
        debug_assert_eq!(offset as u64, size);
        self.queued_messages += ops.len();
        info!(
            ordinal = self.ordinal(slot),
            size,
            files = upload.parts.len(),
            messages = ops.len(),
            "album from the session queued for its topic"
        );
        self.albums.insert(
            (conn, upload.transfer_id),
            Album {
                sent: vec![false; upload.parts.len()],
                left: ops.len(),
            },
        );
        for (parts, bytes, op) in ops {
            self.hand_off(
                Work::Album {
                    conn,
                    transfer_id: upload.transfer_id,
                    size: bytes,
                    parts,
                },
                op,
            );
        }
    }

    /// Telegram answered one message of an album offer; after the last one
    /// the agent hears which files went.
    fn on_album_done(
        &mut self,
        conn: u64,
        transfer_id: u64,
        size: u64,
        parts: &[usize],
        delivery: Option<Delivery>,
    ) {
        self.queued_messages = self.queued_messages.saturating_sub(1);
        if self.queued_messages == 0 {
            self.overflow_warned = false;
        }
        self.file_bytes = self.file_bytes.saturating_sub(size);
        let went = matches!(delivery, Some(Ok(_)));
        match delivery {
            Some(Ok(_)) => info!(
                conn,
                size, "album message from the session sent to its topic"
            ),
            Some(Err(error)) => warn!(%error, size, "album message from the session not delivered"),
            None => warn!(size, "album message from the session got no answer"),
        }
        let Some(album) = self.albums.get_mut(&(conn, transfer_id)) else {
            return;
        };
        for &part in parts {
            if let Some(sent) = album.sent.get_mut(part) {
                *sent = went;
            }
        }
        album.left = album.left.saturating_sub(1);
        if album.left > 0 {
            return;
        }
        let Some(album) = self.albums.remove(&(conn, transfer_id)) else {
            return;
        };
        let parts: Vec<FileOutcome> = album
            .sent
            .iter()
            .map(|&sent| {
                if sent {
                    FileOutcome::Sent
                } else {
                    FileOutcome::Failed
                }
            })
            .collect();
        let outcome = if album.sent.contains(&true) {
            FileOutcome::Sent
        } else {
            FileOutcome::Failed
        };
        self.answer(conn, transfer_id, outcome, parts);
    }

    fn answer_file(&self, conn: u64, transfer_id: u64, outcome: FileOutcome) {
        self.answer(conn, transfer_id, outcome, Vec::new());
    }

    fn answer(&self, conn: u64, transfer_id: u64, outcome: FileOutcome, parts: Vec<FileOutcome>) {
        let answer = HubMsg::FileAnswer {
            transfer_id,
            outcome,
            parts,
        };
        if self
            .conns
            .get(&conn)
            .is_none_or(|bound| bound.to_agent.try_send(answer).is_err())
        {
            debug!(conn, "agent queue full or closed; file answer dropped");
        }
    }

    /// The Inbound of a topic message for `session`: its
    /// [`Self::inbound_meta`] and [`Parked::content`].
    fn inbound(&self, session: &str, parked: &Parked) -> HubMsg {
        HubMsg::Inbound {
            content: parked.content(),
            meta: self.inbound_meta(session, parked),
        }
    }

    /// Meta `chat_id`, `message_id`, `thread_id`, `reply_to_message_id` for
    /// an explicit reply, `target_agent` for a reply to a block of its
    /// subagent, `forwarded` for a forward and `from_name` for a team
    /// member's message (TASK-036).
    fn inbound_meta(&self, session: &str, parked: &Parked) -> BTreeMap<String, String> {
        let mut meta = BTreeMap::from([
            ("chat_id".to_owned(), self.options.chat_id.to_string()),
            ("message_id".to_owned(), parked.message_id.to_string()),
            ("thread_id".to_owned(), parked.thread_id.to_string()),
        ]);
        if parked.forwarded {
            meta.insert("forwarded".to_owned(), "true".to_owned());
        }
        if let Some(name) = &parked.from_name {
            meta.insert("from_name".to_owned(), name.clone());
        }
        if let Some(reply_to) = parked.reply_to {
            meta.insert("reply_to_message_id".to_owned(), reply_to.to_string());
            // A reply to a block of this session's subagent is for that
            // subagent; Claude forwards it (channel instructions).
            if let Some(agent_id) =
                self.registry
                    .subagent_of_message(parked.thread_id, reply_to, session)
            {
                meta.insert("target_agent".to_owned(), agent_id.to_owned());
            }
        }
        meta
    }

    /// The front text of `slot` was kept before its burst's first message
    /// `start`, which still waits behind it.
    fn before_burst(&self, slot: SlotId, start: i64) -> bool {
        self.registry.slot(slot).is_some_and(|entry| {
            let mut kept = entry.buffer.messages.iter();
            kept.next().is_some_and(|front| front.message_id != start)
                && kept.any(|parked| parked.message_id == start)
        })
    }

    /// The texts at the front of `slot` that go as one inbound: up to the
    /// first file, at most [`MAX_GATHER_BYTES`] of content (the first text
    /// always), and only those that reply to the same message as the first
    /// (or all to none): one inbound has one addressee, the session or one
    /// of its subagents, and one reply target.
    fn burst(&self, slot: SlotId) -> Vec<Parked> {
        let mut parts: Vec<Parked> = Vec::new();
        let mut size = 0;
        let Some(entry) = self.registry.slot(slot) else {
            return parts;
        };
        for parked in entry
            .buffer
            .messages
            .iter()
            .take_while(|parked| parked.file.is_none())
        {
            if parts.first().is_some_and(|first| {
                (first.thread_id, first.reply_to) != (parked.thread_id, parked.reply_to)
            }) {
                break;
            }
            size += parked.content().len() + buffer::PART_SEPARATOR.len();
            if !parts.is_empty() && size > MAX_GATHER_BYTES {
                break;
            }
            parts.push(parked.clone());
        }
        parts
    }

    /// The meta of `parts` going as one inbound (all in one topic and
    /// replying to the same message, see [`Self::burst`]):
    /// [`Self::inbound_meta`] of the last one (its channel record turns them
    /// all ✍), `message_ids` of all in order when there are several,
    /// `forwarded` only when every one is a forward and `from_name` only
    /// when one person wrote every part (the content names each part's
    /// author). One part: exactly its own meta.
    fn burst_meta(&self, session: &str, parts: &[Parked]) -> BTreeMap<String, String> {
        let Some(last) = parts.last() else {
            return BTreeMap::new();
        };
        let mut meta = self.inbound_meta(session, last);
        if parts.iter().all(|parked| parked.forwarded) {
            meta.insert("forwarded".to_owned(), "true".to_owned());
        } else {
            meta.remove("forwarded");
        }
        if parts
            .iter()
            .any(|parked| parked.from_name != last.from_name)
        {
            meta.remove("from_name");
        }
        if parts.len() > 1 {
            let ids: Vec<String> = parts
                .iter()
                .map(|parked| parked.message_id.to_string())
                .collect();
            meta.insert("message_ids".to_owned(), ids.join(","));
        }
        meta
    }

    /// Every slot with kept messages tries its live session again: the
    /// session or its agent may have come (back) since.
    fn flush_all(&mut self) {
        let waiting: Vec<SlotId> = (0..self.registry.slots.len())
            .map(SlotId)
            .filter(|&slot| {
                self.registry
                    .slot(slot)
                    .is_some_and(|entry| !entry.buffer.messages.is_empty())
            })
            .collect();
        for slot in waiting {
            self.flush(slot);
        }
    }

    /// A dead slot with kept messages gets one Resume message per offline
    /// period, sent at most once (a lost one is not sent again).
    fn offer_resume(&mut self) {
        for index in 0..self.registry.slots.len() {
            let slot = SlotId(index);
            let entry = &self.registry.slots[index];
            if entry.buffer.messages.is_empty()
                || entry.buffer.resume.is_some()
                || self.registry.state(slot) != SlotState::Dead
            {
                continue;
            }
            let (Some(thread_id), Some(session)) = (entry.topic_id, entry.current_session.clone())
            else {
                continue;
            };
            if self.queued_messages >= MAX_QUEUED_MESSAGES {
                return;
            }
            let reply_markup = buffer::callback_data(&session).map(buffer::keyboard);
            if reply_markup.is_none() {
                debug!(
                    session = short(&session),
                    "session id too long for a Resume button"
                );
            }
            self.resume_sends += 1;
            let number = self.resume_sends;
            self.registry.slots[index].buffer.resume = Some(ResumeNote {
                session: session.clone(),
                number,
                message_id: None,
            });
            self.registry.dirty = true;
            self.queued_messages += 1;
            info!(
                ordinal = self.ordinal(slot),
                session = short(&session),
                "Resume button offered for the slot's kept messages"
            );
            self.hand_off(
                Work::Resume { slot, number },
                Op::Send {
                    thread_id: Some(thread_id),
                    text: buffer::resume_text(&session),
                    html: None,
                    reply_markup,
                    permission: false,
                    reply_to: None,
                    notify: false,
                },
            );
        }
    }

    /// The Resume message of a period that is over loses its button. One
    /// try: a press on a button that stayed gets a fitting answer anyway.
    fn drop_resume_button(&mut self, message_id: i64) {
        self.hand_off(
            Work::Callback,
            Op::Edit {
                message_id,
                text: buffer::RESUMED_TEXT.to_owned(),
                reply_markup: Some(permissions::no_keyboard()),
                background: false,
            },
        );
    }

    /// Telegram answered Resume message `number` of `slot`: its id is kept
    /// for the edit at the end of the period, or, when that period is over
    /// already, the button goes now.
    fn on_resume_done(&mut self, slot: SlotId, number: u64, delivery: Option<Delivery>) {
        self.queued_messages = self.queued_messages.saturating_sub(1);
        if self.queued_messages == 0 {
            self.overflow_warned = false;
        }
        let message_id = match &delivery {
            Some(Ok(Outcome::Sent(message))) if message.message_id != 0 => Some(message.message_id),
            Some(Err(error)) => {
                warn!(%error, "Resume message not delivered; offered again in the next offline period");
                None
            }
            _ => None,
        };
        let Some(message_id) = message_id else {
            return;
        };
        let waiting = self.registry.slot_mut(slot).and_then(|entry| {
            entry
                .buffer
                .resume
                .as_mut()
                .filter(|note| note.number == number && note.message_id.is_none())
        });
        match waiting {
            Some(note) => {
                note.message_id = Some(message_id);
                self.registry.dirty = true;
            }
            None => self.drop_resume_button(message_id),
        }
    }

    /// A Resume press: recorded for a dead slot whose open Resume message
    /// names `session` (a later session may have ended in that slot since)
    /// or whose ended current session it is (bringing it back is TASK-019),
    /// otherwise told why nothing happens.
    fn press_resume(&mut self, session: &str) -> &'static str {
        if self.registry.is_live_top_level(session) {
            return buffer::ANSWER_ALIVE;
        }
        let dead = |slot: &SlotId| self.registry.state(*slot) == SlotState::Dead;
        let slot = (0..self.registry.slots.len())
            .map(SlotId)
            .filter(dead)
            .find(|&slot| {
                self.registry
                    .slot(slot)
                    .and_then(|entry| entry.buffer.resume.as_ref())
                    .is_some_and(|note| note.session == session)
            })
            .or_else(|| {
                self.registry
                    .sessions
                    .get(session)
                    .and_then(|entry| entry.slot)
                    .filter(dead)
                    .filter(|&slot| {
                        self.registry
                            .slot(slot)
                            .is_some_and(|entry| entry.current_session.as_deref() == Some(session))
                    })
            });
        let Some(slot) = slot else {
            debug!("Resume button of a session that is not its slot's ended one");
            return permissions::ANSWER_EXPIRED;
        };
        let ordinal = self.ordinal(slot);
        if let Some(entry) = self.registry.slot_mut(slot)
            && !entry.buffer.resume_asked
        {
            entry.buffer.resume_asked = true;
            self.registry.dirty = true;
        }
        info!(
            ordinal,
            session = short(session),
            "Resume asked in Telegram; starting a session from there is not available yet"
        );
        buffer::ANSWER_UNAVAILABLE
    }

    /// The running top-level session of `slot` and its agent connection.
    fn live_agent(&self, slot: SlotId) -> Option<(String, u64)> {
        let session = self.registry.slot(slot)?.current_session.as_deref()?;
        if !self.registry.is_live_top_level(session) {
            return None;
        }
        let conn = self.registry.sessions.get(session)?.agent?;
        self.conns
            .contains_key(&conn)
            .then(|| (session.to_owned(), conn))
    }

    /// The running top-level session currently represented by `conn` and
    /// its slot. This is the reply-side counterpart of [`Self::live_agent`].
    fn live_reply_slot(&self, conn: u64, session: &str) -> Option<(String, SlotId)> {
        self.conns.get(&conn)?;
        self.registry
            .sessions
            .get(session)
            .filter(|entry| entry.agent == Some(conn))?;
        let slot = self.current_slot(session)?;
        Some((session.to_owned(), slot))
    }

    /// The slot of `session` when it is a running top-level session and
    /// still the current session of that slot.
    fn current_slot(&self, session: &str) -> Option<SlotId> {
        if !self.registry.is_live_top_level(session) {
            return None;
        }
        let slot = self.registry.sessions.get(session)?.slot?;
        (self.registry.slot(slot)?.current_session.as_deref() == Some(session)).then_some(slot)
    }

    /// Sends the final answer of a turn (from the `Stop` hook) to the topic
    /// of its session, like an agent reply, so the answer reaches Telegram
    /// whether or not the model called `reply`.
    fn on_turn_answer(&mut self, session: &str, answer: &str) {
        // A streamed session's blank answer still takes its turn end.
        let streamed = self.stream_target(session).is_some();
        if answer.trim().is_empty() && !streamed {
            return;
        }
        let Some(slot) = self.current_slot(session) else {
            debug!(
                session = short(session),
                "turn answer of a session that is not the live one of its slot; not sent"
            );
            return;
        };
        let ordinal = self.ordinal(slot);
        let Some(thread_id) = self.registry.slot(slot).and_then(|slot| slot.topic_id) else {
            info!(
                ordinal,
                "turn answer for a slot without a topic yet; not sent"
            );
            return;
        };
        if let Some(live) = self.streams.get_mut(session).filter(|_| streamed) {
            let now = Instant::now();
            let mut held = Held {
                thread_id,
                answer: answer.to_owned(),
                until: now + self.options.hold_answer,
                end: None,
            };
            if let Some(end) = live.claim_end(now) {
                // A turn end read already: the lines before it are handed
                // out, and the answer rides the stream behind them.
                held.end = Some(end);
                self.release(session, held);
                return;
            }
            // The lines of this turn up to its end in the transcript go
            // first, for at most `hold_answer`.
            let oldest = (live.held.len() >= stream::MAX_HELD)
                .then(|| live.held.pop_front())
                .flatten();
            live.held.push_back(held);
            live.next_read = Some(now);
            if let Some(oldest) = oldest {
                self.release(session, oldest);
            }
            return;
        }
        if answer.trim().is_empty() {
            return;
        }
        if let Some(parts) = self.send_text(thread_id, session, answer, "answer", true) {
            info!(
                ordinal,
                session = short(session),
                parts,
                "turn answer queued"
            );
        }
    }

    /// Sends a turn answer. A streamed session's answer rides its stream
    /// (see [`answer_ops`]), so it never shows before a stream line queued
    /// earlier, refused or not.
    fn release(&mut self, session: &str, held: Held) {
        let streamed = self.stream_target(session).is_some();
        let room = MAX_QUEUED_MESSAGES.saturating_sub(self.queued_messages);
        let held = match self.streams.get_mut(session).filter(|_| streamed) {
            Some(live) => match answer_ops(live, held, room) {
                Ok(ops) => {
                    self.stream_answer(session, ops);
                    return;
                }
                Err(held) => held,
            },
            None => held,
        };
        if held.answer.trim().is_empty() {
            return;
        }
        if let Some(parts) = self.send_text(held.thread_id, session, &held.answer, "answer", true) {
            info!(session = short(session), parts, "turn answer queued");
        }
    }

    /// Hands the stream messages of a turn answer to the dispatch task.
    fn stream_answer(&mut self, session: &str, ops: Vec<(u64, Op)>) {
        if ops.is_empty() {
            return;
        }
        let parts = ops.len();
        for (number, op) in ops {
            self.queued_messages += 1;
            self.hand_off(
                Work::Stream {
                    session: session.to_owned(),
                    number,
                },
                op,
            );
        }
        info!(session = short(session), parts, "turn answer queued");
    }

    /// Sets the bot's reaction on a topic message; failures are only logged.
    /// At most [`MAX_REACTIONS`] wait for Telegram; one more is skipped.
    fn react(&mut self, message_id: i64, emoji: &str) {
        if self.reactions >= MAX_REACTIONS {
            debug!("too many reactions wait for Telegram; one skipped");
            return;
        }
        self.reactions += 1;
        self.hand_off(
            Work::Reaction,
            Op::React {
                message_id,
                emoji: emoji.to_owned(),
            },
        );
    }

    /// Where the stream of `session` goes, when it may go on now: the live
    /// top-level current session of a slot whose topic exists and whose
    /// session separator is out, with a bound agent that reads transcripts.
    fn stream_target(&self, session: &str) -> Option<(u64, String)> {
        let slot = self.current_slot(session)?;
        let slot = self.registry.slot(slot)?;
        if slot.topic_id.is_none() || slot.pending_separator.is_some() {
            return None;
        }
        let entry = self.registry.sessions.get(session)?;
        entry.stream.as_ref()?;
        if entry.transcript_path.is_empty() {
            return None;
        }
        let conn = entry.agent?;
        let bound = self.conns.get(&conn)?;
        (bound.reads && bound.session == session).then(|| (conn, entry.transcript_path.clone()))
    }

    /// Asks the agents of streamed sessions for new lines, releases held
    /// answers that waited long enough or can no longer be matched, and
    /// forgets the streams of sessions that are over once nothing of theirs
    /// waits for Telegram.
    fn pump_streams(&mut self) {
        let now = Instant::now();
        let mut sessions: HashSet<String> = self
            .conns
            .values()
            .map(|bound| bound.session.clone())
            .collect();
        sessions.extend(self.streams.keys().cloned());
        for session in sessions {
            if self.current_slot(&session).is_none() {
                let Some(live) = self.streams.get_mut(&session) else {
                    continue;
                };
                let held: Vec<Held> = live.held.drain(..).collect();
                if live.unanswered() == 0 {
                    self.streams.remove(&session);
                }
                for held in held {
                    self.release(&session, held);
                }
                continue;
            }
            let target = self.stream_target(&session);
            if target.is_none() && !self.streams.contains_key(&session) {
                continue;
            }
            let (offset, calls) = self
                .registry
                .sessions
                .get(&session)
                .and_then(|entry| entry.stream.as_ref())
                .map(|stream| (stream.offset, stream.calls.clone()))
                .unwrap_or_default();
            let live = self
                .streams
                .entry(session.clone())
                .or_insert_with(|| Live::new(offset, calls));
            // A read that is late, or went to a connection that is no longer
            // the session's, is asked again.
            let conn = target.as_ref().map(|(conn, _)| *conn);
            if let Some((asked, sent)) = live.reading
                && (now >= sent + READ_TIMEOUT || conn != Some(asked))
            {
                debug!(
                    session = short(&session),
                    "transcript read not answered; asking again"
                );
                live.reading = None;
            }
            let mut released = Vec::new();
            while live
                .held
                .front()
                .is_some_and(|held| target.is_none() || now >= held.until)
            {
                if let Some(held) = live.held.pop_front() {
                    live.answered_early(&held);
                    released.push(held);
                }
            }
            for held in released {
                self.release(&session, held);
            }
            let Some((conn, path)) = target else {
                continue;
            };
            let Some(live) = self.streams.get_mut(&session) else {
                continue;
            };
            let due = live.next_read.is_none_or(|at| now >= at);
            if live.reading.is_some()
                || !due
                || live.unanswered() >= stream::MAX_WAITING
                || self.queued_messages >= STREAM_QUEUE
            {
                continue;
            }
            let read = HubMsg::TranscriptRead {
                session_id: session.clone(),
                path,
                from: live.read_at,
            };
            let asked = self
                .conns
                .get(&conn)
                .is_some_and(|bound| bound.to_agent.try_send(read).is_ok());
            if asked {
                live.reading = Some((conn, now));
            } else {
                live.next_read = Some(now + self.options.stream_every);
            }
        }
    }

    /// One answered transcript read: its messages go to the topic in order,
    /// its reactions out, a held answer after the lines of its turn.
    fn on_chunk(&mut self, conn: u64, session: &str, chunk: &Chunk<'_>) {
        let &Chunk {
            from,
            to,
            lines,
            missing,
            more,
            reset,
        } = chunk;
        enum Action {
            Stream(u64, Op),
            Answer(Vec<(u64, Op)>),
            Release(Held),
            React(i64),
        }
        let now = Instant::now();
        let every = self.options.stream_every;
        let hold = self.options.hold_answer;
        let thread_id = self
            .current_slot(session)
            .and_then(|slot| self.registry.slot(slot))
            .and_then(|slot| slot.topic_id);
        let Some(live) = self.streams.get_mut(session) else {
            return;
        };
        if live.reading.is_none_or(|(asked, _)| asked != conn) {
            debug!(
                session = short(session),
                "transcript chunk nobody asked for; dropped"
            );
            return;
        }
        live.reading = None;
        live.next_read = Some(now + every);
        let Some(thread_id) = thread_id else {
            return;
        };
        if live.read_at.is_some_and(|at| at != from) || to < from {
            debug!(
                session = short(session),
                "transcript chunk out of place; asking again"
            );
            live.next_read = Some(now);
            return;
        }
        if missing {
            if !live.missing_warned {
                live.missing_warned = true;
                if !live.file_seen && live.read_at == Some(0) {
                    // A new session: the file comes with its first prompt.
                    debug!(
                        session = short(session),
                        "session transcript not written yet; the stream waits for it"
                    );
                } else {
                    warn!(
                        session = short(session),
                        "session transcript not found; the stream waits for it"
                    );
                }
            }
            // No turn end can come from a file that is not there.
            let held: Vec<Held> = live.held.drain(..).collect();
            for held in held {
                self.release(session, held);
            }
            // Nor any channel record.
            self.check_channel(session);
            return;
        }
        live.missing_warned = false;
        live.file_seen = true;
        if reset {
            if !live.reset_warned {
                live.reset_warned = true;
                warn!(
                    session = short(session),
                    "session transcript was cut or replaced; the stream reads it again from its start"
                );
            }
            live.read_at = Some(0);
            live.calls.clear();
            live.next_read = Some(now);
            return;
        }
        live.reset_warned = false;
        let Some(stream) = self
            .registry
            .sessions
            .get_mut(session)
            .and_then(|entry| entry.stream.as_mut())
        else {
            return;
        };
        if stream.offset.is_none() {
            // The first read of a stream that starts at the end of the file.
            stream.offset = Some(from);
            self.registry.dirty = true;
        }
        let mut actions = Vec::new();
        let mut queued = self.queued_messages;
        // Answers held again whose turn end this read starts after go first.
        for held in live.overdue(from) {
            match answer_ops(live, held, MAX_QUEUED_MESSAGES.saturating_sub(queued)) {
                Ok(ops) => {
                    queued += ops.len();
                    actions.push(Action::Answer(ops));
                }
                Err(held) => actions.push(Action::Release(held)),
            }
        }
        let mut read_to = to;
        let mut stopped = false;
        let mut interrupted = false;
        // Signs that texts handed to the session were taken (TASK-052): a
        // channel record (any: the channel works) or a turn typed here.
        let mut channel_seen = false;
        let mut turn_seen = false;
        for (index, line) in lines.iter().enumerate() {
            // The first line always goes (the read was asked with room); the
            // rest wait in the file while Telegram is behind.
            if index > 0 && (live.unanswered() >= stream::MAX_WAITING || queued >= STREAM_QUEUE) {
                read_to = lines[index - 1].end;
                stopped = true;
                break;
            }
            if let Some(activity) = self.activity.get_mut(session) {
                for item in &line.items {
                    match item {
                        // A call that got its result is over, also when no
                        // PostToolUse came (a denied permission fires none).
                        StreamItem::Result { id, .. } => activity.tool_end(id),
                        StreamItem::Note { text } if text.starts_with(status::INTERRUPT_NOTE) => {
                            interrupted |= activity.interrupted_at(line.end);
                        }
                        _ => {}
                    }
                }
            }
            channel_seen |= line
                .items
                .iter()
                .any(|item| matches!(item, StreamItem::Channel { .. }));
            for step in stream::apply_line(&mut live.calls, &mut stream.receipts, &line.items) {
                match step {
                    Step::Send {
                        text,
                        merge,
                        format,
                    } => {
                        let chunks = stream_chunks(&text, format);
                        let merge = merge && chunks.len() == 1;
                        for (text, html) in chunks {
                            queued += 1;
                            let op = Op::Stream {
                                thread_id,
                                text,
                                html,
                                merge,
                                restart: std::mem::take(&mut live.restart),
                                notify: false,
                            };
                            actions.push(Action::Stream(live.sent(), op));
                        }
                    }
                    Step::Working(message_id) => {
                        live.lapse_ends(now + hold);
                        self.registry.dirty = true;
                        actions.push(Action::React(message_id));
                        // The messages that went as one inbound with it.
                        for part in stream::take_parts(stream, message_id) {
                            actions.push(Action::React(part));
                        }
                    }
                    Step::NewTurn => {
                        turn_seen = true;
                        live.lapse_ends(now + hold);
                    }
                    // The held answer goes right after the lines of its turn.
                    Step::TurnEnd => {
                        if let Some(held) = live.turn_end(line.end) {
                            let room = MAX_QUEUED_MESSAGES.saturating_sub(queued);
                            match answer_ops(live, held, room) {
                                Ok(ops) => {
                                    queued += ops.len();
                                    actions.push(Action::Answer(ops));
                                }
                                Err(held) => actions.push(Action::Release(held)),
                            }
                        }
                    }
                }
            }
        }
        live.barrier(read_to);
        if more || stopped {
            live.next_read = Some(now);
        }
        for action in actions {
            match action {
                Action::Stream(number, op) => {
                    self.queued_messages += 1;
                    self.hand_off(
                        Work::Stream {
                            session: session.to_owned(),
                            number,
                        },
                        op,
                    );
                }
                Action::Answer(ops) => self.stream_answer(session, ops),
                Action::Release(held) => self.release(session, held),
                Action::React(message_id) => self.react(message_id, stream::WORKING),
            }
        }
        if interrupted {
            // Esc ended the turn like Stop does, also at an open prompt.
            self.prompts.quiet(session);
            self.sync_waiting(session);
        }
        if channel_seen {
            self.channel_off_told.remove(session);
        }
        if channel_seen || turn_seen {
            self.unseen.remove(session);
        } else if !more && !stopped {
            self.check_channel(session);
        }
        self.stream_answered(session);
    }

    /// Telegram answered stream message `number` of `session`.
    fn on_stream_done(&mut self, session: &str, number: u64, delivery: Option<Delivery>) {
        self.queued_messages = self.queued_messages.saturating_sub(1);
        if self.queued_messages == 0 {
            self.overflow_warned = false;
        }
        // A message Telegram will never take (a bad request, not a lost
        // topic) is skipped rather than sent again for ever.
        let skipped = matches!(
            &delivery,
            Some(result @ Err(ApiError::Telegram { code, .. }))
                if (400..500).contains(code) && !topic_gone(result)
        );
        let accepted = skipped || matches!(delivery, Some(Ok(Outcome::Sent(_) | Outcome::Merged)));
        let Some(live) = self.streams.get_mut(session) else {
            return;
        };
        live.answered(number, accepted);
        if !live.refused_warned && (skipped || !accepted) {
            live.refused_warned = true;
            if skipped {
                warn!(
                    session = short(session),
                    "stream message refused by Telegram; skipped"
                );
            } else {
                warn!(
                    session = short(session),
                    "stream message not delivered; the stream sends it again"
                );
            }
        }
        self.stream_answered(session);
        let Some(live) = self.streams.get_mut(session) else {
            return;
        };
        if live.stuck() {
            let (offset, calls) = self
                .registry
                .sessions
                .get(session)
                .and_then(|entry| entry.stream.as_ref())
                .map(|stream| (stream.offset, stream.calls.clone()))
                .unwrap_or_default();
            live.rewind(
                offset,
                calls,
                Instant::now() + self.options.stream_retry,
                self.options.hold_answer,
            );
        }
    }

    /// Moves the persisted offset and open calls to the last barrier whose
    /// messages Telegram has all accepted.
    fn stream_answered(&mut self, session: &str) {
        let Some(live) = self.streams.get_mut(session) else {
            return;
        };
        let Some((offset, calls)) = live.advance() else {
            return;
        };
        live.refused_warned = false;
        if let Some(stream) = self
            .registry
            .sessions
            .get_mut(session)
            .and_then(|entry| entry.stream.as_mut())
            && (stream.offset != Some(offset) || stream.calls != calls)
        {
            stream.offset = Some(offset);
            stream.calls = calls;
            self.registry.dirty = true;
        }
    }

    /// Sends an agent's reply to the topic of its session: the chunks of
    /// `split_for_telegram` in order, or one document when it prefers a file.
    #[cfg(test)]
    fn on_reply(&mut self, conn: u64, text: &str) {
        let Some(session) = self.conns.get(&conn).map(|bound| bound.session.clone()) else {
            return;
        };
        self.on_reply_for(conn, &session, text);
    }

    fn on_reply_for(&mut self, conn: u64, frame_session: &str, text: &str) {
        let Some((session, slot)) = self.live_reply_slot(conn, frame_session) else {
            debug!(
                conn,
                "reply from an agent without the current live session; dropped"
            );
            return;
        };
        let ordinal = self.ordinal(slot);
        let Some(thread_id) = self.registry.slot(slot).and_then(|slot| slot.topic_id) else {
            warn!(ordinal, "reply for a slot without a topic yet; dropped");
            return;
        };
        if let Some(parts) = self.send_text(thread_id, &session, text, "reply", false) {
            info!(
                ordinal,
                session = short(&session),
                parts,
                "agent reply queued"
            );
        }
    }

    /// Queues `text` for the topic: the chunks of `split_for_telegram` in
    /// order, or one document `<kind>-<short id>.txt` when it prefers a file,
    /// with a sound when `notify`.
    /// The number of parts, or `None` when the message cap refused them.
    fn send_text(
        &mut self,
        thread_id: i64,
        session: &str,
        text: &str,
        kind: &str,
        notify: bool,
    ) -> Option<usize> {
        let split = split_markdown_for_telegram(text, SplitOptions::default());
        let ops: Vec<Op> = if split.prefer_file {
            vec![Op::SendDocument {
                thread_id: Some(thread_id),
                document: Document {
                    file_name: format!("{kind}-{}.txt", short(session)),
                    bytes: text.as_bytes().to_vec(),
                    caption: None,
                },
                notify,
            }]
        } else {
            split
                .chunks
                .into_iter()
                .map(|chunk| Op::Send {
                    thread_id: Some(thread_id),
                    text: chunk.text,
                    html: Some(chunk.html),
                    reply_markup: None,
                    permission: false,
                    reply_to: None,
                    notify,
                })
                .collect()
        };
        let parts = ops.len();
        self.send_messages(ops).then_some(parts)
    }

    /// Remembers a relayed permission request; [`Self::send_prompts`] puts it
    /// into the topic of the session's own slot. Unlike a reply it needs no
    /// "current session of the slot" check: a session that ended has its
    /// prompts closed (see [`Self::close_ended_prompts`]).
    fn on_permission_request(
        &mut self,
        conn: u64,
        frame_session: &str,
        request: PermissionRequest,
    ) {
        let Some(bound) = self.conns.get(&conn) else {
            return;
        };
        if !is_request_id(&request.request_id) {
            debug!(
                conn,
                "permission request without a valid request id; dropped"
            );
            return;
        }
        if request.tool_name == QUESTION_TOOL {
            debug!(
                conn,
                "permission request of a question; its hook asks in the topic"
            );
            return;
        }
        let session = frame_session.to_owned();
        if self
            .registry
            .sessions
            .get(&session)
            .is_some_and(|entry| entry.ended)
        {
            debug!(
                conn,
                session = short(&session),
                "permission request for an ended session; dropped"
            );
            return;
        }
        let prompt = Prompt::new(
            conn,
            bound.host.clone(),
            bound.claude_pid,
            session.clone(),
            &request,
        );
        match self.prompts.open(prompt) {
            Opened::Added { expired, .. } => {
                // Only a request shown now is the twin of a waiting hook; a
                // re-sent one must not consume an unrelated hook ask.
                self.note_relayed(&session, &request.tool_name);
                info!(
                    conn,
                    session = short(&session),
                    "permission request queued for the topic"
                );
                if let Some(gone) = expired {
                    self.expire(*gone);
                }
            }
            Opened::Duplicate => debug!(conn, "permission request already shown; not repeated"),
            Opened::Full => warn!(
                conn,
                session = short(&session),
                "too many permission prompts wait for an answer; this one only in the terminal"
            ),
        }
        self.sync_waiting(&session);
    }

    /// A channel request of `session` for `tool`: a hook waiting for this
    /// twin gets no decision; otherwise a hook that comes within
    /// [`TWIN_WINDOW`] will find it.
    fn note_relayed(&mut self, session: &str, tool: &str) {
        let twin = |post: &PermissionPost| post.session_id == session && post.tool_name == tool;
        if let Some(at) = self.hook_asks.iter().position(|ask| twin(&ask.post)) {
            self.hook_asks.remove(at);
            info!(
                session = short(session),
                "permission hook: the channel relays this request; no decision"
            );
            return;
        }
        let now = Instant::now();
        self.forget_relayed(now);
        if self.relayed.len() >= MAX_RELAYED {
            self.relayed.pop_front();
        }
        self.relayed
            .push_back((now, session.to_owned(), tool.to_owned()));
    }

    fn forget_relayed(&mut self, now: Instant) {
        while self
            .relayed
            .front()
            .is_some_and(|(at, _, _)| now.saturating_duration_since(*at) > TWIN_WINDOW)
        {
            self.relayed.pop_front();
        }
    }

    /// A `PermissionRequest` hook asks. Only a live top-level session gets a
    /// prompt (nested and headless runs have no topic of their own); one whose
    /// channel request came within [`TWIN_WINDOW`] gets no decision at once.
    /// Dropping `ask.answer` is the "no decision" answer.
    fn on_permission_ask(&mut self, ask: PermissionAsk) {
        let session = ask.post.session_id.clone();
        if ask.post.tool_name == QUESTION_TOOL {
            debug!(
                session = short(&session),
                "permission hook of a question; its PreToolUse hook asks; no decision"
            );
            self.hint_question_hook(&session);
            return;
        }
        if !self.registry.is_live_top_level(&session) {
            debug!(
                session = short(&session),
                "permission hook of a session without a live topic; no decision"
            );
            return;
        }
        let now = Instant::now();
        self.forget_relayed(now);
        let tool = &ask.post.tool_name;
        if let Some(at) = self
            .relayed
            .iter()
            .position(|(_, relayed, relayed_tool)| *relayed == session && relayed_tool == tool)
        {
            self.relayed.remove(at);
            info!(
                session = short(&session),
                "permission hook: the channel relayed this request; no decision"
            );
            return;
        }
        debug!(
            session = short(&session),
            "permission hook waits for its channel twin"
        );
        self.hook_asks.push(HookAsk {
            post: ask.post,
            answer: ask.answer,
            show_at: now + TWIN_WINDOW,
            until: now + self.options.hook_answer_wait,
        });
    }

    /// Asks without a twin become prompts; hooks that went away, ran out of
    /// time or belong to an ended session get no decision (their prompts lose
    /// the buttons); a decided hook prompt sends its answer to its hook.
    fn check_hook_asks(&mut self) {
        let now = Instant::now();
        for ask in std::mem::take(&mut self.hook_asks) {
            if ask.answer.is_closed()
                || now >= ask.until
                || !self.registry.is_live_top_level(&ask.post.session_id)
            {
                continue;
            }
            if now >= ask.show_at {
                self.show_hook_prompt(ask);
            } else {
                self.hook_asks.push(ask);
            }
        }
        for key in self.hook_waiters.keys().copied().collect::<Vec<_>>() {
            let state = self.prompts.get(key).map(|prompt| prompt.state);
            let Some(waiter) = self.hook_waiters.get(&key) else {
                continue;
            };
            let behavior = match state {
                Some(state) if state.is_active() => {
                    if !waiter.answer.is_closed() && now < waiter.until {
                        continue;
                    }
                    self.finish(key, State::Expired);
                    None
                }
                Some(State::Decided(behavior)) => Some(behavior),
                _ => None,
            };
            let Some(waiter) = self.hook_waiters.remove(&key) else {
                continue;
            };
            let session = self
                .prompts
                .get(key)
                .map_or("", |prompt| short(&prompt.session))
                .to_owned();
            match behavior {
                Some(behavior) if waiter.answer.send(Some(behavior)).is_ok() => {
                    info!(session, ?behavior, "permission answer handed to the hook");
                }
                Some(_) => info!(session, "permission hook left before the answer"),
                None => debug!(session, "permission hook gets no decision"),
            }
        }
    }

    /// Opens the prompt of a hook ask under a fresh request id.
    fn show_hook_prompt(&mut self, ask: HookAsk) {
        let HookAsk {
            post,
            answer,
            until,
            ..
        } = ask;
        let session = post.session_id.clone();
        // A few tries: the id must differ from the session's active prompts.
        for _ in 0..8 {
            let request = PermissionRequest {
                request_id: permissions::hook_request_id(),
                tool_name: post.tool_name.clone(),
                description: post.description.clone(),
                input_preview: post.input_preview.clone(),
            };
            let mut prompt = Prompt::new(0, post.host.clone(), None, session.clone(), &request);
            prompt.hook = true;
            match self.prompts.open(prompt) {
                Opened::Added { key, expired } => {
                    info!(
                        session = short(&session),
                        "permission hook request queued for the topic"
                    );
                    self.hook_waiters.insert(key, Waiter { answer, until });
                    if let Some(gone) = expired {
                        self.expire(*gone);
                    }
                    self.sync_waiting(&session);
                    return;
                }
                Opened::Duplicate => {}
                Opened::Full => break,
            }
        }
        warn!(
            session = short(&session),
            "too many permission prompts wait for an answer; this hook gets no decision"
        );
    }

    /// The first press on an open hook prompt decides it; the answer goes to
    /// the hook on the next pump. A hook that already left gets nothing and
    /// the prompt expires.
    fn press_hook(&mut self, key: u64, behavior: Behavior) -> &'static str {
        let listening = self
            .hook_waiters
            .get(&key)
            .is_some_and(|waiter| !waiter.answer.is_closed());
        if !listening {
            self.finish(key, State::Expired);
            return permissions::ANSWER_EXPIRED;
        }
        if let Some(prompt) = self.prompts.get(key) {
            info!(
                session = short(&prompt.session),
                ?behavior,
                "permission answer chosen in Telegram for a hook"
            );
        }
        self.finish(key, State::Decided(behavior));
        permissions::answer(behavior)
    }

    /// An open prompt the full book let go: its buttons go away. One try: a
    /// press on buttons that stayed only answers "expired".
    fn expire(&mut self, gone: Prompt) {
        info!(
            session = short(&gone.session),
            "oldest open permission prompt expired to make room"
        );
        if let Some(message_id) = gone.message_id {
            self.hand_off(
                Work::Callback,
                Op::Edit {
                    message_id,
                    text: permissions::ANSWER_EXPIRED.to_owned(),
                    reply_markup: Some(permissions::no_keyboard()),
                    background: false,
                },
            );
        }
        self.sync_waiting(&gone.session);
    }

    /// A question came as a `PermissionRequest` hook only. Unless a question
    /// hook of the session asked within [`QUESTION_HOOK_WINDOW`] (this is its
    /// "no decision" going on), its client has none: the topic is told once
    /// per session that the question waits in the terminal and how to get
    /// the hook.
    fn hint_question_hook(&mut self, session: &str) {
        let now = Instant::now();
        self.question_hooks
            .retain(|_, at| now.saturating_duration_since(*at) <= QUESTION_HOOK_WINDOW);
        if self.question_hooks.contains_key(session) {
            return;
        }
        let Some(slot) = self.current_slot(session) else {
            return;
        };
        let Some(thread_id) = self.registry.slot(slot).and_then(|slot| slot.topic_id) else {
            return;
        };
        if self
            .registry
            .sessions
            .get(session)
            .is_none_or(|entry| entry.question_hint)
        {
            return;
        }
        if self.send_messages(vec![message_op(
            thread_id,
            questions::NO_HOOK_NOTICE.to_owned(),
        )]) {
            if let Some(entry) = self.registry.sessions.get_mut(session) {
                entry.question_hint = true;
            }
            info!(
                session = short(session),
                "question without a question hook; the topic is told once"
            );
        }
    }

    /// The topic of `session`'s slot, if it has one.
    fn session_topic(&self, session: &str) -> Option<i64> {
        let slot = self.registry.sessions.get(session)?.slot?;
        self.registry.slot(slot)?.topic_id
    }

    /// An `AskUserQuestion` hook asks. Only a live top-level session whose
    /// slot has a topic gets a question there; dropping `ask.answer` is the
    /// "no decision" answer.
    fn on_question_ask(&mut self, ask: QuestionAsk) {
        let QuestionAsk { post, answer } = ask;
        let session = post.session_id.clone();
        let now = Instant::now();
        self.question_hooks
            .retain(|_, at| now.saturating_duration_since(*at) <= QUESTION_HOOK_WINDOW);
        self.question_hooks.insert(session.clone(), now);
        if !self.registry.is_live_top_level(&session) || self.session_topic(&session).is_none() {
            debug!(
                session = short(&session),
                "question hook of a session without a live topic; no decision"
            );
            return;
        }
        let mut id = permissions::hook_request_id();
        for _ in 0..8 {
            if !self.questions.id_taken(&session, &id) {
                break;
            }
            id = permissions::hook_request_id();
        }
        let count = post.questions.len();
        let Some(key) =
            self.questions
                .open(questions::Ask::new(session.clone(), id, post.questions))
        else {
            warn!(
                session = short(&session),
                "too many questions wait for an answer; this one only in the terminal"
            );
            return;
        };
        info!(
            session = short(&session),
            count, "question queued for the topic"
        );
        let until = Instant::now() + self.options.question_wait;
        self.question_waiters
            .insert(key, QuestionWaiter { answer, until });
        self.sync_waiting(&session);
    }

    /// Open questions whose hook went away, ran out of time or lost its live
    /// session end; an ended question hands its answers (or no decision) to
    /// its hook; a question with nothing left to show is forgotten.
    fn check_questions(&mut self) {
        let now = Instant::now();
        for key in self.questions.keys() {
            let Some(ask) = self.questions.get(key) else {
                continue;
            };
            if ask.is_open() {
                let waiter = self.question_waiters.get(&key);
                let end = if !self.registry.is_live_top_level(&ask.session) {
                    Some(questions::State::Closed)
                } else if waiter.is_none_or(|waiter| waiter.answer.is_closed()) {
                    Some(questions::State::Gone)
                } else if waiter.is_some_and(|waiter| now >= waiter.until) {
                    Some(questions::State::Expired)
                } else {
                    None
                };
                if let Some(state) = end {
                    self.end_question(key, state);
                }
            }
            let Some(ask) = self.questions.get(key) else {
                continue;
            };
            if !ask.is_open()
                && let Some(waiter) = self.question_waiters.remove(&key)
            {
                let session = short(&ask.session).to_owned();
                match ask.answers() {
                    Some(answers) => match waiter.answer.send(Some(answers)) {
                        Ok(()) => info!(session, "question answers handed to the hook"),
                        Err(_) => {
                            info!(session, "question hook left before the answers");
                            if let Some(ask) = self.questions.get_mut(key) {
                                ask.answers_not_taken();
                            }
                        }
                    },
                    None => debug!(session, "question hook gets no decision"),
                }
            }
            if self
                .questions
                .get(key)
                .is_some_and(questions::Ask::finished)
            {
                self.questions.remove(key);
            }
        }
    }

    /// Ends an open question; its edit goes out on the next pump.
    fn end_question(&mut self, key: u64, state: questions::State) {
        let Some(ask) = self.questions.get_mut(key) else {
            return;
        };
        if ask.end(state) {
            let session = ask.session.clone();
            info!(session = short(&session), ?state, "question ended");
            self.sync_waiting(&session);
        }
    }

    /// Hands every open question not sent yet whose session's slot has a
    /// topic to Telegram, on the permission lane.
    fn send_questions(&mut self) {
        for key in self.questions.keys() {
            let Some(ask) = self.questions.get(key) else {
                continue;
            };
            if !ask.is_open() || ask.thread_id.is_some() {
                continue;
            }
            let Some(thread_id) = self.session_topic(&ask.session) else {
                // Nowhere to show it: the terminal dialog, not a silent wait.
                self.end_question(key, questions::State::Expired);
                continue;
            };
            let op = Op::Send {
                thread_id: Some(thread_id),
                text: ask.text(),
                html: None,
                reply_markup: Some(ask.keyboard()),
                permission: true,
                reply_to: None,
                notify: true,
            };
            let version = ask.version;
            if let Some(ask) = self.questions.get_mut(key) {
                ask.thread_id = Some(thread_id);
                ask.sending = true;
            }
            self.hand_off(Work::Question { key, version }, op);
        }
    }

    /// Edits every question message that shows an older version.
    fn send_question_edits(&mut self) {
        for key in self.questions.keys() {
            let Some(ask) = self.questions.get_mut(key) else {
                continue;
            };
            let Some(message_id) = ask.message_id.filter(|_| ask.edit_due()) else {
                continue;
            };
            ask.editing = true;
            let (version, text, keyboard) = (ask.version, ask.text(), ask.keyboard());
            self.hand_off(
                Work::QuestionEdit { key, version },
                Op::Edit {
                    message_id,
                    text,
                    reply_markup: Some(keyboard),
                    background: false,
                },
            );
        }
    }

    fn on_question_done(&mut self, key: u64, version: u64, delivery: Option<Delivery>) {
        let Some(ask) = self.questions.get_mut(key) else {
            return;
        };
        ask.sending = false;
        match delivery {
            Some(Ok(Outcome::Sent(message))) if message.message_id != 0 => {
                ask.message_id = Some(message.message_id);
                ask.shown = version;
                return;
            }
            Some(Ok(_)) => warn!("question sent without a message id; its buttons cannot work"),
            Some(Err(error)) => {
                warn!(%error, "question not delivered; it goes to the terminal");
            }
            None => warn!("question got no answer from Telegram; it goes to the terminal"),
        }
        self.end_question(key, questions::State::Expired);
    }

    fn on_question_edit_done(&mut self, key: u64, version: u64, delivery: Option<Delivery>) {
        let applied = match &delivery {
            Some(Ok(_)) => true,
            Some(delivery) => telegram_error(
                delivery,
                &[
                    "message is not modified",
                    "message to edit not found",
                    "message can't be edited",
                ],
            ),
            None => false,
        };
        let Some(ask) = self.questions.get_mut(key) else {
            return;
        };
        ask.editing = false;
        if applied {
            ask.shown = ask.shown.max(version);
            ask.edit_failures = 0;
            return;
        }
        ask.edit_failures += 1;
        if ask.edit_failures >= questions::MAX_EDIT_ATTEMPTS {
            warn!("question edit keeps failing; given up");
            ask.shown = ask.version;
        } else {
            debug!(
                attempts = ask.edit_failures,
                "question edit failed; retrying later"
            );
            ask.retry = true;
        }
    }

    /// A question button. Only the question of that message and its current
    /// step count; a press after the hook left closes the question. A press
    /// that beats Telegram's answer to the send (the message id is not known
    /// yet) counts for the one question of that topic with its id still in
    /// flight.
    fn press_question(
        &mut self,
        input: &CallbackInput,
        id: &str,
        question: usize,
        press: questions::Press,
    ) -> Option<&'static str> {
        let Some(key) = input
            .message_id
            .and_then(|message_id| self.questions.by_message(message_id))
            .or_else(|| self.questions.in_flight(id, input.thread_id))
        else {
            debug!("button of a question this hub does not know");
            return Some(questions::ANSWER_STALE);
        };
        let listening = self
            .question_waiters
            .get(&key)
            .is_some_and(|waiter| !waiter.answer.is_closed());
        let ask = self.questions.get_mut(key).filter(|ask| ask.id == id)?;
        if ask.is_open() && !listening {
            self.end_question(key, questions::State::Gone);
            return Some(questions::ANSWER_STALE);
        }
        let answer = ask.press(question, press);
        if !ask.is_open() {
            let (session, state) = (ask.session.clone(), ask.state);
            info!(
                session = short(&session),
                ?state,
                "question closed in Telegram"
            );
            self.sync_waiting(&session);
        }
        answer
    }

    /// Keeps `input` back while it may answer a question whose message
    /// Telegram has sent but whose id the hub does not know yet: an explicit
    /// reply to a message the hub cannot match, in a topic with such a
    /// question (TASK-060). Later messages of that topic wait behind it, so
    /// the session gets them in order. `Some`: it goes on now.
    fn hold(&mut self, thread_id: i64, input: Inbound) -> Option<Inbound> {
        let behind = self
            .held
            .iter()
            .any(|held| held.thread_id == Some(thread_id));
        let unknown_reply = input.text.is_some()
            && !input.forwarded
            && input
                .reply_to
                .is_some_and(|reply_to| self.questions.by_message(reply_to).is_none())
            && self.questions.sending_in(thread_id);
        if !(behind || unknown_reply) || self.held.len() >= MAX_HELD {
            return Some(input);
        }
        debug!("topic message waits for a question's message id");
        self.held.push_back(input);
        None
    }

    /// A question's send came back: the held messages go through again,
    /// oldest first; those that still have to wait are held again.
    fn release_held(&mut self) {
        for input in std::mem::take(&mut self.held) {
            self.on_topic_message(input);
        }
    }

    /// A text message in topic `thread_id` that answers an open question:
    /// a reply to its message, or the next text after ✏️ Другое. `false`:
    /// it is no answer and goes on as usual.
    fn answer_question(&mut self, thread_id: i64, reply_to: Option<i64>, text: &str) -> bool {
        let Some(key) = self.questions.text_target(thread_id, reply_to) else {
            return false;
        };
        let listening = self
            .question_waiters
            .get(&key)
            .is_some_and(|waiter| !waiter.answer.is_closed());
        if !listening {
            self.end_question(key, questions::State::Gone);
            return false;
        }
        let Some(ask) = self.questions.get_mut(key) else {
            return false;
        };
        if !ask.type_answer(text) {
            return false;
        }
        let session = ask.session.clone();
        let done = !ask.is_open();
        info!(
            session = short(&session),
            "question answered with a text from the topic"
        );
        if done {
            self.sync_waiting(&session);
        }
        true
    }

    /// The waiting icon of `session` shows whether it has a prompt that
    /// still counts (see [`Prompt::waits`]) or an open question.
    fn sync_waiting(&mut self, session: &str) {
        let waiting = self.prompts.waiting(session) || self.questions.waiting(session);
        self.registry.set_waiting(session, waiting);
    }

    /// Ends an active prompt: its final edit goes out on the next pump.
    fn finish(&mut self, key: u64, state: State) {
        let Some(session) = self.prompts.get(key).map(|prompt| prompt.session.clone()) else {
            return;
        };
        if self.prompts.finish(key, state) {
            self.sync_waiting(&session);
        }
    }

    /// Closes the active prompts of every session the registry marks ended:
    /// by its SessionEnd, by `/clear` or by a new session on a reused pid.
    /// A SessionEnd the registry ignored (a nested resume) closes nothing.
    fn close_prompts(&mut self, ended_sessions: &[String]) {
        for key in self.prompts.active() {
            let Some(session) = self.prompts.get(key).map(|prompt| prompt.session.clone()) else {
                continue;
            };
            if ended_sessions.iter().any(|ended| ended == &session) {
                info!(
                    session = short(&session),
                    "permission prompt closed: its session ended"
                );
                self.finish(key, State::Closed);
            }
        }
    }

    /// Hands every active prompt whose session's slot has a topic to
    /// Telegram, on the permission lane. Prompts are never counted against
    /// [`MAX_QUEUED_MESSAGES`]; [`permissions::MAX_PROMPTS`] bounds them.
    fn send_prompts(&mut self) {
        for key in self.prompts.unsent() {
            let Some(session) = self.prompts.get(key).map(|prompt| prompt.session.clone()) else {
                continue;
            };
            let Some(entry) = self.registry.sessions.get(&session) else {
                // Agent-before-SessionStart: retain it until the hook arrives.
                continue;
            };
            if entry.ended {
                self.finish(key, State::Closed);
                continue;
            }
            let thread_id = entry
                .slot
                .and_then(|slot| self.registry.slot(slot))
                .and_then(|slot| slot.topic_id);
            let Some(thread_id) = thread_id else {
                continue;
            };
            let Some(prompt) = self.prompts.get(key) else {
                continue;
            };
            let op = Op::Send {
                thread_id: Some(thread_id),
                text: prompt.text.clone(),
                html: None,
                reply_markup: Some(permissions::keyboard(&prompt.request_id)),
                permission: true,
                reply_to: None,
                notify: true,
            };
            if let Some(prompt) = self.prompts.get_mut(key) {
                prompt.sent = true;
                prompt.thread_id = Some(thread_id);
            }
            self.hand_off(Work::Permission(key), op);
        }
    }

    /// Hands out the final edits that are due: the decision or the end of
    /// the session, always without buttons.
    fn send_prompt_edits(&mut self) {
        for key in self.prompts.due_edits() {
            let Some(prompt) = self.prompts.get_mut(key) else {
                continue;
            };
            let (Some(message_id), Some(text)) = (prompt.message_id, prompt.final_text()) else {
                continue;
            };
            prompt.edit = Edit::InFlight;
            self.hand_off(
                Work::PromptEdit(key),
                Op::Edit {
                    message_id,
                    text,
                    reply_markup: Some(permissions::no_keyboard()),
                    background: false,
                },
            );
        }
    }

    /// Answers every button press at once; the final edit follows when the
    /// prompt ends.
    fn on_callback(&mut self, input: CallbackInput) {
        let answer = self.press(&input);
        self.hand_off(
            Work::Callback,
            Op::AnswerCallback {
                query_id: input.query_id,
                text: answer.map(str::to_owned),
            },
        );
    }

    /// The first press on an open prompt fixes the answer for good; later
    /// presses only push the same verdict again. Buttons that are not
    /// permission buttons get an empty answer.
    fn press(&mut self, input: &CallbackInput) -> Option<&'static str> {
        if let Some(session) = input.data.as_deref().and_then(buffer::parse_callback) {
            return Some(self.press_resume(session));
        }
        if let Some(press) = input.data.as_deref().and_then(status::parse_callback) {
            return Some(self.press_status(input.message_id, press));
        }
        if let Some(session) = input.data.as_deref().and_then(status::parse_update) {
            let session = session.to_owned();
            return Some(self.press_update(&session));
        }
        if let Some((id, question, press)) =
            input.data.as_deref().and_then(questions::parse_callback)
        {
            return self.press_question(input, id, question, press);
        }
        let Some((behavior, request_id)) =
            input.data.as_deref().and_then(permissions::parse_callback)
        else {
            debug!("button press that is not a permission answer");
            return None;
        };
        let expired = Some(permissions::ANSWER_EXPIRED);
        // A press that beats Telegram's answer to the send (the message id
        // is not known yet) counts for the one prompt of that topic with its
        // id still in flight (TASK-060).
        let Some(key) = input
            .message_id
            .and_then(|message_id| self.prompts.by_message(message_id))
            .or_else(|| self.prompts.in_flight(request_id, input.thread_id))
        else {
            debug!("button of a prompt this hub does not know");
            return expired;
        };
        let Some(prompt) = self
            .prompts
            .get_mut(key)
            .filter(|prompt| prompt.request_id == request_id)
        else {
            return expired;
        };
        if prompt.state == State::Open {
            // The press that fixes the answer signs it (TASK-036).
            prompt.decided_by = input.from_name.clone();
        }
        if prompt.hook && prompt.state == State::Open {
            return Some(self.press_hook(key, behavior));
        }
        match prompt.state {
            State::Closed | State::Expired => expired,
            State::Selected { .. } | State::Decided(_) => {
                debug!(
                    session = short(&prompt.session),
                    "prompt already answered; no second verdict"
                );
                self.push_verdict(key);
                Some(permissions::ANSWER_DECIDED)
            }
            State::Open => {
                prompt.state = State::Selected {
                    behavior,
                    verdict_id: crate::wire::random_u64(),
                };
                info!(
                    session = short(&prompt.session),
                    ?behavior,
                    "permission answer chosen in Telegram"
                );
                if self.push_verdict(key) {
                    Some(permissions::answer(behavior))
                } else {
                    Some(permissions::ANSWER_OFFLINE)
                }
            }
        }
    }

    /// A status button. It acts only on the status message's own slot, only
    /// for its live current session, and only through that session's bound
    /// agent that presses keys. ⏹ asks for a second press within
    /// [`status::CONFIRM_FOR`]; while a permission prompt of the session
    /// waits it sends nothing.
    fn press_status(&mut self, message_id: Option<i64>, press: Press) -> &'static str {
        let Some(slot) = message_id.and_then(|id| self.status_slot(id)) else {
            debug!("status button of a message that is no status message");
            return status::ANSWER_STALE;
        };
        let Some((session, conn)) = self.live_agent(slot) else {
            return status::ANSWER_OFFLINE;
        };
        if press == Press::Update {
            return self.press_update(&session);
        }
        if !self.conns.get(&conn).is_some_and(|bound| bound.keys) {
            return status::ANSWER_NO_KEYS;
        }
        let now = Instant::now();
        let waiting = self.waiting(&session);
        let busy = self.busy(&session);
        let shown = self.shown.entry(slot).or_default();
        let armed = shown
            .confirm
            .as_ref()
            .is_some_and(|(armed, until)| *armed == session && now < *until);
        if waiting {
            // Esc would answer the prompt, not stop the turn.
            if shown.confirm.take().is_some() {
                shown.next_at = Some(now);
                shown.urgent = true;
            }
            return status::ANSWER_WAITING;
        }
        match press {
            Press::Update => unreachable!("answered above"),
            _ if !busy => status::ANSWER_IDLE,
            Press::Confirm if armed => {
                shown.confirm = None;
                shown.next_at = Some(now);
                shown.urgent = true;
                if !self.send_key(slot, &session, conn) {
                    return status::ANSWER_OFFLINE;
                }
                info!(
                    ordinal = self.ordinal(slot),
                    session = short(&session),
                    "interrupt confirmed in Telegram"
                );
                status::ANSWER_INTERRUPTING
            }
            // A first press, or a second one after the wait ran out.
            Press::Stop | Press::Confirm => {
                shown.confirm = Some((session, now + status::CONFIRM_FOR));
                // Shown at once, whatever the edit pace and the refreshes
                // of other slots.
                shown.next_at = Some(now);
                shown.urgent = true;
                status::ANSWER_CONFIRM
            }
        }
    }

    /// Agent `conn` runs another build than the hub, or is too old to say.
    /// Never when the hub does not know its own build.
    fn outdated(&self, conn: u64) -> bool {
        let Some(hub) = self.options.build.as_deref() else {
            return false;
        };
        self.conns.get(&conn).is_some_and(|bound| {
            bound
                .client
                .as_ref()
                .is_none_or(|client| client.build != hub)
        })
    }

    /// One loud warning per hub build in the topic of each live current
    /// session whose agent is outdated, with ⬆️ Обновить.
    fn warn_outdated(&mut self) {
        let Some(hub) = self.options.build.clone() else {
            return;
        };
        for index in 0..self.registry.slots.len() {
            let slot = SlotId(index);
            let Some(thread_id) = self.registry.slots[index].topic_id else {
                continue;
            };
            let Some((session, conn)) = self.live_agent(slot) else {
                continue;
            };
            let warned = self
                .registry
                .sessions
                .get(&session)
                .is_some_and(|entry| entry.update_warned.as_deref() == Some(hub.as_str()));
            if warned || !self.outdated(conn) {
                continue;
            }
            let Some(keyboard) = status::update_keyboard(&session) else {
                continue;
            };
            let agent = self
                .conns
                .get(&conn)
                .and_then(|bound| bound.client.as_ref())
                .map(|client| crate::client::short(&client.build));
            let op = Op::Send {
                thread_id: Some(thread_id),
                text: status::outdated_text(agent.as_deref(), &crate::client::short(&hub)),
                html: None,
                reply_markup: Some(keyboard),
                permission: false,
                reply_to: None,
                // Loud (decision 2026-09-24): the user asked to be told.
                notify: true,
            };
            if !self.send_messages(vec![op]) {
                return;
            }
            if let Some(entry) = self.registry.sessions.get_mut(&session) {
                entry.update_warned = Some(hub.clone());
                self.registry.dirty = true;
            }
            info!(
                ordinal = self.ordinal(slot),
                session = short(&session),
                "outdated client warned about"
            );
        }
    }

    /// ⬆️ Обновить for `session`: only its live agent in its current slot,
    /// only when outdated and able to update itself. The `update` goes out
    /// from [`Self::pump_updates`], after the running turn.
    fn press_update(&mut self, session: &str) -> &'static str {
        let Some(slot) = self.current_slot(session) else {
            return status::ANSWER_OFFLINE;
        };
        let Some((_, conn)) = self.live_agent(slot).filter(|(live, _)| live == session) else {
            return status::ANSWER_OFFLINE;
        };
        if self.updates.contains_key(session) {
            return status::ANSWER_UPDATE_RUNNING;
        }
        if !self.outdated(conn) {
            return status::ANSWER_CURRENT;
        }
        let self_update = self
            .conns
            .get(&conn)
            .and_then(|bound| bound.client.as_ref())
            .is_some_and(|client| client.self_update);
        if !self_update {
            return status::ANSWER_OLD_CLIENT;
        }
        info!(
            ordinal = self.ordinal(slot),
            session = short(session),
            "update asked in Telegram"
        );
        self.updates.insert(
            session.to_owned(),
            UpdateAsk {
                sent: None,
                left: None,
                rounds: 0,
                until: Instant::now() + UPDATE_WAIT,
                told: false,
                held: None,
                retry_at: None,
                agents_told: false,
                interrupted: false,
            },
        );
        if self.busy(session) {
            status::ANSWER_AFTER_TURN
        } else {
            status::ANSWER_UPDATING
        }
    }

    /// Sends `update` for every press whose session has a bound agent that
    /// can take it and no turn running; forgets presses past their time or
    /// rounds. A press that waits for a turn longer than [`UPDATE_WAIT`]
    /// stays until the turn ends, and the topic is told once.
    fn pump_updates(&mut self) {
        let now = Instant::now();
        let sessions: Vec<String> = self.updates.keys().cloned().collect();
        for session in sessions {
            let Some(ask) = self.updates.get(&session) else {
                continue;
            };
            if ask.sent.is_none() && ask.left.is_none() && !self.agents_running(&session).is_empty()
            {
                // Subagents run (TASK-047): no `update` goes out, as after an
                // `agents_running` answer; the press is kept and goes on at
                // their last stop (or at their age limit, a deadline), the
                // topic is told once.
                let mut tell = false;
                if let Some(ask) = self.updates.get_mut(&session) {
                    ask.until = now + UPDATE_WAIT;
                    tell = !std::mem::replace(&mut ask.agents_told, true);
                }
                if tell
                    && let Some(slot) = self.current_slot(&session)
                    && let Some(thread_id) = self.registry.slot(slot).and_then(|slot| slot.topic_id)
                {
                    info!(session = short(&session), "update held back: subagents run");
                    self.notify(slot, thread_id, status::UPDATE_AGENTS_NOTICE);
                }
                continue;
            }
            let (expired, idle, told) = (now >= ask.until, ask.sent.is_none(), ask.told);
            if expired && idle && self.busy(&session) {
                if let Some(ask) = self.updates.get_mut(&session) {
                    ask.until = now + UPDATE_WAIT;
                    ask.told = true;
                }
                if !told
                    && let Some(slot) = self.current_slot(&session)
                    && let Some(thread_id) = self.registry.slot(slot).and_then(|slot| slot.topic_id)
                {
                    self.notify(slot, thread_id, status::UPDATE_WAITS_NOTICE);
                }
                continue;
            }
            let Some(ask) = self.updates.get(&session) else {
                continue;
            };
            if expired || (idle && ask.rounds >= UPDATE_ROUNDS) {
                debug!(session = short(&session), "update press forgotten");
                self.updates.remove(&session);
                continue;
            }
            if ask.sent.is_some()
                || ask.held.is_some()
                || ask.retry_at.is_some_and(|at| now < at)
                || self.busy(&session)
            {
                continue;
            }
            let Some(conn) = self
                .current_slot(&session)
                .and_then(|slot| self.live_agent(slot))
                .filter(|(live, _)| *live == session)
                .map(|(_, conn)| conn)
                .filter(|conn| ask.left.is_none_or(|(_, left)| left != *conn))
            else {
                continue;
            };
            let able = self
                .conns
                .get(&conn)
                .and_then(|bound| bound.client.as_ref())
                .is_some_and(|client| client.self_update);
            let update_id = crate::wire::random_u64();
            // Only an agent of another build downloads; the next agent of
            // the session, already the hub's build, only restarts claude.
            let release = self.options.release.clone().filter(|_| self.outdated(conn));
            let sent = able
                && self.conns.get(&conn).is_some_and(|bound| {
                    bound
                        .to_agent
                        .try_send(HubMsg::Update { update_id, release })
                        .is_ok()
                });
            if !sent {
                continue;
            }
            if let Some(ask) = self.updates.get_mut(&session) {
                ask.sent = Some((update_id, conn));
                ask.rounds += 1;
                info!(
                    conn,
                    session = short(&session),
                    round = ask.rounds,
                    "update sent to the session agent"
                );
            }
        }
    }

    /// An agent's answer to `update`. A leaving one is unbound at once and
    /// released behind what was queued for it; the session's next agent is
    /// asked again. Any other answer ends the press with one notice and
    /// binds a leaving agent back.
    fn on_update_answer(
        &mut self,
        conn: u64,
        session: &str,
        update_id: u64,
        outcome: UpdateOutcome,
    ) {
        if let Some(ask) = self.updates.get_mut(session)
            && ask.held == Some((update_id, conn))
        {
            // The held-back agent gave its `update` up (`failed`); the press
            // goes again after the turn.
            ask.held = None;
            debug!(conn, ?outcome, "held-back update given up by the agent");
            return;
        }
        if outcome == UpdateOutcome::Restarting
            && let Some(ask) = self
                .updates
                .get_mut(session)
                .filter(|ask| ask.sent == Some((update_id, conn)))
            && self.activity.get(session).is_some_and(Activity::busy)
        {
            // A turn began after the `update` went out (a terminal prompt
            // too): no `/exit` into it. Not released, the agent stays bound;
            // this round does not count.
            ask.sent = None;
            ask.held = Some((update_id, conn));
            ask.rounds = ask.rounds.saturating_sub(1);
            info!(
                conn,
                session = short(session),
                "restart held back: a turn runs"
            );
            return;
        }
        if outcome == UpdateOutcome::AgentsRunning
            && let Some(ask) = self.updates.get_mut(session).filter(|ask| {
                ask.sent == Some((update_id, conn)) || ask.left == Some((update_id, conn))
            })
        {
            // The terminal shows background agents or the agent view
            // (TASK-047): no `/exit` now. Asked again later, like a press
            // waiting for a turn: this round does not count, the press is
            // kept, the topic is told once.
            let now = Instant::now();
            ask.sent = None;
            ask.left = None;
            ask.rounds = ask.rounds.saturating_sub(1);
            ask.until = now + UPDATE_WAIT;
            ask.retry_at = Some(now + UPDATE_RETRY);
            let tell = !std::mem::replace(&mut ask.agents_told, true);
            info!(
                conn,
                session = short(session),
                "restart held back: background agents"
            );
            self.keep_agent(conn, session);
            if tell
                && let Some(slot) = self.current_slot(session)
                && let Some(thread_id) = self.registry.slot(slot).and_then(|slot| slot.topic_id)
            {
                self.notify(slot, thread_id, status::UPDATE_AGENTS_NOTICE);
            }
            return;
        }
        let asked = self.updates.get(session).is_some_and(|ask| {
            ask.sent == Some((update_id, conn)) || ask.left == Some((update_id, conn))
        });
        if !asked {
            debug!(conn, "update answer nothing waits for");
        }
        info!(conn, session = short(session), ?outcome, "update answered");
        if matches!(
            outcome,
            UpdateOutcome::Reloading | UpdateOutcome::Restarting
        ) {
            let agents = self.agents_running(session);
            if outcome == UpdateOutcome::Restarting
                && (self.restart_cuts_work(session) || !agents.is_empty())
            {
                info!(
                    session = short(session),
                    agents = agents.len(),
                    "the restart cuts off work; the session is told after it"
                );
                if let Some(entry) = self.registry.sessions.get_mut(session) {
                    entry.restart_interrupted = true;
                    entry.restart_agents = agents;
                    self.registry.dirty = true;
                }
            }
            if let Some(ask) = self.updates.get_mut(session).filter(|_| asked) {
                ask.sent = None;
                ask.left = Some((update_id, conn));
            }
            if let Some(bound) = self.conns.get_mut(&conn) {
                bound.leaving = true;
                let _ = bound.to_agent.try_send(HubMsg::Released {
                    update_id,
                    session_id: session.to_owned(),
                });
            }
            self.registry.agent_disconnected(session, conn);
            // A leaving agent answers no read any more.
            self.fail_reads_of(conn);
            return;
        }
        if asked {
            self.updates.remove(session);
        }
        self.keep_agent(conn, session);
        let notice = match outcome {
            UpdateOutcome::UpToDate if self.outdated(conn) => status::NO_NEW_BUILD_NOTICE,
            UpdateOutcome::UpToDate => status::UPDATED_NOTICE,
            UpdateOutcome::NeedsManualRestart => status::MANUAL_RESTART_NOTICE,
            UpdateOutcome::DraftInInput => status::DRAFT_NOTICE,
            UpdateOutcome::AgentsRunning => status::UPDATE_AGENTS_NOTICE,
            UpdateOutcome::DownloadFailed => status::DOWNLOAD_FAILED_NOTICE,
            UpdateOutcome::ChecksumMismatch => status::CHECKSUM_NOTICE,
            UpdateOutcome::NoReleaseBuild => status::NO_RELEASE_BUILD_NOTICE,
            _ => status::UPDATE_FAILED_NOTICE,
        };
        if !asked && outcome == UpdateOutcome::UpToDate {
            return;
        }
        if let Some(slot) = self.current_slot(session)
            && let Some(thread_id) = self.registry.slot(slot).and_then(|slot| slot.topic_id)
        {
            self.notify(slot, thread_id, notice);
        }
        if let Some(slot) = self.current_slot(session)
            && let Some(shown) = self.shown.get_mut(&slot)
        {
            shown.next_at = Some(Instant::now());
        }
    }

    /// A leaving agent `conn` of `session` whose update did not go through
    /// after all is bound back; a restart it announced did not happen.
    fn keep_agent(&mut self, conn: u64, session: &str) {
        let Some(bound) = self.conns.get_mut(&conn).filter(|bound| bound.leaving) else {
            return;
        };
        bound.leaving = false;
        if let Some(entry) = self
            .registry
            .sessions
            .get_mut(session)
            .filter(|entry| entry.restart_interrupted)
        {
            entry.restart_interrupted = false;
            entry.restart_agents.clear();
            self.registry.dirty = true;
        }
        if self.registry.agent_connected(session, conn) {
            info!(conn, session = short(session), "leaving agent stays");
        }
    }

    /// A restart of `session` now cuts off work: a turn or a call runs
    /// (stopped by Esc or not), or ⏹ went in while the update press waited.
    fn restart_cuts_work(&self, session: &str) -> bool {
        self.updates.get(session).is_some_and(|ask| ask.interrupted)
            || self.activity.get(session).is_some_and(Activity::working)
    }

    /// Sessions a client restart cut off get one channel message once the
    /// agent of their next run is bound (TASK-047).
    fn send_continuations(&mut self) {
        let waiting: Vec<String> = self
            .registry
            .sessions
            .iter()
            .filter(|(_, entry)| entry.restart_interrupted)
            .map(|(session, _)| session.clone())
            .collect();
        for session in waiting {
            let Some(slot) = self.current_slot(&session) else {
                continue;
            };
            let Some((_, conn)) = self.live_agent(slot).filter(|(live, _)| *live == session) else {
                continue;
            };
            let mut meta =
                BTreeMap::from([("chat_id".to_owned(), self.options.chat_id.to_string())]);
            if let Some(thread_id) = self.registry.slot(slot).and_then(|slot| slot.topic_id) {
                meta.insert("thread_id".to_owned(), thread_id.to_string());
            }
            let agents = self
                .registry
                .sessions
                .get(&session)
                .map(|entry| entry.restart_agents.as_slice())
                .unwrap_or_default();
            let inbound = HubMsg::Inbound {
                content: continue_text(agents),
                meta,
            };
            let sent = self
                .conns
                .get(&conn)
                .is_some_and(|bound| bound.to_agent.try_send(inbound).is_ok());
            if !sent {
                continue;
            }
            if let Some(entry) = self.registry.sessions.get_mut(&session) {
                entry.restart_interrupted = false;
                entry.restart_agents.clear();
                self.registry.dirty = true;
            }
            info!(
                conn,
                session = short(&session),
                "session told to go on after the restart"
            );
        }
    }

    /// The session works: a turn or a call runs and no Esc went in yet.
    fn busy(&self, session: &str) -> bool {
        self.activity.get(session).is_some_and(Activity::busy)
    }

    /// Topic messages wait in `slot` (the link queue had no room, a file is
    /// on its way).
    fn kept(&self, slot: SlotId) -> bool {
        self.registry
            .slot(slot)
            .is_some_and(|entry| !entry.buffer.messages.is_empty())
    }

    /// Topic texts went to `session` less than `Options::inbound_settle` ago.
    fn settling(&self, session: &str) -> bool {
        self.handed_at
            .get(session)
            .is_some_and(|at| Instant::now() < *at + self.options.inbound_settle)
    }

    /// Topic texts went to `session` now (see [`Self::settling`]).
    fn mark_handed(&mut self, session: &str) {
        let settle = self.options.inbound_settle;
        if settle.is_zero() {
            return;
        }
        let now = Instant::now();
        self.handed_at.retain(|_, at| now < *at + settle);
        self.handed_at.insert(session.to_owned(), now);
    }

    /// Topic texts went to `session` now. Unless a turn runs (they wait in
    /// Claude Code's queue then), a sign that Claude took them is expected
    /// within `Options::channel_wait` (TASK-052): their channel record in
    /// the stream, or a turn. Only a streamed session can show the record,
    /// and a session already told waits for a record first.
    fn expect_taken(&mut self, session: &str) {
        if self.options.channel_wait.is_zero()
            || self.busy(session)
            || self.channel_off_told.contains(session)
            || self.stream_target(session).is_none()
        {
            return;
        }
        self.unseen
            .entry(session.to_owned())
            .or_insert_with(Instant::now);
    }

    /// The stream of `session` has just read its transcript to the end:
    /// texts handed to it `Options::channel_wait` ago and still without a
    /// sign that Claude took them mean Claude Code drops the channel's
    /// messages (started without the channel flag, or its dialog not
    /// confirmed). The topic is told once.
    fn check_channel(&mut self, session: &str) {
        let Some(&since) = self.unseen.get(session) else {
            return;
        };
        if Instant::now() < since + self.options.channel_wait {
            return;
        }
        let Some(slot) = self.current_slot(session) else {
            return;
        };
        let Some(thread_id) = self.registry.slot(slot).and_then(|entry| entry.topic_id) else {
            return;
        };
        if self.send_messages(vec![message_op(thread_id, CHANNEL_OFF_NOTICE.to_owned())]) {
            self.unseen.remove(session);
            self.channel_off_told.insert(session.to_owned());
            info!(
                ordinal = self.ordinal(slot),
                session = short(session),
                "session took no forwarded message; the topic is told its channel seems off"
            );
        }
    }

    /// A permission prompt of the session waits.
    fn waiting(&self, session: &str) -> bool {
        self.registry
            .sessions
            .get(session)
            .is_some_and(|entry| entry.waiting)
    }

    /// Asks agent `conn` of `session` to write Esc. `false`: its queue did
    /// not take it.
    fn send_key(&mut self, slot: SlotId, session: &str, conn: u64) -> bool {
        let key = ConsoleKey::Interrupt;
        let key_id = crate::wire::random_u64();
        let asked = self.conns.get(&conn).is_some_and(|bound| {
            bound
                .to_agent
                .try_send(HubMsg::ConsoleKey { key_id, key })
                .is_ok()
        });
        if !asked {
            warn!(conn, "agent queue full or closed; console key not sent");
            return false;
        }
        let now = Instant::now();
        self.key_asks.retain(|_, ask| ask.until > now);
        if self.key_asks.len() >= status::MAX_KEY_ASKS
            && let Some(oldest) = self
                .key_asks
                .iter()
                .min_by_key(|(_, ask)| ask.until)
                .map(|(id, _)| *id)
        {
            self.key_asks.remove(&oldest);
        }
        self.key_asks.insert(
            key_id,
            KeyAsk {
                slot,
                session: session.to_owned(),
                conn,
                until: now + status::KEY_WAIT,
            },
        );
        info!(
            conn,
            session = short(session),
            ?key,
            "console key sent to the session agent"
        );
        true
    }

    /// An agent wrote Esc, or could not. Written is not stopped: the status
    /// shows it as sent until `Stop`, an interrupt note, the next prompt or
    /// the session's end. A key that was not written is told in the topic.
    /// An answer for a slot, session or connection that changed meanwhile
    /// (the session ended, the slot got another session, the agent
    /// reconnected) changes nothing.
    fn on_key_written(&mut self, conn: u64, frame_session: &str, key_id: u64, written: bool) {
        let Some(ask) = self.key_asks.remove(&key_id) else {
            debug!(conn, "answer to a console key nothing waits for");
            return;
        };
        if ask.conn != conn
            || ask.session != frame_session
            || self.live_agent(ask.slot) != Some((ask.session.clone(), conn))
        {
            debug!(
                conn,
                "console key answer for a session that moved on; ignored"
            );
            return;
        }
        if written {
            info!(conn, session = short(&ask.session), "Esc written");
            if let Some(activity) = self.activity.get_mut(&ask.session) {
                activity.interrupt_written();
            }
            // Esc cancels a running compaction.
            self.compact_settled(&ask.session);
            if let Some(update) = self.updates.get_mut(&ask.session) {
                update.interrupted = true;
            }
            // Shown at once, whatever the edit pace.
            if let Some(shown) = self.shown.get_mut(&ask.slot) {
                shown.next_at = Some(Instant::now());
                shown.urgent = true;
            }
            return;
        }
        warn!(conn, session = short(&ask.session), "Esc not written");
        if let Some(thread_id) = self.registry.slot(ask.slot).and_then(|slot| slot.topic_id) {
            self.notify(ask.slot, thread_id, status::KEY_FAILED_NOTICE);
        }
    }

    /// A console command from topic message `message_id`: asks the agent of
    /// the slot's live session to type it, or answers why not.
    fn on_console_command(
        &mut self,
        slot: SlotId,
        thread_id: i64,
        message_id: i64,
        command: Result<String, console::Invalid>,
    ) {
        let ordinal = self.ordinal(slot);
        let refusal = match (command, self.live_agent(slot)) {
            (Err(console::Invalid), _) => console::INVALID_NOTICE,
            (Ok(_), None) => console::OFFLINE_NOTICE,
            (Ok(_), Some((_, conn)))
                if !self.conns.get(&conn).is_some_and(|bound| bound.commands) =>
            {
                console::NO_CONSOLE_NOTICE
            }
            (Ok(_), Some((session, _))) if self.waiting(&session) => console::WAITING_NOTICE,
            // Topic messages still in the slot, or texts that went a moment
            // ago, come before the command: the session is as good as busy.
            (Ok(_), Some((session, _)))
                if self.busy(&session) || self.kept(slot) || self.settling(&session) =>
            {
                console::BUSY_NOTICE
            }
            (Ok(text), Some((session, conn))) => {
                let command_id = crate::wire::random_u64();
                let asked = self.conns.get(&conn).is_some_and(|bound| {
                    bound
                        .to_agent
                        .try_send(HubMsg::ConsoleCommand { command_id, text })
                        .is_ok()
                });
                if asked {
                    self.remember_command(
                        command_id,
                        CommandAsk {
                            slot,
                            session: session.clone(),
                            conn,
                            thread_id,
                            message_id,
                            until: Instant::now() + console::COMMAND_WAIT,
                        },
                    );
                    info!(
                        ordinal,
                        conn,
                        session = short(&session),
                        "console command sent to the session agent"
                    );
                    return;
                }
                warn!(conn, "agent queue full or closed; console command not sent");
                console::FAILED_NOTICE
            }
        };
        info!(ordinal, "console command refused");
        self.answer_command(thread_id, message_id, refusal);
    }

    fn remember_command(&mut self, command_id: u64, ask: CommandAsk) {
        let now = Instant::now();
        self.command_asks.retain(|_, ask| ask.until > now);
        if self.command_asks.len() >= console::MAX_COMMAND_ASKS
            && let Some(oldest) = self
                .command_asks
                .iter()
                .min_by_key(|(_, ask)| ask.until)
                .map(|(id, _)| *id)
        {
            self.command_asks.remove(&oldest);
        }
        self.command_asks.insert(command_id, ask);
    }

    /// An answer to the topic message `message_id` of a console command.
    fn answer_command(&mut self, thread_id: i64, message_id: i64, text: &str) {
        let mut op = message_op(thread_id, text.to_owned());
        if let Op::Send { reply_to, .. } = &mut op {
            *reply_to = Some(message_id);
        }
        self.send_messages(vec![op]);
    }

    /// The text of a panel a console command opened, monospace, in reply to
    /// the command; one message, cut to the Telegram limit.
    fn answer_panel(&mut self, thread_id: i64, message_id: i64, panel: &str) {
        let Some(text) = split_for_telegram(panel, SplitOptions::default())
            .chunks
            .into_iter()
            .next()
        else {
            return;
        };
        let mut op = message_op(thread_id, text);
        if let Op::Send {
            text,
            html,
            reply_to,
            ..
        } = &mut op
        {
            *html = Some(pre(text));
            *reply_to = Some(message_id);
        }
        self.send_messages(vec![op]);
    }

    /// An agent typed a command, or could not. Typed gets 👀 on its topic
    /// message; the output comes with the stream, or as the text of the panel
    /// the command opened, sent monospace in reply. Like
    /// [`Self::on_key_written`], an answer for a slot, session or connection
    /// that moved on changes nothing.
    fn on_command_typed(
        &mut self,
        conn: u64,
        frame_session: &str,
        command_id: u64,
        outcome: CommandOutcome,
        panel: Option<String>,
    ) {
        let Some(ask) = self.command_asks.remove(&command_id) else {
            debug!(conn, "answer to a console command nothing waits for");
            return;
        };
        if ask.conn != conn
            || ask.session != frame_session
            || self.live_agent(ask.slot) != Some((ask.session.clone(), conn))
        {
            debug!(
                conn,
                "console command answer for a session that moved on; ignored"
            );
            return;
        }
        info!(
            conn,
            session = short(&ask.session),
            ?outcome,
            "console command answered"
        );
        match outcome {
            CommandOutcome::Sent => {
                self.react(ask.message_id, stream::ACCEPTED);
                if let Some(panel) = panel.filter(|panel| !panel.trim().is_empty()) {
                    self.answer_panel(ask.thread_id, ask.message_id, &panel);
                }
            }
            CommandOutcome::Draft => {
                self.answer_command(ask.thread_id, ask.message_id, console::DRAFT_NOTICE);
            }
            CommandOutcome::AgentsRunning => {
                self.answer_command(ask.thread_id, ask.message_id, console::AGENTS_NOTICE);
            }
            CommandOutcome::Failed | CommandOutcome::Other => {
                self.answer_command(ask.thread_id, ask.message_id, console::FAILED_NOTICE);
            }
        }
    }

    /// Text and keyboard the status message of `slot` should show now.
    fn status_view(
        &self,
        slot: SlotId,
        session: &str,
        now: Instant,
    ) -> (String, serde_json::Value) {
        let ended = self.registry.state(slot) == SlotState::Dead;
        let waiting = self.waiting(session);
        let activity = self.activity.get(session);
        let metrics = self
            .registry
            .sessions
            .get(session)
            .and_then(|entry| entry.metrics.as_ref());
        let compacting = self
            .compactions
            .get(session)
            .filter(|compaction| compaction.done.is_none() && compaction.settled.is_none());
        let phase = match compacting {
            Some(compaction) if !ended && !waiting => status::Phase::Compacting {
                auto: compaction.auto,
                minutes: now.saturating_duration_since(compaction.started).as_secs() / 60,
            },
            _ => status::phase(activity, ended, waiting),
        };
        let keys = !ended
            && self
                .live_agent(slot)
                .is_some_and(|(_, conn)| self.conns.get(&conn).is_some_and(|bound| bound.keys));
        let confirm = self.shown.get(&slot).is_some_and(|shown| {
            shown
                .confirm
                .as_ref()
                .is_some_and(|(armed, until)| armed == session && now < *until)
        });
        let update = !ended
            && self
                .live_agent(slot)
                .is_some_and(|(_, conn)| self.outdated(conn));
        let buttons = Buttons {
            interrupt: keys && !waiting && self.busy(session),
            confirm,
            update,
        };
        status::render(&phase, metrics, buttons)
    }

    /// Sends, pins and edits the status messages that need it: one call per
    /// slot at a time (plus a ⏹ edit next to a waiting refresh), edits at
    /// most once per `Options::status_every`, as background edits unless
    /// they show a ⏹ press.
    fn pump_status(&mut self) {
        let Some(every) = self.options.status_every else {
            return;
        };
        let now = Instant::now();
        for index in 0..self.registry.slots.len() {
            let slot = SlotId(index);
            let entry = &self.registry.slots[index];
            let (Some(thread_id), Some(session)) = (entry.topic_id, entry.current_session.clone())
            else {
                continue;
            };
            let message = entry.status;
            let separated = entry.pending_separator.is_none();
            let (in_flight, urgent) = self
                .shown
                .get(&slot)
                .map_or((0, false), |shown| (shown.in_flight, shown.urgent));
            // Only a ⏹ edit goes next to a call in flight.
            if in_flight > 1 || (in_flight == 1 && !urgent) {
                continue;
            }
            let (text, keyboard) = self.status_view(slot, &session, now);
            let live = self.registry.is_live_top_level(&session);
            let can_pin = self.options.can_pin;
            let shown = self.shown.entry(slot).or_default();
            let job = match message {
                None => {
                    // A new message only for a live session, after its
                    // separator, so it lands below it.
                    if in_flight > 0
                        || !live
                        || !separated
                        || shown.retry_at.is_some_and(|at| now < at)
                    {
                        continue;
                    }
                    StatusJob::Create {
                        thread_id,
                        text,
                        keyboard,
                    }
                }
                Some(StatusMessage {
                    message_id,
                    pinned: false,
                }) if can_pin && !shown.pin_failed => {
                    if in_flight > 0 {
                        continue;
                    }
                    StatusJob::Pin { message_id }
                }
                Some(StatusMessage { message_id, .. }) => {
                    let current = Some((text.clone(), keyboard.clone()));
                    if shown.content == current {
                        shown.urgent = false;
                        continue;
                    }
                    if shown.next_at.is_some_and(|at| now < at) {
                        continue;
                    }
                    StatusJob::Edit {
                        message_id,
                        text,
                        keyboard,
                    }
                }
            };
            shown.in_flight += 1;
            let background = if matches!(job, StatusJob::Edit { .. }) {
                shown.next_at = Some(now + every);
                !std::mem::take(&mut shown.urgent)
            } else {
                false
            };
            let op = match &job {
                StatusJob::Create {
                    thread_id,
                    text,
                    keyboard,
                } => Op::Send {
                    thread_id: Some(*thread_id),
                    text: text.clone(),
                    html: None,
                    reply_markup: Some(keyboard.clone()),
                    permission: false,
                    reply_to: None,
                    notify: false,
                },
                StatusJob::Edit {
                    message_id,
                    text,
                    keyboard,
                } => Op::Edit {
                    message_id: *message_id,
                    text: text.clone(),
                    reply_markup: Some(keyboard.clone()),
                    background,
                },
                StatusJob::Pin { message_id } => Op::Pin {
                    message_id: *message_id,
                },
            };
            self.hand_off(Work::Status { slot, job }, op);
        }
    }

    /// Telegram answered a status call.
    fn on_status_done(&mut self, slot: SlotId, job: StatusJob, delivery: Option<Delivery>) {
        let now = Instant::now();
        let every = self.options.status_every.unwrap_or(STATUS_EVERY);
        let retry_every = self.options.retry_every;
        let ordinal = self.ordinal(slot);
        let topic_id = self.registry.slot(slot).and_then(|slot| slot.topic_id);
        let message = self.registry.slot(slot).and_then(|slot| slot.status);
        let shown = self.shown.entry(slot).or_default();
        shown.in_flight = shown.in_flight.saturating_sub(1);
        if matches!(delivery, Some(Ok(Outcome::Superseded))) {
            // A ⏹ edit of the message took its place; its answer counts.
            return;
        }
        match job {
            StatusJob::Create {
                thread_id,
                text,
                keyboard,
            } => match delivery {
                Some(Ok(Outcome::Sent(sent))) if sent.message_id != 0 => {
                    // A message for a topic the slot no longer has stays there.
                    if topic_id != Some(thread_id) || message.is_some() {
                        return;
                    }
                    shown.content = Some((text, keyboard));
                    shown.next_at = Some(now + every);
                    shown.retry_at = None;
                    shown.send_warned = false;
                    if let Some(entry) = self.registry.slot_mut(slot) {
                        entry.status = Some(StatusMessage {
                            message_id: sent.message_id,
                            pinned: false,
                        });
                        self.registry.dirty = true;
                    }
                    info!(ordinal, "status message sent");
                }
                other => {
                    // Known limitation: a send whose answer was lost may have
                    // made a message; the retry makes another one.
                    shown.retry_at = Some(now + retry_every);
                    let first = !std::mem::replace(&mut shown.send_warned, true);
                    match other {
                        Some(Err(error)) if first => {
                            warn!(%error, ordinal, "status message not sent; retrying later");
                        }
                        Some(Err(error)) => {
                            debug!(%error, ordinal, "status message still not sent");
                        }
                        _ if first => {
                            warn!(ordinal, "status message got no answer; retrying later")
                        }
                        _ => debug!(ordinal, "status message still got no answer"),
                    }
                }
            },
            StatusJob::Edit {
                message_id,
                text,
                keyboard,
            } => {
                let applied = matches!(&delivery, Some(Ok(_)))
                    || delivery.as_ref().is_some_and(|delivery| {
                        telegram_error(delivery, &["message is not modified"])
                    });
                let gone = delivery.as_ref().is_some_and(|delivery| {
                    telegram_error(
                        delivery,
                        &["message to edit not found", "message can't be edited"],
                    )
                });
                if applied {
                    // The ⏹ question gets its whole wait from when it shows,
                    // however long its edit waited for the budget.
                    let asked = shown
                        .content
                        .as_ref()
                        .is_some_and(|(_, shown)| status::asks_confirm(shown));
                    if !asked
                        && status::asks_confirm(&keyboard)
                        && let Some((_, until)) = shown.confirm.as_mut()
                    {
                        *until = now + status::CONFIRM_FOR;
                    }
                    shown.content = Some((text, keyboard));
                } else if gone {
                    // Deleted in Telegram: a new one is sent and pinned.
                    shown.content = None;
                    if let Some(entry) = self.registry.slot_mut(slot)
                        && entry
                            .status
                            .is_some_and(|status| status.message_id == message_id)
                    {
                        entry.status = None;
                        self.registry.dirty = true;
                        info!(ordinal, "status message is gone; sending a new one");
                    }
                } else if let Some(Err(error)) = &delivery {
                    debug!(%error, ordinal, "status edit failed; tried again later");
                }
            }
            StatusJob::Pin { message_id } => match delivery {
                Some(Ok(_)) => {
                    if let Some(entry) = self.registry.slot_mut(slot)
                        && let Some(status) = entry
                            .status
                            .as_mut()
                            .filter(|status| status.message_id == message_id)
                    {
                        status.pinned = true;
                        self.registry.dirty = true;
                    }
                }
                other => {
                    shown.pin_failed = true;
                    if !self.pin_warned {
                        self.pin_warned = true;
                        match other {
                            Some(Err(error)) => {
                                warn!(%error, "cannot pin a status message; not tried again until the hub restarts");
                            }
                            _ => warn!(
                                "status message pin got no answer; not tried again until the hub restarts"
                            ),
                        }
                    }
                }
            },
        }
    }

    /// Hands the fixed answer of a selected prompt to an agent of its
    /// session. An agent that acknowledges verdicts decides the prompt with
    /// its ack; for an older agent the hand-off is all there is to know.
    /// `false`: no agent of the session could take it now.
    fn push_verdict(&mut self, key: u64) -> bool {
        let Some(prompt) = self.prompts.get(key) else {
            return false;
        };
        let State::Selected {
            behavior,
            verdict_id,
        } = prompt.state
        else {
            return false;
        };
        let Some((conn, bound)) = self
            .verdict_conn(prompt)
            .and_then(|conn| self.conns.get(&conn).map(|bound| (conn, bound)))
        else {
            info!(
                session = short(&prompt.session),
                "permission answer for an agent that is not on line; it waits"
            );
            return false;
        };
        let verdict = HubMsg::PermissionVerdict {
            request_id: prompt.request_id.clone(),
            behavior,
            verdict_id: bound.acks.then_some(verdict_id),
        };
        if bound.to_agent.try_send(verdict).is_err() {
            warn!(
                conn,
                "agent queue full or closed; the permission answer waits"
            );
            return false;
        }
        info!(
            conn,
            session = short(&prompt.session),
            ?behavior,
            "permission verdict forwarded to the session agent"
        );
        if !bound.acks {
            self.finish(key, State::Decided(behavior));
        }
        true
    }

    /// Selected prompts go again to their session's agents: after a link
    /// came back (`session`) or on the retry tick (all). The agent drops a
    /// verdict id it already passed on and acks it again.
    fn push_selected(&mut self, session: Option<&str>) {
        for key in self.prompts.selected() {
            let wanted = self
                .prompts
                .get(key)
                .is_some_and(|prompt| session.is_none_or(|session| prompt.session == session));
            if wanted {
                self.push_verdict(key);
            }
        }
    }

    /// The agent took the verdict: the prompt is decided.
    fn on_verdict_ack(&mut self, conn: u64, frame_session: &str, verdict_id: u64) {
        let Some(key) = self.prompts.by_verdict(verdict_id) else {
            debug!(conn, "ack of a verdict nothing waits for");
            return;
        };
        let Some(prompt) = self.prompts.get(key) else {
            return;
        };
        let State::Selected { behavior, .. } = prompt.state else {
            return;
        };
        if frame_session != prompt.session {
            debug!(
                conn,
                "verdict ack from an agent of another session; ignored"
            );
            return;
        }
        info!(
            conn,
            session = short(&prompt.session),
            "permission verdict taken by the session agent"
        );
        self.finish(key, State::Decided(behavior));
    }

    /// An agent bound to the prompt's session: the one that relayed it, or
    /// after a link drop the newest connection of the same claude process.
    /// A connection that follows another session (after `/clear`) or another
    /// process on a reused pid never qualifies.
    fn verdict_conn(&self, prompt: &Prompt) -> Option<u64> {
        let serves = |bound: &Conn| bound.session == prompt.session;
        if self.conns.get(&prompt.conn).is_some_and(serves) {
            return Some(prompt.conn);
        }
        let pid = prompt.claude_pid?;
        self.conns
            .iter()
            .filter(|(_, bound)| {
                serves(bound) && bound.host == prompt.host && bound.claude_pid == Some(pid)
            })
            .map(|(conn, _)| *conn)
            .max()
    }

    /// Sends `notice` to the slot's topic unless the slot got it within
    /// `notice_every`.
    fn notify(&mut self, slot: SlotId, thread_id: i64, notice: &'static str) {
        let now = Instant::now();
        if self
            .notices
            .get(&(slot, notice))
            .is_some_and(|&last| now < last + self.options.notice_every)
        {
            debug!(
                ordinal = self.ordinal(slot),
                "notice sent to this slot recently; not repeated"
            );
            return;
        }
        if self.send_messages(vec![message_op(thread_id, notice.to_owned())]) {
            self.notices.insert((slot, notice), now);
        }
    }

    /// Hands all `ops` to the dispatch task in order, or none of them when
    /// that would pass [`MAX_QUEUED_MESSAGES`].
    fn send_messages(&mut self, ops: Vec<Op>) -> bool {
        if self.queued_messages + ops.len() > MAX_QUEUED_MESSAGES {
            if !self.overflow_warned {
                self.overflow_warned = true;
                warn!(
                    "too many messages wait for Telegram; new replies, turn answers and notices are dropped"
                );
            }
            return false;
        }
        self.queued_messages += ops.len();
        for op in ops {
            self.hand_off(Work::Message, op);
        }
        true
    }

    /// Never waits: the dispatch task does.
    fn hand_off(&self, work: Work, op: Op) {
        if self.dispatch.send((work, op)).is_err() {
            debug!("dispatch task stopped; job dropped");
        }
    }

    fn on_tick(&mut self) {
        let now = Instant::now();
        self.key_asks.retain(|_, ask| ask.until > now);
        self.command_asks.retain(|_, ask| ask.until > now);
        let late: Vec<u64> = self
            .reads
            .iter()
            .filter(|(_, pending)| pending.until <= now)
            .map(|(id, _)| *id)
            .collect();
        for id in late {
            if let Some(pending) = self.reads.remove(&id) {
                self.read_failed(pending.purpose, Unavailable::NoAnswer);
            }
        }
        if now >= self.next_retry {
            self.registry.retry_failed();
            self.prompts.retry_failed_edits();
            self.questions.retry_edits();
            self.push_selected(None);
            self.next_retry = now + self.options.retry_every;
        }
        self.check_candidates();
        self.retry_bodies();
        self.check_compactions(now);
        let untold: Vec<u64> = self
            .conns
            .iter()
            .filter(|(_, bound)| bound.untold)
            .map(|(conn, _)| *conn)
            .collect();
        for conn in untold {
            self.tell_bound(conn);
        }
    }

    fn ordinal(&self, slot: SlotId) -> u32 {
        self.registry.slot(slot).map_or(0, |slot| slot.ordinal)
    }

    fn on_done(&mut self, done: Done) {
        match done {
            Done::Topic { job, delivery } => self.on_topic_done(job, delivery),
            Done::Delete(delivery) => match delivery {
                Some(Ok(_)) | None => {}
                Some(Err(error)) if !self.delete_warned => {
                    self.delete_warned = true;
                    warn!(%error, "cannot delete a forum service message; later failures are not logged");
                }
                Some(Err(error)) => debug!(%error, "forum service message not deleted"),
            },
            Done::Message(delivery) => {
                self.queued_messages = self.queued_messages.saturating_sub(1);
                if self.queued_messages == 0 {
                    self.overflow_warned = false;
                }
                match delivery {
                    Some(Ok(_)) => {}
                    Some(Err(error)) => warn!(%error, "message to a topic not delivered"),
                    None => warn!("message to a topic got no answer"),
                }
            }
            Done::Permission { key, delivery } => {
                match delivery {
                    Some(Ok(Outcome::Sent(message))) if message.message_id != 0 => {
                        self.prompts.delivered(key, message.message_id);
                        return;
                    }
                    Some(Ok(_)) => {
                        warn!(
                            "permission prompt sent without a message id; its buttons cannot work"
                        );
                    }
                    Some(Err(error)) => {
                        warn!(%error, "permission prompt not delivered; it can be answered in the terminal");
                    }
                    None => warn!("permission prompt got no answer"),
                }
                if let Some(gone) = self.prompts.remove(key) {
                    self.sync_waiting(&gone.session);
                }
            }
            Done::PromptEdit { key, delivery } => self.on_prompt_edit_done(key, delivery),
            Done::Question {
                key,
                version,
                delivery,
            } => {
                self.on_question_done(key, version, delivery);
                self.release_held();
            }
            Done::QuestionEdit {
                key,
                version,
                delivery,
            } => self.on_question_edit_done(key, version, delivery),
            Done::Callback(delivery) => {
                if let Some(Err(error)) = delivery {
                    debug!(%error, "button answer or expired prompt edit failed");
                }
            }
            Done::Resume {
                slot,
                number,
                delivery,
            } => self.on_resume_done(slot, number, delivery),
            Done::Block { job, delivery } => self.on_block_done(job, delivery),
            Done::Stream {
                session,
                number,
                delivery,
            } => self.on_stream_done(&session, number, delivery),
            Done::Reaction(delivery) => {
                self.reactions = self.reactions.saturating_sub(1);
                self.on_reaction_done(delivery);
            }
            Done::Status {
                slot,
                job,
                delivery,
            } => self.on_status_done(slot, job, delivery),
            Done::Fetched {
                slot,
                transfer_id,
                outcome,
            } => self.on_fetched(slot, transfer_id, outcome),
            Done::File {
                conn,
                transfer_id,
                size,
                delivery,
            } => self.on_file_done(conn, transfer_id, size, delivery),
            Done::Album {
                conn,
                transfer_id,
                size,
                parts,
                delivery,
            } => self.on_album_done(conn, transfer_id, size, &parts, delivery),
        }
    }

    fn on_reaction_done(&mut self, delivery: Option<Delivery>) {
        match delivery {
            Some(Err(error)) if !self.reaction_warned => {
                self.reaction_warned = true;
                warn!(%error, "cannot set a message reaction; later failures are not logged");
            }
            Some(Err(error)) => debug!(%error, "message reaction not set"),
            _ => {}
        }
    }

    fn on_block_done(&mut self, job: BlockJob, delivery: Option<Delivery>) {
        self.block_jobs = self.block_jobs.saturating_sub(1);
        let (key, text, message_id) = match (&job, &delivery) {
            (BlockJob::Send { key, text, .. }, Some(Ok(Outcome::Sent(message))))
                if message.message_id != 0 =>
            {
                (key, text, Some(message.message_id))
            }
            (BlockJob::Edit { key, text, .. }, Some(delivery))
                if delivery.is_ok()
                    || telegram_error(
                        delivery,
                        &[
                            "message is not modified",
                            "message to edit not found",
                            "message can't be edited",
                        ],
                    ) =>
            {
                (key, text, None)
            }
            // Only a refusal proves Telegram has no such message; anything
            // else may have been shown, and a first send goes at most once.
            (BlockJob::Send { key, .. }, delivery) if !send_refused(delivery.as_ref()) => {
                warn!("block message may have been sent without an answer; not sent again");
                self.registry.block_send_unclear(key);
                return;
            }
            (BlockJob::Send { key, .. } | BlockJob::Edit { key, .. }, _) => {
                if self.registry.block_failed(key) {
                    warn!("block message keeps failing; given up");
                } else {
                    debug!("block message failed; retrying later");
                }
                return;
            }
        };
        if let Some(notice) = self.registry.block_done(key, text, message_id) {
            // At most once: the block is marked notified whether or not
            // this send goes through.
            self.send_messages(vec![Op::Send {
                thread_id: Some(notice.thread_id),
                text: notice.text,
                html: None,
                reply_markup: None,
                permission: false,
                reply_to: Some(notice.reply_to),
                notify: true,
            }]);
        }
    }

    fn on_prompt_edit_done(&mut self, key: u64, delivery: Option<Delivery>) {
        let applied = match &delivery {
            Some(Ok(_)) => true,
            // Shown already, or the message is gone: nothing left to fix.
            Some(delivery) => telegram_error(
                delivery,
                &[
                    "message is not modified",
                    "message to edit not found",
                    "message can't be edited",
                ],
            ),
            None => false,
        };
        if applied {
            self.prompts.edit_done(key);
            return;
        }
        let attempts = self.prompts.edit_failed(key);
        if attempts == 1 {
            warn!("permission prompt edit failed; its buttons stay until a retry works");
        } else if attempts >= permissions::MAX_EDIT_ATTEMPTS {
            warn!("permission prompt edit keeps failing; given up");
        } else {
            debug!(attempts, "permission prompt edit failed again");
        }
    }

    fn on_topic_done(&mut self, job: TopicJob, delivery: Option<Delivery>) {
        let icons = &self.options.icons;
        let Some(delivery) = delivery else {
            // The scheduler stopped: handing the job out again would come
            // back at once, so it waits for the retry tick like a failure.
            let (TopicJob::Create { slot, .. }
            | TopicJob::Edit { slot, .. }
            | TopicJob::Separator { slot, .. }) = job;
            warn!(
                ordinal = self.ordinal(slot),
                "topic call got no answer; retrying later"
            );
            self.registry.topic_failed(slot, icons);
            return;
        };
        match job {
            TopicJob::Create { slot, name, icon } => match delivery {
                Ok(Outcome::Topic(topic)) if topic.message_thread_id != 0 => {
                    info!(ordinal = self.ordinal(slot), "forum topic created");
                    self.registry.topic_created(
                        slot,
                        topic.message_thread_id,
                        &name,
                        icon.as_deref(),
                    );
                }
                Ok(_) => {
                    warn!("createForumTopic answered without a topic id");
                    self.registry.topic_failed(slot, icons);
                }
                Err(error) => {
                    // Known limitation (TASK-011 review I3): createForumTopic
                    // has no idempotency key. If Telegram made the topic but
                    // the answer was lost (or the hub died before saving it),
                    // the retry makes a second one and the first is orphaned.
                    warn!(%error, ordinal = self.ordinal(slot), "cannot create a forum topic; retrying later");
                    self.registry.topic_failed(slot, icons);
                }
            },
            TopicJob::Edit {
                slot,
                thread_id,
                name,
                icon,
            } => {
                if delivery.is_ok() || not_modified(&delivery) {
                    self.registry
                        .topic_edited(slot, thread_id, name.as_deref(), icon.as_deref());
                } else if topic_gone(&delivery) {
                    warn!(
                        ordinal = self.ordinal(slot),
                        "forum topic is gone; creating a replacement"
                    );
                    self.registry.topic_invalid(slot, thread_id);
                } else {
                    if let Err(error) = &delivery {
                        warn!(%error, ordinal = self.ordinal(slot), "cannot edit a forum topic; retrying later");
                    }
                    self.registry.topic_failed(slot, icons);
                }
            }
            TopicJob::Separator {
                slot,
                thread_id,
                text,
            } => {
                if delivery.is_ok() {
                    self.registry.topic_separated(slot, thread_id, &text);
                } else if topic_gone(&delivery) {
                    warn!(
                        ordinal = self.ordinal(slot),
                        "forum topic is gone; creating a replacement"
                    );
                    self.registry.topic_invalid(slot, thread_id);
                } else {
                    // Known limitation (TASK-011 QA O1): a separator that
                    // always fails is retried every tick and holds back the
                    // slot's name and icon edits, which come after it.
                    if let Err(error) = &delivery {
                        warn!(%error, ordinal = self.ordinal(slot), "session separator not delivered; retrying later");
                    }
                    self.registry.topic_failed(slot, icons);
                }
            }
        }
    }

    /// Hands kept messages to sessions that can take them now, offers Resume
    /// buttons, hands pending topic work and prompts to the dispatch task,
    /// publishes the snapshot to save.
    fn pump(&mut self) {
        self.check_hook_asks();
        self.check_questions();
        self.send_continuations();
        self.flush_all();
        self.offer_resume();
        let edits = Instant::now() >= self.grace_until;
        for job in self.registry.topic_work(&self.options.icons, edits) {
            let op = match &job {
                TopicJob::Create { name, icon, .. } => Op::CreateTopic {
                    name: name.clone(),
                    icon_custom_emoji_id: icon.clone(),
                },
                TopicJob::Edit {
                    thread_id,
                    name,
                    icon,
                    ..
                } => Op::EditTopic {
                    thread_id: *thread_id,
                    name: name.clone(),
                    icon_custom_emoji_id: icon.clone(),
                },
                TopicJob::Separator {
                    thread_id, text, ..
                } => Op::Send {
                    thread_id: Some(*thread_id),
                    text: text.clone(),
                    html: None,
                    reply_markup: None,
                    permission: false,
                    reply_to: None,
                    notify: false,
                },
            };
            self.hand_off(Work::Topic(job), op);
        }
        let room = MAX_BLOCK_JOBS.saturating_sub(self.block_jobs);
        for job in self.registry.block_work(room) {
            self.block_jobs += 1;
            let op = match &job {
                BlockJob::Send {
                    thread_id, text, ..
                } => message_op(*thread_id, text.clone()),
                BlockJob::Edit {
                    message_id, text, ..
                } => Op::Edit {
                    message_id: *message_id,
                    text: text.clone(),
                    reply_markup: None,
                    background: false,
                },
            };
            self.hand_off(Work::Block(job), op);
        }
        self.send_prompts();
        self.send_questions();
        self.send_prompt_edits();
        self.send_question_edits();
        self.pump_streams();
        self.warn_outdated();
        self.pump_updates();
        self.pump_status();
        if self.registry.dirty {
            self.registry.dirty = false;
            self.saver
                .send_replace(Some(Arc::new(RegistryStore::encode(&self.registry))));
        }
    }
}

/// The stream messages of turn answer `held`: its `split_for_telegram`
/// chunks in order, the last one carrying the answer so a rewind can hold it
/// again (see [`Live::rewind`]). Like stream lines they wait behind a refused
/// line of their topic. `Err`: the answer goes outside the stream, as a file,
/// or not at all when its messages do not fit in `room` (the messages that
/// may still wait for Telegram, [`MAX_QUEUED_MESSAGES`]): nothing is tracked
/// for an answer [`Slots::send_text`] then drops. Either way, and for a blank
/// answer, its turn end counts as answered ([`Live::answered_outside`]).
fn answer_ops(live: &mut Live, held: Held, room: usize) -> Result<Vec<(u64, Op)>, Held> {
    if held.answer.trim().is_empty() {
        live.answered_outside(&held);
        return Ok(Vec::new());
    }
    let split = split_markdown_for_telegram(&held.answer, SplitOptions::default());
    if split.prefer_file || split.chunks.len() > room {
        live.answered_outside(&held);
        return Err(held);
    }
    let thread_id = held.thread_id;
    let mut chunks = split.chunks;
    let Some(last) = chunks.pop() else {
        live.answered_outside(&held);
        return Ok(Vec::new());
    };
    let op = |live: &mut Live, chunk: HtmlChunk| Op::Stream {
        thread_id,
        text: chunk.text,
        html: Some(chunk.html),
        merge: false,
        restart: std::mem::take(&mut live.restart),
        notify: true,
    };
    let mut ops = Vec::with_capacity(chunks.len() + 1);
    for chunk in chunks {
        let message = op(live, chunk);
        ops.push((live.sent(), message));
    }
    let message = op(live, last);
    ops.push((live.sent_answer(held), message));
    Ok(ops)
}

/// The messages of a stream line: markdown as HTML with its plain source,
/// code as one `<pre>` block, plain text as is.
fn stream_chunks(text: &str, format: Format) -> Vec<(String, Option<String>)> {
    match format {
        Format::Markdown => split_markdown_for_telegram(text, SplitOptions::default())
            .chunks
            .into_iter()
            .map(|chunk| (chunk.text, Some(chunk.html)))
            .collect(),
        Format::Code | Format::Plain => split_for_telegram(text, SplitOptions::default())
            .chunks
            .into_iter()
            .map(|text| {
                let html = (format == Format::Code).then(|| pre(&text));
                (text, html)
            })
            .collect(),
    }
}

/// `text` as one monospace block of Telegram HTML.
fn pre(text: &str) -> String {
    format!("<pre>{}</pre>", escape_html(text))
}

/// A compaction of a live top-level session (TASK-053).
struct Compaction {
    /// `Some(true)`: auto, `Some(false)`: `/compact`, `None`: not known.
    auto: Option<bool>,
    started: Instant,
    /// The context percentage of the status line when it began, and the
    /// last one before its end once it ended.
    before: Option<u32>,
    /// Not ended, but the session showed activity since: not shown any more,
    /// its end is still taken until [`COMPACT_GRACE`] after this.
    settled: Option<Instant>,
    /// Ended: how long it took, and until when its line waits for the new
    /// percentage.
    done: Option<(Duration, Instant)>,
}

/// A plain message without a sound.
fn message_op(thread_id: i64, text: String) -> Op {
    Op::Send {
        thread_id: Some(thread_id),
        text,
        html: None,
        reply_markup: None,
        permission: false,
        reply_to: None,
        notify: false,
    }
}

/// Enqueues jobs in order, waiting for room in the scheduler queue so the
/// actor never has to; each answer comes back as a [`Done`].
async fn dispatch_loop(
    outbox: Outbox,
    mut work: mpsc::UnboundedReceiver<(Work, Op)>,
    done: mpsc::UnboundedSender<Done>,
) {
    while let Some((work, op)) = work.recv().await {
        let answer = outbox.submit(op).await;
        let done = done.clone();
        tokio::spawn(async move {
            let delivery = answer.await.ok();
            let _ = done.send(match work {
                Work::Topic(job) => Done::Topic { job, delivery },
                Work::Delete => Done::Delete(delivery),
                Work::Message => Done::Message(delivery),
                Work::Permission(key) => Done::Permission { key, delivery },
                Work::PromptEdit(key) => Done::PromptEdit { key, delivery },
                Work::Question { key, version } => Done::Question {
                    key,
                    version,
                    delivery,
                },
                Work::QuestionEdit { key, version } => Done::QuestionEdit {
                    key,
                    version,
                    delivery,
                },
                Work::Callback => Done::Callback(delivery),
                Work::Resume { slot, number } => Done::Resume {
                    slot,
                    number,
                    delivery,
                },
                Work::Block(job) => Done::Block { job, delivery },
                Work::Stream { session, number } => Done::Stream {
                    session,
                    number,
                    delivery,
                },
                Work::Reaction => Done::Reaction(delivery),
                Work::Status { slot, job } => Done::Status {
                    slot,
                    job,
                    delivery,
                },
                Work::File {
                    conn,
                    transfer_id,
                    size,
                } => Done::File {
                    conn,
                    transfer_id,
                    size,
                    delivery,
                },
                Work::Album {
                    conn,
                    transfer_id,
                    size,
                    parts,
                } => Done::Album {
                    conn,
                    transfer_id,
                    size,
                    parts,
                    delivery,
                },
            });
        });
    }
}

async fn save_once(store: &RegistryStore, bytes: Arc<Vec<u8>>) -> std::io::Result<()> {
    let store = store.clone();
    tokio::task::spawn_blocking(move || store.save(&bytes))
        .await
        .map_err(|_| std::io::Error::other("registry save worker failed"))?
}

/// Writes the latest snapshot; snapshots that arrive meanwhile collapse into one.
async fn save_loop(store: RegistryStore, mut saves: watch::Receiver<Option<Arc<Vec<u8>>>>) {
    while saves.changed().await.is_ok() {
        let Some(bytes) = saves.borrow_and_update().clone() else {
            continue;
        };
        let mut result = save_once(&store, bytes.clone()).await;
        for wait in SAVE_RETRY_WAITS {
            if result.is_ok() {
                break;
            }
            tokio::time::sleep(wait).await;
            result = save_once(&store, bytes.clone()).await;
        }
        if let Err(error) = result {
            warn!(kind = ?error.kind(), "cannot save the slot registry; the next change tries again");
        }
    }
}

#[cfg(test)]
mod tests {
    use std::collections::HashSet;
    use std::sync::Mutex;
    use std::sync::atomic::{AtomicBool, Ordering};

    use super::*;
    use crate::hub::api::{ForumTopic, Message};
    use crate::hub::registry::{ICON_ALIVE, ICON_DEAD, ICON_NO_CHANNEL};
    use crate::hub::scheduler::{BucketConfig, Limits, Scheduler, Transport};
    use crate::hub::testdir::TempDir;
    use crate::wire::{
        AskedOption, AskedQuestion, Behavior, HookEvent, PermissionRequest, QuestionPost, Register,
    };

    const A: &str = "aaaaaaaa-0000-4000-8000-000000000001";
    const B: &str = "bbbbbbbb-0000-4000-8000-000000000002";
    const CWD: &str = r"C:\Work\Project";
    // Upper bound for events a test expects; a stall shows up as a hang, so a
    // generous bound only protects against a loaded host, it hides nothing.
    const WAIT: Duration = Duration::from_secs(60);

    /// Records every op. Topics are numbered from 100, sent messages from
    /// 1000. `edit_errors` and
    /// `send_errors` answer the next edits or sends with these Telegram
    /// errors (last first). `stall`: no call ever returns.
    #[derive(Default)]
    struct Fake {
        ops: Mutex<Vec<Op>>,
        next_topic: Mutex<i64>,
        next_message: Mutex<i64>,
        edit_errors: Mutex<Vec<&'static str>>,
        send_errors: Mutex<Vec<&'static str>>,
        /// Answers the next `editMessageText` calls (last first).
        message_edit_errors: Mutex<Vec<(i64, &'static str)>>,
        delete_error: Option<&'static str>,
        stall: bool,
        /// Only sends never return; topic calls still answer.
        stall_sends: bool,
        /// The next sends reach Telegram but the answer cannot be read.
        unclear_sends: Mutex<usize>,
        /// Every reaction is refused (no such reaction in the chat).
        react_error: bool,
        /// The next stream messages fail with a 502.
        stream_errors: Mutex<usize>,
    }

    impl Fake {
        fn take_stream_error(&self) -> bool {
            let mut left = self.stream_errors.lock().unwrap();
            let fail = *left > 0;
            *left = left.saturating_sub(1);
            fail
        }
    }

    impl Transport for Fake {
        async fn execute(&self, op: &Op) -> Delivery {
            self.ops.lock().unwrap().push(op.clone());
            let send = matches!(
                op,
                Op::Send { .. } | Op::SendDocument { .. } | Op::SendPhoto { .. }
            );
            if self.stall || (self.stall_sends && send) {
                return std::future::pending().await;
            }
            let error = |description: &str| {
                Err(ApiError::Telegram {
                    code: 400,
                    description: description.to_owned(),
                })
            };
            match op {
                Op::CreateTopic { name, .. } => {
                    let mut next = self.next_topic.lock().unwrap();
                    *next = (*next).max(100);
                    let topic = ForumTopic {
                        message_thread_id: *next,
                        name: name.clone(),
                        icon_custom_emoji_id: None,
                    };
                    *next += 1;
                    Ok(Outcome::Topic(topic))
                }
                Op::EditTopic { .. } => match self.edit_errors.lock().unwrap().pop() {
                    Some(description) => error(description),
                    None => Ok(Outcome::Done),
                },
                Op::Edit { .. } => match self.message_edit_errors.lock().unwrap().pop() {
                    Some((code, description)) => Err(ApiError::Telegram {
                        code,
                        description: description.to_owned(),
                    }),
                    None => Ok(Outcome::Done),
                },
                Op::React { .. } if self.react_error => error("Bad Request: REACTION_INVALID"),
                Op::Stream { .. } if self.take_stream_error() => Err(ApiError::Telegram {
                    code: 502,
                    description: "Bad Gateway".to_owned(),
                }),
                Op::Delete { .. } => match self.delete_error {
                    Some(description) => error(description),
                    None => Ok(Outcome::Done),
                },
                Op::Send { .. } if self.take_unclear_send() => Err(ApiError::Decode(
                    serde_json::from_str::<i64>("x").unwrap_err(),
                )),
                Op::Send { .. } => match self.send_errors.lock().unwrap().pop() {
                    Some(description) => error(description),
                    None => {
                        let mut next = self.next_message.lock().unwrap();
                        *next = (*next).max(1000);
                        let message_id = *next;
                        *next += 1;
                        Ok(Outcome::Sent(Message {
                            message_id,
                            ..Message::default()
                        }))
                    }
                },
                _ => Ok(Outcome::Sent(Message::default())),
            }
        }
    }

    impl Fake {
        fn take_unclear_send(&self) -> bool {
            let mut unclear = self.unclear_sends.lock().unwrap();
            let take = *unclear > 0;
            if take {
                *unclear -= 1;
            }
            take
        }

        fn ops(&self) -> Vec<Op> {
            self.ops.lock().unwrap().clone()
        }
    }

    struct Rig {
        fake: Arc<Fake>,
        agents: mpsc::Sender<AgentEvent>,
        hooks: mpsc::Sender<HookPost>,
        control: mpsc::UnboundedSender<Control>,
        dir: TempDir,
        _to_agent: Vec<mpsc::Receiver<HubMsg>>,
        asks: mpsc::Sender<PermissionAsk>,
        questions: mpsc::Sender<QuestionAsk>,
    }

    fn options() -> Options {
        Options {
            grace: Duration::ZERO,
            ..Options::default()
        }
    }

    fn rig(fake: Fake, options: Options) -> Rig {
        let dir = TempDir::new("slots");
        rig_in(fake, options, dir)
    }

    fn rig_in(fake: Fake, options: Options, dir: TempDir) -> Rig {
        rig_with(fake, options, dir, BucketConfig::default())
    }

    fn rig_with(fake: Fake, options: Options, dir: TempDir, limits: impl Into<Limits>) -> Rig {
        let fake = Arc::new(fake);
        let store = RegistryStore::open(dir.path()).unwrap();
        let registry = store.load().unwrap();
        let (scheduler, outbox) = Scheduler::new(fake.clone(), limits);
        tokio::spawn(scheduler.run());
        let mut slots = Slots::new(registry, store, outbox, options);
        let asks = slots.permission_asks();
        let questions = slots.question_asks();
        let (agents, agents_rx) = mpsc::channel(16);
        let (hooks, hooks_rx) = mpsc::channel(16);
        let (control, control_rx) = mpsc::unbounded_channel();
        tokio::spawn(slots.run(agents_rx, hooks_rx, control_rx));
        Rig {
            fake,
            agents,
            hooks,
            control,
            dir,
            _to_agent: Vec::new(),
            asks,
            questions,
        }
    }

    fn hook(session: &str, event: HookEvent) -> HookPost {
        HookPost::new(
            "box".into(),
            session.into(),
            CWD.into(),
            String::new(),
            event,
        )
    }

    fn start(session: &str, pid: u32) -> HookPost {
        hook(
            session,
            HookEvent::SessionStart {
                source: Some("startup".into()),
                claude_pid: Some(pid),
                parent_claude_pid: None,
            },
        )
    }

    impl Rig {
        async fn hook(&self, post: HookPost) {
            self.hooks.send(post).await.unwrap();
        }

        async fn agent(&mut self, conn: u64, session: &str) {
            self.agent_of(conn, session, None).await;
        }

        /// An agent that reports the pid of its claude process and does not
        /// acknowledge verdicts (built before TASK-014).
        async fn agent_of(&mut self, conn: u64, session: &str, claude_pid: Option<u32>) {
            self.agent_with(conn, session, claude_pid, false).await;
        }

        async fn agent_with(
            &mut self,
            conn: u64,
            session: &str,
            claude_pid: Option<u32>,
            verdict_ack: bool,
        ) {
            let (to_agent, rx) = mpsc::channel(4);
            self._to_agent.push(rx);
            let register = Register {
                session_id: session.into(),
                host: "box".into(),
                cwd: CWD.into(),
                claude_pid,
                verdict_ack,
                transcript_reads: false,
                console_keys: false,
                console_commands: false,
                client: None,
                files: false,
                session_reads: false,
                status_lines: false,
                heartbeat: false,
            };
            self.agents
                .send(AgentEvent::Registered {
                    conn,
                    register,
                    to_agent,
                })
                .await
                .unwrap();
        }

        /// Waits until `count` ops were made, then a little more to catch extras.
        async fn ops_after(&self, count: usize) -> Vec<Op> {
            let reached = async {
                while self.fake.ops().len() < count {
                    tokio::time::sleep(Duration::from_millis(5)).await;
                }
            };
            tokio::time::timeout(WAIT, reached)
                .await
                .expect("ops in time");
            tokio::time::sleep(Duration::from_millis(100)).await;
            self.fake.ops()
        }
    }

    fn icon_edit(op: &Op) -> Option<&str> {
        match op {
            Op::EditTopic {
                icon_custom_emoji_id: Some(icon),
                ..
            } => Some(icon),
            _ => None,
        }
    }

    const CHAT: i64 = -1000000000001;

    fn say(thread_id: Option<i64>, message_id: i64, text: Option<&str>) -> Control {
        Control::Message(Inbound {
            message_id,
            thread_id,
            text: text.map(str::to_owned),
            reply_to: None,
            quote: None,
            forwarded: false,
            media: None,
            from_name: None,
        })
    }

    fn message_options() -> Options {
        Options {
            chat_id: CHAT,
            ..options()
        }
    }

    /// What agent `index` (in registration order) received, after a pause.
    async fn received(rig: &mut Rig, index: usize) -> Vec<HubMsg> {
        tokio::time::sleep(Duration::from_millis(200)).await;
        let mut got = Vec::new();
        while let Ok(msg) = rig._to_agent[index].try_recv() {
            got.push(msg);
        }
        got
    }

    /// Texts of the messages sent with a sound (TASK-041).
    fn loud(ops: &[Op]) -> Vec<&str> {
        ops.iter()
            .filter_map(|op| match op {
                Op::Send {
                    text, notify: true, ..
                }
                | Op::Stream {
                    text, notify: true, ..
                } => Some(text.as_str()),
                _ => None,
            })
            .collect()
    }

    fn sent_to(ops: &[Op], thread: i64) -> Vec<&str> {
        ops.iter()
            .filter_map(|op| match op {
                Op::Send {
                    thread_id: Some(t),
                    text,
                    ..
                } if *t == thread => Some(text.as_str()),
                _ => None,
            })
            .collect()
    }

    /// Topic 100 for A (agent 0, conn 1) and, with `second`, topic 101 for B
    /// (agent 1, conn 2); waits until both are alive.
    async fn two_live_slots(rig: &mut Rig, second: bool) {
        rig.hook(start(A, 10)).await;
        rig.ops_after(1).await;
        rig.agent_of(1, A, Some(10)).await;
        if second {
            rig.hook(start(B, 11)).await;
            settled(rig, |ops| count(ops, is_create) == 2).await;
            rig.agent_of(2, B, Some(11)).await;
            settled(rig, |ops| last_icon(ops, 101) == Some(ICON_ALIVE)).await;
        }
        settled(rig, |ops| last_icon(ops, 100) == Some(ICON_ALIVE)).await;
    }

    #[tokio::test]
    async fn a_topic_message_reaches_only_the_agent_of_its_slot() {
        let mut rig = rig(Fake::default(), message_options());
        two_live_slots(&mut rig, true).await;
        let before = rig.fake.ops().len();
        rig.control.send(say(Some(101), 42, Some("hi B"))).unwrap();
        rig.control
            .send(Control::Message(Inbound {
                message_id: 43,
                thread_id: Some(101),
                text: Some("again".into()),
                reply_to: Some(40),
                quote: Some("Удалить build/?".into()),
                forwarded: false,
                media: None,
                from_name: None,
            }))
            .unwrap();
        rig.control
            .send(Control::Message(Inbound {
                message_id: 46,
                thread_id: Some(101),
                text: Some("чужие слова".into()),
                reply_to: None,
                quote: None,
                forwarded: true,
                media: None,
                from_name: None,
            }))
            .unwrap();
        // General and a topic that is no slot reach nobody and say nothing.
        rig.control.send(say(None, 44, Some("general"))).unwrap();
        rig.control
            .send(say(Some(999), 45, Some("foreign")))
            .unwrap();

        let got = received(&mut rig, 1).await;
        let meta = |pairs: &[(&str, &str)]| -> BTreeMap<String, String> {
            pairs
                .iter()
                .map(|(k, v)| ((*k).to_owned(), (*v).to_owned()))
                .collect()
        };
        assert_eq!(
            got,
            [
                HubMsg::Inbound {
                    content: "hi B".into(),
                    meta: meta(&[
                        ("chat_id", "-1000000000001"),
                        ("message_id", "42"),
                        ("thread_id", "101"),
                    ]),
                },
                HubMsg::Inbound {
                    content: "> Удалить build/?

again"
                        .into(),
                    meta: meta(&[
                        ("chat_id", "-1000000000001"),
                        ("message_id", "43"),
                        ("reply_to_message_id", "40"),
                        ("thread_id", "101"),
                    ]),
                },
                HubMsg::Inbound {
                    content: "(переслано)
чужие слова"
                        .into(),
                    meta: meta(&[
                        ("chat_id", "-1000000000001"),
                        ("forwarded", "true"),
                        ("message_id", "46"),
                        ("thread_id", "101"),
                    ]),
                },
            ]
        );
        for msg in &got {
            if let HubMsg::Inbound { meta, .. } = msg {
                assert!(meta.keys().all(|key| crate::channel::is_meta_key(key)));
            }
        }
        assert!(received(&mut rig, 0).await.is_empty(), "A got nothing");
        // No notice; each delivered message only gets its 👀.
        let ops = rig.fake.ops();
        let after: Vec<(i64, &str)> = ops[before..]
            .iter()
            .map(|op| match op {
                Op::React { message_id, emoji } => (*message_id, emoji.as_str()),
                other => panic!("unexpected {other:?}"),
            })
            .collect();
        assert_eq!(after, [(42, "👀"), (43, "👀"), (46, "👀")]);
    }

    #[tokio::test]
    async fn a_message_nobody_can_take_now_is_kept_and_told_once() {
        let options = Options {
            notice_every: Duration::ZERO,
            ..message_options()
        };
        let mut rig = rig(Fake::default(), options);
        rig.hook(start(A, 10)).await;
        rig.ops_after(1).await;
        // A running session without an agent: kept, told once per period.
        rig.control.send(say(Some(100), 1, Some("one"))).unwrap();
        rig.control.send(say(Some(100), 2, Some("two"))).unwrap();
        settled(&rig, |ops| sent_to(ops, 100).len() == 1).await;
        // A sticker: it never reaches a session.
        rig.control.send(say(Some(100), 3, None)).unwrap();
        settled(&rig, |ops| sent_to(ops, 100).len() == 2).await;
        tokio::time::sleep(Duration::from_millis(300)).await;
        assert_eq!(
            sent_to(&rig.fake.ops(), 100),
            [buffer::QUEUED_NOTICE, buffer::UNSUPPORTED_NOTICE]
        );
        // The agent comes: both messages, in order, once; no Resume button.
        rig.agent_of(1, A, Some(10)).await;
        let got = received(&mut rig, 0).await;
        assert_eq!(contents(&got), ["one", "two"]);
        // An agent whose link queue is gone: the message waits, no notice.
        rig._to_agent.clear();
        rig.control.send(say(Some(100), 4, Some("four"))).unwrap();
        tokio::time::sleep(Duration::from_millis(300)).await;
        assert_eq!(sent_to(&rig.fake.ops(), 100).len(), 2);
    }

    fn contents(got: &[HubMsg]) -> Vec<&str> {
        got.iter()
            .filter_map(|msg| match msg {
                HubMsg::Inbound { content, .. } => Some(content.as_str()),
                _ => None,
            })
            .collect()
    }

    #[tokio::test]
    async fn an_ended_session_gets_no_inbound_even_with_its_agent_still_linked() {
        let mut rig = rig(Fake::default(), message_options());
        two_live_slots(&mut rig, false).await;
        rig.hook(hook(
            A,
            HookEvent::SessionEnd {
                reason: None,
                claude_pid: Some(10),
            },
        ))
        .await;
        settled(&rig, |ops| last_icon(ops, 100) == Some(ICON_DEAD)).await;
        rig.control.send(say(Some(100), 4, Some("four"))).unwrap();
        let resume = buffer::resume_text(A);
        settled(&rig, |ops| sent_to(ops, 100) == [resume.as_str()]).await;
        assert!(received(&mut rig, 0).await.is_empty());
    }

    #[tokio::test]
    async fn after_clear_the_new_session_of_the_slot_gets_the_message() {
        let mut rig = rig(Fake::default(), message_options());
        two_live_slots(&mut rig, false).await;
        rig.hook(hook(
            A,
            HookEvent::SessionEnd {
                reason: Some("clear".into()),
                claude_pid: Some(10),
            },
        ))
        .await;
        rig.hook(hook(
            B,
            HookEvent::SessionStart {
                source: Some("clear".into()),
                claude_pid: Some(10),
                parent_claude_pid: None,
            },
        ))
        .await;
        settled(&rig, |ops| {
            sent_to(ops, 100).contains(&"── session bbbbbbbb · new ──")
        })
        .await;
        rig.control
            .send(say(Some(100), 7, Some("after clear")))
            .unwrap();
        let got = received(&mut rig, 0).await;
        assert!(
            matches!(got.as_slice(), [HubMsg::Inbound { content, .. }] if content == "after clear"),
            "{got:?}"
        );
        assert_eq!(count(&rig.fake.ops(), is_create), 1);
    }

    fn reply(conn: u64, text: &str) -> AgentEvent {
        AgentEvent::Message {
            conn,
            received_at: StdInstant::now(),
            msg: AgentMsg::Reply { text: text.into() },
        }
    }

    #[tokio::test]
    async fn a_reply_goes_to_its_session_topic_in_split_order() {
        let mut rig = rig(Fake::default(), message_options());
        two_live_slots(&mut rig, true).await;
        let paragraph = |c: char| format!("{}\n\n", c.to_string().repeat(3000));
        let long: String = ['a', 'b', 'c'].into_iter().map(paragraph).collect();
        let want = split_for_telegram(&long, SplitOptions::default());
        assert!(want.chunks.len() > 1 && !want.prefer_file);
        rig.agents.send(reply(2, &long)).await.unwrap();
        rig.agents.send(reply(2, "short")).await.unwrap();
        let expected: Vec<&str> = want
            .chunks
            .iter()
            .map(String::as_str)
            .chain(["short"])
            .collect();
        let ops = settled(&rig, |ops| sent_to(ops, 101).len() == expected.len()).await;
        assert_eq!(sent_to(&ops, 101), expected);
        assert!(sent_to(&ops, 100).is_empty());
        assert!(loud(&ops).is_empty(), "replies go without a sound");

        // More chunks than `max_chunks`: one document with the whole text.
        let huge = "x".repeat(5 * 4096);
        rig.agents.send(reply(1, &huge)).await.unwrap();
        let ops = settled(&rig, |ops| {
            ops.iter().any(|op| matches!(op, Op::SendDocument { .. }))
        })
        .await;
        assert!(ops.iter().any(|op| matches!(op,
            Op::SendDocument { thread_id: Some(100), document, notify: false } if document.bytes == huge.as_bytes())));
    }

    #[tokio::test]
    async fn a_reply_from_an_agent_without_a_slot_is_dropped() {
        let mut rig = rig(Fake::default(), message_options());
        two_live_slots(&mut rig, false).await;
        rig.agent(5, B).await; // no SessionStart for B: waits, unbound
        let before = rig.fake.ops().len();
        rig.agents.send(reply(5, "lost")).await.unwrap();
        rig.agents.send(reply(99, "unknown conn")).await.unwrap();
        tokio::time::sleep(Duration::from_millis(300)).await;
        assert_eq!(rig.fake.ops().len(), before, "{:?}", rig.fake.ops());
    }

    #[tokio::test]
    async fn a_late_reply_cannot_cross_into_a_reused_slot() {
        let dir = TempDir::new("slots-late-reply");
        let (fake, mut slots) = stalled_slots_with_fake(&dir, message_options());
        slots.on_hook(&start(A, 10));
        slots.registry.topic_created(SlotId(0), 100, "a", None);
        connect(&mut slots, 1, A, Some(10));
        slots.on_hook(&hook(
            A,
            HookEvent::SessionEnd {
                reason: None,
                claude_pid: Some(10),
            },
        ));
        slots.on_hook(&start(B, 11));
        connect(&mut slots, 2, B, Some(11));

        slots.on_reply(1, "late A");
        tokio::time::sleep(Duration::from_millis(50)).await;
        assert!(fake.ops().is_empty(), "{:?}", fake.ops());
        slots.on_reply(2, "current B");
        let ops = wait_for_ops(&fake, 1).await;
        assert!(
            matches!(&ops[0], Op::Send { thread_id: Some(100), text, .. } if text == "current B")
        );
    }

    #[tokio::test]
    async fn a_stale_duplicate_connection_cannot_reply() {
        let dir = TempDir::new("slots-duplicate-reply");
        let (fake, mut slots) = stalled_slots_with_fake(&dir, message_options());
        slots.on_hook(&start(A, 10));
        slots.registry.topic_created(SlotId(0), 100, "a", None);
        connect(&mut slots, 1, A, Some(10));
        connect(&mut slots, 2, A, Some(10));

        slots.on_reply(1, "stale connection");
        tokio::time::sleep(Duration::from_millis(50)).await;
        assert!(fake.ops().is_empty(), "{:?}", fake.ops());
        slots.on_reply(2, "current connection");
        let ops = wait_for_ops(&fake, 1).await;
        assert!(
            matches!(&ops[0], Op::Send { thread_id: Some(100), text, .. } if text == "current connection")
        );
    }

    #[tokio::test]
    async fn a_short_lived_second_agent_leaves_the_session_bound_to_the_first() {
        let mut rig = rig(Fake::default(), message_options());
        two_live_slots(&mut rig, false).await;
        // A second link of the same session registers and closes at once
        // (TASK-042: a `cctg agent` of a test run with the session's env).
        rig.agent_of(2, A, Some(10)).await;
        rig.agents
            .send(AgentEvent::Disconnected { conn: 2 })
            .await
            .unwrap();
        tokio::time::sleep(Duration::from_millis(200)).await;
        let ops = rig.fake.ops();
        assert!(
            ops.iter().all(|op| icon_edit(op) != Some(ICON_NO_CHANNEL)),
            "{ops:?}"
        );
        assert_eq!(last_icon(&ops, 100), Some(ICON_ALIVE), "{ops:?}");

        rig.control
            .send(say(Some(100), 42, Some("still here")))
            .unwrap();
        let got = received(&mut rig, 0).await;
        assert!(
            matches!(got.as_slice(), [HubMsg::Inbound { content, .. }] if content == "still here"),
            "{got:?}"
        );
        let before = rig.fake.ops().len();
        rig.agents.send(reply(1, "from the first")).await.unwrap();
        let ops = rig.ops_after(before + 1).await;
        assert!(sent_to(&ops, 100).contains(&"from the first"), "{ops:?}");
    }

    #[tokio::test]
    async fn a_link_of_another_claude_process_does_not_inherit_the_session() {
        let mut rig = rig(Fake::default(), message_options());
        rig.hook(start(A, 10)).await;
        rig.ops_after(1).await;
        // A foreign process claims A's id, then A's own agent binds and drops.
        rig.agent_of(1, A, Some(99)).await;
        rig.agent_of(2, A, Some(10)).await;
        settled(&rig, |ops| last_icon(ops, 100) == Some(ICON_ALIVE)).await;
        rig.agents
            .send(AgentEvent::Disconnected { conn: 2 })
            .await
            .unwrap();
        settled(&rig, |ops| last_icon(ops, 100) == Some(ICON_NO_CHANNEL)).await;
    }

    #[tokio::test]
    async fn an_unrebound_connection_cannot_reply_after_clear() {
        let dir = TempDir::new("slots-clear-reply");
        let (fake, mut slots) = stalled_slots_with_fake(&dir, message_options());
        slots.on_hook(&start(A, 10));
        slots.registry.topic_created(SlotId(0), 100, "a", None);
        connect(&mut slots, 1, A, None);
        slots.on_hook(&hook(
            A,
            HookEvent::SessionEnd {
                reason: Some("clear".into()),
                claude_pid: None,
            },
        ));
        slots.on_hook(&hook(
            B,
            HookEvent::SessionStart {
                source: Some("clear".into()),
                claude_pid: None,
                parent_claude_pid: None,
            },
        ));

        slots.on_reply(1, "stale after clear");
        tokio::time::sleep(Duration::from_millis(50)).await;
        assert!(fake.ops().is_empty(), "{:?}", fake.ops());
    }

    fn stop(session: &str, answer: Option<&str>) -> HookPost {
        hook(
            session,
            HookEvent::Stop {
                prompt_id: None,
                last_assistant_message: answer.map(str::to_owned),
            },
        )
    }

    /// Text and HTML of the new messages in `thread`, in order.
    fn topic_html(ops: &[Op], thread: i64) -> Vec<(String, Option<String>)> {
        ops.iter()
            .filter_map(|op| match op {
                Op::Send {
                    thread_id: Some(t),
                    text,
                    html,
                    ..
                }
                | Op::Stream {
                    thread_id: t,
                    text,
                    html,
                    ..
                } if *t == thread => Some((text.clone(), html.clone())),
                _ => None,
            })
            .collect()
    }

    /// TASK-027: turn answers and replies go as Telegram HTML with their
    /// markdown source as the plain fallback.
    #[tokio::test]
    async fn turn_answers_and_replies_go_as_html_with_their_source() {
        let mut rig = rig(Fake::default(), message_options());
        two_live_slots(&mut rig, true).await;
        rig.hook(stop(B, Some("# Done\n**all** <ok>"))).await;
        let ops = settled(&rig, |ops| topic_html(ops, 101).len() == 1).await;
        assert_eq!(
            topic_html(&ops, 101),
            [(
                "# Done\n**all** <ok>".to_owned(),
                Some("<b>Done</b>\n<b>all</b> &lt;ok&gt;".to_owned())
            )]
        );
    }

    #[tokio::test]
    async fn a_turn_answer_goes_to_its_session_topic_in_split_order() {
        let mut rig = rig(Fake::default(), message_options());
        two_live_slots(&mut rig, true).await;
        let paragraph = |c: char| format!("{}\n\n", c.to_string().repeat(3000));
        let long: String = ['a', 'b', 'c'].into_iter().map(paragraph).collect();
        let want = split_for_telegram(&long, SplitOptions::default());
        assert!(want.chunks.len() > 1 && !want.prefer_file);
        rig.hook(stop(B, Some(&long))).await;
        rig.hook(stop(B, Some("short"))).await;
        let expected: Vec<&str> = want
            .chunks
            .iter()
            .map(String::as_str)
            .chain(["short"])
            .collect();
        let ops = settled(&rig, |ops| sent_to(ops, 101).len() == expected.len()).await;
        assert_eq!(sent_to(&ops, 101), expected);
        assert!(sent_to(&ops, 100).is_empty());
        assert_eq!(loud(&ops), expected, "only turn answers have a sound");

        // More chunks than `max_chunks`: one document with the whole text.
        let huge = "x".repeat(5 * 4096);
        rig.hook(stop(A, Some(&huge))).await;
        let ops = settled(&rig, |ops| {
            ops.iter().any(|op| matches!(op, Op::SendDocument { .. }))
        })
        .await;
        assert!(ops.iter().any(|op| matches!(op,
            Op::SendDocument { thread_id: Some(100), document, notify: true }
                if document.bytes == huge.as_bytes() && document.file_name == "answer-aaaaaaaa.txt")));
    }

    #[tokio::test]
    async fn only_a_live_top_level_current_session_with_a_topic_sends_its_answer() {
        let dir = TempDir::new("slots-turn-answer");
        let (fake, mut slots) = stalled_slots_with_fake(&dir, message_options());
        slots.on_hook(&start(A, 10));
        // No topic yet.
        slots.on_hook(&stop(A, Some("before the topic")));
        assert_eq!(slots.queued_messages, 0);
        slots.registry.topic_created(SlotId(0), 100, "a", None);
        // Absent, empty and blank answers.
        for answer in [None, Some(""), Some(" \n\t ")] {
            slots.on_hook(&stop(A, answer));
        }
        assert_eq!(slots.queued_messages, 0);
        // A nested run inside A and a session no SessionStart announced.
        slots.on_hook(&hook(
            B,
            HookEvent::SessionStart {
                source: Some("startup".into()),
                claude_pid: Some(20),
                parent_claude_pid: Some(10),
            },
        ));
        slots.on_hook(&stop(B, Some("nested answer")));
        slots.on_hook(&stop(
            "cccccccc-0000-4000-8000-000000000003",
            Some("unknown answer"),
        ));
        assert_eq!(slots.queued_messages, 0);
        // The live session, with no agent linked: the hook alone is enough.
        slots.on_hook(&stop(A, Some("current answer")));
        assert_eq!(slots.queued_messages, 1);
        // An ended session, also after its slot went to the next session.
        slots.on_hook(&hook(
            A,
            HookEvent::SessionEnd {
                reason: None,
                claude_pid: Some(10),
            },
        ));
        slots.on_hook(&stop(A, Some("after the end")));
        slots.on_hook(&start("dddddddd-0000-4000-8000-000000000004", 11));
        slots.on_hook(&stop(A, Some("after the slot moved on")));
        assert_eq!(slots.queued_messages, 1);

        let ops = wait_for_ops(&fake, 1).await;
        tokio::time::sleep(Duration::from_millis(50)).await;
        assert_eq!(fake.ops().len(), 1, "{:?}", fake.ops());
        assert!(
            matches!(&ops[0], Op::Send { thread_id: Some(100), text, .. } if text == "current answer")
        );
    }

    #[tokio::test]
    async fn turn_answers_and_replies_share_the_message_cap() {
        let dir = TempDir::new("slots-answer-cap");
        let mut slots = stalled_slots(&dir, message_options());
        slots.on_hook(&start(A, 10));
        slots.registry.topic_created(SlotId(0), 100, "a", None);
        connect(&mut slots, 1, A, Some(10));
        slots.queued_messages = MAX_QUEUED_MESSAGES - 1;
        let text = format!("{}\n\n{}", "a".repeat(3000), "b".repeat(3000));
        assert!(
            split_for_telegram(&text, SplitOptions::default())
                .chunks
                .len()
                > 1
        );

        // Two chunks do not fit: the answer is dropped whole.
        slots.on_hook(&stop(A, Some(&text)));
        assert_eq!(slots.queued_messages, MAX_QUEUED_MESSAGES - 1);
        assert!(slots.overflow_warned);
        // One chunk fits and fills the cap; a reply then finds no room.
        slots.on_hook(&stop(A, Some("one")));
        assert_eq!(slots.queued_messages, MAX_QUEUED_MESSAGES);
        slots.on_reply(1, "reply");
        assert_eq!(slots.queued_messages, MAX_QUEUED_MESSAGES);
    }

    #[tokio::test]
    async fn a_stalled_telegram_never_stalls_turn_answers() {
        let fake = Fake {
            stall_sends: true,
            ..Fake::default()
        };
        let mut rig = rig(fake, message_options());
        two_live_slots(&mut rig, false).await;
        // Far more answers than the scheduler queue (1024) and the cap, each
        // through the bounded hook channel: a stalled actor would block here.
        let flood = async {
            for _ in 0..1500 {
                rig.hook(stop(A, Some("stuck"))).await;
            }
        };
        tokio::time::timeout(WAIT, flood)
            .await
            .expect("hook events kept draining");
        rig.control
            .send(say(Some(100), 5000, Some("still here")))
            .unwrap();
        let arrived = async {
            loop {
                if let Some(HubMsg::Inbound { content, .. }) = rig._to_agent[0].recv().await
                    && content == "still here"
                {
                    return;
                }
            }
        };
        tokio::time::timeout(WAIT, arrived)
            .await
            .expect("inbound reached the agent while Telegram stalled");
    }

    fn permission(conn: u64, request_id: &str, preview: &str) -> AgentEvent {
        AgentEvent::Message {
            conn,
            received_at: StdInstant::now(),
            msg: AgentMsg::PermissionRequest(PermissionRequest {
                request_id: request_id.into(),
                tool_name: "Bash".into(),
                description: "run the tests".into(),
                input_preview: preview.into(),
            }),
        }
    }

    fn press(query: &str, message_id: Option<i64>, data: &str) -> Control {
        Control::Callback(CallbackInput {
            query_id: query.into(),
            data: Some(data.into()),
            message_id,
            thread_id: None,
            from_name: None,
        })
    }

    /// Permission prompts sent so far: (thread, text, message id Telegram gave).
    fn prompts(ops: &[Op]) -> Vec<(i64, String, i64)> {
        let mut message_id = 1000;
        let mut found = Vec::new();
        for op in ops {
            if let Op::Send {
                thread_id: Some(thread),
                text,
                permission,
                ..
            } = op
            {
                if *permission {
                    found.push((*thread, text.clone(), message_id));
                }
                message_id += 1;
            }
        }
        found
    }

    fn answers(ops: &[Op]) -> Vec<Option<&str>> {
        ops.iter()
            .filter_map(|op| match op {
                Op::AnswerCallback { text, .. } => Some(text.as_deref()),
                _ => None,
            })
            .collect()
    }

    fn verdicts(got: &[HubMsg]) -> Vec<(String, Behavior)> {
        got.iter()
            .filter_map(|msg| match msg {
                HubMsg::PermissionVerdict {
                    request_id,
                    behavior,
                    ..
                } => Some((request_id.clone(), *behavior)),
                _ => None,
            })
            .collect()
    }

    /// A `PermissionRequest` hook of `session` for `tool`; the receiver gets
    /// its answer (an error: dropped, no decision).
    async fn hook_ask(rig: &Rig, session: &str, tool: &str) -> oneshot::Receiver<Option<Behavior>> {
        let (answer, answered) = oneshot::channel();
        let post = PermissionPost {
            v: crate::wire::VERSION,
            host: "box".into(),
            session_id: session.into(),
            tool_name: tool.into(),
            description: "remove the build".into(),
            input_preview: "{\"command\":\"rm -rf $X\"}".into(),
        };
        rig.asks.send(PermissionAsk { post, answer }).await.unwrap();
        answered
    }

    async fn hook_answer(answered: oneshot::Receiver<Option<Behavior>>) -> Option<Behavior> {
        tokio::time::timeout(WAIT, answered)
            .await
            .expect("hook answered in time")
            .ok()
            .flatten()
    }

    fn question_post(session: &str) -> QuestionPost {
        let option = |label: &str| AskedOption {
            label: label.into(),
            description: String::new(),
        };
        QuestionPost {
            v: crate::wire::VERSION,
            host: "box".into(),
            session_id: session.into(),
            questions: vec![
                AskedQuestion {
                    question: "Which color?".into(),
                    header: "Color".into(),
                    multi_select: false,
                    options: vec![option("Red"), option("Blue")],
                },
                AskedQuestion {
                    question: "Which fruits?".into(),
                    header: "Fruits".into(),
                    multi_select: true,
                    options: vec![option("Apple"), option("Pear"), option("Plum")],
                },
            ],
        }
    }

    async fn question_ask(rig: &Rig, session: &str) -> oneshot::Receiver<Option<Vec<Answered>>> {
        let (answer, answered) = oneshot::channel();
        let post = question_post(session);
        rig.questions
            .send(QuestionAsk { post, answer })
            .await
            .unwrap();
        answered
    }

    async fn question_answer(
        answered: oneshot::Receiver<Option<Vec<Answered>>>,
    ) -> Option<Vec<Answered>> {
        tokio::time::timeout(WAIT, answered)
            .await
            .expect("question hook answered in time")
            .ok()
            .flatten()
    }

    /// The ask id on the buttons of the question send `index`.
    fn question_id(ops: &[Op], index: usize) -> String {
        let markup = ops
            .iter()
            .filter_map(|op| match op {
                Op::Send {
                    permission: true,
                    reply_markup: Some(markup),
                    ..
                } if markup.to_string().contains("\"ask:") => Some(markup),
                _ => None,
            })
            .nth(index)
            .expect("question send");
        let data = markup["inline_keyboard"][0][0]["callback_data"]
            .as_str()
            .unwrap();
        questions::parse_callback(data).unwrap().0.to_owned()
    }

    /// The last edit of `message` starts with `title`.
    fn edited_to(ops: &[Op], message: i64, title: &str) -> bool {
        edits_of(ops, message)
            .last()
            .is_some_and(|(text, _)| text.starts_with(title))
    }

    fn inbound_contents(got: &[HubMsg]) -> Vec<String> {
        got.iter()
            .filter_map(|msg| match msg {
                HubMsg::Inbound { content, .. } => Some(content.clone()),
                _ => None,
            })
            .collect()
    }

    #[tokio::test]
    async fn a_question_is_answered_with_buttons_and_own_text() {
        let mut rig = rig(Fake::default(), message_options());
        two_live_slots(&mut rig, false).await;
        let answered = question_ask(&rig, A).await;
        let ops = settled(&rig, |ops| prompts(ops).len() == 1).await;
        let (thread, text, message_id) = prompts(&ops).remove(0);
        assert_eq!(thread, 100);
        assert!(
            text.contains("(1 из 2)") && text.contains("Which color?"),
            "{text}"
        );
        settled(&rig, |ops| {
            last_icon(ops, 100) == Some(crate::hub::registry::ICON_WAITING)
        })
        .await;
        let id = question_id(&ops, 0);
        let data = |question, press| questions::callback_data(&id, question, press);
        for (query, question, press) in [
            ("q1", 0, questions::Press::Option(1)),
            ("q2", 1, questions::Press::Option(2)),
            ("q3", 1, questions::Press::Other),
        ] {
            rig.control
                .send(press_of(query, message_id, &data(question, press)))
                .unwrap();
        }
        rig.control
            .send(say(Some(100), 50, Some("and a fig")))
            .unwrap();
        assert_eq!(
            question_answer(answered).await,
            Some(vec![
                Answered {
                    options: vec![1],
                    text: None
                },
                Answered {
                    options: vec![2],
                    text: Some("and a fig".into())
                }
            ])
        );
        let ops = settled(&rig, |ops| {
            edited_to(ops, message_id, questions::ANSWERED_TITLE)
        })
        .await;
        let (_, keyboard) = edits_of(&ops, message_id).pop().unwrap();
        assert_eq!(keyboard, Some(permissions::no_keyboard()));
        assert_eq!(
            answers(&ops),
            [
                Some(questions::ANSWER_NEXT),
                None,
                Some(questions::ANSWER_TYPE)
            ]
        );
        // The answer never reached the session.
        assert!(inbound_contents(&received(&mut rig, 0).await).is_empty());
        settled(&rig, |ops| last_icon(ops, 100) == Some(ICON_ALIVE)).await;
        rig.control
            .send(press_of("q4", message_id, &data(1, questions::Press::Done)))
            .unwrap();
        let ops = settled(&rig, |ops| answers(ops).len() == 4).await;
        assert_eq!(answers(&ops)[3], Some(questions::ANSWER_STALE));
    }

    fn press_of(query: &str, message_id: i64, data: &str) -> Control {
        press(query, Some(message_id), data)
    }

    #[tokio::test]
    async fn a_reply_answers_and_the_terminal_button_lets_the_hook_go() {
        let mut rig = rig(Fake::default(), message_options());
        two_live_slots(&mut rig, false).await;
        let answered = question_ask(&rig, A).await;
        let ops = settled(&rig, |ops| prompts(ops).len() == 1).await;
        let (_, _, message_id) = prompts(&ops).remove(0);
        let id = question_id(&ops, 0);
        rig.control
            .send(Control::Message(Inbound {
                message_id: 51,
                thread_id: Some(100),
                text: Some("Green".into()),
                reply_to: Some(message_id),
                quote: Some("Which color?".into()),
                forwarded: false,
                media: None,
                from_name: None,
            }))
            .unwrap();
        settled(&rig, |ops| {
            edits_of(ops, message_id)
                .iter()
                .any(|(text, _)| text.contains("(2 из 2)") && text.contains("Color: Green"))
        })
        .await;
        rig.control
            .send(press_of(
                "q1",
                message_id,
                &questions::callback_data(&id, 1, questions::Press::Terminal),
            ))
            .unwrap();
        assert_eq!(question_answer(answered).await, None);
        let ops = settled(&rig, |ops| {
            edited_to(ops, message_id, questions::TERMINAL_TITLE)
        })
        .await;
        assert_eq!(answers(&ops), [Some(questions::ANSWER_TERMINAL)]);
        // Once the question is closed, a text goes to the session again.
        rig.control.send(say(Some(100), 52, Some("hello"))).unwrap();
        let texts = inbound_contents(&received(&mut rig, 0).await);
        assert_eq!(texts.len(), 1, "{texts:?}");
        assert!(texts[0].contains("hello") && !texts[0].contains("Green"));
    }

    #[tokio::test]
    async fn an_unanswered_question_goes_to_the_terminal() {
        let options = Options {
            question_wait: Duration::from_millis(300),
            ..message_options()
        };
        let mut rig = rig(Fake::default(), options);
        two_live_slots(&mut rig, false).await;
        let answered = question_ask(&rig, A).await;
        let ops = settled(&rig, |ops| prompts(ops).len() == 1).await;
        let (_, _, message_id) = prompts(&ops).remove(0);
        assert_eq!(question_answer(answered).await, None);
        settled(&rig, |ops| {
            edited_to(ops, message_id, questions::EXPIRED_TITLE)
        })
        .await;
    }

    #[tokio::test]
    async fn a_question_closes_when_its_hook_leaves_or_its_session_ends() {
        let mut rig = rig(Fake::default(), message_options());
        two_live_slots(&mut rig, false).await;
        let answered = question_ask(&rig, A).await;
        let ops = settled(&rig, |ops| prompts(ops).len() == 1).await;
        let (_, _, first) = prompts(&ops).remove(0);
        drop(answered);
        settled(&rig, |ops| edited_to(ops, first, questions::GONE_TITLE)).await;
        let answered = question_ask(&rig, A).await;
        let ops = settled(&rig, |ops| prompts(ops).len() == 2).await;
        let (_, _, second) = prompts(&ops).remove(1);
        rig.hook(hook(
            A,
            HookEvent::SessionEnd {
                reason: Some("other".into()),
                claude_pid: Some(10),
            },
        ))
        .await;
        assert_eq!(question_answer(answered).await, None);
        settled(&rig, |ops| edited_to(ops, second, questions::CLOSED_TITLE)).await;
        // A session that is not live gets no question at all.
        let answered = question_ask(&rig, A).await;
        assert_eq!(question_answer(answered).await, None);
    }

    #[tokio::test]
    async fn permission_requests_of_a_question_get_no_buttons() {
        let mut rig = rig(Fake::default(), message_options());
        two_live_slots(&mut rig, false).await;
        rig.agents
            .send(AgentEvent::Message {
                conn: 1,
                received_at: std::time::Instant::now(),
                msg: AgentMsg::PermissionRequest(PermissionRequest {
                    request_id: "abcde".into(),
                    tool_name: QUESTION_TOOL.into(),
                    description: "d".into(),
                    input_preview: "p".into(),
                }),
            })
            .await
            .unwrap();
        let asked = std::time::Instant::now();
        let answered = hook_ask(&rig, A, QUESTION_TOOL).await;
        assert_eq!(hook_answer(answered).await, None);
        assert!(asked.elapsed() < TWIN_WINDOW, "{:?}", asked.elapsed());
        let answered = hook_ask(&rig, A, "Bash").await;
        let ops = settled(&rig, |ops| prompts(ops).len() == 1).await;
        assert!(
            prompts(&ops)[0].1.starts_with("Запрос разрешения: Bash"),
            "{:?}",
            prompts(&ops)
        );
        drop(answered);
    }

    /// Sends of the "no question hook" notice: (topic, has buttons).
    fn hook_hints(ops: &[Op]) -> Vec<(i64, bool)> {
        ops.iter()
            .filter_map(|op| match op {
                Op::Send {
                    thread_id: Some(thread),
                    text,
                    reply_markup,
                    ..
                } if text == questions::NO_HOOK_NOTICE => Some((*thread, reply_markup.is_some())),
                _ => None,
            })
            .collect()
    }

    /// A client without the question hook (settings from before TASK-038)
    /// asks only through the `PermissionRequest` hook: no decision at once
    /// and one plain notice per session. A session whose question hook
    /// asked just before (its "no decision" going on) gets no notice.
    #[tokio::test]
    async fn a_question_without_its_hook_is_told_once_per_session() {
        let mut rig = rig(Fake::default(), message_options());
        two_live_slots(&mut rig, true).await;
        for _ in 0..2 {
            let asked = std::time::Instant::now();
            assert_eq!(
                hook_answer(hook_ask(&rig, A, QUESTION_TOOL).await).await,
                None
            );
            assert!(asked.elapsed() < TWIN_WINDOW, "{:?}", asked.elapsed());
        }
        let ops = settled(&rig, |ops| !hook_hints(ops).is_empty()).await;
        assert_eq!(hook_hints(&ops), [(100, false)]);
        // B has the question hook: its question went to the terminal first.
        let answered = question_ask(&rig, B).await;
        let ops = settled(&rig, |ops| prompts(ops).len() == 1).await;
        let (thread, _, message_id) = prompts(&ops).remove(0);
        assert_eq!(thread, 101);
        let id = question_id(&ops, 0);
        rig.control
            .send(press_of(
                "q1",
                message_id,
                &questions::callback_data(&id, 0, questions::Press::Terminal),
            ))
            .unwrap();
        assert_eq!(question_answer(answered).await, None);
        assert_eq!(
            hook_answer(hook_ask(&rig, B, QUESTION_TOOL).await).await,
            None
        );
        tokio::time::sleep(Duration::from_millis(300)).await;
        assert_eq!(hook_hints(&rig.fake.ops()), [(100, false)]);
    }

    /// A live session whose slot has no topic (its creation hangs) gets no
    /// decision at once, not a silent five-minute wait.
    #[tokio::test]
    async fn a_question_of_a_session_without_a_topic_goes_to_the_terminal_at_once() {
        let fake = Fake {
            stall: true,
            ..Fake::default()
        };
        let rig = rig(fake, message_options());
        rig.hook(start(A, 10)).await;
        let asked = std::time::Instant::now();
        assert_eq!(question_answer(question_ask(&rig, A).await).await, None);
        assert!(
            asked.elapsed() < Duration::from_secs(5),
            "{:?}",
            asked.elapsed()
        );
    }

    /// Two sessions ask at once in their own topics: presses and texts of
    /// one topic never touch the other's question.
    #[tokio::test]
    async fn questions_of_two_sessions_never_mix() {
        let mut rig = rig(Fake::default(), message_options());
        two_live_slots(&mut rig, true).await;
        let answered_a = question_ask(&rig, A).await;
        settled(&rig, |ops| prompts(ops).len() == 1).await;
        let answered_b = question_ask(&rig, B).await;
        let ops = settled(&rig, |ops| prompts(ops).len() == 2).await;
        let found = prompts(&ops);
        let ((thread_a, _, message_a), (thread_b, _, message_b)) =
            (found[0].clone(), found[1].clone());
        assert_eq!((thread_a, thread_b), (100, 101));
        let (id_a, id_b) = (question_id(&ops, 0), question_id(&ops, 1));
        let press_in = |query: &str, message: i64, id: &str, question, press| {
            press_of(
                query,
                message,
                &questions::callback_data(id, question, press),
            )
        };
        // ✏️ Другое in A; a text in B's topic goes to B's session.
        rig.control
            .send(press_in("q1", message_a, &id_a, 0, questions::Press::Other))
            .unwrap();
        rig.control
            .send(say(Some(101), 60, Some("for session B")))
            .unwrap();
        let texts = inbound_contents(&received(&mut rig, 1).await);
        assert!(
            texts.len() == 1 && texts[0].contains("for session B"),
            "{texts:?}"
        );
        // A's own text; B's button pressed on A's message changes nothing.
        rig.control.send(say(Some(100), 61, Some("Teal"))).unwrap();
        rig.control
            .send(press_in(
                "q2",
                message_a,
                &id_b,
                1,
                questions::Press::Terminal,
            ))
            .unwrap();
        for (query, press) in [
            ("q3", questions::Press::Option(0)),
            ("q4", questions::Press::Done),
        ] {
            rig.control
                .send(press_in(query, message_a, &id_a, 1, press))
                .unwrap();
        }
        assert_eq!(
            question_answer(answered_a).await,
            Some(vec![
                Answered {
                    options: vec![],
                    text: Some("Teal".into())
                },
                Answered {
                    options: vec![0],
                    text: None
                }
            ])
        );
        rig.control
            .send(press_in(
                "q5",
                message_b,
                &id_b,
                0,
                questions::Press::Terminal,
            ))
            .unwrap();
        assert_eq!(question_answer(answered_b).await, None);
        settled(&rig, |ops| {
            edited_to(ops, message_a, questions::ANSWERED_TITLE)
                && edited_to(ops, message_b, questions::TERMINAL_TITLE)
        })
        .await;
        assert!(inbound_contents(&received(&mut rig, 0).await).is_empty());
    }

    /// The request id on the buttons of the permission send `index`.
    fn prompt_id(ops: &[Op], index: usize) -> String {
        let markup = ops
            .iter()
            .filter_map(|op| match op {
                Op::Send {
                    permission: true,
                    reply_markup: Some(markup),
                    ..
                } => Some(markup),
                _ => None,
            })
            .nth(index)
            .expect("permission send");
        let data = markup["inline_keyboard"][0][0]["callback_data"]
            .as_str()
            .unwrap();
        data.strip_prefix("allow:").unwrap().to_owned()
    }

    #[tokio::test]
    async fn a_hook_without_a_channel_twin_gets_buttons_and_the_first_press() {
        let mut rig = rig(Fake::default(), message_options());
        two_live_slots(&mut rig, false).await;
        let asked = std::time::Instant::now();
        let answered = hook_ask(&rig, A, "Bash").await;
        let ops = settled(&rig, |ops| prompts(ops).len() == 1).await;
        assert!(asked.elapsed() >= TWIN_WINDOW, "{:?}", asked.elapsed());
        let (thread, text, message_id) = prompts(&ops).remove(0);
        assert_eq!(thread, 100);
        assert!(
            text.starts_with("Запрос разрешения: Bash\nremove the build"),
            "{text}"
        );
        settled(&rig, |ops| {
            last_icon(ops, 100) == Some(crate::hub::registry::ICON_WAITING)
        })
        .await;
        let id = prompt_id(&ops, 0);
        rig.control
            .send(press("q1", Some(message_id), &format!("deny:{id}")))
            .unwrap();
        assert_eq!(hook_answer(answered).await, Some(Behavior::Deny));
        rig.control
            .send(press("q2", Some(message_id), &format!("allow:{id}")))
            .unwrap();
        let ops = settled(&rig, |ops| {
            answers(ops).len() == 2 && edits_of(ops, message_id).len() == 1
        })
        .await;
        assert_eq!(
            answers(&ops),
            [
                Some(permissions::ANSWER_DENIED),
                Some(permissions::ANSWER_DECIDED)
            ]
        );
        let edits = edits_of(&ops, message_id);
        assert_eq!(edits.len(), 1, "{edits:?}");
        assert!(edits[0].0.ends_with(permissions::DENIED_MARK));
        assert_eq!(edits[0].1, Some(permissions::no_keyboard()));
        // No agent got anything: the answer went to the hook only.
        assert!(verdicts(&received(&mut rig, 0).await).is_empty());
    }

    #[tokio::test]
    async fn the_decision_is_signed_by_the_team_member_whose_press_fixed_it() {
        let mut rig = rig(Fake::default(), message_options());
        two_live_slots(&mut rig, false).await;
        let answered = hook_ask(&rig, A, "Bash").await;
        let ops = settled(&rig, |ops| prompts(ops).len() == 1).await;
        let message_id = prompts(&ops)[0].2;
        let id = prompt_id(&ops, 0);
        let named =
            |query: &str, data: String, name: &str| match press(query, Some(message_id), &data) {
                Control::Callback(input) => Control::Callback(CallbackInput {
                    from_name: Some(name.into()),
                    ..input
                }),
                other => other,
            };
        rig.control
            .send(named("q1", format!("deny:{id}"), "Анна"))
            .unwrap();
        assert_eq!(hook_answer(answered).await, Some(Behavior::Deny));
        // A later press of someone else changes neither the answer nor the name.
        rig.control
            .send(named("q2", format!("allow:{id}"), "Иван"))
            .unwrap();
        let ops = settled(&rig, |ops| {
            answers(ops).len() == 2 && edits_of(ops, message_id).len() == 1
        })
        .await;
        let edits = edits_of(&ops, message_id);
        assert!(
            edits[0].0.ends_with(&format!(
                "{}{}Анна",
                permissions::DENIED_MARK,
                permissions::BY
            )),
            "{edits:?}"
        );
    }

    #[tokio::test]
    async fn a_hook_whose_channel_twin_comes_before_or_after_gets_no_decision() {
        let mut rig = rig(Fake::default(), message_options());
        two_live_slots(&mut rig, false).await;
        // The channel first, then the hook.
        rig.agents.send(permission(1, "abcde", "p")).await.unwrap();
        settled(&rig, |ops| prompts(ops).len() == 1).await;
        let started = std::time::Instant::now();
        assert_eq!(hook_answer(hook_ask(&rig, A, "Bash").await).await, None);
        assert!(started.elapsed() < Duration::from_millis(500));
        // The hook first, then the channel.
        let started = std::time::Instant::now();
        let answered = hook_ask(&rig, A, "Bash").await;
        tokio::time::sleep(Duration::from_millis(300)).await;
        rig.agents.send(permission(1, "bcdef", "p")).await.unwrap();
        assert_eq!(hook_answer(answered).await, None);
        assert!(started.elapsed() < TWIN_WINDOW, "{:?}", started.elapsed());
        // Another tool is not a twin: it gets its own prompt.
        let _other_tool = hook_ask(&rig, A, "Write").await;
        let ops = settled(&rig, |ops| prompts(ops).len() == 3).await;
        assert_eq!(
            prompts(&ops).len(),
            3,
            "two channel prompts, one hook prompt"
        );
    }

    #[tokio::test]
    async fn a_hook_prompt_ends_without_a_decision_on_time_out_gone_hook_or_session_end() {
        let options = Options {
            hook_answer_wait: TWIN_WINDOW + Duration::from_millis(800),
            ..message_options()
        };
        let mut rig = rig(Fake::default(), options);
        two_live_slots(&mut rig, false).await;
        // Time runs out: no decision, the buttons go.
        let answered = hook_ask(&rig, A, "Bash").await;
        let ops = settled(&rig, |ops| prompts(ops).len() == 1).await;
        let first = prompts(&ops)[0].2;
        assert_eq!(hook_answer(answered).await, None);
        let ops = settled(&rig, |ops| edits_of(ops, first).len() == 1).await;
        assert_eq!(
            edits_of(&ops, first)[0],
            (
                permissions::ANSWER_EXPIRED.to_owned(),
                Some(permissions::no_keyboard())
            )
        );
        // The hook went away: a press answers "expired" and closes the prompt.
        let answered = hook_ask(&rig, A, "Bash").await;
        let ops = settled(&rig, |ops| prompts(ops).len() == 2).await;
        let second = prompts(&ops)[1].2;
        let id = prompt_id(&ops, 1);
        drop(answered);
        rig.control
            .send(press("q1", Some(second), &format!("allow:{id}")))
            .unwrap();
        let ops = settled(&rig, |ops| edits_of(ops, second).len() == 1).await;
        assert_eq!(answers(&ops), [Some(permissions::ANSWER_EXPIRED)]);
        assert_eq!(edits_of(&ops, second)[0].0, permissions::ANSWER_EXPIRED);
        // The session ends: no decision, the prompt is closed.
        let answered = hook_ask(&rig, A, "Bash").await;
        let ops = settled(&rig, |ops| prompts(ops).len() == 3).await;
        let third = prompts(&ops)[2].2;
        rig.hook(end(A, 10)).await;
        assert_eq!(hook_answer(answered).await, None);
        let ops = settled(&rig, |ops| edits_of(ops, third).len() == 1).await;
        assert_eq!(edits_of(&ops, third)[0].0, permissions::CLOSED_TEXT);
        // An ended session's hook gets no decision at once.
        let started = std::time::Instant::now();
        assert_eq!(hook_answer(hook_ask(&rig, A, "Bash").await).await, None);
        assert!(started.elapsed() < Duration::from_millis(500));
    }

    #[tokio::test]
    async fn a_hook_that_leaves_before_its_prompt_is_shown_gets_none() {
        let mut rig = rig(Fake::default(), message_options());
        two_live_slots(&mut rig, false).await;
        drop(hook_ask(&rig, A, "Bash").await);
        tokio::time::sleep(TWIN_WINDOW + Duration::from_millis(500)).await;
        assert!(prompts(&rig.fake.ops()).is_empty());
    }

    #[tokio::test]
    async fn a_prompt_reaches_its_topic_with_two_bounded_buttons() {
        let mut rig = rig(Fake::default(), message_options());
        two_live_slots(&mut rig, true).await;
        let huge = "😀".repeat(16 * 1024);
        rig.agents
            .send(permission(2, "abcde", &huge))
            .await
            .unwrap();
        let ops = settled(&rig, |ops| prompts(ops).len() == 1).await;
        let sent = ops
            .iter()
            .find(|op| {
                matches!(
                    op,
                    Op::Send {
                        permission: true,
                        ..
                    }
                )
            })
            .unwrap();
        let Op::Send {
            thread_id,
            text,
            reply_markup: Some(markup),
            notify: true,
            ..
        } = sent
        else {
            panic!("{sent:?}");
        };
        assert_eq!(*thread_id, Some(101), "B's topic");
        assert!(transcript::telegram_len(text) <= transcript::TELEGRAM_TEXT_LIMIT);
        let data: Vec<&str> = markup["inline_keyboard"][0]
            .as_array()
            .unwrap()
            .iter()
            .map(|button| button["callback_data"].as_str().unwrap())
            .collect();
        assert_eq!(data, ["allow:abcde", "deny:abcde"]);
        assert!(
            data.iter()
                .all(|d| d.len() <= permissions::MAX_CALLBACK_DATA)
        );
        settled(&rig, |ops| {
            last_icon(ops, 101) == Some(crate::hub::registry::ICON_WAITING)
        })
        .await;
    }

    #[tokio::test]
    async fn the_first_press_sends_one_verdict_and_later_presses_do_not() {
        let mut rig = rig(Fake::default(), message_options());
        two_live_slots(&mut rig, true).await;
        rig.agents.send(permission(2, "abcde", "p")).await.unwrap();
        let ops = settled(&rig, |ops| prompts(ops).len() == 1).await;
        let (_, text, message_id) = prompts(&ops).remove(0);
        // Let Done::Permission record the message id.
        tokio::time::sleep(Duration::from_millis(100)).await;
        rig.control
            .send(press("q1", Some(message_id), "allow:abcde"))
            .unwrap();
        rig.control
            .send(press("q2", Some(message_id), "allow:abcde"))
            .unwrap();
        rig.control
            .send(press("q3", Some(message_id), "deny:abcde"))
            .unwrap();
        let ops = settled(&rig, |ops| answers(ops).len() == 3).await;
        assert_eq!(
            answers(&ops),
            [
                Some(permissions::ANSWER_ALLOWED),
                Some(permissions::ANSWER_DECIDED),
                Some(permissions::ANSWER_DECIDED),
            ]
        );
        let got = received(&mut rig, 1).await;
        assert_eq!(verdicts(&got), [("abcde".to_owned(), Behavior::Allow)]);
        assert!(
            matches!(
                got.as_slice(),
                [HubMsg::PermissionVerdict {
                    verdict_id: None,
                    ..
                }]
            ),
            "an agent without acks gets the v1 verdict: {got:?}"
        );
        assert!(verdicts(&received(&mut rig, 0).await).is_empty());
        let edits: Vec<&Op> = ops
            .iter()
            .filter(|op| matches!(op, Op::Edit { .. }))
            .collect();
        assert_eq!(edits.len(), 1, "{edits:?}");
        assert!(
            matches!(edits[0], Op::Edit { message_id: id, text: edited, reply_markup: Some(markup), background: false }
            if *id == message_id
                && *edited == format!("{text}{}", permissions::ALLOWED_MARK)
                && *markup == permissions::no_keyboard())
        );
        settled(&rig, |ops| last_icon(ops, 101) == Some(ICON_ALIVE)).await;
    }

    #[tokio::test]
    async fn one_request_id_in_two_sessions_is_told_apart_by_its_message() {
        let mut rig = rig(Fake::default(), message_options());
        two_live_slots(&mut rig, true).await;
        rig.agents.send(permission(1, "abcde", "a")).await.unwrap();
        rig.agents.send(permission(2, "abcde", "b")).await.unwrap();
        let ops = settled(&rig, |ops| prompts(ops).len() == 2).await;
        tokio::time::sleep(Duration::from_millis(100)).await;
        let of_b = prompts(&ops)
            .into_iter()
            .find(|(thread, _, _)| *thread == 101)
            .unwrap();
        rig.control
            .send(press("q", Some(of_b.2), "deny:abcde"))
            .unwrap();
        settled(&rig, |ops| answers(ops).len() == 1).await;
        assert_eq!(
            verdicts(&received(&mut rig, 1).await),
            [("abcde".to_owned(), Behavior::Deny)]
        );
        assert!(verdicts(&received(&mut rig, 0).await).is_empty());
    }

    #[tokio::test]
    async fn stale_or_foreign_presses_send_no_verdict() {
        let mut rig = rig(Fake::default(), message_options());
        two_live_slots(&mut rig, false).await;
        rig.agents.send(permission(1, "abcde", "p")).await.unwrap();
        let ops = settled(&rig, |ops| prompts(ops).len() == 1).await;
        let (_, _, message_id) = prompts(&ops).remove(0);
        tokio::time::sleep(Duration::from_millis(100)).await;
        for control in [
            press("q1", Some(message_id + 50), "allow:abcde"), // unknown message
            press("q2", Some(message_id), "allow:bcdef"),      // another id
            press("q3", None, "allow:abcde"),                  // inaccessible message
            press("q4", Some(message_id), "allow:abcdl"),      // not a request id
            press("q5", Some(message_id), "resume:x"),         // Resume of no known session
        ] {
            rig.control.send(control).unwrap();
        }
        let ops = settled(&rig, |ops| answers(ops).len() == 5).await;
        let expired = Some(permissions::ANSWER_EXPIRED);
        assert_eq!(answers(&ops), [expired, expired, expired, None, expired]);
        assert!(verdicts(&received(&mut rig, 0).await).is_empty());
        assert!(!ops.iter().any(|op| matches!(op, Op::Edit { .. })));
    }

    // ---- TASK-014 review 2: lifecycle defects of the planner reference ----

    const CLOSED: &str = "Сессия завершилась";

    #[test]
    fn the_closing_text_is_the_decided_wording() {
        assert_eq!(permissions::CLOSED_TEXT, CLOSED);
    }

    impl Rig {
        /// An agent that acknowledges verdicts (`Register::verdict_ack`).
        async fn agent_acking(&mut self, conn: u64, session: &str, claude_pid: Option<u32>) {
            self.agent_with(conn, session, claude_pid, true).await;
        }
    }

    /// The actor driven by direct calls over a Telegram that answers at once
    /// (answers are not fed back: there is no run loop).
    fn live_slots(dir: &TempDir, options: Options) -> (Arc<Fake>, Slots) {
        let store = RegistryStore::open(dir.path()).unwrap();
        let fake = Arc::new(Fake::default());
        let fast = BucketConfig {
            capacity: 1000,
            refill_every: Duration::from_millis(1),
            min_gap: Duration::ZERO,
        };
        let (scheduler, outbox) = Scheduler::new(fake.clone(), fast);
        tokio::spawn(scheduler.run());
        let slots = Slots::new(Registry::default(), store, outbox, options);
        (fake, slots)
    }

    fn edits_of(ops: &[Op], message: i64) -> Vec<(String, Option<serde_json::Value>)> {
        ops.iter()
            .filter_map(|op| match op {
                Op::Edit {
                    message_id,
                    text,
                    reply_markup,
                    ..
                } if *message_id == message => Some((text.clone(), reply_markup.clone())),
                _ => None,
            })
            .collect()
    }

    /// Five letters without `l`, distinct for every `n` below 25^5.
    fn request_id(mut n: usize) -> String {
        const LETTERS: &[u8] = b"abcdefghijkmnopqrstuvwxyz";
        let mut id = String::new();
        for _ in 0..5 {
            id.push(LETTERS[n % LETTERS.len()] as char);
            n /= LETTERS.len();
        }
        id
    }

    #[tokio::test]
    async fn session_end_closes_its_open_prompt_and_a_late_press_does_nothing() {
        let mut rig = rig(Fake::default(), message_options());
        two_live_slots(&mut rig, true).await;
        rig.agents.send(permission(2, "abcde", "p")).await.unwrap();
        let ops = settled(&rig, |ops| prompts(ops).len() == 1).await;
        let (_, _, message_id) = prompts(&ops).remove(0);
        tokio::time::sleep(Duration::from_millis(100)).await;
        rig.hook(hook(
            B,
            HookEvent::SessionEnd {
                reason: None,
                claude_pid: Some(11),
            },
        ))
        .await;
        settled(&rig, |ops| last_icon(ops, 101) == Some(ICON_DEAD)).await;
        tokio::time::sleep(Duration::from_millis(200)).await;
        rig.control
            .send(press("late", Some(message_id), "allow:abcde"))
            .unwrap();
        let ops = settled(&rig, |ops| answers(ops).len() == 1).await;
        assert!(
            verdicts(&received(&mut rig, 1).await).is_empty(),
            "no verdict after the end"
        );
        assert_eq!(answers(&ops), [Some(permissions::ANSWER_EXPIRED)]);
        assert_eq!(
            edits_of(&ops, message_id),
            [(CLOSED.to_owned(), Some(permissions::no_keyboard()))]
        );
    }

    #[tokio::test]
    async fn a_session_whose_process_died_ends_as_by_its_session_end() {
        const C: &str = "cccccccc-0000-4000-8000-000000000003";
        let dir = TempDir::new("slots-reap");
        let (_fake, mut slots) = live_slots(&dir, message_options());
        slots.on_hook(&start(A, 10));
        slots.registry.topic_created(SlotId(0), 100, "a", None);
        connect(&mut slots, 1, A, Some(10));
        slots.on_hook(&hook(A, HookEvent::UserPromptSubmit { prompt_id: None }));
        assert!(slots.activity.contains_key(A));
        // A nested run of A, with its block.
        slots.on_hook(&hook(
            B,
            HookEvent::SessionStart {
                source: Some("startup".into()),
                claude_pid: Some(20),
                parent_claude_pid: Some(10),
            },
        ));
        slots.on_agent(permission(1, "abcde", "p"));
        slots.pump();
        assert_eq!(slots.prompts.active().len(), 1);
        let nested = BlockKey::Nested(B.into());
        assert!(
            slots
                .registry
                .block(&nested)
                .is_some_and(|block| block.running)
        );

        // A's window was closed (its run with it); C starts in the folder.
        slots.registry.forget_recent_starts();
        let mut next = start(C, 30);
        next.live_claude_pids = Some(vec![30]);
        slots.on_hook(&next);
        slots.pump();

        assert!(slots.registry.sessions[A].ended);
        assert!(slots.registry.sessions[B].ended);
        assert!(!slots.activity.contains_key(A), "what A did is dropped");
        assert_eq!(slots.registry.sessions[A].agent, None);
        assert_eq!(
            slots.registry.sessions[C].slot,
            Some(SlotId(0)),
            "the old topic"
        );
        assert_eq!(slots.registry.slots.len(), 1, "no #2");
        assert!(slots.prompts.active().is_empty(), "the prompt is closed");
        let block = slots.registry.block(&nested).unwrap();
        assert!(!block.running, "the nested block is finished");
        assert_eq!(slots.registry.slots[0].current_session.as_deref(), Some(C));
        assert_eq!(
            slots.registry.slots[0].pending_separator.as_deref(),
            Some("── session cccccccc · new ──")
        );
    }

    fn context(percent: u32) -> HookEvent {
        HookEvent::StatusLine {
            model: None,
            effort: None,
            context: Some(percent),
            five_hour: None,
            seven_day: None,
        }
    }

    fn compact(trigger: &str) -> HookEvent {
        HookEvent::PreCompact {
            trigger: Some(trigger.into()),
        }
    }

    fn compacted(pid: u32) -> HookEvent {
        HookEvent::SessionStart {
            source: Some("compact".into()),
            claude_pid: Some(pid),
            parent_claude_pid: None,
        }
    }

    /// The first line of the status message of slot 0 at `at`.
    fn status_head(slots: &Slots, session: &str, at: Instant) -> String {
        let (text, _) = slots.status_view(SlotId(0), session, at);
        text.lines().next().unwrap_or_default().to_owned()
    }

    /// Topic lines about compactions, in send order.
    async fn compact_lines(fake: &Fake) -> Vec<String> {
        tokio::time::sleep(Duration::from_millis(200)).await;
        fake.ops()
            .into_iter()
            .filter_map(|op| match op {
                Op::Send {
                    thread_id: Some(100),
                    text,
                    notify: false,
                    ..
                } if text.starts_with("🗜") => Some(text),
                Op::Send { text, .. } if text.starts_with("🗜") => {
                    panic!("a compaction line with a sound: {text}")
                }
                _ => None,
            })
            .collect()
    }

    #[tokio::test]
    async fn a_compaction_shows_in_the_status_and_its_end_is_one_line() {
        let dir = TempDir::new("slots-compact");
        let (fake, mut slots) = live_slots(&dir, message_options());
        slots.on_hook(&start(A, 10));
        slots.registry.topic_created(SlotId(0), 100, "a", None);
        slots.on_hook(&hook(A, context(80)));
        slots.on_hook(&hook(A, compact("auto")));
        let now = Instant::now();
        assert_eq!(status_head(&slots, A, now), "🗜 Сжимаю контекст (авто)…");
        assert_eq!(
            status_head(&slots, A, now + Duration::from_secs(125)),
            "🗜 Сжимаю контекст (авто)… 2 мин"
        );
        // A repeat while it runs changes nothing; a status line sent while it
        // runs is still the old context.
        slots.on_hook(&hook(A, compact("manual")));
        slots.on_hook(&hook(A, context(83)));
        assert_eq!(status_head(&slots, A, now), "🗜 Сжимаю контекст (авто)…");
        assert_eq!(compact_lines(&fake).await, ["🗜 Сжимаю контекст (авто)…"]);

        slots.on_hook(&hook(A, compacted(10)));
        assert_eq!(status_head(&slots, A, Instant::now()), "💤 Ждёт вас");
        // The old percentage again is not the new one.
        slots.on_hook(&hook(A, context(83)));
        assert_eq!(compact_lines(&fake).await.len(), 1);
        slots.on_hook(&hook(A, context(12)));
        assert_eq!(
            compact_lines(&fake).await,
            [
                "🗜 Сжимаю контекст (авто)…",
                "🗜 Контекст сжат за 0 с: 83% → 12%"
            ]
        );
        assert!(slots.compactions.is_empty());
        // A second SessionStart(compact) with nothing running says nothing.
        slots.on_hook(&hook(A, compacted(10)));
        assert_eq!(compact_lines(&fake).await.len(), 2);

        // Ended, and no new percentage comes: the line goes without numbers.
        slots.on_hook(&hook(A, compact("manual")));
        slots.on_hook(&hook(A, compacted(10)));
        slots.check_compactions(Instant::now());
        assert!(!slots.compactions.is_empty(), "waits for the numbers");
        slots.check_compactions(Instant::now() + COMPACT_NUMBERS_WAIT);
        assert!(slots.compactions.is_empty());
        let lines = compact_lines(&fake).await;
        assert_eq!(
            lines[2..],
            ["🗜 Сжимаю контекст (вручную)…", "🗜 Контекст сжат за 0 с"]
        );

        // One that never ends is forgotten after COMPACT_MAX, with no line.
        slots.on_hook(&hook(A, compact("auto")));
        slots.check_compactions(Instant::now() + COMPACT_MAX);
        assert!(slots.compactions.is_empty());
        assert_eq!(status_head(&slots, A, Instant::now()), "💤 Ждёт вас");
        // And one cut by the session's end leaves no line either.
        slots.on_hook(&hook(A, compact("auto")));
        slots.on_hook(&hook(
            A,
            HookEvent::SessionEnd {
                reason: None,
                claude_pid: Some(10),
            },
        ));
        assert!(slots.compactions.is_empty());
        slots.on_hook(&hook(A, compacted(10)));
        let lines = compact_lines(&fake).await;
        assert_eq!(lines.len(), 6, "{lines:?}");
        assert!(lines[4..].iter().all(|line| line.starts_with("🗜 Сжимаю")));
    }

    #[tokio::test]
    async fn a_compaction_without_a_status_line_is_told_at_once() {
        let dir = TempDir::new("slots-compact-plain");
        let (fake, mut slots) = live_slots(&dir, message_options());
        slots.on_hook(&start(A, 10));
        // No topic yet: nothing to show.
        slots.on_hook(&hook(A, compact("manual")));
        assert!(slots.compactions.is_empty());
        slots.registry.topic_created(SlotId(0), 100, "a", None);
        slots.on_hook(&hook(A, HookEvent::PreCompact { trigger: None }));
        assert_eq!(status_head(&slots, A, Instant::now()), "🗜 Сжимаю контекст…");
        slots.on_hook(&hook(A, compacted(10)));
        assert!(slots.compactions.is_empty());
        assert_eq!(
            compact_lines(&fake).await,
            ["🗜 Сжимаю контекст…", "🗜 Контекст сжат за 0 с"]
        );
        // A nested run's compaction is not the slot's.
        slots.on_hook(&hook(
            B,
            HookEvent::SessionStart {
                source: Some("startup".into()),
                claude_pid: Some(20),
                parent_claude_pid: Some(10),
            },
        ));
        slots.on_hook(&hook(B, compact("auto")));
        assert!(slots.compactions.is_empty());
    }

    #[tokio::test]
    async fn activity_after_a_compaction_ends_its_status_and_a_late_end_still_counts() {
        let dir = TempDir::new("slots-compact-cancel");
        let (fake, mut slots) = live_slots(&dir, message_options());
        slots.on_hook(&start(A, 10));
        slots.registry.topic_created(SlotId(0), 100, "a", None);
        slots.on_hook(&hook(A, context(80)));

        // Cancelled (Esc, an error): the next prompt ends the status at once,
        // and without a SessionStart(compact) no line comes.
        slots.on_hook(&hook(A, compact("manual")));
        slots.on_hook(&hook(A, HookEvent::UserPromptSubmit { prompt_id: None }));
        assert!(!status_head(&slots, A, Instant::now()).starts_with("🗜"));
        // A repeat PreCompact after it is a new compaction.
        slots.on_hook(&hook(A, compact("auto")));
        assert_eq!(
            status_head(&slots, A, Instant::now()),
            "🗜 Сжимаю контекст (авто)…"
        );
        // A tool start ends it too; the grace runs out with no line.
        slots.on_hook(&hook(
            A,
            HookEvent::ToolStart {
                tool_use_id: "t1".into(),
                line: "Bash: ls".into(),
            },
        ));
        assert!(!status_head(&slots, A, Instant::now()).starts_with("🗜"));
        slots.check_compactions(Instant::now());
        assert!(!slots.compactions.is_empty(), "its end is still taken");
        slots.check_compactions(Instant::now() + COMPACT_GRACE);
        assert!(slots.compactions.is_empty());
        slots.on_hook(&hook(A, compacted(10)));
        assert_eq!(
            compact_lines(&fake).await,
            ["🗜 Сжимаю контекст (вручную)…", "🗜 Сжимаю контекст (авто)…"]
        );

        // Activity first, then a late SessionStart(compact) within the grace:
        // the done line still goes.
        slots.on_hook(&hook(A, compact("auto")));
        slots.on_hook(&stop(A, None));
        assert!(!status_head(&slots, A, Instant::now()).starts_with("🗜"));
        slots.on_hook(&hook(A, compacted(10)));
        slots.on_hook(&hook(A, context(20)));
        assert!(slots.compactions.is_empty());
        let lines = compact_lines(&fake).await;
        assert_eq!(
            lines[2..],
            [
                "🗜 Сжимаю контекст (авто)…",
                "🗜 Контекст сжат за 0 с: 80% → 20%"
            ]
        );

        // The usual order: the end, then activity; activity changes nothing.
        slots.on_hook(&hook(A, compact("auto")));
        slots.on_hook(&hook(A, compacted(10)));
        slots.on_hook(&hook(A, HookEvent::UserPromptSubmit { prompt_id: None }));
        slots.on_hook(&hook(A, context(9)));
        let lines = compact_lines(&fake).await;
        assert_eq!(
            lines[4..],
            [
                "🗜 Сжимаю контекст (авто)…",
                "🗜 Контекст сжат за 0 с: 20% → 9%"
            ]
        );
    }

    #[tokio::test]
    async fn only_a_smaller_context_is_the_one_after_a_compaction() {
        let dir = TempDir::new("slots-compact-after");
        let (fake, mut slots) = live_slots(&dir, message_options());
        slots.on_hook(&start(A, 10));
        slots.registry.topic_created(SlotId(0), 100, "a", None);
        slots.on_hook(&hook(A, context(80)));
        slots.on_hook(&hook(A, compact("auto")));
        slots.on_hook(&hook(A, compacted(10)));
        // A line sent while it ran, arriving late: higher, not the new one.
        slots.on_hook(&hook(A, context(83)));
        assert!(!slots.compactions.is_empty(), "still waits");
        slots.on_hook(&hook(A, context(15)));
        assert!(slots.compactions.is_empty());
        assert_eq!(
            compact_lines(&fake).await,
            [
                "🗜 Сжимаю контекст (авто)…",
                "🗜 Контекст сжат за 0 с: 80% → 15%"
            ]
        );
    }

    #[tokio::test]
    async fn a_new_compaction_tells_the_one_waiting_for_its_numbers_first() {
        let dir = TempDir::new("slots-compact-twice");
        let (fake, mut slots) = live_slots(&dir, message_options());
        slots.on_hook(&start(A, 10));
        slots.registry.topic_created(SlotId(0), 100, "a", None);
        slots.on_hook(&hook(A, context(80)));
        slots.on_hook(&hook(A, compact("manual")));
        slots.on_hook(&hook(A, compacted(10)));
        slots.on_hook(&hook(A, compact("auto")));
        assert_eq!(
            status_head(&slots, A, Instant::now()),
            "🗜 Сжимаю контекст (авто)…"
        );
        assert_eq!(
            compact_lines(&fake).await,
            [
                "🗜 Сжимаю контекст (вручную)…",
                "🗜 Контекст сжат за 0 с",
                "🗜 Сжимаю контекст (авто)…"
            ]
        );
    }

    #[tokio::test]
    async fn a_compaction_without_a_context_percentage_is_told_at_once() {
        let dir = TempDir::new("slots-compact-nocontext");
        let (fake, mut slots) = live_slots(&dir, message_options());
        slots.on_hook(&start(A, 10));
        slots.registry.topic_created(SlotId(0), 100, "a", None);
        slots.on_hook(&hook(
            A,
            HookEvent::StatusLine {
                model: Some("opus".into()),
                effort: None,
                context: None,
                five_hour: None,
                seven_day: None,
            },
        ));
        slots.on_hook(&hook(A, compact("auto")));
        slots.on_hook(&hook(A, compacted(10)));
        assert!(slots.compactions.is_empty(), "told at once");
        assert_eq!(
            compact_lines(&fake).await,
            ["🗜 Сжимаю контекст (авто)…", "🗜 Контекст сжат за 0 с"]
        );
    }

    #[tokio::test]
    async fn a_permission_request_consumed_after_session_end_is_dropped() {
        let dir = TempDir::new("slots-late-permission");
        let (fake, mut slots) = live_slots(&dir, message_options());
        slots.on_hook(&start(A, 10));
        slots.registry.topic_created(SlotId(0), 100, "a", None);
        let (to_agent, mut from_hub) = mpsc::channel(4);
        slots.on_agent(AgentEvent::Registered {
            conn: 1,
            register: Register {
                session_id: A.into(),
                host: "box".into(),
                cwd: CWD.into(),
                claude_pid: Some(10),
                verdict_ack: false,
                transcript_reads: false,
                console_keys: false,
                console_commands: false,
                client: None,
                files: false,
                session_reads: false,
                status_lines: false,
                heartbeat: false,
            },
            to_agent,
        });
        slots.on_hook(&hook(
            A,
            HookEvent::SessionEnd {
                reason: None,
                claude_pid: Some(10),
            },
        ));

        slots.on_agent(permission(1, "abcde", "late"));
        slots.pump();
        tokio::time::sleep(Duration::from_millis(100)).await;

        assert!(slots.prompts.is_empty());
        assert!(!fake.ops().iter().any(|op| matches!(
            op,
            Op::Send {
                permission: true,
                ..
            }
        )));
        assert!(from_hub.try_recv().is_err(), "no verdict after SessionEnd");
    }

    /// TASK-058 review: a `bound` that finds the agent queue full after
    /// `/clear` is not lost; the tick tells the new session once there is
    /// room.
    #[tokio::test]
    async fn a_bound_that_finds_the_queue_full_is_told_again() {
        let dir = TempDir::new("slots-bound-retry");
        let (_fake, mut slots) = live_slots(&dir, message_options());
        slots.on_hook(&start(A, 10));
        slots.registry.topic_created(SlotId(0), 100, "a", None);
        let (to_agent, mut from_hub) = mpsc::channel(1);
        slots.on_agent(AgentEvent::Registered {
            conn: 1,
            register: Register {
                session_id: A.into(),
                host: "box".into(),
                cwd: CWD.into(),
                claude_pid: Some(10),
                verdict_ack: true,
                transcript_reads: false,
                console_keys: false,
                console_commands: false,
                client: None,
                files: false,
                session_reads: false,
                status_lines: true,
                heartbeat: false,
            },
            to_agent,
        });
        // The queue holds A's `bound` and has no room for B's.
        slots.on_hook(&hook(
            A,
            HookEvent::SessionEnd {
                reason: Some("clear".into()),
                claude_pid: Some(10),
            },
        ));
        slots.on_hook(&hook(
            B,
            HookEvent::SessionStart {
                source: Some("clear".into()),
                claude_pid: Some(10),
                parent_claude_pid: None,
            },
        ));
        assert_eq!(slots.conns[&1].session, B);
        assert!(slots.conns[&1].untold);
        assert!(slots.next_deadline() <= Instant::now() + BOUND_RETRY);
        assert!(matches!(
            from_hub.try_recv(),
            Ok(HubMsg::Bound { session_id }) if session_id == A
        ));
        assert!(from_hub.try_recv().is_err());
        slots.on_tick();
        assert!(matches!(
            from_hub.try_recv(),
            Ok(HubMsg::Bound { session_id }) if session_id == B
        ));
        assert!(!slots.conns[&1].untold);
        slots.on_tick();
        assert!(from_hub.try_recv().is_err(), "told once");
    }

    #[tokio::test]
    async fn a_frame_read_before_clear_is_not_attributed_to_the_new_session() {
        let dir = TempDir::new("slots-clear-frame-binding");
        let (fake, mut slots) = live_slots(&dir, message_options());
        slots.on_hook(&start(A, 10));
        slots.registry.topic_created(SlotId(0), 100, "a", None);
        connect(&mut slots, 1, A, Some(10));
        let queued = permission(1, "abcde", "before clear");
        tokio::time::sleep(Duration::from_millis(1)).await;

        slots.on_hook(&hook(
            A,
            HookEvent::SessionEnd {
                reason: Some("clear".into()),
                claude_pid: Some(10),
            },
        ));
        slots.on_hook(&hook(
            B,
            HookEvent::SessionStart {
                source: Some("clear".into()),
                claude_pid: Some(10),
                parent_claude_pid: None,
            },
        ));
        slots.on_agent(queued);
        assert!(
            slots.prompts.is_empty(),
            "the pre-clear frame belonged to A"
        );

        slots.on_agent(permission(1, "bcdef", "after clear"));
        assert_eq!(
            slots.prompts.get(0).map(|prompt| prompt.session.as_str()),
            Some(B)
        );
        slots.pump();
        let wait = async {
            while !fake.ops().iter().any(|op| {
                matches!(
                    op,
                    Op::Send {
                        permission: true,
                        ..
                    }
                )
            }) {
                tokio::time::sleep(Duration::from_millis(5)).await;
            }
        };
        tokio::time::timeout(WAIT, wait)
            .await
            .expect("prompt in time");
        let ops = fake.ops();
        assert_eq!(
            ops.iter()
                .filter(|op| matches!(
                    op,
                    Op::Send {
                        permission: true,
                        ..
                    }
                ))
                .count(),
            1
        );
    }

    #[tokio::test]
    async fn start_first_clear_closes_a_prompt_even_when_pruning_removes_its_session() {
        let dir = TempDir::new("slots-clear-prune-prompt");
        let (fake, mut slots) = live_slots(&dir, message_options());
        slots.on_hook(&start(A, 10));
        slots.registry.topic_created(SlotId(0), 100, "a", None);
        connect(&mut slots, 1, A, Some(10));
        slots.on_agent(permission(1, "abcde", "p"));
        slots.prompts.get_mut(0).unwrap().sent = true;
        slots.prompts.delivered(0, 500);

        for i in 0..crate::hub::registry::MAX_SESSIONS - 1 {
            let session = format!("nested-{i:04}");
            slots.on_hook(&hook(
                &session,
                HookEvent::SessionStart {
                    source: Some("startup".into()),
                    claude_pid: None,
                    parent_claude_pid: Some(50_000 + i as u32),
                },
            ));
        }
        assert_eq!(
            slots.registry.sessions.len(),
            crate::hub::registry::MAX_SESSIONS
        );

        slots.on_hook(&hook(
            B,
            HookEvent::SessionStart {
                source: Some("clear".into()),
                claude_pid: Some(10),
                parent_claude_pid: None,
            },
        ));
        assert!(!slots.registry.sessions.contains_key(A), "A was pruned");
        assert_eq!(
            slots.prompts.get(0).map(|prompt| prompt.state),
            Some(State::Closed)
        );

        slots.pump();
        let wait = async {
            while edits_of(&fake.ops(), 500).is_empty() {
                tokio::time::sleep(Duration::from_millis(5)).await;
            }
        };
        tokio::time::timeout(WAIT, wait)
            .await
            .expect("edit in time");
        let ops = fake.ops();
        assert_eq!(
            edits_of(&ops, 500),
            [(CLOSED.to_owned(), Some(permissions::no_keyboard()))]
        );
    }

    #[tokio::test]
    async fn a_prompt_stays_in_its_slot_topic_when_clear_moves_the_slot_on() {
        let mut rig = rig(Fake::default(), message_options());
        two_live_slots(&mut rig, false).await;
        rig.agents.send(permission(1, "abcde", "a")).await.unwrap();
        let ops = settled(&rig, |ops| prompts(ops).len() == 1).await;
        let (thread, _, old) = prompts(&ops).remove(0);
        assert_eq!(thread, 100);
        tokio::time::sleep(Duration::from_millis(100)).await;
        rig.hook(hook(
            A,
            HookEvent::SessionEnd {
                reason: Some("clear".into()),
                claude_pid: Some(10),
            },
        ))
        .await;
        rig.hook(hook(
            B,
            HookEvent::SessionStart {
                source: Some("clear".into()),
                claude_pid: Some(10),
                parent_claude_pid: None,
            },
        ))
        .await;
        settled(&rig, |ops| {
            sent_to(ops, 100).contains(&"── session bbbbbbbb · new ──")
        })
        .await;
        // B, on the same agent (it follows its claude process), asks with the
        // same five letters.
        rig.agents.send(permission(1, "abcde", "b")).await.unwrap();
        let ops = settled(&rig, |ops| prompts(ops).len() == 2).await;
        let (thread, _, new) = prompts(&ops)[1].clone();
        assert_eq!(thread, 100, "the slot's topic");
        tokio::time::sleep(Duration::from_millis(100)).await;
        rig.control
            .send(press("old", Some(old), "allow:abcde"))
            .unwrap();
        rig.control
            .send(press("new", Some(new), "deny:abcde"))
            .unwrap();
        let ops = settled(&rig, |ops| answers(ops).len() == 2).await;
        assert_eq!(
            verdicts(&received(&mut rig, 0).await),
            [("abcde".to_owned(), Behavior::Deny)]
        );
        assert_eq!(
            answers(&ops),
            [
                Some(permissions::ANSWER_EXPIRED),
                Some(permissions::ANSWER_DENIED)
            ]
        );
        assert_eq!(
            edits_of(&ops, old),
            [(CLOSED.to_owned(), Some(permissions::no_keyboard()))]
        );
    }

    #[tokio::test]
    async fn an_ended_sessions_prompt_never_reaches_the_topic_its_slot_moved_on_to() {
        let dir = TempDir::new("slots-prompt-moved");
        let (fake, mut slots) = live_slots(&dir, message_options());
        slots.on_hook(&start(A, 10));
        connect(&mut slots, 1, A, Some(10));
        // A asks while its slot has no topic yet.
        slots.on_agent(permission(1, "abcde", "p"));
        slots.pump();
        slots.on_hook(&hook(
            A,
            HookEvent::SessionEnd {
                reason: None,
                claude_pid: Some(10),
            },
        ));
        slots.on_hook(&start(B, 11));
        slots.registry.topic_created(SlotId(0), 100, "t", None);
        slots.pump();
        tokio::time::sleep(Duration::from_millis(300)).await;
        assert!(
            !fake.ops().iter().any(|op| matches!(
                op,
                Op::Send {
                    permission: true,
                    ..
                }
            )),
            "{:?}",
            fake.ops()
        );
    }

    #[tokio::test]
    async fn a_verdict_lost_with_the_link_goes_again_to_the_reconnected_agent() {
        let mut rig = rig(Fake::default(), message_options());
        rig.hook(start(A, 10)).await;
        rig.ops_after(1).await;
        rig.agent_acking(1, A, Some(10)).await;
        settled(&rig, |ops| last_icon(ops, 100) == Some(ICON_ALIVE)).await;
        rig.agents.send(permission(1, "abcde", "p")).await.unwrap();
        let ops = settled(&rig, |ops| prompts(ops).len() == 1).await;
        let (_, _, message_id) = prompts(&ops).remove(0);
        tokio::time::sleep(Duration::from_millis(100)).await;
        rig.control
            .send(press("q1", Some(message_id), "allow:abcde"))
            .unwrap();
        assert_eq!(
            verdicts(&received(&mut rig, 0).await),
            [("abcde".to_owned(), Behavior::Allow)]
        );
        // The link drops before the agent read the verdict: no ack came back.
        rig.agents
            .send(AgentEvent::Disconnected { conn: 1 })
            .await
            .unwrap();
        rig.agent_acking(2, A, Some(10)).await;
        let again = received(&mut rig, 1).await;
        assert_eq!(
            verdicts(&again),
            [("abcde".to_owned(), Behavior::Allow)],
            "the same verdict again"
        );
        let [
            HubMsg::PermissionVerdict {
                verdict_id: Some(verdict_id),
                ..
            },
        ] = again.as_slice()
        else {
            panic!("{again:?}");
        };
        assert!(
            edits_of(&rig.fake.ops(), message_id).is_empty(),
            "not decided before the ack"
        );
        rig.agents
            .send(AgentEvent::Message {
                conn: 2,
                received_at: StdInstant::now(),
                msg: AgentMsg::PermissionAck {
                    verdict_id: *verdict_id,
                },
            })
            .await
            .unwrap();
        settled(&rig, |ops| edits_of(ops, message_id).len() == 1).await;
    }

    #[tokio::test]
    async fn an_acking_agent_decides_a_prompt_only_with_its_own_ack() {
        let options = Options {
            retry_every: Duration::from_millis(100),
            ..message_options()
        };
        let mut rig = rig(Fake::default(), options);
        rig.hook(start(A, 10)).await;
        rig.ops_after(1).await;
        rig.agent_of(1, A, Some(10)).await;
        rig.hook(start(B, 11)).await;
        settled(&rig, |ops| count(ops, is_create) == 2).await;
        rig.agent_acking(2, B, Some(11)).await;
        settled(&rig, |ops| last_icon(ops, 101) == Some(ICON_ALIVE)).await;
        rig.agents.send(permission(2, "abcde", "p")).await.unwrap();
        let ops = settled(&rig, |ops| {
            prompts(ops).len() == 1
                && last_icon(ops, 101) == Some(crate::hub::registry::ICON_WAITING)
        })
        .await;
        let (_, text, message_id) = prompts(&ops).remove(0);
        tokio::time::sleep(Duration::from_millis(100)).await;
        rig.control
            .send(press("q1", Some(message_id), "deny:abcde"))
            .unwrap();
        // Unacked, it goes again on every tick with the same id.
        tokio::time::sleep(Duration::from_millis(350)).await;
        let got = received(&mut rig, 1).await;
        let ids: HashSet<Option<u64>> = got
            .iter()
            .map(|msg| match msg {
                HubMsg::PermissionVerdict { verdict_id, .. } => *verdict_id,
                other => panic!("{other:?}"),
            })
            .collect();
        assert!(got.len() >= 2, "{got:?}");
        assert_eq!(ids.len(), 1, "{got:?}");
        let Some(Some(verdict_id)) = ids.into_iter().next() else {
            panic!("an acking agent gets an id");
        };
        assert!(edits_of(&rig.fake.ops(), message_id).is_empty());
        assert_eq!(
            last_icon(&rig.fake.ops(), 101),
            Some(crate::hub::registry::ICON_WAITING)
        );
        // An ack from an agent of another session decides nothing.
        rig.agents
            .send(AgentEvent::Message {
                conn: 1,
                received_at: StdInstant::now(),
                msg: AgentMsg::PermissionAck { verdict_id },
            })
            .await
            .unwrap();
        tokio::time::sleep(Duration::from_millis(200)).await;
        assert!(edits_of(&rig.fake.ops(), message_id).is_empty());
        rig.agents
            .send(AgentEvent::Message {
                conn: 2,
                received_at: StdInstant::now(),
                msg: AgentMsg::PermissionAck { verdict_id },
            })
            .await
            .unwrap();
        let ops = settled(&rig, |ops| {
            edits_of(ops, message_id).len() == 1 && last_icon(ops, 101) == Some(ICON_ALIVE)
        })
        .await;
        assert_eq!(
            edits_of(&ops, message_id),
            [(
                format!("{text}{}", permissions::DENIED_MARK),
                Some(permissions::no_keyboard())
            )]
        );
        // Decided: nothing goes again, a press only hears "already decided".
        let _ = received(&mut rig, 1).await;
        rig.control
            .send(press("q2", Some(message_id), "allow:abcde"))
            .unwrap();
        let ops = settled(&rig, |ops| answers(ops).len() == 2).await;
        assert_eq!(
            answers(&ops),
            [
                Some(permissions::ANSWER_DENIED),
                Some(permissions::ANSWER_DECIDED)
            ]
        );
        assert!(received(&mut rig, 1).await.is_empty());
        assert!(verdicts(&received(&mut rig, 0).await).is_empty());
    }

    #[tokio::test]
    async fn a_nested_resume_ending_leaves_the_prompt_open() {
        let mut rig = rig(Fake::default(), message_options());
        two_live_slots(&mut rig, true).await;
        rig.agents.send(permission(2, "abcde", "p")).await.unwrap();
        let ops = settled(&rig, |ops| prompts(ops).len() == 1).await;
        let (_, _, message_id) = prompts(&ops).remove(0);
        tokio::time::sleep(Duration::from_millis(100)).await;
        // The end of a nested `claude -p --resume` of B: another pid.
        rig.hook(hook(
            B,
            HookEvent::SessionEnd {
                reason: None,
                claude_pid: Some(99),
            },
        ))
        .await;
        tokio::time::sleep(Duration::from_millis(200)).await;
        rig.control
            .send(press("q", Some(message_id), "allow:abcde"))
            .unwrap();
        settled(&rig, |ops| answers(ops).len() == 1).await;
        assert_eq!(
            verdicts(&received(&mut rig, 1).await),
            [("abcde".to_owned(), Behavior::Allow)]
        );
    }

    #[tokio::test]
    async fn a_turn_boundary_stops_an_unanswered_prompt_from_holding_the_icon() {
        let mut rig = rig(Fake::default(), message_options());
        two_live_slots(&mut rig, true).await;
        // Answered in the terminal: the hub only sees the turn end.
        rig.agents.send(permission(2, "abcde", "1")).await.unwrap();
        settled(&rig, |ops| {
            last_icon(ops, 101) == Some(crate::hub::registry::ICON_WAITING)
        })
        .await;
        rig.hook(hook(
            B,
            HookEvent::Stop {
                prompt_id: None,
                last_assistant_message: None,
            },
        ))
        .await;
        settled(&rig, |ops| last_icon(ops, 101) == Some(ICON_ALIVE)).await;
        rig.agents.send(permission(2, "bcdef", "2")).await.unwrap();
        let ops = settled(&rig, |ops| {
            prompts(ops).len() == 2
                && last_icon(ops, 101) == Some(crate::hub::registry::ICON_WAITING)
        })
        .await;
        let second = prompts(&ops)[1].2;
        tokio::time::sleep(Duration::from_millis(100)).await;
        rig.control
            .send(press("q", Some(second), "allow:bcdef"))
            .unwrap();
        settled(&rig, |ops| last_icon(ops, 101) == Some(ICON_ALIVE)).await;
    }

    #[tokio::test]
    async fn a_verdict_never_reaches_another_session_on_the_same_pid() {
        let mut rig = rig(Fake::default(), message_options());
        // A hook without a pid: the registry cannot tie pid 10 to A.
        rig.hook(hook(
            A,
            HookEvent::SessionStart {
                source: Some("startup".into()),
                claude_pid: None,
                parent_claude_pid: None,
            },
        ))
        .await;
        rig.ops_after(1).await;
        rig.agent_of(1, A, Some(10)).await;
        settled(&rig, |ops| last_icon(ops, 100) == Some(ICON_ALIVE)).await;
        rig.agents.send(permission(1, "abcde", "p")).await.unwrap();
        let ops = settled(&rig, |ops| prompts(ops).len() == 1).await;
        let (_, _, message_id) = prompts(&ops).remove(0);
        rig.agents
            .send(AgentEvent::Disconnected { conn: 1 })
            .await
            .unwrap();
        // An agent of another session reports the same claude pid (a reused
        // pid); its SessionStart has not arrived yet, so it waits unbound.
        rig.agent_of(7, "cccccccc-0000-4000-8000-000000000003", Some(10))
            .await;
        tokio::time::sleep(Duration::from_millis(200)).await;
        rig.control
            .send(press("q", Some(message_id), "allow:abcde"))
            .unwrap();
        let ops = settled(&rig, |ops| answers(ops).len() == 1).await;
        assert!(verdicts(&received(&mut rig, 1).await).is_empty());
        assert_eq!(answers(&ops), [Some(permissions::ANSWER_OFFLINE)]);
    }

    #[tokio::test]
    async fn the_waiting_icon_stays_while_another_prompt_of_the_session_is_open() {
        let mut rig = rig(Fake::default(), message_options());
        two_live_slots(&mut rig, true).await;
        rig.agents.send(permission(2, "abcde", "1")).await.unwrap();
        rig.agents.send(permission(2, "bcdef", "2")).await.unwrap();
        let ops = settled(&rig, |ops| {
            prompts(ops).len() == 2
                && last_icon(ops, 101) == Some(crate::hub::registry::ICON_WAITING)
        })
        .await;
        let shown = prompts(&ops);
        tokio::time::sleep(Duration::from_millis(100)).await;
        rig.control
            .send(press("q1", Some(shown[0].2), "allow:abcde"))
            .unwrap();
        settled(&rig, |ops| edits_of(ops, shown[0].2).len() == 1).await;
        tokio::time::sleep(Duration::from_millis(300)).await;
        assert_eq!(
            last_icon(&rig.fake.ops(), 101),
            Some(crate::hub::registry::ICON_WAITING),
            "the second prompt is still open"
        );
        rig.control
            .send(press("q2", Some(shown[1].2), "allow:bcdef"))
            .unwrap();
        settled(&rig, |ops| last_icon(ops, 101) == Some(ICON_ALIVE)).await;
    }

    #[tokio::test]
    async fn a_prompt_telegram_refused_leaves_no_waiting_icon() {
        let mut rig = rig(Fake::default(), message_options());
        two_live_slots(&mut rig, true).await;
        rig.fake
            .send_errors
            .lock()
            .unwrap()
            .push("Bad Request: message text is empty");
        rig.agents.send(permission(2, "abcde", "p")).await.unwrap();
        settled(&rig, |ops| {
            ops.iter().any(|op| {
                matches!(
                    op,
                    Op::Send {
                        permission: true,
                        ..
                    }
                )
            })
        })
        .await;
        tokio::time::sleep(Duration::from_millis(500)).await;
        assert_eq!(last_icon(&rig.fake.ops(), 101), Some(ICON_ALIVE));
    }

    #[tokio::test]
    async fn a_failed_decision_edit_is_tried_again_on_the_tick() {
        let options = Options {
            retry_every: Duration::from_millis(100),
            ..message_options()
        };
        let mut rig = rig(Fake::default(), options);
        two_live_slots(&mut rig, true).await;
        rig.agents.send(permission(2, "abcde", "p")).await.unwrap();
        let ops = settled(&rig, |ops| prompts(ops).len() == 1).await;
        let (_, text, message_id) = prompts(&ops).remove(0);
        tokio::time::sleep(Duration::from_millis(100)).await;
        rig.fake
            .message_edit_errors
            .lock()
            .unwrap()
            .push((500, "Internal Server Error"));
        rig.control
            .send(press("q1", Some(message_id), "allow:abcde"))
            .unwrap();
        settled(&rig, |ops| !edits_of(ops, message_id).is_empty()).await;
        tokio::time::sleep(Duration::from_millis(800)).await;
        let decided = (
            format!("{text}{}", permissions::ALLOWED_MARK),
            Some(permissions::no_keyboard()),
        );
        assert_eq!(
            edits_of(&rig.fake.ops(), message_id),
            [decided.clone(), decided],
            "one failure, one retry, then done"
        );
        assert_eq!(verdicts(&received(&mut rig, 1).await).len(), 1);
    }

    #[tokio::test]
    async fn terminal_permission_edit_errors_are_treated_as_applied() {
        let dir = TempDir::new("slots-terminal-prompt-edits");
        let mut slots = stalled_slots(&dir, message_options());
        connect(&mut slots, 1, A, Some(10));

        for (key, description) in [
            "Bad Request: message is not modified",
            "Bad Request: message to edit not found",
            "Bad Request: message can't be edited",
        ]
        .into_iter()
        .enumerate()
        {
            slots.on_agent(permission(1, &request_id(key), "p"));
            let key = key as u64;
            slots.prompts.get_mut(key).unwrap().sent = true;
            slots.prompts.delivered(key, 500 + key as i64);
            slots.finish(key, State::Closed);
            slots.on_prompt_edit_done(
                key,
                Some(Err(ApiError::Telegram {
                    code: 400,
                    description: description.to_owned(),
                })),
            );
            let prompt = slots.prompts.get(key).unwrap();
            assert_eq!(prompt.edit, Edit::Done, "{description}");
            assert_eq!(prompt.edit_failures, 0, "{description}");
        }
    }

    #[tokio::test]
    async fn a_full_prompt_book_expires_its_oldest_prompt_visibly() {
        let dir = TempDir::new("slots-prompt-book");
        let (fake, mut slots) = live_slots(&dir, message_options());
        slots.on_hook(&start(A, 10));
        slots.registry.topic_created(SlotId(0), 100, "a", None);
        connect(&mut slots, 1, A, Some(10));
        for n in 0..permissions::MAX_PROMPTS {
            slots.on_agent(permission(1, &request_id(n), "p"));
        }
        // All shown (no pump: nothing else goes out).
        for key in 0..permissions::MAX_PROMPTS as u64 {
            slots.prompts.get_mut(key).unwrap().sent = true;
            slots.prompts.delivered(key, 5000 + key as i64);
        }
        slots.on_agent(permission(1, "zzzzz", "p"));
        tokio::time::sleep(Duration::from_millis(300)).await;
        let ops = fake.ops();
        let old = edits_of(&ops, 5000);
        assert!(
            old.len() == 1 && old[0].1 == Some(permissions::no_keyboard()),
            "{ops:?}"
        );
        assert_eq!(ops.len(), 1, "{ops:?}");
    }

    #[tokio::test]
    async fn the_first_press_stays_the_answer_while_the_agent_is_away() {
        let mut rig = rig(Fake::default(), message_options());
        two_live_slots(&mut rig, false).await;
        rig.agents.send(permission(1, "abcde", "p")).await.unwrap();
        let ops = settled(&rig, |ops| prompts(ops).len() == 1).await;
        let (_, _, message_id) = prompts(&ops).remove(0);
        rig.agents
            .send(AgentEvent::Disconnected { conn: 1 })
            .await
            .unwrap();
        settled(&rig, |ops| last_icon(ops, 100) == Some(ICON_NO_CHANNEL)).await;
        rig.control
            .send(press("q1", Some(message_id), "deny:abcde"))
            .unwrap();
        settled(&rig, |ops| answers(ops).len() == 1).await;
        // A second thought while the agent is still away changes nothing.
        rig.control
            .send(press("q2", Some(message_id), "allow:abcde"))
            .unwrap();
        settled(&rig, |ops| answers(ops).len() == 2).await;
        rig.agent_of(2, A, Some(10)).await;
        tokio::time::sleep(Duration::from_millis(300)).await;
        rig.control
            .send(press("q3", Some(message_id), "allow:abcde"))
            .unwrap();
        let ops = settled(&rig, |ops| answers(ops).len() == 3).await;
        assert_eq!(
            verdicts(&received(&mut rig, 1).await),
            [("abcde".to_owned(), Behavior::Deny)],
            "the first choice, once"
        );
        assert_eq!(
            answers(&ops),
            [
                Some(permissions::ANSWER_OFFLINE),
                Some(permissions::ANSWER_DECIDED),
                Some(permissions::ANSWER_DECIDED)
            ]
        );
    }

    #[tokio::test]
    async fn a_prompt_overtakes_a_full_reply_backlog() {
        let mut rig = rig(Fake::default(), message_options());
        two_live_slots(&mut rig, true).await;
        // Fill the reply cap for topic 100, then ask for a permission in 101.
        for i in 0..MAX_QUEUED_MESSAGES + 10 {
            rig.agents.send(reply(1, &format!("r{i}"))).await.unwrap();
        }
        rig.agents.send(permission(2, "abcde", "p")).await.unwrap();
        // Identified by its text, not by the lane flag under test.
        let is_prompt = |text: &&str| text.starts_with("Запрос разрешения");
        let ops = settled(&rig, |ops| {
            sends(ops).iter().any(is_prompt) || sends(ops).len() >= 3
        })
        .await;
        let sends: Vec<&str> = sends(&ops);
        // The first reply may already be on its way; nothing else is ahead.
        let at = sends.iter().position(is_prompt);
        assert!(at.is_some_and(|at| at <= 1), "prompt at {at:?}: {sends:?}");
    }

    #[tokio::test]
    async fn a_multi_chunk_reply_is_rejected_atomically_at_the_cap() {
        let dir = TempDir::new("slots-atomic-reply");
        let (fake, mut slots) = stalled_slots_with_fake(&dir, message_options());
        slots.on_hook(&start(A, 10));
        slots.registry.topic_created(SlotId(0), 100, "a", None);
        connect(&mut slots, 1, A, Some(10));
        slots.queued_messages = MAX_QUEUED_MESSAGES - 1;
        let text = format!("{}\n\n{}", "a".repeat(3000), "b".repeat(3000));
        let split = split_for_telegram(&text, SplitOptions::default());
        assert!(split.chunks.len() > 1 && !split.prefer_file);

        slots.on_reply(1, &text);
        tokio::time::sleep(Duration::from_millis(50)).await;
        assert_eq!(slots.queued_messages, MAX_QUEUED_MESSAGES - 1);
        assert!(slots.overflow_warned);
        assert!(fake.ops().is_empty(), "{:?}", fake.ops());
    }

    #[tokio::test]
    async fn a_stalled_telegram_never_stalls_inbound() {
        let fake = Fake {
            stall_sends: true,
            ..Fake::default()
        };
        let mut rig = rig(fake, message_options());
        two_live_slots(&mut rig, false).await;
        rig.hook(start(B, 11)).await; // slot 101, no agent: kept
        settled(&rig, |ops| count(ops, is_create) == 2).await;
        // Far more replies than the scheduler queue (1024) and the cap, plus
        // a burst to a slot without an agent (kept, two notices): every send is
        // stuck behind a send that never ends.
        let flood = async {
            for i in 0..1500 {
                rig.agents.send(reply(1, "stuck")).await.unwrap();
                rig.control.send(say(Some(101), i, Some("x"))).unwrap();
            }
        };
        tokio::time::timeout(WAIT, flood)
            .await
            .expect("agent events kept draining");
        rig.control
            .send(say(Some(100), 5000, Some("still here")))
            .unwrap();
        let arrived = async {
            loop {
                if let Some(HubMsg::Inbound { content, .. }) = rig._to_agent[0].recv().await
                    && content == "still here"
                {
                    return;
                }
            }
        };
        tokio::time::timeout(WAIT, arrived)
            .await
            .expect("inbound reached the agent while Telegram stalled");
    }

    /// The actor alone, driven by direct calls; Telegram never answers, so
    /// `queued_messages` counts every message handed out.
    fn stalled_slots(dir: &TempDir, options: Options) -> Slots {
        stalled_slots_with_fake(dir, options).1
    }

    fn stalled_slots_with_fake(dir: &TempDir, options: Options) -> (Arc<Fake>, Slots) {
        let store = RegistryStore::open(dir.path()).unwrap();
        let stalled = Arc::new(Fake {
            stall: true,
            ..Fake::default()
        });
        let (scheduler, outbox) = Scheduler::new(stalled.clone(), BucketConfig::default());
        tokio::spawn(scheduler.run());
        let slots = Slots::new(Registry::default(), store, outbox, options);
        (stalled, slots)
    }

    fn connect(slots: &mut Slots, conn: u64, session: &str, claude_pid: Option<u32>) {
        let (to_agent, _from_hub) = mpsc::channel(4);
        slots.on_agent(AgentEvent::Registered {
            conn,
            register: Register {
                session_id: session.into(),
                host: "box".into(),
                cwd: CWD.into(),
                claude_pid,
                verdict_ack: false,
                transcript_reads: false,
                console_keys: false,
                console_commands: false,
                client: None,
                files: false,
                session_reads: false,
                status_lines: false,
                heartbeat: false,
            },
            to_agent,
        });
    }

    async fn wait_for_ops(fake: &Arc<Fake>, count: usize) -> Vec<Op> {
        let reached = async {
            loop {
                let ops = fake.ops();
                if ops.len() >= count {
                    return ops;
                }
                tokio::time::sleep(Duration::from_millis(5)).await;
            }
        };
        tokio::time::timeout(WAIT, reached)
            .await
            .expect("ops in time")
    }

    #[tokio::test(start_paused = true)]
    async fn a_burst_of_stickers_gets_one_notice_a_minute() {
        let dir = TempDir::new("slots-notice");
        let mut slots = stalled_slots(&dir, message_options());
        slots.on_hook(&start(A, 10));
        slots.on_hook(&start(B, 11));
        slots.registry.topic_created(SlotId(0), 100, "a", None);
        slots.registry.topic_created(SlotId(1), 101, "b", None);
        for i in 0..10 {
            slots.on_control(say(Some(100), i, None));
        }
        assert_eq!(
            slots.queued_messages, 1,
            "one unsupported-message notice for the burst"
        );
        for i in 10..15 {
            slots.on_control(say(Some(101), i, None));
        }
        assert_eq!(slots.queued_messages, 2, "one notice for the other slot");
        tokio::time::advance(Duration::from_secs(59)).await;
        slots.on_control(say(Some(100), 20, None));
        assert_eq!(slots.queued_messages, 2, "still inside the minute");
        tokio::time::advance(Duration::from_secs(2)).await;
        slots.on_control(say(Some(100), 21, None));
        assert_eq!(slots.queued_messages, 3, "a minute later: one more");
    }

    fn buffered(slots: &Slots, slot: usize) -> Vec<i64> {
        slots.registry.slots[slot]
            .buffer
            .messages
            .iter()
            .map(|parked| parked.message_id)
            .collect()
    }

    fn end(session: &str, pid: u32) -> HookPost {
        hook(
            session,
            HookEvent::SessionEnd {
                reason: None,
                claude_pid: Some(pid),
            },
        )
    }

    /// Like [`connect`], with a link queue that holds `capacity` messages;
    /// returns the agent's end.
    fn connect_queue(
        slots: &mut Slots,
        conn: u64,
        session: &str,
        claude_pid: Option<u32>,
        capacity: usize,
    ) -> mpsc::Receiver<HubMsg> {
        let (to_agent, from_hub) = mpsc::channel(capacity);
        slots.on_agent(AgentEvent::Registered {
            conn,
            register: Register {
                session_id: session.into(),
                host: "box".into(),
                cwd: CWD.into(),
                claude_pid,
                verdict_ack: false,
                transcript_reads: false,
                console_keys: false,
                console_commands: false,
                client: None,
                files: false,
                session_reads: false,
                status_lines: false,
                heartbeat: false,
            },
            to_agent,
        });
        from_hub
    }

    fn drain(from_hub: &mut mpsc::Receiver<HubMsg>) -> Vec<i64> {
        let mut ids = Vec::new();
        while let Ok(msg) = from_hub.try_recv() {
            if let HubMsg::Inbound { meta, .. } = msg {
                ids.push(meta["message_id"].parse().unwrap());
            }
        }
        ids
    }

    #[tokio::test]
    async fn the_51st_message_drops_the_oldest_and_warns_once_per_period() {
        let dir = TempDir::new("slots-buffer-cap");
        let mut slots = stalled_slots(&dir, message_options());
        slots.on_hook(&start(A, 10));
        slots.registry.topic_created(SlotId(0), 100, "a", None);
        slots.on_hook(&end(A, 10));
        for i in 0..60 {
            slots.on_control(say(Some(100), i, Some("x")));
        }
        assert_eq!(buffered(&slots, 0), (10..60).collect::<Vec<_>>());
        assert_eq!(
            slots.queued_messages, 1,
            "one overflow notice, no queued one"
        );
        slots.pump();
        assert_eq!(slots.queued_messages, 2, "and one Resume message");
        slots.pump();
        slots.on_control(say(Some(100), 60, Some("x")));
        assert_eq!(slots.queued_messages, 2, "nothing more in the same period");

        // A new session takes the slot: all 50 go in order, once.
        slots.on_hook(&start(B, 11));
        let mut from_hub = connect_queue(&mut slots, 1, B, Some(11), 64);
        slots.pump();
        assert_eq!(drain(&mut from_hub), (11..61).collect::<Vec<_>>());
        assert!(slots.registry.slots[0].buffer.is_idle());
        slots.pump();
        assert!(drain(&mut from_hub).is_empty(), "never twice");

        // The next dead period warns again.
        slots.on_hook(&end(B, 11));
        let before = slots.queued_messages;
        for i in 100..151 {
            slots.on_control(say(Some(100), i, Some("x")));
        }
        assert_eq!(slots.queued_messages, before + 1);
    }

    #[tokio::test]
    async fn a_full_link_queue_keeps_the_rest_in_order_for_the_next_try() {
        let dir = TempDir::new("slots-buffer-queue");
        let mut slots = stalled_slots(&dir, message_options());
        slots.on_hook(&start(A, 10));
        slots.registry.topic_created(SlotId(0), 100, "a", None);
        for i in 0..6 {
            slots.on_control(say(Some(100), i, Some("x")));
        }
        let mut from_hub = connect_queue(&mut slots, 1, A, Some(10), 4);
        slots.pump();
        assert_eq!(drain(&mut from_hub), [0, 1, 2, 3]);
        // A new message queues behind the kept ones, never ahead.
        slots.on_control(say(Some(100), 6, Some("x")));
        assert_eq!(drain(&mut from_hub), [4, 5, 6]);
        assert!(slots.registry.slots[0].buffer.is_idle());
    }

    #[tokio::test]
    async fn revival_by_resume_new_session_or_clear_delivers_once_in_order() {
        for how in ["resume", "new", "clear"] {
            let dir = TempDir::new("slots-revival");
            let mut slots = stalled_slots(&dir, message_options());
            slots.on_hook(&start(A, 10));
            slots.registry.topic_created(SlotId(0), 100, "a", None);
            let mut next = match how {
                // `/clear` with no agent on line: the slot never looked dead.
                "clear" => {
                    for i in 0..3 {
                        slots.on_control(say(Some(100), i, Some("x")));
                    }
                    slots.on_hook(&hook(
                        A,
                        HookEvent::SessionEnd {
                            reason: Some("clear".into()),
                            claude_pid: Some(10),
                        },
                    ));
                    slots.on_hook(&hook(
                        B,
                        HookEvent::SessionStart {
                            source: Some("clear".into()),
                            claude_pid: Some(10),
                            parent_claude_pid: None,
                        },
                    ));
                    // The channel server keeps the pre-clear id (TASK-013).
                    connect_queue(&mut slots, 1, A, Some(10), 64)
                }
                _ => {
                    slots.on_hook(&end(A, 10));
                    for i in 0..3 {
                        slots.on_control(say(Some(100), i, Some("x")));
                    }
                    slots.pump();
                    let session = if how == "resume" { A } else { B };
                    slots.on_hook(&hook(
                        session,
                        HookEvent::SessionStart {
                            source: Some(if how == "resume" { "resume" } else { "startup" }.into()),
                            claude_pid: Some(11),
                            parent_claude_pid: None,
                        },
                    ));
                    slots.pump();
                    // The agent registers a moment after SessionStart.
                    assert_eq!(buffered(&slots, 0), [0, 1, 2], "{how}: waits for the agent");
                    connect_queue(&mut slots, 1, session, Some(11), 64)
                }
            };
            slots.pump();
            assert_eq!(drain(&mut next), [0, 1, 2], "{how}");
            slots.pump();
            slots.on_tick();
            slots.pump();
            assert!(drain(&mut next).is_empty(), "{how}: once");
            assert!(slots.registry.slots[0].buffer.is_idle(), "{how}");
            assert_eq!(slots.registry.slots.len(), 1, "{how}: same slot");
        }
    }

    #[tokio::test]
    async fn nested_runs_and_subagents_never_revive_a_slot() {
        let dir = TempDir::new("slots-no-revival");
        let mut slots = stalled_slots(&dir, message_options());
        slots.on_hook(&start(A, 10));
        slots.on_hook(&start(B, 11));
        slots.registry.topic_created(SlotId(0), 100, "a", None);
        slots.on_hook(&end(A, 10));
        for i in 0..3 {
            slots.on_control(say(Some(100), i, Some("x")));
        }
        // A nested `claude -p --resume A` inside B, with its agent linked.
        slots.on_hook(&hook(
            A,
            HookEvent::SessionStart {
                source: Some("resume".into()),
                claude_pid: Some(20),
                parent_claude_pid: Some(11),
            },
        ));
        let mut nested_resume = connect_queue(&mut slots, 1, A, Some(20), 64);
        // A nested run of B and its agent.
        let nested = "cccccccc-0000-4000-8000-000000000003";
        slots.on_hook(&hook(
            nested,
            HookEvent::SessionStart {
                source: Some("startup".into()),
                claude_pid: Some(21),
                parent_claude_pid: Some(11),
            },
        ));
        let mut nested_agent = connect_queue(&mut slots, 2, nested, Some(21), 64);
        // Subagent hooks of the ended session.
        slots.on_hook(&hook(
            A,
            HookEvent::SubagentStart {
                agent_id: "a1b2c3d4e5f607182".into(),
                agent_type: "Explore".into(),
            },
        ));
        slots.pump();
        slots.on_tick();
        slots.pump();
        assert!(drain(&mut nested_resume).is_empty());
        assert!(drain(&mut nested_agent).is_empty());
        assert_eq!(buffered(&slots, 0), [0, 1, 2]);
        assert_eq!(slots.registry.state(SlotId(0)), SlotState::Dead);
    }

    #[tokio::test]
    async fn the_backlog_of_messages_for_telegram_is_capped() {
        let dir = TempDir::new("slots-cap");
        let options = Options {
            notice_every: Duration::ZERO,
            ..message_options()
        };
        let mut slots = stalled_slots(&dir, options);
        slots.on_hook(&start(A, 10));
        slots.registry.topic_created(SlotId(0), 100, "t", None);
        // Photos: each asks for a text-only notice.
        for i in 0..MAX_QUEUED_MESSAGES as i64 + 50 {
            slots.on_control(say(Some(100), i, None));
        }
        assert_eq!(slots.queued_messages, MAX_QUEUED_MESSAGES);
        assert!(slots.overflow_warned);
        // An answer frees one place and the next notice takes it.
        slots.on_done(Done::Message(None));
        slots.on_control(say(Some(100), 1000, None));
        assert_eq!(slots.queued_messages, MAX_QUEUED_MESSAGES);
    }

    #[tokio::test]
    async fn kept_messages_never_go_to_the_agent_of_an_earlier_run() {
        let dir = TempDir::new("slots-buffer-old-run");
        let mut slots = stalled_slots(&dir, message_options());
        slots.on_hook(&start(A, 10));
        slots.registry.topic_created(SlotId(0), 100, "a", None);
        let mut old_run = connect_queue(&mut slots, 1, A, Some(10), 64);
        slots.on_hook(&end(A, 10));
        for i in 0..3 {
            slots.on_control(say(Some(100), i, Some("x")));
        }
        // `claude --resume A` starts before the old run's link is closed.
        slots.on_hook(&hook(
            A,
            HookEvent::SessionStart {
                source: Some("resume".into()),
                claude_pid: Some(12),
                parent_claude_pid: None,
            },
        ));
        slots.pump();
        slots.on_control(say(Some(100), 3, Some("x")));
        assert!(drain(&mut old_run).is_empty(), "the old run takes nothing");
        assert_eq!(buffered(&slots, 0), [0, 1, 2, 3]);
        let mut new_run = connect_queue(&mut slots, 2, A, Some(12), 64);
        slots.pump();
        assert_eq!(drain(&mut new_run), [0, 1, 2, 3]);
        assert!(drain(&mut old_run).is_empty());
    }

    #[tokio::test]
    async fn only_the_slot_that_revives_gets_its_kept_messages() {
        let dir = TempDir::new("slots-buffer-two-slots");
        let mut slots = stalled_slots(&dir, message_options());
        slots.on_hook(&start(A, 10));
        slots.on_hook(&start(B, 11));
        slots.registry.topic_created(SlotId(0), 100, "a", None);
        slots.registry.topic_created(SlotId(1), 101, "b", None);
        let mut agent_b = connect_queue(&mut slots, 2, B, Some(11), 64);
        slots.on_hook(&end(A, 10));
        for i in 0..3 {
            slots.on_control(say(Some(100), i, Some("x")));
        }
        slots.on_control(say(Some(101), 10, Some("x")));
        slots.pump();
        assert_eq!(drain(&mut agent_b), [10], "B gets only its own topic");
        assert_eq!(buffered(&slots, 0), [0, 1, 2]);
        // A new session of the folder takes the free slot #1, not a new one.
        let c = "cccccccc-0000-4000-8000-000000000003";
        slots.on_hook(&start(c, 12));
        let mut agent_c = connect_queue(&mut slots, 3, c, Some(12), 64);
        slots.pump();
        assert_eq!(drain(&mut agent_c), [0, 1, 2]);
        assert!(drain(&mut agent_b).is_empty());
        assert_eq!(slots.registry.slots.len(), 2);
        assert_eq!(slots.registry.slots[0].current_session.as_deref(), Some(c));
        assert!(slots.registry.slots[0].buffer.is_idle());
    }

    fn resume_sends(ops: &[Op]) -> Vec<(i64, Option<serde_json::Value>)> {
        let mut message_id = 1000;
        let mut found = Vec::new();
        for op in ops {
            if let Op::Send {
                text, reply_markup, ..
            } = op
            {
                if text.starts_with("Сессия ") && text.contains("claude --resume") {
                    found.push((message_id, reply_markup.clone()));
                }
                message_id += 1;
            }
        }
        found
    }

    #[tokio::test]
    async fn a_dead_slot_shows_one_resume_button_that_records_the_wish() {
        let mut rig = rig(Fake::default(), message_options());
        two_live_slots(&mut rig, false).await;
        rig.hook(end(A, 10)).await;
        settled(&rig, |ops| last_icon(ops, 100) == Some(ICON_DEAD)).await;
        for i in 1..=3 {
            rig.control.send(say(Some(100), i, Some("x"))).unwrap();
        }
        let ops = settled(&rig, |ops| resume_sends(ops).len() == 1).await;
        tokio::time::sleep(Duration::from_millis(300)).await;
        let ops = rig
            .fake
            .ops()
            .into_iter()
            .skip(ops.len())
            .collect::<Vec<_>>();
        assert!(resume_sends(&ops).is_empty(), "one button per period");
        let (message, markup) = resume_sends(&rig.fake.ops())[0].clone();
        let data = markup.unwrap()["inline_keyboard"][0][0]["callback_data"]
            .as_str()
            .unwrap()
            .to_owned();
        assert_eq!(data, format!("resume:{A}"));
        assert!(data.len() <= permissions::MAX_CALLBACK_DATA);
        assert!(
            !rig.fake
                .ops()
                .iter()
                .any(|op| matches!(op, Op::Delete { .. })),
            "the topic stays as it is"
        );

        rig.control.send(press("q1", Some(message), &data)).unwrap();
        rig.control
            .send(press("q2", Some(message), "resume:0000"))
            .unwrap();
        let ops = settled(&rig, |ops| answers(ops).len() == 2).await;
        assert_eq!(
            answers(&ops),
            [
                Some(buffer::ANSWER_UNAVAILABLE),
                Some(permissions::ANSWER_EXPIRED)
            ]
        );
        let saved = || std::fs::read_to_string(rig.dir.path().join("registry.json")).unwrap();
        let wait = async {
            while !saved().contains("\"resume_asked\": true") {
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        };
        tokio::time::timeout(WAIT, wait).await.expect("wish saved");

        // The session comes back: the messages go, the button goes.
        rig.hook(hook(
            A,
            HookEvent::SessionStart {
                source: Some("resume".into()),
                claude_pid: Some(12),
                parent_claude_pid: None,
            },
        ))
        .await;
        rig.agent_of(2, A, Some(12)).await;
        let ops = settled(&rig, |ops| !edits_of(ops, message).is_empty()).await;
        assert_eq!(
            edits_of(&ops, message),
            [(
                buffer::RESUMED_TEXT.to_owned(),
                Some(permissions::no_keyboard())
            )]
        );
        let got = received(&mut rig, 1).await;
        assert_eq!(contents(&got), ["x"; 3]);
        rig.control.send(press("q3", Some(message), &data)).unwrap();
        let ops = settled(&rig, |ops| answers(ops).len() == 3).await;
        assert_eq!(answers(&ops)[2], Some(buffer::ANSWER_ALIVE));
    }

    #[tokio::test]
    async fn kept_messages_survive_a_restart_and_go_out_once() {
        // The previous run: A ended, three messages kept, the button out.
        let dir = TempDir::new("slots-buffer-restart");
        {
            let mut slots = stalled_slots(&dir, message_options());
            slots.on_hook(&start(A, 10));
            slots.registry.topic_created(SlotId(0), 100, "a", None);
            slots.on_hook(&end(A, 10));
            for i in 1..=3 {
                slots.on_control(say(Some(100), i, Some(&format!("m{i}"))));
            }
            slots.offer_resume();
            if let Some(note) = slots.registry.slots[0].buffer.resume.as_mut() {
                note.message_id = Some(900);
            }
            let store = RegistryStore::open(dir.path()).unwrap();
            store.save(&RegistryStore::encode(&slots.registry)).unwrap();
        }
        let path = dir.path().join("registry.json");
        let mut rig = rig_in(Fake::default(), message_options(), dir);
        rig.hook(hook(
            A,
            HookEvent::SessionStart {
                source: Some("resume".into()),
                claude_pid: Some(12),
                parent_claude_pid: None,
            },
        ))
        .await;
        rig.agent_of(1, A, Some(12)).await;
        let got = received(&mut rig, 0).await;
        assert_eq!(contents(&got), ["m1", "m2", "m3"]);
        let ops = settled(&rig, |ops| !edits_of(ops, 900).is_empty()).await;
        assert!(
            resume_sends(&ops).is_empty(),
            "the button is not offered again"
        );
        // What the next restart would load: nothing left to send.
        let wait = async {
            loop {
                let registry: Registry =
                    serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
                if registry.slots[0].buffer.is_idle() {
                    return;
                }
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        };
        tokio::time::timeout(WAIT, wait)
            .await
            .expect("emptied buffer saved");
        tokio::time::sleep(Duration::from_millis(200)).await;
        assert!(received(&mut rig, 0).await.is_empty());
    }

    /// Swaps the dispatch task of a directly driven actor for a channel the
    /// test reads; Telegram answers only what the test feeds back.
    fn capture_dispatch(slots: &mut Slots) -> mpsc::UnboundedReceiver<(Work, Op)> {
        let (dispatch, work) = mpsc::unbounded_channel();
        slots.dispatch = dispatch;
        work
    }

    /// The numbers of the Resume sends and the message edits handed out
    /// since the last call.
    fn handed(work: &mut mpsc::UnboundedReceiver<(Work, Op)>) -> (Vec<u64>, Vec<(i64, String)>) {
        let (mut sends, mut edits) = (Vec::new(), Vec::new());
        while let Ok((job, op)) = work.try_recv() {
            match (job, op) {
                (Work::Resume { number, .. }, _) => sends.push(number),
                (
                    _,
                    Op::Edit {
                        message_id, text, ..
                    },
                ) => edits.push((message_id, text)),
                _ => {}
            }
        }
        (sends, edits)
    }

    /// Telegram took Resume send `number` of slot 0 as `message_id`.
    fn resume_sent(slots: &mut Slots, number: u64, message_id: i64) {
        slots.on_done(Done::Resume {
            slot: SlotId(0),
            number,
            delivery: Some(Ok(Outcome::Sent(Message {
                message_id,
                ..Message::default()
            }))),
        });
    }

    fn resumed(session: &str, pid: u32) -> HookPost {
        hook(
            session,
            HookEvent::SessionStart {
                source: Some("resume".into()),
                claude_pid: Some(pid),
                parent_claude_pid: None,
            },
        )
    }

    /// A ended with one message kept and its Resume send out; returns the
    /// number of that send.
    fn dead_slot_with_a_button(
        slots: &mut Slots,
        work: &mut mpsc::UnboundedReceiver<(Work, Op)>,
    ) -> u64 {
        slots.on_hook(&start(A, 10));
        slots.registry.topic_created(SlotId(0), 100, "a", None);
        slots.on_hook(&end(A, 10));
        slots.on_control(say(Some(100), 1, Some("m1")));
        slots.pump();
        let (sends, _) = handed(work);
        assert_eq!(sends.len(), 1, "one Resume send");
        sends[0]
    }

    #[tokio::test]
    async fn a_resume_message_answered_after_its_period_ended_loses_its_button() {
        let dir = TempDir::new("slots-resume-late");
        let mut slots = stalled_slots(&dir, message_options());
        let mut work = capture_dispatch(&mut slots);
        let first = dead_slot_with_a_button(&mut slots, &mut work);
        // A comes back before Telegram answered the send: the period ends.
        slots.on_hook(&resumed(A, 12));
        let mut from_hub = connect_queue(&mut slots, 1, A, Some(12), 8);
        slots.pump();
        assert_eq!(drain(&mut from_hub), [1]);
        assert!(slots.registry.slots[0].buffer.is_idle());
        assert!(handed(&mut work).1.is_empty(), "no message id to edit yet");
        resume_sent(&mut slots, first, 901);
        assert_eq!(
            handed(&mut work).1,
            [(901, buffer::RESUMED_TEXT.to_owned())]
        );
        assert!(slots.registry.slots[0].buffer.is_idle());
    }

    #[tokio::test]
    async fn a_late_answer_of_an_earlier_period_never_takes_the_new_button() {
        let dir = TempDir::new("slots-resume-race");
        let mut slots = stalled_slots(&dir, message_options());
        let mut work = capture_dispatch(&mut slots);
        let first = dead_slot_with_a_button(&mut slots, &mut work);
        slots.on_hook(&resumed(A, 12));
        let mut from_hub = connect_queue(&mut slots, 1, A, Some(12), 8);
        slots.pump();
        assert_eq!(drain(&mut from_hub), [1]);
        // A ends again and a message comes: a second button goes out while
        // Telegram still has not answered the first.
        slots.on_hook(&end(A, 12));
        slots.on_control(say(Some(100), 2, Some("m2")));
        slots.pump();
        let (sends, _) = handed(&mut work);
        assert_eq!(sends.len(), 1);
        let second = sends[0];
        assert_ne!(first, second);

        resume_sent(&mut slots, first, 901);
        assert_eq!(
            handed(&mut work).1,
            [(901, buffer::RESUMED_TEXT.to_owned())],
            "the first period is over"
        );
        let note = |slots: &Slots| slots.registry.slots[0].buffer.resume.clone().unwrap();
        assert_eq!(note(&slots).message_id, None);
        resume_sent(&mut slots, second, 902);
        assert!(handed(&mut work).1.is_empty(), "the live button stays");
        assert_eq!(note(&slots).message_id, Some(902));
        assert_eq!(buffered(&slots, 0), [2]);
    }

    #[tokio::test]
    async fn a_resume_press_still_counts_after_a_later_session_ended_in_the_slot() {
        let dir = TempDir::new("slots-resume-later-end");
        let mut slots = stalled_slots(&dir, message_options());
        let mut work = capture_dispatch(&mut slots);
        dead_slot_with_a_button(&mut slots, &mut work);
        // A top-level session without a channel takes the slot and ends.
        slots.on_hook(&start(B, 11));
        assert_eq!(slots.registry.slots[0].current_session.as_deref(), Some(B));
        slots.on_hook(&end(B, 11));
        slots.pump();
        assert!(handed(&mut work).0.is_empty(), "one button per period");
        assert_eq!(slots.press_resume(A), buffer::ANSWER_UNAVAILABLE);
        assert!(slots.registry.slots[0].buffer.resume_asked);
        assert_eq!(buffered(&slots, 0), [1]);
    }

    fn count(ops: &[Op], pred: impl Fn(&Op) -> bool) -> usize {
        ops.iter().filter(|op| pred(op)).count()
    }

    fn is_create(op: &Op) -> bool {
        matches!(op, Op::CreateTopic { .. })
    }

    #[tokio::test]
    async fn the_agent_follows_its_claude_process_through_clear() {
        let mut rig = rig(Fake::default(), options());
        rig.hook(start(A, 10)).await;
        rig.ops_after(1).await;
        rig.agent_of(1, A, Some(10)).await;
        let ops = rig.ops_after(2).await;
        assert_eq!(icon_edit(&ops[1]), Some(ICON_ALIVE), "{ops:?}");

        // `/clear`: the channel server keeps running with the old id.
        rig.hook(hook(
            A,
            HookEvent::SessionEnd {
                reason: Some("clear".into()),
                claude_pid: Some(10),
            },
        ))
        .await;
        rig.hook(hook(
            B,
            HookEvent::SessionStart {
                source: Some("clear".into()),
                claude_pid: Some(10),
                parent_claude_pid: None,
            },
        ))
        .await;
        // The end may flash "dead" before the start arrives; what matters is
        // that the new session ends up alive, never "no channel".
        let ops = rig.ops_after(5).await;
        assert_eq!(count(&ops, is_create), 1, "same slot: {ops:?}");
        assert!(
            ops[1..]
                .iter()
                .all(|op| icon_edit(op) != Some(ICON_NO_CHANNEL)),
            "{ops:?}"
        );
        let last_icon = ops.iter().rev().find_map(icon_edit);
        assert_eq!(last_icon, Some(ICON_ALIVE), "{ops:?}");

        // The link drops and comes back with the stale env id: still B.
        rig.agents
            .send(AgentEvent::Disconnected { conn: 1 })
            .await
            .unwrap();
        let before = rig.ops_after(ops.len() + 1).await.len();
        rig.agent_of(2, A, Some(10)).await;
        let ops = rig.ops_after(before + 1).await;
        assert_eq!(icon_edit(ops.last().unwrap()), Some(ICON_ALIVE), "{ops:?}");
    }

    /// The recorded ops, once they satisfy `ready`.
    async fn settled(rig: &Rig, ready: impl Fn(&[Op]) -> bool) -> Vec<Op> {
        let reached = async {
            loop {
                let ops = rig.fake.ops();
                if ready(&ops) {
                    return ops;
                }
                tokio::time::sleep(Duration::from_millis(5)).await;
            }
        };
        match tokio::time::timeout(WAIT, reached).await {
            Ok(ops) => ops,
            Err(_) => panic!("ops never settled: {:?}", rig.fake.ops()),
        }
    }

    fn last_icon(ops: &[Op], thread: i64) -> Option<&str> {
        ops.iter().rev().find_map(|op| match op {
            Op::EditTopic {
                thread_id,
                icon_custom_emoji_id: Some(icon),
                ..
            } if *thread_id == thread => Some(icon.as_str()),
            _ => None,
        })
    }

    #[tokio::test]
    async fn the_agent_follows_its_claude_process_when_the_new_start_comes_first() {
        let mut rig = rig(Fake::default(), options());
        rig.hook(start(A, 10)).await;
        rig.ops_after(1).await;
        rig.agent_of(1, A, Some(10)).await;
        rig.ops_after(2).await;

        // `/clear` hooks run as separate processes: the new start can win.
        rig.hook(hook(
            B,
            HookEvent::SessionStart {
                source: Some("clear".into()),
                claude_pid: Some(10),
                parent_claude_pid: None,
            },
        ))
        .await;
        rig.hook(hook(
            A,
            HookEvent::SessionEnd {
                reason: Some("clear".into()),
                claude_pid: Some(10),
            },
        ))
        .await;
        // B takes A's slot (same claude pid) and the agent moves with it:
        // the topic is renamed and never shows "no channel".
        let named_b = |ops: &[Op]| {
            ops.iter().any(|op| {
                matches!(op, Op::EditTopic { thread_id: 100, name: Some(name), .. }
                    if name.ends_with("bbbbbbbb"))
            })
        };
        settled(&rig, named_b).await;
        // Room for a wrong "no channel" edit to show up.
        tokio::time::sleep(Duration::from_millis(300)).await;
        let ops = rig.fake.ops();
        assert_eq!(count(&ops, is_create), 1, "{ops:?}");
        assert!(
            ops.iter().all(|op| icon_edit(op) != Some(ICON_NO_CHANNEL)),
            "{ops:?}"
        );
        assert_eq!(last_icon(&ops, 100), Some(ICON_ALIVE), "{ops:?}");

        // The late end of A must not take the pid away from B: the link
        // drops (B loses its agent) and a reconnect with the stale env id
        // lands on B again.
        rig.agents
            .send(AgentEvent::Disconnected { conn: 1 })
            .await
            .unwrap();
        settled(&rig, |ops| last_icon(ops, 100) == Some(ICON_NO_CHANNEL)).await;
        rig.agent_of(2, A, Some(10)).await;
        settled(&rig, |ops| last_icon(ops, 100) == Some(ICON_ALIVE)).await;
    }

    #[tokio::test]
    async fn one_slot_lives_through_hook_agent_end_and_the_next_session() {
        let mut rig = rig(Fake::default(), options());
        rig.hook(start(A, 10)).await;
        let ops = rig.ops_after(1).await;
        assert_eq!(ops.len(), 1);
        assert!(
            matches!(&ops[0], Op::CreateTopic { name, icon_custom_emoji_id }
            if name == "[box] Project · aaaaaaaa" && icon_custom_emoji_id.as_deref() == Some(ICON_NO_CHANNEL))
        );

        rig.agent(1, A).await;
        let ops = rig.ops_after(2).await;
        assert_eq!(ops.len(), 2, "{ops:?}");
        assert_eq!(icon_edit(&ops[1]), Some(ICON_ALIVE));

        // The edit's service message is deleted; one in an unknown topic is not.
        for (thread, message_id) in [(Some(100), 55), (Some(999), 56), (None, 57)] {
            rig.control
                .send(Control::TopicEdited {
                    thread_id: thread,
                    message_id,
                })
                .unwrap();
        }
        let ops = rig.ops_after(3).await;
        assert_eq!(ops.len(), 3, "{ops:?}");
        assert!(matches!(ops[2], Op::Delete { message_id: 55 }));

        rig.hook(hook(
            A,
            HookEvent::SessionEnd {
                reason: None,
                claude_pid: None,
            },
        ))
        .await;
        let ops = rig.ops_after(4).await;
        assert_eq!(ops.len(), 4, "{ops:?}");
        assert_eq!(icon_edit(&ops[3]), Some(ICON_DEAD));
        assert!(matches!(
            &ops[3],
            Op::EditTopic {
                thread_id: 100,
                name: None,
                ..
            }
        ));

        rig.hook(start(B, 11)).await;
        let ops = rig.ops_after(6).await;
        assert_eq!(count(&ops, is_create), 1, "no second topic: {ops:?}");
        let separators: Vec<&Op> = ops
            .iter()
            .filter(|op| matches!(op, Op::Send { .. }))
            .collect();
        assert_eq!(separators.len(), 1);
        assert!(
            matches!(separators[0], Op::Send { thread_id: Some(100), text, notify: false, .. }
            if text == "── session bbbbbbbb · new ──")
        );
        assert!(
            matches!(ops.last().unwrap(), Op::EditTopic { name: Some(name), .. }
            if name == "[box] Project · bbbbbbbb")
        );

        // Saved to disk.
        let store = RegistryStore::open(rig.dir.path()).unwrap();
        let saved_b = async {
            loop {
                if let Ok(saved) = store.load()
                    && saved
                        .slots
                        .first()
                        .and_then(|slot| slot.current_session.as_deref())
                        == Some(B)
                {
                    return saved;
                }
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        };
        let saved = tokio::time::timeout(WAIT, saved_b).await.expect("B saved");
        assert_eq!(saved.slots.len(), 1);
        assert_eq!(saved.slots[0].current_session.as_deref(), Some(B));
        assert_eq!(saved.slots[0].topic_id, Some(100));
    }

    #[tokio::test]
    async fn two_live_sessions_make_two_topics_and_waiting_shows() {
        let mut rig = rig(Fake::default(), options());
        rig.hook(start(A, 10)).await;
        rig.hook(start(B, 11)).await;
        let ops = rig.ops_after(2).await;
        let names: Vec<&str> = ops
            .iter()
            .filter_map(|op| match op {
                Op::CreateTopic { name, .. } => Some(name.as_str()),
                _ => None,
            })
            .collect();
        assert_eq!(
            names,
            ["[box] Project · aaaaaaaa", "[box] Project #2 · bbbbbbbb"]
        );
        rig.agent(1, B).await;
        rig.agents
            .send(AgentEvent::Message {
                conn: 1,
                received_at: StdInstant::now(),
                msg: AgentMsg::PermissionRequest(PermissionRequest {
                    request_id: "abcde".into(),
                    tool_name: "Bash".into(),
                    description: "d".into(),
                    input_preview: "p".into(),
                }),
            })
            .await
            .unwrap();
        let ops = rig.ops_after(3).await;
        let last = icon_edit(ops.last().unwrap());
        // Either the alive edit coalesced into waiting or both happened.
        assert_eq!(last, Some(crate::hub::registry::ICON_WAITING), "{ops:?}");
    }

    #[tokio::test]
    async fn an_agent_before_its_hook_is_bound_without_a_second_topic() {
        let mut rig = rig(Fake::default(), options());
        rig.agent(1, A).await;
        tokio::time::sleep(Duration::from_millis(50)).await;
        assert!(rig.fake.ops().is_empty(), "waits for the hook");
        rig.hook(start(A, 10)).await;
        let ops = rig.ops_after(1).await;
        assert_eq!(ops.len(), 1);
        assert!(
            matches!(&ops[0], Op::CreateTopic { icon_custom_emoji_id, .. }
            if icon_custom_emoji_id.as_deref() == Some(ICON_ALIVE))
        );

        // No hook yet: the agent waits, however long, and makes no topic.
        rig.agent(2, B).await;
        tokio::time::sleep(Duration::from_millis(500)).await;
        assert_eq!(count(&rig.fake.ops(), is_create), 1);
        rig.hook(start(B, 11)).await;
        let ops = rig.ops_after(2).await;
        assert_eq!(count(&ops, is_create), 2);
        assert!(
            matches!(&ops[1], Op::CreateTopic { name, icon_custom_emoji_id }
            if name == "[box] Project #2 · bbbbbbbb" && icon_custom_emoji_id.as_deref() == Some(ICON_ALIVE))
        );
    }

    #[tokio::test]
    async fn nested_and_subagent_events_make_no_topics() {
        // Only the nested run's one block goes into the parent's topic; an
        // unmatched subagent makes nothing.
        let rig = rig(Fake::default(), options());
        rig.hook(start(A, 10)).await;
        rig.ops_after(1).await;
        rig.hook(hook(
            B,
            HookEvent::SessionStart {
                source: Some("startup".into()),
                claude_pid: Some(20),
                parent_claude_pid: Some(10),
            },
        ))
        .await;
        rig.hook(hook(
            A,
            HookEvent::SubagentStart {
                agent_id: "a1".into(),
                agent_type: "Explore".into(),
            },
        ))
        .await;
        rig.hook(hook(
            A,
            HookEvent::SubagentStop {
                agent_id: "a1".into(),
                agent_type: "Explore".into(),
                agent_transcript_path: None,
                last_assistant_message: None,
            },
        ))
        .await;
        let ops = rig.ops_after(2).await;
        assert_eq!(ops.len(), 2, "{ops:?}");
        assert_eq!(count(&ops, is_create), 1);
        assert_eq!(
            sent_to(&ops, 100),
            [format!(
                "⇣ nested bbbbbbbb\n{}",
                crate::hub::registry::BLOCK_RUNNING
            )]
        );
    }

    const SUBAGENT_JSONL: &str =
        include_str!("../../../transcript/tests/fixtures/subagent_handback.jsonl");
    const SUBAGENT_META: &str =
        include_str!("../../../transcript/tests/fixtures/subagent_handback.meta.json");
    const REPORT: &str = "Modules: lib, render, split.";

    fn subagent_options() -> Options {
        Options {
            correlate_for: Duration::from_millis(400),
            recheck_after: Duration::from_millis(20),
            ..message_options()
        }
    }

    fn start_in(session: &str, pid: u32, transcript: &std::path::Path) -> HookPost {
        let mut post = start(session, pid);
        post.transcript_path = transcript.to_string_lossy().into_owned();
        post
    }

    /// An `Agent` call of the parent, as Claude Code writes it.
    fn call_line(tool: &str, description: &str) -> String {
        let record = serde_json::json!({
            "type": "assistant",
            "message": { "role": "assistant", "stop_reason": "tool_use", "content": [{
                "type": "tool_use", "id": tool, "name": "Agent",
                "input": { "description": description, "prompt": "p", "subagent_type": "Explore" },
            }]},
        });
        format!("{record}\n")
    }

    /// The result of that call: `async_launched` with the agent id.
    fn result_line(tool: &str, agent: &str) -> String {
        let record = serde_json::json!({
            "type": "user",
            "message": { "role": "user", "content": [{
                "type": "tool_result", "tool_use_id": tool,
                "content": [{ "type": "text", "text": "Async agent launched." }],
            }]},
            "toolUseResult": { "isAsync": true, "status": "async_launched", "agentId": agent },
        });
        format!("{record}\n")
    }

    fn sub_start(session: &str, agent: &str, kind: &str) -> HookPost {
        hook(
            session,
            HookEvent::SubagentStart {
                agent_id: agent.into(),
                agent_type: kind.into(),
            },
        )
    }

    fn sub_stop(
        session: &str,
        agent: &str,
        kind: &str,
        path: &std::path::Path,
        last: &str,
    ) -> HookPost {
        hook(
            session,
            HookEvent::SubagentStop {
                agent_id: agent.into(),
                agent_type: kind.into(),
                agent_transcript_path: Some(path.to_string_lossy().into_owned()),
                last_assistant_message: Some(last.into()),
            },
        )
    }

    /// Where the tests' agents find session files: `<dir>/projects`.
    fn projects(dir: &TempDir) -> std::path::PathBuf {
        dir.path().join("projects")
    }

    /// The tests' agents' own project folder: `<dir>/projects/C--w`.
    fn own_project(dir: &TempDir) -> std::path::PathBuf {
        projects(dir).join("C--w")
    }

    /// `<dir>/projects/C--w/<session>.jsonl` holding `jsonl`.
    fn parent_file(dir: &TempDir, session: &str, jsonl: &str) -> std::path::PathBuf {
        let project = projects(dir).join("C--w");
        std::fs::create_dir_all(&project).unwrap();
        let path = project.join(format!("{session}.jsonl"));
        std::fs::write(&path, jsonl).unwrap();
        path
    }

    /// `<dir>/projects/C--w/<session>/subagents/agent-<agent>.jsonl`,
    /// written when `jsonl` is given, its `.meta.json` when `meta` is.
    fn agent_file(
        dir: &TempDir,
        session: &str,
        agent: &str,
        jsonl: Option<&str>,
        meta: Option<&str>,
    ) -> std::path::PathBuf {
        let subagents = projects(dir).join("C--w").join(session).join("subagents");
        std::fs::create_dir_all(&subagents).unwrap();
        if let Some(meta) = meta {
            std::fs::write(subagents.join(format!("agent-{agent}.meta.json")), meta).unwrap();
        }
        let path = subagents.join(format!("agent-{agent}.jsonl"));
        if let Some(jsonl) = jsonl {
            std::fs::write(&path, jsonl).unwrap();
        }
        path
    }

    fn reads_register(session: &str, claude_pid: Option<u32>) -> Register {
        Register {
            session_id: session.into(),
            host: "box".into(),
            cwd: CWD.into(),
            claude_pid,
            verdict_ack: false,
            transcript_reads: false,
            console_keys: false,
            console_commands: false,
            client: None,
            files: false,
            session_reads: true,
            status_lines: false,
            heartbeat: false,
        }
    }

    impl Rig {
        /// An agent of `session` that answers session reads from
        /// `<dir>/projects` like `cctg agent` (TASK-034) and keeps what
        /// else the hub sends it.
        async fn files_agent(
            &mut self,
            conn: u64,
            session: &str,
            pid: u32,
        ) -> mpsc::UnboundedReceiver<HubMsg> {
            let (to_agent, mut from_hub) = mpsc::channel(16);
            let (kept, kept_rx) = mpsc::unbounded_channel();
            let agents = self.agents.clone();
            let own = crate::tail::OwnProject::at(own_project(&self.dir));
            tokio::spawn(async move {
                while let Some(msg) = from_hub.recv().await {
                    let HubMsg::SessionRead {
                        read_id,
                        session_id,
                        ask,
                    } = msg
                    else {
                        let _ = kept.send(msg);
                        continue;
                    };
                    for answer in crate::reads::answer(Some(&own), &session_id, ask) {
                        let event = AgentEvent::Message {
                            conn,
                            received_at: StdInstant::now(),
                            msg: AgentMsg::SessionAnswer { read_id, answer },
                        };
                        if agents.send(event).await.is_err() {
                            return;
                        }
                    }
                }
            });
            self.agents
                .send(AgentEvent::Registered {
                    conn,
                    register: reads_register(session, Some(pid)),
                    to_agent,
                })
                .await
                .unwrap();
            kept_rx
        }
    }

    /// A directly driven agent of `session` that reads session files; what
    /// the hub asks it comes out of the receiver ([`answer_reads`]).
    fn connect_reader(slots: &mut Slots, conn: u64, session: &str) -> mpsc::Receiver<HubMsg> {
        let (to_agent, from_hub) = mpsc::channel(16);
        slots.on_agent(AgentEvent::Registered {
            conn,
            register: reads_register(session, Some(10)),
            to_agent,
        });
        from_hub
    }

    /// Answers the session reads waiting in `from_hub` now (not the ones
    /// the answers bring) from `<dir>/projects`, like `cctg agent`; the
    /// number answered.
    fn answer_reads(
        slots: &mut Slots,
        conn: u64,
        from_hub: &mut mpsc::Receiver<HubMsg>,
        dir: &TempDir,
    ) -> usize {
        let mut asked = Vec::new();
        while let Ok(msg) = from_hub.try_recv() {
            asked.push(msg);
        }
        let mut answered = 0;
        for msg in asked {
            let HubMsg::SessionRead {
                read_id,
                session_id,
                ask,
            } = msg
            else {
                continue;
            };
            let own = crate::tail::OwnProject::at(own_project(dir));
            for answer in crate::reads::answer(Some(&own), &session_id, ask) {
                slots.on_agent(AgentEvent::Message {
                    conn,
                    received_at: StdInstant::now(),
                    msg: AgentMsg::SessionAnswer { read_id, answer },
                });
            }
            answered += 1;
        }
        answered
    }

    /// Waits until the topic shows the alive icon: the session's agent is
    /// bound, so the hooks after this find it.
    async fn bound(rig: &Rig) {
        settled(rig, |ops| {
            ops.iter().any(|op| {
                matches!(op, Op::CreateTopic { icon_custom_emoji_id: Some(icon), .. }
                    | Op::EditTopic { icon_custom_emoji_id: Some(icon), .. } if icon == ICON_ALIVE)
            })
        })
        .await;
    }

    /// What each message shows in the end: sends are numbered like the
    /// fake numbers them, edits replace the text.
    fn shown(ops: &[Op]) -> BTreeMap<i64, String> {
        let mut next = 1000;
        let mut texts = BTreeMap::new();
        for op in ops {
            match op {
                Op::Send { text, .. } => {
                    texts.insert(next, text.clone());
                    next += 1;
                }
                Op::Edit {
                    message_id, text, ..
                } => {
                    texts.insert(*message_id, text.clone());
                }
                _ => {}
            }
        }
        texts
    }

    fn block_of<'a>(texts: &'a BTreeMap<i64, String>, agent: &str) -> Vec<(&'a i64, &'a String)> {
        texts
            .iter()
            .filter(|(_, text)| {
                text.starts_with("↳ ") && text.lines().next().unwrap().contains(agent)
            })
            .collect()
    }

    const S1: &str = "a0000000000000011";
    const S2: &str = "a0000000000000012";
    const S3: &str = "a0000000000000013";
    const INTERNAL: &str = "a0000000000000019";

    #[tokio::test]
    async fn three_explicit_subagents_make_three_blocks_and_internal_agents_none() {
        let mut rig = rig(Fake::default(), subagent_options());
        let mut lines = String::new();
        for (tool, agent, what) in [("t1", S1, "one"), ("t2", S2, "two"), ("t3", S3, "three")] {
            lines.push_str(&call_line(tool, what));
            lines.push_str(&result_line(tool, agent));
        }
        let parent = parent_file(&rig.dir, A, &lines);
        let file = |agent: &str, jsonl: &str, meta: Option<&str>| {
            agent_file(&rig.dir, A, agent, Some(jsonl), meta)
        };
        let one = file(S1, SUBAGENT_JSONL, Some(SUBAGENT_META));
        let two = file(S2, SUBAGENT_JSONL, None);
        // S3's file lags: it stops on the Bash tool result.
        let lagging: String = SUBAGENT_JSONL
            .lines()
            .take(6)
            .map(|l| format!("{l}\n"))
            .collect();
        let three = file(S3, &lagging, None);
        // The `--agent` session's own agent: typed, with files, never called.
        let internal = file(INTERNAL, SUBAGENT_JSONL, None);

        let _agent = rig.files_agent(1, A, 10).await;
        rig.hook(start_in(A, 10, &parent)).await;
        rig.ops_after(1).await;
        bound(&rig).await;
        for agent in [S1, S2, S3] {
            rig.hook(sub_start(A, agent, "Explore")).await;
        }
        rig.hook(sub_start(A, INTERNAL, "my-agent")).await;
        settled(&rig, |ops| {
            count(ops, |op| matches!(op, Op::Send { .. })) == 3
        })
        .await;
        rig.hook(hook(
            A,
            HookEvent::SubagentHandback {
                agent_id: S1.into(),
                message: REPORT.into(),
            },
        ))
        .await;
        rig.hook(sub_stop(A, S1, "Explore", &one, "Report handed back."))
            .await;
        rig.hook(sub_stop(A, S2, "Explore", &two, "Report handed back."))
            .await;
        rig.hook(sub_stop(A, S3, "Explore", &three, "The final answer."))
            .await;
        rig.hook(sub_stop(
            A,
            INTERNAL,
            "my-agent",
            &internal,
            "Internal text.",
        ))
        .await;
        // Each finished block gets its "закончил" reply (TASK-033). Replies
        // are metered sends, one per `min_gap` (1 s), so they trail the edits
        // by seconds and used to race the quiet window below (TASK-060).
        let all = settled(&rig, |ops| {
            count(ops, |op| matches!(op, Op::Edit { .. })) == 3 && replies(ops).len() == 3
        })
        .await;
        // Past every window and past `min_gap`, so a pending metered send
        // would have shown: nothing more comes.
        tokio::time::sleep(Duration::from_millis(1500)).await;
        let ops_later = rig.fake.ops();
        assert_eq!(ops_later.len(), all.len(), "{ops_later:?}");
        let mut replied: Vec<i64> = replies(&all).iter().map(|reply| reply.1).collect();
        replied.sort_unstable();
        assert_eq!(replied, [1000, 1001, 1002], "{all:?}");
        let ops: Vec<Op> = all
            .into_iter()
            .filter(|op| {
                !matches!(
                    op,
                    Op::Send {
                        reply_to: Some(_),
                        ..
                    }
                )
            })
            .collect();

        assert_eq!(count(&ops, is_create), 1);
        let texts = shown(&ops);
        assert_eq!(texts.len(), 3, "{texts:?}");
        let one = block_of(&texts, S1);
        assert_eq!(one.len(), 1);
        assert_eq!(one[0].1, &format!("↳ Explore {S1}: one\n{REPORT}"));
        let two = block_of(&texts, S2);
        assert_eq!(
            two[0].1,
            &format!(
                "↳ Explore {S2}: two\n• Bash: List source files\n• SubagentHandback\nReport handed back."
            )
        );
        let three = block_of(&texts, S3);
        assert_eq!(
            three[0].1,
            &format!("↳ Explore {S3}: three\nThe final answer.")
        );
        // No ghost block, and a subagent's stop is never a turn answer.
        for op in &ops {
            if let Op::Send { text, .. } | Op::Edit { text, .. } = op {
                assert!(
                    !text.contains(INTERNAL) && !text.contains("Internal text."),
                    "{text}"
                );
                assert!(text.starts_with("↳ "), "{text}");
            }
        }
    }

    #[tokio::test]
    async fn without_an_agent_that_reads_a_block_opens_on_its_stop_from_the_hooks() {
        // An old agent (no `session_reads`) or none at all: nothing reads
        // the parent transcript, so a block opens only on the stop.
        let mut rig = rig(Fake::default(), subagent_options());
        rig.agent_of(1, A, Some(10)).await;
        rig.hook(start(A, 10)).await;
        // The bind may land after the topic: then an icon edit follows.
        bound(&rig).await;
        let before = rig.fake.ops().len();
        let path = agent_file(&rig.dir, A, S1, Some(SUBAGENT_JSONL), None);
        rig.hook(sub_start(A, S1, "Explore")).await;
        tokio::time::sleep(Duration::from_millis(600)).await;
        assert_eq!(
            rig.fake.ops().len(),
            before,
            "no block before the stop: {:?}",
            rig.fake.ops()
        );
        rig.hook(hook(
            A,
            HookEvent::SubagentHandback {
                agent_id: S2.into(),
                message: REPORT.into(),
            },
        ))
        .await;
        rig.hook(sub_stop(A, S1, "Explore", &path, "Done.")).await;
        rig.hook(sub_stop(A, S2, "Plan", &path, "Other.")).await;
        let ops = settled(&rig, |ops| replies(ops).len() == 2).await;
        let texts = shown(&ops);
        assert_eq!(block_of(&texts, S1)[0].1, &format!("↳ Explore {S1}\nDone."));
        assert_eq!(block_of(&texts, S2)[0].1, &format!("↳ Plan {S2}\n{REPORT}"));
        // Its agent was never asked to read.
        assert!(received(&mut rig, 0).await.is_empty());
    }

    #[tokio::test]
    async fn a_stop_before_its_call_is_visible_still_gets_its_block() {
        let mut rig = rig(Fake::default(), subagent_options());
        let parent = parent_file(&rig.dir, A, &call_line("t1", "late"));
        let _agent = rig.files_agent(1, A, 10).await;
        rig.hook(start_in(A, 10, &parent)).await;
        rig.ops_after(1).await;
        bound(&rig).await;
        // No start seen (hooks installed mid-session); the stop comes first.
        let gone = agent_file(&rig.dir, A, S1, None, None);
        rig.hook(sub_stop(A, S1, "Explore", &gone, "Done.")).await;
        tokio::time::sleep(Duration::from_millis(100)).await;
        assert_eq!(
            count(&rig.fake.ops(), |op| matches!(
                op,
                Op::Send { .. } | Op::Edit { .. }
            )),
            0,
            "no block before the match"
        );
        // The parent transcript catches up.
        let mut file = std::fs::OpenOptions::new()
            .append(true)
            .open(&parent)
            .unwrap();
        std::io::Write::write_all(&mut file, result_line("t1", S1).as_bytes()).unwrap();
        drop(file);
        let ops = settled(&rig, |ops| {
            shown(ops).values().any(|text| text.ends_with("\nDone."))
        })
        .await;
        tokio::time::sleep(Duration::from_millis(200)).await;
        let ops_later = rig.fake.ops();
        assert_eq!(ops_later.len(), ops.len());
        assert_eq!(count(&ops, |op| matches!(op, Op::Send { .. })), 1);
        assert_eq!(
            shown(&ops).values().collect::<Vec<_>>(),
            [&format!("↳ Explore {S1}: late\nDone.")]
        );
    }

    #[tokio::test]
    async fn a_reply_to_a_subagent_block_goes_to_the_parent_with_its_agent_id() {
        let mut rig = rig(Fake::default(), subagent_options());
        let parent = parent_file(
            &rig.dir,
            A,
            &(call_line("t1", "one") + &result_line("t1", S1)),
        );
        rig.hook(start_in(A, 10, &parent)).await;
        rig.ops_after(1).await;
        let mut agent = rig.files_agent(1, A, 10).await;
        bound(&rig).await;
        rig.hook(sub_start(A, S1, "Explore")).await;
        // A nested run of A, with an agent of its own.
        rig.hook(hook(
            B,
            HookEvent::SessionStart {
                source: Some("startup".into()),
                claude_pid: Some(20),
                parent_claude_pid: Some(10),
            },
        ))
        .await;
        rig.agent_of(2, B, Some(20)).await;
        let ops = settled(&rig, |ops| {
            count(ops, |op| matches!(op, Op::Send { .. })) == 2
        })
        .await;
        let texts = shown(&ops);
        let block = *block_of(&texts, S1)[0].0;
        let nested = *texts
            .iter()
            .find(|(_, text)| text.starts_with("⇣ nested"))
            .unwrap()
            .0;
        for (message_id, reply_to) in [(50, block), (51, nested), (52, 999)] {
            rig.control
                .send(Control::Message(Inbound {
                    message_id,
                    thread_id: Some(100),
                    text: Some("hi".into()),
                    reply_to: Some(reply_to),
                    quote: None,
                    forwarded: false,
                    media: None,
                    from_name: None,
                }))
                .unwrap();
        }
        let mut got = Vec::new();
        while got.len() < 3 {
            match tokio::time::timeout(WAIT, agent.recv()).await {
                Ok(Some(msg)) => got.push(msg),
                other => panic!("{other:?}"),
            }
        }
        let metas: Vec<Option<String>> = got
            .iter()
            .map(|msg| match msg {
                HubMsg::Inbound { meta, .. } => meta.get("target_agent").cloned(),
                other => panic!("{other:?}"),
            })
            .collect();
        assert_eq!(metas, [Some(S1.to_owned()), None, None]);
        assert!(crate::channel::is_meta_key("target_agent"));
        // The nested run's agent is no channel: it got nothing.
        assert!(received(&mut rig, 0).await.is_empty());
    }

    #[tokio::test]
    async fn a_nested_run_shows_one_block_and_its_answer_only_there() {
        let rig = rig(Fake::default(), options());
        rig.hook(start(A, 10)).await;
        rig.ops_after(1).await;
        let nested_start = hook(
            B,
            HookEvent::SessionStart {
                source: Some("startup".into()),
                claude_pid: Some(20),
                parent_claude_pid: Some(10),
            },
        );
        rig.hook(nested_start.clone()).await;
        rig.ops_after(2).await;
        let mut again = nested_start;
        again.event_id = crate::wire::EventId::new();
        rig.hook(again).await;
        rig.hook(stop(B, Some("nested answer"))).await;
        rig.hook(hook(
            B,
            HookEvent::SessionEnd {
                reason: Some("other".into()),
                claude_pid: Some(20),
            },
        ))
        .await;
        let ops = settled(&rig, |ops| {
            count(ops, |op| matches!(op, Op::Edit { .. })) == 1 && replies(ops).len() == 1
        })
        .await;
        tokio::time::sleep(Duration::from_millis(100)).await;
        assert_eq!(rig.fake.ops().len(), ops.len());
        assert_eq!(count(&ops, is_create), 1);
        let block = ops
            .iter()
            .find_map(|op| match op {
                Op::Edit { message_id, .. } => Some(*message_id),
                _ => None,
            })
            .unwrap();
        assert_eq!(
            sent_to(&ops, 100),
            [
                format!("⇣ nested bbbbbbbb\n{}", crate::hub::registry::BLOCK_RUNNING),
                "✓ nested bbbbbbbb закончил".to_owned(),
            ]
        );
        assert_eq!(
            replies(&ops),
            [(100, block, "✓ nested bbbbbbbb закончил".to_owned())]
        );
        assert_eq!(loud(&ops), ["✓ nested bbbbbbbb закончил"]);
        assert_eq!(shown(&ops)[&block], "⇣ nested bbbbbbbb\nnested answer");
    }

    #[tokio::test]
    async fn after_a_restart_blocks_are_edited_never_sent_again() {
        let dir = TempDir::new("slots-block-restart");
        let store = RegistryStore::open(dir.path()).unwrap();
        let mut registry = Registry::default();
        registry.apply_hook(&start(A, 10));
        registry.apply_hook(&start(B, 11));
        let jobs = registry.topic_work(&Icons::default(), true);
        for (job, topic) in jobs.into_iter().zip([100, 101]) {
            if let TopicJob::Create { slot, name, icon } = job {
                registry.topic_created(slot, topic, &name, icon.as_deref());
            }
        }
        registry.confirm_subagent(S1, A, format!("↳ Explore {S1}"));
        registry.confirm_subagent(S2, B, format!("↳ Explore {S2}"));
        for (job, message) in registry.block_work(usize::MAX).into_iter().zip([700, 701]) {
            if let BlockJob::Send { key, text, .. } = job {
                registry.block_done(&key, &text, Some(message));
            }
        }
        // B ends while the hub is down.
        registry.apply_hook(&hook(
            B,
            HookEvent::SessionEnd {
                reason: None,
                claude_pid: None,
            },
        ));
        store.save(&RegistryStore::encode(&registry)).unwrap();

        let rig = rig_in(Fake::default(), subagent_options(), dir);
        let ops = settled(&rig, |ops| {
            ops.iter().any(|op| {
                matches!(
                    op,
                    Op::Edit {
                        message_id: 701,
                        ..
                    }
                )
            })
        })
        .await;
        let edits: Vec<(i64, &str)> = ops
            .iter()
            .filter_map(|op| match op {
                Op::Edit {
                    message_id, text, ..
                } => Some((*message_id, text.as_str())),
                _ => None,
            })
            .collect();
        let lost = format!("↳ Explore {S2}\n{}", crate::hub::registry::BLOCK_LOST);
        assert!(edits.contains(&(701, lost.as_str())), "{ops:?}");
        // The live session's subagent hooks come again: same message.
        rig.hook(sub_start(A, S1, "Explore")).await;
        let gone = rig.dir.path().join("agent-missing.jsonl");
        rig.hook(sub_stop(A, S1, "Explore", &gone, "Late.")).await;
        settled(&rig, |ops| replies(ops).len() == 2).await;
        tokio::time::sleep(Duration::from_millis(100)).await;
        let all = rig.fake.ops();
        assert_eq!(
            count(&all, |op| matches!(
                op,
                Op::Send { reply_to: None, .. } | Op::CreateTopic { .. }
            )),
            0
        );
        assert_eq!(shown(&all)[&700], format!("↳ Explore {S1}\nLate."));
        // Both blocks ended after the restart: one reply each.
        assert_eq!(
            replies(&all),
            [
                (101, 701, format!("✗ Explore {} итог не получен", &S2[..8])),
                (100, 700, format!("✓ Explore {} закончил", &S1[..8])),
            ]
        );
    }

    /// The "finished" replies to blocks: `(thread, reply_to, text)`.
    fn replies(ops: &[Op]) -> Vec<(i64, i64, String)> {
        ops.iter()
            .filter_map(|op| match op {
                Op::Send {
                    thread_id: Some(thread),
                    text,
                    reply_to: Some(reply_to),
                    ..
                } => Some((*thread, *reply_to, text.clone())),
                _ => None,
            })
            .collect()
    }

    /// A registry with A's topic (100) and the subagents `agents` of A
    /// confirmed; `legacy`: saved like TASK-011 code did, without a block.
    fn saved_with_subagents(dir: &TempDir, agents: &[&str], legacy: bool) {
        let store = RegistryStore::open(dir.path()).unwrap();
        let mut registry = Registry::default();
        registry.apply_hook(&start(A, 10));
        for job in registry.topic_work(&Icons::default(), true) {
            if let TopicJob::Create { slot, name, icon } = job {
                registry.topic_created(slot, 100, &name, icon.as_deref());
            }
        }
        for agent in agents {
            registry.confirm_subagent(agent, A, format!("↳ Explore {agent}"));
        }
        let mut json: serde_json::Value =
            serde_json::from_slice(&RegistryStore::encode(&registry)).unwrap();
        if legacy {
            for entry in json["subagents"].as_object_mut().unwrap().values_mut() {
                entry.as_object_mut().unwrap().remove("block");
            }
        }
        store.save(&serde_json::to_vec(&json).unwrap()).unwrap();
    }

    #[tokio::test]
    async fn a_legacy_subagent_record_never_becomes_a_block() {
        // TASK-011 recorded every typed hook, internal agents included.
        let dir = TempDir::new("slots-legacy-ghost");
        saved_with_subagents(&dir, &[INTERNAL], true);
        let mut rig = rig_in(Fake::default(), subagent_options(), dir);
        let _agent = rig.files_agent(1, A, 10).await;
        bound(&rig).await;
        let gone = agent_file(&rig.dir, A, INTERNAL, None, None);
        rig.hook(sub_stop(A, INTERNAL, "my-agent", &gone, "Ghost."))
            .await;
        tokio::time::sleep(Duration::from_millis(600)).await;
        let ops = rig.fake.ops();
        assert_eq!(
            count(&ops, |op| matches!(op, Op::Send { .. } | Op::Edit { .. })),
            0,
            "{ops:?}"
        );
    }

    #[tokio::test]
    async fn a_first_send_cut_off_by_a_restart_stays_unsent() {
        // S1's send was out when the hub stopped: Telegram may show it.
        let dir = TempDir::new("slots-unknown-send");
        {
            let store = RegistryStore::open(dir.path()).unwrap();
            saved_with_subagents(&dir, &[S1], false);
            let mut registry = store.load().unwrap();
            assert_eq!(registry.block_work(usize::MAX).len(), 1);
            store.save(&RegistryStore::encode(&registry)).unwrap();
        }
        let rig = rig_in(Fake::default(), subagent_options(), dir);
        let gone = rig.dir.path().join("agent-missing.jsonl");
        rig.hook(sub_stop(A, S1, "Explore", &gone, "Late.")).await;
        tokio::time::sleep(Duration::from_millis(600)).await;
        let ops = rig.fake.ops();
        assert_eq!(
            count(&ops, |op| matches!(op, Op::Send { .. } | Op::Edit { .. })),
            0,
            "at most once: {ops:?}"
        );
        // The tombstone is on disk: no text waits, the send is marked.
        let saved = RegistryStore::open(rig.dir.path()).unwrap().load().unwrap();
        let block = &saved.subagents[S1].block;
        assert!(block.sending && block.pending.is_none() && !block.running);
        assert_eq!(block.message_id, None);
    }

    #[tokio::test]
    async fn a_first_send_with_an_unclear_answer_is_not_sent_again() {
        let fake = Fake {
            unclear_sends: Mutex::new(1),
            ..Fake::default()
        };
        let options = Options {
            retry_every: Duration::from_millis(50),
            ..subagent_options()
        };
        let mut rig = rig(fake, options);
        let parent = parent_file(
            &rig.dir,
            A,
            &(call_line("t1", "one") + &result_line("t1", S1)),
        );
        let _agent = rig.files_agent(1, A, 10).await;
        rig.hook(start_in(A, 10, &parent)).await;
        rig.ops_after(1).await;
        bound(&rig).await;
        rig.hook(sub_start(A, S1, "Explore")).await;
        rig.ops_after(2).await;
        tokio::time::sleep(Duration::from_millis(400)).await;
        let gone = agent_file(&rig.dir, A, S1, None, None);
        rig.hook(sub_stop(A, S1, "Explore", &gone, "Done.")).await;
        // Longer than the scheduler's 1 s gap between sends.
        tokio::time::sleep(Duration::from_millis(2500)).await;
        let ops = rig.fake.ops();
        assert_eq!(
            count(&ops, |op| matches!(op, Op::Send { .. })),
            1,
            "{ops:?}"
        );
    }

    #[tokio::test]
    async fn a_refused_first_send_is_tried_again() {
        let fake = Fake {
            send_errors: Mutex::new(vec!["Bad Request: not enough rights"]),
            ..Fake::default()
        };
        let options = Options {
            retry_every: Duration::from_millis(50),
            ..subagent_options()
        };
        let mut rig = rig(fake, options);
        let parent = parent_file(
            &rig.dir,
            A,
            &(call_line("t1", "one") + &result_line("t1", S1)),
        );
        let _agent = rig.files_agent(1, A, 10).await;
        rig.hook(start_in(A, 10, &parent)).await;
        rig.ops_after(1).await;
        bound(&rig).await;
        rig.hook(sub_start(A, S1, "Explore")).await;
        let ops = settled(&rig, |ops| {
            count(ops, |op| matches!(op, Op::Send { .. })) == 2
        })
        .await;
        tokio::time::sleep(Duration::from_millis(200)).await;
        assert_eq!(rig.fake.ops().len(), ops.len());
    }

    #[tokio::test]
    async fn a_nested_answer_survives_a_restart_before_its_end() {
        let first = rig(Fake::default(), options());
        first.hook(start(A, 10)).await;
        first.ops_after(1).await;
        first
            .hook(hook(
                B,
                HookEvent::SessionStart {
                    source: Some("startup".into()),
                    claude_pid: Some(20),
                    parent_claude_pid: Some(10),
                },
            ))
            .await;
        first.ops_after(2).await;
        first.hook(stop(B, Some("nested answer"))).await;
        tokio::time::sleep(Duration::from_millis(300)).await;
        let Rig { dir, .. } = first;
        let rig = rig_in(Fake::default(), options(), dir);
        rig.hook(hook(
            B,
            HookEvent::SessionEnd {
                reason: Some("other".into()),
                claude_pid: Some(20),
            },
        ))
        .await;
        let ops = settled(&rig, |ops| {
            count(ops, |op| matches!(op, Op::Edit { .. })) == 1
        })
        .await;
        let edits: Vec<&str> = ops
            .iter()
            .filter_map(|op| match op {
                Op::Edit { text, .. } => Some(text.as_str()),
                _ => None,
            })
            .collect();
        assert_eq!(edits, ["⇣ nested bbbbbbbb\nnested answer"]);
    }

    #[tokio::test]
    async fn a_huge_call_description_still_fits_one_message() {
        let mut rig = rig(Fake::default(), subagent_options());
        let description = "описание ".repeat(2000);
        let parent = parent_file(
            &rig.dir,
            A,
            &(call_line("t1", &description) + &result_line("t1", S1)),
        );
        let _agent = rig.files_agent(1, A, 10).await;
        rig.hook(start_in(A, 10, &parent)).await;
        rig.ops_after(1).await;
        bound(&rig).await;
        rig.hook(sub_start(A, S1, "Explore")).await;
        rig.ops_after(2).await;
        rig.hook(hook(
            A,
            HookEvent::SessionEnd {
                reason: None,
                claude_pid: None,
            },
        ))
        .await;
        let ops = settled(&rig, |ops| {
            ops.iter().any(|op| {
                matches!(op, Op::Edit { text, .. }
                    if text.ends_with(crate::hub::registry::BLOCK_LOST))
            })
        })
        .await;
        for op in &ops {
            if let Op::Send { text, .. } | Op::Edit { text, .. } = op {
                assert!(transcript::telegram_len(text) <= transcript::TELEGRAM_TEXT_LIMIT);
            }
        }
        assert_eq!(
            count(&ops, |op| matches!(op, Op::Send { .. })),
            1,
            "{ops:?}"
        );
    }

    #[tokio::test]
    async fn a_subagent_of_a_session_that_ended_before_its_match_gets_no_block() {
        // The session's agent goes with its end: nothing reads the parent
        // transcript any more, and no stop came to build a block from.
        let mut rig = rig(Fake::default(), subagent_options());
        let parent = parent_file(&rig.dir, A, &call_line("t1", "late"));
        let _agent = rig.files_agent(1, A, 10).await;
        rig.hook(start_in(A, 10, &parent)).await;
        rig.ops_after(1).await;
        bound(&rig).await;
        rig.hook(sub_start(A, S1, "Explore")).await;
        rig.hook(hook(
            A,
            HookEvent::SessionEnd {
                reason: None,
                claude_pid: None,
            },
        ))
        .await;
        tokio::time::sleep(Duration::from_millis(100)).await;
        let mut file = std::fs::OpenOptions::new()
            .append(true)
            .open(&parent)
            .unwrap();
        std::io::Write::write_all(&mut file, result_line("t1", S1).as_bytes()).unwrap();
        drop(file);
        tokio::time::sleep(Duration::from_millis(800)).await;
        assert!(block_of(&shown(&rig.fake.ops()), S1).is_empty());
    }

    #[tokio::test]
    async fn the_agent_calls_of_an_ended_session_are_forgotten() {
        let dir = TempDir::new("slots-index-end");
        let parent = parent_file(&dir, A, &(call_line("t1", "one") + &result_line("t1", S1)));
        let mut slots = stalled_slots(&dir, subagent_options());
        let mut from_hub = connect_reader(&mut slots, 1, A);
        slots.on_hook(&start_in(A, 10, &parent));
        slots.on_hook(&sub_start(A, S1, "Explore"));
        assert_eq!(answer_reads(&mut slots, 1, &mut from_hub, &dir), 1);
        assert!(slots.registry.subagents.contains_key(S1));
        // A live session keeps its calls for its next subagents.
        assert!(slots.indexes.contains_key(A));
        slots.on_hook(&hook(
            A,
            HookEvent::SessionEnd {
                reason: None,
                claude_pid: None,
            },
        ));
        assert!(slots.indexes.is_empty());
        // A stop after that still shows the header the block had.
        let gone = agent_file(&dir, A, S1, None, None);
        slots.on_hook(&sub_stop(A, S1, "Explore", &gone, "Late."));
        let late = format!("↳ Explore {S1}: one\nLate.");
        assert_eq!(
            slots.registry.subagents[S1].block.pending.as_deref(),
            Some(late.as_str())
        );
        assert!(slots.indexes.is_empty());
    }

    #[tokio::test]
    async fn a_nested_resume_of_a_top_level_id_makes_no_block_and_no_topic() {
        let rig = rig(Fake::default(), options());
        rig.hook(start(A, 10)).await;
        rig.hook(start(B, 11)).await;
        rig.ops_after(2).await;
        // A runs `claude -p --resume` of B's id and of its own id.
        for (session, pid) in [(B, 30), (A, 31)] {
            rig.hook(hook(
                session,
                HookEvent::SessionStart {
                    source: Some("resume".into()),
                    claude_pid: Some(pid),
                    parent_claude_pid: Some(10),
                },
            ))
            .await;
            rig.hook(hook(
                session,
                HookEvent::SessionEnd {
                    reason: Some("other".into()),
                    claude_pid: Some(pid),
                },
            ))
            .await;
        }
        tokio::time::sleep(Duration::from_millis(300)).await;
        let ops = rig.fake.ops();
        assert_eq!(count(&ops, is_create), 2, "{ops:?}");
        assert_eq!(
            count(&ops, |op| matches!(op, Op::Send { .. } | Op::Edit { .. })),
            0,
            "{ops:?}"
        );
    }

    #[tokio::test]
    async fn a_nested_answer_after_its_parent_ended_still_shows() {
        let rig = rig(Fake::default(), options());
        rig.hook(start(A, 10)).await;
        rig.ops_after(1).await;
        rig.hook(hook(
            B,
            HookEvent::SessionStart {
                source: Some("startup".into()),
                claude_pid: Some(20),
                parent_claude_pid: Some(10),
            },
        ))
        .await;
        rig.ops_after(2).await;
        // The parent goes first (e.g. `/clear`) while the run still works.
        rig.hook(hook(
            A,
            HookEvent::SessionEnd {
                reason: Some("clear".into()),
                claude_pid: Some(10),
            },
        ))
        .await;
        settled(&rig, |ops| {
            shown(ops)
                .values()
                .any(|text| text.ends_with(crate::hub::registry::BLOCK_LOST))
        })
        .await;
        rig.hook(stop(B, Some("late answer"))).await;
        rig.hook(hook(
            B,
            HookEvent::SessionEnd {
                reason: Some("other".into()),
                claude_pid: Some(20),
            },
        ))
        .await;
        let ops = settled(&rig, |ops| {
            shown(ops)
                .values()
                .any(|text| text == "⇣ nested bbbbbbbb\nlate answer")
        })
        .await;
        assert_eq!(
            count(&ops, |op| matches!(op, Op::Send { .. })),
            1,
            "{ops:?}"
        );
    }

    #[tokio::test]
    async fn block_messages_in_flight_are_capped() {
        let dir = TempDir::new("slots-block-cap");
        let mut slots = stalled_slots(&dir, subagent_options());
        slots.on_hook(&start(A, 10));
        let slot = slots.registry.sessions[A].slot.unwrap();
        slots.registry.topic_created(slot, 100, "t", None);
        for i in 0..100 {
            let agent = format!("a{i:016}");
            slots
                .registry
                .confirm_subagent(&agent, A, format!("↳ Explore {agent}"));
        }
        slots.pump();
        slots.pump();
        let busy = slots
            .registry
            .subagents
            .values()
            .filter(|entry| entry.block.busy)
            .count();
        assert_eq!(busy, MAX_BLOCK_JOBS);
    }

    #[tokio::test]
    async fn a_late_body_read_never_overwrites_a_newer_one() {
        let dir = TempDir::new("slots-body-order");
        let mut slots = stalled_slots(&dir, subagent_options());
        let mut from_hub = connect_reader(&mut slots, 1, A);
        slots.on_hook(&start(A, 10));
        slots
            .registry
            .confirm_subagent(S1, A, format!("↳ Explore {S1}"));
        let gone = agent_file(&dir, A, S1, None, None);
        slots.on_hook(&sub_stop(A, S1, "Explore", &gone, "First."));
        // The second stop waits while the first read is out.
        slots.on_hook(&sub_stop(A, S1, "Explore", &gone, "Second."));
        assert_eq!(answer_reads(&mut slots, 1, &mut from_hub, &dir), 1);
        // The older stop's text is never shown, not even for a moment.
        let pending = slots.registry.subagents[S1].block.pending.clone();
        assert!(!pending.is_some_and(|text| text.ends_with("First.")));
        assert_eq!(answer_reads(&mut slots, 1, &mut from_hub, &dir), 1);
        assert_eq!(
            slots.registry.subagents[S1].block.pending.as_deref(),
            Some(format!("↳ Explore {S1}\nSecond.").as_str())
        );
    }

    /// `/brief` or `/full` asked of the directly driven actor.
    fn brief(
        slots: &mut Slots,
        view: commands::View,
        prompts: usize,
        prefix: Option<&str>,
    ) -> oneshot::Receiver<Prepared> {
        let (answer, answered) = oneshot::channel();
        slots.on_transcript_ask(TranscriptAsk {
            thread_id: None,
            command: commands::TranscriptCommand {
                view,
                prompts,
                session_prefix: prefix.map(str::to_owned),
            },
            answer,
        });
        answered
    }

    fn notice(answered: &mut oneshot::Receiver<Prepared>) -> String {
        match answered.try_recv() {
            Ok(Prepared::Notice(text)) => text,
            other => panic!("{other:?}"),
        }
    }

    #[tokio::test]
    async fn brief_is_rendered_by_the_sessions_agent_or_says_why_not() {
        use commands::View;
        let dir = TempDir::new("slots-brief");
        let jsonl = include_str!("../../../transcript/tests/fixtures/tool_use_result.jsonl");
        let transcript = parent_file(&dir, A, jsonl);
        let mut slots = stalled_slots(&dir, options());
        assert_eq!(
            notice(&mut brief(&mut slots, View::Brief, 3, None)),
            "Запущенных сессий нет."
        );
        slots.on_hook(&start_in(A, 10, &transcript));
        // No agent yet, then one built before TASK-034.
        assert!(
            notice(&mut brief(&mut slots, View::Brief, 3, None)).contains("нет связи с агентом")
        );
        connect(&mut slots, 1, A, Some(10));
        assert!(notice(&mut brief(&mut slots, View::Brief, 3, None)).contains("старой версии"));
        // An agent that reads: the reply is its rendering, exactly.
        let mut from_hub = connect_reader(&mut slots, 2, A);
        let mut answered = brief(&mut slots, View::Full, 2, Some("aaaa"));
        assert_eq!(answer_reads(&mut slots, 2, &mut from_hub, &dir), 1);
        let turns = transcript::parse(jsonl);
        let want = transcript::render_full(transcript::last_prompts(&turns, 2));
        match answered.try_recv() {
            Ok(Prepared::Transcript(reply)) => {
                assert_eq!(reply.body, want);
                assert_eq!(reply.file_name, "full-aaaaaaaa.txt");
            }
            other => panic!("{other:?}"),
        }
        // A text in pieces is put together; an agent that sends more than
        // it may is cut off.
        let mut answered = brief(&mut slots, View::Brief, 1, None);
        let Ok(HubMsg::SessionRead { read_id, ask, .. }) = from_hub.try_recv() else {
            panic!("a read");
        };
        assert_eq!(
            ask,
            SessionAsk::Render {
                view: crate::wire::TranscriptView::Brief,
                prompts: 1
            }
        );
        for (text, more) in [("> one\n", true), ("two", false)] {
            slots.on_agent(AgentEvent::Message {
                conn: 2,
                received_at: StdInstant::now(),
                msg: AgentMsg::SessionAnswer {
                    read_id,
                    answer: SessionAnswer::Text {
                        text: text.into(),
                        more,
                    },
                },
            });
        }
        match answered.try_recv() {
            Ok(Prepared::Transcript(reply)) => assert_eq!(reply.body, "> one\ntwo"),
            other => panic!("{other:?}"),
        }
        let mut answered = brief(&mut slots, View::Brief, 1, None);
        let Ok(HubMsg::SessionRead { read_id, .. }) = from_hub.try_recv() else {
            panic!("a read");
        };
        let piece = "x".repeat(crate::reads::PIECE);
        for _ in 0..=(MAX_READ_TEXT / crate::reads::PIECE) {
            slots.on_agent(AgentEvent::Message {
                conn: 2,
                received_at: StdInstant::now(),
                msg: AgentMsg::SessionAnswer {
                    read_id,
                    answer: SessionAnswer::Text {
                        text: piece.clone(),
                        more: true,
                    },
                },
            });
        }
        assert!(notice(&mut answered).contains("слишком большой"));
        assert!(slots.reads.is_empty());
        // The agent found no file.
        let mut answered = brief(&mut slots, View::Brief, 1, None);
        let Ok(HubMsg::SessionRead { read_id, .. }) = from_hub.try_recv() else {
            panic!("a read");
        };
        slots.on_agent(AgentEvent::Message {
            conn: 2,
            received_at: StdInstant::now(),
            msg: AgentMsg::SessionAnswer {
                read_id,
                answer: SessionAnswer::Missing,
            },
        });
        assert!(notice(&mut answered).contains("не найден"));
        // A nested run and an ended session: no agent to ask.
        slots.on_hook(&hook(
            B,
            HookEvent::SessionStart {
                source: Some("startup".into()),
                claude_pid: Some(20),
                parent_claude_pid: Some(10),
            },
        ));
        assert!(
            notice(&mut brief(&mut slots, View::Brief, 3, Some("bbbb")))
                .contains("вложенный запуск")
        );
        slots.on_hook(&hook(
            A,
            HookEvent::SessionEnd {
                reason: None,
                claude_pid: None,
            },
        ));
        let ended = notice(&mut brief(&mut slots, View::Brief, 3, Some("aaaa")));
        assert!(ended.ends_with(&format!("claude --resume {A}")), "{ended}");
        assert!(from_hub.try_recv().is_err(), "nothing more was asked");
    }

    #[tokio::test]
    async fn a_brief_read_out_when_its_agent_leaves_or_is_late_gets_a_notice() {
        use commands::View;
        let dir = TempDir::new("slots-brief-lost");
        let transcript = parent_file(&dir, A, "");
        let options = Options {
            read_wait: Duration::from_millis(50),
            ..options()
        };
        let mut slots = stalled_slots(&dir, options);
        let _from_hub = connect_reader(&mut slots, 1, A);
        slots.on_hook(&start_in(A, 10, &transcript));
        let mut late = brief(&mut slots, View::Brief, 1, None);
        tokio::time::sleep(Duration::from_millis(80)).await;
        slots.on_tick();
        assert!(notice(&mut late).contains("не ответил"));
        // The agent leaves to hand over to a newer binary (TASK-040): its
        // read fails at once.
        let mut leaving = brief(&mut slots, View::Brief, 1, None);
        slots.on_agent(AgentEvent::Message {
            conn: 1,
            received_at: StdInstant::now(),
            msg: AgentMsg::UpdateAnswer {
                update_id: 1,
                outcome: UpdateOutcome::Reloading,
            },
        });
        assert!(notice(&mut leaving).contains("прервалась"));
        // The next worker's link closes with a read out.
        let _next = connect_reader(&mut slots, 2, A);
        let mut lost = brief(&mut slots, View::Brief, 1, None);
        slots.on_agent(AgentEvent::Disconnected { conn: 2 });
        assert!(notice(&mut lost).contains("прервалась"));
        assert!(slots.reads.is_empty());
    }

    #[tokio::test]
    async fn a_long_answer_keeps_its_read_alive_piece_by_piece() {
        use commands::View;
        let dir = TempDir::new("slots-brief-pieces");
        let transcript = parent_file(&dir, A, "");
        let options = Options {
            read_wait: Duration::from_millis(1000),
            ..options()
        };
        let mut slots = stalled_slots(&dir, options);
        let mut from_hub = connect_reader(&mut slots, 1, A);
        slots.on_hook(&start_in(A, 10, &transcript));
        let mut answered = brief(&mut slots, View::Brief, 1, None);
        let Ok(HubMsg::SessionRead { read_id, .. }) = from_hub.try_recv() else {
            panic!("a read");
        };
        let piece = |text: &str, more| AgentEvent::Message {
            conn: 1,
            received_at: StdInstant::now(),
            msg: AgentMsg::SessionAnswer {
                read_id,
                answer: SessionAnswer::Text {
                    text: text.into(),
                    more,
                },
            },
        };
        // Each piece comes within the wait, the whole read takes longer.
        for text in ["> a\n", "> b\n"] {
            tokio::time::sleep(Duration::from_millis(600)).await;
            slots.on_agent(piece(text, true));
            slots.on_tick();
        }
        tokio::time::sleep(Duration::from_millis(600)).await;
        slots.on_tick();
        // Another agent's link cannot answer this read.
        let _other = connect_reader(&mut slots, 3, B);
        let mut forged = piece("forged", false);
        if let AgentEvent::Message { conn, .. } = &mut forged {
            *conn = 3;
        }
        slots.on_agent(forged);
        assert!(answered.try_recv().is_err(), "another link answered");
        slots.on_agent(piece("c", false));
        match answered.try_recv() {
            Ok(Prepared::Transcript(reply)) => assert_eq!(reply.body, "> a\n> b\nc"),
            other => panic!("{other:?}"),
        }
    }

    #[tokio::test]
    async fn an_answer_left_from_an_earlier_hub_run_answers_no_read() {
        use commands::View;
        // An agent keeps answers it could not write in its outbox and sends
        // them after the next registration, possibly to a restarted hub.
        let mut earlier = Vec::new();
        for _run in 0..2 {
            let dir = TempDir::new("slots-read-ids");
            let transcript = parent_file(&dir, A, "");
            let mut slots = stalled_slots(&dir, options());
            let mut from_hub = connect_reader(&mut slots, 1, A);
            slots.on_hook(&start_in(A, 10, &transcript));
            let mut answered = brief(&mut slots, View::Brief, 1, None);
            let Ok(HubMsg::SessionRead { read_id, .. }) = from_hub.try_recv() else {
                panic!("a read");
            };
            for stale in &earlier {
                slots.on_agent(AgentEvent::Message {
                    conn: 1,
                    received_at: StdInstant::now(),
                    msg: AgentMsg::SessionAnswer {
                        read_id: *stale,
                        answer: SessionAnswer::Text {
                            text: "stale".into(),
                            more: false,
                        },
                    },
                });
            }
            assert!(answered.try_recv().is_err(), "a stale answer took the read");
            earlier.push(read_id);
        }
    }

    #[tokio::test]
    async fn a_session_read_fails_on_timeout_and_on_a_lost_or_leaving_link() {
        let dir = TempDir::new("slots-read-fail");
        let options = Options {
            read_wait: Duration::from_millis(50),
            ..subagent_options()
        };
        let mut slots = stalled_slots(&dir, options);
        let mut from_hub = connect_reader(&mut slots, 1, A);
        slots.on_hook(&start(A, 10));
        slots
            .registry
            .confirm_subagent(S1, A, format!("↳ Explore {S1}"));
        let gone = agent_file(&dir, A, S1, None, None);
        // Nobody answers: past the wait it is asked once more, then the
        // block gets the stop's text.
        slots.on_hook(&sub_stop(A, S1, "Explore", &gone, "Timed out."));
        for _ in 0..2 {
            assert!(matches!(
                from_hub.try_recv(),
                Ok(HubMsg::SessionRead { .. })
            ));
            tokio::time::sleep(Duration::from_millis(80)).await;
            slots.on_tick();
        }
        assert!(slots.reads.is_empty());
        assert!(from_hub.try_recv().is_err(), "asked a third time");
        assert_eq!(
            slots.registry.subagents[S1].block.pending.as_deref(),
            Some(format!("↳ Explore {S1}\nTimed out.").as_str())
        );
        // A read out when the link closes fails at once and waits for the
        // next agent, which is asked again.
        slots.on_hook(&sub_stop(A, S1, "Explore", &gone, "Link lost."));
        assert_eq!(slots.reads.len(), 1);
        slots.on_agent(AgentEvent::Disconnected { conn: 1 });
        assert!(slots.reads.is_empty());
        slots.on_tick();
        assert!(slots.bodies_parked.contains_key(S1));
        let mut next = connect_reader(&mut slots, 2, A);
        slots.on_tick();
        assert_eq!(answer_reads(&mut slots, 2, &mut next, &dir), 1);
        assert_eq!(
            slots.registry.subagents[S1].block.pending.as_deref(),
            Some(format!("↳ Explore {S1}\nLink lost.").as_str())
        );
        // A late answer of the closed link is dropped.
        let before = slots.registry.subagents[S1].block.pending.clone();
        slots.on_agent(AgentEvent::Message {
            conn: 1,
            received_at: StdInstant::now(),
            msg: AgentMsg::SessionAnswer {
                read_id: 2,
                answer: SessionAnswer::Text {
                    text: "stale".into(),
                    more: false,
                },
            },
        });
        assert_eq!(slots.registry.subagents[S1].block.pending, before);
    }

    #[tokio::test]
    async fn a_block_read_cut_by_a_worker_swap_is_read_by_the_next_agent() {
        let dir = TempDir::new("slots-body-retry");
        let mut slots = stalled_slots(&dir, subagent_options());
        let _first = connect_reader(&mut slots, 1, A);
        slots.on_hook(&start(A, 10));
        slots
            .registry
            .confirm_subagent(S1, A, format!("↳ Explore {S1}"));
        let path = agent_file(&dir, A, S1, Some(SUBAGENT_JSONL), Some(SUBAGENT_META));
        slots.on_hook(&sub_stop(A, S1, "Explore", &path, "Report handed back."));
        assert_eq!(slots.reads.len(), 1);
        // The agent leaves for a newer binary (TASK-040): the read is cut and
        // the block waits instead of settling for the stop's text.
        slots.on_agent(AgentEvent::Message {
            conn: 1,
            received_at: StdInstant::now(),
            msg: AgentMsg::UpdateAnswer {
                update_id: 1,
                outcome: UpdateOutcome::Reloading,
            },
        });
        slots.on_tick();
        assert!(slots.bodies_parked.contains_key(S1));
        let running = slots.registry.subagents[S1].block.pending.clone();
        assert!(
            running
                .as_deref()
                .is_none_or(|text| text.ends_with("в работе…")),
            "{running:?}"
        );
        // The next worker reads the subagent's files.
        let mut next = connect_reader(&mut slots, 2, A);
        slots.on_tick();
        assert_eq!(answer_reads(&mut slots, 2, &mut next, &dir), 1);
        let text = slots.registry.subagents[S1].block.pending.clone().unwrap();
        assert!(
            text.contains("• Bash: List source files") && text.ends_with("Report handed back."),
            "{text}"
        );
        // Only once: a read cut again settles for the stop's text.
        slots.on_hook(&sub_stop(A, S1, "Explore", &path, "Again."));
        assert_eq!(slots.reads.len(), 1);
        slots.on_agent(AgentEvent::Disconnected { conn: 2 });
        let _third = connect_reader(&mut slots, 3, A);
        slots.on_tick();
        assert_eq!(slots.reads.len(), 1, "asked of the third agent");
        slots.on_agent(AgentEvent::Disconnected { conn: 3 });
        assert!(slots.bodies_parked.is_empty());
        let text = slots.registry.subagents[S1].block.pending.clone().unwrap();
        assert!(
            text.ends_with("\nAgain.") && !text.contains("• Bash"),
            "{text}"
        );
    }

    #[tokio::test]
    async fn a_block_waiting_for_the_next_agent_of_an_ended_session_gets_the_stop_text() {
        let dir = TempDir::new("slots-body-retry-ended");
        let mut slots = stalled_slots(&dir, subagent_options());
        let _first = connect_reader(&mut slots, 1, A);
        slots.on_hook(&start(A, 10));
        slots
            .registry
            .confirm_subagent(S1, A, format!("↳ Explore {S1}"));
        let path = agent_file(&dir, A, S1, Some(SUBAGENT_JSONL), None);
        slots.on_hook(&sub_stop(A, S1, "Explore", &path, "Stopped."));
        slots.on_agent(AgentEvent::Disconnected { conn: 1 });
        assert!(slots.bodies_parked.contains_key(S1));
        slots.on_hook(&hook(
            A,
            HookEvent::SessionEnd {
                reason: None,
                claude_pid: None,
            },
        ));
        slots.on_tick();
        assert!(slots.bodies_parked.is_empty());
        let block = &slots.registry.subagents[S1].block;
        assert!(
            block
                .pending
                .as_deref()
                .is_some_and(|text| text.ends_with("Stopped.") && !text.contains("• Bash")),
            "{block:?}"
        );
    }

    /// An agent that reads but finds no parent transcript (`missing`: its
    /// folder is not the transcript's, or Claude Code keeps none) or serves
    /// nothing (`refused`): a subagent's block still opens on its stop, as
    /// without an agent that reads.
    #[tokio::test]
    async fn an_agent_that_cannot_give_the_calls_still_gets_a_block_on_the_stop() {
        for (name, folder) in [("missing", Some("C--not-mine")), ("refused", None)] {
            let rig = rig(Fake::default(), subagent_options());
            let parent = parent_file(
                &rig.dir,
                A,
                &(call_line("t1", "one") + &result_line("t1", S1)),
            );
            let (to_agent, mut from_hub) = mpsc::channel(16);
            let agents = rig.agents.clone();
            let own =
                folder.map(|folder| crate::tail::OwnProject::at(projects(&rig.dir).join(folder)));
            tokio::spawn(async move {
                while let Some(msg) = from_hub.recv().await {
                    if let HubMsg::SessionRead {
                        read_id,
                        session_id,
                        ask,
                    } = msg
                    {
                        for answer in crate::reads::answer(own.as_ref(), &session_id, ask) {
                            let _ = agents
                                .send(AgentEvent::Message {
                                    conn: 1,
                                    received_at: StdInstant::now(),
                                    msg: AgentMsg::SessionAnswer { read_id, answer },
                                })
                                .await;
                        }
                    }
                }
            });
            rig.agents
                .send(AgentEvent::Registered {
                    conn: 1,
                    register: reads_register(A, Some(10)),
                    to_agent,
                })
                .await
                .unwrap();
            rig.hook(start_in(A, 10, &parent)).await;
            rig.ops_after(1).await;
            bound(&rig).await;
            let path = agent_file(&rig.dir, A, S1, Some(SUBAGENT_JSONL), None);
            rig.hook(sub_start(A, S1, "Explore")).await;
            rig.hook(sub_stop(A, S1, "Explore", &path, "Done.")).await;
            let ops = settled(&rig, |ops| !block_of(&shown(ops), S1).is_empty()).await;
            let texts = shown(&ops);
            let block = block_of(&texts, S1);
            assert_eq!(block.len(), 1, "{name}: {texts:?}");
            assert!(block[0].1.ends_with("\nDone."), "{name}: {}", block[0].1);
        }
    }

    #[tokio::test]
    async fn a_title_that_cannot_be_read_is_asked_of_one_agent_only_a_few_times() {
        let dir = TempDir::new("slots-title-backoff");
        // No transcript in the agents' folder: every title read is missing.
        let transcript = projects(&dir).join("C--w").join(format!("{A}.jsonl"));
        let mut slots = stalled_slots(&dir, options());
        let mut first = connect_reader(&mut slots, 1, A);
        slots.on_hook(&start_in(A, 10, &transcript));
        let prompt = || hook(A, HookEvent::UserPromptSubmit { prompt_id: None });
        let mut asked = 0;
        for _ in 0..(MAX_TITLE_FAILURES + 3) {
            slots.on_hook(&prompt());
            asked += answer_reads(&mut slots, 1, &mut first, &dir);
        }
        assert_eq!(asked, MAX_TITLE_FAILURES as usize);
        // The session's next agent is asked again.
        slots.on_agent(AgentEvent::Disconnected { conn: 1 });
        let mut next = connect_reader(&mut slots, 2, A);
        slots.on_hook(&prompt());
        assert_eq!(answer_reads(&mut slots, 2, &mut next, &dir), 1);
        // A title found clears the count.
        parent_file(&dir, A, "{\"type\":\"ai-title\",\"aiTitle\":\"Found\"}\n");
        slots.on_hook(&prompt());
        assert_eq!(answer_reads(&mut slots, 2, &mut next, &dir), 1);
        assert!(!slots.title_failures.contains_key(A));
    }

    #[tokio::test]
    async fn a_deleted_topic_is_replaced_once() {
        let fake = Fake {
            edit_errors: Mutex::new(vec!["Bad Request: TOPIC_ID_INVALID"]),
            ..Fake::default()
        };
        let mut rig = rig(fake, options());
        rig.hook(start(A, 10)).await;
        rig.hook(start(B, 11)).await;
        rig.ops_after(2).await;
        rig.agent(1, A).await; // edit of topic 100 fails: gone
        let ops = rig.ops_after(4).await;
        assert_eq!(ops.len(), 4, "{ops:?}");
        assert!(
            matches!(&ops[3], Op::CreateTopic { name, icon_custom_emoji_id }
            if name == "[box] Project · aaaaaaaa" && icon_custom_emoji_id.as_deref() == Some(ICON_ALIVE))
        );
        // Topic 101 of the other slot was never touched.
        assert!(
            !ops.iter()
                .any(|op| matches!(op, Op::EditTopic { thread_id: 101, .. }))
        );
    }

    #[tokio::test]
    async fn topic_not_modified_counts_as_applied() {
        let fake = Fake {
            edit_errors: Mutex::new(vec!["Bad Request: TOPIC_NOT_MODIFIED"]),
            ..Fake::default()
        };
        // A short retry period: a failure would be retried within the test.
        let mut rig = rig(
            fake,
            Options {
                retry_every: Duration::from_millis(100),
                ..options()
            },
        );
        rig.hook(start(A, 10)).await;
        rig.ops_after(1).await;
        rig.agent(1, A).await;
        rig.ops_after(2).await;
        tokio::time::sleep(Duration::from_millis(400)).await;
        let ops = rig.fake.ops();
        assert_eq!(ops.len(), 2, "applied: no retry, no replacement: {ops:?}");
    }

    #[tokio::test]
    async fn without_the_delete_right_nothing_is_deleted() {
        let rig = rig(
            Fake::default(),
            Options {
                can_delete: false,
                ..options()
            },
        );
        rig.hook(start(A, 10)).await;
        rig.ops_after(1).await;
        rig.control
            .send(Control::TopicEdited {
                thread_id: Some(100),
                message_id: 5,
            })
            .unwrap();
        let ops = rig.ops_after(1).await;
        assert_eq!(ops.len(), 1);
    }

    #[tokio::test]
    async fn a_restart_keeps_slots_and_waits_out_the_grace() {
        // What the previous run saved: a slot whose session was alive.
        let dir = TempDir::new("slots-restart");
        let store = RegistryStore::open(dir.path()).unwrap();
        let mut registry = Registry::default();
        registry.apply_hook(&start(A, 10));
        registry.agent_connected(A, 1);
        for job in registry.topic_work(&Icons::default(), true) {
            if let TopicJob::Create { slot, name, icon } = job {
                registry.topic_created(slot, 100, &name, icon.as_deref());
            }
        }
        store.save(&RegistryStore::encode(&registry)).unwrap();

        let mut rig = rig_in(
            Fake::default(),
            Options {
                grace: Duration::from_millis(400),
                ..options()
            },
            dir,
        );
        // The agent reconnects within the grace: no edit at all.
        rig.agent(7, A).await;
        tokio::time::sleep(Duration::from_millis(700)).await;
        assert!(rig.fake.ops().is_empty(), "{:?}", rig.fake.ops());
        // A new concurrent session still gets its topic.
        rig.hook(start(B, 11)).await;
        let ops = rig.ops_after(1).await;
        assert_eq!(count(&ops, is_create), 1);
        assert!(
            matches!(&ops[0], Op::CreateTopic { name, .. } if name == "[box] Project #2 · bbbbbbbb")
        );
    }

    #[tokio::test]
    async fn a_stopped_scheduler_is_retried_on_the_tick_not_in_a_loop() {
        let dir = TempDir::new("slots-stopped");
        let store = RegistryStore::open(dir.path()).unwrap();
        let (scheduler, outbox) =
            Scheduler::new(Arc::new(Fake::default()), BucketConfig::default());
        drop(scheduler); // every submit now comes back without an answer
        let mut slots = Slots::new(Registry::default(), store, outbox, options());
        let mut done = slots.done_rx.take().unwrap();
        slots.on_hook(&start(A, 10));
        slots.pump();
        let answer = tokio::time::timeout(WAIT, done.recv()).await.unwrap();
        assert!(
            matches!(answer, Some(Done::Topic { delivery: None, .. })),
            "{answer:?}"
        );
        slots.on_done(answer.unwrap());
        slots.pump();
        let again = tokio::time::timeout(Duration::from_millis(300), done.recv()).await;
        assert!(again.is_err(), "handed out again at once: {again:?}");
        // The retry tick hands it out once more.
        slots.next_retry = Instant::now();
        slots.on_tick();
        slots.pump();
        let retried = tokio::time::timeout(WAIT, done.recv()).await.unwrap();
        assert!(matches!(retried, Some(Done::Topic { delivery: None, .. })));
    }

    #[tokio::test]
    async fn a_title_scan_that_ends_after_session_end_keeps_nothing() {
        let dir = TempDir::new("slots-scan-end");
        let store = RegistryStore::open(dir.path()).unwrap();
        let (_scheduler, outbox) =
            Scheduler::new(Arc::new(Fake::default()), BucketConfig::default());
        let mut slots = Slots::new(Registry::default(), store, outbox, options());
        slots.on_hook(&start(A, 10));
        slots.on_hook(&hook(
            A,
            HookEvent::SessionEnd {
                reason: None,
                claude_pid: None,
            },
        ));
        slots.on_title(A.into(), "t.jsonl".into(), None, 10);
        assert!(slots.scanned.is_empty());
    }

    #[tokio::test]
    async fn a_title_less_transcript_is_scanned_only_past_the_last_scan() {
        let mut rig = rig(Fake::default(), options());
        let head = "{\"type\":\"user\",\"message\":{\"role\":\"user\",\"content\":\"hi\"}}\n";
        let transcript = parent_file(&rig.dir, A, head);
        let _agent = rig.files_agent(1, A, 10).await;
        rig.hook(start_in(A, 10, &transcript)).await;
        rig.ops_after(1).await;
        bound(&rig).await;
        let stop = || {
            hook(
                A,
                HookEvent::Stop {
                    prompt_id: None,
                    last_assistant_message: None,
                },
            )
        };
        rig.hook(stop()).await;
        tokio::time::sleep(Duration::from_millis(200)).await;
        // A title placed inside the part already scanned is never seen again:
        // proof that the next scan starts past it. One appended later is.
        let mut hidden = String::from("{\"type\":\"ai-title\",\"aiTitle\":\"Old\"}");
        hidden.push_str(&" ".repeat(head.len() - hidden.len() - 1));
        hidden.push('\n');
        assert_eq!(hidden.len(), head.len());
        std::fs::write(
            &transcript,
            format!("{hidden}{{\"type\":\"ai-title\",\"aiTitle\":\"New\"}}\n"),
        )
        .unwrap();
        rig.hook(stop()).await;
        let ops = rig.ops_after(2).await;
        assert!(
            matches!(ops.last().unwrap(), Op::EditTopic { name: Some(name), .. }
            if name == "[box] Project · New"),
            "{ops:?}"
        );
    }

    #[tokio::test]
    async fn the_ai_title_replaces_the_short_id() {
        let mut rig = rig(Fake::default(), options());
        let transcript = parent_file(
            &rig.dir,
            A,
            "{\"type\":\"user\",\"message\":{\"role\":\"user\",\"content\":\"hi\"}}\n{\"type\":\"ai-title\",\"aiTitle\":\"Slot registry\"}\n",
        );
        rig.hook(start_in(A, 10, &transcript)).await;
        rig.ops_after(1).await;
        let _agent = rig.files_agent(1, A, 10).await;
        rig.ops_after(2).await; // the alive icon
        rig.hook(hook(
            A,
            HookEvent::Stop {
                prompt_id: None,
                last_assistant_message: None,
            },
        ))
        .await;
        let ops = rig.ops_after(3).await;
        assert_eq!(ops.len(), 3, "{ops:?}");
        assert!(
            matches!(ops.last().unwrap(), Op::EditTopic { name: Some(name), icon_custom_emoji_id: None, .. }
            if name == "[box] Project · Slot registry"),
            "{ops:?}"
        );
    }

    #[tokio::test]
    async fn without_an_agent_that_reads_the_title_stays_the_short_id() {
        let mut rig = rig(Fake::default(), options());
        let transcript = parent_file(
            &rig.dir,
            A,
            "{\"type\":\"ai-title\",\"aiTitle\":\"Slot registry\"}\n",
        );
        // An agent before TASK-034: it reads no files.
        rig.agent_of(1, A, Some(10)).await;
        rig.hook(start_in(A, 10, &transcript)).await;
        rig.ops_after(1).await;
        rig.hook(hook(
            A,
            HookEvent::Stop {
                prompt_id: None,
                last_assistant_message: None,
            },
        ))
        .await;
        tokio::time::sleep(Duration::from_millis(300)).await;
        assert!(
            !rig.fake
                .ops()
                .iter()
                .any(|op| matches!(op, Op::EditTopic { name: Some(_), .. })),
            "{:?}",
            rig.fake.ops()
        );
        assert!(received(&mut rig, 0).await.is_empty(), "never asked");
    }

    fn sends(ops: &[Op]) -> Vec<&str> {
        ops.iter()
            .filter_map(|op| match op {
                Op::Send { text, .. } => Some(text.as_str()),
                _ => None,
            })
            .collect()
    }

    #[tokio::test]
    async fn unknown_sessions_make_no_topics_until_their_session_start() {
        let mut rig = rig(Fake::default(), options());
        // Prompt hooks and an agent of a session no SessionStart announced.
        for event in [
            HookEvent::UserPromptSubmit { prompt_id: None },
            HookEvent::Stop {
                prompt_id: None,
                last_assistant_message: None,
            },
        ] {
            rig.hook(hook(A, event)).await;
        }
        rig.agent(1, A).await;
        tokio::time::sleep(Duration::from_millis(600)).await;
        assert!(rig.fake.ops().is_empty(), "{:?}", rig.fake.ops());
        // Its SessionStart proves top-level: one topic, the agent bound.
        rig.hook(start(A, 10)).await;
        let ops = rig.ops_after(1).await;
        assert_eq!(ops.len(), 1, "{ops:?}");
        assert!(
            matches!(&ops[0], Op::CreateTopic { icon_custom_emoji_id, .. }
            if icon_custom_emoji_id.as_deref() == Some(ICON_ALIVE))
        );
    }

    #[tokio::test]
    async fn a_stalled_telegram_never_stalls_ingress() {
        let fake = Fake {
            stall: true,
            ..Fake::default()
        };
        let rig = rig(fake, options());
        // More topics than the scheduler queue holds (1024) plus the hook
        // channel (16): an actor that waits for the queue stops reading.
        const SESSIONS: u32 = 1100;
        let sent = async {
            for i in 0..SESSIONS {
                let mut post = start(&format!("s{i:05}"), 1000 + i);
                post.cwd = format!(r"C:\Work\P{i}");
                rig.hook(post).await;
            }
        };
        tokio::time::timeout(WAIT, sent)
            .await
            .expect("ingress kept draining while Telegram stalled");
        let saved = async {
            loop {
                let store = RegistryStore::open(rig.dir.path()).unwrap();
                if store
                    .load()
                    .is_ok_and(|saved| saved.slots.len() == SESSIONS as usize)
                {
                    break;
                }
                tokio::time::sleep(Duration::from_millis(50)).await;
            }
        };
        tokio::time::timeout(WAIT, saved)
            .await
            .expect("every session registered");
    }

    #[tokio::test]
    async fn a_failed_separator_is_sent_again() {
        let fake = Fake {
            send_errors: Mutex::new(vec!["Bad Request: something else"]),
            ..Fake::default()
        };
        let rig = rig(
            fake,
            Options {
                retry_every: Duration::from_millis(100),
                ..options()
            },
        );
        rig.hook(start(A, 10)).await;
        rig.ops_after(1).await;
        rig.hook(hook(
            A,
            HookEvent::SessionEnd {
                reason: None,
                claude_pid: None,
            },
        ))
        .await;
        rig.hook(start(B, 11)).await;
        let separator = "── session bbbbbbbb · new ──";
        let delivered = async {
            while sends(&rig.fake.ops()).len() < 2 {
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        };
        tokio::time::timeout(WAIT, delivered)
            .await
            .expect("the separator is tried again");
        tokio::time::sleep(Duration::from_millis(400)).await;
        let ops = rig.fake.ops();
        assert_eq!(sends(&ops), [separator, separator], "{ops:?}");
    }

    #[tokio::test]
    async fn a_gone_topic_during_a_session_change_is_replaced_once() {
        let gone = "Bad Request: TOPIC_ID_INVALID";
        let fake = Fake {
            edit_errors: Mutex::new(vec![gone]),
            send_errors: Mutex::new(vec![gone]),
            ..Fake::default()
        };
        let rig = rig(fake, options());
        rig.hook(start(A, 10)).await;
        rig.ops_after(1).await;
        // `/clear` in A's process: one event changes the session (separator)
        // and the title (edit) of the slot whose topic was deleted.
        let mut clear = start(B, 10);
        clear.event = HookEvent::SessionStart {
            source: Some("clear".into()),
            claude_pid: Some(10),
            parent_claude_pid: None,
        };
        rig.hook(clear).await;
        let ops = rig.ops_after(3).await;
        tokio::time::sleep(Duration::from_millis(300)).await;
        let ops = if ops.len() < rig.fake.ops().len() {
            rig.fake.ops()
        } else {
            ops
        };
        assert_eq!(count(&ops, is_create), 2, "one replacement: {ops:?}");
        let replacement = ops.iter().rposition(is_create).unwrap();
        assert!(
            !ops[replacement..].iter().any(|op| matches!(
                op,
                Op::EditTopic { thread_id: 100, .. }
                    | Op::Send {
                        thread_id: Some(100),
                        ..
                    }
            )),
            "{ops:?}"
        );
    }

    // ---- Live transcript stream (TASK-016) ----

    const FAST: BucketConfig = BucketConfig {
        capacity: 1000,
        refill_every: Duration::from_millis(1),
        min_gap: Duration::ZERO,
    };

    fn stream_options() -> Options {
        Options {
            chat_id: CHAT,
            stream_every: Duration::from_millis(20),
            hold_answer: Duration::from_millis(400),
            ..options()
        }
    }

    /// A rig with a scheduler that never makes the stream wait.
    fn stream_rig(fake: Fake, options: Options, dir: TempDir) -> Rig {
        let fake = Arc::new(fake);
        let store = RegistryStore::open(dir.path()).unwrap();
        let registry = store.load().unwrap();
        let (scheduler, outbox) = Scheduler::new(fake.clone(), FAST);
        tokio::spawn(scheduler.run());
        let mut slots = Slots::new(registry, store, outbox, options);
        let asks = slots.permission_asks();
        let questions = slots.question_asks();
        let (agents, agents_rx) = mpsc::channel(16);
        let (hooks, hooks_rx) = mpsc::channel(16);
        let (control, control_rx) = mpsc::unbounded_channel();
        tokio::spawn(slots.run(agents_rx, hooks_rx, control_rx));
        Rig {
            fake,
            agents,
            hooks,
            control,
            dir,
            _to_agent: Vec::new(),
            asks,
            questions,
        }
    }

    /// `<dir>/projects/C--w/<session>.jsonl`, created empty.
    fn transcript_file(dir: &TempDir, session: &str) -> String {
        let project = dir.path().join("projects").join("C--w");
        std::fs::create_dir_all(&project).unwrap();
        let path = project.join(format!("{session}.jsonl"));
        std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(&path)
            .unwrap();
        path.to_string_lossy().into_owned()
    }

    fn append(path: &str, text: &str) {
        use std::io::Write;
        let mut file = std::fs::OpenOptions::new().append(true).open(path).unwrap();
        file.write_all(text.as_bytes()).unwrap();
    }

    fn typed(text: &str) -> String {
        format!("{{\"type\":\"user\",\"message\":{{\"role\":\"user\",\"content\":\"{text}\"}}}}\n")
    }

    fn channel_record(message_id: i64) -> String {
        format!(
            "{{\"type\":\"user\",\"isMeta\":true,\"message\":{{\"role\":\"user\",\"content\":\"<channel source=\\\"cctg\\\" message_id=\\\"{message_id}\\\">hi</channel>\"}}}}\n"
        )
    }

    fn tool_call(id: &str, description: &str) -> String {
        format!(
            "{{\"type\":\"assistant\",\"message\":{{\"role\":\"assistant\",\"stop_reason\":\"tool_use\",\"content\":[{{\"type\":\"tool_use\",\"id\":\"{id}\",\"name\":\"Bash\",\"input\":{{\"command\":\"x\",\"description\":\"{description}\"}}}}]}}}}\n"
        )
    }

    fn tool_result(id: &str, error: Option<&str>) -> String {
        format!(
            "{{\"type\":\"user\",\"message\":{{\"role\":\"user\",\"content\":[{{\"type\":\"tool_result\",\"tool_use_id\":\"{id}\",\"content\":\"{}\",\"is_error\":{}}}]}}}}\n",
            error.unwrap_or("ok"),
            error.is_some()
        )
    }

    fn start_with(session: &str, pid: u32, path: &str, source: &str) -> HookPost {
        HookPost::new(
            "box".into(),
            session.into(),
            CWD.into(),
            path.into(),
            HookEvent::SessionStart {
                source: Some(source.into()),
                claude_pid: Some(pid),
                parent_claude_pid: None,
            },
        )
    }

    impl Rig {
        /// An agent that answers transcript reads from the real file, like
        /// `cctg agent` does, and keeps what else the hub sends it.
        async fn reader(
            &mut self,
            conn: u64,
            session: &str,
            pid: u32,
        ) -> mpsc::UnboundedReceiver<HubMsg> {
            let gate = Arc::new(ReadGate::default());
            self.gated_reader(conn, session, pid, gate).await
        }

        /// Like [`Self::reader`] until `gate.stopped` is set; then it
        /// leaves the next read unanswered and sets `gate.parked`.
        async fn gated_reader(
            &mut self,
            conn: u64,
            session: &str,
            pid: u32,
            gate: Arc<ReadGate>,
        ) -> mpsc::UnboundedReceiver<HubMsg> {
            let (to_agent, mut from_hub) = mpsc::channel(16);
            let (kept, kept_rx) = mpsc::unbounded_channel();
            let agents = self.agents.clone();
            let root = crate::tail::OwnProject::at(self.dir.path().join("projects").join("C--w"));
            tokio::spawn(async move {
                while let Some(msg) = from_hub.recv().await {
                    let HubMsg::TranscriptRead {
                        session_id, from, ..
                    } = msg
                    else {
                        let _ = kept.send(msg);
                        continue;
                    };
                    if gate.stopped.load(Ordering::SeqCst) {
                        gate.parked.store(true, Ordering::SeqCst);
                        continue;
                    }
                    let chunk = crate::tail::read_chunk(Some(&root), &session_id, from);
                    let event = AgentEvent::Message {
                        conn,
                        received_at: StdInstant::now(),
                        msg: chunk,
                    };
                    if agents.send(event).await.is_err() {
                        return;
                    }
                }
            });
            let register = Register {
                session_id: session.into(),
                host: "box".into(),
                cwd: CWD.into(),
                claude_pid: Some(pid),
                verdict_ack: true,
                transcript_reads: true,
                console_keys: false,
                console_commands: false,
                client: None,
                files: false,
                session_reads: false,
                status_lines: false,
                heartbeat: false,
            };
            self.agents
                .send(AgentEvent::Registered {
                    conn,
                    register,
                    to_agent,
                })
                .await
                .unwrap();
            kept_rx
        }
    }

    #[derive(Default)]
    struct ReadGate {
        stopped: AtomicBool,
        parked: AtomicBool,
    }

    /// Texts of new messages in `thread`, in order: sends and stream lines.
    fn topic_texts(ops: &[Op], thread: i64) -> Vec<String> {
        ops.iter()
            .filter_map(|op| match op {
                Op::Send {
                    thread_id: Some(t),
                    text,
                    ..
                }
                | Op::Stream {
                    thread_id: t, text, ..
                } if *t == thread => Some(text.clone()),
                _ => None,
            })
            .collect()
    }

    fn reactions(ops: &[Op]) -> Vec<(i64, String)> {
        ops.iter()
            .filter_map(|op| match op {
                Op::React { message_id, emoji } => Some((*message_id, emoji.clone())),
                _ => None,
            })
            .collect()
    }

    async fn stream_texts(rig: &Rig, thread: i64, want: usize) -> Vec<String> {
        let ops = settled(rig, |ops| topic_texts(ops, thread).len() >= want).await;
        tokio::time::sleep(Duration::from_millis(200)).await;
        let _ = ops;
        topic_texts(&rig.fake.ops(), thread)
    }

    #[tokio::test]
    async fn appended_lines_reach_the_slot_topic_in_order_and_a_partial_line_waits() {
        let dir = TempDir::new("slots-stream-order");
        let path = transcript_file(&dir, A);
        let mut rig = stream_rig(Fake::default(), stream_options(), dir);
        rig.hook(start_with(A, 10, &path, "startup")).await;
        rig.ops_after(1).await;
        let _kept = rig.reader(1, A, 10).await;
        settled(&rig, |ops| last_icon(ops, 100) == Some(ICON_ALIVE)).await;

        let second = tool_call("t2", "two");
        append(&path, &typed("go"));
        append(&path, &tool_call("t1", "one"));
        append(&path, &tool_result("t1", None));
        append(&path, &second[..20]);
        let first = stream_texts(&rig, 100, 2).await;
        assert_eq!(first, ["> go", "• Bash: one ✓"]);

        // The rest of the cut line arrives: it is read whole, never lost.
        append(&path, &second[20..]);
        append(&path, &tool_result("t2", Some("boom")));
        let all = stream_texts(&rig, 100, 3).await;
        assert_eq!(all, ["> go", "• Bash: one ✓", "• Bash: two ✗ boom"]);
    }

    #[tokio::test]
    async fn a_restart_neither_repeats_nor_loses_stream_lines() {
        let dir = TempDir::new("slots-stream-restart");
        let path = transcript_file(&dir, A);
        let state = dir.path().to_path_buf();
        let mut rig = stream_rig(Fake::default(), stream_options(), dir);
        rig.hook(start_with(A, 10, &path, "startup")).await;
        rig.ops_after(1).await;
        let _kept = rig.reader(1, A, 10).await;
        append(&path, &tool_call("t1", "one"));
        append(&path, &tool_result("t1", None));
        assert_eq!(stream_texts(&rig, 100, 1).await, ["• Bash: one ✓"]);
        let saved = async {
            loop {
                let text = std::fs::read_to_string(state.join("registry.json")).unwrap_or_default();
                let offset = serde_json::from_str::<serde_json::Value>(&text)
                    .ok()
                    .and_then(|v| v["sessions"][A]["stream"]["offset"].as_u64());
                if offset == Some(std::fs::metadata(&path).unwrap().len()) {
                    return;
                }
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        };
        tokio::time::timeout(WAIT, saved)
            .await
            .expect("offset saved");

        // The hub goes away; the session writes on meanwhile.
        let Rig { dir, .. } = rig;
        append(&path, &tool_call("t2", "two"));
        append(&path, &tool_result("t2", None));
        let mut rig = stream_rig(Fake::default(), stream_options(), dir);
        let _kept = rig.reader(2, A, 10).await;
        let after = stream_texts(&rig, 100, 1).await;
        assert_eq!(after, ["• Bash: two ✓"]);
    }

    #[tokio::test]
    async fn a_new_session_in_the_slot_streams_after_its_one_separator() {
        let dir = TempDir::new("slots-stream-rotation");
        let first = transcript_file(&dir, A);
        let second = transcript_file(&dir, B);
        let mut rig = stream_rig(Fake::default(), stream_options(), dir);
        rig.hook(start_with(A, 10, &first, "startup")).await;
        rig.ops_after(1).await;
        let _a = rig.reader(1, A, 10).await;
        append(&first, &typed("from A"));
        assert_eq!(stream_texts(&rig, 100, 1).await, ["> from A"]);
        rig.hook(hook(
            A,
            HookEvent::SessionEnd {
                reason: None,
                claude_pid: Some(10),
            },
        ))
        .await;
        append(&second, &typed("from B"));
        rig.hook(start_with(B, 11, &second, "startup")).await;
        let _b = rig.reader(2, B, 11).await;
        let texts = stream_texts(&rig, 100, 3).await;
        assert_eq!(
            texts,
            ["> from A", "── session bbbbbbbb · new ──", "> from B"]
        );
        assert_eq!(count(&rig.fake.ops(), is_create), 1);
    }

    #[tokio::test]
    async fn eyes_on_hand_off_and_writing_only_for_the_same_messages_channel_record() {
        let dir = TempDir::new("slots-stream-reactions");
        let path = transcript_file(&dir, A);
        let mut rig = stream_rig(Fake::default(), stream_options(), dir);
        rig.hook(start_with(A, 10, &path, "startup")).await;
        rig.ops_after(1).await;
        let mut kept = rig.reader(1, A, 10).await;
        settled(&rig, |ops| last_icon(ops, 100) == Some(ICON_ALIVE)).await;
        rig.control.send(say(Some(100), 42, Some("hi"))).unwrap();
        settled(&rig, |ops| reactions(ops) == [(42, "👀".to_owned())]).await;
        assert!(matches!(kept.recv().await, Some(HubMsg::Inbound { .. })));

        // A prompt typed in the terminal, its UserPromptSubmit and the channel
        // record of a message this session never got change nothing.
        rig.hook(hook(A, HookEvent::UserPromptSubmit { prompt_id: None }))
            .await;
        append(&path, &typed("typed here"));
        append(&path, &channel_record(41));
        stream_texts(&rig, 100, 1).await;
        assert_eq!(reactions(&rig.fake.ops()), [(42, "👀".to_owned())]);

        append(&path, &channel_record(42));
        let ops = settled(&rig, |ops| reactions(ops).len() == 2).await;
        assert_eq!(
            reactions(&ops),
            [(42, "👀".to_owned()), (42, "✍".to_owned())]
        );
        // Its record again (a restart re-read) marks nothing twice.
        append(&path, &channel_record(42));
        append(&path, &typed("later"));
        stream_texts(&rig, 100, 2).await;
        assert_eq!(reactions(&rig.fake.ops()).len(), 2);
    }

    fn channel_off_options() -> Options {
        Options {
            channel_wait: Duration::from_millis(300),
            ..stream_options()
        }
    }

    fn channel_off_notices(ops: &[Op]) -> usize {
        topic_texts(ops, 100)
            .iter()
            .filter(|text| *text == CHANNEL_OFF_NOTICE)
            .count()
    }

    /// TASK-052: messages Claude Code drops (no channel) give the topic one
    /// notice, and another only after a message of the session was taken.
    #[tokio::test]
    async fn a_message_nobody_takes_tells_the_topic_once_until_one_is_taken() {
        let dir = TempDir::new("slots-channel-off");
        let path = transcript_file(&dir, A);
        let mut rig = stream_rig(Fake::default(), channel_off_options(), dir);
        rig.hook(start_with(A, 10, &path, "startup")).await;
        rig.ops_after(1).await;
        let mut kept = rig.reader(1, A, 10).await;
        settled(&rig, |ops| last_icon(ops, 100) == Some(ICON_ALIVE)).await;

        rig.control.send(say(Some(100), 42, Some("hi"))).unwrap();
        assert!(matches!(kept.recv().await, Some(HubMsg::Inbound { .. })));
        let asked = std::time::Instant::now();
        settled(&rig, |ops| channel_off_notices(ops) == 1).await;
        assert!(asked.elapsed() >= Duration::from_millis(250), "told early");

        // More messages that go nowhere: no second notice.
        rig.control.send(say(Some(100), 43, Some("again"))).unwrap();
        assert!(matches!(kept.recv().await, Some(HubMsg::Inbound { .. })));
        tokio::time::sleep(Duration::from_millis(900)).await;
        assert_eq!(channel_off_notices(&rig.fake.ops()), 1);

        // The channel works again (claude-cctg --continue): its record ends
        // the told period; the next message that goes nowhere is told again.
        append(&path, &channel_record(44));
        rig.control.send(say(Some(100), 44, Some("back"))).unwrap();
        assert!(matches!(kept.recv().await, Some(HubMsg::Inbound { .. })));
        settled(&rig, |ops| {
            reactions(ops).contains(&(44, stream::WORKING.to_owned()))
        })
        .await;
        tokio::time::sleep(Duration::from_millis(900)).await;
        assert_eq!(channel_off_notices(&rig.fake.ops()), 1);
        rig.control.send(say(Some(100), 45, Some("lost"))).unwrap();
        assert!(matches!(kept.recv().await, Some(HubMsg::Inbound { .. })));
        settled(&rig, |ops| channel_off_notices(ops) == 2).await;
    }

    /// TASK-052: a message handed during a turn waits in Claude Code's queue,
    /// and a turn or a channel record after a hand-over means it was taken.
    #[tokio::test]
    async fn a_message_during_a_turn_or_one_taken_tells_nothing() {
        let dir = TempDir::new("slots-channel-on");
        let path = transcript_file(&dir, A);
        let mut rig = stream_rig(Fake::default(), channel_off_options(), dir);
        rig.hook(start_with(A, 10, &path, "startup")).await;
        rig.ops_after(1).await;
        let mut kept = rig.reader(1, A, 10).await;
        settled(&rig, |ops| last_icon(ops, 100) == Some(ICON_ALIVE)).await;

        // A long turn runs: the message waits in its queue.
        rig.hook(hook(A, HookEvent::UserPromptSubmit { prompt_id: None }))
            .await;
        rig.control
            .send(say(Some(100), 50, Some("queued")))
            .unwrap();
        assert!(matches!(kept.recv().await, Some(HubMsg::Inbound { .. })));
        tokio::time::sleep(Duration::from_millis(900)).await;
        rig.hook(stop(A, None)).await;

        // Taken: its channel record shows up.
        rig.control.send(say(Some(100), 51, Some("taken"))).unwrap();
        assert!(matches!(kept.recv().await, Some(HubMsg::Inbound { .. })));
        append(&path, &channel_record(51));
        settled(&rig, |ops| {
            reactions(ops).contains(&(51, stream::WORKING.to_owned()))
        })
        .await;
        tokio::time::sleep(Duration::from_millis(900)).await;
        assert_eq!(channel_off_notices(&rig.fake.ops()), 0);

        // Taken: a turn starts (its UserPromptSubmit).
        rig.control.send(say(Some(100), 52, Some("turn"))).unwrap();
        assert!(matches!(kept.recv().await, Some(HubMsg::Inbound { .. })));
        rig.hook(hook(A, HookEvent::UserPromptSubmit { prompt_id: None }))
            .await;
        tokio::time::sleep(Duration::from_millis(900)).await;
        assert_eq!(channel_off_notices(&rig.fake.ops()), 0);
    }

    #[tokio::test]
    async fn a_refused_reaction_never_stops_routing() {
        let dir = TempDir::new("slots-stream-reaction-error");
        let fake = Fake {
            react_error: true,
            ..Fake::default()
        };
        let path = transcript_file(&dir, A);
        let mut rig = stream_rig(fake, stream_options(), dir);
        rig.hook(start_with(A, 10, &path, "startup")).await;
        rig.ops_after(1).await;
        let mut kept = rig.reader(1, A, 10).await;
        settled(&rig, |ops| last_icon(ops, 100) == Some(ICON_ALIVE)).await;
        for id in [1, 2] {
            rig.control.send(say(Some(100), id, Some("m"))).unwrap();
        }
        for _ in 0..2 {
            let got = tokio::time::timeout(WAIT, kept.recv()).await.unwrap();
            assert!(matches!(got, Some(HubMsg::Inbound { .. })), "{got:?}");
        }
        append(&path, &typed("still streaming"));
        assert_eq!(stream_texts(&rig, 100, 1).await, ["> still streaming"]);
    }

    #[tokio::test]
    async fn a_turn_answer_follows_the_lines_read_after_its_stop() {
        let dir = TempDir::new("slots-stream-hold");
        let path = transcript_file(&dir, A);
        let options = Options {
            // Only the read the Stop asks for can find the lines in time.
            stream_every: Duration::from_secs(3600),
            hold_answer: Duration::from_secs(30),
            ..stream_options()
        };
        let mut rig = stream_rig(Fake::default(), options, dir);
        rig.hook(start_with(A, 10, &path, "startup")).await;
        rig.ops_after(1).await;
        let _kept = rig.reader(1, A, 10).await;
        settled(&rig, |ops| last_icon(ops, 100) == Some(ICON_ALIVE)).await;
        tokio::time::sleep(Duration::from_millis(200)).await;
        append(&path, &tool_call("t1", "last step"));
        append(&path, &tool_result("t1", None));
        append(&path, &answer_record("done"));
        rig.hook(stop(A, Some("done"))).await;
        assert_eq!(
            stream_texts(&rig, 100, 2).await,
            ["• Bash: last step ✓", "done"]
        );
    }

    /// TASK-027: assistant text in the stream and the streamed answer go as
    /// HTML; prompts and tool lines stay plain.
    #[tokio::test]
    async fn stream_text_and_answer_go_as_html_and_tool_lines_stay_plain() {
        let dir = TempDir::new("slots-stream-html");
        let path = transcript_file(&dir, A);
        let mut rig = stream_rig(Fake::default(), stream_options(), dir);
        rig.hook(start_with(A, 10, &path, "startup")).await;
        rig.ops_after(1).await;
        let _kept = rig.reader(1, A, 10).await;
        settled(&rig, |ops| last_icon(ops, 100) == Some(ICON_ALIVE)).await;
        append(&path, &typed("go *now*"));
        append(&path, &note_record("**Checking** `a<b`"));
        append(&path, &tool_call("t1", "x <y>"));
        append(&path, &tool_result("t1", None));
        append(&path, &answer_record("_done_"));
        rig.hook(stop(A, Some("_done_"))).await;
        stream_texts(&rig, 100, 4).await;
        let html = |text: &str| Some(text.to_owned());
        assert_eq!(
            topic_html(&rig.fake.ops(), 100),
            [
                ("> go *now*".to_owned(), html("<pre>&gt; go *now*</pre>")),
                (
                    "**Checking** `a<b`".to_owned(),
                    html("<b>Checking</b> <code>a&lt;b</code>")
                ),
                ("• Bash: x <y> ✓".to_owned(), None),
                ("_done_".to_owned(), html("<i>done</i>")),
            ]
        );
    }

    fn note_record(text: &str) -> String {
        format!(
            "{{\"type\":\"assistant\",\"message\":{{\"role\":\"assistant\",\"stop_reason\":\"tool_use\",\"content\":[{{\"type\":\"text\",\"text\":\"{text}\"}}]}}}}\n"
        )
    }

    #[tokio::test]
    async fn a_held_answer_goes_out_when_the_agent_never_answers() {
        let dir = TempDir::new("slots-stream-hold-timeout");
        let path = transcript_file(&dir, A);
        let rig = stream_rig(Fake::default(), stream_options(), dir);
        rig.hook(start_with(A, 10, &path, "startup")).await;
        rig.ops_after(1).await;
        // Registers as a reader but never reads.
        let (to_agent, silent) = mpsc::channel(64);
        rig.agents
            .send(AgentEvent::Registered {
                conn: 1,
                register: Register {
                    session_id: A.into(),
                    host: "box".into(),
                    cwd: CWD.into(),
                    claude_pid: Some(10),
                    verdict_ack: true,
                    transcript_reads: true,
                    console_keys: false,
                    console_commands: false,
                    client: None,
                    files: false,
                    session_reads: false,
                    status_lines: false,
                    heartbeat: false,
                },
                to_agent,
            })
            .await
            .unwrap();
        settled(&rig, |ops| last_icon(ops, 100) == Some(ICON_ALIVE)).await;
        let asked = std::time::Instant::now();
        rig.hook(stop(A, Some("done anyway"))).await;
        assert_eq!(stream_texts(&rig, 100, 1).await, ["done anyway"]);
        assert!(asked.elapsed() >= Duration::from_millis(300), "held first");
        drop(silent);
    }

    #[tokio::test]
    async fn an_agent_without_transcript_reads_is_never_asked() {
        let dir = TempDir::new("slots-stream-old-agent");
        let path = transcript_file(&dir, A);
        let mut rig = stream_rig(Fake::default(), stream_options(), dir);
        rig.hook(start_with(A, 10, &path, "startup")).await;
        rig.ops_after(1).await;
        rig.agent_with(1, A, Some(10), true).await;
        settled(&rig, |ops| last_icon(ops, 100) == Some(ICON_ALIVE)).await;
        append(&path, &typed("not streamed"));
        rig.hook(stop(A, Some("answer at once"))).await;
        assert_eq!(stream_texts(&rig, 100, 1).await, ["answer at once"]);
        assert!(received(&mut rig, 0).await.is_empty());
    }

    #[tokio::test]
    async fn idle_reads_do_not_rewrite_the_registry() {
        let dir = TempDir::new("slots-stream-idle");
        let path = transcript_file(&dir, A);
        let mut slots = stalled_slots(&dir, stream_options());
        slots.on_hook(&start_with(A, 10, &path, "startup"));
        slots.registry.topic_created(SlotId(0), 100, "a", None);
        let (to_agent, mut from_hub) = mpsc::channel(4);
        slots.on_agent(AgentEvent::Registered {
            conn: 1,
            register: Register {
                session_id: A.into(),
                host: "box".into(),
                cwd: CWD.into(),
                claude_pid: Some(10),
                verdict_ack: true,
                transcript_reads: true,
                console_keys: false,
                console_commands: false,
                client: None,
                files: false,
                session_reads: false,
                status_lines: false,
                heartbeat: false,
            },
            to_agent,
        });
        slots.pump();
        assert!(matches!(
            from_hub.try_recv(),
            Ok(HubMsg::TranscriptRead { from: Some(0), .. })
        ));
        slots.registry.dirty = false;
        let empty = Chunk {
            from: 0,
            to: 0,
            lines: &[],
            missing: false,
            more: false,
            reset: false,
        };
        slots.on_chunk(1, A, &empty);
        assert!(!slots.registry.dirty, "an empty read wrote the registry");
    }

    #[tokio::test]
    async fn a_busy_queue_leaves_the_rest_of_a_chunk_in_the_file() {
        let dir = TempDir::new("slots-stream-budget");
        let path = transcript_file(&dir, A);
        let mut slots = stalled_slots(&dir, stream_options());
        slots.on_hook(&start_with(A, 10, &path, "startup"));
        slots.registry.topic_created(SlotId(0), 100, "a", None);
        let (to_agent, mut from_hub) = mpsc::channel(4);
        slots.on_agent(AgentEvent::Registered {
            conn: 1,
            register: Register {
                session_id: A.into(),
                host: "box".into(),
                cwd: CWD.into(),
                claude_pid: Some(10),
                verdict_ack: true,
                transcript_reads: true,
                console_keys: false,
                console_commands: false,
                client: None,
                files: false,
                session_reads: false,
                status_lines: false,
                heartbeat: false,
            },
            to_agent,
        });
        slots.queued_messages = STREAM_QUEUE - 1;
        slots.pump();
        assert!(matches!(
            from_hub.try_recv(),
            Ok(HubMsg::TranscriptRead { from: Some(0), .. })
        ));
        let prompt = |end: u64, text: &str| StreamLine {
            end,
            items: vec![crate::wire::StreamItem::Prompt { text: text.into() }],
        };
        let lines = [prompt(10, "one"), prompt(20, "two"), prompt(30, "three")];
        let chunk = Chunk {
            from: 0,
            to: 40,
            lines: &lines,
            missing: false,
            more: false,
            reset: false,
        };
        slots.on_chunk(1, A, &chunk);
        // The first line went out and counts; the rest waits in the file.
        assert_eq!(slots.queued_messages, STREAM_QUEUE);
        assert_eq!(slots.streams[A].read_at, Some(10));
        slots.pump();
        assert!(
            from_hub.try_recv().is_err(),
            "no read while the queue is full"
        );
    }

    /// Slots with session A streaming through agent conn 1, its first read
    /// (from 0) asked; nothing ever reaches Telegram.
    fn asked_slots(dir: &TempDir) -> Slots {
        let path = transcript_file(dir, A);
        let mut slots = stalled_slots(dir, stream_options());
        slots.on_hook(&start_with(A, 10, &path, "startup"));
        slots.registry.topic_created(SlotId(0), 100, "a", None);
        let (to_agent, mut from_hub) = mpsc::channel(4);
        slots.on_agent(AgentEvent::Registered {
            conn: 1,
            register: Register {
                session_id: A.into(),
                host: "box".into(),
                cwd: CWD.into(),
                claude_pid: Some(10),
                verdict_ack: true,
                transcript_reads: true,
                console_keys: false,
                console_commands: false,
                client: None,
                files: false,
                session_reads: false,
                status_lines: false,
                heartbeat: false,
            },
            to_agent,
        });
        slots.pump();
        assert!(matches!(
            from_hub.try_recv(),
            Ok(HubMsg::TranscriptRead { from: Some(0), .. })
        ));
        slots
    }

    fn one_prompt_chunk(session: &str) -> AgentMsg {
        AgentMsg::TranscriptChunk {
            session_id: session.into(),
            from: 0,
            to: 10,
            lines: vec![StreamLine {
                end: 10,
                items: vec![crate::wire::StreamItem::Prompt { text: "one".into() }],
            }],
            missing: false,
            more: false,
            reset: false,
        }
    }

    fn stream_offset(slots: &Slots) -> Option<u64> {
        slots.registry.sessions[A].stream.as_ref().unwrap().offset
    }

    #[tokio::test]
    async fn a_chunk_for_another_session_on_the_connection_is_dropped() {
        let dir = TempDir::new("slots-stream-foreign-chunk");
        let mut slots = asked_slots(&dir);
        let message = |msg| AgentEvent::Message {
            conn: 1,
            received_at: StdInstant::now(),
            msg,
        };
        slots.on_agent(message(one_prompt_chunk(B)));
        assert!(slots.streams[A].reading.is_some(), "the read still waits");
        assert!(!slots.streams.contains_key(B));
        assert_eq!(slots.queued_messages, 0);
        assert_eq!(slots.streams[A].read_at, Some(0));
        // The same chunk for the bound session is taken.
        slots.on_agent(message(one_prompt_chunk(A)));
        assert!(slots.streams[A].reading.is_none());
        assert_eq!(slots.queued_messages, 1);
        assert_eq!(slots.streams[A].read_at, Some(10));
    }

    /// TASK-023: an answer whose turn end was read before its `Stop` rides
    /// the stream behind the lines of its turn; when a line before it is
    /// refused (and the answer dropped unsent behind it), the rewind holds it
    /// again and the re-read turn end lets it go after the lines again.
    #[tokio::test]
    async fn an_answer_behind_a_refused_line_is_held_again_and_follows_the_re_read_lines() {
        use crate::wire::StreamItem;
        let dir = TempDir::new("slots-stream-answer-refused");
        let mut slots = asked_slots(&dir);
        let chunk = || AgentMsg::TranscriptChunk {
            session_id: A.into(),
            from: 0,
            to: 20,
            lines: vec![
                StreamLine {
                    end: 10,
                    items: vec![StreamItem::Prompt { text: "one".into() }],
                },
                StreamLine {
                    end: 20,
                    items: vec![StreamItem::TurnEnd],
                },
            ],
            missing: false,
            more: false,
            reset: false,
        };
        let message = |msg| AgentEvent::Message {
            conn: 1,
            received_at: StdInstant::now(),
            msg,
        };
        slots.on_agent(message(chunk()));
        assert_eq!(slots.streams[A].ends_unclaimed, [20]);
        slots.on_hook(&stop(A, Some("done")));
        let live = &slots.streams[A];
        assert!(live.ends_unclaimed.is_empty(), "the Stop took the turn end");
        assert_eq!(live.unanswered(), 2, "the answer rides the stream");
        assert!(live.held.is_empty());

        // The line is refused; the answer behind it never left (dropped).
        slots.on_stream_done(
            A,
            1,
            Some(Err(ApiError::Telegram {
                code: 502,
                description: "Bad Gateway".into(),
            })),
        );
        slots.on_stream_done(A, 2, None);
        let live = &slots.streams[A];
        assert_eq!(live.read_at, Some(0), "rewound");
        let held: Vec<&str> = live.held.iter().map(|h| h.answer.as_str()).collect();
        assert_eq!(held, ["done"], "held again, not lost");
        assert_eq!(stream_offset(&slots), Some(0));

        // The re-read: the line, then the answer at its turn end.
        let live = slots.streams.get_mut(A).unwrap();
        live.reading = Some((1, Instant::now()));
        assert!(live.restart);
        slots.on_agent(message(chunk()));
        let live = &slots.streams[A];
        assert!(live.held.is_empty());
        assert_eq!(live.unanswered(), 2);
        slots.on_stream_done(A, 1, Some(Ok(Outcome::Sent(Message::default()))));
        slots.on_stream_done(A, 2, Some(Ok(Outcome::Sent(Message::default()))));
        assert_eq!(stream_offset(&slots), Some(20));
        assert!(slots.streams[A].held.is_empty());
        assert_eq!(slots.queued_messages, 0);
    }

    /// TASK-023 review I1 (the reviewer's probe): two turns in one read,
    /// both answers held before it. Turn 2's prompt is refused, its answer
    /// dropped unsent behind it; the rewind re-reads turn 1's end too, whose
    /// answer is in the topic. That end lets nothing go and is not left for
    /// a later `Stop`: turn 2's answer goes at its own end, after `> two`.
    #[tokio::test]
    async fn a_turn_end_read_again_lets_only_its_own_answer_go() {
        use crate::wire::StreamItem;
        let dir = TempDir::new("slots-stream-answer-pairing");
        let mut slots = asked_slots(&dir);
        let chunk = || AgentMsg::TranscriptChunk {
            session_id: A.into(),
            from: 0,
            to: 40,
            lines: vec![
                StreamLine {
                    end: 10,
                    items: vec![StreamItem::Prompt { text: "one".into() }],
                },
                StreamLine {
                    end: 20,
                    items: vec![StreamItem::TurnEnd],
                },
                StreamLine {
                    end: 30,
                    items: vec![StreamItem::Prompt { text: "two".into() }],
                },
                StreamLine {
                    end: 40,
                    items: vec![StreamItem::TurnEnd],
                },
            ],
            missing: false,
            more: false,
            reset: false,
        };
        let message = |msg| AgentEvent::Message {
            conn: 1,
            received_at: StdInstant::now(),
            msg,
        };
        let ok = || Some(Ok(Outcome::Sent(Message::default())));
        slots.on_hook(&stop(A, Some("first")));
        slots.on_hook(&stop(A, Some("second")));
        slots.on_agent(message(chunk()));
        // 1 "> one", 2 "first", 3 "> two", 4 "second".
        assert_eq!(
            slots.streams[A].answer_numbers(),
            [(2, "first"), (4, "second")]
        );
        slots.on_stream_done(A, 1, ok());
        slots.on_stream_done(A, 2, ok());
        slots.on_stream_done(
            A,
            3,
            Some(Err(ApiError::Telegram {
                code: 502,
                description: "Bad Gateway".into(),
            })),
        );
        slots.on_stream_done(A, 4, None);
        let live = &slots.streams[A];
        assert_eq!(live.read_at, Some(0), "rewound to the start of the read");
        let held: Vec<&str> = live.held.iter().map(|h| h.answer.as_str()).collect();
        assert_eq!(held, ["second"]);

        slots.streams.get_mut(A).unwrap().reading = Some((1, Instant::now()));
        slots.on_agent(message(chunk()));
        let live = &slots.streams[A];
        // 1 "> one" again, 2 "> two", 3 "second": "first" is not sent twice
        // and "second" follows its own lines.
        assert_eq!(live.answer_numbers(), [(3, "second")]);
        assert_eq!(live.unanswered(), 3);
        assert!(live.held.is_empty());
        assert!(
            live.ends_unclaimed.is_empty(),
            "no stale turn end for the next Stop: {:?}",
            live.ends_unclaimed
        );
        // The next turn's answer waits for its own turn end.
        slots.on_hook(&stop(A, Some("third")));
        let held: Vec<&str> = slots.streams[A]
            .held
            .iter()
            .map(|h| h.answer.as_str())
            .collect();
        assert_eq!(held, ["third"]);
    }

    /// TASK-023 review I1: an answer that claimed a turn end already behind
    /// the committed offset, then was dropped behind a refused line, goes
    /// first on the re-read (its lines are in the topic), not at the next
    /// turn's end.
    #[tokio::test]
    async fn an_answer_held_again_whose_turn_end_is_committed_goes_first_on_the_re_read() {
        use crate::wire::StreamItem;
        let dir = TempDir::new("slots-stream-answer-overdue");
        let mut slots = asked_slots(&dir);
        let message = |msg| AgentEvent::Message {
            conn: 1,
            received_at: StdInstant::now(),
            msg,
        };
        let ok = || Some(Ok(Outcome::Sent(Message::default())));
        slots.on_agent(message(AgentMsg::TranscriptChunk {
            session_id: A.into(),
            from: 0,
            to: 20,
            lines: vec![
                StreamLine {
                    end: 10,
                    items: vec![StreamItem::Prompt { text: "one".into() }],
                },
                StreamLine {
                    end: 20,
                    items: vec![StreamItem::TurnEnd],
                },
            ],
            missing: false,
            more: false,
            reset: false,
        }));
        slots.on_stream_done(A, 1, ok());
        assert_eq!(stream_offset(&slots), Some(20));
        // The Stop comes after its turn end is committed.
        slots.on_hook(&stop(A, Some("first")));
        assert_eq!(slots.streams[A].answer_numbers(), [(2, "first")]);
        let two = || AgentMsg::TranscriptChunk {
            session_id: A.into(),
            from: 20,
            to: 30,
            lines: vec![StreamLine {
                end: 30,
                items: vec![StreamItem::Prompt { text: "two".into() }],
            }],
            missing: false,
            more: false,
            reset: false,
        };
        slots.streams.get_mut(A).unwrap().reading = Some((1, Instant::now()));
        slots.on_agent(message(two()));
        slots.on_stream_done(
            A,
            2,
            Some(Err(ApiError::Telegram {
                code: 502,
                description: "Bad Gateway".into(),
            })),
        );
        slots.on_stream_done(A, 3, None);
        let live = &slots.streams[A];
        assert_eq!(live.read_at, Some(20));
        assert_eq!(live.held.len(), 1);

        slots.streams.get_mut(A).unwrap().reading = Some((1, Instant::now()));
        slots.on_agent(message(two()));
        let live = &slots.streams[A];
        // 1 "first", 2 "> two".
        assert_eq!(live.answer_numbers(), [(1, "first")]);
        assert_eq!(live.unanswered(), 2);
        assert!(live.held.is_empty());
    }

    /// TASK-023 review I2: a streamed answer counts against
    /// [`MAX_QUEUED_MESSAGES`]; one that does not fit is dropped like a plain
    /// one, and nothing of it stays tracked in the stream.
    #[tokio::test]
    async fn a_streamed_answer_past_the_message_cap_is_dropped_untracked() {
        let dir = TempDir::new("slots-stream-answer-cap");
        let mut slots = asked_slots(&dir);
        slots.queued_messages = MAX_QUEUED_MESSAGES;
        for n in 0..=stream::MAX_HELD {
            slots.on_hook(&stop(A, Some(format!("answer {n}").as_str())));
        }
        // The oldest was pushed out of the held ones with no room left.
        assert_eq!(slots.streams[A].held.len(), stream::MAX_HELD);
        assert_eq!(slots.queued_messages, MAX_QUEUED_MESSAGES);
        assert_eq!(slots.streams[A].unanswered(), 0, "nothing tracked");
        assert!(slots.overflow_warned);
        // With room the next pushed-out answer rides the stream.
        slots.queued_messages = 0;
        slots.on_hook(&stop(A, Some("late")));
        assert_eq!(slots.queued_messages, 1);
        assert_eq!(slots.streams[A].answer_numbers(), [(1, "answer 1")]);
    }

    #[tokio::test]
    async fn a_stream_message_telegram_refuses_with_a_4xx_is_skipped_and_the_offset_moves() {
        let dir = TempDir::new("slots-stream-skip-4xx");
        let mut slots = asked_slots(&dir);
        slots.on_agent(AgentEvent::Message {
            conn: 1,
            received_at: StdInstant::now(),
            msg: one_prompt_chunk(A),
        });
        assert_eq!(stream_offset(&slots), Some(0));
        slots.on_stream_done(
            A,
            1,
            Some(Err(ApiError::Telegram {
                code: 400,
                description: "Bad Request: message text is empty".into(),
            })),
        );
        assert_eq!(stream_offset(&slots), Some(10));
        assert_eq!(slots.queued_messages, 0);
        assert!(!slots.streams[A].stuck(), "skipped, not sent again");
    }

    // ---- TASK-016 review 2: ordering barrier, delivery commit, reset ----

    /// The assistant text that ends a turn.
    fn answer_record(text: &str) -> String {
        format!(
            "{{\"type\":\"assistant\",\"message\":{{\"role\":\"assistant\",\"stop_reason\":\"end_turn\",\"content\":[{{\"type\":\"text\",\"text\":\"{text}\"}}]}}}}\n"
        )
    }

    fn saved_offset(dir: &std::path::Path) -> Option<u64> {
        let text = std::fs::read_to_string(dir.join("registry.json")).unwrap_or_default();
        serde_json::from_str::<serde_json::Value>(&text)
            .ok()
            .and_then(|v| v["sessions"][A]["stream"]["offset"].as_u64())
    }

    async fn live_stream(options: Options, fake: Fake, name: &str) -> (Rig, String) {
        let dir = TempDir::new(name);
        let path = transcript_file(&dir, A);
        let mut rig = stream_rig(fake, options, dir);
        rig.hook(start_with(A, 10, &path, "startup")).await;
        rig.ops_after(1).await;
        drop(rig.reader(1, A, 10).await);
        settled(&rig, |ops| last_icon(ops, 100) == Some(ICON_ALIVE)).await;
        (rig, path)
    }

    #[tokio::test]
    async fn a_turn_answer_waits_for_its_turn_end_even_when_the_file_lags() {
        let options = Options {
            hold_answer: Duration::from_secs(30),
            ..stream_options()
        };
        let (rig, path) = live_stream(options, Fake::default(), "slots-stream-lag").await;
        rig.hook(stop(A, Some("done"))).await;
        // The file lags far behind the hook.
        tokio::time::sleep(Duration::from_millis(2000)).await;
        assert!(topic_texts(&rig.fake.ops(), 100).is_empty(), "held");
        append(&path, &tool_call("t1", "late step"));
        append(&path, &tool_result("t1", None));
        append(&path, &answer_record("done"));
        assert_eq!(
            stream_texts(&rig, 100, 2).await,
            ["• Bash: late step ✓", "done"]
        );
    }

    #[tokio::test]
    async fn every_stop_of_a_turn_follows_its_own_tool_lines() {
        let options = Options {
            hold_answer: Duration::from_secs(30),
            ..stream_options()
        };
        let (rig, path) = live_stream(options, Fake::default(), "slots-stream-stops").await;
        // A blocking Stop hook of the user: the turn answers twice.
        rig.hook(stop(A, Some("first"))).await;
        rig.hook(stop(A, Some("second"))).await;
        tokio::time::sleep(Duration::from_millis(300)).await;
        append(&path, &tool_call("t1", "one"));
        append(&path, &tool_result("t1", None));
        append(&path, &answer_record("first"));
        append(&path, &tool_call("t2", "two"));
        append(&path, &tool_result("t2", None));
        append(&path, &answer_record("second"));
        assert_eq!(
            stream_texts(&rig, 100, 4).await,
            ["• Bash: one ✓", "first", "• Bash: two ✓", "second"]
        );
    }

    #[tokio::test]
    async fn two_turn_ends_in_one_read_serve_the_held_answer_and_the_next_stop() {
        let options = Options {
            hold_answer: Duration::from_secs(30),
            ..stream_options()
        };
        let (rig, path) = live_stream(options, Fake::default(), "slots-stream-two-ends").await;
        rig.hook(stop(A, Some("first"))).await;
        tokio::time::sleep(Duration::from_millis(300)).await;
        let mut lines = tool_call("t1", "one");
        lines.push_str(&tool_result("t1", None));
        lines.push_str(&answer_record("first"));
        lines.push_str(&tool_call("t2", "two"));
        lines.push_str(&tool_result("t2", None));
        lines.push_str(&answer_record("second"));
        append(&path, &lines);
        assert_eq!(
            stream_texts(&rig, 100, 3).await,
            ["• Bash: one ✓", "first", "• Bash: two ✓"]
        );
        let asked = std::time::Instant::now();
        rig.hook(stop(A, Some("second"))).await;
        assert_eq!(stream_texts(&rig, 100, 4).await[3], "second");
        assert!(asked.elapsed() < Duration::from_secs(10), "not held");
    }

    #[tokio::test]
    async fn a_turn_end_read_before_its_stop_lets_the_answer_go_at_once() {
        let options = Options {
            hold_answer: Duration::from_secs(30),
            ..stream_options()
        };
        let (rig, path) = live_stream(options, Fake::default(), "slots-stream-quick").await;
        append(&path, &tool_call("t1", "one"));
        append(&path, &tool_result("t1", None));
        append(&path, &answer_record("quick"));
        assert_eq!(stream_texts(&rig, 100, 1).await, ["• Bash: one ✓"]);
        let asked = std::time::Instant::now();
        rig.hook(stop(A, Some("quick"))).await;
        assert_eq!(stream_texts(&rig, 100, 2).await, ["• Bash: one ✓", "quick"]);
        assert!(asked.elapsed() < Duration::from_secs(10), "not held");
    }

    #[tokio::test]
    async fn a_stop_that_comes_after_the_next_prompt_was_read_still_takes_its_turn_end() {
        let options = Options {
            hold_answer: Duration::from_secs(30),
            ..stream_options()
        };
        let (rig, path) = live_stream(options, Fake::default(), "slots-stream-late-stop").await;
        let mut lines = tool_call("t1", "one");
        lines.push_str(&tool_result("t1", None));
        lines.push_str(&answer_record("late"));
        lines.push_str(&typed("next"));
        append(&path, &lines);
        assert_eq!(
            stream_texts(&rig, 100, 2).await,
            ["• Bash: one ✓", "> next"]
        );
        let asked = std::time::Instant::now();
        rig.hook(stop(A, Some("late"))).await;
        assert_eq!(stream_texts(&rig, 100, 3).await[2], "late");
        assert!(asked.elapsed() < Duration::from_secs(10), "not held");
    }

    #[tokio::test]
    async fn lines_after_a_refused_stream_message_never_show_before_it() {
        let fake = Fake {
            stream_errors: Mutex::new(1),
            ..Fake::default()
        };
        let options = Options {
            stream_retry: Duration::from_millis(200),
            ..stream_options()
        };
        let (rig, path) = live_stream(options, fake, "slots-stream-refused-order").await;
        let mut lines = typed("go");
        for (id, description) in [("t1", "one"), ("t2", "two"), ("t3", "three")] {
            lines.push_str(&tool_call(id, description));
            lines.push_str(&tool_result(id, None));
        }
        append(&path, &lines);
        // Every attempt Telegram saw: the refused one, then the stream again
        // from it, in order. Nothing queued behind it went out in between.
        assert_eq!(
            stream_texts(&rig, 100, 5).await,
            [
                "> go",
                "> go",
                "• Bash: one ✓",
                "• Bash: two ✓",
                "• Bash: three ✓"
            ]
        );
    }

    #[tokio::test]
    async fn a_refused_stream_message_is_sent_again_before_the_offset_moves() {
        let fake = Fake {
            stream_errors: Mutex::new(1),
            ..Fake::default()
        };
        let options = Options {
            stream_retry: Duration::from_millis(200),
            ..stream_options()
        };
        let (rig, path) = live_stream(options, fake, "slots-stream-refused").await;
        append(&path, &tool_call("t1", "one"));
        append(&path, &tool_result("t1", None));
        // The refused attempt, then the one Telegram takes.
        assert_eq!(
            stream_texts(&rig, 100, 2).await,
            ["• Bash: one ✓", "• Bash: one ✓"]
        );
        let len = std::fs::metadata(&path).unwrap().len();
        let state = rig.dir.path().to_path_buf();
        let saved = async {
            while saved_offset(&state) != Some(len) {
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        };
        tokio::time::timeout(WAIT, saved)
            .await
            .expect("offset saved");
    }

    #[tokio::test]
    async fn a_refused_stream_message_comes_again_after_a_restart() {
        let fake = Fake {
            stream_errors: Mutex::new(usize::MAX),
            ..Fake::default()
        };
        let options = Options {
            stream_retry: Duration::from_secs(3600),
            ..stream_options()
        };
        let (rig, path) = live_stream(options, fake, "slots-stream-refused-restart").await;
        append(&path, &tool_call("t1", "one"));
        append(&path, &tool_result("t1", None));
        settled(&rig, |ops| topic_texts(ops, 100).len() == 1).await;
        tokio::time::sleep(Duration::from_millis(300)).await;
        let len = std::fs::metadata(&path).unwrap().len();
        assert_ne!(saved_offset(rig.dir.path()), Some(len), "not committed");

        let Rig { dir, .. } = rig;
        let mut rig = stream_rig(Fake::default(), stream_options(), dir);
        let _kept = rig.reader(2, A, 10).await;
        assert_eq!(stream_texts(&rig, 100, 1).await, ["• Bash: one ✓"]);
    }

    #[tokio::test]
    async fn a_new_agent_process_goes_on_from_the_stream_position() {
        let dir = TempDir::new("slots-stream-agent-restart");
        let path = transcript_file(&dir, A);
        let mut rig = stream_rig(Fake::default(), stream_options(), dir);
        rig.hook(start_with(A, 10, &path, "startup")).await;
        rig.ops_after(1).await;
        let gate = Arc::new(ReadGate::default());
        let _first = rig.gated_reader(1, A, 10, gate.clone()).await;
        append(&path, &typed("one"));
        assert_eq!(stream_texts(&rig, 100, 1).await, ["> one"]);
        // The agent goes away while a read to it is in flight: that read
        // must not hold the stream until its timeout.
        gate.stopped.store(true, Ordering::SeqCst);
        let parked = async {
            while !gate.parked.load(Ordering::SeqCst) {
                tokio::time::sleep(Duration::from_millis(5)).await;
            }
        };
        tokio::time::timeout(WAIT, parked)
            .await
            .expect("a read in flight");
        rig.agents
            .send(AgentEvent::Disconnected { conn: 1 })
            .await
            .unwrap();
        append(&path, &typed("two"));
        let back = std::time::Instant::now();
        let _kept = rig.reader(2, A, 10).await;
        assert_eq!(stream_texts(&rig, 100, 2).await, ["> one", "> two"]);
        assert!(back.elapsed() < Duration::from_secs(5), "no read timeout");
    }

    #[tokio::test]
    async fn a_clear_in_the_same_process_streams_the_new_session_after_one_separator() {
        let dir = TempDir::new("slots-stream-clear");
        let first = transcript_file(&dir, A);
        let second = transcript_file(&dir, B);
        let mut rig = stream_rig(Fake::default(), stream_options(), dir);
        rig.hook(start_with(A, 10, &first, "startup")).await;
        rig.ops_after(1).await;
        let _kept = rig.reader(1, A, 10).await;
        append(&first, &typed("from A"));
        assert_eq!(stream_texts(&rig, 100, 1).await, ["> from A"]);
        rig.hook(hook(
            A,
            HookEvent::SessionEnd {
                reason: Some("clear".into()),
                claude_pid: Some(10),
            },
        ))
        .await;
        append(&second, &typed("from B"));
        rig.hook(start_with(B, 10, &second, "clear")).await;
        // The same agent (conn 1) now serves B.
        assert_eq!(
            stream_texts(&rig, 100, 3).await,
            ["> from A", "── session bbbbbbbb · new ──", "> from B"]
        );
        append(&first, &typed("late A"));
        tokio::time::sleep(Duration::from_millis(300)).await;
        assert_eq!(topic_texts(&rig.fake.ops(), 100).len(), 3);
    }

    #[tokio::test]
    async fn a_cut_transcript_is_read_again_from_its_start() {
        let (rig, path) = live_stream(stream_options(), Fake::default(), "slots-stream-cut").await;
        append(&path, &typed("before"));
        assert_eq!(stream_texts(&rig, 100, 1).await, ["> before"]);
        std::fs::write(&path, typed("after")).unwrap();
        assert_eq!(stream_texts(&rig, 100, 2).await, ["> before", "> after"]);
    }

    #[tokio::test]
    async fn a_channel_record_of_another_server_leaves_the_reaction() {
        let (rig, path) =
            live_stream(stream_options(), Fake::default(), "slots-stream-foreign").await;
        rig.control.send(say(Some(100), 42, Some("hi"))).unwrap();
        settled(&rig, |ops| reactions(ops) == [(42, "👀".to_owned())]).await;
        append(
            &path,
            &channel_record(42).replace("source=\\\"cctg\\\"", "source=\\\"webhook\\\""),
        );
        append(&path, &typed("marker"));
        stream_texts(&rig, 100, 1).await;
        assert_eq!(reactions(&rig.fake.ops()), [(42, "👀".to_owned())]);
    }

    fn status_options() -> Options {
        Options {
            status_every: Some(Duration::from_millis(20)),
            ..stream_options()
        }
    }

    /// The status message: the first send of the topic that carries a
    /// keyboard.
    fn status_id(ops: &[Op]) -> Option<i64> {
        let sends: Vec<&Op> = ops
            .iter()
            .filter(|op| matches!(op, Op::Send { .. }))
            .collect();
        sends
            .iter()
            .position(|op| {
                matches!(
                    op,
                    Op::Send {
                        reply_markup: Some(_),
                        ..
                    }
                )
            })
            .map(|index| 1000 + index as i64)
    }

    fn last_status_text(ops: &[Op]) -> Option<String> {
        let id = status_id(ops)?;
        edits_of(ops, id).last().map(|(text, _)| text.clone())
    }

    #[tokio::test]
    async fn status_messages_are_off_without_status_every() {
        let mut rig = rig(Fake::default(), options());
        rig.hook(start(A, 10)).await;
        rig.ops_after(1).await;
        rig.agent_of(1, A, Some(10)).await;
        rig.hook(hook(A, HookEvent::UserPromptSubmit { prompt_id: None }))
            .await;
        let ops = settled(&rig, |ops| last_icon(ops, 100) == Some(ICON_ALIVE)).await;
        tokio::time::sleep(Duration::from_millis(200)).await;
        let ops = [ops, rig.fake.ops()].concat();
        assert!(!ops.iter().any(|op| matches!(op, Op::Pin { .. })));
        assert_eq!(status_id(&ops), None);
    }

    #[tokio::test]
    async fn an_interrupt_note_in_the_stream_ends_the_turn_on_the_status_message() {
        let dir = TempDir::new("slots-status-interrupt");
        let path = transcript_file(&dir, A);
        let mut rig = stream_rig(Fake::default(), status_options(), dir);
        rig.hook(start_with(A, 10, &path, "startup")).await;
        let _kept = rig.reader(1, A, 10).await;
        settled(&rig, |ops| {
            ops.iter().any(|op| matches!(op, Op::Pin { .. }))
        })
        .await;
        rig.hook(hook(A, HookEvent::UserPromptSubmit { prompt_id: None }))
            .await;
        settled(&rig, |ops| {
            last_status_text(ops).as_deref() == Some("💭 Думает")
        })
        .await;
        // Esc in the terminal: Claude Code writes its note, no Stop comes.
        append(&path, &typed("[Request interrupted by user]"));
        settled(&rig, |ops| {
            last_status_text(ops).as_deref() == Some("💤 Ждёт вас")
        })
        .await;
    }

    #[tokio::test]
    async fn a_call_whose_result_is_in_the_stream_no_longer_shows_as_running() {
        let dir = TempDir::new("slots-status-result");
        let path = transcript_file(&dir, A);
        let mut rig = stream_rig(Fake::default(), status_options(), dir);
        rig.hook(start_with(A, 10, &path, "startup")).await;
        let _kept = rig.reader(1, A, 10).await;
        settled(&rig, |ops| {
            ops.iter().any(|op| matches!(op, Op::Pin { .. }))
        })
        .await;
        rig.hook(hook(A, HookEvent::UserPromptSubmit { prompt_id: None }))
            .await;
        rig.hook(hook(
            A,
            HookEvent::ToolStart {
                tool_use_id: "toolu_9".into(),
                line: "• Bash: deploy".into(),
            },
        ))
        .await;
        settled(&rig, |ops| {
            last_status_text(ops).as_deref() == Some("⚙️ Bash: deploy")
        })
        .await;
        // Denied in the terminal: no PostToolUse comes, but the transcript
        // gets the call and its refused result.
        append(&path, &tool_call("toolu_9", "deploy"));
        append(&path, &tool_result("toolu_9", Some("denied")));
        settled(&rig, |ops| {
            last_status_text(ops).as_deref() == Some("💭 Думает")
        })
        .await;
    }

    #[tokio::test]
    async fn a_status_button_of_an_agent_without_console_keys_sends_nothing() {
        let mut rig = rig(Fake::default(), status_options());
        rig.hook(start(A, 10)).await;
        rig.ops_after(1).await;
        rig.agent_of(1, A, Some(10)).await;
        rig.hook(hook(A, HookEvent::UserPromptSubmit { prompt_id: None }))
            .await;
        let ops = settled(&rig, |ops| {
            last_status_text(ops).as_deref() == Some("💭 Думает")
        })
        .await;
        let id = status_id(&ops).unwrap();
        // No buttons: this agent cannot press keys.
        assert_eq!(
            edits_of(&ops, id)
                .last()
                .and_then(|(_, markup)| markup.clone()),
            Some(permissions::no_keyboard())
        );
        for data in ["status:stop", "status:confirm"] {
            rig.control.send(press("q", Some(id), data)).unwrap();
        }
        let ops = settled(&rig, |ops| answers(ops).len() >= 2).await;
        assert_eq!(answers(&ops), [Some(status::ANSWER_NO_KEYS); 2]);
        assert!(
            !received(&mut rig, 0)
                .await
                .iter()
                .any(|msg| matches!(msg, HubMsg::ConsoleKey { .. }))
        );
    }

    /// A live session A in slot 0 with topic 100 and an agent (conn 1) that
    /// presses keys; what the hub sends that agent comes out of the receiver.
    fn keyed_slots(dir: &TempDir, options: Options) -> (Slots, mpsc::Receiver<HubMsg>) {
        let (_fake, mut slots) = live_slots(dir, options);
        slots.on_hook(&start(A, 10));
        slots.registry.topic_created(SlotId(0), 100, "a", None);
        let (to_agent, from_hub) = mpsc::channel(8);
        slots.on_agent(AgentEvent::Registered {
            conn: 1,
            register: Register {
                session_id: A.into(),
                host: "box".into(),
                cwd: CWD.into(),
                claude_pid: Some(10),
                verdict_ack: false,
                transcript_reads: false,
                console_keys: true,
                console_commands: false,
                client: None,
                files: false,
                session_reads: false,
                status_lines: false,
                heartbeat: false,
            },
            to_agent,
        });
        (slots, from_hub)
    }

    fn offers_stop(slots: &Slots) -> bool {
        let (_, keyboard) = slots.status_view(SlotId(0), A, Instant::now());
        keyboard != permissions::no_keyboard()
    }

    #[tokio::test]
    async fn a_prompt_answered_in_the_terminal_stops_waiting_once_a_later_call_comes() {
        let dir = TempDir::new("slots-status-terminal-answer");
        let (mut slots, _from_hub) = keyed_slots(&dir, options());
        slots.on_hook(&hook(A, HookEvent::UserPromptSubmit { prompt_id: None }));
        let start_call = |id: &str| {
            hook(
                A,
                HookEvent::ToolStart {
                    tool_use_id: id.into(),
                    line: "• Bash: x".into(),
                },
            )
        };
        slots.on_hook(&start_call("t1"));
        slots.on_agent(permission(1, "abcde", "p"));
        assert!(slots.waiting(A));
        assert!(!offers_stop(&slots), "Esc would answer the prompt");
        // The end of the call before the prompt comes in after it (the tool
        // hooks run in the background): the prompt still waits.
        slots.on_hook(&hook(
            A,
            HookEvent::ToolEnd {
                tool_use_id: "t1".into(),
            },
        ));
        assert!(slots.waiting(A));
        // Later, the next call starts: the prompt was answered in the
        // terminal. Its buttons stay; ⏹ is back.
        for key in slots.prompts.active() {
            let prompt = slots.prompts.get_mut(key).unwrap();
            prompt.opened -= PROMPT_SETTLE;
        }
        slots.on_hook(&start_call("t2"));
        assert!(!slots.waiting(A));
        assert!(!slots.registry.sessions[A].waiting, "the icon too");
        assert_eq!(slots.prompts.active().len(), 1);
        assert!(offers_stop(&slots));
    }

    #[tokio::test]
    async fn a_written_esc_is_shown_at_once_whatever_the_edit_pace() {
        let dir = TempDir::new("slots-status-written");
        let (mut slots, mut from_hub) = keyed_slots(&dir, status_options());
        slots.registry.slots[0].status = Some(StatusMessage {
            message_id: 500,
            pinned: true,
        });
        slots.on_hook(&hook(A, HookEvent::UserPromptSubmit { prompt_id: None }));
        assert_eq!(
            slots.press_status(Some(500), Press::Stop),
            status::ANSWER_CONFIRM
        );
        assert_eq!(
            slots.press_status(Some(500), Press::Confirm),
            status::ANSWER_INTERRUPTING
        );
        let Some(HubMsg::ConsoleKey { key_id, .. }) = from_hub.recv().await else {
            panic!("no console key");
        };
        // The confirming press's edit went out; the next one waits the pace.
        let paced = Instant::now() + Duration::from_secs(60);
        slots.shown.entry(SlotId(0)).or_default().next_at = Some(paced);
        slots.on_key_written(1, A, key_id, true);
        assert!(
            slots.shown[&SlotId(0)]
                .next_at
                .is_some_and(|at| at <= Instant::now())
        );
        let (text, _) = slots.status_view(SlotId(0), A, Instant::now());
        assert_eq!(text, "⏹ Esc отправлен в терминал");
    }

    /// The status calls handed to the scheduler since the last call.
    fn status_work(work: &mut mpsc::UnboundedReceiver<(Work, Op)>) -> Vec<(StatusJob, Op)> {
        let mut found = Vec::new();
        while let Ok((job, op)) = work.try_recv() {
            if let Work::Status { job, .. } = job {
                found.push((job, op));
            }
        }
        found
    }

    #[tokio::test(start_paused = true)]
    async fn a_stop_question_replaces_a_waiting_refresh_and_gets_its_whole_wait_once_shown() {
        let dir = TempDir::new("slots-status-late-question");
        let (mut slots, mut from_hub) = keyed_slots(&dir, status_options());
        slots.registry.slots[0].status = Some(StatusMessage {
            message_id: 500,
            pinned: true,
        });
        let mut work = capture_dispatch(&mut slots);
        slots.on_hook(&hook(A, HookEvent::UserPromptSubmit { prompt_id: None }));
        slots.pump();
        // A refresh waits for the edit budget.
        let mut handed = status_work(&mut work);
        assert_eq!(handed.len(), 1);
        let (refresh, op) = handed.remove(0);
        assert!(
            matches!(
                op,
                Op::Edit {
                    background: true,
                    ..
                }
            ),
            "{op:?}"
        );
        // ⏹: the question goes next to it as a foreground edit.
        assert_eq!(
            slots.press_status(Some(500), Press::Stop),
            status::ANSWER_CONFIRM
        );
        slots.pump();
        let mut handed = status_work(&mut work);
        assert_eq!(handed.len(), 1);
        let (question, op) = handed.remove(0);
        assert!(
            matches!(&op, Op::Edit { message_id: 500, background: false, reply_markup: Some(keys), .. }
                if status::asks_confirm(keys)),
            "{op:?}"
        );
        // No third call while both are out.
        tokio::time::advance(Duration::from_secs(1)).await;
        slots.pump();
        assert!(status_work(&mut work).is_empty());
        // The scheduler let the question replace the refresh; Telegram shows
        // it 9 s after the press.
        slots.on_status_done(SlotId(0), refresh, Some(Ok(Outcome::Superseded)));
        tokio::time::advance(Duration::from_secs(8)).await;
        slots.on_status_done(SlotId(0), question, Some(Ok(Outcome::Done)));
        assert_eq!(slots.shown[&SlotId(0)].in_flight, 0);
        // 14 s after the press, 5 s after it showed: still the second press.
        tokio::time::advance(Duration::from_secs(5)).await;
        assert_eq!(
            slots.press_status(Some(500), Press::Confirm),
            status::ANSWER_INTERRUPTING
        );
        assert!(matches!(
            from_hub.recv().await,
            Some(HubMsg::ConsoleKey { .. })
        ));
    }

    #[tokio::test]
    async fn an_interrupt_note_at_an_open_prompt_ends_the_wait() {
        let dir = TempDir::new("slots-status-interrupt-waiting");
        let path = transcript_file(&dir, A);
        let mut rig = stream_rig(Fake::default(), status_options(), dir);
        rig.hook(start_with(A, 10, &path, "startup")).await;
        let _kept = rig.reader(1, A, 10).await;
        settled(&rig, |ops| {
            ops.iter().any(|op| matches!(op, Op::Pin { .. }))
        })
        .await;
        rig.hook(hook(A, HookEvent::UserPromptSubmit { prompt_id: None }))
            .await;
        // The prompt comes after the turn started (hooks and agent frames
        // travel apart; UserPromptSubmit ends the wait).
        settled(&rig, |ops| {
            last_status_text(ops).as_deref() == Some("💭 Думает")
        })
        .await;
        rig.agents.send(permission(1, "abcde", "p")).await.unwrap();
        settled(&rig, |ops| {
            last_status_text(ops).as_deref() == Some("❓ Ждёт разрешения")
        })
        .await;
        // Esc in the terminal at the prompt: the note, no Stop.
        append(&path, &typed("[Request interrupted by user for tool use]"));
        settled(&rig, |ops| {
            last_status_text(ops).as_deref() == Some("💤 Ждёт вас")
        })
        .await;
    }

    // ------------------------------------------------------------ TASK-054

    /// Session `n` of the paced test: its short id is `n` in hex.
    fn paced_session(n: u32) -> String {
        format!("{n:08x}-0000-4000-8000-{n:012x}")
    }

    /// An agent of `session` that presses keys; what the hub sends it comes
    /// out of `rig._to_agent` (in registration order).
    async fn keys_agent(rig: &mut Rig, conn: u64, session: &str, pid: u32) {
        let (to_agent, rx) = mpsc::channel(64);
        rig._to_agent.push(rx);
        let register = Register {
            session_id: session.into(),
            host: "box".into(),
            cwd: CWD.into(),
            claude_pid: Some(pid),
            verdict_ack: false,
            transcript_reads: false,
            console_keys: true,
            console_commands: false,
            client: None,
            files: false,
            session_reads: false,
            status_lines: false,
            heartbeat: false,
        };
        rig.agents
            .send(AgentEvent::Registered {
                conn,
                register,
                to_agent,
            })
            .await
            .unwrap();
    }

    fn tool_start(session: &str, step: u64) -> HookPost {
        hook(
            session,
            HookEvent::ToolStart {
                tool_use_id: format!("t{step}"),
                line: format!("• Bash: step {step}"),
            },
        )
    }

    /// The ops once they satisfy `ready`; fails when that takes longer than
    /// `limit` of (paused) time.
    async fn within(
        rig: &Rig,
        limit: Duration,
        what: &str,
        ready: impl Fn(&[Op]) -> bool,
    ) -> Vec<Op> {
        let start = Instant::now();
        loop {
            let ops = rig.fake.ops();
            if ready(&ops) {
                return ops;
            }
            assert!(
                Instant::now() - start <= limit,
                "{what} took longer than {limit:?}"
            );
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
    }

    /// When each recorded op was first seen, sampled every 100 ms.
    fn op_times(fake: Arc<Fake>) -> Arc<Mutex<Vec<Instant>>> {
        let times = Arc::new(Mutex::new(Vec::new()));
        let seen = times.clone();
        tokio::spawn(async move {
            loop {
                let count = fake.ops().len();
                if let Ok(mut seen) = seen.lock() {
                    let now = Instant::now();
                    while seen.len() < count {
                        seen.push(now);
                    }
                }
                tokio::time::sleep(Duration::from_millis(100)).await;
            }
        });
        times
    }

    /// Status messages sent so far: (topic, message id Telegram gave).
    fn status_messages(ops: &[Op]) -> Vec<(i64, i64)> {
        ops.iter()
            .filter(|op| matches!(op, Op::Send { .. }))
            .zip(1000..)
            .filter_map(|(op, message_id)| match op {
                Op::Send {
                    thread_id: Some(thread),
                    reply_markup: Some(_),
                    permission: false,
                    ..
                } => Some((*thread, message_id)),
                _ => None,
            })
            .collect()
    }

    /// The topic Telegram gave the session with short id `n`.
    fn topic_of(ops: &[Op], n: u32) -> Option<i64> {
        let short = format!("{n:08x}");
        ops.iter()
            .filter(|op| is_create(op))
            .zip(100..)
            .find_map(|(op, topic)| match op {
                Op::CreateTopic { name, .. } if name.ends_with(&short) => Some(topic),
                _ => None,
            })
    }

    fn has_button(markup: Option<&serde_json::Value>, data: &str) -> bool {
        markup.is_some_and(|markup| markup.to_string().contains(data))
    }

    /// The hub's real pacing (`Limits::default()`): ten sessions whose status
    /// changes every 2 s keep the edit budget busy for good, and still a new
    /// session gets its topic, a permission prompt and its decision show,
    /// ⏹ asks and interrupts, and every status message is refreshed.
    #[tokio::test(start_paused = true)]
    async fn with_the_hubs_pacing_topics_prompts_and_stop_stay_quick_under_status_churn() {
        let options = Options {
            status_every: Some(STATUS_EVERY),
            ..message_options()
        };
        let mut rig = rig_with(
            Fake::default(),
            options,
            TempDir::new("slots-paced"),
            Limits::default(),
        );
        let times = op_times(rig.fake.clone());
        // Session 0 asks for a permission; sessions 1..=10 churn.
        for n in 0..=10u32 {
            let session = paced_session(n);
            rig.hook(start(&session, 10 + n)).await;
            keys_agent(&mut rig, u64::from(n) + 1, &session, 10 + n).await;
            rig.hook(hook(
                &session,
                HookEvent::UserPromptSubmit { prompt_id: None },
            ))
            .await;
            rig.hook(tool_start(&session, 0)).await;
        }
        within(
            &rig,
            Duration::from_secs(300),
            "eleven pinned status messages",
            |ops| count(ops, |op| matches!(op, Op::Pin { .. })) == 11,
        )
        .await;
        let hooks = rig.hooks.clone();
        let churn = tokio::spawn(async move {
            for step in 1u64.. {
                for n in 1..=10u32 {
                    let session = paced_session(n);
                    let _ = hooks.send(tool_start(&session, step)).await;
                    let end = HookEvent::ToolEnd {
                        tool_use_id: format!("t{}", step - 1),
                    };
                    let _ = hooks.send(hook(&session, end)).await;
                }
                tokio::time::sleep(Duration::from_secs(2)).await;
            }
        });
        let churn_from = Instant::now();
        tokio::time::sleep(Duration::from_secs(30)).await;

        // A new session: its topic comes at once, not after the refreshes.
        rig.hook(start(&paced_session(11), 21)).await;
        within(&rig, Duration::from_secs(10), "the new topic", |ops| {
            topic_of(ops, 11).is_some()
        })
        .await;

        // A permission prompt and the decision on it.
        rig.agents.send(permission(1, "abcde", "p")).await.unwrap();
        let ops = within(&rig, Duration::from_secs(5), "the prompt", |ops| {
            prompts(ops).len() == 1
        })
        .await;
        let (_, _, prompt_id) = prompts(&ops).remove(0);
        tokio::time::sleep(Duration::from_millis(100)).await;
        rig.control
            .send(press("q1", Some(prompt_id), "allow:abcde"))
            .unwrap();
        // Two edit tokens at most: a topic call (the ❓ icon, the new
        // session's pin) may take its turn first.
        within(&rig, Duration::from_secs(8), "the decision edit", |ops| {
            edits_of(ops, prompt_id)
                .iter()
                .any(|(text, _)| text.contains(permissions::ALLOWED_MARK))
        })
        .await;

        // ⏹ on session 1 right after that: the question shows within two
        // edit tokens (the icon edit after the decision may take its turn first), inside its 10 s
        // even counted from the press, and the second press interrupts.
        let ops = rig.fake.ops();
        let topic = topic_of(&ops, 1).expect("topic of session 1");
        let (_, status_id) = status_messages(&ops)
            .into_iter()
            .find(|(thread, _)| *thread == topic)
            .expect("status message of session 1");
        rig.control
            .send(press("q2", Some(status_id), "status:stop"))
            .unwrap();
        within(&rig, Duration::from_secs(8), "the ⏹ question", |ops| {
            edits_of(ops, status_id)
                .iter()
                .any(|(_, markup)| has_button(markup.as_ref(), "status:confirm"))
        })
        .await;
        rig.control
            .send(press("q3", Some(status_id), "status:confirm"))
            .unwrap();
        let ops = within(&rig, Duration::from_secs(5), "both answers", |ops| {
            answers(ops).len() == 3
        })
        .await;
        assert_eq!(
            answers(&ops)[1..],
            [
                Some(status::ANSWER_CONFIRM),
                Some(status::ANSWER_INTERRUPTING)
            ]
        );
        let got = received(&mut rig, 1).await;
        assert!(
            got.iter()
                .any(|msg| matches!(msg, HubMsg::ConsoleKey { .. })),
            "{got:?}"
        );

        // Every churning slot's status goes on being refreshed.
        tokio::time::sleep(Duration::from_secs(150)).await;
        churn.abort();
        let ops = rig.fake.ops();
        let times = times.lock().map(|times| times.clone()).unwrap_or_default();
        let statuses = status_messages(&ops);
        for n in 1..=10u32 {
            let topic = topic_of(&ops, n).expect("topic");
            let (_, status_id) = statuses
                .iter()
                .find(|(thread, _)| *thread == topic)
                .copied()
                .expect("status message");
            let edited: Vec<Instant> = ops
                .iter()
                .zip(&times)
                .filter(
                    |(op, _)| matches!(op, Op::Edit { message_id, .. } if *message_id == status_id),
                )
                .map(|(_, at)| *at)
                .filter(|at| *at >= churn_from)
                .collect();
            assert!(edited.len() >= 3, "session {n}: {} refreshes", edited.len());
            for pair in edited.windows(2) {
                assert!(
                    pair[1] - pair[0] <= Duration::from_secs(90),
                    "session {n}: {:?} between refreshes",
                    pair[1] - pair[0]
                );
            }
        }
    }

    // ------------------------------------------------------------ TASK-040

    const HUB_BUILD: &str = "a0a0a0a0b1b1b1b1c2c2c2c2d3d3d3d3e4e4e4e4f5f5f5f5a6a6a6a6b7b7b7b7";
    const OLD_BUILD: &str = "0101010102020202030303030404040405050505060606060707070708080808";

    fn client(build: &str, self_update: bool) -> Option<Client> {
        Some(Client {
            version: "0.1.0".into(),
            build: build.into(),
            self_update,
        })
    }

    /// A live session A in slot 0 with topic 100; the hub runs `HUB_BUILD`.
    fn updating_slots(dir: &TempDir) -> (Arc<Fake>, Slots) {
        let options = Options {
            build: Some(HUB_BUILD.into()),
            ..options()
        };
        let (fake, mut slots) = live_slots(dir, options);
        slots.on_hook(&start(A, 10));
        slots.registry.topic_created(SlotId(0), 100, "a", None);
        (fake, slots)
    }

    fn register_client(
        slots: &mut Slots,
        conn: u64,
        client: Option<Client>,
    ) -> mpsc::Receiver<HubMsg> {
        let (to_agent, from_hub) = mpsc::channel(8);
        slots.on_agent(AgentEvent::Registered {
            conn,
            register: Register {
                session_id: A.into(),
                host: "box".into(),
                cwd: CWD.into(),
                claude_pid: Some(10),
                verdict_ack: false,
                transcript_reads: false,
                console_keys: true,
                console_commands: false,
                client,
                files: false,
                session_reads: false,
                status_lines: false,
                heartbeat: false,
            },
            to_agent,
        });
        from_hub
    }

    fn answer(slots: &mut Slots, conn: u64, update_id: u64, outcome: UpdateOutcome) {
        slots.on_agent(AgentEvent::Message {
            conn,
            received_at: StdInstant::now(),
            msg: AgentMsg::UpdateAnswer { update_id, outcome },
        });
    }

    async fn sent_texts(fake: &Fake) -> Vec<(String, Option<serde_json::Value>)> {
        tokio::time::sleep(Duration::from_millis(200)).await;
        fake.ops()
            .into_iter()
            .filter_map(|op| match op {
                Op::Send {
                    thread_id: Some(100),
                    text,
                    reply_markup,
                    ..
                } => Some((text, reply_markup)),
                _ => None,
            })
            .collect()
    }

    /// TASK-035: a hub in a Linux container and a Windows client built from
    /// one commit report the same build (`client::identity`); another commit
    /// gives exactly one warning, which names both commits.
    #[tokio::test]
    async fn one_commit_on_two_systems_is_current_and_another_commit_is_warned_once() {
        const COMMIT: &str = "0123456789abcdef0123456789abcdef01234567";
        const EARLIER: &str = "fedcba9876543210fedcba9876543210fedcba98";
        let linux_hub = crate::client::identity(COMMIT, || Some("11".repeat(32)));
        let windows_client = crate::client::identity(COMMIT, || Some("22".repeat(32)));
        let dir = TempDir::new("slots-commit-build");
        let options = Options {
            build: linux_hub,
            ..options()
        };
        let (fake, mut slots) = live_slots(&dir, options);
        slots.on_hook(&start(A, 10));
        slots.registry.topic_created(SlotId(0), 100, "a", None);
        let windows = windows_client.clone().unwrap();
        let _same = register_client(&mut slots, 1, client(&windows, true));
        slots.pump();
        slots.pump();
        assert!(
            sent_texts(&fake).await.is_empty(),
            "same commit: no warning"
        );
        assert!(!slots.outdated(1));

        let _older = register_client(&mut slots, 2, client(EARLIER, true));
        slots.pump();
        slots.pump();
        let warnings = sent_texts(&fake).await;
        assert_eq!(warnings.len(), 1, "{warnings:?}");
        assert!(
            warnings[0].0.contains("fedcba98") && warnings[0].0.contains("01234567"),
            "{}",
            warnings[0].0
        );
        slots.pump();
        assert_eq!(sent_texts(&fake).await.len(), 1, "warned once");
    }

    #[tokio::test]
    async fn an_outdated_client_is_warned_once_and_updates_only_on_a_press_after_the_turn() {
        let dir = TempDir::new("slots-update-flow");
        let (fake, mut slots) = updating_slots(&dir);
        let mut first = register_client(&mut slots, 1, client(OLD_BUILD, true));
        slots.on_hook(&hook(A, HookEvent::UserPromptSubmit { prompt_id: None }));
        slots.pump();
        slots.pump();
        let warnings = sent_texts(&fake).await;
        assert_eq!(warnings.len(), 1, "one warning: {warnings:?}");
        assert!(
            fake.ops().iter().any(|op| matches!(
                op,
                Op::Send {
                    thread_id: Some(100),
                    notify: true,
                    ..
                }
            )),
            "the warning is loud (decision 2026-09-24)"
        );
        let (text, keyboard) = &warnings[0];
        assert!(
            text.contains("01010101") && text.contains("a0a0a0a0"),
            "{text}"
        );
        assert_eq!(keyboard, &status::update_keyboard(A));
        assert_eq!(
            slots.registry.sessions[A].update_warned.as_deref(),
            Some(HUB_BUILD)
        );
        let (text, keyboard) = slots.status_view(SlotId(0), A, Instant::now());
        assert!(text.ends_with(status::OUTDATED_LINE), "{text}");
        assert!(keyboard.to_string().contains("status:update"), "{keyboard}");
        // Nothing goes to the agent without a press.
        assert!(first.try_recv().is_err());

        // A press during the turn waits for its end.
        assert_eq!(slots.press_update(A), status::ANSWER_AFTER_TURN);
        assert_eq!(slots.press_update(A), status::ANSWER_UPDATE_RUNNING);
        slots.pump();
        assert!(first.try_recv().is_err(), "the turn runs");
        slots.on_hook(&hook(
            A,
            HookEvent::Stop {
                prompt_id: None,
                last_assistant_message: None,
            },
        ));
        slots.pump();
        // A hub built without a release tag sends none (TASK-050).
        let Ok(HubMsg::Update {
            update_id,
            release: None,
        }) = first.try_recv()
        else {
            panic!("no update after the turn");
        };

        // The agent hands over: unbound at once, released behind its queue.
        answer(&mut slots, 1, update_id, UpdateOutcome::Reloading);
        assert_eq!(
            first.try_recv().ok(),
            Some(HubMsg::Released {
                update_id,
                session_id: A.into()
            })
        );
        assert_eq!(slots.live_agent(SlotId(0)), None);
        slots.follow_pid("box", 10);
        assert_eq!(slots.live_agent(SlotId(0)), None, "never rebound by pid");

        // The next agent runs the hub's build and is asked again.
        let mut second = register_client(&mut slots, 2, client(HUB_BUILD, true));
        slots.pump();
        let Ok(HubMsg::Update { update_id, .. }) = second.try_recv() else {
            panic!("no second round");
        };
        answer(&mut slots, 2, update_id, UpdateOutcome::UpToDate);
        assert!(slots.updates.is_empty());
        let (text, _) = slots.status_view(SlotId(0), A, Instant::now());
        assert!(!text.contains(status::OUTDATED_LINE), "{text}");
        slots.pump();
        let texts = sent_texts(&fake).await;
        assert_eq!(texts.len(), 2, "{texts:?}");
        assert_eq!(texts[1].0, status::UPDATED_NOTICE);
        assert_eq!(slots.press_update(A), status::ANSWER_CURRENT);
    }

    /// TASK-050: a hub built for a release sends its tag with `update` to
    /// an outdated agent only; a failed download ends the press with its
    /// own notice and the agent stays bound.
    #[tokio::test]
    async fn a_release_hub_sends_its_tag_to_outdated_agents_and_tells_download_failures() {
        let dir = TempDir::new("slots-update-release");
        let options = Options {
            build: Some(HUB_BUILD.into()),
            release: Some("v0.1.3".into()),
            ..options()
        };
        let (fake, mut slots) = live_slots(&dir, options);
        slots.on_hook(&start(A, 10));
        slots.registry.topic_created(SlotId(0), 100, "a", None);
        let mut first = register_client(&mut slots, 1, client(OLD_BUILD, true));
        let failures = [
            (
                UpdateOutcome::DownloadFailed,
                status::DOWNLOAD_FAILED_NOTICE,
            ),
            (UpdateOutcome::ChecksumMismatch, status::CHECKSUM_NOTICE),
            (
                UpdateOutcome::NoReleaseBuild,
                status::NO_RELEASE_BUILD_NOTICE,
            ),
        ];
        for (outcome, _) in failures {
            assert_eq!(slots.press_update(A), status::ANSWER_UPDATING);
            slots.pump();
            let Ok(HubMsg::Update { update_id, release }) = first.try_recv() else {
                panic!("no update");
            };
            assert_eq!(release.as_deref(), Some("v0.1.3"));
            answer(&mut slots, 1, update_id, outcome);
            assert!(slots.updates.is_empty(), "{outcome:?} ends the press");
            assert_eq!(slots.live_agent(SlotId(0)), Some((A.to_owned(), 1)));
        }
        slots.pump();
        let texts: Vec<String> = sent_texts(&fake)
            .await
            .into_iter()
            .map(|(text, _)| text)
            .collect();
        for (outcome, notice) in failures {
            assert!(
                texts.iter().any(|text| text == notice),
                "{outcome:?}: {texts:?}"
            );
        }
        // Downloaded and handed over: the next agent runs the hub's build
        // and is asked only about a restart, without the tag.
        assert_eq!(slots.press_update(A), status::ANSWER_UPDATING);
        slots.pump();
        let Ok(HubMsg::Update { update_id, .. }) = first.try_recv() else {
            panic!("no update");
        };
        answer(&mut slots, 1, update_id, UpdateOutcome::Reloading);
        assert!(matches!(first.try_recv(), Ok(HubMsg::Released { .. })));
        let mut second = register_client(&mut slots, 2, client(HUB_BUILD, true));
        slots.pump();
        let Ok(HubMsg::Update { release, .. }) = second.try_recv() else {
            panic!("no second round");
        };
        assert_eq!(release, None, "a current agent downloads nothing");
    }

    #[tokio::test]
    async fn a_refused_restart_binds_the_agent_back_and_old_clients_are_told() {
        let dir = TempDir::new("slots-update-refused");
        let (fake, mut slots) = updating_slots(&dir);
        let mut agent = register_client(&mut slots, 1, client(OLD_BUILD, true));
        assert_eq!(slots.press_update(A), status::ANSWER_UPDATING);
        slots.pump();
        let Ok(HubMsg::Update { update_id, .. }) = agent.try_recv() else {
            panic!("no update");
        };
        answer(&mut slots, 1, update_id, UpdateOutcome::Restarting);
        assert!(matches!(agent.try_recv(), Ok(HubMsg::Released { .. })));
        assert_eq!(slots.live_agent(SlotId(0)), None);
        // A draft in the terminal: the agent stays and the topic is told.
        answer(&mut slots, 1, update_id, UpdateOutcome::DraftInInput);
        assert_eq!(slots.live_agent(SlotId(0)), Some((A.to_owned(), 1)));
        assert!(!slots.conns[&1].leaving);
        slots.pump();
        let texts = sent_texts(&fake).await;
        assert!(
            texts.iter().any(|(text, _)| text == status::DRAFT_NOTICE),
            "{texts:?}"
        );
        // A client too old to update itself: told, nothing sent.
        let mut old = register_client(&mut slots, 2, None);
        assert_eq!(slots.press_update(A), status::ANSWER_OLD_CLIENT);
        slots.pump();
        assert!(old.try_recv().is_err());
    }

    #[tokio::test]
    async fn a_press_outlasting_its_wait_in_a_turn_is_kept_and_told_once() {
        let dir = TempDir::new("slots-update-long-turn");
        let (fake, mut slots) = updating_slots(&dir);
        let mut first = register_client(&mut slots, 1, client(OLD_BUILD, true));
        slots.on_hook(&hook(A, HookEvent::UserPromptSubmit { prompt_id: None }));
        assert_eq!(slots.press_update(A), status::ANSWER_AFTER_TURN);
        let before = sent_texts(&fake).await.len();
        for _ in 0..2 {
            slots.updates.get_mut(A).unwrap().until = Instant::now();
            slots.pump();
        }
        assert!(slots.updates.contains_key(A), "kept while the turn runs");
        assert!(first.try_recv().is_err(), "the turn runs");
        let texts = sent_texts(&fake).await;
        let told: Vec<_> = texts[before..]
            .iter()
            .filter(|(text, _)| text == status::UPDATE_WAITS_NOTICE)
            .collect();
        assert_eq!(told.len(), 1, "told once: {texts:?}");
        slots.on_hook(&hook(
            A,
            HookEvent::Stop {
                prompt_id: None,
                last_assistant_message: None,
            },
        ));
        slots.pump();
        assert!(
            matches!(first.try_recv(), Ok(HubMsg::Update { .. })),
            "the update goes after the turn"
        );
    }

    #[tokio::test]
    async fn a_restart_is_held_back_when_a_turn_began_meanwhile() {
        let dir = TempDir::new("slots-update-restart-turn");
        let (fake, mut slots) = updating_slots(&dir);
        let mut agent = register_client(&mut slots, 1, client(OLD_BUILD, true));
        assert_eq!(slots.press_update(A), status::ANSWER_UPDATING);
        slots.pump();
        let Ok(HubMsg::Update { update_id, .. }) = agent.try_recv() else {
            panic!("no update");
        };
        // A prompt from the terminal between `update` and its answer.
        slots.on_hook(&hook(A, HookEvent::UserPromptSubmit { prompt_id: None }));
        answer(&mut slots, 1, update_id, UpdateOutcome::Restarting);
        assert!(
            agent.try_recv().is_err(),
            "not released: no /exit into the turn"
        );
        assert_eq!(slots.live_agent(SlotId(0)), Some((A.to_owned(), 1)));
        assert!(!slots.conns[&1].leaving);
        // The agent gives that `update` up by itself: noted, nothing told.
        let before = sent_texts(&fake).await.len();
        answer(&mut slots, 1, update_id, UpdateOutcome::Failed);
        assert!(
            slots.updates.contains_key(A),
            "the press waits for the turn"
        );
        slots.pump();
        assert!(agent.try_recv().is_err(), "the turn runs");
        slots.on_hook(&hook(
            A,
            HookEvent::Stop {
                prompt_id: None,
                last_assistant_message: None,
            },
        ));
        slots.pump();
        let Ok(HubMsg::Update { update_id, .. }) = agent.try_recv() else {
            panic!("no update after the turn");
        };
        answer(&mut slots, 1, update_id, UpdateOutcome::Restarting);
        assert!(matches!(agent.try_recv(), Ok(HubMsg::Released { .. })));
        let texts = sent_texts(&fake).await;
        assert_eq!(
            texts.len(),
            before,
            "no notice for the held round: {texts:?}"
        );
    }

    // ------------------------------------------------------------ TASK-047

    fn update_of(agent: &mut mpsc::Receiver<HubMsg>) -> u64 {
        match agent.try_recv() {
            Ok(HubMsg::Update { update_id, .. }) => update_id,
            other => panic!("no update: {other:?}"),
        }
    }

    /// The press waits for the retry time to pass.
    fn retry_now(slots: &mut Slots) {
        slots.updates.get_mut(A).unwrap().retry_at = Some(Instant::now());
        slots.pump();
    }

    #[tokio::test]
    async fn a_restart_waits_for_background_agents_tells_once_and_is_asked_again() {
        let dir = TempDir::new("slots-update-agents");
        let (fake, mut slots) = updating_slots(&dir);
        let mut agent = register_client(&mut slots, 1, client(OLD_BUILD, true));
        assert_eq!(slots.press_update(A), status::ANSWER_UPDATING);
        slots.pump();
        let before = sent_texts(&fake).await.len();
        // More answers than rounds: a wait for agents never uses one up.
        for _ in 0..UPDATE_ROUNDS + 1 {
            let update_id = update_of(&mut agent);
            // The agent saw the agent list before leaving: it stays bound.
            answer(&mut slots, 1, update_id, UpdateOutcome::AgentsRunning);
            assert_eq!(slots.live_agent(SlotId(0)), Some((A.to_owned(), 1)));
            assert!(agent.try_recv().is_err(), "not released");
            slots.pump();
            assert!(agent.try_recv().is_err(), "asked again only later");
            retry_now(&mut slots);
        }
        // Found only right before `/exit`, after leaving: bound back.
        let update_id = update_of(&mut agent);
        answer(&mut slots, 1, update_id, UpdateOutcome::Restarting);
        assert!(matches!(agent.try_recv(), Ok(HubMsg::Released { .. })));
        assert_eq!(slots.live_agent(SlotId(0)), None);
        answer(&mut slots, 1, update_id, UpdateOutcome::AgentsRunning);
        assert_eq!(slots.live_agent(SlotId(0)), Some((A.to_owned(), 1)));
        assert!(!slots.conns[&1].leaving);
        assert!(slots.updates.contains_key(A), "the press is kept");
        retry_now(&mut slots);
        let update_id = update_of(&mut agent);
        // The agents finished: the restart goes through.
        answer(&mut slots, 1, update_id, UpdateOutcome::Restarting);
        assert!(matches!(agent.try_recv(), Ok(HubMsg::Released { .. })));
        let texts = sent_texts(&fake).await;
        let told: Vec<_> = texts[before..]
            .iter()
            .filter(|(text, _)| text == status::UPDATE_AGENTS_NOTICE)
            .collect();
        assert_eq!(told.len(), 1, "told once: {texts:?}");
        assert_eq!(texts.len(), before + 1, "nothing else: {texts:?}");
    }

    #[tokio::test]
    async fn a_command_refused_for_background_agents_is_answered() {
        let dir = TempDir::new("slots-console-agents");
        let (fake, mut slots, mut from_hub) = console_slots(&dir, true);
        slots.on_topic_message(topic_text(41, "/compact", false));
        let (command_id, _) = command_of(from_hub.try_recv().ok());
        slots.on_command_typed(1, A, command_id, CommandOutcome::AgentsRunning, None);
        assert_eq!(
            command_replies(&fake, 1).await,
            [(41, console::AGENTS_NOTICE.to_owned())]
        );
    }

    /// The claude of session A exits after `/exit` and `cctg run` starts it
    /// again with `--resume`; its new agent is `conn`.
    fn restart_run(slots: &mut Slots, old: u64, conn: u64) -> mpsc::Receiver<HubMsg> {
        slots.on_agent(AgentEvent::Disconnected { conn: old });
        slots.on_hook(&end(A, 10));
        let agent = register_client(slots, conn, client(HUB_BUILD, true));
        slots.on_hook(&hook(
            A,
            HookEvent::SessionStart {
                source: Some("resume".into()),
                claude_pid: Some(10),
                parent_claude_pid: None,
            },
        ));
        slots.pump();
        agent
    }

    fn inbound_texts(agent: &mut mpsc::Receiver<HubMsg>) -> Vec<String> {
        let mut texts = Vec::new();
        while let Ok(msg) = agent.try_recv() {
            if let HubMsg::Inbound { content, .. } = msg {
                texts.push(content);
            }
        }
        texts
    }

    #[tokio::test]
    async fn a_restart_that_cut_off_work_tells_the_next_agent_once() {
        let dir = TempDir::new("slots-update-continue");
        let (_fake, mut slots) = updating_slots(&dir);
        let mut first = register_client(&mut slots, 1, client(OLD_BUILD, true));
        slots.on_hook(&hook(A, HookEvent::UserPromptSubmit { prompt_id: None }));
        assert_eq!(slots.press_update(A), status::ANSWER_AFTER_TURN);
        // ⏹ while the press waits: Esc written, the turn's end not seen yet.
        assert!(slots.send_key(SlotId(0), A, 1));
        let Ok(HubMsg::ConsoleKey { key_id, .. }) = first.try_recv() else {
            panic!("no key");
        };
        slots.on_key_written(1, A, key_id, true);
        // The turn's end is seen before the restart: the ⏹ during the press
        // alone says that work was cut off.
        slots.on_hook(&stop(A, None));
        slots.pump();
        let update_id = update_of(&mut first);
        answer(&mut slots, 1, update_id, UpdateOutcome::Restarting);
        assert!(matches!(first.try_recv(), Ok(HubMsg::Released { .. })));
        assert!(slots.registry.sessions[A].restart_interrupted);
        // The flag lives in registry.json until the next agent took it.
        let saved = String::from_utf8(RegistryStore::encode(&slots.registry)).unwrap();
        assert!(saved.contains("restart_interrupted"), "{saved}");

        let mut second = restart_run(&mut slots, 1, 2);
        let texts = inbound_texts(&mut second);
        assert_eq!(texts, [CONTINUE_TEXT], "one message after the restart");
        assert!(!slots.registry.sessions[A].restart_interrupted);
        slots.pump();
        assert!(inbound_texts(&mut second).is_empty(), "only once");
    }

    #[tokio::test]
    async fn an_idle_restart_and_a_refused_one_tell_the_session_nothing() {
        let dir = TempDir::new("slots-update-no-continue");
        let (_fake, mut slots) = updating_slots(&dir);
        // A turn stopped by ⏹, then a restart that did not happen (a draft):
        // the agent stays and nothing is sent.
        let mut first = register_client(&mut slots, 1, client(OLD_BUILD, true));
        slots.on_hook(&hook(A, HookEvent::UserPromptSubmit { prompt_id: None }));
        assert_eq!(slots.press_update(A), status::ANSWER_AFTER_TURN);
        assert!(slots.send_key(SlotId(0), A, 1));
        let Ok(HubMsg::ConsoleKey { key_id, .. }) = first.try_recv() else {
            panic!("no key");
        };
        slots.on_key_written(1, A, key_id, true);
        slots.pump();
        let update_id = update_of(&mut first);
        answer(&mut slots, 1, update_id, UpdateOutcome::Restarting);
        assert!(matches!(first.try_recv(), Ok(HubMsg::Released { .. })));
        answer(&mut slots, 1, update_id, UpdateOutcome::DraftInInput);
        assert!(!slots.registry.sessions[A].restart_interrupted);
        slots.pump();
        assert!(inbound_texts(&mut first).is_empty());

        // The turn ended; a restart in the idle session.
        slots.on_hook(&stop(A, None));
        assert_eq!(slots.press_update(A), status::ANSWER_UPDATING);
        slots.pump();
        let update_id = update_of(&mut first);
        answer(&mut slots, 1, update_id, UpdateOutcome::Restarting);
        assert!(matches!(first.try_recv(), Ok(HubMsg::Released { .. })));
        assert!(!slots.registry.sessions[A].restart_interrupted);
        let mut second = restart_run(&mut slots, 1, 2);
        slots.pump();
        assert!(inbound_texts(&mut second).is_empty(), "idle: nothing");
    }

    const WORKER: &str = "a2e9235bfcf0c0407";

    async fn agents_notices(fake: &Fake) -> usize {
        sent_texts(fake)
            .await
            .iter()
            .filter(|(text, _)| text == status::UPDATE_AGENTS_NOTICE)
            .count()
    }

    #[tokio::test]
    async fn a_subagent_seen_running_holds_the_update_until_it_stops() {
        let dir = TempDir::new("slots-update-hub-agents");
        let (fake, mut slots) = updating_slots(&dir);
        let mut agent = register_client(&mut slots, 1, client(OLD_BUILD, true));
        // Claude Code's internal agents (untyped) never count.
        slots.on_hook(&sub_start(A, "b0000000000000001", " "));
        slots.on_hook(&sub_start(A, WORKER, "general-purpose"));
        assert_eq!(slots.agents_running(A), [WORKER]);
        assert_eq!(slots.press_update(A), status::ANSWER_UPDATING);
        for _ in 0..3 {
            slots.pump();
            assert!(agent.try_recv().is_err(), "no update while it runs");
        }
        assert_eq!(agents_notices(&fake).await, 1, "told once");
        let path = dir.path().join("agent-worker.jsonl");
        slots.on_hook(&sub_stop(A, WORKER, "general-purpose", &path, "done"));
        assert!(slots.agents_running(A).is_empty());
        slots.pump();
        update_of(&mut agent);
        assert_eq!(agents_notices(&fake).await, 1);
    }

    #[tokio::test]
    async fn a_lost_subagent_stop_holds_the_update_only_so_long() {
        let dir = TempDir::new("slots-update-hub-agents-stale");
        let (_fake, mut slots) = updating_slots(&dir);
        let mut agent = register_client(&mut slots, 1, client(OLD_BUILD, true));
        slots.on_hook(&sub_start(A, WORKER, "general-purpose"));
        assert_eq!(slots.press_update(A), status::ANSWER_UPDATING);
        slots.pump();
        assert!(agent.try_recv().is_err());
        // Its age limit comes: the actor wakes then, and it no longer counts.
        let limit = Instant::now() + Duration::from_millis(50);
        slots.started_agents.get_mut(WORKER).unwrap().1 = limit;
        assert!(slots.next_deadline() <= limit);
        slots.pump();
        assert!(agent.try_recv().is_err());
        tokio::time::sleep_until(limit).await;
        slots.pump();
        update_of(&mut agent);

        // A start the parent transcript never confirmed (the candidate was
        // dropped at its window's end) does not count either.
        slots.on_hook(&sub_start(A, "a0000000000000003", "maw-qa"));
        assert_eq!(slots.agents_running(A), ["a0000000000000003"]);
        slots.candidates.take("a0000000000000003");
        assert!(slots.agents_running(A).is_empty());
        // A session's end drops its subagents.
        slots.on_hook(&sub_start(A, "a0000000000000004", "maw-qa"));
        slots.on_hook(&end(A, 10));
        assert!(slots.started_agents.is_empty());
    }

    #[tokio::test]
    async fn a_restart_that_stops_subagents_names_them_in_its_message() {
        let dir = TempDir::new("slots-update-continue-agents");
        let (_fake, mut slots) = updating_slots(&dir);
        let mut first = register_client(&mut slots, 1, client(OLD_BUILD, true));
        assert_eq!(slots.press_update(A), status::ANSWER_UPDATING);
        slots.pump();
        let update_id = update_of(&mut first);
        // The subagent started after the `update` went out; the session was
        // idle otherwise.
        slots.on_hook(&sub_start(A, WORKER, "general-purpose"));
        answer(&mut slots, 1, update_id, UpdateOutcome::Restarting);
        assert!(matches!(first.try_recv(), Ok(HubMsg::Released { .. })));
        assert!(slots.registry.sessions[A].restart_interrupted);
        assert_eq!(slots.registry.sessions[A].restart_agents, [WORKER]);
        let saved = String::from_utf8(RegistryStore::encode(&slots.registry)).unwrap();
        assert!(saved.contains(WORKER), "{saved}");

        let mut second = restart_run(&mut slots, 1, 2);
        let texts = inbound_texts(&mut second);
        assert_eq!(texts, [continue_text(&[WORKER.to_owned()])]);
        assert!(texts[0].contains(WORKER) && texts[0].contains("SendMessage"));
        assert!(slots.registry.sessions[A].restart_agents.is_empty());
        slots.pump();
        assert!(inbound_texts(&mut second).is_empty(), "only once");
    }

    #[test]
    fn the_continuation_names_stopped_subagents_only_when_there_are_some() {
        assert_eq!(continue_text(&[]), CONTINUE_TEXT);
        let text = continue_text(&["a1".into(), "b2".into()]);
        assert!(text.starts_with(CONTINUE_TEXT));
        assert!(text.contains(&format!("{CONTINUE_AGENTS_TEXT} a1, b2. ")));
        assert!(text.ends_with(CONTINUE_AGENTS_HOW));
    }

    /// TASK-042 rebinds a session to an older open link of its run when the
    /// bound one closes; a leaving worker is never that heir.
    #[tokio::test]
    async fn a_leaving_agent_is_never_the_heir_of_a_closed_link() {
        let dir = TempDir::new("slots-update-heir");
        let (_fake, mut slots) = updating_slots(&dir);
        let mut first = register_client(&mut slots, 1, client(OLD_BUILD, true));
        assert_eq!(slots.press_update(A), status::ANSWER_UPDATING);
        slots.pump();
        let Ok(HubMsg::Update { update_id, .. }) = first.try_recv() else {
            panic!("no update");
        };
        answer(&mut slots, 1, update_id, UpdateOutcome::Reloading);
        let _second = register_client(&mut slots, 2, client(HUB_BUILD, true));
        assert_eq!(slots.live_agent(SlotId(0)), Some((A.to_owned(), 2)));
        slots.on_agent(AgentEvent::Disconnected { conn: 2 });
        assert_eq!(
            slots.live_agent(SlotId(0)),
            None,
            "the leaving link stays unbound"
        );
    }

    #[tokio::test]
    async fn without_its_own_build_the_hub_never_calls_a_client_outdated() {
        let dir = TempDir::new("slots-update-unknown");
        let (fake, mut slots) = live_slots(&dir, options());
        slots.on_hook(&start(A, 10));
        slots.registry.topic_created(SlotId(0), 100, "a", None);
        let _agent = register_client(&mut slots, 1, None);
        slots.pump();
        assert!(sent_texts(&fake).await.is_empty());
        assert_eq!(slots.press_update(A), status::ANSWER_CURRENT);
        let (text, _) = slots.status_view(SlotId(0), A, Instant::now());
        assert!(!text.contains(status::OUTDATED_LINE));
    }

    /// A live session A in slot 0 with topic 100 and an agent (conn 1) that
    /// types console commands when `commands`.
    fn console_slots(dir: &TempDir, commands: bool) -> (Arc<Fake>, Slots, mpsc::Receiver<HubMsg>) {
        console_slots_with(dir, commands, message_options())
    }

    fn console_slots_with(
        dir: &TempDir,
        commands: bool,
        options: Options,
    ) -> (Arc<Fake>, Slots, mpsc::Receiver<HubMsg>) {
        let (fake, mut slots) = live_slots(dir, options);
        slots.on_hook(&start(A, 10));
        slots.registry.topic_created(SlotId(0), 100, "a", None);
        let (to_agent, from_hub) = mpsc::channel(8);
        slots.on_agent(AgentEvent::Registered {
            conn: 1,
            register: Register {
                session_id: A.into(),
                host: "box".into(),
                cwd: CWD.into(),
                claude_pid: Some(10),
                verdict_ack: false,
                transcript_reads: false,
                console_keys: true,
                console_commands: commands,
                client: None,
                files: false,
                session_reads: false,
                status_lines: false,
                heartbeat: false,
            },
            to_agent,
        });
        (fake, slots, from_hub)
    }

    fn topic_text(message_id: i64, text: &str, forwarded: bool) -> Inbound {
        Inbound {
            message_id,
            thread_id: Some(100),
            text: Some(text.into()),
            reply_to: None,
            quote: None,
            forwarded,
            media: None,
            from_name: None,
        }
    }

    /// The answers the hub sent as replies to topic messages, once `count`
    /// are out: `(replied message, text)`.
    async fn command_replies(fake: &Fake, count: usize) -> Vec<(i64, String)> {
        let replied = || {
            fake.ops()
                .into_iter()
                .filter_map(|op| match op {
                    Op::Send {
                        reply_to: Some(to),
                        text,
                        ..
                    } => Some((to, text)),
                    _ => None,
                })
                .collect::<Vec<_>>()
        };
        let reached = async {
            loop {
                let got = replied();
                if got.len() >= count {
                    return got;
                }
                tokio::time::sleep(Duration::from_millis(5)).await;
            }
        };
        match tokio::time::timeout(WAIT, reached).await {
            Ok(got) => got,
            Err(_) => panic!("replies never came: {:?}", fake.ops()),
        }
    }

    fn command_of(msg: Option<HubMsg>) -> (u64, String) {
        match msg {
            Some(HubMsg::ConsoleCommand { command_id, text }) => (command_id, text),
            other => panic!("no console command: {other:?}"),
        }
    }

    #[tokio::test]
    async fn bang_and_slash_commands_go_to_the_console_and_are_answered() {
        let dir = TempDir::new("slots-console-commands");
        let (fake, mut slots, mut from_hub) = console_slots(&dir, true);
        slots.on_topic_message(topic_text(11, "!echo hi", false));
        let (typed, text) = command_of(from_hub.try_recv().ok());
        assert_eq!(text, "!echo hi");
        slots.on_topic_message(topic_text(12, "/compact@cctg_bot keep it", false));
        let (drafted, text) = command_of(from_hub.try_recv().ok());
        assert_eq!(text, "/compact keep it");
        // Neither went to the model, nor waits in the slot.
        assert!(slots.registry.slots[0].buffer.messages.is_empty());
        // Typed: 👀 on the message. A draft in the box: an answer.
        slots.on_command_typed(1, A, typed, CommandOutcome::Sent, None);
        slots.on_command_typed(1, A, drafted, CommandOutcome::Draft, None);
        assert_eq!(
            command_replies(&fake, 1).await,
            [(12, console::DRAFT_NOTICE.to_owned())]
        );
        let reached = async {
            while !fake.ops().iter().any(
                |op| matches!(op, Op::React { message_id: 11, emoji } if emoji == stream::ACCEPTED),
            ) {
                tokio::time::sleep(Duration::from_millis(5)).await;
            }
        };
        tokio::time::timeout(WAIT, reached).await.expect("👀 on 11");
        // A late or repeated answer changes nothing.
        slots.on_command_typed(1, A, typed, CommandOutcome::Failed, None);
        // A plain text, a path and a forwarded bang are messages for the model.
        slots.on_topic_message(topic_text(13, "hello", false));
        slots.on_topic_message(topic_text(14, "/tmp/app.log fails", false));
        slots.on_topic_message(topic_text(15, "!echo hi", true));
        for want in ["hello", "/tmp/app.log fails", "!echo hi"] {
            match from_hub.try_recv() {
                Ok(HubMsg::Inbound { content, .. }) => assert!(content.contains(want), "{content}"),
                other => panic!("{other:?}"),
            }
        }
        assert_eq!(
            command_replies(&fake, 1).await.len(),
            1,
            "nothing more answered"
        );
    }

    #[tokio::test]
    async fn the_panel_a_command_opened_is_answered_monospace() {
        let dir = TempDir::new("slots-console-panel");
        let (fake, mut slots, mut from_hub) = console_slots(&dir, true);
        slots.on_topic_message(topic_text(31, "/cost", false));
        let (typed, _) = command_of(from_hub.try_recv().ok());
        slots.on_command_typed(
            1,
            A,
            typed,
            CommandOutcome::Sent,
            Some("Total cost: $0.01\nSession <1>".into()),
        );
        assert_eq!(
            command_replies(&fake, 1).await,
            [(31, "Total cost: $0.01\nSession <1>".to_owned())]
        );
        let html = fake.ops().into_iter().find_map(|op| match op {
            Op::Send {
                reply_to: Some(31),
                html,
                ..
            } => html,
            _ => None,
        });
        assert_eq!(
            html.as_deref(),
            Some("<pre>Total cost: $0.01\nSession &lt;1&gt;</pre>")
        );
    }

    #[tokio::test]
    async fn a_console_command_is_refused_in_a_turn_at_a_prompt_or_when_it_is_not_one_line() {
        let dir = TempDir::new("slots-console-refused");
        let (fake, mut slots, mut from_hub) = console_slots(&dir, true);
        slots.on_hook(&hook(A, HookEvent::UserPromptSubmit { prompt_id: None }));
        slots.on_topic_message(topic_text(21, "!ls", false));
        slots.on_hook(&stop(A, None));
        slots.on_agent(permission(1, "abcde", "p"));
        slots.on_topic_message(topic_text(22, "/model", false));
        slots.on_topic_message(topic_text(23, "!echo a\nb", false));
        slots.on_topic_message(topic_text(24, "/compact \u{1b}[A", false));
        let mut got = command_replies(&fake, 4).await;
        got.sort();
        assert_eq!(
            got,
            [
                (21, console::BUSY_NOTICE.to_owned()),
                (22, console::WAITING_NOTICE.to_owned()),
                (23, console::INVALID_NOTICE.to_owned()),
                (24, console::INVALID_NOTICE.to_owned()),
            ]
        );
        while let Ok(msg) = from_hub.try_recv() {
            assert!(
                !matches!(msg, HubMsg::ConsoleCommand { .. } | HubMsg::Inbound { .. }),
                "{msg:?}"
            );
        }
    }

    #[tokio::test]
    async fn a_console_command_needs_a_live_agent_that_types() {
        let dir = TempDir::new("slots-console-no-agent");
        let (fake, mut slots, mut from_hub) = console_slots(&dir, false);
        slots.on_topic_message(topic_text(31, "!echo hi", false));
        assert_eq!(
            command_replies(&fake, 1).await,
            [(31, console::NO_CONSOLE_NOTICE.to_owned())]
        );
        assert!(from_hub.try_recv().is_err());
        slots.on_agent(AgentEvent::Disconnected { conn: 1 });
        slots.on_topic_message(topic_text(32, "/compact", false));
        assert_eq!(
            command_replies(&fake, 2).await[1],
            (32, console::OFFLINE_NOTICE.to_owned())
        );
        // Not kept for a later session either.
        assert!(slots.registry.slots[0].buffer.messages.is_empty());
    }

    /// Telegram's files for the download task, by id; `big` is too big for
    /// a bot, any other unknown id fails.
    struct TelegramFiles(HashMap<String, Vec<u8>>);

    impl Fetch for TelegramFiles {
        async fn fetch(&self, file_id: &str, _limit: u64) -> Result<fetch::Download, ApiError> {
            match self.0.get(file_id) {
                Some(bytes) => Ok(fetch::Download {
                    bytes: bytes.clone(),
                    path: Some(format!("photos/{file_id}.jpg")),
                }),
                None if file_id == "big" => Err(ApiError::Telegram {
                    code: 400,
                    description: crate::hub::api::FILE_TOO_BIG.to_owned(),
                }),
                None => Err(ApiError::Telegram {
                    code: 400,
                    description: "Bad Request: wrong file_id".to_owned(),
                }),
            }
        }
    }

    fn photo(message_id: i64, file_id: &str, caption: Option<&str>, size: Option<u64>) -> Control {
        Control::Message(Inbound {
            message_id,
            thread_id: Some(100),
            text: None,
            reply_to: None,
            quote: None,
            forwarded: false,
            media: Some(crate::hub::updates::Media {
                file: Attachment {
                    kind: crate::wire::FileKind::Photo,
                    file_id: file_id.into(),
                    name: None,
                    size,
                },
                caption: caption.map(str::to_owned),
            }),
            from_name: None,
        })
    }

    /// Like [`connect_queue`] with room for 64, for an agent that takes
    /// files or not.
    fn connect_files(
        slots: &mut Slots,
        conn: u64,
        session: &str,
        claude_pid: Option<u32>,
        files: bool,
    ) -> mpsc::Receiver<HubMsg> {
        let (to_agent, from_hub) = mpsc::channel(64);
        slots.on_agent(AgentEvent::Registered {
            conn,
            register: Register {
                session_id: session.into(),
                host: "box".into(),
                cwd: CWD.into(),
                claude_pid,
                verdict_ack: false,
                transcript_reads: false,
                console_keys: false,
                console_commands: false,
                client: None,
                files,
                session_reads: false,
                status_lines: false,
                heartbeat: false,
            },
            to_agent,
        });
        from_hub
    }

    /// A live in slot 0 with topic 100, the download task on `files`;
    /// returns the actor, what it hands to the scheduler and the download
    /// task's answers.
    fn file_slots(
        dir: &TempDir,
        files: TelegramFiles,
    ) -> (
        Slots,
        mpsc::UnboundedReceiver<(Work, Op)>,
        mpsc::UnboundedReceiver<Done>,
    ) {
        let options = Options {
            notice_every: Duration::ZERO,
            ..message_options()
        };
        let mut slots = stalled_slots(dir, options);
        let work = capture_dispatch(&mut slots);
        slots.fetch_files(Arc::new(files));
        let done = slots.done_rx.take().unwrap();
        slots.on_hook(&start(A, 10));
        slots.registry.topic_created(SlotId(0), 100, "a", None);
        (slots, work, done)
    }

    async fn fetched(slots: &mut Slots, done: &mut mpsc::UnboundedReceiver<Done>) {
        let finished = tokio::time::timeout(WAIT, done.recv())
            .await
            .expect("a download answer in time")
            .expect("download task alive");
        slots.on_done(finished);
        slots.pump();
    }

    /// What reached the agent: `text <content>` per inbound, `file <name>
    /// <content>` per complete file (its bytes in `bytes`).
    fn arrived(from_hub: &mut mpsc::Receiver<HubMsg>, bytes: &mut Vec<Vec<u8>>) -> Vec<String> {
        let mut got = Vec::new();
        let mut open: Option<(String, String, files::Assembly)> = None;
        while let Ok(msg) = from_hub.try_recv() {
            match msg {
                HubMsg::Inbound { content, .. } => got.push(format!("text {content}")),
                HubMsg::FileStart {
                    name,
                    size,
                    content,
                    meta,
                    ..
                } => {
                    assert!(meta.contains_key("message_id") && meta.contains_key("chat_id"));
                    let assembly = files::Assembly::new(size);
                    if assembly.is_complete() {
                        got.push(format!("file {name} {content}"));
                        bytes.push(Vec::new());
                    } else {
                        open = Some((name, content, assembly));
                    }
                }
                HubMsg::FileChunk(chunk) => {
                    let (name, content, mut assembly) = open.take().expect("a started file");
                    if assembly.push(&chunk).unwrap() {
                        got.push(format!("file {name} {content}"));
                        bytes.push(assembly.into_bytes());
                    } else {
                        open = Some((name, content, assembly));
                    }
                }
                other => panic!("unexpected {other:?}"),
            }
        }
        assert!(open.is_none(), "a file came only in part");
        got
    }

    /// The texts sent to topic 100 and the messages reacted to, handed out
    /// since the last call.
    fn topic_ops(work: &mut mpsc::UnboundedReceiver<(Work, Op)>) -> (Vec<String>, Vec<i64>) {
        let (mut texts, mut reacted) = (Vec::new(), Vec::new());
        while let Ok((_, op)) = work.try_recv() {
            match op {
                Op::Send {
                    thread_id: Some(100),
                    text,
                    ..
                } => texts.push(text),
                Op::React { message_id, .. } => reacted.push(message_id),
                _ => {}
            }
        }
        (texts, reacted)
    }

    #[tokio::test]
    async fn a_kept_file_reaches_the_agent_in_its_place_among_the_messages() {
        let dir = TempDir::new("slots-file-order");
        let png: Vec<u8> = (0..files::CHUNK + 10).map(|n| (n % 251) as u8).collect();
        let (mut slots, mut work, mut done) =
            file_slots(&dir, TelegramFiles([("p".to_owned(), png.clone())].into()));
        let mut agent = connect_files(&mut slots, 1, A, Some(10), true);
        slots.on_control(say(Some(100), 1, Some("one")));
        slots.on_control(photo(2, "p", Some("look"), Some(png.len() as u64)));
        slots.on_control(say(Some(100), 3, Some("two")));
        slots.pump();
        // The file is on its way; the message after it waits behind it.
        assert_eq!(buffered(&slots, 0), [2, 3]);
        fetched(&mut slots, &mut done).await;
        let mut bytes = Vec::new();
        assert_eq!(
            arrived(&mut agent, &mut bytes),
            ["text one", "file photo.jpg look", "text two"]
        );
        assert_eq!(bytes, [png]);
        assert!(slots.registry.slots[0].buffer.is_idle());
        let (texts, reacted) = topic_ops(&mut work);
        assert!(texts.is_empty(), "{texts:?}");
        assert_eq!(reacted, [1, 2, 3]);
    }

    #[tokio::test]
    async fn an_agent_that_takes_no_files_gets_the_caption_and_the_topic_a_notice() {
        let dir = TempDir::new("slots-file-old-agent");
        let (mut slots, mut work, _done) = file_slots(
            &dir,
            TelegramFiles([("p".to_owned(), b"x".to_vec())].into()),
        );
        let mut agent = connect_files(&mut slots, 1, A, Some(10), false);
        slots.on_control(photo(2, "p", Some("look"), None));
        slots.on_control(photo(3, "p", None, None));
        slots.on_control(say(Some(100), 4, Some("after")));
        slots.pump();
        assert_eq!(
            arrived(&mut agent, &mut Vec::new()),
            ["text look", "text after"]
        );
        let (texts, reacted) = topic_ops(&mut work);
        assert_eq!(texts, [buffer::OLD_AGENT_NOTICE, buffer::OLD_AGENT_NOTICE]);
        assert_eq!(reacted, [2, 4]);
        assert!(slots.fetching.is_empty());
        assert!(slots.registry.slots[0].buffer.is_idle());
    }

    /// Ingress answers `registered` before this actor binds the agent, and
    /// topic messages come over another channel: messages handled before
    /// the binding wait in the slot and reach the agent right after it.
    #[tokio::test]
    async fn topic_messages_that_overtake_the_agents_registration_reach_it_after_binding() {
        let dir = TempDir::new("slots-file-overtake");
        let (mut slots, mut work, _done) = file_slots(
            &dir,
            TelegramFiles([("p".to_owned(), b"x".to_vec())].into()),
        );
        slots.on_control(photo(2, "p", Some("look"), None));
        slots.on_control(say(Some(100), 3, Some("after")));
        slots.pump();
        assert_eq!(buffered(&slots, 0), [2, 3]);
        let mut agent = connect_files(&mut slots, 1, A, Some(10), false);
        slots.pump();
        assert_eq!(
            arrived(&mut agent, &mut Vec::new()),
            ["text look", "text after"]
        );
        let (texts, reacted) = topic_ops(&mut work);
        assert_eq!(texts, [buffer::QUEUED_NOTICE, buffer::OLD_AGENT_NOTICE]);
        assert_eq!(reacted, [2, 3]);
        assert!(slots.registry.slots[0].buffer.is_idle());
    }

    #[tokio::test]
    async fn a_dead_slot_keeps_a_file_as_its_reference_and_hands_it_over_on_revival() {
        let dir = TempDir::new("slots-file-dead");
        let png = b"\x89PNG\r\n\x1a\nbytes".to_vec();
        let (mut slots, _work, mut done) =
            file_slots(&dir, TelegramFiles([("p".to_owned(), png.clone())].into()));
        slots.on_hook(&end(A, 10));
        slots.on_control(photo(2, "p", Some("look"), Some(png.len() as u64)));
        slots.pump();
        assert_eq!(buffered(&slots, 0), [2]);
        assert!(
            slots.fetching.is_empty(),
            "nothing is downloaded for a dead slot"
        );
        // registry.json keeps the reference, not the bytes.
        let saved: serde_json::Value =
            serde_json::from_slice(&RegistryStore::encode(&slots.registry)).unwrap();
        assert_eq!(
            saved["slots"][0]["buffer"]["messages"][0]["file"],
            serde_json::json!({ "kind": "photo", "file_id": "p", "size": 13 })
        );
        slots.on_hook(&resumed(A, 11));
        slots.pump();
        let mut agent = connect_files(&mut slots, 1, A, Some(11), true);
        slots.pump();
        fetched(&mut slots, &mut done).await;
        let mut bytes = Vec::new();
        assert_eq!(arrived(&mut agent, &mut bytes), ["file photo.jpg look"]);
        assert_eq!(bytes, [png]);
        assert!(slots.registry.slots[0].buffer.is_idle());
    }

    #[tokio::test]
    async fn a_file_too_big_or_not_downloaded_is_told_and_a_closed_link_keeps_it() {
        let dir = TempDir::new("slots-file-fail");
        let (mut slots, mut work, mut done) = file_slots(
            &dir,
            TelegramFiles([("p".to_owned(), b"x".to_vec())].into()),
        );
        let mut agent = connect_files(&mut slots, 1, A, Some(10), true);
        // Announced too big: told at once, never kept.
        slots.on_control(photo(1, "p", None, Some(files::MAX_DOWNLOAD + 1)));
        assert!(buffered(&slots, 0).is_empty());
        // Too big only by Telegram's answer, and a file Telegram does not give.
        slots.on_control(photo(2, "big", None, None));
        slots.on_control(photo(3, "gone", None, None));
        slots.on_control(say(Some(100), 4, Some("after")));
        slots.pump();
        fetched(&mut slots, &mut done).await;
        fetched(&mut slots, &mut done).await;
        assert_eq!(arrived(&mut agent, &mut Vec::new()), ["text after"]);
        let (texts, reacted) = topic_ops(&mut work);
        assert_eq!(
            texts,
            [
                buffer::TOO_BIG_NOTICE,
                buffer::TOO_BIG_NOTICE,
                buffer::FETCH_FAILED_NOTICE
            ]
        );
        assert_eq!(reacted, [4]);
        // The link closes before the file goes: it waits for the next agent.
        slots.on_control(photo(5, "p", Some("again"), None));
        drop(agent);
        fetched(&mut slots, &mut done).await;
        assert_eq!(buffered(&slots, 0), [5]);
        slots.on_agent(AgentEvent::Disconnected { conn: 1 });
        let mut next = connect_files(&mut slots, 2, A, Some(10), true);
        slots.pump();
        fetched(&mut slots, &mut done).await;
        assert_eq!(
            arrived(&mut next, &mut Vec::new()),
            ["file photo.jpg again"]
        );
    }

    /// Code-review repro (TASK-032). TASK-017 contract: an ended session's
    /// still-open link never gets the slot's kept messages; they wait for
    /// the next live session. A file whose download was already running
    /// when the session ended stays in the slot too, without 👀.
    #[tokio::test]
    async fn a_file_downloading_when_its_session_ends_stays_in_the_slot() {
        let dir = TempDir::new("slots-file-end");
        let png = b"x".to_vec();
        let (mut slots, mut work, mut done) =
            file_slots(&dir, TelegramFiles([("p".to_owned(), png.clone())].into()));
        let agent = connect_files(&mut slots, 1, A, Some(10), true);
        slots.on_control(photo(2, "p", Some("look"), None));
        slots.pump();
        assert_eq!(slots.fetching.len(), 1, "download started");
        // The session ends (/exit) while the file downloads; its agent's
        // link is still open for a moment, as in production.
        slots.on_hook(&end(A, 10));
        slots.on_control(say(Some(100), 3, Some("after")));
        slots.pump();
        fetched(&mut slots, &mut done).await;
        // The bytes went into the old link (nobody takes them back), but
        // the message counts as not delivered.
        let (_, reacted) = topic_ops(&mut work);
        assert!(reacted.is_empty(), "{reacted:?}");
        assert_eq!(buffered(&slots, 0), [2, 3]);
        assert!(slots.fetching.is_empty());
        // The next session of the slot gets both, in order.
        slots.on_agent(AgentEvent::Disconnected { conn: 1 });
        drop(agent);
        slots.on_hook(&resumed(A, 11));
        let mut next = connect_files(&mut slots, 2, A, Some(11), true);
        slots.pump();
        fetched(&mut slots, &mut done).await;
        let mut bytes = Vec::new();
        assert_eq!(
            arrived(&mut next, &mut bytes),
            ["file photo.jpg look", "text after"]
        );
        assert_eq!(bytes, [png]);
        let (_, reacted) = topic_ops(&mut work);
        assert_eq!(reacted, [2, 3]);
    }

    #[tokio::test]
    async fn an_overflow_while_a_file_downloads_drops_the_next_oldest_and_keeps_the_order() {
        let dir = TempDir::new("slots-file-overflow");
        let (mut slots, mut work, mut done) = file_slots(
            &dir,
            TelegramFiles([("p".to_owned(), b"x".to_vec())].into()),
        );
        let mut agent = connect_files(&mut slots, 1, A, Some(10), true);
        slots.on_control(photo(1, "p", Some("look"), None));
        slots.pump();
        assert_eq!(slots.fetching.len(), 1, "download started");
        // 50 texts behind the file: the 51st kept message drops text 2, not
        // the file on its way.
        for id in 2..=51 {
            slots.on_control(say(Some(100), id, Some(&format!("m{id}"))));
        }
        slots.pump();
        let kept = buffered(&slots, 0);
        assert_eq!(kept.len(), 50);
        assert_eq!(kept[..2], [1, 3]);
        let (texts, _) = topic_ops(&mut work);
        assert_eq!(texts, [buffer::OVERFLOW_NOTICE]);
        fetched(&mut slots, &mut done).await;
        let got = arrived(&mut agent, &mut Vec::new());
        let want: Vec<String> = ["file photo.jpg look".to_owned()]
            .into_iter()
            .chain((3..=51).map(|id| format!("text m{id}")))
            .collect();
        assert_eq!(got, want);
        let (_, reacted) = topic_ops(&mut work);
        assert_eq!(reacted, [1].into_iter().chain(3..=51).collect::<Vec<i64>>());
    }

    #[tokio::test]
    async fn a_file_whose_hand_over_is_cut_again_and_again_is_dropped_with_a_notice() {
        let dir = TempDir::new("slots-file-losses");
        let (mut slots, mut work, mut done) = file_slots(
            &dir,
            TelegramFiles([("p".to_owned(), b"x".to_vec())].into()),
        );
        let mut agent = Some(connect_files(&mut slots, 1, A, Some(10), true));
        slots.on_control(photo(2, "p", Some("look"), None));
        slots.on_control(say(Some(100), 3, Some("after")));
        for conn in 1..=MAX_LINK_LOSSES as u64 {
            if agent.is_none() {
                agent = Some(connect_files(&mut slots, conn, A, Some(10), true));
            }
            slots.pump();
            // The link closes before the file goes.
            drop(agent.take());
            fetched(&mut slots, &mut done).await;
            slots.on_agent(AgentEvent::Disconnected { conn });
        }
        let (texts, reacted) = topic_ops(&mut work);
        assert_eq!(texts, [buffer::LINK_LOST_NOTICE]);
        assert!(reacted.is_empty(), "{reacted:?}");
        assert_eq!(buffered(&slots, 0), [3]);
        // The message after it goes to the next agent.
        let mut next = connect_files(&mut slots, 9, A, Some(10), true);
        slots.pump();
        assert_eq!(arrived(&mut next, &mut Vec::new()), ["text after"]);
    }

    /// The file answers the agent got since the last call.
    fn file_answers(from_hub: &mut mpsc::Receiver<HubMsg>) -> Vec<(u64, FileOutcome)> {
        let mut answers = Vec::new();
        while let Ok(msg) = from_hub.try_recv() {
            if let HubMsg::FileAnswer {
                transfer_id,
                outcome,
                ..
            } = msg
            {
                answers.push((transfer_id, outcome));
            }
        }
        answers
    }

    fn from_agent(slots: &mut Slots, conn: u64, msg: AgentMsg) {
        slots.on_agent(AgentEvent::Message {
            conn,
            received_at: StdInstant::now(),
            msg,
        });
    }

    fn offer(slots: &mut Slots, conn: u64, transfer_id: u64, name: &str, size: u64) {
        let msg = AgentMsg::FileOffer {
            transfer_id,
            name: name.into(),
            size,
            caption: Some("see".into()),
            parts: Vec::new(),
        };
        from_agent(slots, conn, msg);
    }

    fn send_bytes(slots: &mut Slots, conn: u64, transfer_id: u64, bytes: &[u8]) {
        for chunk in files::chunks(transfer_id, bytes) {
            from_agent(slots, conn, AgentMsg::FileChunk(chunk));
        }
    }

    #[tokio::test]
    async fn a_file_from_the_session_goes_to_its_topic_as_a_photo_or_a_document() {
        let dir = TempDir::new("slots-file-upload");
        let (mut slots, mut work, _done) = file_slots(&dir, TelegramFiles(HashMap::new()));
        let mut agent = connect_files(&mut slots, 1, A, Some(10), true);
        let png = [b"\x89PNG\r\n\x1a\n".to_vec(), vec![0; files::CHUNK]].concat();
        offer(&mut slots, 1, 1, "../shot.png", png.len() as u64);
        assert_eq!(file_answers(&mut agent), [(1, FileOutcome::Accepted)]);
        send_bytes(&mut slots, 1, 1, &png);
        let (job, op) = work.try_recv().expect("the photo is handed out");
        let size = png.len() as u64;
        assert!(matches!(
            job,
            Work::File {
                conn: 1,
                transfer_id: 1,
                size: s
            } if s == size
        ));
        match op {
            Op::SendPhoto {
                thread_id: Some(100),
                document,
                notify: false,
            } => {
                assert_eq!(document.file_name, "shot.png");
                assert_eq!(document.caption.as_deref(), Some("see"));
                assert_eq!(document.bytes, png);
            }
            other => panic!("{other:?}"),
        }
        assert_eq!((slots.file_bytes, slots.queued_messages), (size, 1));
        slots.on_done(Done::File {
            conn: 1,
            transfer_id: 1,
            size,
            delivery: Some(Ok(Outcome::Sent(Message::default()))),
        });
        assert_eq!(file_answers(&mut agent), [(1, FileOutcome::Sent)]);
        assert_eq!((slots.file_bytes, slots.queued_messages), (0, 0));
        // Not a picture: a document; Telegram refuses it: failed.
        offer(&mut slots, 1, 2, "notes.txt", 5);
        send_bytes(&mut slots, 1, 2, b"hello");
        let (_, op) = work.try_recv().unwrap();
        assert!(
            matches!(op, Op::SendDocument { ref document, .. } if document.file_name == "notes.txt")
        );
        slots.on_done(Done::File {
            conn: 1,
            transfer_id: 2,
            size: 5,
            delivery: Some(Err(ApiError::Telegram {
                code: 400,
                description: "Bad Request: file is empty".into(),
            })),
        });
        assert_eq!(
            file_answers(&mut agent),
            [(2, FileOutcome::Accepted), (2, FileOutcome::Failed)]
        );
        // Too big for Telegram, too much waiting, a broken transfer.
        offer(&mut slots, 1, 3, "huge.bin", files::MAX_UPLOAD + 1);
        slots.file_bytes = MAX_FILE_BYTES - 4;
        offer(&mut slots, 1, 4, "a.bin", 5);
        slots.file_bytes = 0;
        offer(&mut slots, 1, 5, "b.bin", 10);
        from_agent(
            &mut slots,
            1,
            AgentMsg::FileChunk(FileChunk {
                transfer_id: 5,
                offset: 3,
                data: "AAAA".into(),
            }),
        );
        assert_eq!(
            file_answers(&mut agent),
            [
                (3, FileOutcome::Failed),
                (4, FileOutcome::Busy),
                (5, FileOutcome::Accepted),
                (5, FileOutcome::Failed)
            ]
        );
        assert_eq!(slots.file_bytes, 0);
        // The session ends before the last chunk: nothing goes out.
        offer(&mut slots, 1, 6, "c.txt", files::CHUNK as u64 + 1);
        let bytes = vec![b'c'; files::CHUNK + 1];
        let mut pieces = files::chunks(6, &bytes);
        from_agent(&mut slots, 1, AgentMsg::FileChunk(pieces.next().unwrap()));
        slots.on_hook(&end(A, 10));
        from_agent(&mut slots, 1, AgentMsg::FileChunk(pieces.next().unwrap()));
        assert_eq!(
            file_answers(&mut agent),
            [(6, FileOutcome::Accepted), (6, FileOutcome::NoTopic)]
        );
        assert!(work.try_recv().is_err(), "no send");
        // An agent of a session that is no slot's live one is refused, and
        // a closed link drops its unfinished file.
        let mut stranger = connect_files(&mut slots, 2, B, Some(12), true);
        offer(&mut slots, 2, 7, "x.txt", 3);
        assert_eq!(file_answers(&mut stranger), [(7, FileOutcome::NoTopic)]);
        // B takes the slot A left; its agent is bound now.
        slots.on_hook(&start(B, 12));
        offer(&mut slots, 2, 8, "x.txt", 3);
        assert_eq!(file_answers(&mut stranger), [(8, FileOutcome::Accepted)]);
        assert_eq!(slots.file_bytes, 3);
        slots.on_agent(AgentEvent::Disconnected { conn: 2 });
        assert_eq!(slots.file_bytes, 0);
        assert!(slots.uploads.is_empty());
    }

    /// The last answers with their per-file outcomes (TASK-059).
    fn album_answers(
        from_hub: &mut mpsc::Receiver<HubMsg>,
    ) -> Vec<(u64, FileOutcome, Vec<FileOutcome>)> {
        let mut answers = Vec::new();
        while let Ok(msg) = from_hub.try_recv() {
            if let HubMsg::FileAnswer {
                transfer_id,
                outcome,
                parts,
            } = msg
            {
                answers.push((transfer_id, outcome, parts));
            }
        }
        answers
    }

    fn album_offer(
        slots: &mut Slots,
        transfer_id: u64,
        files: &[(&str, &[u8])],
        caption: Option<&str>,
    ) {
        let parts: Vec<FilePart> = files
            .iter()
            .map(|(name, bytes)| FilePart {
                name: (*name).into(),
                size: bytes.len() as u64,
            })
            .collect();
        let msg = AgentMsg::FileOffer {
            transfer_id,
            name: files[0].0.into(),
            size: parts.iter().map(|part| part.size).sum(),
            caption: caption.map(str::to_owned),
            parts,
        };
        from_agent(slots, 1, msg);
    }

    #[tokio::test]
    async fn an_album_goes_as_a_photo_album_and_a_document_album_and_says_what_went() {
        let dir = TempDir::new("slots-file-album");
        let (mut slots, mut work, _done) = file_slots(&dir, TelegramFiles(HashMap::new()));
        let mut agent = connect_files(&mut slots, 1, A, Some(10), true);
        let png: &[u8] = b"\x89PNG\r\n\x1a\npng";
        let jpeg: &[u8] = b"\xFF\xD8\xFF\xE0jpeg";
        let files: [(&str, &[u8]); 4] = [
            ("a.png", png),
            ("notes.txt", b"notes"),
            ("b.jpg", jpeg),
            ("../log.txt", b"log"),
        ];
        album_offer(&mut slots, 1, &files, Some("two kinds"));
        assert_eq!(
            album_answers(&mut agent),
            [(1, FileOutcome::Accepted, Vec::new())]
        );
        let bytes: Vec<u8> = files.iter().flat_map(|(_, bytes)| bytes.to_vec()).collect();
        send_bytes(&mut slots, 1, 1, &bytes);
        // Pictures first, as one photo album with the caption on its first.
        let (job, op) = work.try_recv().expect("the photo album");
        let Work::Album { parts, size, .. } = job else {
            panic!("{job:?}");
        };
        assert_eq!((parts, size), (vec![0, 2], (png.len() + jpeg.len()) as u64));
        match op {
            Op::SendAlbum {
                thread_id: Some(100),
                items,
                photos: true,
                notify: false,
            } => {
                let names: Vec<_> = items.iter().map(|item| item.file_name.as_str()).collect();
                assert_eq!(names, ["a.png", "b.jpg"]);
                assert_eq!(items[0].caption.as_deref(), Some("two kinds"));
                assert_eq!(items[1].caption, None);
                assert_eq!(
                    (items[0].bytes.as_slice(), items[1].bytes.as_slice()),
                    (png, jpeg)
                );
            }
            other => panic!("{other:?}"),
        }
        let (job, op) = work.try_recv().expect("the document album");
        let Work::Album { parts, .. } = job else {
            panic!("{job:?}");
        };
        assert_eq!(parts, [1, 3]);
        match op {
            Op::SendAlbum {
                items,
                photos: false,
                ..
            } => {
                let names: Vec<_> = items.iter().map(|item| item.file_name.as_str()).collect();
                assert_eq!(names, ["notes.txt", "log.txt"]);
                assert!(items.iter().all(|item| item.caption.is_none()));
                assert_eq!(items[1].bytes, b"log");
            }
            other => panic!("{other:?}"),
        }
        assert!(work.try_recv().is_err(), "two messages");
        assert_eq!(
            (slots.file_bytes, slots.queued_messages),
            (bytes.len() as u64, 2)
        );
        slots.on_done(Done::Album {
            conn: 1,
            transfer_id: 1,
            size: (png.len() + jpeg.len()) as u64,
            parts: vec![0, 2],
            delivery: Some(Ok(Outcome::Sent(Message::default()))),
        });
        assert!(
            album_answers(&mut agent).is_empty(),
            "one message still waits"
        );
        slots.on_done(Done::Album {
            conn: 1,
            transfer_id: 1,
            size: 8,
            parts: vec![1, 3],
            delivery: Some(Err(ApiError::Telegram {
                code: 400,
                description: "Bad Request: file is empty".into(),
            })),
        });
        use FileOutcome::{Accepted, Failed, Sent};
        assert_eq!(
            album_answers(&mut agent),
            [(1, Sent, vec![Sent, Failed, Sent, Failed])]
        );
        assert_eq!((slots.file_bytes, slots.queued_messages), (0, 0));
        assert!(slots.albums.is_empty());

        // One of each kind and no caption: each goes alone, the photo named.
        album_offer(&mut slots, 2, &[("c.txt", b"c"), ("d.png", png)], None);
        send_bytes(&mut slots, 1, 2, &[b"c".as_slice(), png].concat());
        let (_, op) = work.try_recv().unwrap();
        assert!(
            matches!(op, Op::SendPhoto { ref document, .. }
                if document.caption.as_deref() == Some("d.png")),
            "{op:?}"
        );
        let (_, op) = work.try_recv().unwrap();
        assert!(
            matches!(op, Op::SendDocument { ref document, .. }
                if document.file_name == "c.txt" && document.caption.is_none()),
            "{op:?}"
        );
        for (parts, size) in [(vec![1], png.len() as u64), (vec![0], 1)] {
            slots.on_done(Done::Album {
                conn: 1,
                transfer_id: 2,
                size,
                parts,
                delivery: None,
            });
        }
        assert_eq!(
            album_answers(&mut agent),
            [(2, Accepted, Vec::new()), (2, Failed, vec![Failed, Failed])]
        );

        // Offers whose parts do not fit are refused before any byte.
        let one: [(&str, &[u8]); 1] = [("a", b"a")];
        album_offer(&mut slots, 3, &one, None);
        let eleven = [("a", b"a".as_slice()); 11];
        album_offer(&mut slots, 4, &eleven, None);
        let mut wrong = AgentMsg::FileOffer {
            transfer_id: 5,
            name: "a".into(),
            size: 3,
            caption: None,
            parts: vec![
                FilePart {
                    name: "a".into(),
                    size: 1,
                },
                FilePart {
                    name: "b".into(),
                    size: 1,
                },
            ],
        };
        from_agent(&mut slots, 1, wrong.clone());
        if let AgentMsg::FileOffer {
            transfer_id, parts, ..
        } = &mut wrong
        {
            *transfer_id = 6;
            parts[1].size = 0;
            parts[0].size = 3;
        }
        from_agent(&mut slots, 1, wrong);
        assert_eq!(
            album_answers(&mut agent),
            [
                (3, Failed, Vec::new()),
                (4, Failed, Vec::new()),
                (5, Failed, Vec::new()),
                (6, Failed, Vec::new())
            ]
        );
        assert_eq!(slots.file_bytes, 0);
    }

    #[tokio::test(start_paused = true)]
    async fn an_accepted_file_whose_bytes_never_come_does_not_hold_the_budget() {
        let dir = TempDir::new("slots-file-idle");
        let (mut slots, _work, _done) = file_slots(&dir, TelegramFiles(HashMap::new()));
        let mut first = connect_files(&mut slots, 1, A, Some(10), true);
        // A second session in the same folder: slot #2 with its own topic.
        slots.on_hook(&start(B, 12));
        slots.registry.topic_created(SlotId(1), 101, "b", None);
        let mut second = connect_files(&mut slots, 2, B, Some(12), true);
        offer(&mut slots, 1, 1, "a.bin", files::MAX_UPLOAD);
        assert_eq!(file_answers(&mut first), [(1, FileOutcome::Accepted)]);
        // Chunks that keep coming keep the file.
        let half = UPLOAD_IDLE / 2 + Duration::from_secs(1);
        tokio::time::advance(half).await;
        let piece = vec![0u8; files::CHUNK];
        send_bytes(&mut slots, 1, 1, &piece);
        tokio::time::advance(half).await;
        offer(&mut slots, 2, 2, "b.bin", 1);
        assert_eq!(file_answers(&mut second), [(2, FileOutcome::Busy)]);
        assert!(slots.uploads.contains_key(&1));
        // Then the first agent stops sending: its share is freed and it
        // hears that the file failed.
        tokio::time::advance(half).await;
        offer(&mut slots, 2, 3, "b.bin", 1);
        assert_eq!(file_answers(&mut second), [(3, FileOutcome::Accepted)]);
        assert_eq!(file_answers(&mut first), [(1, FileOutcome::Failed)]);
        assert!(!slots.uploads.contains_key(&1));
        assert_eq!(slots.file_bytes, 1);
    }

    #[tokio::test]
    async fn at_most_one_file_of_the_largest_size_waits_at_a_time() {
        let dir = TempDir::new("slots-file-cap");
        let (mut slots, _work, _done) = file_slots(&dir, TelegramFiles(HashMap::new()));
        let mut first = connect_files(&mut slots, 1, A, Some(10), true);
        slots.on_hook(&start(B, 12));
        slots.registry.topic_created(SlotId(1), 101, "b", None);
        let mut second = connect_files(&mut slots, 2, B, Some(12), true);
        offer(&mut slots, 1, 1, "a.bin", files::MAX_UPLOAD);
        assert_eq!(file_answers(&mut first), [(1, FileOutcome::Accepted)]);
        offer(&mut slots, 2, 2, "b.bin", 1);
        assert_eq!(file_answers(&mut second), [(2, FileOutcome::Busy)]);
    }

    fn gather_options() -> Options {
        Options {
            gather_quiet: GATHER_QUIET,
            gather_max: GATHER_MAX,
            inbound_settle: INBOUND_SETTLE,
            notice_every: Duration::ZERO,
            ..message_options()
        }
    }

    /// Live A in slot 0 (topic 100) on a hub that gathers bursts, with an
    /// agent (conn 1) whose link queue holds 64; what the actor hands to
    /// the scheduler is captured.
    fn gather_slots(
        dir: &TempDir,
    ) -> (
        Slots,
        mpsc::UnboundedReceiver<(Work, Op)>,
        mpsc::Receiver<HubMsg>,
    ) {
        let mut slots = stalled_slots(dir, gather_options());
        let work = capture_dispatch(&mut slots);
        slots.on_hook(&start(A, 10));
        slots.registry.topic_created(SlotId(0), 100, "a", None);
        let agent = connect_queue(&mut slots, 1, A, Some(10), 64);
        slots.pump();
        (slots, work, agent)
    }

    /// Time passes for the actor as in `run`: its tick, then a pump.
    async fn pass(slots: &mut Slots, by: Duration) {
        tokio::time::advance(by).await;
        slots.on_tick();
        slots.pump();
    }

    fn ms(millis: u64) -> Duration {
        Duration::from_millis(millis)
    }

    /// `(content, meta)` of each inbound the agent got since the last call.
    fn inbounds(from_hub: &mut mpsc::Receiver<HubMsg>) -> Vec<(String, BTreeMap<String, String>)> {
        let mut got = Vec::new();
        while let Ok(msg) = from_hub.try_recv() {
            match msg {
                HubMsg::Inbound { content, meta } => got.push((content, meta)),
                other => panic!("unexpected {other:?}"),
            }
        }
        got
    }

    fn meta_of(pairs: &[(&str, &str)]) -> BTreeMap<String, String> {
        pairs
            .iter()
            .map(|(key, value)| ((*key).to_owned(), (*value).to_owned()))
            .collect()
    }

    #[tokio::test(start_paused = true)]
    async fn three_forwards_and_a_note_within_300_ms_reach_the_session_as_one_inbound() {
        let dir = TempDir::new("slots-gather-burst");
        let (mut slots, mut work, mut agent) = gather_slots(&dir);
        topic_ops(&mut work);
        for id in 1..=3 {
            slots.on_topic_message(topic_text(id, &format!("чужое {id}"), true));
            slots.pump();
            pass(&mut slots, ms(100)).await;
        }
        slots.on_topic_message(topic_text(4, "что скажешь?", false));
        slots.pump();
        pass(&mut slots, GATHER_QUIET - ms(1)).await;
        assert!(
            inbounds(&mut agent).is_empty(),
            "the burst waits for a quiet window"
        );
        assert_eq!(buffered(&slots, 0), [1, 2, 3, 4]);
        assert!(topic_ops(&mut work).1.is_empty(), "no 👀 before it goes");
        pass(&mut slots, ms(1)).await;
        let got = inbounds(&mut agent);
        assert_eq!(
            got,
            [(
                "(переслано)\nчужое 1\n\n---\n\n(переслано)\nчужое 2\n\n---\n\n\
                 (переслано)\nчужое 3\n\n---\n\nчто скажешь?"
                    .to_owned(),
                meta_of(&[
                    ("chat_id", "-1000000000001"),
                    ("message_id", "4"),
                    ("message_ids", "1,2,3,4"),
                    ("thread_id", "100"),
                ]),
            )]
        );
        assert!(got[0].1.keys().all(|key| crate::channel::is_meta_key(key)));
        assert_eq!(topic_ops(&mut work).1, [1, 2, 3, 4], "👀 on every message");
        assert!(slots.registry.slots[0].buffer.is_idle());
        let stream = slots.registry.sessions[A].stream.as_ref().unwrap();
        assert_eq!(stream.receipts, [4]);
        assert_eq!(stream.parts, [(4, vec![1, 2, 3])]);
        pass(&mut slots, GATHER_MAX).await;
        assert!(inbounds(&mut agent).is_empty(), "it went once");
    }

    #[tokio::test(start_paused = true)]
    async fn a_team_burst_names_each_part_and_its_meta_names_only_a_sole_author() {
        let dir = TempDir::new("slots-gather-team");
        let (mut slots, _work, mut agent) = gather_slots(&dir);
        let by = |id: i64, text: &str, name: &str| Inbound {
            from_name: Some(name.to_owned()),
            ..topic_text(id, text, false)
        };
        let burst = |slots: &mut Slots, messages: Vec<Inbound>| {
            for message in messages {
                slots.on_topic_message(message);
                slots.pump();
            }
        };
        burst(&mut slots, vec![by(1, "раз", "Анна"), by(2, "два", "Анна")]);
        pass(&mut slots, GATHER_QUIET).await;
        burst(
            &mut slots,
            vec![by(3, "три", "Анна"), by(4, "четыре", "Иван")],
        );
        pass(&mut slots, GATHER_QUIET).await;
        let got = inbounds(&mut agent);
        let base = [("chat_id", "-1000000000001"), ("thread_id", "100")];
        assert_eq!(
            got,
            [
                (
                    "Анна: раз\n\n---\n\nАнна: два".to_owned(),
                    meta_of(&[
                        base[0],
                        base[1],
                        ("message_id", "2"),
                        ("message_ids", "1,2"),
                        ("from_name", "Анна"),
                    ]),
                ),
                (
                    "Анна: три\n\n---\n\nИван: четыре".to_owned(),
                    meta_of(&[
                        base[0],
                        base[1],
                        ("message_id", "4"),
                        ("message_ids", "3,4"),
                    ]),
                ),
            ]
        );
        assert!(got[0].1.keys().all(|key| crate::channel::is_meta_key(key)));
    }

    #[tokio::test(start_paused = true)]
    async fn a_lone_message_goes_after_the_quiet_window_and_a_steady_stream_by_its_limit() {
        let dir = TempDir::new("slots-gather-window");
        let (mut slots, _work, mut agent) = gather_slots(&dir);
        slots.on_topic_message(topic_text(1, "один", false));
        slots.pump();
        pass(&mut slots, GATHER_QUIET - ms(1)).await;
        assert!(inbounds(&mut agent).is_empty());
        pass(&mut slots, ms(1)).await;
        assert_eq!(
            inbounds(&mut agent),
            [(
                "один".to_owned(),
                meta_of(&[
                    ("chat_id", "-1000000000001"),
                    ("message_id", "1"),
                    ("thread_id", "100"),
                ]),
            )]
        );
        // A message every half second: its burst goes 3 s after the first.
        let mut went = Vec::new();
        for id in 10..20 {
            slots.on_topic_message(topic_text(id, "x", false));
            slots.pump();
            pass(&mut slots, ms(500)).await;
            for (_, meta) in inbounds(&mut agent) {
                went.push((id, meta["message_ids"].clone()));
            }
        }
        assert_eq!(went, [(15, "10,11,12,13,14,15".to_owned())]);
        // The rest goes a quiet window after the last one.
        pass(&mut slots, ms(499)).await;
        assert!(inbounds(&mut agent).is_empty());
        pass(&mut slots, ms(1)).await;
        let rest = inbounds(&mut agent);
        assert_eq!(rest.len(), 1);
        assert_eq!(rest[0].1["message_ids"], "16,17,18,19");
    }

    #[tokio::test(start_paused = true)]
    async fn a_file_in_a_burst_lets_the_burst_go_first_and_keeps_its_place() {
        let dir = TempDir::new("slots-gather-file");
        let mut slots = stalled_slots(&dir, gather_options());
        let mut work = capture_dispatch(&mut slots);
        slots.fetch_files(Arc::new(TelegramFiles(
            [("p".to_owned(), b"x".to_vec())].into(),
        )));
        let mut done = slots.done_rx.take().unwrap();
        slots.on_hook(&start(A, 10));
        slots.registry.topic_created(SlotId(0), 100, "a", None);
        let mut agent = connect_files(&mut slots, 1, A, Some(10), true);
        slots.pump();
        let mut bytes = Vec::new();
        slots.on_topic_message(topic_text(1, "one", false));
        slots.pump();
        slots.on_topic_message(topic_text(2, "two", false));
        slots.pump();
        assert!(arrived(&mut agent, &mut bytes).is_empty());
        // The file does not wait for the quiet window, and the burst goes
        // before it.
        slots.on_control(photo(3, "p", Some("look"), Some(1)));
        slots.pump();
        assert_eq!(arrived(&mut agent, &mut bytes), ["text one\n\n---\n\ntwo"]);
        // A text after the file waits behind it and for its own window.
        slots.on_topic_message(topic_text(4, "after", false));
        slots.pump();
        fetched(&mut slots, &mut done).await;
        assert_eq!(arrived(&mut agent, &mut bytes), ["file photo.jpg look"]);
        pass(&mut slots, GATHER_QUIET).await;
        assert_eq!(arrived(&mut agent, &mut bytes), ["text after"]);
        assert_eq!(topic_ops(&mut work).1, [1, 2, 3, 4]);
        assert!(slots.registry.slots[0].buffer.is_idle());
    }

    #[tokio::test(start_paused = true)]
    async fn a_command_in_a_burst_lets_the_burst_go_first_and_is_refused_while_it_settles() {
        let dir = TempDir::new("slots-gather-command");
        let (fake, mut slots, mut from_hub) = console_slots_with(&dir, true, gather_options());
        slots.on_topic_message(topic_text(1, "one", false));
        slots.on_topic_message(topic_text(2, "two", true));
        slots.on_topic_message(topic_text(3, "!ls", false));
        match from_hub.try_recv() {
            Ok(HubMsg::Inbound { content, meta }) => {
                assert_eq!(content, "one\n\n---\n\n(переслано)\ntwo");
                assert_eq!(meta["message_id"], "2");
                assert_eq!(meta["message_ids"], "1,2");
                assert!(!meta.contains_key("forwarded"), "{meta:?}");
            }
            other => panic!("no burst first: {other:?}"),
        }
        // The burst may be starting a turn the hub does not see yet.
        assert!(from_hub.try_recv().is_err(), "the command is not typed");
        assert_eq!(
            command_replies(&fake, 1).await,
            [(3, console::BUSY_NOTICE.to_owned())]
        );
        slots.on_topic_message(topic_text(4, "four", false));
        assert!(from_hub.try_recv().is_err());
        pass(&mut slots, GATHER_QUIET).await;
        match from_hub.try_recv() {
            Ok(HubMsg::Inbound { content, meta }) => {
                assert_eq!(content, "four");
                assert!(!meta.contains_key("message_ids"));
            }
            other => panic!("no message after the command: {other:?}"),
        }
        // Once the texts settled and no turn is known, a command is typed.
        pass(&mut slots, INBOUND_SETTLE).await;
        slots.on_topic_message(topic_text(5, "!ls", false));
        let (_, text) = command_of(from_hub.try_recv().ok());
        assert_eq!(text, "!ls");
    }

    #[tokio::test]
    async fn a_command_behind_messages_the_link_queue_had_no_room_for_is_refused() {
        let dir = TempDir::new("slots-command-behind");
        // No gathering, no settle: only the kept messages hold it back.
        let (fake, mut slots, mut from_hub) = console_slots(&dir, true);
        for id in 1..=9 {
            slots.on_topic_message(topic_text(id, "x", false));
        }
        assert_eq!(buffered(&slots, 0), [9], "the queue holds 8");
        slots.on_topic_message(topic_text(10, "!ls", false));
        assert_eq!(
            command_replies(&fake, 1).await,
            [(10, console::BUSY_NOTICE.to_owned())]
        );
        let mut ids = Vec::new();
        for _ in 0..2 {
            while let Ok(msg) = from_hub.try_recv() {
                match msg {
                    HubMsg::Inbound { meta, .. } => ids.push(meta["message_id"].clone()),
                    other => panic!("the command overtook the messages: {other:?}"),
                }
            }
            slots.pump();
        }
        assert_eq!(ids, (1..=9).map(|id| id.to_string()).collect::<Vec<_>>());
        slots.on_topic_message(topic_text(11, "!ls", false));
        let (_, text) = command_of(from_hub.try_recv().ok());
        assert_eq!(text, "!ls");
    }

    #[tokio::test(start_paused = true)]
    async fn messages_kept_while_the_slot_had_no_live_session_go_one_by_one() {
        let dir = TempDir::new("slots-gather-dead");
        let (mut slots, _work, mut agent) = gather_slots(&dir);
        // A burst begun while A lived; A ends before it is due.
        slots.on_topic_message(topic_text(1, "one", false));
        slots.pump();
        slots.on_hook(&end(A, 10));
        slots.pump();
        for id in 2..=4 {
            slots.on_topic_message(topic_text(id, "x", true));
            slots.pump();
            pass(&mut slots, ms(100)).await;
        }
        pass(&mut slots, GATHER_MAX).await;
        assert!(inbounds(&mut agent).is_empty());
        assert_eq!(buffered(&slots, 0), [1, 2, 3, 4]);
        // B takes the slot: the kept messages go at once, one inbound each.
        slots.on_hook(&start(B, 11));
        let mut revived = connect_queue(&mut slots, 2, B, Some(11), 64);
        slots.pump();
        let got = inbounds(&mut revived);
        let ids: Vec<&str> = got
            .iter()
            .map(|(_, meta)| meta["message_id"].as_str())
            .collect();
        assert_eq!(ids, ["1", "2", "3", "4"]);
        assert!(
            got.iter()
                .all(|(_, meta)| !meta.contains_key("message_ids"))
        );
        assert!(slots.registry.slots[0].buffer.is_idle());
    }

    #[tokio::test]
    async fn every_message_of_a_burst_turns_writing_with_its_channel_record() {
        let dir = TempDir::new("slots-gather-writing");
        let path = transcript_file(&dir, A);
        let options = Options {
            gather_quiet: ms(200),
            gather_max: GATHER_MAX,
            ..stream_options()
        };
        let mut rig = stream_rig(Fake::default(), options, dir);
        rig.hook(start_with(A, 10, &path, "startup")).await;
        rig.ops_after(1).await;
        let mut kept = rig.reader(1, A, 10).await;
        settled(&rig, |ops| last_icon(ops, 100) == Some(ICON_ALIVE)).await;
        for id in [41, 42, 43] {
            rig.control.send(say(Some(100), id, Some("m"))).unwrap();
        }
        settled(&rig, |ops| reactions(ops).len() == 3).await;
        match kept.recv().await {
            Some(HubMsg::Inbound { meta, .. }) => {
                assert_eq!(meta["message_id"], "43");
                assert_eq!(meta["message_ids"], "41,42,43");
            }
            other => panic!("no burst: {other:?}"),
        }
        append(&path, &channel_record(43));
        let ops = settled(&rig, |ops| reactions(ops).len() == 6).await;
        let mut writing: Vec<(i64, String)> = reactions(&ops).split_off(3);
        writing.sort();
        assert_eq!(
            writing,
            [41, 42, 43].map(|id| (id, "✍".to_owned())).to_vec()
        );
    }

    fn topic_reply(message_id: i64, text: &str, reply_to: i64) -> Inbound {
        Inbound {
            reply_to: Some(reply_to),
            ..topic_text(message_id, text, false)
        }
    }

    #[tokio::test(start_paused = true)]
    async fn a_burst_is_cut_where_its_addressee_or_reply_target_changes() {
        let dir = TempDir::new("slots-gather-addressee");
        let (mut slots, _work, mut agent) = gather_slots(&dir);
        // Subagent S1 of A has its block as message 900 of topic 100.
        slots.registry.subagents.insert(
            S1.to_owned(),
            crate::hub::registry::SubagentEntry {
                parent_session: A.to_owned(),
                slot: Some(SlotId(0)),
                block: crate::hub::registry::Block {
                    thread_id: Some(100),
                    message_id: Some(900),
                    ..Default::default()
                },
                seen: 0,
            },
        );
        // For the subagent, then for the session; for the session, then
        // for the subagent.
        slots.on_topic_message(topic_reply(1, "s1 a", 900));
        slots.on_topic_message(topic_reply(2, "s1 b", 900));
        slots.on_topic_message(topic_reply(3, "on 999", 999));
        slots.on_topic_message(topic_text(4, "plain", false));
        slots.on_topic_message(topic_text(5, "plain 2", true));
        slots.on_topic_message(topic_reply(6, "s1 c", 900));
        pass(&mut slots, GATHER_QUIET - ms(1)).await;
        assert!(inbounds(&mut agent).is_empty(), "one window for all");
        pass(&mut slots, ms(1)).await;
        let chat = ("chat_id", "-1000000000001");
        let topic = ("thread_id", "100");
        assert_eq!(
            inbounds(&mut agent),
            [
                (
                    "s1 a\n\n---\n\ns1 b".to_owned(),
                    meta_of(&[
                        chat,
                        ("message_id", "2"),
                        ("message_ids", "1,2"),
                        ("reply_to_message_id", "900"),
                        ("target_agent", S1),
                        topic,
                    ]),
                ),
                (
                    "on 999".to_owned(),
                    meta_of(&[
                        chat,
                        ("message_id", "3"),
                        ("reply_to_message_id", "999"),
                        topic
                    ]),
                ),
                (
                    "plain\n\n---\n\n(переслано)\nplain 2".to_owned(),
                    meta_of(&[chat, ("message_id", "5"), ("message_ids", "4,5"), topic]),
                ),
                (
                    "s1 c".to_owned(),
                    meta_of(&[
                        chat,
                        ("message_id", "6"),
                        ("reply_to_message_id", "900"),
                        ("target_agent", S1),
                        topic,
                    ]),
                ),
            ]
        );
        assert!(slots.registry.slots[0].buffer.is_idle());
    }

    #[tokio::test(start_paused = true)]
    async fn a_message_the_link_queue_left_behind_does_not_wait_for_a_new_burst() {
        let dir = TempDir::new("slots-gather-left");
        let mut slots = stalled_slots(&dir, gather_options());
        let _work = capture_dispatch(&mut slots);
        slots.on_hook(&start(A, 10));
        slots.registry.topic_created(SlotId(0), 100, "a", None);
        // Kept while A had no agent; its agent's queue then takes one.
        slots.on_topic_message(topic_text(1, "one", false));
        slots.on_topic_message(topic_text(2, "two", false));
        let mut agent = connect_queue(&mut slots, 1, A, Some(10), 1);
        slots.pump();
        assert_eq!(buffered(&slots, 0), [2]);
        // A new message starts a burst behind the one left over.
        slots.on_topic_message(topic_text(3, "three", false));
        assert_eq!(inbounds(&mut agent)[0].0, "one");
        // The queue has room again: two goes at once, alone.
        slots.pump();
        let got = inbounds(&mut agent);
        assert_eq!(got.len(), 1);
        assert_eq!(got[0].0, "two");
        assert_eq!(got[0].1["message_id"], "2");
        assert!(!got[0].1.contains_key("message_ids"));
        pass(&mut slots, GATHER_QUIET - ms(1)).await;
        assert!(inbounds(&mut agent).is_empty(), "three keeps its window");
        pass(&mut slots, ms(1)).await;
        let got = inbounds(&mut agent);
        assert_eq!(got.len(), 1);
        assert_eq!(got[0].0, "three");
        assert!(slots.registry.slots[0].buffer.is_idle());
    }

    #[tokio::test(start_paused = true)]
    async fn a_link_lost_in_the_window_loses_and_repeats_nothing() {
        let dir = TempDir::new("slots-gather-reconnect");
        let (mut slots, _work, mut agent) = gather_slots(&dir);
        slots.on_topic_message(topic_text(1, "one", false));
        slots.on_topic_message(topic_text(2, "two", false));
        pass(&mut slots, ms(500)).await;
        slots.on_agent(AgentEvent::Disconnected { conn: 1 });
        slots.pump();
        pass(&mut slots, GATHER_MAX).await;
        assert!(inbounds(&mut agent).is_empty());
        assert_eq!(buffered(&slots, 0), [1, 2]);
        // The agent links again: what waited goes once, in order.
        let mut again = connect_queue(&mut slots, 2, A, Some(10), 64);
        slots.pump();
        let ids: Vec<String> = inbounds(&mut again)
            .into_iter()
            .map(|(_, meta)| meta["message_id"].clone())
            .collect();
        assert_eq!(ids, ["1", "2"]);
        pass(&mut slots, GATHER_MAX).await;
        assert!(inbounds(&mut again).is_empty(), "nothing twice");
        assert!(slots.registry.slots[0].buffer.is_idle());
    }

    #[tokio::test(start_paused = true)]
    async fn a_burst_over_the_content_limit_goes_as_two_inbounds() {
        let dir = TempDir::new("slots-gather-big");
        let (mut slots, mut work, mut agent) = gather_slots(&dir);
        topic_ops(&mut work);
        // Two of these and a separator fit, three do not.
        let big = "x".repeat(MAX_GATHER_BYTES / 3);
        for id in 1..=3 {
            slots.on_topic_message(topic_text(id, &big, false));
        }
        pass(&mut slots, GATHER_QUIET).await;
        let got = inbounds(&mut agent);
        assert_eq!(got.len(), 2);
        assert_eq!(got[0].0, format!("{big}{}{big}", buffer::PART_SEPARATOR));
        assert_eq!(got[0].1["message_id"], "2");
        assert_eq!(got[0].1["message_ids"], "1,2");
        assert_eq!(got[1].0, big);
        assert_eq!(got[1].1["message_id"], "3");
        assert!(!got[1].1.contains_key("message_ids"));
        assert_eq!(topic_ops(&mut work).1, [1, 2, 3], "👀 on every message");
        // Both channel records turn their parts ✍.
        let stream = slots.registry.sessions[A].stream.as_ref().unwrap();
        assert_eq!(stream.receipts, [2, 3]);
        assert_eq!(stream.parts, [(2, vec![1])]);
        assert!(slots.registry.slots[0].buffer.is_idle());
    }
}
