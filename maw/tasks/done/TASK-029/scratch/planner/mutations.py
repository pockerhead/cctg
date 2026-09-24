# Runs each mutation of the TASK-029 logic against the tests that should
# catch it, in a workspace with task029.patch applied, and restores the file.
# Usage: python mutations.py <workspace root>
# Expects CARGO_TARGET_DIR and CARGO_PROFILE_DEV_DEBUG=0 in the environment.
import io, os, subprocess, sys

ws = sys.argv[1]
S = 'crates/cctg/src/hub/slots.rs'
T = 'crates/cctg/src/hub/status.rs'
I = 'crates/cctg/src/hub/ingress.rs'
K = 'crates/cctg/src/hook.rs'
LIB = ['cargo', 'test', '-j', '1', '--offline', '-p', 'cctg', '--lib']
E2E = ['cargo', 'test', '-j', '1', '--offline', '-p', 'cctg', '--test', 'status_e2e']
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
 ('M5 an interrupt that went in does not end the turn', S,
  '            if ask.key == ConsoleKey::Interrupt\n                && let Some(activity) = self.activity.get_mut(&ask.session)\n            {\n                activity.stop();\n            }\n',
  '            let _ = ConsoleKey::Interrupt;\n', E2E),
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
  '                        | AgentMsg::TranscriptChunk { .. }\n                        | AgentMsg::ConsoleKeyDone { .. }),',
  '                        | AgentMsg::TranscriptChunk { .. }),', E2E),
 ('M13 the background button shows while it is switched off', S,
  'background: self.options.background_button\n                && keys', 'background: keys', E2E),
 ('M12 a running call outranks a waiting permission prompt', T,
  '    if waiting {\n        return Phase::Waiting;', '    if false && waiting {\n        return Phase::Waiting;', LIB),
]
out = []
for name, rel, old, new, cmd in M:
    path = os.path.join(ws, rel)
    raw = open(path, 'rb').read()
    crlf = b'\r\n' in raw
    text = raw.decode('utf-8')
    o, n = (old.replace('\n', '\r\n'), new.replace('\n', '\r\n')) if crlf else (old, new)
    if text.count(o) != 1:
        out.append('%s: PATTERN NOT FOUND ONCE (%d)' % (name, text.count(o)))
        continue
    open(path, 'wb').write(text.replace(o, n).encode('utf-8'))
    try:
        r = subprocess.run(cmd, cwd=ws, capture_output=True, text=True, encoding='utf-8', errors='replace', timeout=1200)
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
