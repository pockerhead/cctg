# QA round 2 copy of scratch/qa/mutations_repo_crlf.py (reviewer2 set adapted to the repo), path fixed for round2/.
# Runs each mutation of the TASK-016 logic (planner's set, adapted to the
# reviewed code, plus one per review-2 fix) against the affected tests in ws/
# and restores the file. Expects CARGO_TARGET_DIR, CARGO_PROFILE_DEV_DEBUG=0.
# Usage: python mutations.py [name-prefix ...]
import io, os, subprocess, sys
here = os.path.dirname(os.path.abspath(__file__))
# QA: run against the real repo (5 levels up from scratch/qa)
ws = os.path.abspath(os.path.join(here, '..', '..', '..', '..', '..', '..', '..'))
S = 'crates/cctg/src/hub/slots.rs'
H = 'crates/cctg/src/hub/stream.rs'
C = 'crates/cctg/src/hub/scheduler.rs'
R = 'crates/cctg/src/hub/registry.rs'
T = 'crates/cctg/src/tail.rs'
X = 'crates/transcript/src/stream.rs'
CCTG = ['cargo', 'test', '-j', '1', '--offline', '-p', 'cctg', '--lib', '--test', 'stream_logs']
TRANSCRIPT = ['cargo', 'test', '-j', '1', '--offline', '-p', 'transcript', '--test', 'stream']
M = [
 # planner's mutations, adapted
 ('M1 a partial last line is read as a line', T,
  'if !line.ends_with(b"\\n") {', 'if false && !line.ends_with(b"\\n") {', CCTG),
 ('M2 the offset moves at hand-off, not at the answer', H,
  '''                Some(Entry::Message {
                    state: Answer::Accepted,
                    ..
                }) => {}''', '''                Some(Entry::Message { .. }) => {}''', CCTG),
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
  'if let Some(live) = self.streams.get_mut(session).filter(|_| streamed) {',
  'if let Some(live) = self.streams.get_mut(session).filter(|_| false) {', CCTG),
 ('M10 a held answer waits for a read that never comes', S,
  '.is_some_and(|held| target.is_none() || now >= held.until)',
  '.is_some_and(|held| target.is_none() && now >= held.until)', CCTG),
 ('M11 an agent without transcript reads is asked', S,
  '(bound.reads && bound.session == session)', '(bound.session == session)', CCTG),
 ('M12 a missing transcript is warned on every read', S,
  'if !live.missing_warned {', 'if true {', CCTG),
 ('M14 a first read starts at the file start, not its end', T,
  '        None => len,\n', '        None => 0,\n', CCTG),
 ('M15 a new transcript starts at its end', R,
  'offset: fresh.then_some(0),', 'offset: None,', CCTG),
 ('M16 no eyes on hand-off', S,
  '            self.react(input.message_id, stream::ACCEPTED);\n', '', CCTG),
 ('M17 the final answer text is streamed too', X,
  '                    Some("tool_use") if !text.is_empty() => {',
  '                    _ if !text.is_empty() => {', TRANSCRIPT),
 ('M18 a typed prompt is not streamed', X,
  '                    events.push(StreamEvent::Prompt(prompt.into_owned()));\n', '                    let _ = prompt;\n', TRANSCRIPT),
 ('M19 every read writes the registry', S,
  '            && (stream.offset != Some(offset) || stream.calls != calls)\n',
  '            && true\n', CCTG),
 # review 2: one per fix
 ('R1 results go out in result order', H,
  '    let ready = calls.iter().take_while(|call| call.done).count();\n    steps.extend(calls.drain(..ready).map(|call| finished(&call)));',
  '    let mut index = 0;\n    while index < calls.len() {\n        if calls[index].done {\n            steps.push(finished(&calls.remove(index)));\n        } else {\n            index += 1;\n        }\n    }', CCTG),
 ('R2 a refused stream message counts as delivered', S,
  'let accepted = skipped || matches!(delivery, Some(Ok(Outcome::Sent(_) | Outcome::Merged)));',
  'let accepted = skipped || delivery.is_some();', CCTG),
 ('R3 merged lines of a refused message are answered Merged', C,
  '                if accepted {\n                    for merged in job.merged {',
  '                if accepted || true {\n                    for merged in job.merged {', CCTG),
 ('R4 a refused stream is never read again', S,
  '        if live.stuck() {', '        if live.stuck() && false {', CCTG),
 ('R5 a cut transcript is read from its end, not its start (reset ignored)', T,
  '        Some(at) if at > len || !at_line_start(&mut file, at) => return empty(false, true),',
  '        Some(at) if at > len => return empty(false, true),', CCTG),
 ('R6 a reset chunk is taken as an ordinary one', S,
  '        if reset {\n            if !live.reset_warned {', '        if reset && false {\n            if !live.reset_warned {', CCTG),
 ('R7 a turn end does not let a held answer go', S,
  '                        actions.push(Action::Release);\n', '', CCTG),
 ('R8 a turn end read before its Stop is not claimed', S,
  '            if live.ends_unclaimed > 0 {', '            if live.ends_unclaimed > 200 {', CCTG),
 ('R9 the path gate trusts a lexical path', T,
  '    let inside = path.parent().and_then(Path::parent) == Some(root.as_path())\n        && path',
  '    let inside = true\n        && path', CCTG),
 ('R10 a line that does not fit is still added to the chunk', T,
  '            // Does not fit behind the lines already taken: the next read.\n            break true;',
  '            // Does not fit behind the lines already taken: the next read.\n            let _ = ();', CCTG),
 ('R11 a read that went to a gone connection waits for its timeout', S,
  '&& (now >= sent + READ_TIMEOUT || conn != Some(asked))', '&& (now >= sent + READ_TIMEOUT)', CCTG),
 ('R12 the stream ignores the shared message budget', S,
  '                || self.queued_messages >= STREAM_QUEUE\n', '', CCTG),
 ('R13 stream messages are not counted as queued messages', S,
  '                Action::Stream(number, op) => {\n                    self.queued_messages += 1;',
  '                Action::Stream(number, op) => {', CCTG),
 ('R14 any channel source counts', X,
  '    if channel_attribute(text, "source")? != SOURCE {', '    if channel_attribute(text, "source")? == "\\u{0}" {', TRANSCRIPT),
 ('R15 a BOM line is skipped', X,
  "    let line = line.trim().trim_start_matches('\\u{feff}');", '    let line = line.trim();', TRANSCRIPT),
 ('R16 no turn end marker', X,
  '                    Some(_) => answer = true,', '                    Some(_) => {}', TRANSCRIPT),
 ('R17 an unfinished call is shown as done at a turn end', H,
  '            .filter(|call| call.done)\n', '', CCTG),
 ('R18 a second turn end in one read is lost while one answer is held', S,
  'Step::TurnEnd if live.held.len() <= releases => {', 'Step::TurnEnd if live.held.is_empty() => {', CCTG),
]
only = sys.argv[1:]
out = []
for name, f, old, new, cmd in M:
    if only and not any(name.startswith(prefix) for prefix in only):
        continue
    p = os.path.join(ws, f)
    src = io.open(p, encoding='utf-8', newline='').read()
    CR, LF = chr(13), chr(10)
    if CR + LF in src:
        old = old.replace(LF, CR + LF); new = new.replace(LF, CR + LF)
    if src.count(old) != 1:
        out.append('%s: NOT APPLICABLE (pattern count %d)' % (name, src.count(old)))
        print(out[-1], flush=True)
        continue
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
name = 'mutations_r2.out.txt'
io.open(os.path.join(here, name), 'w', encoding='utf-8', newline='\n').write('\n'.join(out) + '\n')
