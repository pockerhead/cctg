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
 ('R11a a read that went to a gone connection waits for its timeout (rerun 1)', S,
  '&& (now >= sent + READ_TIMEOUT || conn != Some(asked))', '&& (now >= sent + READ_TIMEOUT)', CCTG),
 ('R11b same (rerun 2)', S,
  '&& (now >= sent + READ_TIMEOUT || conn != Some(asked))', '&& (now >= sent + READ_TIMEOUT)', CCTG),
 ('M14x (M14 for the round-2 code) a first read starts at the file start', T,
  '        None => last_line_start(&mut file, len),\n', '        None => 0,\n', CCTG),
 ('M14y (M1 revert) a first read starts at its end even mid-line', T,
  '        None => last_line_start(&mut file, len),\n', '        None => len,\n', CCTG + ['--test', 'stream_e2e']),
 ('R8x (R8 for the round-2 code) a turn end read before its Stop is never claimed', H,
  '        let claimed = self.ends_unclaimed > 0;\n', '        let claimed = false;\n', CCTG),
 ('R8y (ends_unclaimed fix revert) a new turn clears unclaimed turn ends at once', H,
  '            self.ends_until.get_or_insert(until);\n', '            self.ends_unclaimed = 0;\n            let _ = until;\n', CCTG),
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
name = 'mutations_r2_extra.out.txt'
io.open(os.path.join(here, name), 'w', encoding='utf-8', newline='\n').write('\n'.join(out) + '\n')
