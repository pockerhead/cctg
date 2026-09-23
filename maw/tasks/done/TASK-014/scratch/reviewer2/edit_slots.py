import os
here = os.path.dirname(os.path.abspath(__file__))
p = os.path.join(here, 'ws/crates/cctg/src/hub/slots.rs')
lines = open(p, encoding='utf-8').read().split('\n')
# 1-based 621..787 is the planner's prompt block, 787 the blank line after it
assert lines[620].startswith('    /// Remembers a relayed permission request'), lines[620]
assert lines[786] == '' and lines[787].startswith('    /// Sends `notice`'), (lines[786], lines[787])
block = open(os.path.join(here, 'prompts_block.rs'), encoding='utf-8').read().rstrip('\n').split('\n')
lines = lines[:620] + block + [''] + lines[787:]
s = '\n'.join(lines)


def rep(old, new, count=1):
    global s
    assert s.count(old) == count, (old, s.count(old))
    s = s.replace(old, new)


rep('''//! Permission requests become prompts with Allow/Deny buttons in the topic of
//! the requesting session's own slot (see [`permissions`]). They bypass the
//! message cap and ride the scheduler's permission lane; the first press on a
//! prompt forwards one verdict to the agent, later presses only get "already
//! decided".''', '''//! Permission requests become prompts with Allow/Deny buttons in the topic of
//! the requesting session's own slot (see [`permissions`]). They bypass the
//! message cap and ride the scheduler's permission lane. The first press
//! fixes the answer; the verdict goes only to an agent of the requesting
//! session, and again after a link drop until that agent acknowledges it.
//! Later presses only get "already decided". The end of the session closes
//! its open prompts; every ended prompt loses its buttons, retried on the
//! tick.''')
rep('use super::permissions::{self, Prompt, Prompts};',
    'use super::permissions::{self, Edit, Opened, Prompt, Prompts, State};')
rep('''    /// A button answer or a decision edit.
    Callback(Option<Delivery>),''', '''    /// The final edit of a prompt, by its key.
    PromptEdit {
        key: u64,
        delivery: Option<Delivery>,
    },
    /// A button answer or the edit of an expired prompt.
    Callback(Option<Delivery>),''')
rep('''    Permission(u64),
    Callback,
}''', '''    Permission(u64),
    PromptEdit(u64),
    Callback,
}''')
rep('''    claude_pid: Option<u32>,
    to_agent: mpsc::Sender<HubMsg>,
}''', '''    claude_pid: Option<u32>,
    to_agent: mpsc::Sender<HubMsg>,
    /// It acknowledges permission verdicts ([`crate::wire::Register::verdict_ack`]).
    acks: bool,
}''')
rep('''                self.conns.insert(
                    conn,
                    Conn {
                        session,
                        host: register.host,
                        claude_pid: register.claude_pid,
                        to_agent,
                    },
                );
            }''', '''                self.conns.insert(
                    conn,
                    Conn {
                        session: session.clone(),
                        host: register.host,
                        claude_pid: register.claude_pid,
                        to_agent,
                        acks: register.verdict_ack,
                    },
                );
                // A link that came back takes the answers that wait for it.
                self.push_selected(Some(&session));
                self.sync_waiting(&session);
            }''')
rep('''                    AgentMsg::Reply { text } => self.on_reply(conn, &text),
                    _ => debug!(conn, "agent message not routed yet"),''', '''                    AgentMsg::PermissionAck { verdict_id } => self.on_verdict_ack(conn, verdict_id),
                    AgentMsg::Reply { text } => self.on_reply(conn, &text),
                    _ => debug!(conn, "agent message not routed yet"),''')
rep('''        if let Some((session, path)) = followup.read_title {
            self.read_title(session, path);
        }
    }''', '''        if matches!(
            post.event,
            HookEvent::Stop { .. } | HookEvent::UserPromptSubmit { .. }
        ) {
            self.prompts.quiet(session);
        }
        self.close_ended_prompts();
        if let Some((session, path)) = followup.read_title {
            self.read_title(session, path);
        }
    }''')
rep('''        if now >= self.next_retry {
            self.registry.retry_failed();
            self.next_retry = now + self.options.retry_every;
        }''', '''        if now >= self.next_retry {
            self.registry.retry_failed();
            self.prompts.retry_failed_edits();
            self.push_selected(None);
            self.next_retry = now + self.options.retry_every;
        }''')
rep('''            Done::Permission { key, delivery } => match delivery {
                Some(Ok(Outcome::Sent(message))) if message.message_id != 0 => {
                    self.prompts.delivered(key, message.message_id);
                }
                Some(Ok(_)) => {
                    warn!("permission prompt sent without a message id; its buttons cannot work");
                    self.prompts.remove(key);
                }
                Some(Err(error)) => {
                    warn!(%error, "permission prompt not delivered; it can be answered in the terminal");
                    self.prompts.remove(key);
                }
                None => {
                    warn!("permission prompt got no answer");
                    self.prompts.remove(key);
                }
            },
            Done::Callback(delivery) => {
                if let Some(Err(error)) = delivery {
                    debug!(%error, "button answer or decision edit failed");
                }
            }''', '''            Done::Permission { key, delivery } => {
                match delivery {
                    Some(Ok(Outcome::Sent(message))) if message.message_id != 0 => {
                        self.prompts.delivered(key, message.message_id);
                        return;
                    }
                    Some(Ok(_)) => {
                        warn!("permission prompt sent without a message id; its buttons cannot work");
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
            Done::Callback(delivery) => {
                if let Some(Err(error)) = delivery {
                    debug!(%error, "button answer or expired prompt edit failed");
                }
            }''')
rep('''    fn on_topic_done(&mut self, job: TopicJob, delivery: Option<Delivery>) {''', '''    fn on_prompt_edit_done(&mut self, key: u64, delivery: Option<Delivery>) {
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

    fn on_topic_done(&mut self, job: TopicJob, delivery: Option<Delivery>) {''')
rep('''        self.send_prompts();
        let view = self.registry.topic_view();''', '''        self.send_prompts();
        self.send_prompt_edits();
        let view = self.registry.topic_view();''')
rep('''                Work::Permission(key) => Done::Permission { key, delivery },
                Work::Callback => Done::Callback(delivery),''', '''                Work::Permission(key) => Done::Permission { key, delivery },
                Work::PromptEdit(key) => Done::PromptEdit { key, delivery },
                Work::Callback => Done::Callback(delivery),''')
open(p, 'w', encoding='utf-8', newline='\n').write(s)
print('ok')
