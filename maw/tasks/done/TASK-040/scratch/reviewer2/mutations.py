# Runs each mutation of the TASK-040 logic against the tests that should
# catch it, in a workspace with task040.patch applied, and restores the file.
# Usage: python mutations.py <workspace root>
# Expects CARGO_TARGET_DIR and CARGO_PROFILE_DEV_DEBUG=0 in the environment.
import os, subprocess, sys

ws = sys.argv[1]
S = 'crates/cctg/src/hub/slots.rs'
I = 'crates/cctg/src/hub/ingress.rs'
K = 'crates/cctg/src/keys.rs'
H = 'crates/cctg/src/shim.rs'
A = 'crates/cctg/src/agent.rs'
R = 'crates/cctg/src/run.rs'
U = 'crates/cctg/src/update.rs'
D = 'crates/cctg/src/deploy.rs'
BASE = ['cargo', 'test', '-j', '1', '--offline', '-p', 'cctg']
LIB = BASE + ['--lib']
UPD = BASE + ['--test', 'update_e2e']
SUP = BASE + ['--test', 'supervise_e2e']
RUN = BASE + ['--test', 'run_e2e']
M = [
 ('M1 the spaces around /exit (U+00A0 after the glyph) are not trimmed', K,
  '[line] => line.strip_prefix(PROMPT).map(str::trim) == Some("/exit"),',
  '[line] => line.strip_prefix(PROMPT) == Some("/exit"),', LIB),
 ('M2 the shim forwards the switch marker to Claude Code', H,
  'Ok(_) if line == SWITCH => {', 'Ok(_) if line == b"never".to_vec() => {', UPD),
 ('M3 lines that came during the switch are dropped', H,
  '        while let Some(line) = waiting.pop_front() {',
  '        waiting.clear();\n        while let Some(line) = waiting.pop_front() {', UPD),
 ('M4 a resumed worker waits for a second initialize', A,
  'server = server.initialized();', 'let _ = &mut server;', UPD),
 ('M5 an update goes out during a running turn', S,
  'if ask.sent.is_some() || self.busy(&session) {', 'if ask.sent.is_some() {', LIB),
 ('M6 a leaving agent stays bound', S,
  '            self.registry.agent_disconnected(session, conn);\n            return;',
  '            return;', LIB),
 ('M7 the outdated warning repeats', S,
  'if warned || !self.outdated(conn) {', 'if !self.outdated(conn) {', LIB),
 ('M8 a restart keeps --continue', U,
  '"-c" | "--continue" | "--fork-session" => Values::None,', '"--fork-session" => Values::None,', LIB),
 ('M9 changed settings do not ask for a restart', U,
  'match (changed, self.restartable()) {', 'match (false, self.restartable()) {', LIB),
 ('M10 ingress drops update answers', I,
  '                        | AgentMsg::ConsoleKeyWritten { .. }\n                        | AgentMsg::UpdateAnswer { .. }),',
  '                        | AgentMsg::ConsoleKeyWritten { .. }),', UPD),
 ('M11 a hub that exits in its trial is not rolled back', D,
  'if stamp(files) == first {', 'if true {', SUP),
 ('M12 a prompt argument is sent again on a restart', U,
  '            Values::None => {}', '            Values::None => kept.push(arg.clone()),', LIB),
 ('M13 an inherited CCTG_RUN of another session counts', U,
  'if matches!(name.trim_end_matches(".exe"), "claude" | "node") {', 'if false {', LIB),
 ('M14 the outdated warning is silent', S,
  '// Loud (decision 2026-09-24): the user asked to be told.\n                notify: true,',
  '// Loud (decision 2026-09-24): the user asked to be told.\n                notify: false,', LIB),
 ('M15 cctg run ignores the arguments of the request', R,
  '        claude_args = next;', '        let _ = next;', RUN),
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
        print(out[-1], flush=True)
        continue
    open(path, 'wb').write(text.replace(o, n).encode('utf-8'))
    try:
        r = subprocess.run(cmd, cwd=ws, capture_output=True, text=True, encoding='utf-8', errors='replace', timeout=1200)
        verdict = 'KILLED' if r.returncode != 0 else 'SURVIVED'
        failed = [l.split()[1] for l in r.stdout.splitlines() if l.startswith('test ') and l.endswith('FAILED')]
        if r.returncode != 0 and not failed:
            failed = ['(harness=false test or build failure)']
        out.append('%s: %s %s' % (name, verdict, ', '.join(failed[:4])))
    finally:
        open(path, 'wb').write(raw)
    print(out[-1], flush=True)
txt = '\n'.join(out) + '\n'
open(os.path.join(os.path.dirname(os.path.abspath(__file__)), 'mutations.out.txt'), 'w', encoding='utf-8', newline='\n').write(txt)
