# Applies one mutation at a time to ws/, runs the named tests, restores the file.
import subprocess, sys, os
here = os.path.dirname(os.path.abspath(__file__))
ws = os.path.join(here, 'ws')
H = 'crates/cctg/src/hub/'
MUTATIONS = [
    ('M1 live filter removed', H+'slots.rs', 'if !self.registry.is_live_top_level(session) {', 'if false {', ['--lib', 'hub::slots']),
    ('M2 topic-root reply kept', H+'updates.rs', '.filter(|&id| id != 0 && Some(id) != thread_id);', '.filter(|&id| id != 0);', ['--lib', 'hub::updates']),
    ('M3 cap removed', H+'slots.rs', 'if self.queued_messages + ops.len() > MAX_QUEUED_MESSAGES {', 'if false {', ['--lib', 'hub::slots']),
    ('M4 commands forwarded as messages', H+'mod.rs', 'Routed::Input(input) if commands::is_command(&input) => {', 'Routed::Input(input) if false => {', ['--lib', 'hub::tests']),
    ('MF1 notice cooldown removed', H+'slots.rs', '.is_some_and(|&last| now < last + self.options.notice_every)', '.is_some_and(|_| false)', ['--lib', 'hub::slots']),
    ('MF2 delivery does not re-arm the offline notice', H+'slots.rs', 'self.notices.remove(&(slot, OFFLINE_NOTICE));', '', ['--lib', 'hub::slots']),
    ('MF3 one cooldown per slot for every kind', H+'slots.rs', '.get(&(slot, notice))', '.get(&(slot, OFFLINE_NOTICE))', ['--lib', 'hub::slots']),
    ('MF4 overflow warns every time', H+'slots.rs', 'if !self.overflow_warned {', 'if true {', ['--test', 'overflow_logs']),
    ('MF5 overflow warning never re-armed', H+'slots.rs', 'if self.queued_messages == 0 {', 'if false {', ['--test', 'overflow_logs']),
]
MF3_INSERT = ('self.notices.insert((slot, notice), now);', 'self.notices.insert((slot, OFFLINE_NOTICE), now);')
env = dict(os.environ, CARGO_TARGET_DIR=r'C:\Users\user\AppData\Local\Temp\cctg-t021-rev2-target', CARGO_PROFILE_DEV_DEBUG='0')
only = sys.argv[1:]
for name, path, old, new, args in MUTATIONS:
    if only and name.split()[0] not in only:
        continue
    p = os.path.join(ws, path)
    orig = open(p, encoding='utf-8', newline='').read()
    assert orig.count(old) == 1, (name, orig.count(old))
    mutated = orig.replace(old, new)
    if name.startswith('MF3'):
        mutated = mutated.replace(*MF3_INSERT)
    open(p, 'w', encoding='utf-8', newline='').write(mutated)
    try:
        cmd = ['cargo', 'test', '-j', '1', '-p', 'cctg'] + args[:1] + ([] if args[0] == '--lib' else [args[1]])
        if args[0] == '--lib':
            cmd += ['--', args[1]]
        r = subprocess.run(cmd, cwd=ws, env=env, capture_output=True, text=True, encoding='utf-8', errors='replace')
        out = r.stdout + r.stderr
        lines = [l for l in out.splitlines() if ' FAILED' in l or l.startswith('test result') or 'error[' in l or 'panicked' in l]
        print(name + ':' + (' KILLED' if r.returncode != 0 else ' SURVIVED'))
        print('\n'.join(lines[:8]))
        sys.stdout.flush()
    finally:
        open(p, 'w', encoding='utf-8', newline='').write(orig)
