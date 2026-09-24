# Fixer mutation check: applies one mutation, runs the filtered tests, restores the file byte for byte.
import os, subprocess, sys

ROOT = 'C:/Users/user/dev/cctg'
MUTANTS = {
    # M1: a turn end read again whose answer is in the topic is not skipped.
    'M1_no_answered_skip': ('crates/cctg/src/hub/stream.rs',
        'self.answered_ends.swap_remove(at);\r\n            return None;',
        'self.answered_ends.swap_remove(at);'),
    # M2: overdue answers are not sent first on the re-read.
    'M2_no_overdue': ('crates/cctg/src/hub/stream.rs',
        'let mut due = Vec::new();\r\n        while self',
        'let mut due = Vec::new();\r\n        while false && self'),
    # M3: the cap is ignored for streamed answers.
    'M3_no_room': ('crates/cctg/src/hub/slots.rs',
        'split.prefer_file || split.chunks.len() > room',
        'split.prefer_file'),
    # M4: accepted answers popped by advance are not remembered.
    'M4_no_advance_record': ('crates/cctg/src/hub/stream.rs',
        '}) => self.answered_ends.extend(held.end),',
        '}) => drop(held),'),
    # M5: an answer gone by its timeout does not keep its turn end.
    'M5_no_answered_early': ('crates/cctg/src/hub/stream.rs',
        'self.answered_ends.push(end);',
        'let _ = end;'),
    # M6: a re-held answer's end stays in answered_ends.
    'M6_no_rewind_retain': ('crates/cctg/src/hub/stream.rs',
        'answered_ends.retain(|end| !held.iter()',
        'answered_ends.retain(|end| true || !held.iter()'),
}
env = dict(os.environ, CARGO_TARGET_DIR=os.path.join(os.environ['TEMP'], 'cctg-023-fix-target'),
           CARGO_PROFILE_DEV_DEBUG='0')
for name in sys.argv[1:] or MUTANTS:
    rel, a, b = MUTANTS[name]
    path = os.path.join(ROOT, rel)
    orig = open(path, 'rb').read()
    text = orig.decode('utf-8')
    assert text.count(a) == 1, name
    open(path, 'wb').write(text.replace(a, b).encode('utf-8'))
    try:
        r = subprocess.run(['cargo', 'test', '-j', '1', '-p', 'cctg', '--lib', '--', 'hub::stream', 'hub::slots'],
                           cwd=ROOT, env=env, capture_output=True, text=True, encoding='utf-8', errors='replace')
        failed = [l for l in r.stdout.splitlines() if l.endswith('FAILED') and l.startswith('test ')]
        print(name, 'KILLED' if r.returncode else 'SURVIVED', failed)
    finally:
        open(path, 'wb').write(orig)
