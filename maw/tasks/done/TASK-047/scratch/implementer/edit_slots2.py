p = 'C:/Users/user/dev/cctg/crates/cctg/src/hub/slots.rs'
s = open(p, encoding='utf-8').read()
def rep(old, new):
    global s
    assert s.count(old) == 1, old[:120]
    s = s.replace(old, new)
rep('''            CommandOutcome::Draft => {
                self.answer_command(ask.thread_id, ask.message_id, console::DRAFT_NOTICE);
            }
''', '''            CommandOutcome::Draft => {
                self.answer_command(ask.thread_id, ask.message_id, console::DRAFT_NOTICE);
            }
            CommandOutcome::AgentsRunning => {
                self.answer_command(ask.thread_id, ask.message_id, console::AGENTS_NOTICE);
            }
''')
rep('''//! a turn; the press is asked again after it.
''', '''//! a turn; the press is asked again after it. Likewise no `/exit` goes in
//! while the terminal shows background agents or the agent view (TASK-047):
//! the agent answers `agents_running`, the topic is told once and the press
//! is asked again every [`UPDATE_RETRY`]. A restart that cuts off work (a
//! turn stopped by ⏹ while the press waited, or still running) is followed
//! by one channel message into the session once its next agent is bound.
''')
open(p, 'w', encoding='utf-8', newline='').write(s)
print('ok')
