"""Reviewer-2 mutation run over scratch/reviewer2/ws: planner's 12 (adapted to the
revised code) plus one per reviewer change. Each mutation must make its named
test filter fail. Output: mutations.out.txt."""
import io, os, subprocess

HERE = os.path.dirname(os.path.abspath(__file__))
WS = os.path.join(HERE, 'ws')
env = dict(os.environ, CARGO_TARGET_DIR=os.path.join(os.environ['TEMP'], 'cctg-task009-rev2-target'))
CMD = 'crates/cctg/src/hub/commands.rs'
UPD = 'crates/cctg/src/hub/updates.rs'
SES = 'crates/cctg/src/hub/sessions.rs'
OFF = 'crates/cctg/src/hub/offset.rs'
LIB = ['-p', 'cctg', '--lib']

MUTATIONS = [
    # planner's 12, adapted
    ('offset not saved', UPD, [('save_offset(store, next).await;', 'let _ = next;')], LIB + ['saved_offset_prevents']),
    ('offset saved but not loaded', UPD, [('let mut offset = store.load();', 'let mut offset = None;')], LIB + ['saved_offset_prevents']),
    ('too-long not detected', CMD, [('if description.to_ascii_lowercase().contains("too long")', 'if description.is_empty()')], LIB + ['too_long_switches']),
    ('document retried as text', CMD, [('return send_document(outbox, thread_id, reply, rest).await;',
                                        'if send_document(outbox, thread_id, reply, rest).await.is_err() { continue; } return Ok(());')], LIB + ['too_long_switches']),
    ('prefer_file ignored', CMD, [('if split.prefer_file {', 'if false && split.prefer_file {')], LIB + ['large_reply_goes']),
    ('directories accepted as sessions', SES, [('if !metadata.is_file() {', 'if metadata.is_symlink() {')], LIB + ['non_session_entries']),
    ('any file name accepted', SES, [('.filter(|id| is_session_id(id))', '.filter(|id| !id.is_empty())')], LIB + ['sessions::']),
    ('oldest instead of newest', SES, [('b_time\n                .cmp(a_time)', 'a_time\n                .cmp(b_time)')], LIB + ['newest_session_wins']),
    ('ambiguous prefix guesses', SES, [('_ => Err(LocateError::Ambiguous(sessions)),', '_ => Ok(sessions.remove(0)),')], LIB + ['prefix_selects']),
    ('path in read-error log', CMD, [('warn!(session = %short, kind = ?error.kind(), "transcript cannot be read");',
                                      'warn!(session = %short, path = %located.path.display(), kind = ?error.kind(), "transcript cannot be read");')],
     ['-p', 'cctg', '--test', 'command_logs']),
    ('whole transcript instead of last n', CMD, [('let slice = transcript::last_prompts(&turns, command.prompts);', 'let slice = &turns[..];')], LIB + ['replies_match_the_library']),
    ('last_prompts off by one', 'crates/transcript/src/render.rs', [('.nth(n - 1)', '.nth(n)')], ['-p', 'transcript', '--test', 'render', 'last_prompts']),
    # reviewer-2 changes
    ('header added to the reply', CMD, [('        body,\n        file_name', '        body: format!("brief · x\\n\\n{body}"),\n        file_name')], LIB + ['replies_match_the_library']),
    ('document suffix duplicates the accepted chunk', CMD, [('let rest = split.chunks[index..].concat();', 'let rest = split.chunks.concat();')], LIB + ['too_long_switches']),
    ('size limit not enforced', CMD, [('if file.metadata()?.len() > limit {', 'if false && file.metadata()?.len() > limit {'),
                                      ('Ok((bytes.len() as u64 <= limit).then_some(bytes))', 'Ok(Some(bytes))')], LIB + ['oversized_transcripts']),
    ('offset saved after the batch is handled', UPD, [('                    save_offset(store, next).await;\n                }\n                routed.into_iter().for_each(&mut handle);',
                                                       '                }\n                routed.into_iter().for_each(&mut handle);\n                if let Some(next) = next { save_offset(store, next).await; }')],
     LIB + ['offset_is_saved_before']),
    ('no save retry', UPD, [('for wait in SAVE_RETRY_WAITS {', 'for wait in [Duration::ZERO; 0] {')], LIB + ['briefly_failing_offset']),
    ('failed save stops dispatch (PLAN_V2 rule)', UPD, [('    if let Err(error) = result {\n        warn!',
                                                         '    while result.is_err() { tokio::time::sleep(Duration::from_secs(1)).await; result = store.save(offset); }\n    if let Err(error) = result {\n        warn!')],
     LIB + ['failing_offset_saves']),
    ('next offset never goes below the old one', UPD, [('let next = highest.map(|id| id.saturating_add(1)).or(offset);',
                                                        'let next = highest.map(|id| offset.map_or(id + 1, |o| o.max(id + 1))).or(offset);')],
     LIB + ['updates::']),
    ('stale offset still loaded', OFF, [('if age.is_some_and(|age| age > MAX_AGE) {', 'if false && age.is_some_and(|age| age > MAX_AGE) {')], LIB + ['offset_older_than']),
    ('poll callback waits for the command', 'crates/cctg/src/hub/mod.rs',
     [('            if commands.send(input).is_err() {', '            if commands.send(input).is_err() || { std::thread::sleep(std::time::Duration::from_secs(30)); false } {')],
     LIB + ['a_slow_command']),
]

out = []
failures = 0
for name, rel, pairs, args in MUTATIONS:
    path = os.path.join(WS, rel)
    original = io.open(path, encoding='utf-8', newline='').read()
    mutated = original
    for old, new in pairs:
        assert mutated.count(old) == 1, (name, old)
        mutated = mutated.replace(old, new)
    io.open(path, 'w', encoding='utf-8', newline='').write(mutated)
    try:
        result = subprocess.run(['cargo', 'test', '--offline', '-q', *args], cwd=WS, env=env,
                                capture_output=True, text=True, encoding='utf-8', errors='replace', timeout=900)
    finally:
        io.open(path, 'w', encoding='utf-8', newline='').write(original)
    compiled = 'error[' not in result.stderr and 'could not compile' not in result.stderr
    killed = result.returncode != 0 and compiled
    if not killed:
        failures += 1
    line = f"{'KILLED  ' if killed else 'SURVIVED'} compiled={compiled} {name}  ({' '.join(args)})"
    print(line, flush=True)
    out.append(line)
out.append(f"survivors={failures} of {len(MUTATIONS)}")
io.open(os.path.join(HERE, 'mutations.out.txt'), 'w', encoding='utf-8', newline='\n').write('\n'.join(out) + '\n')
