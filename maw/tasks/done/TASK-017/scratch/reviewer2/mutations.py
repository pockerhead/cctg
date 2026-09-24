# TASK-017 reviewer2: planner M1-M12 plus M13-M14. Runs each mutation of the TASK-017 logic against `cargo test -p cctg --lib hub::`
# in ws/ and restores the file. Expects CARGO_TARGET_DIR etc. in the env.
import io, os, subprocess
here = os.path.dirname(os.path.abspath(__file__))
ws = os.path.join(here, 'ws')
S = 'crates/cctg/src/hub/slots.rs'
R = 'crates/cctg/src/hub/registry.rs'
B = 'crates/cctg/src/hub/buffer.rs'
M = [
 ('M13 an agent of an earlier run takes kept messages', R,
  '                // `/clear` the agent follows its pid (`Slots::follow_pid`).\n                entry.agent = None;\n', '                // `/clear` the agent follows its pid (`Slots::follow_pid`).\n'),
 ('M14 a message is kept in the wrong slot', S,
  '        self.park(\n            slot,\n', '        self.park(\n            SlotId(0),\n'),
 ('M1 a handed message stays in the buffer (duplicates)', S,
  '                entry.buffer.messages.pop_front();\n', ''),
 ('M2 a full buffer keeps growing', B,
  '        if dropped {\n            self.messages.pop_front();\n        }\n', ''),
 ('M3 overflow told on every drop', S,
  'let tell_overflow = dropped && !entry.buffer.overflow_told;', 'let tell_overflow = dropped;'),
 ('M4 Resume message on every pump', S,
  '                || entry.buffer.resume.is_some()\n', ''),
 ('M5 an ended session\'s agent takes messages', S,
  '        if !self.registry.is_live_top_level(session) {\n            return None;\n        }\n        let conn = self.registry.sessions.get(session)?.agent?;',
  '        let conn = self.registry.sessions.get(session)?.agent?;'),
 ('M6 no revival flush in pump', S, '        self.flush_all();\n', ''),
 ('M7 buffer not persisted', R,
  '    #[serde(default, skip_serializing_if = "Buffer::is_idle")]\n    pub buffer: Buffer,',
  '    #[serde(skip)]\n    pub buffer: Buffer,'),
 ('M8 a Resume press records nothing', S,
  '            entry.buffer.resume_asked = true;\n', ''),
 ('M9 the Resume button stays after revival', S,
  'if let Some(message_id) = note.and_then(|note| note.message_id) {',
  'if let Some(message_id) = note.and(None::<i64>) {'),
 ('M10 queued notice on every message', S,
  'let tell_queued = offline && !dead && !entry.buffer.queued_told;', 'let tell_queued = offline && !dead;'),
 ('M11 the offline period never closes', S,
  '        let note = entry.buffer.close();\n', '        let note = entry.buffer.resume.clone();\n        entry.buffer.messages.clear();\n'),
 ('M12 Resume data parsed as unknown', B,
  "    (action == RESUME && is_token(session)).then_some(session)", "    (action == RESUME && is_token(session) && false).then_some(session)"),
]
out = []
for name, f, old, new in M:
    p = os.path.join(ws, f)
    src = io.open(p, encoding='utf-8', newline='').read()
    assert src.count(old) == 1, name
    io.open(p, 'w', encoding='utf-8', newline='\n').write(src.replace(old, new))
    try:
        r = subprocess.run(['cargo', 'test', '-j', '1', '--offline', '-p', 'cctg', '--lib', 'hub::'],
                           cwd=ws, capture_output=True, text=True, encoding='utf-8', errors='replace', timeout=900)
        text = r.stdout + r.stderr
        failed = [l.strip() for l in text.splitlines() if l.strip().endswith('FAILED') and l.startswith('test ')]
        verdict = 'KILLED' if r.returncode != 0 else 'SURVIVED'
        if r.returncode != 0 and not failed:
            failed = ['(did not compile)'] if 'error[' in text else ['(nonzero exit)']
        out.append('%s: %s %s' % (name, verdict, failed))
    except subprocess.TimeoutExpired:
        out.append('%s: KILLED [(timeout)]' % name)
    finally:
        io.open(p, 'w', encoding='utf-8', newline='\n').write(src)
    print(out[-1], flush=True)
io.open(os.path.join(here, 'mutations.out.txt'), 'w', encoding='utf-8', newline='\n').write('\n'.join(out) + '\n')
