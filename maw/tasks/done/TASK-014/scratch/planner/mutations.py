# Applies one mutation at a time to ws/ slots.rs, runs the lib tests of
# hub::slots and hub::permissions, restores the file. Output: mutations.out.txt.
import os, subprocess
here = os.path.dirname(os.path.abspath(__file__))
ws = os.path.join(here, 'ws')
p = os.path.join(ws, 'crates', 'cctg', 'src', 'hub', 'slots.rs')
orig = open(p, encoding='utf-8').read()
env = dict(os.environ, CARGO_TARGET_DIR=os.path.join(os.environ['TEMP'], 'cctg-t014-target'),
           CARGO_PROFILE_DEV_DEBUG='0')
mutations = [
    ('M1 no already-decided check (second verdict)',
     'if prompt.decided.is_some() {', 'if false && prompt.decided.is_some() {'),
    ('M2 prompt not on the permission lane',
     '''                reply_markup: Some(permissions::keyboard(&prompt.request_id)),
                permission: true,''',
     '''                reply_markup: Some(permissions::keyboard(&prompt.request_id)),
                permission: false,'''),
    ('M3 prompt only for a live, current session (reply rule)',
     '''                .get(&prompt.session)
                .and_then(|entry| entry.slot)''',
     '''                .get(&prompt.session)
                .filter(|entry| !entry.ended)
                .and_then(|entry| entry.slot)'''),
    ('M4 buttons kept after the decision',
     'reply_markup: Some(permissions::no_keyboard()),', 'reply_markup: None,'),
    ('M5 prompt found by request id, not by message',
     'let Some(key) = self.prompts.by_message(message_id) else {',
     'let Some(key) = self.prompts.unsent().first().copied().or(Some(0)).filter(|_| message_id > 0) else {'),
    ('M6 no fallback to the reconnected process',
     'let pid = prompt.claude_pid?;', 'let pid = prompt.claude_pid.filter(|_| false)?;'),
]
out = []
for name, old, new in mutations:
    assert orig.count(old) == 1, name
    open(p, 'w', encoding='utf-8', newline='\n').write(orig.replace(old, new))
    r = subprocess.run(['cargo', 'test', '-j', '1', '-p', 'cctg', '--lib', 'hub::'],
                       cwd=ws, env=env, capture_output=True, text=True, encoding='utf-8', errors='replace')
    failed = [l for l in r.stdout.splitlines() if l.endswith('FAILED') and l.startswith('test ')]
    verdict = 'KILLED' if r.returncode != 0 else 'SURVIVED'
    out.append('%s: %s %s' % (name, verdict, failed))
    print(out[-1], flush=True)
open(p, 'w', encoding='utf-8', newline='\n').write(orig)
open(os.path.join(here, 'mutations.out.txt'), 'w', encoding='utf-8', newline='\n').write('\n'.join(out) + '\n')
