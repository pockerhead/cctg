import os
p = os.path.join(os.path.dirname(os.path.abspath(__file__)), 'ws/crates/cctg/src/hub/scheduler.rs')
s = open(p, encoding='utf-8').read()


def rep(a, b):
    global s
    assert s.count(a) == 1, a[:80]
    s = s.replace(a, b)


rep("""//! 1. a permission prompt from `Message` that has no older message of its topic
//!    queued; metered.
//! 2. `Edit` - `editMessageText` (coalesced per message) and `answerCallbackQuery`.
//! 3. `Topic` - `createForumTopic`, `editForumTopic`, `deleteMessage`.
//! 4. `Message` - `sendMessage`, `sendDocument`; metered, one FIFO. Permission
//!    prompts live here too, so they never overtake their own topic.
""", """//! 1. a permission prompt from `Message` that has no older message of its topic
//!    queued (stream lines do not count); metered.
//! 2. `Edit` - `editMessageText` and `setMessageReaction` (both coalesced per
//!    message) and `answerCallbackQuery`.
//! 3. `Topic` - `createForumTopic`, `editForumTopic`, `deleteMessage`.
//! 4. `Message` - `sendMessage`, `sendDocument` and transcript stream lines;
//!    metered, one FIFO. Permission prompts live here too, so they never
//!    overtake their own topic's ordinary messages; they do overtake its
//!    stream lines.
//!
//! Stream lines marked `merge` (one tool call each) go out one per message
//! while the group budget has room. When more messages wait than there are
//! tokens, the head line takes the lines of its topic queued right after it
//! (up to the first other message of that topic) into one message, in order,
//! while it fits Telegram's limit.
""")
rep("""    EditTopic {
        thread_id: i64,
        name: Option<String>,
        icon_custom_emoji_id: Option<String>,
    },
}
""", """    EditTopic {
        thread_id: i64,
        name: Option<String>,
        icon_custom_emoji_id: Option<String>,
    },
    /// A message of the live transcript stream (TASK-016). `merge`: a one-line
    /// tool call that may share a message with the lines queued after it.
    Stream {
        thread_id: i64,
        text: String,
        merge: bool,
    },
    /// `setMessageReaction` with one emoji; a newer one for the same message
    /// replaces a queued one.
    React { message_id: i64, emoji: String },
}
""")
rep("""            Op::Send { .. } | Op::SendDocument { .. } => Lane::Message(0),
            Op::Edit { .. } | Op::AnswerCallback { .. } => Lane::Edit,""", """            Op::Send { .. } | Op::SendDocument { .. } | Op::Stream { .. } => Lane::Message(0),
            Op::Edit { .. } | Op::AnswerCallback { .. } | Op::React { .. } => Lane::Edit,""")
rep("""        matches!(self, Op::Send { .. } | Op::SendDocument { .. })
    }
}""", """        matches!(
            self,
            Op::Send { .. } | Op::SendDocument { .. } | Op::Stream { .. }
        )
    }

    /// The topic of a new message.
    fn thread(&self) -> Option<Option<i64>> {
        match self {
            Op::Send { thread_id, .. } | Op::SendDocument { thread_id, .. } => Some(*thread_id),
            Op::Stream { thread_id, .. } => Some(Some(*thread_id)),
            _ => None,
        }
    }
}""")
rep("""    /// A newer edit of the same message replaced this one before it was sent.
    Superseded,
}""", """    /// A newer edit of the same message replaced this one before it was sent.
    Superseded,
    /// This stream line went out inside the message of an earlier line of its
    /// topic, which got the actual answer.
    Merged,
}""")
rep("""            } => self
                .edit_forum_topic(*thread_id, name.as_deref(), icon_custom_emoji_id.as_deref())
                .await
                .map(|()| Outcome::Done),
        }""", """            } => self
                .edit_forum_topic(*thread_id, name.as_deref(), icon_custom_emoji_id.as_deref())
                .await
                .map(|()| Outcome::Done),
            Op::Stream {
                thread_id, text, ..
            } => self
                .send_message(Some(*thread_id), text, None)
                .await
                .map(Outcome::Sent),
            Op::React { message_id, emoji } => self
                .set_message_reaction(*message_id, emoji)
                .await
                .map(|()| Outcome::Done),
        }""")
rep("""struct Job {
    op: Op,
    reply: oneshot::Sender<Delivery>,
}""", """struct Job {
    op: Op,
    reply: oneshot::Sender<Delivery>,
    /// Stream lines sent inside this job's message.
    merged: Vec<oneshot::Sender<Delivery>>,
}""")
rep("""        let _ = self.tx.send(Job { op, reply }).await;""", """        let _ = self
            .tx
            .send(Job {
                op,
                reply,
                merged: Vec::new(),
            })
            .await;""")
rep("""                Op::SendDocument { thread_id, .. } => (*thread_id, false),
                _ => continue,
            };""", """                Op::SendDocument { thread_id, .. } => (*thread_id, false),
                // Stream lines yield to a prompt of their own topic.
                _ => continue,
            };""")
rep("""    fn enqueue(&mut self, job: Job) {
        if let Op::Edit {""", """    fn enqueue(&mut self, job: Job) {
        if let Op::React { message_id, emoji } = &job.op
            && let Some(queued) = self.edit.iter_mut().find(
                |queued| matches!(queued.op, Op::React { message_id: id, .. } if id == *message_id),
            )
        {
            if let Op::React {
                emoji: queued_emoji,
                ..
            } = &mut queued.op
            {
                queued_emoji.clone_from(emoji);
            }
            let superseded = std::mem::replace(&mut queued.reply, job.reply);
            let _ = superseded.send(Ok(Outcome::Superseded));
            return;
        }
        if let Op::Edit {""")
rep("""        let Some(job) = self.lane_mut(lane).remove(index) else {
            return;
        };
        if job.op.metered() {""", """        let Some(mut job) = self.lane_mut(lane).remove(index) else {
            return;
        };
        if matches!(lane, Lane::Message(_)) {
            self.merge_lines(&mut job, Instant::now());
        }
        if job.op.metered() {""")
rep("""            result => {
                let _ = job.reply.send(result);
            }
        }
    }
}""", """            result => {
                let _ = job.reply.send(result);
                for merged in job.merged {
                    let _ = merged.send(Ok(Outcome::Merged));
                }
            }
        }
    }

    /// Joins the stream lines of `job`'s topic queued right after it into
    /// its text when more messages wait than the bucket has tokens.
    fn merge_lines(&mut self, job: &mut Job, now: Instant) {
        let Op::Stream {
            thread_id,
            text,
            merge: true,
        } = &mut job.op
        else {
            return;
        };
        self.bucket.refill(now);
        if (self.message.len() + 1) as f64 <= self.bucket.tokens {
            return;
        }
        let mut index = 0;
        while index < self.message.len() {
            let queued = &self.message[index].op;
            if queued.thread() != Some(Some(*thread_id)) {
                index += 1;
                continue;
            }
            let Op::Stream {
                text: next,
                merge: true,
                ..
            } = queued
            else {
                break;
            };
            if transcript::telegram_len(text) + 1 + transcript::telegram_len(next)
                > transcript::TELEGRAM_TEXT_LIMIT
            {
                break;
            }
            text.push('\\n');
            text.push_str(next);
            let Some(next) = self.message.remove(index) else {
                break;
            };
            job.merged.push(next.reply);
            job.merged.extend(next.merged);
        }
    }
}""")
open(p, 'w', encoding='utf-8', newline='\n').write(s)
print('ok')
