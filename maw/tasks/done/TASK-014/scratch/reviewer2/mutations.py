# Applies one mutation at a time to reviewer2/ws, runs the cctg lib tests,
# restores the file. Every mutation must be KILLED. Output: mutations.out.txt.
import os, subprocess
here = os.path.dirname(os.path.abspath(__file__))
ws = os.path.join(here, 'ws')
SLOTS = 'crates/cctg/src/hub/slots.rs'
BOOK = 'crates/cctg/src/hub/permissions.rs'
AGENT = 'crates/cctg/src/agent.rs'
env = dict(os.environ, CARGO_TARGET_DIR=os.path.join(os.environ['TEMP'], 'cctg-t014-rev2-target'),
           CARGO_PROFILE_DEV_DEBUG='0')
mutations = [
    # carried over from the planner (M3 is equivalent now, see PLAN_FINAL)
    ('M1 a decided prompt is selected again (second verdict)', SLOTS, [(
        '        match prompt.state {\n            State::Closed => expired,',
        '        match if prompt.state == State::Closed { prompt.state } else { State::Open } {\n            State::Closed => expired,')]),
    ('M2 prompt not on the permission lane', SLOTS, [(
        '                reply_markup: Some(permissions::keyboard(&prompt.request_id)),\n                permission: true,',
        '                reply_markup: Some(permissions::keyboard(&prompt.request_id)),\n                permission: false,')]),
    ('M4 buttons kept by the final edit', SLOTS, [(
        '                    message_id,\n                    text,\n                    reply_markup: Some(permissions::no_keyboard()),',
        '                    message_id,\n                    text,\n                    reply_markup: None,')]),
    ('M5 prompt found by request id, not by message', SLOTS, [(
        'let Some(key) = self.prompts.by_message(message_id) else {',
        'let Some(key) = self.prompts.active().first().copied().filter(|_| message_id > 0) else {')]),
    ('M6 no fallback to the reconnected process', SLOTS, [(
        'let pid = prompt.claude_pid?;', 'let pid = prompt.claude_pid.filter(|_| false)?;')]),
    # review 2
    ('M7 SessionEnd does not close prompts', SLOTS, [(
        '        self.close_ended_prompts();\n', '')]),
    ('M8 no re-send when the link comes back', SLOTS, [(
        '                self.push_selected(Some(&session));\n', '')]),
    ('M9 pid fallback not scoped to the session', SLOTS, [(
        'let serves = |bound: &Conn| bound.session == prompt.session;',
        'let serves = |_: &Conn| true;')]),
    ('M10 a finished prompt clears waiting for the whole session', SLOTS, [(
        '        if self.prompts.finish(key, state) {\n            self.sync_waiting(&session);',
        '        if self.prompts.finish(key, state) {\n            self.registry.set_waiting(&session, false);')]),
    ('M11 a failed prompt send keeps the waiting icon', SLOTS, [(
        '                if let Some(gone) = self.prompts.remove(key) {\n                    self.sync_waiting(&gone.session);',
        '                if let Some(gone) = self.prompts.remove(key) {\n                    let _ = gone;')]),
    ('M12 a failed final edit is not retried', SLOTS, [(
        '            self.prompts.retry_failed_edits();\n', '')]),
    ('M13 the full book evicts an open prompt silently', BOOK, [(
        'expired = self.remove(key).filter(|gone| gone.state.is_active());',
        'self.remove(key);')]),
    ('M14 the full book evicts a selected prompt', BOOK, [(
        'at(Prompt::finished).or_else(|| at(Prompt::expirable))',
        'at(Prompt::finished).or_else(|| at(|prompt| prompt.state.is_active()))')]),
    ('M15 a later press changes a selected answer', SLOTS, [
        ('            State::Selected { .. } | State::Decided(_) => {', '            State::Decided(_) => {'),
        ('            State::Open => {\n                prompt.state = State::Selected {',
         '            State::Open | State::Selected { .. } => {\n                prompt.state = State::Selected {')]),
    ('M16 decided on hand-off even for an acking agent', SLOTS, [(
        '        if !bound.acks {\n', '        if true {\n')]),
    ('M17 an ack from another session decides', SLOTS, [(
        '.is_none_or(|bound| bound.session != prompt.session)', '.is_none_or(|_| false)')]),
    ('M18 an old agent gets the verdict id', SLOTS, [(
        'verdict_id: bound.acks.then_some(verdict_id),', 'verdict_id: Some(verdict_id),')]),
    ('M19 a turn boundary does not quiet the icon', SLOTS, [(
        '            self.prompts.quiet(session);\n', '')]),
    ('M20 the agent passes a repeated verdict on again', AGENT, [(
        'let repeated = ack.is_some_and(|id| verdicts.contains(&id));',
        'let repeated = ack.is_some_and(|id| verdicts.contains(&id)) && false;')]),
]
out = []
for name, rel, edits in mutations:
    path = os.path.join(ws, rel)
    orig = open(path, encoding='utf-8').read()
    mutated = orig
    for old, new in edits:
        assert mutated.count(old) == 1, (name, old)
        mutated = mutated.replace(old, new)
    open(path, 'w', encoding='utf-8', newline='\n').write(mutated)
    try:
        r = subprocess.run(['cargo', 'test', '-j', '1', '-p', 'cctg', '--lib'],
                           cwd=ws, env=env, capture_output=True, text=True, encoding='utf-8', errors='replace')
    finally:
        open(path, 'w', encoding='utf-8', newline='\n').write(orig)
    failed = [l[5:-10] for l in r.stdout.splitlines() if l.endswith('FAILED') and l.startswith('test ')]
    if r.returncode == 0:
        verdict = 'SURVIVED'
    elif failed:
        verdict = 'KILLED'
    else:
        verdict = 'BUILD-FAILED'
    out.append('%s: %s %s' % (name, verdict, failed))
    print(out[-1], flush=True)
open(os.path.join(here, 'mutations.out.txt'), 'w', encoding='utf-8', newline='\n').write('\n'.join(out) + '\n')
