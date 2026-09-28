//! Mirrors of a slot's primary topic (TASK-063).
//!
//! With topics in private chats a slot shows in its owner's private chat
//! and in the group. The slots actor answers the session in one of them,
//! the primary view (the owner's), exactly as it did in the group before;
//! every message it puts there goes to each other view as a twin, and every
//! later call about that message (an edit, a delete, the stream writing
//! into it) goes to the twins as well. A press or a reply on a twin counts
//! for its primary message, so the first press in any view decides.
//!
//! This is the book of twins: which twin a primary message has in each
//! mirror chat, known once Telegram answered both sends (like the message
//! id cache of chat bridges). Pure: the actor hands out and answers.
//!
//! - A twin send waits for the answer to its primary send (by the dispatch
//!   number of that send, `seq`) and for its own. A primary send that made
//!   no message and is made again (a stream line, a status message) or ends
//!   (a prompt, a question) takes its twin away; any other keeps it unlinked
//!   (the mirror still shows it), and so does one merged into an earlier
//!   message. A prompt or question lost in a private chat (its topic
//!   deleted, the bot blocked) goes again: its twins are kept for that send
//!   ([`Landed::Again`], [`Mirror::carry`]), or one of them becomes the
//!   prompt itself.
//! - A call about a primary message whose twin is still on its way waits
//!   in the book, the newest one only (every call carries the whole text; a
//!   delete wins over an edit), and goes once the twin's id is known.
//! - A twin Telegram did not take is lost: calls about it are dropped,
//!   except content the stream writes into it, which the actor sends as a
//!   new twin message instead ([`Write::Lost`]).
//! - At most [`MAX_TWINS`] primary messages are remembered; the oldest go.
//! - The links of lasting messages (subagent blocks, Resume offers) are
//!   also kept apart, at most [`MAX_LASTING`], for `registry.json`: after a
//!   restart of the hub their edits still reach the twins.

use std::collections::{HashMap, VecDeque};

use super::chat::{Chat, MessageKey};
use super::scheduler::Op;

/// Primary messages remembered with their twins.
pub const MAX_TWINS: usize = 4096;
/// Twin sends waiting for their primary's answer or their own; one more is
/// not made (the mirror falls behind, the primary view never does).
pub const MAX_SENDS: usize = 1024;
/// Links of lasting messages kept for `registry.json`.
pub const MAX_LASTING: usize = 256;

/// What became of the primary send of a twin.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Landed {
    /// Telegram took it as this message.
    Message(i64),
    /// No message of its own and none made again: it went inside an
    /// earlier stream message, or a stream line was skipped for good. The
    /// twin stays, unlinked.
    Merged,
    /// No message: refused, superseded or no answer.
    Nothing,
    /// No message now, and the actor sends it again (a prompt or question
    /// lost in a private chat): its twins are kept, never taken away as
    /// ghosts ([`Mirror::kept`]).
    Again,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Primary {
    /// Its send, by dispatch number, has not been answered.
    Waiting(u64),
    Key(MessageKey),
    Merged,
    Nothing,
    /// It goes again; the twin waits for that send ([`Mirror::carry`]).
    Again,
}

#[derive(Debug)]
struct Send {
    chat: Chat,
    primary: Primary,
    /// The twin's own answer: `Some(None)` when it made no message.
    answer: Option<Option<i64>>,
    /// The newest call about the primary message while this is on its way.
    queued: Option<Op>,
    /// Taken away when its primary send made no message.
    ghost: bool,
    /// Its link is kept across a restart.
    lasting: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Twin {
    Sending(u64),
    Shown(i64),
    Lost,
}

/// Where a call about a primary message goes in one mirror chat.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Write {
    /// To this twin now.
    Now(MessageKey),
    /// Kept in the book until the id of twin send `u64` is known.
    Queued(u64),
    /// The twin was not made or not taken.
    Lost(Chat),
}

/// What an answer asks of the actor.
#[derive(Debug)]
pub enum Follow {
    /// A call kept for a twin whose id is known now: `op` is about the
    /// primary message, to be pointed at `twin`.
    Write { twin: MessageKey, op: Op },
    /// A call kept for a twin that was lost: stream content goes as a new
    /// twin message of `primary` in `chat`, anything else is dropped.
    Lost {
        primary: MessageKey,
        chat: Chat,
        op: Op,
    },
    /// A twin whose primary send made no message: it goes.
    Delete(MessageKey),
}

#[derive(Debug, Default)]
pub struct Mirror {
    next: u64,
    sends: HashMap<u64, Send>,
    /// Twin sends by the dispatch number of their primary send.
    by_seq: HashMap<u64, Vec<u64>>,
    twins: HashMap<MessageKey, Vec<(Chat, Twin)>>,
    /// The primary message of each twin shown.
    back: HashMap<MessageKey, MessageKey>,
    order: VecDeque<MessageKey>,
    /// Links of lasting messages, newest last.
    lasting: VecDeque<(MessageKey, MessageKey)>,
    /// `lasting` changed since [`Self::take_changed`].
    changed: bool,
    /// Twin sends kept for a primary send that goes again, by the dispatch
    /// number of the send that was lost.
    kept: HashMap<u64, Vec<u64>>,
}

impl Mirror {
    /// A twin in `chat` of the send handed out as dispatch number `seq`;
    /// its id for [`Self::twin_answered`]. `None` when too many wait.
    /// `ghost`: it goes when that send makes no message; `lasting`: its
    /// link is kept for a restart ([`Self::lasting`]).
    pub fn send(&mut self, seq: u64, chat: Chat, ghost: bool, lasting: bool) -> Option<u64> {
        let id = self.open(chat, Primary::Waiting(seq))?;
        if let Some(send) = self.sends.get_mut(&id) {
            send.ghost = ghost;
            send.lasting = lasting;
        }
        self.by_seq.entry(seq).or_default().push(id);
        Some(id)
    }

    /// A new twin in `chat` of `primary`, a message Telegram shows already
    /// (stream content whose twin was lost, [`Follow::Lost`]).
    pub fn send_for(&mut self, primary: MessageKey, chat: Chat) -> Option<u64> {
        let id = self.open(chat, Primary::Key(primary))?;
        self.set(primary, chat, Twin::Sending(id));
        Some(id)
    }

    fn open(&mut self, chat: Chat, primary: Primary) -> Option<u64> {
        if self.sends.len() >= MAX_SENDS {
            return None;
        }
        self.next += 1;
        self.sends.insert(
            self.next,
            Send {
                chat,
                primary,
                answer: None,
                queued: None,
                ghost: false,
                lasting: false,
            },
        );
        Some(self.next)
    }

    /// Telegram answered the send handed out as `seq`.
    pub fn primary_answered(&mut self, seq: u64, chat: Chat, landed: Landed) -> Vec<Follow> {
        let Some(ids) = self.by_seq.remove(&seq) else {
            return Vec::new();
        };
        let primary = match landed {
            Landed::Message(id) => Primary::Key(MessageKey::new(chat, id)),
            Landed::Merged => Primary::Merged,
            Landed::Nothing => Primary::Nothing,
            Landed::Again => {
                for id in &ids {
                    if let Some(send) = self.sends.get_mut(id) {
                        send.primary = Primary::Again;
                    }
                }
                self.kept.entry(seq).or_default().extend(ids);
                return Vec::new();
            }
        };
        let mut follows = Vec::new();
        for id in ids {
            let Some(send) = self.sends.get_mut(&id) else {
                continue;
            };
            send.primary = primary;
            let twin_chat = send.chat;
            if let Primary::Key(key) = primary {
                self.set(key, twin_chat, Twin::Sending(id));
            }
            follows.extend(self.settle(id));
        }
        follows
    }

    /// Telegram answered twin send `id`: the message it made, if any.
    pub fn twin_answered(&mut self, id: u64, message_id: Option<i64>) -> Vec<Follow> {
        let Some(send) = self.sends.get_mut(&id) else {
            return Vec::new();
        };
        send.answer = Some(message_id);
        self.settle(id).into_iter().collect()
    }

    /// Both answers of twin send `id` are in: it is linked, taken away or
    /// forgotten.
    fn settle(&mut self, id: u64) -> Option<Follow> {
        let send = self.sends.get(&id)?;
        let answer = send.answer?;
        if matches!(send.primary, Primary::Waiting(_) | Primary::Again) {
            return None;
        }
        let send = self.sends.remove(&id)?;
        match (send.primary, answer) {
            (Primary::Key(primary), Some(twin_id)) => {
                let twin = MessageKey::new(send.chat, twin_id);
                self.set(primary, send.chat, Twin::Shown(twin_id));
                self.back.insert(twin, primary);
                if send.lasting {
                    self.keep(primary, twin);
                    self.changed = true;
                }
                send.queued.map(|op| Follow::Write { twin, op })
            }
            (Primary::Key(primary), None) => {
                self.set(primary, send.chat, Twin::Lost);
                send.queued.map(|op| Follow::Lost {
                    primary,
                    chat: send.chat,
                    op,
                })
            }
            (Primary::Nothing, Some(twin_id)) if send.ghost => {
                Some(Follow::Delete(MessageKey::new(send.chat, twin_id)))
            }
            _ => None,
        }
    }

    /// The twin of `primary` in `chat` is now `twin`.
    fn set(&mut self, primary: MessageKey, chat: Chat, twin: Twin) {
        let known = self.twins.contains_key(&primary);
        let twins = self.twins.entry(primary).or_default();
        let before = match twins.iter_mut().find(|(of, _)| *of == chat) {
            Some((_, state)) => Some(std::mem::replace(state, twin)),
            None => {
                twins.push((chat, twin));
                None
            }
        };
        if let Some(Twin::Shown(old)) = before.filter(|before| *before != twin) {
            self.back.remove(&MessageKey::new(chat, old));
        }
        if !known {
            self.order.push_back(primary);
            while self.order.len() > MAX_TWINS {
                if let Some(oldest) = self.order.pop_front() {
                    self.forget(oldest);
                }
            }
        }
    }

    fn forget(&mut self, primary: MessageKey) {
        for (chat, twin) in self.twins.remove(&primary).unwrap_or_default() {
            if let Twin::Shown(id) = twin {
                self.back.remove(&MessageKey::new(chat, id));
            }
        }
    }

    /// Where a call about `primary` (`op`, an edit, a delete or stream
    /// content written into it) goes in each mirror chat the book knows a
    /// twin in; one queued replaces the one queued before it, but never a
    /// delete. Empty: the book knows no twin of it.
    pub fn write(&mut self, primary: MessageKey, op: &Op) -> Vec<Write> {
        let Some(twins) = self.twins.get(&primary) else {
            return Vec::new();
        };
        let mut writes = Vec::new();
        for &(chat, twin) in twins {
            match twin {
                Twin::Shown(id) => writes.push(Write::Now(MessageKey::new(chat, id))),
                Twin::Lost => writes.push(Write::Lost(chat)),
                Twin::Sending(id) => {
                    if let Some(send) = self.sends.get_mut(&id)
                        && !matches!(send.queued, Some(Op::Delete { .. }))
                    {
                        send.queued = Some(op.clone());
                    }
                    writes.push(Write::Queued(id));
                }
            }
        }
        writes
    }

    /// The primary send `seq` has twin sends (it is not answered yet).
    pub fn twinned(&self, seq: u64) -> bool {
        self.by_seq.contains_key(&seq)
    }

    /// The twins kept for lost primary send `seq` ([`Landed::Again`]): each
    /// one's chat and, once Telegram answered it, the message it made.
    pub fn kept(&self, seq: u64) -> Vec<(Chat, Option<Option<i64>>)> {
        self.kept
            .get(&seq)
            .into_iter()
            .flatten()
            .filter_map(|id| self.sends.get(id))
            .map(|send| (send.chat, send.answer))
            .collect()
    }

    /// The lost primary send whose kept twin `twin` is.
    pub fn kept_of(&self, twin: MessageKey) -> Option<u64> {
        self.kept.iter().find_map(|(seq, ids)| {
            ids.iter()
                .filter_map(|id| self.sends.get(id))
                .any(|send| send.chat == twin.chat && send.answer == Some(Some(twin.id)))
                .then_some(*seq)
        })
    }

    /// Lost primary sends with kept twins.
    pub fn lost(&self) -> Vec<u64> {
        self.kept.keys().copied().collect()
    }

    /// Lost primary send `old` went again as dispatch number `new`: its kept
    /// twins wait for that send's answer, and a twin that made no message is
    /// forgotten. The chats that keep a twin: no new twin goes there.
    pub fn carry(&mut self, old: u64, new: u64) -> Vec<Chat> {
        let mut chats = Vec::new();
        for id in self.kept.remove(&old).unwrap_or_default() {
            let Some(send) = self.sends.get_mut(&id) else {
                continue;
            };
            if send.answer == Some(None) {
                self.sends.remove(&id);
                continue;
            }
            send.primary = Primary::Waiting(new);
            chats.push(send.chat);
            self.by_seq.entry(new).or_default().push(id);
        }
        chats
    }

    /// The chats [`Self::carry`] would keep a twin in.
    pub fn kept_chats(&self, old: u64) -> Vec<Chat> {
        self.kept(old)
            .into_iter()
            .filter(|(_, answer)| *answer != Some(None))
            .map(|(chat, _)| chat)
            .collect()
    }

    /// Lost primary send `old` does not go again (one of its twins became
    /// the prompt itself, or the prompt ended): its kept twins are let go.
    /// The ones Telegram shows are returned for the actor to clear or keep;
    /// one still on its way is deleted once it comes, like a ghost.
    pub fn release(&mut self, old: u64) -> Vec<MessageKey> {
        let mut shown = Vec::new();
        for id in self.kept.remove(&old).unwrap_or_default() {
            let Some(send) = self.sends.get_mut(&id) else {
                continue;
            };
            match send.answer {
                Some(answer) => {
                    let chat = send.chat;
                    self.sends.remove(&id);
                    shown.extend(answer.map(|twin| MessageKey::new(chat, twin)));
                }
                None => {
                    send.primary = Primary::Nothing;
                    send.ghost = true;
                }
            }
        }
        shown
    }

    /// The twin of `primary` shown in `chat`.
    pub fn twin(&self, primary: MessageKey, chat: Chat) -> Option<MessageKey> {
        self.twins
            .get(&primary)?
            .iter()
            .find_map(|(of, twin)| match twin {
                Twin::Shown(id) if *of == chat => Some(MessageKey::new(chat, *id)),
                _ => None,
            })
    }

    /// The book knows a twin of `primary` in `chat`, in any state.
    pub fn knows(&self, primary: MessageKey, chat: Chat) -> bool {
        self.twins
            .get(&primary)
            .is_some_and(|twins| twins.iter().any(|(of, _)| *of == chat))
    }

    /// The primary message of `twin`.
    pub fn primary_of(&self, twin: MessageKey) -> Option<MessageKey> {
        self.back.get(&twin).copied()
    }

    /// `twin` shows `primary` (a status message kept in `registry.json`
    /// across a restart).
    pub fn link(&mut self, primary: MessageKey, twin: MessageKey) {
        self.set(primary, twin.chat, Twin::Shown(twin.id));
        self.back.insert(twin, primary);
    }

    /// Twin sends waiting for an answer.
    pub fn waiting(&self) -> usize {
        self.sends.len()
    }

    /// [`Self::link`] of a lasting message, as `registry.json` kept it.
    pub fn link_lasting(&mut self, primary: MessageKey, twin: MessageKey) {
        self.link(primary, twin);
        self.keep(primary, twin);
    }

    fn keep(&mut self, primary: MessageKey, twin: MessageKey) {
        self.lasting
            .retain(|&(of, to)| !(of == primary && to.chat == twin.chat));
        self.lasting.push_back((primary, twin));
        while self.lasting.len() > MAX_LASTING {
            self.lasting.pop_front();
        }
    }

    /// The links of lasting messages, oldest first.
    pub fn lasting(&self) -> Vec<(MessageKey, MessageKey)> {
        self.lasting.iter().copied().collect()
    }

    /// A lasting link came since the last call.
    pub fn take_changed(&mut self) -> bool {
        std::mem::take(&mut self.changed)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::hub::chat::PrivateChat;

    fn owner() -> Chat {
        Chat::Private(PrivateChat::of_user(7))
    }

    fn edit(chat: Chat, message_id: i64, text: &str) -> Op {
        Op::Edit {
            chat,
            message_id,
            text: text.to_owned(),
            reply_markup: None,
            background: false,
        }
    }

    #[test]
    fn a_twin_is_linked_whichever_answer_comes_first() {
        let mut mirror = Mirror::default();
        let first = mirror.send(1, Chat::Group, false, false).unwrap();
        let second = mirror.send(2, Chat::Group, false, false).unwrap();
        assert!(mirror.twin_answered(first, Some(50)).is_empty());
        assert!(
            mirror
                .primary_answered(1, owner(), Landed::Message(5))
                .is_empty()
        );
        assert!(
            mirror
                .primary_answered(2, owner(), Landed::Message(6))
                .is_empty()
        );
        assert!(mirror.twin_answered(second, Some(51)).is_empty());
        let primary = MessageKey::new(owner(), 5);
        assert_eq!(
            mirror.twin(primary, Chat::Group),
            Some(MessageKey::new(Chat::Group, 50))
        );
        assert_eq!(
            mirror.primary_of(MessageKey::new(Chat::Group, 51)),
            Some(MessageKey::new(owner(), 6))
        );
        assert_eq!(mirror.waiting(), 0);
    }

    #[test]
    fn a_call_waits_for_its_twin_and_the_newest_one_goes() {
        let mut mirror = Mirror::default();
        let id = mirror.send(1, Chat::Group, false, false).unwrap();
        mirror.primary_answered(1, owner(), Landed::Message(5));
        let primary = MessageKey::new(owner(), 5);
        assert_eq!(
            mirror.write(primary, &edit(owner(), 5, "a")),
            [Write::Queued(id)]
        );
        mirror.write(primary, &edit(owner(), 5, "b"));
        let follows = mirror.twin_answered(id, Some(50));
        let [Follow::Write { twin, op }] = follows.as_slice() else {
            panic!("{follows:?}");
        };
        assert_eq!(*twin, MessageKey::new(Chat::Group, 50));
        assert!(matches!(op, Op::Edit { text, .. } if text == "b"));
        assert_eq!(
            mirror.write(primary, &edit(owner(), 5, "c")),
            [Write::Now(MessageKey::new(Chat::Group, 50))]
        );
    }

    #[test]
    fn a_delete_waiting_for_its_twin_is_not_replaced() {
        let mut mirror = Mirror::default();
        let id = mirror.send(1, Chat::Group, false, false).unwrap();
        mirror.primary_answered(1, owner(), Landed::Message(5));
        let primary = MessageKey::new(owner(), 5);
        let delete = Op::Delete {
            chat: owner(),
            message_id: 5,
        };
        mirror.write(primary, &delete);
        mirror.write(primary, &edit(owner(), 5, "late"));
        let follows = mirror.twin_answered(id, Some(50));
        assert!(matches!(
            follows.as_slice(),
            [Follow::Write {
                op: Op::Delete { .. },
                ..
            }]
        ));
    }

    #[test]
    fn a_primary_without_a_message_takes_its_ghost_twin_away() {
        let mut mirror = Mirror::default();
        let refused = mirror.send(1, Chat::Group, true, false).unwrap();
        let merged = mirror.send(2, Chat::Group, true, false).unwrap();
        let kept = mirror.send(3, Chat::Group, false, false).unwrap();
        mirror.twin_answered(refused, Some(50));
        let follows = mirror.primary_answered(1, owner(), Landed::Nothing);
        assert!(matches!(
            follows.as_slice(),
            [Follow::Delete(key)] if *key == MessageKey::new(Chat::Group, 50)
        ));
        // A reply that did not reach the primary topic stays in the mirror.
        mirror.twin_answered(kept, Some(52));
        assert!(
            mirror
                .primary_answered(3, owner(), Landed::Nothing)
                .is_empty()
        );
        // Merged: the twin stays, unlinked.
        mirror.primary_answered(2, owner(), Landed::Merged);
        assert!(mirror.twin_answered(merged, Some(51)).is_empty());
        assert_eq!(mirror.primary_of(MessageKey::new(Chat::Group, 51)), None);
        assert_eq!(mirror.waiting(), 0);
    }

    #[test]
    fn a_prompt_that_goes_again_keeps_its_twin_for_the_next_send() {
        let mut mirror = Mirror::default();
        // The twin comes before the primary is lost, and one after.
        let early = mirror.send(1, Chat::Group, true, false).unwrap();
        mirror.twin_answered(early, Some(50));
        assert!(
            mirror
                .primary_answered(1, owner(), Landed::Again)
                .is_empty()
        );
        let late = mirror.send(2, Chat::Group, true, false).unwrap();
        mirror.primary_answered(2, owner(), Landed::Again);
        assert_eq!(mirror.kept(2), [(Chat::Group, None)]);
        assert!(mirror.twin_answered(late, Some(51)).is_empty(), "no delete");
        assert_eq!(mirror.kept(1), [(Chat::Group, Some(Some(50)))]);
        assert_eq!(mirror.kept_of(MessageKey::new(Chat::Group, 51)), Some(2));
        let mut lost = mirror.lost();
        lost.sort_unstable();
        assert_eq!(lost, [1, 2]);
        // Sent again as number 7: the kept twin is linked to that message.
        assert_eq!(mirror.kept_chats(1), [Chat::Group]);
        assert_eq!(mirror.carry(1, 7), [Chat::Group]);
        assert!(mirror.twinned(7));
        mirror.primary_answered(7, owner(), Landed::Message(9));
        assert_eq!(
            mirror.twin(MessageKey::new(owner(), 9), Chat::Group),
            Some(MessageKey::new(Chat::Group, 50))
        );
        assert_eq!(
            mirror.primary_of(MessageKey::new(Chat::Group, 50)),
            Some(MessageKey::new(owner(), 9))
        );
        // Not sent again: the twin is handed back, not deleted.
        assert_eq!(mirror.release(2), [MessageKey::new(Chat::Group, 51)]);
        assert!(mirror.lost().is_empty());
        assert_eq!(mirror.waiting(), 0);
        // One still on its way when let go is deleted once it comes.
        let unanswered = mirror.send(3, Chat::Group, true, false).unwrap();
        mirror.primary_answered(3, owner(), Landed::Again);
        assert!(mirror.release(3).is_empty());
        assert!(matches!(
            mirror.twin_answered(unanswered, Some(52)).as_slice(),
            [Follow::Delete(key)] if *key == MessageKey::new(Chat::Group, 52)
        ));
    }

    #[test]
    fn content_for_a_lost_twin_goes_back_to_the_actor() {
        let mut mirror = Mirror::default();
        let id = mirror.send(1, Chat::Group, true, false).unwrap();
        mirror.primary_answered(1, owner(), Landed::Message(5));
        let primary = MessageKey::new(owner(), 5);
        let write = Op::Stream {
            chat: owner(),
            thread_id: 9,
            text: "more".into(),
            html: None,
            merge: true,
            restart: false,
            notify: false,
            into: Some(5),
        };
        mirror.write(primary, &write);
        let follows = mirror.twin_answered(id, None);
        assert!(matches!(
            follows.as_slice(),
            [Follow::Lost { primary: key, chat: Chat::Group, .. }] if *key == primary
        ));
        assert_eq!(
            mirror.write(primary, &edit(owner(), 5, "x")),
            [Write::Lost(Chat::Group)]
        );
        // The actor sends it as a new twin, which then takes the calls.
        let again = mirror.send_for(primary, Chat::Group).unwrap();
        mirror.twin_answered(again, Some(60));
        assert_eq!(
            mirror.write(primary, &edit(owner(), 5, "y")),
            [Write::Now(MessageKey::new(Chat::Group, 60))]
        );
        assert_eq!(
            mirror.primary_of(MessageKey::new(Chat::Group, 60)),
            Some(primary)
        );
    }

    #[test]
    fn lasting_links_are_kept_apart_and_bounded() {
        let mut mirror = Mirror::default();
        let block = mirror.send(1, Chat::Group, false, true).unwrap();
        let line = mirror.send(2, Chat::Group, true, false).unwrap();
        mirror.primary_answered(1, owner(), Landed::Message(5));
        mirror.primary_answered(2, owner(), Landed::Message(6));
        mirror.twin_answered(line, Some(51));
        assert!(!mirror.take_changed(), "a stream line is not lasting");
        mirror.twin_answered(block, Some(50));
        assert!(mirror.take_changed());
        assert!(!mirror.take_changed());
        let kept = mirror.lasting();
        assert_eq!(
            kept,
            [(
                MessageKey::new(owner(), 5),
                MessageKey::new(Chat::Group, 50)
            )]
        );
        // After a restart: linked again, and still kept.
        let mut again = Mirror::default();
        for (primary, twin) in kept {
            again.link_lasting(primary, twin);
        }
        assert_eq!(
            again.write(MessageKey::new(owner(), 5), &edit(owner(), 5, "done")),
            [Write::Now(MessageKey::new(Chat::Group, 50))]
        );
        assert_eq!(again.lasting().len(), 1);
        assert!(!again.take_changed(), "loading changes nothing to save");
        for n in 0..(MAX_LASTING as i64 + 1) {
            again.link_lasting(
                MessageKey::new(owner(), 100 + n),
                MessageKey::new(Chat::Group, 1000 + n),
            );
        }
        assert_eq!(again.lasting().len(), MAX_LASTING);
        assert_eq!(
            again.lasting().last(),
            Some(&(
                MessageKey::new(owner(), 100 + MAX_LASTING as i64),
                MessageKey::new(Chat::Group, 1000 + MAX_LASTING as i64)
            ))
        );
    }

    #[test]
    fn the_oldest_twins_are_forgotten_and_sends_are_bounded() {
        let mut mirror = Mirror::default();
        for n in 0..(MAX_TWINS as i64 + 1) {
            mirror.link(
                MessageKey::new(owner(), n),
                MessageKey::new(Chat::Group, 10_000 + n),
            );
        }
        assert_eq!(
            mirror.primary_of(MessageKey::new(Chat::Group, 10_000)),
            None
        );
        assert!(
            mirror
                .write(MessageKey::new(owner(), 0), &edit(owner(), 0, "x"))
                .is_empty()
        );
        assert_eq!(
            mirror.twin(MessageKey::new(owner(), 1), Chat::Group),
            Some(MessageKey::new(Chat::Group, 10_001))
        );
        let mut mirror = Mirror::default();
        for seq in 0..MAX_SENDS as u64 {
            assert!(mirror.send(seq, Chat::Group, false, false).is_some());
        }
        assert!(mirror.send(9999, Chat::Group, false, false).is_none());
    }
}
