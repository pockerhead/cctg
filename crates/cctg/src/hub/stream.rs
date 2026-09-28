//! Hub side of the live transcript stream (TASK-016).
//!
//! The session's agent reads the transcript on its own device and answers
//! each `transcript_read` with the stream events of the complete lines it
//! found ([`crate::tail`]). Here those events become topic messages: a
//! terminal prompt as `> text`, assistant text before a tool call as is, and
//! one line per finished tool call (`• Bash: ... ✓`, `• Edit: ... ✗ error`)
//! in the order of the calls: a result that comes before the result of an
//! earlier call waits for it. The final answer of a turn comes from the
//! `Stop` hook, never from here; the end of a turn in the transcript only
//! lets a held answer go (see [`Live::held`]). A Telegram message the agent
//! handed to Claude turns 👀 -> ✍ when its own channel record shows up.
//!
//! Offsets: [`Stream::offset`] in the registry is the transcript byte up to
//! which every stream message was accepted by Telegram, and
//! [`Stream::calls`] the calls still open at that byte. Each read ends with a
//! barrier; it moves them only when every message before it was accepted.
//! A hub restart re-reads from there: messages in flight may come twice, a
//! line is never skipped. A message Telegram refuses (not 429, which the
//! scheduler retries) stops the commits; once nothing is in flight the stream
//! reads again from the last barrier.
//!
//! A released turn answer rides the stream as its messages (see
//! [`Live::sent_answer`]): it waits behind a refused line like the lines do,
//! and a rewind holds it again, so it shows after the lines of its turn.
//! An answer is paired with its turn end by the transcript byte of that turn
//! end ([`Held::end`]), not by count: a turn end read again after a rewind
//! lets only its own answer go, or nothing when that answer is in the topic
//! already ([`Live::turn_end`]).
//!
//! A session that leaves its slot while its stream waits for Telegram can no
//! longer read (TASK-024): its refused messages go again as they were
//! ([`Live::resend`]), and the next session's separator waits for them.
//!
//! The turn message (TASK-062, with status messages on): the quiet content
//! of a turn - assistant text, thinking and tool lines - is written into one
//! message ([`Live::open`], [`Open`]) by edits of its whole text until the
//! next piece no longer fits; the slots actor starts it by turning the
//! status message into it, or with a new message. Every barrier keeps the
//! open message as it was then, and a rewind goes on from the one of the
//! last barrier Telegram accepted: the lines read again are written into
//! that message again, so an edit that already showed them changes nothing.
//! A message the topic got in between (a user's, a prompt, an answer)
//! closes it for good ([`Live::close_open`]).
//!
//! Compact turn view (TASK-076): the owner may choose that the tool lines
//! and 💭 of the turn message sit in Telegram's collapsed quote
//! (`<blockquote expandable>`), which the reader opens with a tap; no edit
//! and no state of ours. [`Open`] builds that HTML: each run of quoted
//! pieces is one quote, assistant text and the interrupt note stay outside
//! between the runs, and a piece whose HTML has a quote of its own
//! (a markdown quote in 💭) stays outside too, since quotes do not nest.
//! The plain text is the same as in the full view, so a message Telegram
//! refuses as HTML falls back to all its lines. The quote tags count
//! against the message limit like any HTML.
//!
//! Rich turn message (TASK-075): in a view with rich messages on, the turn
//! message also has a third form, rich markdown ([`Open::rich`]), which the
//! scheduler sends while Telegram takes it: assistant text and 💭 as
//! markdown blocks as they are ([`transcript::rich_markdown`]), each run of
//! tool lines as one HTML `<p>` block with `<br>` between them, and the
//! quote of a compact message as `<blockquote expandable>` with `<br>`
//! (raw line breaks there join lines). All three forms stay within one
//! message (4096), so a refused rich write falls back to one HTML write.
//!
//! Mirror topics (TASK-078): the group topic of a slot answered in its
//! owner's private chat has a turn stream of its own ([`MirrorTurn`]), fed
//! the same steps of the same reads, shown by that view's settings with its
//! own turn message. Its checkpoint lives in memory; the persisted offset
//! follows the primary view alone. The turn answer goes there as a twin. The
//! turn message of a mirror grows by one write in flight at a time: what
//! comes meanwhile waits and goes in one write of the whole text, and a
//! message closed while it still owes one gets that last write. A new turn
//! message never joins another one still on its way (the scheduler would
//! answer it `Merged` and the other one's last write would erase it), and
//! the call numbers come from one counter of the actor, so a late answer of
//! a mirror turn made before never lands on a new one.

use std::collections::VecDeque;
use std::time::Duration;

use tokio::time::Instant;
use tracing::warn;

use super::chat::{MessageKey, Place};
use super::menu::Piece;
use super::registry::{PendingCall, Stream};
use super::scheduler::Op;
use super::status::INTERRUPT_NOTE;
use crate::wire::StreamItem;

/// Tool calls of a turn waiting for their result, per session. A call past it
/// lets the oldest go: with its mark when its result is in, else unshown.
pub const MAX_CALLS: usize = 64;
/// Telegram messages per session that wait for ✍; the oldest keeps 👀.
pub const MAX_RECEIPTS: usize = 32;
/// Stream messages of a session waiting for Telegram; no further line is
/// taken while this many wait.
pub const MAX_WAITING: usize = 64;
/// Turn answers of a session held for their transcript turn end; the oldest
/// goes when one more comes.
pub const MAX_HELD: usize = 8;
/// Times the refused messages of a session that left its slot are sent
/// again before they are given up.
pub const MAX_RESENDS: u32 = 5;
/// Mark of a thinking message in the topic.
pub const THINKING: &str = "\u{1F4AD}";
/// The collapsed quote of a compact turn message (TASK-076).
pub const QUOTE_OPEN: &str = "<blockquote expandable>";
pub const QUOTE_CLOSE: &str = "</blockquote>";
/// Reaction for a message handed to the session's agent.
pub const ACCEPTED: &str = "👀";
/// Reaction for a message Claude took into work (its channel record is in the
/// transcript). Without U+FE0F, exactly as the Bot API lists it.
pub const WORKING: &str = "✍";

/// What one transcript line asks of the actor, in order.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Step {
    /// A topic message; `merge`: a one-line tool call that may share a
    /// message with the next ones; `piece`: what it shows, for the detail
    /// level of the menu (TASK-073).
    Send {
        text: String,
        merge: bool,
        format: Format,
        piece: Piece,
    },
    /// Mark this Telegram message ✍.
    Working(MessageKey),
    /// A turn ended here: a held answer may go now.
    TurnEnd,
    /// A prompt typed in the terminal starts a turn.
    NewTurn,
}

/// How a stream message is sent.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Format {
    Plain,
    /// Assistant text, sent as Telegram HTML.
    Markdown,
    /// A prompt typed in the terminal: the whole message monospace.
    Code,
}

/// The message a turn's quiet content is written into (TASK-062).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Open {
    /// The message; `None` while the send that makes it waits for Telegram.
    pub key: Option<MessageKey>,
    /// The stream message that made it.
    pub number: u64,
    /// Its whole text.
    pub text: String,
    /// Its text as Telegram HTML, once a formatted piece is in it.
    pub html: Option<String>,
    /// The compact turn view (TASK-076): it goes on only in that view.
    pub compact: bool,
    /// Its HTML ends with an open run of quoted pieces: the next quoted one
    /// goes into that quote.
    pub quote: bool,
    /// Its whole text as rich markdown (TASK-075); `None` in a view without
    /// rich messages or when its first piece did not fit so.
    pub rich: Option<String>,
    /// The view's rich setting when it was made: it goes on only in that
    /// view, as with `compact`.
    pub rich_view: bool,
    /// What `rich` ends with.
    pub rich_run: RichRun,
}

/// The last block of a turn message's rich form (TASK-075).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RichRun {
    /// Markdown: assistant text, 💭, the interrupt note.
    Block,
    /// A `<p>` of tool lines.
    Lines,
    /// The `<blockquote expandable>` of a compact message.
    Quote,
}

impl Open {
    /// The turn message of one piece; `quoted`: in the quote of a compact
    /// message, when that fits one message with its tags; `rich`: the view
    /// shows rich messages (TASK-075).
    pub fn new(
        key: Option<MessageKey>,
        number: u64,
        text: String,
        html: Option<String>,
        compact: bool,
        quoted: bool,
        rich: bool,
    ) -> Self {
        let wrapped = quoted
            .then(|| {
                format!(
                    "{QUOTE_OPEN}{}{QUOTE_CLOSE}",
                    html_of(&text, html.as_deref())
                )
            })
            .filter(|wrapped| fits(wrapped));
        let quote = wrapped.is_some();
        let rich_form = rich
            .then(|| rich_piece(None, RichRun::Block, &text, html.as_deref(), quote))
            .filter(|(form, _)| fits(form));
        let rich_run = rich_form.as_ref().map_or(RichRun::Block, |(_, run)| *run);
        Self {
            key,
            number,
            text,
            html: wrapped.or(html),
            compact,
            quote,
            rich: rich_form.map(|(form, _)| form),
            rich_view: rich,
            rich_run,
        }
    }

    /// `text` (as `html`, when formatted) below the message's text,
    /// `quoted` in the quote of its run; false, and nothing changes, when
    /// that would not fit one message (in its rich form too).
    pub fn push(&mut self, text: &str, html: Option<&str>, quoted: bool) -> bool {
        let joined = if quoted {
            self.quoted(text, html)
        } else {
            join(&self.text, self.html.as_deref(), text, html)
        };
        let Some((joined, joined_html)) = joined else {
            return false;
        };
        let rich = match self.rich.as_deref() {
            Some(own) => {
                let (form, run) = rich_piece(Some(own), self.rich_run, text, html, quoted);
                if !fits(&form) {
                    return false;
                }
                Some((form, run))
            }
            None => None,
        };
        self.text = joined;
        self.html = joined_html;
        self.quote = quoted;
        if let Some((form, run)) = rich {
            self.rich = Some(form);
            self.rich_run = run;
        }
        true
    }

    /// The message with `text` below it in the quote of the open run, or
    /// in a new one.
    fn quoted(&self, text: &str, html: Option<&str>) -> Option<(String, Option<String>)> {
        let own = html_of(&self.text, self.html.as_deref());
        let next = html_of(text, html);
        let joined_html = match own.strip_suffix(QUOTE_CLOSE).filter(|_| self.quote) {
            Some(run) => format!("{run}\n{next}{QUOTE_CLOSE}"),
            None => format!("{own}\n{QUOTE_OPEN}{next}{QUOTE_CLOSE}"),
        };
        let joined = format!("{}\n{text}", self.text);
        (fits(&joined) && fits(&joined_html)).then_some((joined, Some(joined_html)))
    }
}

/// `own` (a turn message's rich form ending with `run`) with the piece
/// `text`/`html` below it (TASK-075): quoted into the quote of the compact
/// message, a formatted piece (markdown: assistant text, 💭, the interrupt
/// note) as its markdown, a plain one (a tool line) into the `<p>` of its
/// run.
fn rich_piece(
    own: Option<&str>,
    run: RichRun,
    text: &str,
    html: Option<&str>,
    quoted: bool,
) -> (String, RichRun) {
    let own = own.unwrap_or_default();
    let separator = if own.is_empty() { "" } else { "\n\n" };
    let (next, open, close) = if quoted {
        (RichRun::Quote, QUOTE_OPEN, QUOTE_CLOSE)
    } else if html.is_some() {
        let block = transcript::rich_markdown(text);
        return (format!("{own}{separator}{block}"), RichRun::Block);
    } else {
        (RichRun::Lines, "<p>", "</p>")
    };
    let content = match next {
        RichRun::Quote => html_lines(&html_of(text, html)),
        _ => html_lines(&transcript::escape_html(text)),
    };
    if run == next
        && let Some(body) = own.strip_suffix(close)
    {
        return (format!("{body}<br>{content}{close}"), next);
    }
    (format!("{own}{separator}{open}{content}{close}"), next)
}

/// `html` with its line breaks as `<br>` (TASK-075): in an HTML block of rich
/// markdown a raw line break joins the lines; inside `<pre>` it stays.
fn html_lines(html: &str) -> String {
    let mut out = String::with_capacity(html.len());
    let mut rest = html;
    while let Some(at) = rest.find("<pre") {
        let (before, pre) = rest.split_at(at);
        out.push_str(&before.replace('\n', "<br>"));
        let end = pre
            .find("</pre>")
            .map_or(pre.len(), |end| end + "</pre>".len());
        out.push_str(&pre[..end]);
        rest = &pre[end..];
    }
    out.push_str(&rest.replace('\n', "<br>"));
    out
}

/// A piece with this HTML may go into the quote of a compact turn message
/// (TASK-076): it has no quote of its own (quotes do not nest; Telegram
/// takes `<pre>` in one, tried 2026-09-28).
pub fn quotable(html: Option<&str>) -> bool {
    html.is_none_or(|html| !html.contains("<blockquote"))
}

/// `html`, or `text` escaped as HTML.
fn html_of(text: &str, html: Option<&str>) -> String {
    html.map_or_else(|| transcript::escape_html(text), str::to_owned)
}

/// `text` fits one Telegram message.
fn fits(text: &str) -> bool {
    transcript::telegram_len(text) <= transcript::TELEGRAM_TEXT_LIMIT
}

/// `text` and `next` one below the other, as HTML when either is formatted;
/// `None` when that is longer than one Telegram message.
pub fn join(
    text: &str,
    html: Option<&str>,
    next: &str,
    next_html: Option<&str>,
) -> Option<(String, Option<String>)> {
    let joined = format!("{text}\n{next}");
    let joined_html = (html.is_some() || next_html.is_some())
        .then(|| format!("{}\n{}", html_of(text, html), html_of(next, next_html)));
    (fits(&joined) && joined_html.as_deref().is_none_or(fits)).then_some((joined, joined_html))
}

/// Quiet pieces of one line that go together: as one turn message of their
/// own, and each piece (text, HTML, quoted) for the turn message above.
/// `Err`: a terminal prompt, on its own.
pub type Joined = Result<(Open, Vec<(String, Option<String>, bool)>), (String, Option<String>)>;

/// Pieces of one line (text, HTML, whether it is quiet turn content (a
/// terminal prompt is not), whether it goes into the quote of a compact turn
/// message) joined where they fit: a result line may end several calls.
/// `rich`: the view shows rich messages (TASK-075).
pub fn join_pieces(
    pieces: Vec<(String, Option<String>, bool, bool)>,
    compact: bool,
    rich: bool,
) -> Vec<Joined> {
    let mut joined: Vec<Joined> = Vec::new();
    for (text, html, quiet, quoted) in pieces {
        if !quiet {
            joined.push(Err((text, html)));
            continue;
        }
        if let Some(Ok((together, parts))) = joined.last_mut()
            && together.push(&text, html.as_deref(), quoted)
        {
            parts.push((text, html, quoted));
            continue;
        }
        let together = Open::new(None, 0, text.clone(), html.clone(), compact, quoted, rich);
        joined.push(Ok((together, vec![(text, html, quoted)])));
    }
    joined
}

/// `open` with every one of `parts` below it; `None` when they do not all
/// fit.
pub fn grown(open: &Open, parts: &[(String, Option<String>, bool)]) -> Option<Open> {
    let mut grown = open.clone();
    parts
        .iter()
        .all(|(text, html, quoted)| grown.push(text, html.as_deref(), *quoted))
        .then_some(grown)
}

/// Closed turn messages of a mirror kept for their last write; the oldest
/// goes without it when one more comes.
pub const MAX_CLOSING: usize = 4;

/// The turn stream of a mirror topic (TASK-078): its own filter, compact
/// view and turn message. Its checkpoint `read_to` lives in memory (a
/// restart of the hub starts it again at the persisted offset of the
/// primary stream, so lines in flight may show twice); it never re-reads;
/// it sends nothing itself. A message Telegram did not take is given up:
/// its pieces are not in the topic, the next piece starts a new message.
#[derive(Debug)]
pub struct MirrorTurn {
    /// The session whose stream it shows.
    pub session: String,
    /// Lines up to this transcript byte were shown (or skipped) here.
    read_to: u64,
    /// The turn message the next quiet content goes into.
    current: Option<MirrorMessage>,
    /// Messages closed while an answer or a last write was due.
    closing: VecDeque<MirrorMessage>,
    /// New messages the scheduler may join later ones into (`merge`) whose
    /// answer has not come: until it came, a new message does not join. It
    /// holds at most one of the new messages waiting in a row (those after
    /// it are not `merge`, nothing joins them).
    joining: Vec<u64>,
}

#[derive(Debug)]
struct MirrorMessage {
    /// Its text; `open.key` is `None` while the send that makes it waits.
    open: Open,
    /// The op that made it.
    number: u64,
    /// The write in flight into it.
    writing: Option<u64>,
    /// It grew since that write went: one more is due.
    dirty: bool,
    /// The status twin it is being made of, while that write waits.
    absorbs: Option<MessageKey>,
}

/// A call of a mirror's turn stream, by its number there.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MirrorOp {
    /// A new message; `into`: made of this status twin instead (an edit);
    /// `rich`: its rich form (TASK-075).
    New {
        number: u64,
        text: String,
        html: Option<String>,
        rich: Option<String>,
        merge: bool,
        into: Option<MessageKey>,
    },
    /// The whole text of the turn message `into`.
    Write {
        number: u64,
        into: MessageKey,
        text: String,
        html: Option<String>,
        rich: Option<String>,
    },
}

impl MirrorOp {
    pub fn number(&self) -> u64 {
        match self {
            Self::New { number, .. } | Self::Write { number, .. } => *number,
        }
    }
}

/// What an answer to a mirror's call asks of the actor.
#[derive(Debug, Default, PartialEq, Eq)]
pub struct Answered {
    /// The write the message owes now.
    pub write: Option<MirrorOp>,
    /// A status twin that may still show the old status: cleared away.
    pub retire: Option<MessageKey>,
}

impl MirrorTurn {
    pub fn new(session: &str, read_to: u64) -> Self {
        Self {
            session: session.to_owned(),
            read_to,
            current: None,
            closing: VecDeque::new(),
            joining: Vec::new(),
        }
    }

    /// Another session's stream from `read_to` (or the same one read again
    /// from its start): the turn message is closed.
    pub fn reset(&mut self, session: &str, read_to: u64) {
        self.close();
        session.clone_into(&mut self.session);
        self.read_to = read_to;
    }

    /// The line ending at `end` is new here.
    pub fn takes(&mut self, end: u64) -> bool {
        if end > self.read_to {
            self.read_to = end;
            true
        } else {
            false
        }
    }

    /// No stream feeds it any more (its topic is no mirror now): the turn
    /// message is closed; it only waits for the answers to its calls, and a
    /// stream that feeds it again starts it over ([`Self::reset`]).
    pub fn leave(&mut self) {
        self.close();
        self.session.clear();
    }

    /// A new message of it that the scheduler may join later ones into
    /// still waits: no other new message of its topic may be `merge` now,
    /// the primary stream's included (a turn left by [`Self::leave`] would
    /// erase what joined it with its last write).
    pub fn joinable(&self) -> bool {
        !self.joining.is_empty()
    }

    /// Nothing it sent waits for an answer, nothing is owed.
    pub fn idle(&self) -> bool {
        self.current.is_none() && self.closing.is_empty() && self.joining.is_empty()
    }

    /// Something came below the turn message: nothing more goes into it; a
    /// write it still owes goes once the one in flight is answered.
    pub fn close(&mut self) {
        if let Some(message) = self.current.take()
            && (message.dirty || message.absorbs.is_some())
        {
            self.park(message);
        }
    }

    fn park(&mut self, message: MirrorMessage) {
        self.closing.push_back(message);
        if self.closing.len() > MAX_CLOSING {
            self.closing.pop_front();
            warn!("a closed mirror turn message goes without its last write");
        }
    }

    /// The calls for `joined` (see [`join_pieces`]) in the view's `compact`
    /// turn view and `rich` setting (TASK-075). Quiet pieces grow the turn
    /// message while they fit, else start the next one. The first new
    /// message of a read (`posted == 0`) is made of status twin `twin` when
    /// there is one (taken then); every other new one counts in `posted`.
    /// Call numbers come from `calls`.
    pub fn roll(
        &mut self,
        joined: Vec<Joined>,
        compact: bool,
        rich: bool,
        twin: &mut Option<MessageKey>,
        posted: &mut usize,
        calls: &mut u64,
    ) -> Vec<MirrorOp> {
        let mut ops = Vec::new();
        for piece in joined {
            if let Ok((_, parts)) = &piece
                && let Some(current) = self.current.as_mut()
                && current.open.compact == compact
                && current.open.rich_view == rich
                && let Some(grown) = grown(&current.open, parts)
            {
                current.open = grown;
                match current.open.key {
                    Some(into) if current.writing.is_none() => {
                        let number = next_call(calls);
                        current.writing = Some(number);
                        ops.push(MirrorOp::Write {
                            number,
                            into,
                            text: current.open.text.clone(),
                            html: current.open.html.clone(),
                            rich: current.open.rich.clone(),
                        });
                    }
                    _ => current.dirty = true,
                }
                continue;
            }
            self.close();
            let number = next_call(calls);
            let absorb = if *posted == 0 { twin.take() } else { None };
            let (text, html, open) = match piece {
                Ok((open, _)) => (open.text.clone(), open.html.clone(), Some(open)),
                Err((text, html)) => (text, html, None),
            };
            let rich_form = open.as_ref().and_then(|open| open.rich.clone());
            // The status twin becoming content goes at once; a message that
            // joined one still on its way would be lost (answered `Merged`,
            // then erased by that one's last write).
            let merge = open.is_some() && absorb.is_none() && self.joining.is_empty();
            if merge {
                self.joining.push(number);
            }
            ops.push(MirrorOp::New {
                number,
                text: text.clone(),
                html: html.clone(),
                rich: rich_form,
                merge,
                into: absorb,
            });
            let message = |open: Open, key: Option<MessageKey>| MirrorMessage {
                open: Open {
                    key,
                    number,
                    ..open
                },
                number,
                writing: key.map(|_| number),
                dirty: false,
                absorbs: key,
            };
            match (absorb, open) {
                (Some(twin), Some(open)) => self.current = Some(message(open, Some(twin))),
                // A prompt made of the twin: nothing goes into it, its
                // answer is still awaited.
                (Some(twin), None) => {
                    let open = Open::new(Some(twin), number, text, html, compact, false, false);
                    self.park(message(open, Some(twin)));
                }
                (None, Some(open)) => {
                    *posted += 1;
                    self.current = Some(message(open, None));
                }
                (None, None) => *posted += 1,
            }
        }
        ops
    }

    /// A message of its own for one stream chunk (no turn message: status
    /// messages off).
    pub fn line(
        &mut self,
        text: String,
        html: Option<String>,
        merge: bool,
        calls: &mut u64,
    ) -> MirrorOp {
        MirrorOp::New {
            number: next_call(calls),
            text,
            html,
            rich: None,
            merge,
            into: None,
        }
    }

    /// Telegram answered call `number`: `made` the message a new one made,
    /// `accepted` it is in the topic, `gone` the message written into is
    /// gone. A write owed now takes its number from `calls`.
    pub fn answered(
        &mut self,
        number: u64,
        made: Option<MessageKey>,
        accepted: bool,
        gone: bool,
        calls: &mut u64,
    ) -> Answered {
        self.joining.retain(|joining| *joining != number);
        let mut answered = Answered::default();
        let of = |message: &MirrorMessage| {
            (message.open.key.is_none() && message.number == number)
                || message.writing == Some(number)
        };
        let at = match self.current.as_ref().filter(|message| of(message)) {
            Some(_) => None,
            None => match self.closing.iter().position(of) {
                Some(at) => Some(at),
                None => return answered,
            },
        };
        let message = match at {
            None => self.current.as_mut(),
            Some(at) => self.closing.get_mut(at),
        };
        let Some(message) = message else {
            return answered;
        };
        let keep = if message.open.key.is_none() {
            match made.filter(|_| accepted) {
                Some(key) => {
                    message.open.key = Some(key);
                    true
                }
                None => false,
            }
        } else {
            message.writing = None;
            if let Some(twin) = message.absorbs.take()
                && !accepted
                && !gone
            {
                answered.retire = Some(twin);
            }
            accepted && !gone
        };
        let settled = if !keep {
            true
        } else if let (true, Some(into)) = (message.dirty, message.open.key) {
            let number = next_call(calls);
            message.writing = Some(number);
            message.dirty = false;
            answered.write = Some(MirrorOp::Write {
                number,
                into,
                text: message.open.text.clone(),
                html: message.open.html.clone(),
                rich: message.open.rich.clone(),
            });
            false
        } else {
            at.is_some() && message.writing.is_none()
        };
        if settled {
            match at {
                None => self.current = None,
                Some(at) => {
                    self.closing.remove(at);
                }
            }
        }
        answered
    }
}

/// The next number of `calls`, the one call counter of all mirror turns.
fn next_call(calls: &mut u64) -> u64 {
    *calls += 1;
    *calls
}

/// Applies the items of one line to the calls still open and the receipts.
pub fn apply_line(
    calls: &mut Vec<PendingCall>,
    receipts: &mut Vec<MessageKey>,
    items: &[StreamItem],
) -> Vec<Step> {
    let mut steps = Vec::new();
    for item in items {
        match item {
            StreamItem::Prompt { text } => {
                flush(calls, &mut steps);
                steps.push(Step::NewTurn);
                steps.push(Step::Send {
                    text: format!("> {text}"),
                    merge: false,
                    format: Format::Code,
                    piece: Piece::Prompt,
                });
            }
            StreamItem::Note { text } => {
                flush(calls, &mut steps);
                // The interrupt note shows at every detail level: after ⏹
                // no answer comes (TASK-073).
                let piece = if text.starts_with(INTERRUPT_NOTE) {
                    Piece::Interrupt
                } else {
                    Piece::Text
                };
                steps.push(Step::Send {
                    text: text.clone(),
                    merge: false,
                    format: Format::Markdown,
                    piece,
                });
            }
            // Its own message; under the rate limit it joins its neighbours like a tool line.
            StreamItem::Thinking { text } => {
                flush(calls, &mut steps);
                steps.push(Step::Send {
                    text: format!("{THINKING} {text}"),
                    merge: true,
                    format: Format::Markdown,
                    piece: Piece::Thinking,
                });
            }
            StreamItem::TurnEnd => {
                flush(calls, &mut steps);
                steps.push(Step::TurnEnd);
            }
            // A session has one owner: its private receipts are of one chat.
            StreamItem::Channel {
                message_id,
                private,
            } => {
                let found = receipts.iter().position(|known| {
                    known.id == *message_id && known.chat.is_private() == *private
                });
                if let Some(at) = found {
                    steps.push(Step::Working(receipts.remove(at)));
                }
            }
            StreamItem::Call { id, line } => {
                if calls.iter().any(|known| known.id == *id) {
                    continue;
                }
                if calls.len() >= MAX_CALLS {
                    let oldest = calls.remove(0);
                    if oldest.done {
                        steps.push(finished(&oldest));
                    }
                    release_ready(calls, &mut steps);
                }
                calls.push(PendingCall {
                    id: id.clone(),
                    line: line.clone(),
                    done: false,
                    error: None,
                });
            }
            StreamItem::Result { id, error } => {
                if let Some(call) = calls
                    .iter_mut()
                    .find(|known| known.id == *id && !known.done)
                {
                    call.done = true;
                    call.error.clone_from(error);
                    release_ready(calls, &mut steps);
                }
            }
            StreamItem::Other => {}
        }
    }
    steps
}

/// Sends the finished calls at the head, in call order.
fn release_ready(calls: &mut Vec<PendingCall>, steps: &mut Vec<Step>) {
    let ready = calls.iter().take_while(|call| call.done).count();
    steps.extend(calls.drain(..ready).map(|call| finished(&call)));
}

/// A turn moved on: finished calls go in call order, calls that never got a
/// result are not shown (no ✓ for what did not finish).
fn flush(calls: &mut Vec<PendingCall>, steps: &mut Vec<Step>) {
    steps.extend(
        calls
            .drain(..)
            .filter(|call| call.done)
            .map(|call| finished(&call)),
    );
}

fn finished(call: &PendingCall) -> Step {
    let text = match call.error.as_deref() {
        None => format!("{} ✓", call.line),
        Some("") => format!("{} ✗", call.line),
        Some(error) => format!("{} ✗ {error}", call.line),
    };
    Step::Send {
        text,
        merge: true,
        format: Format::Plain,
        piece: Piece::Tool,
    }
}

/// A message handed to the session's agent now shows 👀 and waits for ✍.
pub fn receipt(stream: &mut Stream, message: MessageKey) {
    if stream.receipts.contains(&message) {
        return;
    }
    if stream.receipts.len() >= MAX_RECEIPTS {
        stream.receipts.remove(0);
    }
    stream.receipts.push(message);
    let receipts = &stream.receipts;
    stream.parts.retain(|(key, _)| receipts.contains(key));
}

/// Messages handed to the session's agent as one inbound (TASK-048), in
/// order: the last one, whose id the inbound's `message_id` carries, waits
/// for ✍ like [`receipt`]; the others turn ✍ with it ([`take_parts`]).
pub fn receipt_parts(stream: &mut Stream, messages: &[MessageKey]) {
    let Some((&key, others)) = messages.split_last() else {
        return;
    };
    receipt(stream, key);
    stream.parts.retain(|(known, _)| *known != key);
    if !others.is_empty() {
        stream.parts.push((key, others.to_vec()));
    }
}

/// The other messages of the burst whose receipt `key` turned ✍.
pub fn take_parts(stream: &mut Stream, key: MessageKey) -> Vec<MessageKey> {
    match stream.parts.iter().position(|(known, _)| *known == key) {
        Some(at) => stream.parts.remove(at).1,
        None => Vec::new(),
    }
}

/// A turn answer held until the stream lines before it are handed out.
#[derive(Debug)]
pub struct Held {
    /// The topic it goes to.
    pub place: Place,
    /// Blank for a `Stop` without text: it only takes its turn end.
    pub answer: String,
    pub until: Instant,
    /// The transcript byte of its turn end, once paired with one.
    pub end: Option<u64>,
    /// When it went unpaired and left a debt ([`Live::answered_early`]):
    /// held again by a rewind, it takes that debt back.
    pub gone: Option<Instant>,
    /// The answer carries its rich form (TASK-075): a view of its topic,
    /// its own or a mirror's, shows rich messages.
    pub rich: bool,
    /// Its own topic's view shows rich messages; when not, the answer goes
    /// there as today's messages, and as today's document outside the
    /// stream when it is one ([`crate::hub::slots`]).
    pub rich_here: bool,
}

#[derive(Debug)]
enum Entry {
    Message {
        number: u64,
        state: Answer,
        /// The last message of a turn answer: held again if it is not in
        /// the topic when the stream rewinds.
        answer: Option<Held>,
        /// What went to the scheduler, to send again when the stream cannot
        /// read any more.
        op: Box<Op>,
        /// It turned the status message into turn content (TASK-062): sent
        /// again, it is a new message.
        absorb: bool,
    },
    /// Everything before it read: the offset, the calls open there and the
    /// turn message then (TASK-062).
    Barrier {
        to: u64,
        calls: Vec<PendingCall>,
        open: Option<Open>,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Answer {
    Waiting,
    Accepted,
    Refused,
}

/// In-memory stream state of one session.
#[derive(Debug, Default)]
pub struct Live {
    /// Next byte to ask the agent for; ahead of `Stream::offset` while
    /// messages wait. `None`: the end of the file.
    pub read_at: Option<u64>,
    /// Calls open at `read_at`.
    pub calls: Vec<PendingCall>,
    /// The request in flight: the connection it went to and when.
    pub reading: Option<(u64, Instant)>,
    /// When to ask again.
    pub next_read: Option<Instant>,
    pub missing_warned: bool,
    /// The agent found the transcript at least once. Until then a missing
    /// file is normal: Claude Code writes it with the first prompt.
    pub file_seen: bool,
    pub reset_warned: bool,
    pub refused_warned: bool,
    /// Turn answers waiting for a turn end in the transcript, oldest first.
    pub held: VecDeque<Held>,
    /// Turn ends read that no `Stop` has taken yet (the file was quicker than
    /// the hook), by transcript byte, oldest first, at most [`MAX_HELD`]. A
    /// new prompt read after them does not clear them at once (their `Stop`
    /// may still be on its way to the actor); they lapse at `ends_until`.
    pub ends_unclaimed: VecDeque<u64>,
    pub ends_until: Option<Instant>,
    /// The next stream message is the first of this stream, or the first
    /// after a rewind: it lets the scheduler send the topic's stream again
    /// after a refused message (see `Op::Stream::restart`).
    pub restart: bool,
    /// Turn ends ahead of `read_at` whose answer went to the topic already:
    /// read again, they let nothing go and are not claimable.
    answered_ends: Vec<u64>,
    /// A refused message waits: the stream reads again from the last
    /// barrier once this time is past, and only the answer to that read
    /// drops what was sent since (a session that leaves its slot meanwhile
    /// sends it again, see [`Live::resend`]).
    pub rewind_at: Option<Instant>,
    /// How often a session that left its slot sent its refused messages again.
    pub resends: u32,
    /// Since when a message of the next session of its slot waits behind
    /// this drain: it holds that session's separator one stream retry at
    /// most after this.
    pub waited: Option<Instant>,
    /// Answers that went without a turn end (by their timeout), oldest
    /// first, by when they went: the next turn ends read that no held
    /// answer owns are theirs. A rewind that holds such an answer again
    /// takes its debt back ([`Held::gone`]).
    unpaired: VecDeque<Instant>,
    /// The turn message the next quiet content goes into (TASK-062).
    pub open: Option<Open>,
    /// `open` as of the last barrier Telegram fully accepted: a rewind goes
    /// on from it.
    committed_open: Option<Open>,
    waiting: VecDeque<Entry>,
    next: u64,
}

impl Live {
    pub fn new(read_at: Option<u64>, calls: Vec<PendingCall>) -> Self {
        Self {
            read_at,
            calls,
            restart: true,
            ..Self::default()
        }
    }

    /// A new turn started in the transcript: turn ends no `Stop` has taken
    /// yet stay claimable until `until`, not longer.
    pub fn lapse_ends(&mut self, until: Instant) {
        if !self.ends_unclaimed.is_empty() {
            self.ends_until.get_or_insert(until);
        }
    }

    /// A `Stop` takes the oldest turn end read before it, if one is still
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
        // The mark stays: another rewind may read this end again. A barrier
        // past it (`advance`) or a rewind beyond it drops it.
        if self.answered_ends.contains(&end) {
            return None;
        }
        let own = self.held.iter().position(|held| held.end == Some(end));
        // An answer that went unpaired is older than every held one.
        let at = own.or_else(|| match self.held.front()?.end {
            Some(known) => (known <= end).then_some(0),
            None => self.unpaired.is_empty().then_some(0),
        });
        if let Some(mut held) = at.and_then(|at| self.held.remove(at)) {
            held.end = Some(end);
            return Some(held);
        }
        if self.unpaired.pop_front().is_some() {
            // Its answer is in the topic already.
            self.answered_ends.push(end);
            return None;
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

    /// `held` goes at `now` by its timeout ahead of a read of its turn end:
    /// that turn end, read later, must not let another answer go. Not paired
    /// yet, it takes the next turn end read that no held answer owns; a blank
    /// one too (its turn end may be an empty text), and a debt without a
    /// turn end lapses ([`Live::lapse_unpaired`]).
    pub fn answered_early(&mut self, held: &mut Held, now: Instant) {
        match held.end {
            Some(end) if self.read_at.is_some_and(|at| at < end) => self.answered_ends.push(end),
            Some(_) => {}
            None => {
                self.unpaired.push_back(now);
                held.gone = Some(now);
            }
        }
    }

    /// A read asked at `asked` went to the end of the file: an answer that
    /// went unpaired before it and found no turn end there has none (the
    /// turn end is written before its `Stop`), so it takes none later.
    pub fn lapse_unpaired(&mut self, asked: Instant) {
        self.unpaired.retain(|&gone| gone >= asked);
    }

    /// The transcript was cut or replaced: it is read again from its start
    /// and no transcript byte known so far means anything in it.
    pub fn reset(&mut self) {
        self.read_at = Some(0);
        self.calls.clear();
        self.answered_ends.clear();
        self.unpaired.clear();
        self.ends_unclaimed.clear();
        self.ends_until = None;
        self.close_open();
        for held in &mut self.held {
            held.end = None;
            held.gone = None;
        }
        for entry in &mut self.waiting {
            if let Entry::Message {
                answer: Some(held), ..
            } = entry
            {
                held.end = None;
                held.gone = None;
            }
        }
    }

    /// `held` does not ride the stream (blank, a file, or dropped): nothing
    /// records its turn end when it is accepted, so it is recorded now. A
    /// rewind that reads that end again lets nothing go there and leaves
    /// nothing claimable for the next `Stop`.
    pub fn answered_outside(&mut self, held: &Held) {
        if let Some(end) = held.end
            && !self.answered_ends.contains(&end)
        {
            self.answered_ends.push(end);
        }
    }

    /// Message `op` goes to Telegram; its number.
    pub fn sent(&mut self, op: &Op) -> u64 {
        self.push_message(None, op, false)
    }

    /// `op` turns the status message into turn content (TASK-062); its
    /// number.
    pub fn sent_absorb(&mut self, op: &Op) -> u64 {
        self.push_message(None, op, true)
    }

    /// `op`, the last message of turn answer `held`, goes to Telegram; its
    /// number.
    pub fn sent_answer(&mut self, held: Held, op: &Op) -> u64 {
        self.push_message(Some(held), op, false)
    }

    fn push_message(&mut self, answer: Option<Held>, op: &Op, absorb: bool) -> u64 {
        self.next += 1;
        self.waiting.push_back(Entry::Message {
            number: self.next,
            state: Answer::Waiting,
            answer,
            op: Box::new(op.clone()),
            absorb,
        });
        self.next
    }

    /// What went to the scheduler as stream message `number`, until the
    /// barrier after it passed.
    pub fn op_of(&self, number: u64) -> Option<&Op> {
        self.waiting.iter().find_map(|entry| match entry {
            Entry::Message { number: n, op, .. } if *n == number => Some(op.as_ref()),
            _ => None,
        })
    }

    /// The turn message's send waits for Telegram: the next quiet content
    /// has no message to go into yet.
    pub fn open_pending(&self) -> bool {
        self.open.as_ref().is_some_and(|open| open.key.is_none())
    }

    /// The send of stream message `number` answered: the turn message it
    /// made is `key` (`None`: no message to write into, it closes).
    pub fn opened(&mut self, number: u64, key: Option<MessageKey>) {
        let barriers = self.waiting.iter_mut().filter_map(|entry| match entry {
            Entry::Barrier { open, .. } => Some(open),
            Entry::Message { .. } => None,
        });
        for open in [&mut self.open, &mut self.committed_open]
            .into_iter()
            .chain(barriers)
        {
            if open
                .as_ref()
                .is_some_and(|open| open.key.is_none() && open.number == number)
            {
                match key {
                    Some(key) => {
                        if let Some(open) = open.as_mut() {
                            open.key = Some(key);
                        }
                    }
                    None => *open = None,
                }
            }
        }
    }

    /// Something else came into the topic below the turn message (or it is
    /// gone): nothing is written into it any more, not after a rewind
    /// either.
    pub fn close_open(&mut self) {
        self.open = None;
        self.committed_open = None;
        for entry in &mut self.waiting {
            if let Entry::Barrier { open, .. } = entry {
                *open = None;
            }
        }
    }

    /// The refused messages again, in order, for a stream that cannot read
    /// any more: each waits for Telegram again under its number, and the
    /// first one ends the break of its topic's stream.
    pub fn resend(&mut self) -> Vec<(u64, Op)> {
        self.fold_refused_absorbs();
        let mut again = Vec::new();
        for entry in &mut self.waiting {
            if let Entry::Message {
                number,
                state: state @ Answer::Refused,
                op,
                ..
            } = entry
            {
                *state = Answer::Waiting;
                let mut op = Op::clone(op);
                if again.is_empty()
                    && let Op::Stream { restart, .. } = &mut op
                {
                    *restart = true;
                }
                again.push((*number, op));
            }
        }
        again
    }

    /// A refused message that was to turn the status message into turn
    /// content goes again as a new message (TASK-062): that status message
    /// is cleared away and not the session's to write any more. The refused
    /// writes after it into the same message carry its whole text, so the
    /// newest of them becomes the new message's text and they are not sent
    /// again. The ops are changed where they wait, so an answer counts as
    /// the one of what went, also when it is refused once more.
    fn fold_refused_absorbs(&mut self) {
        let mut moved: Vec<(i64, usize)> = Vec::new();
        for index in 0..self.waiting.len() {
            let Entry::Message {
                state: state @ Answer::Refused,
                op,
                absorb,
                ..
            } = &mut self.waiting[index]
            else {
                continue;
            };
            let Op::Stream {
                into, text, html, ..
            } = op.as_mut()
            else {
                continue;
            };
            let Some(target) = *into else {
                continue;
            };
            if std::mem::take(absorb) {
                *into = None;
                moved.push((target, index));
                continue;
            }
            let Some(&(_, first)) = moved.iter().find(|(moved, _)| *moved == target) else {
                continue;
            };
            let (text, html) = (std::mem::take(text), html.take());
            *state = Answer::Accepted;
            if let Entry::Message { op, .. } = &mut self.waiting[first]
                && let Op::Stream {
                    text: first_text,
                    html: first_html,
                    ..
                } = op.as_mut()
            {
                *first_text = text;
                *first_html = html;
            }
        }
    }

    /// Every message is in the topic (or skipped): nothing waits for
    /// Telegram and nothing waits to go again.
    pub fn settled(&self) -> bool {
        self.waiting.iter().all(|entry| {
            matches!(
                entry,
                Entry::Barrier { .. }
                    | Entry::Message {
                        state: Answer::Accepted,
                        ..
                    }
            )
        })
    }

    /// Everything up to `to` is handed out; `calls` are open there. A
    /// barrier right after another replaces it (nothing lies between them),
    /// so idle reads while a message waits keep one barrier, not one each.
    pub fn barrier(&mut self, to: u64) {
        self.read_at = Some(to);
        let barrier = Entry::Barrier {
            to,
            calls: self.calls.clone(),
            open: self.open.clone(),
        };
        match self.waiting.back_mut() {
            Some(last @ Entry::Barrier { .. }) => *last = barrier,
            _ => self.waiting.push_back(barrier),
        }
    }

    /// Telegram answered message `number`; `accepted` false: it is not in
    /// the topic.
    pub fn answered(&mut self, number: u64, accepted: bool) {
        for entry in &mut self.waiting {
            if let Entry::Message {
                number: n, state, ..
            } = entry
                && *n == number
            {
                *state = if accepted {
                    Answer::Accepted
                } else {
                    Answer::Refused
                };
            }
        }
    }

    /// Drops the accepted head; the last barrier passed, if any: the offset
    /// and open calls to persist.
    pub fn advance(&mut self) -> Option<(u64, Vec<PendingCall>)> {
        let mut passed = None;
        loop {
            match self.waiting.front() {
                Some(Entry::Message {
                    state: Answer::Accepted,
                    ..
                }) => {}
                Some(Entry::Barrier { .. }) => {}
                _ => return passed,
            }
            match self.waiting.pop_front() {
                Some(Entry::Barrier { to, calls, open }) => {
                    // A rewind never goes back past `to` now.
                    self.answered_ends.retain(|&end| end > to);
                    self.committed_open = open;
                    passed = Some((to, calls));
                }
                // Until a barrier passes it, a rewind may read its turn end
                // again.
                Some(Entry::Message {
                    answer: Some(held), ..
                }) => self.answered_ends.extend(held.end),
                _ => {}
            }
        }
    }

    /// The numbers of the messages waiting for Telegram that carry an answer.
    #[cfg(test)]
    pub fn answer_numbers(&self) -> Vec<(u64, &str)> {
        self.waiting
            .iter()
            .filter_map(|entry| match entry {
                Entry::Message {
                    number,
                    answer: Some(held),
                    ..
                } => Some((*number, held.answer.as_str())),
                _ => None,
            })
            .collect()
    }

    /// Messages not answered yet.
    pub fn unanswered(&self) -> usize {
        self.waiting
            .iter()
            .filter(|entry| {
                matches!(
                    entry,
                    Entry::Message {
                        state: Answer::Waiting,
                        ..
                    }
                )
            })
            .count()
    }

    /// A refused message waits and nothing is in flight: time to read again
    /// from the last barrier.
    pub fn stuck(&self) -> bool {
        self.unanswered() == 0
            && self.waiting.iter().any(|entry| {
                matches!(
                    entry,
                    Entry::Message {
                        state: Answer::Refused,
                        ..
                    }
                )
            })
    }

    /// Starts over at the last barrier Telegram fully accepted, keeping the
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
        let mut answered_ends: Vec<u64> = self.answered_ends.drain(..).filter(read_again).collect();
        for entry in std::mem::take(&mut self.waiting) {
            if let Entry::Message {
                state,
                answer: Some(mut answer),
                ..
            } = entry
            {
                if state == Answer::Accepted {
                    answered_ends.extend(answer.end.filter(read_again));
                } else {
                    // Not in the topic: the debt it left when it went is not
                    // owed; its own turn end lets it go again. Debts of the
                    // same instant are alike, so one of them goes.
                    if let Some(gone) = answer.gone.take()
                        && let Some(at) = self.unpaired.iter().position(|&debt| debt == gone)
                    {
                        self.unpaired.remove(at);
                    }
                    held.push_back(answer);
                }
            }
        }
        // An answer that went by its timeout and is held again waits for its
        // own turn end again.
        answered_ends.retain(|end| !held.iter().any(|answer| answer.end == Some(*end)));
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
        let unpaired = std::mem::take(&mut self.unpaired);
        // The turn message as it was at that barrier: the lines read again
        // go into it again.
        let open = self.committed_open.take();
        *self = Self::new(offset, calls);
        self.open.clone_from(&open);
        self.committed_open = open;
        self.held = held;
        self.answered_ends = answered_ends;
        self.unpaired = unpaired;
        self.ends_unclaimed = ends_unclaimed;
        self.ends_until = ends_until;
        self.refused_warned = refused_warned;
        self.file_seen = file_seen;
        self.next_read = Some(at);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::hub::chat::Chat;

    /// A stream message; its text does not matter here.
    fn any_op() -> Op {
        stream_op("x")
    }

    fn stream_op(text: &str) -> Op {
        Op::Stream {
            chat: Chat::Group,
            thread_id: 100,
            text: text.into(),
            html: None,
            rich: None,
            merge: false,
            restart: false,
            notify: false,
            into: None,
        }
    }

    fn call(id: &str, line: &str) -> StreamItem {
        StreamItem::Call {
            id: id.into(),
            line: line.into(),
        }
    }

    fn result(id: &str, error: Option<&str>) -> StreamItem {
        StreamItem::Result {
            id: id.into(),
            error: error.map(str::to_owned),
        }
    }

    fn sends(steps: &[Step]) -> Vec<&str> {
        steps
            .iter()
            .filter_map(|step| match step {
                Step::Send { text, .. } => Some(text.as_str()),
                _ => None,
            })
            .collect()
    }

    /// Applies lines one by one, like the actor.
    fn run(calls: &mut Vec<PendingCall>, lines: &[Vec<StreamItem>]) -> Vec<Step> {
        let mut receipts = Vec::new();
        lines
            .iter()
            .flat_map(|items| apply_line(calls, &mut receipts, items))
            .collect()
    }

    #[test]
    fn results_out_of_call_order_wait_and_go_in_call_order() {
        let mut calls = Vec::new();
        let first = run(
            &mut calls,
            &[
                vec![StreamItem::Prompt { text: "go".into() }],
                vec![StreamItem::Note {
                    text: "Running.".into(),
                }],
                vec![call("a", "• Bash: A")],
                vec![call("b", "• Edit: B")],
                vec![result("b", Some("not found"))],
            ],
        );
        // B is done, but A was called first and is still running.
        assert_eq!(sends(&first), ["> go", "Running."]);
        let second = run(&mut calls, &[vec![result("a", None)]]);
        assert_eq!(sends(&second), ["• Bash: A ✓", "• Edit: B ✗ not found"]);
        assert!(
            second
                .iter()
                .all(|step| matches!(step, Step::Send { merge: true, .. }))
        );
        assert!(calls.is_empty());
    }

    /// TASK-025: thinking is its own message, marked, after the finished
    /// calls before it; like a tool line it may join neighbours under the limit.
    #[test]
    fn thinking_goes_as_its_own_message_in_turn_order() {
        let mut calls = Vec::new();
        let steps = run(
            &mut calls,
            &[
                vec![StreamItem::Prompt { text: "go".into() }],
                vec![call("a", "• Bash: A")],
                vec![result("a", None)],
                vec![call("b", "• Bash: B")],
                vec![StreamItem::Thinking {
                    text: "Checking cargo.".into(),
                }],
                vec![StreamItem::TurnEnd],
            ],
        );
        assert_eq!(
            steps,
            [
                Step::NewTurn,
                Step::Send {
                    text: "> go".into(),
                    merge: false,
                    format: Format::Code,
                    piece: Piece::Prompt,
                },
                Step::Send {
                    text: "• Bash: A ✓".into(),
                    merge: true,
                    format: Format::Plain,
                    piece: Piece::Tool,
                },
                Step::Send {
                    text: "\u{1F4AD} Checking cargo.".into(),
                    merge: true,
                    format: Format::Markdown,
                    piece: Piece::Thinking,
                },
                Step::TurnEnd,
            ]
        );
        assert!(calls.is_empty());
    }

    /// TASK-073: each message says what it shows, set where it is made.
    #[test]
    fn every_step_names_its_piece() {
        let mut calls = Vec::new();
        let steps = run(
            &mut calls,
            &[
                vec![StreamItem::Prompt { text: "go".into() }],
                vec![StreamItem::Note {
                    text: "Looking.".into(),
                }],
                vec![StreamItem::Thinking {
                    text: "Hmm.".into(),
                }],
                vec![call("a", "• Bash: A")],
                vec![result("a", None)],
                vec![call("b", "• Bash: B")],
                vec![result("b", Some("no"))],
                vec![StreamItem::Note {
                    text: "[Request interrupted by user]".into(),
                }],
                vec![StreamItem::TurnEnd],
            ],
        );
        let pieces: Vec<(&str, Piece)> = steps
            .iter()
            .filter_map(|step| match step {
                Step::Send { text, piece, .. } => Some((text.as_str(), *piece)),
                _ => None,
            })
            .collect();
        assert_eq!(
            pieces,
            [
                ("> go", Piece::Prompt),
                ("Looking.", Piece::Text),
                ("\u{1F4AD} Hmm.", Piece::Thinking),
                ("• Bash: A ✓", Piece::Tool),
                ("• Bash: B ✗ no", Piece::Tool),
                ("[Request interrupted by user]", Piece::Interrupt),
            ]
        );
    }

    #[test]
    fn a_turn_end_lets_finished_calls_go_and_never_marks_an_unfinished_one() {
        let mut calls = Vec::new();
        let steps = run(
            &mut calls,
            &[
                vec![call("a", "• Bash: A"), call("b", "• Bash: B")],
                vec![result("b", None)],
                vec![StreamItem::TurnEnd],
            ],
        );
        assert_eq!(
            steps,
            [
                Step::Send {
                    text: "• Bash: B ✓".into(),
                    merge: true,
                    format: Format::Plain,
                    piece: Piece::Tool,
                },
                Step::TurnEnd
            ]
        );
        assert!(calls.is_empty());
        // A late result of the flushed call shows nothing.
        assert!(run(&mut calls, &[vec![result("a", None)]]).is_empty());
    }

    #[test]
    fn open_calls_are_bounded_without_blocking_later_ones() {
        let mut calls = Vec::new();
        let mut lines: Vec<Vec<StreamItem>> = (0..MAX_CALLS)
            .map(|n| vec![call(&format!("t{n}"), &format!("c{n}"))])
            .collect();
        // t1 finishes while t0 never does.
        lines.push(vec![result("t1", None)]);
        lines.push(vec![call("late", "late")]);
        let steps = run(&mut calls, &lines);
        assert_eq!(sends(&steps), ["c1 ✓"]);
        assert_eq!(calls.len(), MAX_CALLS - 1);
        assert_eq!(calls[0].id, "t2");
    }

    /// A message of the group.
    fn key(id: i64) -> MessageKey {
        MessageKey::new(Chat::Group, id)
    }

    fn keys(ids: &[i64]) -> Vec<MessageKey> {
        ids.iter().copied().map(key).collect()
    }

    #[test]
    fn only_a_received_message_turns_to_working_and_only_once() {
        let mut stream = Stream::default();
        receipt(&mut stream, key(7));
        receipt(&mut stream, key(7));
        let steps = apply_line(
            &mut Vec::new(),
            &mut stream.receipts,
            &[
                StreamItem::Channel {
                    message_id: 8,
                    private: false,
                },
                StreamItem::Channel {
                    message_id: 7,
                    private: false,
                },
                StreamItem::Channel {
                    message_id: 7,
                    private: false,
                },
            ],
        );
        assert_eq!(steps, [Step::Working(key(7))]);
        assert!(stream.receipts.is_empty());
        for id in 0..(MAX_RECEIPTS as i64 + 5) {
            receipt(&mut stream, key(id));
        }
        assert_eq!(stream.receipts.len(), MAX_RECEIPTS);
        assert_eq!(stream.receipts[0], key(5));
    }

    /// TASK-061: the same message id in the group and in the owner's
    /// private chat are two receipts; a record turns only its own place's.
    #[test]
    fn a_record_turns_only_the_receipt_of_its_place() {
        let private = MessageKey::new(
            Chat::Private(crate::hub::chat::PrivateChat::of_user(7_319_402_518)),
            7,
        );
        let mut stream = Stream::default();
        receipt(&mut stream, key(7));
        receipt(&mut stream, private);
        let record = |private| StreamItem::Channel {
            message_id: 7,
            private,
        };
        let steps = apply_line(&mut Vec::new(), &mut stream.receipts, &[record(true)]);
        assert_eq!(steps, [Step::Working(private)]);
        assert_eq!(stream.receipts, [key(7)]);
        let steps = apply_line(&mut Vec::new(), &mut stream.receipts, &[record(true)]);
        assert!(steps.is_empty(), "the group's receipt stays");
        let steps = apply_line(&mut Vec::new(), &mut stream.receipts, &[record(false)]);
        assert_eq!(steps, [Step::Working(key(7))]);
        assert!(stream.receipts.is_empty());
    }

    #[test]
    fn a_burst_waits_on_its_last_message_and_its_parts_go_with_it_once() {
        let mut stream = Stream::default();
        receipt_parts(&mut stream, &keys(&[1, 2, 3]));
        receipt_parts(&mut stream, &keys(&[4]));
        assert_eq!(stream.receipts, keys(&[3, 4]));
        assert_eq!(stream.parts, [(key(3), keys(&[1, 2]))]);
        let steps = apply_line(
            &mut Vec::new(),
            &mut stream.receipts,
            &[StreamItem::Channel {
                message_id: 3,
                private: false,
            }],
        );
        assert_eq!(steps, [Step::Working(key(3))]);
        assert_eq!(take_parts(&mut stream, key(3)), keys(&[1, 2]));
        assert!(take_parts(&mut stream, key(3)).is_empty(), "only once");
        assert!(
            take_parts(&mut stream, key(4)).is_empty(),
            "a lone message has none"
        );
        // Parts leave with their receipt when newer ones push it out.
        receipt_parts(&mut stream, &keys(&[10, 11]));
        for id in 100..(100 + MAX_RECEIPTS as i64) {
            receipt(&mut stream, key(id));
        }
        assert!(stream.parts.is_empty());
    }

    #[test]
    fn the_offset_moves_only_over_accepted_messages() {
        let mut live = Live::new(Some(0), Vec::new());
        let a = live.sent(&any_op());
        let b = live.sent(&any_op());
        live.calls.push(PendingCall {
            id: "open".into(),
            line: "x".into(),
            done: false,
            error: None,
        });
        live.barrier(25);
        assert_eq!(live.unanswered(), 2);
        live.answered(b, true);
        assert_eq!(live.advance(), None);
        live.answered(a, true);
        let (offset, calls) = live.advance().unwrap();
        assert_eq!(offset, 25);
        assert_eq!(calls[0].id, "open");
        assert_eq!(live.unanswered(), 0);
        // A read without messages moves it at once.
        live.barrier(40);
        assert_eq!(live.advance().map(|(offset, _)| offset), Some(40));
    }

    #[test]
    fn a_refused_message_stops_the_offset_until_the_stream_rewinds() {
        let mut live = Live::new(Some(0), Vec::new());
        let a = live.sent(&any_op());
        live.barrier(10);
        let b = live.sent(&any_op());
        live.barrier(20);
        let c = live.sent(&any_op());
        live.barrier(30);
        live.answered(a, true);
        assert_eq!(live.advance().map(|(offset, _)| offset), Some(10));
        live.answered(b, false);
        live.answered(c, true);
        assert_eq!(live.advance(), None, "nothing passes a refused message");
        assert!(live.stuck());
        live.rewind(Some(10), Vec::new(), Instant::now(), Duration::ZERO);
        assert_eq!(live.read_at, Some(10));
        assert!(!live.stuck());
        assert_eq!(live.unanswered(), 0);
    }

    #[test]
    fn a_rewind_holds_again_the_answers_not_in_the_topic_before_the_held_ones() {
        let now = Instant::now();
        let held = |answer: &str| Held {
            place: Place::topic(Chat::Group, 100),
            answer: answer.into(),
            until: now,
            end: None,
            gone: None,
            rich: false,
            rich_here: false,
        };
        let mut live = Live::new(Some(0), Vec::new());
        let line = live.sent(&any_op());
        let first = live.sent_answer(held("first"), &any_op());
        live.barrier(10);
        let refused = live.sent(&any_op());
        let second = live.sent_answer(held("second"), &any_op());
        live.barrier(20);
        live.held.push_back(Held {
            until: now + Duration::from_secs(60),
            ..held("still held")
        });
        live.answered(line, true);
        live.answered(first, true);
        live.answered(refused, false);
        // Dropped unsent behind the refused line.
        live.answered(second, false);
        assert_eq!(live.advance().map(|(offset, _)| offset), Some(10));
        assert!(live.stuck());
        let at = now + Duration::from_secs(5);
        live.rewind(Some(10), Vec::new(), at, Duration::from_secs(1));
        let answers: Vec<(&str, Instant)> = live
            .held
            .iter()
            .map(|held| (held.answer.as_str(), held.until))
            .collect();
        assert_eq!(
            answers,
            [
                ("second", at + Duration::from_secs(1)),
                ("still held", now + Duration::from_secs(60)),
            ],
            "the accepted answer is not held again"
        );
        assert_eq!(live.unanswered(), 0);
        assert!(live.restart, "the next message restarts the stream");
    }

    /// TASK-023 review I1: an answer that went by its timeout before its
    /// turn end was read takes that turn end when it comes (nothing else
    /// goes there, nothing is left claimable); held again by a rewind it
    /// waits for that turn end again.
    #[test]
    fn an_answer_gone_by_its_timeout_keeps_its_turn_end() {
        let now = Instant::now();
        let held = |answer: &str| Held {
            place: Place::topic(Chat::Group, 100),
            answer: answer.into(),
            until: now,
            end: Some(40),
            gone: None,
            rich: false,
            rich_here: false,
        };
        let mut live = Live::new(Some(0), Vec::new());
        let mut early = held("early");
        live.answered_early(&mut early, now);
        let number = live.sent_answer(early, &any_op());
        live.answered(number, true);
        live.held.push_back(Held {
            end: None,
            ..held("next")
        });
        assert!(live.turn_end(40).is_none(), "its answer is in the topic");
        assert!(live.ends_unclaimed.is_empty());
        assert_eq!(live.held.len(), 1, "the next answer waits for its own end");

        let mut live = Live::new(Some(0), Vec::new());
        let mut early = held("early");
        live.answered_early(&mut early, now);
        let number = live.sent_answer(early, &any_op());
        live.answered(number, false);
        assert!(live.stuck());
        live.rewind(Some(0), Vec::new(), now, Duration::ZERO);
        assert!(live.turn_end(20).is_none(), "not its turn end");
        assert_eq!(live.ends_unclaimed, [20]);
        assert_eq!(
            live.turn_end(40).map(|held| held.answer),
            Some("early".into())
        );
    }

    /// TASK-023 QA BUG-1: an answer that goes outside the stream (a file, a
    /// blank Stop) marks its turn end; a rewind that reads that end again
    /// lets nothing go and leaves nothing claimable.
    #[test]
    fn a_turn_end_answered_outside_the_stream_is_not_claimable_when_read_again() {
        let now = Instant::now();
        let mut live = Live::new(Some(0), Vec::new());
        live.held.push_back(Held {
            place: Place::topic(Chat::Group, 100),
            answer: "a file".into(),
            until: now,
            end: None,
            gone: None,
            rich: false,
            rich_here: false,
        });
        let doc = live.turn_end(40).expect("its answer");
        live.answered_outside(&doc);
        live.answered_outside(&doc);
        let line = live.sent(&any_op());
        live.barrier(80);
        live.answered(line, false);
        assert!(live.stuck());
        live.rewind(Some(0), Vec::new(), now, Duration::ZERO);
        live.held.push_back(Held {
            place: Place::topic(Chat::Group, 100),
            answer: "next".into(),
            until: now,
            end: None,
            gone: None,
            rich: false,
            rich_here: false,
        });
        assert!(live.turn_end(40).is_none(), "answered already");
        assert!(live.ends_unclaimed.is_empty(), "nothing to claim");
        assert_eq!(live.held.len(), 1, "the next answer waits for its own end");
        assert_eq!(
            live.turn_end(70).map(|held| held.answer),
            Some("next".into())
        );
    }

    /// TASK-023 QA BUG-2: the turn end of an answer in the topic is read
    /// again by two rewinds to the same barrier; neither read leaves it
    /// claimable. Its mark goes once a barrier passes it.
    #[test]
    fn a_turn_end_read_again_by_two_rewinds_is_not_claimable() {
        let now = Instant::now();
        let mut live = Live::new(Some(0), Vec::new());
        live.held.push_back(Held {
            place: Place::topic(Chat::Group, 100),
            answer: "first".into(),
            until: now,
            end: None,
            gone: None,
            rich: false,
            rich_here: false,
        });
        let first = live.turn_end(40).expect("its answer");
        let number = live.sent_answer(first, &any_op());
        live.answered(number, true);
        for _ in 0..2 {
            let line = live.sent(&any_op());
            live.barrier(80);
            live.answered(line, false);
            assert!(live.advance().is_none());
            assert!(live.stuck());
            live.rewind(Some(0), Vec::new(), now, Duration::ZERO);
            assert!(live.turn_end(40).is_none(), "answered already");
            assert!(live.ends_unclaimed.is_empty(), "nothing to claim");
        }
        let line = live.sent(&any_op());
        live.barrier(80);
        live.answered(line, true);
        assert_eq!(live.advance().map(|(offset, _)| offset), Some(80));
        assert!(live.answered_ends.is_empty(), "a barrier past it drops it");
    }

    #[test]
    fn turn_ends_read_before_a_new_turn_stay_claimable_until_they_lapse() {
        use std::time::Duration;
        let now = Instant::now();
        let mut live = Live::new(Some(0), Vec::new());
        assert_eq!(live.claim_end(now), None, "nothing read");
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
        assert_eq!(live.claim_end(now + Duration::from_secs(3600)), None);
    }

    #[test]
    fn idle_reads_while_a_message_waits_keep_one_barrier() {
        let mut live = Live::new(Some(0), Vec::new());
        let a = live.sent(&any_op());
        live.barrier(10);
        // 60 s of idle reads every 300 ms while Telegram makes `a` wait.
        for to in 0..200 {
            if to == 150 {
                live.calls.push(PendingCall {
                    id: "open".into(),
                    line: "x".into(),
                    done: false,
                    error: None,
                });
            }
            live.barrier(10 + to);
        }
        assert_eq!(live.waiting.len(), 2, "{:?}", live.waiting);
        assert_eq!(live.read_at, Some(209));
        assert_eq!(live.advance(), None);
        live.answered(a, true);
        let (offset, calls) = live.advance().unwrap();
        assert_eq!(offset, 209);
        assert_eq!(calls[0].id, "open");
        assert!(live.waiting.is_empty());
        // A message between two barriers keeps both.
        let b = live.sent(&any_op());
        live.barrier(220);
        let c = live.sent(&any_op());
        live.barrier(230);
        assert_eq!(live.waiting.len(), 4);
        live.answered(b, true);
        assert_eq!(live.advance().map(|(offset, _)| offset), Some(220));
        live.answered(c, true);
        assert_eq!(live.advance().map(|(offset, _)| offset), Some(230));
    }

    fn unpaired(answer: &str, now: Instant) -> Held {
        Held {
            place: Place::topic(Chat::Group, 100),
            answer: answer.into(),
            until: now,
            end: None,
            gone: None,
            rich: false,
            rich_here: false,
        }
    }

    /// TASK-024 item 2: an answer that went by its timeout before any turn
    /// end was read takes the next one; the answer after it waits for its
    /// own instead of going one turn early, turn after turn.
    #[test]
    fn an_answer_gone_unpaired_takes_the_next_turn_end_and_shifts_nothing() {
        let now = Instant::now();
        let mut live = Live::new(Some(0), Vec::new());
        let mut first = unpaired("first", now);
        live.answered_early(&mut first, now);
        live.held.push_back(unpaired("second", now));
        assert!(live.turn_end(40).is_none(), "the turn end of \"first\"");
        assert!(live.ends_unclaimed.is_empty(), "not left for a Stop");
        assert_eq!(
            live.turn_end(80).map(|held| held.answer),
            Some("second".into())
        );
        // Read again after a rewind, the end of "first" lets nothing go.
        live.held.push_back(unpaired("third", now));
        assert!(live.turn_end(40).is_none());
        assert_eq!(live.held.len(), 1);
        // A blank Stop that went unpaired takes its (empty text) turn end too.
        live.answered_early(&mut unpaired(" ", now), now);
        assert!(live.turn_end(120).is_none(), "the end of the blank Stop");
        assert_eq!(
            live.turn_end(160).map(|held| held.answer),
            Some("third".into())
        );
    }

    /// TASK-024 review 2: the turn end of a blank Stop that went by its
    /// timeout is its own; the next answer waits for its own end. Without a
    /// turn end its debt lapses like any other.
    #[test]
    fn a_blank_answer_gone_unpaired_keeps_its_turn_end_or_lapses() {
        let now = Instant::now();
        let mut live = Live::new(Some(0), Vec::new());
        live.answered_early(&mut unpaired(" ", now), now);
        live.held.push_back(unpaired("y", now));
        assert!(live.turn_end(40).is_none(), "40 is the blank Stop's end");
        assert_eq!(live.turn_end(80).map(|held| held.answer), Some("y".into()));

        let mut live = Live::new(Some(0), Vec::new());
        live.answered_early(&mut unpaired("", now), now);
        live.lapse_unpaired(now + Duration::from_millis(1));
        live.held.push_back(unpaired("y", now));
        assert_eq!(live.turn_end(40).map(|held| held.answer), Some("y".into()));
    }

    /// TASK-024 review 1: an answer that went unpaired and was refused is
    /// held again by the rewind, and its debt goes with it: the answer takes
    /// its own turn end, and the next answer is not shifted onto a later one.
    #[test]
    fn a_rewind_that_holds_an_unpaired_answer_again_takes_its_debt_back() {
        let now = Instant::now();
        let mut live = Live::new(Some(0), Vec::new());
        let mut x = unpaired("x", now);
        live.answered_early(&mut x, now);
        let n = live.sent_answer(x, &any_op());
        live.answered(n, false);
        assert!(live.stuck());
        live.rewind(Some(0), Vec::new(), now, Duration::ZERO);
        assert_eq!(live.held.len(), 1, "x held again");
        assert!(live.unpaired.is_empty(), "its debt went with it");
        assert_eq!(live.turn_end(40).map(|held| held.answer), Some("x".into()));
        live.held.push_back(unpaired("y", now));
        assert_eq!(live.turn_end(80).map(|held| held.answer), Some("y".into()));
    }

    /// Two answers went unpaired at the same instant (one pump); only the
    /// refused one is held again, so exactly one debt goes: the accepted
    /// one's turn end still lets nothing go.
    #[test]
    fn a_rewind_takes_back_one_debt_of_the_same_instant() {
        let now = Instant::now();
        let mut live = Live::new(Some(0), Vec::new());
        let mut w = unpaired("w", now);
        live.answered_early(&mut w, now);
        let mut x = unpaired("x", now);
        live.answered_early(&mut x, now);
        let w = live.sent_answer(w, &any_op());
        let x = live.sent_answer(x, &any_op());
        live.answered(w, true);
        live.answered(x, false);
        live.rewind(Some(0), Vec::new(), now, Duration::ZERO);
        assert_eq!(live.unpaired.len(), 1, "w's debt stays");
        assert!(live.turn_end(40).is_none(), "the end of w");
        assert_eq!(live.turn_end(80).map(|held| held.answer), Some("x".into()));
    }

    /// TASK-024 item 2 with a blocking Stop hook of the user: one turn
    /// answers twice, both answers go by their timeout, then both turn ends
    /// are read; the next turn's answer still waits for its own end.
    #[test]
    fn two_answers_of_one_turn_gone_unpaired_take_both_of_its_turn_ends() {
        let now = Instant::now();
        let mut live = Live::new(Some(0), Vec::new());
        for answer in ["first", "second"] {
            live.answered_early(&mut unpaired(answer, now), now);
        }
        live.held.push_back(unpaired("next turn", now));
        assert!(live.turn_end(40).is_none());
        assert!(live.turn_end(60).is_none());
        assert_eq!(live.held.len(), 1, "waits for its own turn end");
        assert_eq!(
            live.turn_end(90).map(|held| held.answer),
            Some("next turn".into())
        );
    }

    /// An unpaired answer whose turn end is not in the file by a read to its
    /// end asked later has none; it does not take a later turn's end.
    #[test]
    fn an_unpaired_answer_without_a_turn_end_lapses_at_a_read_to_the_end() {
        let now = Instant::now();
        let mut live = Live::new(Some(0), Vec::new());
        live.answered_early(&mut unpaired("no end", now), now);
        // A read asked before it went keeps it.
        live.lapse_unpaired(now - Duration::from_millis(1));
        live.held.push_back(unpaired("later", now));
        live.lapse_unpaired(now + Duration::from_millis(1));
        assert_eq!(
            live.turn_end(40).map(|held| held.answer),
            Some("later".into())
        );
        // It survives a rewind until then.
        let mut live = Live::new(Some(0), Vec::new());
        live.answered_early(&mut unpaired("gone", now), now);
        live.rewind(Some(0), Vec::new(), now, Duration::ZERO);
        live.held.push_back(unpaired("later", now));
        assert!(live.turn_end(40).is_none(), "the end of \"gone\"");
    }

    /// TASK-024 item 3: a reset reads a replaced file from 0; turn-end marks
    /// of the old file (answered, claimable, of held answers) mean nothing
    /// there.
    #[test]
    fn a_reset_forgets_every_turn_end_of_the_old_file() {
        let now = Instant::now();
        let mut live = Live::new(Some(0), Vec::new());
        live.answered_outside(&Held {
            end: Some(40),
            ..unpaired("a file", now)
        });
        live.answered_early(&mut unpaired("gone", now), now);
        live.ends_unclaimed.push_back(90);
        live.held.push_back(Held {
            end: Some(60),
            ..unpaired("held again", now)
        });
        let number = live.sent_answer(
            Held {
                end: Some(70),
                ..unpaired("in flight", now)
            },
            &any_op(),
        );
        live.reset();
        assert_eq!(live.read_at, Some(0));
        assert!(live.answered_ends.is_empty());
        assert!(live.ends_unclaimed.is_empty());
        assert_eq!(live.claim_end(now), None);
        // The held answer takes the first turn end of the new file.
        assert_eq!(
            live.turn_end(40).map(|held| held.answer),
            Some("held again".into())
        );
        // The answer in flight marks no old byte when it is accepted.
        live.answered(number, true);
        live.barrier(10);
        live.advance();
        assert!(live.turn_end(70).is_none());
        assert_eq!(live.ends_unclaimed, [70], "claimable: no old mark");
    }

    /// TASK-024 item 1: a stream that cannot read again sends its refused
    /// messages again as they were, in order, the first one restarting its
    /// topic's stream; accepted ones do not go twice.
    #[test]
    fn a_resend_sends_only_the_refused_messages_again_in_order() {
        let now = Instant::now();
        let mut live = Live::new(Some(0), Vec::new());
        let a = live.sent(&stream_op("a"));
        let b = live.sent(&stream_op("b"));
        live.barrier(10);
        let c = live.sent_answer(unpaired("c", now), &stream_op("c"));
        live.barrier(20);
        live.answered(a, true);
        live.answered(b, false);
        live.answered(c, false);
        assert_eq!(live.advance().map(|(offset, _)| offset), None);
        assert!(live.stuck() && !live.settled());
        let again: Vec<(u64, String, bool)> = live
            .resend()
            .into_iter()
            .map(|(number, op)| match op {
                Op::Stream { text, restart, .. } => (number, text, restart),
                other => panic!("{other:?}"),
            })
            .collect();
        assert_eq!(again, [(b, "b".into(), true), (c, "c".into(), false)]);
        assert_eq!(live.unanswered(), 2);
        assert!(live.resend().is_empty(), "nothing refused now");
        live.answered(b, true);
        live.answered(c, true);
        assert_eq!(live.advance().map(|(offset, _)| offset), Some(20));
        assert!(live.settled());
    }

    // ------------------------------------------------------------ TASK-062

    fn open(key: Option<i64>, number: u64, text: &str) -> Open {
        Open {
            key: key.map(|id| MessageKey::new(Chat::Group, id)),
            number,
            text: text.into(),
            html: None,
            compact: false,
            quote: false,
            rich: None,
            rich_view: false,
            rich_run: RichRun::Block,
        }
    }

    fn into_op(message: i64, text: &str) -> Op {
        let mut op = stream_op(text);
        if let Op::Stream { into, merge, .. } = &mut op {
            *into = Some(message);
            *merge = true;
        }
        op
    }

    #[test]
    fn the_turn_message_takes_pieces_while_they_fit_and_turns_html_once_one_is() {
        let mut turn = open(Some(7), 1, "• Bash: a ✓");
        assert!(turn.push("• Read: b ✓", None, false));
        assert_eq!(turn.text, "• Bash: a ✓\n• Read: b ✓");
        assert_eq!(turn.html, None);
        assert!(turn.push("Done <now>.", Some("<b>Done</b> &lt;now&gt;."), false));
        assert_eq!(
            turn.html.as_deref(),
            Some("• Bash: a ✓\n• Read: b ✓\n<b>Done</b> &lt;now&gt;.")
        );
        // One more that would pass Telegram's limit changes nothing.
        let long = "я".repeat(transcript::TELEGRAM_TEXT_LIMIT);
        let before = turn.clone();
        assert!(!turn.push(&long, None, false));
        assert_eq!(turn, before);
        assert_eq!(
            join("a", Some("<i>a</i>"), "<b>", None).unwrap().1.unwrap(),
            "<i>a</i>\n&lt;b&gt;"
        );
    }

    // ------------------------------------------------------------ TASK-076

    /// The pieces of a turn: (text, HTML, a tool line or 💭).
    const TURN: [(&str, Option<&str>, bool); 6] = [
        ("Смотрю.", Some("Смотрю."), false),
        ("• Bash: a ✓", None, true),
        ("• Bash: b ✓", None, true),
        ("💭 …", Some("💭 …"), true),
        ("Дальше.", Some("Дальше."), false),
        ("• Edit: x ✓", None, true),
    ];

    /// The turn message of `pieces` in view `compact`, as the slots actor
    /// builds it: the first piece makes it, the rest are pushed.
    fn built(compact: bool, pieces: &[(&str, Option<&str>, bool)]) -> Open {
        let (text, html, tool) = pieces[0];
        let mut turn = Open::new(
            Some(MessageKey::new(Chat::Group, 7)),
            1,
            text.into(),
            html.map(str::to_owned),
            compact,
            compact && tool,
            false,
        );
        for (text, html, tool) in &pieces[1..] {
            assert!(turn.push(text, *html, compact && *tool), "{text}");
        }
        turn
    }

    #[test]
    fn a_compact_turn_message_quotes_runs_of_tool_lines_and_thinking() {
        let turn = built(true, &TURN);
        assert_eq!(
            turn.html.as_deref(),
            Some(
                "Смотрю.\n<blockquote expandable>• Bash: a ✓\n• Bash: b ✓\n💭 …</blockquote>\nДальше.\n<blockquote expandable>• Edit: x ✓</blockquote>"
            )
        );
        assert!(turn.quote, "it ends in a run");
        assert!(turn.compact);
        // The plain text, for a message Telegram refuses as HTML, is the
        // full view's.
        assert_eq!(turn.text, built(false, &TURN).text);
        assert_eq!(
            turn.text,
            "Смотрю.\n• Bash: a ✓\n• Bash: b ✓\n💭 …\nДальше.\n• Edit: x ✓"
        );
        // A message that starts with a tool line starts with its quote.
        let turn = built(true, &TURN[1..4]);
        assert_eq!(
            turn.html.as_deref(),
            Some("<blockquote expandable>• Bash: a ✓\n• Bash: b ✓\n💭 …</blockquote>")
        );
    }

    #[test]
    fn a_piece_with_a_block_stays_outside_the_quote() {
        assert!(quotable(None));
        assert!(quotable(Some("💭 <b>x</b> <code>y</code>")));
        assert!(
            quotable(Some("💭 <pre>fn main() {}</pre>")),
            "Telegram takes it"
        );
        let quoting = "💭 <blockquote>said</blockquote>";
        assert!(!quotable(Some(quoting)));
        let mut turn = built(true, &TURN[1..2]);
        // The actor pushes it unquoted: it closes the run.
        assert!(turn.push("💭 said", Some(quoting), false));
        assert!(!turn.quote);
        assert!(turn.push("• Read: c ✓", None, true));
        assert_eq!(
            turn.html.as_deref(),
            Some(
                "<blockquote expandable>• Bash: a ✓</blockquote>\n💭 <blockquote>said</blockquote>\n<blockquote expandable>• Read: c ✓</blockquote>"
            )
        );
        // Tool lines are escaped inside the quote.
        let mut turn = built(true, &TURN[..1]);
        assert!(turn.push("• Bash: a<b ✓", None, true));
        assert_eq!(
            turn.html.as_deref(),
            Some("Смотрю.\n<blockquote expandable>• Bash: a&lt;b ✓</blockquote>")
        );
    }

    #[test]
    fn the_quote_tags_count_against_the_limit() {
        let limit = transcript::TELEGRAM_TEXT_LIMIT;
        let tags = QUOTE_OPEN.len() + QUOTE_CLOSE.len();
        // Without tags it fits, with them it does not.
        let long = "я".repeat(limit - "a\n".len() - tags + 1);
        let mut turn = built(true, &[("a", Some("a"), false)]);
        let before = turn.clone();
        assert!(!turn.push(&long, None, true));
        assert_eq!(turn, before);
        assert!(turn.push(&long, None, false), "unquoted it fits");
        // A first piece whose quote does not fit starts unquoted.
        let long = "я".repeat(limit - tags + 1);
        let turn = Open::new(None, 1, long.clone(), None, true, true, false);
        assert_eq!((turn.html, turn.quote), (None, false));
        let short = "я".repeat(limit - tags);
        let turn = Open::new(None, 1, short.clone(), None, true, true, false);
        assert_eq!(turn.html, Some(format!("{QUOTE_OPEN}{short}{QUOTE_CLOSE}")));
        assert!(turn.quote);
    }

    #[test]
    fn a_full_turn_message_is_as_before() {
        let turn = built(false, &TURN);
        let (mut text, mut html) = (TURN[0].0.to_owned(), TURN[0].1.map(str::to_owned));
        for (next, next_html, _) in &TURN[1..] {
            (text, html) = join(&text, html.as_deref(), next, *next_html).unwrap();
        }
        assert_eq!((turn.text, turn.html), (text, html));
        assert!(!turn.quote && !turn.compact);
        // Plain pieces stay plain.
        let turn = built(false, &TURN[1..3]);
        assert_eq!(turn.html, None);
    }

    /// A rewind goes on writing into the turn message as the last barrier
    /// Telegram fully accepted left it: the lines read again go into it
    /// again, not into a new message.
    #[test]
    fn a_rewind_goes_on_in_the_turn_message_of_the_last_accepted_barrier() {
        let mut live = Live::new(Some(0), Vec::new());
        live.open = Some(open(Some(7), 1, "one"));
        let first = live.sent(&into_op(7, "one"));
        live.barrier(10);
        live.open = Some(open(Some(7), 1, "one\ntwo"));
        let second = live.sent(&into_op(7, "one\ntwo"));
        live.barrier(20);
        live.answered(first, true);
        live.answered(second, false);
        assert_eq!(live.advance().map(|(offset, _)| offset), Some(10));
        assert!(live.stuck());
        live.rewind(Some(10), Vec::new(), Instant::now(), Duration::ZERO);
        assert_eq!(live.open, Some(open(Some(7), 1, "one")));
        // The barrier of the read again keeps it for the next rewind too.
        live.barrier(20);
        live.open = None;
        live.rewind(Some(10), Vec::new(), Instant::now(), Duration::ZERO);
        assert_eq!(live.open, Some(open(Some(7), 1, "one")));
    }

    /// Something else in the topic below the turn message closes it, also
    /// for a rewind to a barrier from before.
    #[test]
    fn a_closed_turn_message_is_not_written_into_after_a_rewind() {
        let mut live = Live::new(Some(0), Vec::new());
        live.open = Some(open(Some(7), 1, "one"));
        let first = live.sent(&into_op(7, "one"));
        live.barrier(10);
        live.answered(first, true);
        live.advance();
        live.barrier(15);
        let second = live.sent(&into_op(7, "one\ntwo"));
        live.barrier(20);
        live.close_open();
        assert_eq!(live.open, None);
        live.answered(second, false);
        live.rewind(Some(10), Vec::new(), Instant::now(), Duration::ZERO);
        assert_eq!(live.open, None);
    }

    /// The send that starts a turn message: its id reaches every copy of
    /// the message; without one (refused, merged) the message closes.
    #[test]
    fn the_id_of_a_new_turn_message_reaches_its_barriers() {
        let mut live = Live::new(Some(0), Vec::new());
        let number = live.sent(&stream_op("one"));
        live.open = Some(open(None, number, "one"));
        assert!(live.open_pending());
        live.barrier(10);
        live.opened(number + 1, Some(MessageKey::new(Chat::Group, 9)));
        assert!(live.open_pending(), "another message's answer");
        live.opened(number, Some(MessageKey::new(Chat::Group, 9)));
        assert_eq!(live.open, Some(open(Some(9), number, "one")));
        live.answered(number, true);
        live.advance();
        live.open = None;
        live.rewind(Some(10), Vec::new(), Instant::now(), Duration::ZERO);
        assert_eq!(
            live.open,
            Some(open(Some(9), number, "one")),
            "from the barrier"
        );

        let mut live = Live::new(Some(0), Vec::new());
        let number = live.sent(&stream_op("one"));
        live.open = Some(open(None, number, "one"));
        live.opened(number, None);
        assert_eq!(live.open, None);
    }

    /// The status message a refused message was to become is not the
    /// stream's to write any more: sent again, it is a new message.
    #[test]
    fn a_refused_absorb_goes_again_as_a_new_message() {
        let mut live = Live::new(Some(0), Vec::new());
        let absorb = live.sent_absorb(&into_op(500, "> go"));
        let append = live.sent(&into_op(600, "one\ntwo"));
        live.barrier(10);
        live.answered(absorb, false);
        live.answered(append, false);
        let again: Vec<Option<i64>> = live
            .resend()
            .into_iter()
            .map(|(_, op)| match op {
                Op::Stream { into, .. } => into,
                other => panic!("{other:?}"),
            })
            .collect();
        assert_eq!(again, [None, Some(600)]);
        assert!(matches!(
            live.op_of(append),
            Some(Op::Stream {
                into: Some(600),
                ..
            })
        ));
    }
    /// A refused absorb goes again as a new message with the text of the
    /// newest refused write into that status message, which is not sent:
    /// the status message is cleared away, a write into it would be lost
    /// (TASK-062 plan review).
    #[test]
    fn a_refused_absorb_goes_again_with_the_writes_into_its_message() {
        let mut live = Live::new(Some(0), Vec::new());
        let absorb = live.sent_absorb(&into_op(500, "one"));
        live.barrier(10);
        let grown = live.sent(&into_op(500, "one\ntwo"));
        let other = live.sent(&into_op(600, "x\ny"));
        live.barrier(20);
        live.answered(absorb, false);
        live.answered(grown, false);
        live.answered(other, false);
        let again: Vec<(u64, Option<i64>, String)> = live
            .resend()
            .into_iter()
            .map(|(number, op)| match op {
                Op::Stream { into, text, .. } => (number, into, text),
                other => panic!("{other:?}"),
            })
            .collect();
        assert_eq!(
            again,
            [
                (absorb, None, "one\ntwo".to_owned()),
                (other, Some(600), "x\ny".to_owned())
            ]
        );
        // What waits is what went: its answer is a new message's.
        assert!(matches!(
            live.op_of(absorb),
            Some(Op::Stream { into: None, .. })
        ));
        live.answered(absorb, true);
        live.answered(other, true);
        assert_eq!(live.advance().map(|(offset, _)| offset), Some(20));

        // Refused once more, it goes again as the same new message.
        let mut live = Live::new(Some(0), Vec::new());
        let absorb = live.sent_absorb(&into_op(500, "one"));
        live.answered(absorb, false);
        assert_eq!(live.resend().len(), 1);
        live.answered(absorb, false);
        let again = live.resend();
        assert!(matches!(
            again.as_slice(),
            [(_, Op::Stream { into: None, text, .. })] if text == "one"
        ));
    }

    /// Pieces of one line for [`join_pieces`]: quiet, not quoted.
    fn quiet(texts: &[&str]) -> Vec<(String, Option<String>, bool, bool)> {
        texts
            .iter()
            .map(|text| ((*text).to_owned(), None, true, false))
            .collect()
    }

    /// `turn` rolls `texts` of one line, with nothing to absorb.
    fn roll_quiet(turn: &mut MirrorTurn, calls: &mut u64, texts: &[&str]) -> Vec<MirrorOp> {
        let mut posted = 0;
        turn.roll(
            join_pieces(quiet(texts), false, false),
            false,
            false,
            &mut None,
            &mut posted,
            calls,
        )
    }

    fn group_key(id: i64) -> MessageKey {
        MessageKey::new(Chat::Group, id)
    }

    #[test]
    fn a_mirror_turn_skips_lines_it_had() {
        let mut turn = MirrorTurn::new("s", 10);
        assert!(!turn.takes(5));
        assert!(!turn.takes(10));
        assert!(turn.takes(20));
        assert!(!turn.takes(20));
        assert!(turn.takes(30));
        turn.reset("t", 0);
        assert_eq!(turn.session, "t");
        assert!(turn.takes(5), "a reset reads from its start");
    }

    #[test]
    fn a_mirror_turn_grows_while_its_send_waits_and_writes_once() {
        let mut turn = MirrorTurn::new("s", 0);
        let mut calls = 0;
        let ops = roll_quiet(&mut turn, &mut calls, &["a"]);
        let [
            MirrorOp::New {
                number: new,
                merge: true,
                into: None,
                ..
            },
        ] = ops.as_slice()
        else {
            panic!("{ops:?}");
        };
        // Its send waits: the next lines wait in it.
        assert!(roll_quiet(&mut turn, &mut calls, &["b"]).is_empty());
        assert!(roll_quiet(&mut turn, &mut calls, &["c"]).is_empty());
        let answered = turn.answered(*new, Some(group_key(50)), true, false, &mut calls);
        let Some(MirrorOp::Write {
            number: write,
            into,
            text,
            ..
        }) = answered.write
        else {
            panic!("{answered:?}");
        };
        assert_eq!((into, text.as_str()), (group_key(50), "a\nb\nc"));
        // While that write waits: one more, once it is answered.
        assert!(roll_quiet(&mut turn, &mut calls, &["d"]).is_empty());
        let answered = turn.answered(write, Some(group_key(50)), true, false, &mut calls);
        assert!(matches!(
            answered.write,
            Some(MirrorOp::Write { ref text, .. }) if text == "a\nb\nc\nd"
        ));
        assert_eq!(answered.retire, None);
    }

    #[test]
    fn a_mirror_turn_absorbs_the_twin_first() {
        let mut turn = MirrorTurn::new("s", 0);
        let mut calls = 0;
        let (mut twin, mut posted) = (Some(group_key(40)), 0);
        let ops = turn.roll(
            join_pieces(quiet(&["a"]), false, false),
            false,
            false,
            &mut twin,
            &mut posted,
            &mut calls,
        );
        let [
            MirrorOp::New {
                number,
                merge: false,
                into: Some(into),
                ..
            },
        ] = ops.as_slice()
        else {
            panic!("{ops:?}");
        };
        assert_eq!(*into, group_key(40));
        assert_eq!((twin, posted), (None, 0), "no new message");
        // The next piece grows it once the edit is answered.
        assert!(roll_quiet(&mut turn, &mut calls, &["b"]).is_empty());
        let answered = turn.answered(*number, None, true, false, &mut calls);
        assert!(matches!(
            answered.write,
            Some(MirrorOp::Write { into, ref text, .. }) if into == group_key(40) && text == "a\nb"
        ));
        // A later read with a twin to absorb makes nothing of it: not first.
        let (mut twin, mut posted) = (Some(group_key(41)), 1);
        let ops = turn.roll(
            join_pieces(quiet(&["x".repeat(4095).as_str()]), false, false),
            false,
            false,
            &mut twin,
            &mut posted,
            &mut calls,
        );
        assert!(matches!(ops.as_slice(), [MirrorOp::New { into: None, .. }]));
        assert_eq!((twin, posted), (Some(group_key(41)), 2));
    }

    #[test]
    fn a_prompt_absorbs_the_twin_and_closes() {
        let mut turn = MirrorTurn::new("s", 0);
        let mut calls = 0;
        let (mut twin, mut posted) = (Some(group_key(40)), 0);
        let prompt = vec![("> go".to_owned(), None, false, false)];
        let ops = turn.roll(
            join_pieces(prompt, false, false),
            false,
            false,
            &mut twin,
            &mut posted,
            &mut calls,
        );
        let [
            MirrorOp::New {
                number,
                merge: false,
                into: Some(_),
                ..
            },
        ] = ops.as_slice()
        else {
            panic!("{ops:?}");
        };
        let ops = roll_quiet(&mut turn, &mut calls, &["a"]);
        assert!(
            matches!(ops.as_slice(), [MirrorOp::New { into: None, .. }]),
            "below the prompt: {ops:?}"
        );
        // Refused, the twin may still show the old status.
        let answered = turn.answered(*number, None, false, false, &mut calls);
        assert_eq!(answered.retire, Some(group_key(40)));
    }

    #[test]
    fn a_closed_mirror_message_gets_its_last_write() {
        let mut turn = MirrorTurn::new("s", 0);
        let mut calls = 0;
        let ops = roll_quiet(&mut turn, &mut calls, &["a"]);
        let new = ops[0].number();
        let write = turn
            .answered(new, Some(group_key(50)), true, false, &mut calls)
            .write
            .map_or(0, |op| op.number());
        assert_eq!(write, 0, "nothing owed yet");
        let ops = roll_quiet(&mut turn, &mut calls, &["b"]);
        let write = ops[0].number();
        // The last lines of the turn while that write waits, then the answer.
        assert!(roll_quiet(&mut turn, &mut calls, &["c"]).is_empty());
        turn.close();
        let answered = turn.answered(write, Some(group_key(50)), true, false, &mut calls);
        let Some(MirrorOp::Write {
            number: last,
            into,
            text,
            ..
        }) = answered.write
        else {
            panic!("{answered:?}");
        };
        assert_eq!((into, text.as_str()), (group_key(50), "a\nb\nc"));
        // The next turn: a new message; the closed one grows no more.
        let ops = roll_quiet(&mut turn, &mut calls, &["d"]);
        assert!(matches!(ops.as_slice(), [MirrorOp::New { .. }]), "{ops:?}");
        assert_eq!(
            turn.answered(last, None, true, false, &mut calls),
            Answered::default()
        );
        assert!(turn.closing.is_empty());
    }

    #[test]
    fn a_refused_mirror_write_closes_the_message() {
        let mut turn = MirrorTurn::new("s", 0);
        let mut calls = 0;
        let new = roll_quiet(&mut turn, &mut calls, &["a"])[0].number();
        turn.answered(new, Some(group_key(50)), true, false, &mut calls);
        let write = roll_quiet(&mut turn, &mut calls, &["b"])[0].number();
        assert_eq!(
            turn.answered(write, None, false, false, &mut calls),
            Answered::default()
        );
        let ops = roll_quiet(&mut turn, &mut calls, &["c"]);
        assert!(
            matches!(ops.as_slice(), [MirrorOp::New { text, .. }] if text == "c"),
            "{ops:?}"
        );
        // A refused new message: the next piece is a new one too.
        let refused = ops[0].number();
        turn.answered(refused, None, false, false, &mut calls);
        assert!(matches!(
            roll_quiet(&mut turn, &mut calls, &["d"]).as_slice(),
            [MirrorOp::New { .. }]
        ));
    }

    #[test]
    fn a_refused_absorption_retires_the_twin() {
        for (gone, retire) in [(false, Some(group_key(40))), (true, None)] {
            let mut turn = MirrorTurn::new("s", 0);
            let mut calls = 0;
            let (mut twin, mut posted) = (Some(group_key(40)), 0);
            let ops = turn.roll(
                join_pieces(quiet(&["a"]), false, false),
                false,
                false,
                &mut twin,
                &mut posted,
                &mut calls,
            );
            let answered = turn.answered(ops[0].number(), None, false, gone, &mut calls);
            assert_eq!(answered.retire, retire, "gone={gone}");
            assert!(matches!(
                roll_quiet(&mut turn, &mut calls, &["b"]).as_slice(),
                [MirrorOp::New { into: None, .. }]
            ));
        }
    }

    /// TASK-078 review finding 1: a mirror turn message whose send still
    /// waits grew locally; the next piece does not fit it and starts message
    /// B. B must not be one the scheduler joins into A (answered `Merged`,
    /// dropped with what grew it, then erased by A's last write): it goes
    /// unjoinable, and every line ends up in one of the two messages.
    #[test]
    fn a_new_mirror_message_never_joins_one_on_its_way() {
        let mut turn = MirrorTurn::new("s", 0);
        let mut calls = 0;
        let a = roll_quiet(&mut turn, &mut calls, &["Looking."]);
        let [
            MirrorOp::New {
                number: a,
                merge: true,
                into: None,
                ..
            },
        ] = a.as_slice()
        else {
            panic!("{a:?}");
        };
        let t1 = "x".repeat(2500);
        assert!(
            roll_quiet(&mut turn, &mut calls, &[t1.as_str()]).is_empty(),
            "A grows locally"
        );
        let t2 = "y".repeat(2000);
        let b = roll_quiet(&mut turn, &mut calls, &[t2.as_str()]);
        let [
            MirrorOp::New {
                number: b,
                merge: false,
                into: None,
                text,
                ..
            },
        ] = b.as_slice()
        else {
            panic!("B may not join A: {b:?}");
        };
        assert_eq!(text, &t2);
        assert!(
            roll_quiet(&mut turn, &mut calls, &["z tail"]).is_empty(),
            "B grows locally"
        );
        let after_a = turn.answered(*a, Some(group_key(50)), true, false, &mut calls);
        let after_b = turn.answered(*b, Some(group_key(51)), true, false, &mut calls);
        let written: Vec<(MessageKey, String)> = [after_a.write, after_b.write]
            .into_iter()
            .flatten()
            .map(|op| match op {
                MirrorOp::Write { into, text, .. } => (into, text),
                other => panic!("{other:?}"),
            })
            .collect();
        assert_eq!(
            written,
            [
                (group_key(50), format!("Looking.\n{t1}")),
                (group_key(51), format!("{t2}\nz tail")),
            ]
        );
        // Both answered: the next new message may be joined into again.
        let next = roll_quiet(&mut turn, &mut calls, &["w".repeat(4000).as_str()]);
        assert!(
            matches!(next.as_slice(), [MirrorOp::New { merge: true, .. }]),
            "{next:?}"
        );
    }

    /// Also a new message the turn no longer keeps (closed before it grew,
    /// e.g. by a user's message) may take no later one while it waits.
    #[test]
    fn a_closed_message_on_its_way_takes_no_new_one() {
        let mut turn = MirrorTurn::new("s", 0);
        let mut calls = 0;
        let a = roll_quiet(&mut turn, &mut calls, &["a"])[0].number();
        turn.close();
        let b = roll_quiet(&mut turn, &mut calls, &["b"]);
        assert!(
            matches!(b.as_slice(), [MirrorOp::New { merge: false, .. }]),
            "{b:?}"
        );
        assert!(!turn.idle());
        turn.answered(a, Some(group_key(50)), true, false, &mut calls);
        turn.close();
        let c = roll_quiet(&mut turn, &mut calls, &["c"]);
        assert!(
            matches!(c.as_slice(), [MirrorOp::New { merge: true, .. }]),
            "{c:?}"
        );
    }

    /// TASK-078 review finding 2: the call numbers come from one counter of
    /// the actor, so a mirror turn made again for the same topic never
    /// takes a late answer of the one before as its own.
    #[test]
    fn mirror_turns_share_one_call_counter() {
        let mut calls = 0;
        let mut old = MirrorTurn::new("s", 0);
        let old_new = roll_quiet(&mut old, &mut calls, &["old"])[0].number();
        drop(old);
        let mut turn = MirrorTurn::new("s", 100);
        let new = roll_quiet(&mut turn, &mut calls, &["new"])[0].number();
        assert_ne!(old_new, new);
        // The old call's answer comes now: it is no call of this turn.
        let answered = turn.answered(old_new, Some(group_key(7)), true, false, &mut calls);
        assert_eq!(answered, Answered::default());
        assert!(
            roll_quiet(&mut turn, &mut calls, &["more"]).is_empty(),
            "still waits for its own"
        );
        let answered = turn.answered(new, Some(group_key(8)), true, false, &mut calls);
        assert!(matches!(
            answered.write,
            Some(MirrorOp::Write { into, ref text, .. }) if into == group_key(8) && text == "new\nmore"
        ));
    }

    /// TASK-078 review finding 3: a mirror turn no stream feeds any more
    /// still owes the last write of its turn message; it is idle once that
    /// went, and a stream that feeds it again starts over.
    #[test]
    fn a_left_mirror_turn_writes_what_it_owes() {
        let mut turn = MirrorTurn::new("s", 0);
        let mut calls = 0;
        let new = roll_quiet(&mut turn, &mut calls, &["a"])[0].number();
        assert!(roll_quiet(&mut turn, &mut calls, &["b"]).is_empty());
        turn.leave();
        assert_eq!(turn.session, "");
        assert!(!turn.idle(), "its last write is owed");
        let answered = turn.answered(new, Some(group_key(50)), true, false, &mut calls);
        let Some(MirrorOp::Write {
            number: last,
            into,
            text,
            ..
        }) = answered.write
        else {
            panic!("{answered:?}");
        };
        assert_eq!((into, text.as_str()), (group_key(50), "a\nb"));
        assert!(!turn.idle());
        turn.answered(last, None, true, false, &mut calls);
        assert!(turn.idle());
        turn.reset("s", 30);
        assert!(!turn.takes(30));
        assert!(turn.takes(40));
    }

    /// A new message whose answer never came (the sender dropped: no
    /// delivery, not accepted) lets the next new one be joined again.
    #[test]
    fn a_message_without_an_answer_lets_the_next_one_join() {
        let mut turn = MirrorTurn::new("s", 0);
        let mut calls = 0;
        let a = roll_quiet(&mut turn, &mut calls, &["a"])[0].number();
        assert!(turn.joinable());
        turn.answered(a, None, false, false, &mut calls);
        assert!(!turn.joinable());
        let b = roll_quiet(&mut turn, &mut calls, &["b"]);
        assert!(
            matches!(b.as_slice(), [MirrorOp::New { merge: true, .. }]),
            "{b:?}"
        );
    }

    // ------------------------------------------------------------ TASK-075

    /// [`built`] in a view with rich messages.
    fn built_rich(compact: bool, pieces: &[(&str, Option<&str>, bool)]) -> Open {
        let (text, html, tool) = pieces[0];
        let mut turn = Open::new(
            None,
            1,
            text.into(),
            html.map(str::to_owned),
            compact,
            compact && tool,
            true,
        );
        for (text, html, tool) in &pieces[1..] {
            assert!(turn.push(text, *html, compact && *tool), "{text}");
        }
        turn
    }

    #[test]
    fn a_rich_full_turn_message_puts_tool_lines_into_one_paragraph() {
        let turn = built_rich(
            false,
            &[
                ("Смотрю.", Some("Смотрю."), false),
                ("• Bash: a<b ✓", None, true),
                ("• Read: c ✓", None, true),
                ("Дальше.", Some("Дальше."), false),
            ],
        );
        assert_eq!(
            turn.rich.as_deref(),
            Some("Смотрю.\n\n<p>• Bash: a&lt;b ✓<br>• Read: c ✓</p>\n\nДальше.")
        );
        assert_eq!(turn.rich_run, RichRun::Block);
        assert!(turn.rich_view);
        // The other forms are as without rich.
        let plain = built(
            false,
            &[
                ("Смотрю.", Some("Смотрю."), false),
                ("• Bash: a<b ✓", None, true),
                ("• Read: c ✓", None, true),
                ("Дальше.", Some("Дальше."), false),
            ],
        );
        assert_eq!((&turn.text, &turn.html), (&plain.text, &plain.html));
        assert_eq!(plain.rich, None);
        assert!(!plain.rich_view);
    }

    #[test]
    fn a_rich_compact_turn_message_quotes_with_line_breaks() {
        let mut turn = built_rich(true, &TURN);
        assert_eq!(
            turn.rich.as_deref(),
            Some(
                "Смотрю.\n\n<blockquote expandable>• Bash: a ✓<br>• Bash: b ✓<br>💭 …</blockquote>\n\nДальше.\n\n<blockquote expandable>• Edit: x ✓</blockquote>"
            )
        );
        assert_eq!(turn.rich_run, RichRun::Quote);
        // A `<pre>` in 💭 keeps its line breaks.
        assert!(turn.push("💭 x", Some("💭 <pre>a\nb</pre>\nc"), true));
        assert!(
            turn.rich.as_deref().unwrap().ends_with(
                "<blockquote expandable>• Edit: x ✓<br>💭 <pre>a\nb</pre><br>c</blockquote>"
            ),
            "{:?}",
            turn.rich
        );
    }

    #[test]
    fn a_rich_markdown_piece_goes_through_rich_markdown() {
        let turn = Open::new(
            None,
            1,
            "Vec<T> ok".into(),
            Some("Vec&lt;T&gt; ok".into()),
            false,
            false,
            true,
        );
        assert_eq!(turn.rich.as_deref(), Some("Vec&lt;T> ok"));
        // A fence left open is closed: the tool line after it is no code.
        let mut turn = Open::new(
            None,
            1,
            "```rust\nlet a: Vec<u8>;".into(),
            Some("<pre>let a</pre>".into()),
            false,
            false,
            true,
        );
        assert!(turn.push("• Bash: a ✓", None, false));
        assert_eq!(
            turn.rich.as_deref(),
            Some("```rust\nlet a: Vec<u8>;\n```\n\n<p>• Bash: a ✓</p>")
        );
    }

    #[test]
    fn a_piece_whose_rich_form_does_not_fit_starts_the_next_message() {
        let mut turn = built_rich(false, &[("• x ✓", None, true)]);
        loop {
            let before = turn.clone();
            if !turn.push("• x ✓", None, false) {
                assert_eq!(turn, before, "nothing changes");
                // The rich form was the one over the limit.
                assert!(fits(&format!("{}\n• x ✓", turn.text)));
                assert!(turn.html.is_none());
                assert!(!fits(&format!(
                    "{}<br>• x ✓</p>",
                    turn.rich.as_deref().unwrap().strip_suffix("</p>").unwrap()
                )));
                break;
            }
        }
        // A first piece whose rich form does not fit: the message has none.
        let long = "<".repeat(transcript::TELEGRAM_TEXT_LIMIT / 4 + 1);
        let turn = Open::new(None, 1, long.clone(), Some(long), false, false, true);
        assert_eq!(turn.rich, None);
        assert!(turn.rich_view);
    }

    #[test]
    fn a_mirror_turn_goes_on_only_in_its_rich_setting() {
        let mut turn = MirrorTurn::new("s", 0);
        let mut calls = 0;
        let mut roll = |turn: &mut MirrorTurn, text: &str, rich: bool| {
            let mut posted = 0;
            turn.roll(
                join_pieces(quiet(&[text]), false, rich),
                false,
                rich,
                &mut None,
                &mut posted,
                &mut calls,
            )
        };
        let ops = roll(&mut turn, "a", true);
        let [MirrorOp::New { number, rich, .. }] = ops.as_slice() else {
            panic!("{ops:?}");
        };
        assert_eq!(rich.as_deref(), Some("<p>a</p>"));
        let number = *number;
        let mut answer_calls = 100;
        turn.answered(number, Some(group_key(50)), true, false, &mut answer_calls);
        let ops = roll(&mut turn, "b", true);
        assert!(
            matches!(ops.as_slice(), [MirrorOp::Write { into, rich: Some(rich), .. }]
                if *into == group_key(50) && rich == "<p>a<br>b</p>"),
            "{ops:?}"
        );
        let ops = roll(&mut turn, "c", false);
        assert!(
            matches!(
                ops.as_slice(),
                [MirrorOp::New {
                    rich: None,
                    into: None,
                    ..
                }]
            ),
            "another setting, a new message: {ops:?}"
        );
    }
}
