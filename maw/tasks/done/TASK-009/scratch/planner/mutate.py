"""Applies one mutation at a time to the reference workspace, runs the named
test filter, expects a failure, restores the file. Output: mutations.out.txt."""
import io, os, subprocess

HERE = os.path.dirname(os.path.abspath(__file__))
WS = os.path.join(HERE, 'ws')
env = dict(os.environ, CARGO_TARGET_DIR=os.path.join(os.environ['TEMP'], 'cctg-task009-target'))

MUTATIONS = [
    ('offset not saved', 'crates/cctg/src/hub/updates.rs',
     '&& let Err(error) = store.save(next)', '&& let Err(error) = Ok::<(), std::io::Error>(())',
     ['-p', 'cctg', '--lib', 'saved_offset_prevents']),
    ('offset saved but not loaded', 'crates/cctg/src/hub/updates.rs',
     'let mut offset = store.load();', 'let mut offset = None;',
     ['-p', 'cctg', '--lib', 'saved_offset_prevents']),
    ('too-long not detected', 'crates/cctg/src/hub/commands.rs',
     'if description.to_ascii_lowercase().contains("too long")', 'if description.is_empty()',
     ['-p', 'cctg', '--lib', 'too_long_switches']),
    ('document retried as text', 'crates/cctg/src/hub/commands.rs',
     'return send_document(outbox, thread_id, reply, rest).await;',
     'if send_document(outbox, thread_id, reply, rest).await.is_err() { continue; } return Ok(());',
     ['-p', 'cctg', '--lib', 'too_long_switches']),
    ('prefer_file ignored', 'crates/cctg/src/hub/commands.rs',
     'if split.prefer_file {', 'if false && split.prefer_file {',
     ['-p', 'cctg', '--lib', 'large_reply_goes']),
    ('directories accepted as sessions', 'crates/cctg/src/hub/sessions.rs',
     'if !metadata.is_file() {', 'if metadata.is_symlink() {',
     ['-p', 'cctg', '--lib', 'non_session_entries']),
    ('any file name accepted', 'crates/cctg/src/hub/sessions.rs',
     '.filter(|id| is_session_id(id))', '.filter(|id| !id.is_empty())',
     ['-p', 'cctg', '--lib', 'sessions::']),
    ('oldest instead of newest', 'crates/cctg/src/hub/sessions.rs',
     'b_time\n                .cmp(a_time)', 'a_time\n                .cmp(b_time)',
     ['-p', 'cctg', '--lib', 'newest_session_wins']),
    ('ambiguous prefix guesses', 'crates/cctg/src/hub/sessions.rs',
     '_ => Err(LocateError::Ambiguous(sessions)),', '_ => Ok(sessions.remove(0)),',
     ['-p', 'cctg', '--lib', 'prefix_selects']),
    ('path in read-error log', 'crates/cctg/src/hub/commands.rs',
     'warn!(session = %short, kind = ?error.kind(), "transcript cannot be read");',
     'warn!(session = %short, path = %located.path.display(), kind = ?error.kind(), "transcript cannot be read");',
     ['-p', 'cctg', '--test', 'command_logs']),
    ('whole transcript instead of last n', 'crates/cctg/src/hub/commands.rs',
     'let slice = transcript::last_prompts(&turns, command.prompts);', 'let slice = &turns[..];',
     ['-p', 'cctg', '--lib', 'replies_match_the_library']),
    ('last_prompts off by one', 'crates/transcript/src/render.rs',
     '.nth(n - 1)', '.nth(n)',
     ['-p', 'transcript', '--test', 'render', 'last_prompts']),
]

out = []
for name, rel, old, new, args in MUTATIONS:
    path = os.path.join(WS, rel)
    original = io.open(path, encoding='utf-8', newline='').read()
    assert original.count(old) == 1, (name, old)
    io.open(path, 'w', encoding='utf-8', newline='').write(original.replace(old, new))
    try:
        result = subprocess.run(['cargo', 'test', '--offline', '-q', *args], cwd=WS, env=env,
                                capture_output=True, text=True, encoding='utf-8', errors='replace')
    finally:
        io.open(path, 'w', encoding='utf-8', newline='').write(original)
    verdict = 'KILLED' if result.returncode != 0 else 'SURVIVED'
    compiled = 'error[' not in result.stderr
    out.append(f'{verdict:8} compiled={compiled} {name}  ({" ".join(args)})')
    print(out[-1], flush=True)

io.open(os.path.join(HERE, 'mutations.out.txt'), 'w', encoding='utf-8', newline='\n').write('\n'.join(out) + '\n')
