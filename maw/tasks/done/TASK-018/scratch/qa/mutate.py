# QA mutations against the current soak (TASK-018). Applies the edits of one
# mutation to the working tree, runs the fake soak once, restores every file
# byte for byte. Usage: python mutate.py Q1|Q2|Q3 (CARGO_TARGET_DIR in env).
import io, os, subprocess, sys
repo = r'C:/Users/user/dev/cctg'
here = os.path.dirname(os.path.abspath(__file__))
SC = 'crates/cctg/src/hub/scheduler.rs'
SOAK = 'crates/cctg/tests/soak.rs'
NO_PRIORITY = (SC, '    fn next_permission(&self) -> Option<usize> {\n',
               '    fn next_permission(&self) -> Option<usize> {\n        if true {\n            return None;\n        }\n')
NO_PAUSE = (SC, '                self.paused_until = Some(Instant::now() + wait);\n', '                let _ = wait;\n')
NO_FLOOD_CHECK = (SOAK, '        tg.flood.lock().unwrap().is_empty(),\n', '        true,\n')
M = {
 'Q1': ('permission prompts are not first (scheduler next_permission -> None)', [NO_PRIORITY]),
 'Q2': ('a 429 does not pause the queue', [NO_PAUSE]),
 'Q3': ('Q1 with the "429s fell into the burst" check neutralised: does the permission assertion itself catch it',
        [NO_PRIORITY, NO_FLOOD_CHECK]),
}
key = sys.argv[1]
name, edits = M[key]
saved = []
try:
    for f, old, new in edits:
        p = os.path.join(repo, f)
        raw = io.open(p, 'rb').read()
        saved.append((p, raw))
        src = raw.decode('utf-8')
        crlf = '\r\n' in src
        o = old.replace('\n', '\r\n') if crlf else old
        n = new.replace('\n', '\r\n') if crlf else new
        assert src.count(o) == 1, (key, f, src.count(o))
        io.open(p, 'wb').write(src.replace(o, n).encode('utf-8'))
    r = subprocess.run(['cargo', 'test', '-j', '1', '--offline', '-p', 'cctg', '--test', 'soak', '--', '--ignored'],
                       cwd=repo, capture_output=True, text=True, encoding='utf-8', errors='replace', timeout=900)
    out = r.stdout + r.stderr
    io.open(os.path.join(here, f'mut_{key}.log'), 'w', encoding='utf-8').write(out)
    lines = out.splitlines()
    msg = []
    for i, l in enumerate(lines):
        if 'panicked' in l:
            msg = lines[i:i + 2]
            break
    print(f'{key} {name}: {"KILLED" if r.returncode != 0 else "SURVIVED"} exit={r.returncode}')
    for l in msg:
        print('   ', l)
finally:
    for p, raw in saved:
        io.open(p, 'wb').write(raw)
