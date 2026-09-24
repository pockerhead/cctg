# Runs each mutation of the TASK-016 logic against the affected tests in ws/
# and restores the file. Expects CARGO_TARGET_DIR, CARGO_PROFILE_DEV_DEBUG=0.
import io, os, subprocess
here = os.path.dirname(os.path.abspath(__file__))
ws = os.path.join(here, 'ws')
S = 'crates/cctg/src/hub/slots.rs'
H = 'crates/cctg/src/hub/stream.rs'
C = 'crates/cctg/src/hub/scheduler.rs'
R = 'crates/cctg/src/hub/registry.rs'
T = 'crates/cctg/src/tail.rs'
X = 'crates/transcript/src/stream.rs'
CCTG = ['cargo', 'test', '-j', '1', '--offline', '-p', 'cctg', '--lib', '--test', 'stream_logs']
TRANSCRIPT = ['cargo', 'test', '-j', '1', '--offline', '-p', 'transcript', '--test', 'stream']
M = [
 ('M1 a partial last line is read as a line', T,
  'if !line.ends_with(b"\\n") {', 'if false && !line.ends_with(b"\\n") {', CCTG),
 ('M2 the offset moves at hand-off, not at the answer', H,
  'self.waiting.push_back((self.next, end, false));', 'self.waiting.push_back((self.next, end, true));', CCTG),
 ('M3 a result re-read after a restart is not sent again', H,
  'known.id == *id && known.result_end.is_none_or(|end| end > answered)',
  'known.id == *id && known.result_end.is_none()', CCTG),
 ('M4 any channel record turns a waiting message to working', H,
  '.position(|id| id == message_id)', '.position(|_| true)', CCTG),
 ('M5 the stream does not wait for the session separator', S,
  'if slot.topic_id.is_none() || slot.pending_separator.is_some() {', 'if slot.topic_id.is_none() {', CCTG),
 ('M6 lines merge even while the budget has room', C,
  '        if (self.message.len() + 1) as f64 <= self.bucket.tokens {\n            return;\n        }\n', '', CCTG),
 ('M7 merging reaches past an ordinary message of the topic', C,
  '            } = queued\n            else {\n                break;\n            };',
  '            } = queued\n            else {\n                index += 1;\n                continue;\n            };', CCTG),
 ('M8 a permission prompt waits behind stream lines', C,
  '                // Stream lines yield to a prompt of their own topic.\n',
  '                Op::Stream { thread_id, .. } => (Some(*thread_id), false),\n', CCTG),
 ('M9 the turn answer is not held for the stream', S,
  'if let Some(live) = self.streams.get_mut(session) {\n            // The lines of this turn',
  'if let Some(live) = None::<&mut Live> {\n            // The lines of this turn', CCTG),
 ('M10 a held answer waits for a read that never comes', S,
  '.is_some_and(|held| now >= held.until || target.is_none())',
  '.is_some_and(|held| target.is_none() && now >= held.until)', CCTG),
 ('M11 an agent without transcript reads is asked', S,
  '(bound.reads && bound.session == session)', '(bound.session == session)', CCTG),
 ('M19 every read writes the registry', S,
  'self.registry.dirty |= !lines.is_empty();', 'self.registry.dirty = true;', CCTG),
 ('M12 a missing transcript is warned on every read', S,
  'if !live.missing_warned {', 'if true {', CCTG),
 ('M13 any path is read', T,
  '    plain && named && in_projects && plain_path\n', '    let _ = (plain, named, in_projects, plain_path);\n    true\n', CCTG),
 ('M14 a first read starts at the file start, not its end', T,
  'let start = from.filter(|&at| at <= len).unwrap_or(len);', 'let start = from.filter(|&at| at <= len).unwrap_or(0);', CCTG),
 ('M15 a new transcript starts at its end', R,
  'offset: fresh.then_some(0),', 'offset: None,', CCTG),
 ('M16 no eyes on hand-off', S,
  '            self.react(input.message_id, stream::ACCEPTED);\n', '', CCTG),
 ('M17 the final answer text is streamed too', X,
  'if turn.stop_reason.as_deref() == Some("tool_use") && !text.is_empty() {', 'if !text.is_empty() {', TRANSCRIPT),
 ('M18 a typed prompt is not streamed', X,
  '                    events.push(StreamEvent::Prompt(prompt.into_owned()));\n', '                    let _ = prompt;\n', TRANSCRIPT),
]
out = []
for name, f, old, new, cmd in [x for x in M if x[0].split()[0] in ('M11', 'M19')]:
    p = os.path.join(ws, f)
    src = io.open(p, encoding='utf-8', newline='').read()
    assert src.count(old) == 1, name
    io.open(p, 'w', encoding='utf-8', newline='\n').write(src.replace(old, new))
    try:
        r = subprocess.run(cmd, cwd=ws, capture_output=True, text=True, encoding='utf-8', errors='replace')
        text = r.stdout + r.stderr
        failed = [l.strip() for l in text.splitlines() if l.strip().endswith('FAILED') and l.startswith('test ')]
        verdict = 'KILLED' if r.returncode != 0 else 'SURVIVED'
        if r.returncode != 0 and not failed:
            failed = ['(did not compile)'] if 'error[' in text else ['(nonzero exit)']
        out.append('%s: %s %s' % (name, verdict, failed))
    finally:
        io.open(p, 'w', encoding='utf-8', newline='\n').write(src)
    print(out[-1], flush=True)
io.open(os.path.join(here, 'mutations.rerun.out.txt'), 'w', encoding='utf-8', newline='\n').write('\n'.join(out) + '\n')
