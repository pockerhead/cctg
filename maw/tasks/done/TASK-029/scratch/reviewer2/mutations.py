# Runs each mutation of the TASK-029 logic against the tests that should
# catch it, in a workspace with the reviewer2 task029.patch applied, and
# restores the file. M1-M12 are the planner's (M5 and M11 adapted to
# ConsoleKeyWritten, M13 removed with Ctrl+B); R1-R10 guard the plan-review-2
# fixes (each one reverts one fix to the planner's behaviour).
# Usage: python mutations.py <workspace root>
# Expects CARGO_TARGET_DIR and CARGO_PROFILE_DEV_DEBUG=0 in the environment.
import os, subprocess, sys

ws = sys.argv[1]
S = 'crates/cctg/src/hub/slots.rs'
T = 'crates/cctg/src/hub/status.rs'
I = 'crates/cctg/src/hub/ingress.rs'
K = 'crates/cctg/src/hook.rs'
H = 'crates/cctg/src/hub/mod.rs'
R = 'crates/cctg/src/hub/registry.rs'
L = 'crates/cctg/src/statusline.rs'
C = ['cargo', 'test', '-j', '1', '--offline', '-p', 'cctg']
LIB = C + ['--lib']
E2E = C + ['--test', 'status_e2e']
CLI = C + ['--test', 'statusline_cli']
M = [
 ('M1 the first stop press already sends Esc', S,
  'Press::Confirm if armed => {', 'Press::Confirm | Press::Stop => {', E2E),
 ('M2 a status press is not tied to its own message', S,
  '                slot.status\n                    .is_some_and(|status| status.message_id == message_id)',
  '                slot.status.is_some()', E2E),
 ('M3 an agent without console_keys is asked to press keys', S,
  'if !self.conns.get(&conn).is_some_and(|bound| bound.keys) {', 'if false {', LIB),
 ('M4 status edits are not paced', S,
  '            if matches!(job, StatusJob::Edit { .. }) {\n                shown.next_at = Some(now + every);\n            }\n', '', E2E),
 ('M5 a written Esc is not shown', S,
  '                activity.interrupt_written();', '                let _ = activity;', E2E),
 ('M6 every pin notice is deleted', S,
  'if self.options.can_delete && self.status_slot(pinned).is_some() {', 'if self.options.can_delete {', E2E),
 ('M7 a pinned status message is pinned again', S,
  '                        status.pinned = true;', '                        status.pinned = false;', E2E),
 ('M8 interrupt notes in the stream are ignored', S,
  'activity.interrupted_at(line.end);', 'let _ = line.end;', LIB),
 ('M9 tool calls inside subagents reach the hub', K,
  'if input.agent_id.is_some_and(|id| !id.is_empty()) {', 'if false {', LIB),
 ('M10 a start that comes after its end shows a finished call', T,
  'if self.finished.iter().any(|done| done == id) || self.running.iter().any(|r| r.id == id)',
  'if self.running.iter().any(|r| r.id == id)', LIB),
 ('M11 console key answers are not forwarded by ingress', I,
  '                        | AgentMsg::TranscriptChunk { .. }\n                        | AgentMsg::ConsoleKeyWritten { .. }),',
  '                        | AgentMsg::TranscriptChunk { .. }),', E2E),
 ('M12 a running call outranks a waiting permission prompt', T,
  '    if waiting {\n        return Phase::Waiting;', '    if false && waiting {\n        return Phase::Waiting;', LIB),
 ('R1 a person\'s pin notice is deleted too', H,
  '        }) if from == bot_id => {', '        }) if true || from == bot_id => {', LIB),
 ('R2a a press while a permission prompt waits writes Esc', S,
  '        if waiting {\n            // Esc would answer the prompt', '        if false && waiting {\n            // Esc would answer the prompt', E2E),
 ('R2b the stop button shows while a permission prompt waits', S,
  '            interrupt: keys && !waiting && self.busy(session),', '            interrupt: keys && self.busy(session),', E2E),
 ('R3 a written Esc counts as the end of the turn', S,
  '                activity.interrupt_written();', '                activity.stop();', E2E),
 ('R4 a late key answer of a session that moved on is taken', S,
  '            || self.live_agent(ask.slot) != Some((ask.session.clone(), conn))\n', '', E2E),
 ('R5 a tool result in the stream does not end the call', S,
  'StreamItem::Result { id, .. } => activity.tool_end(id),', 'StreamItem::Result { .. } => {}', LIB),
 ('R6 status line numbers are not kept in registry.json', R,
  '    #[serde(default, skip_serializing_if = "Option::is_none")]\n    pub metrics: Option<Metrics>,',
  '    #[serde(skip)]\n    pub metrics: Option<Metrics>,', E2E),
 ('R7 percentages up to 1000 are shown', L,
  '    (0.0..=100.0)', '    (0.0..=1000.0)', LIB),
 ('R8 the status line waits 150 ms for a stopped hub', L,
  'Duration::from_millis(80);', 'Duration::from_millis(150);', LIB),
 ('R9 an empty output of the user command becomes cctg\'s line', L,
  '        Some(command) => run_chained(&command, &input).await,',
  '        Some(command) => run_chained(&command, &input).await.filter(|(out, _)| !out.is_empty()),', CLI),
 ('R10 the user command exit code is dropped', L,
  '            .unwrap_or(1);\n        (output, code)', '            .unwrap_or(1);\n        (output, code * 0)', CLI),
]
only = set(sys.argv[2:])
out = []
for name, rel, old, new, cmd in M:
    if only and name.split()[0] not in only:
        continue
    path = os.path.join(ws, rel)
    raw = open(path, 'rb').read()
    crlf = b'\r\n' in raw
    text = raw.decode('utf-8')
    o, n = (old.replace('\n', '\r\n'), new.replace('\n', '\r\n')) if crlf else (old, new)
    if text.count(o) != 1:
        out.append('%s: PATTERN NOT FOUND ONCE (%d)' % (name, text.count(o)))
        print(out[-1], flush=True)
        continue
    open(path, 'wb').write(text.replace(o, n).encode('utf-8'))
    try:
        r = subprocess.run(cmd, cwd=ws, capture_output=True, text=True, encoding='utf-8', errors='replace', timeout=1800)
        verdict = 'KILLED' if r.returncode != 0 else 'SURVIVED'
        failed = [l.split()[1] for l in r.stdout.splitlines() if l.startswith('test ') and l.endswith('FAILED')]
        if r.returncode != 0 and not failed:
            failed = ['(build or other failure)']
        out.append('%s: %s %s' % (name, verdict, ', '.join(failed[:4])))
    finally:
        open(path, 'wb').write(raw)
    print(out[-1], flush=True)
txt = '\n'.join(out) + '\n'
open(os.path.join(os.path.dirname(os.path.abspath(__file__)), 'mutations.out.txt'), 'w', encoding='utf-8', newline='\n').write(txt)
