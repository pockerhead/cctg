# Reviewer2 copy of planner/mutations.py plus M16-M23 for its fixes.
# Runs each mutation of the TASK-032 logic against the tests that should
# catch it, in a workspace with task032.patch applied, and restores the file.
# Usage: python mutations.py <workspace root> [M19,M20,...]
# With a list only those run and the result goes to mutations.rerun.out.txt.
# Expects CARGO_TARGET_DIR and CARGO_PROFILE_DEV_DEBUG=0 in the environment.
import os, subprocess, sys

ws = sys.argv[1]
F = 'crates/cctg/src/files.rs'
S = 'crates/cctg/src/hub/slots.rs'
I = 'crates/cctg/src/hub/ingress.rs'
A = 'crates/cctg/src/agent.rs'
C = 'crates/cctg/src/hub/scheduler.rs'
H = 'crates/cctg/src/hub/fetch.rs'
U = 'crates/cctg/src/hub/updates.rs'
BASE = ['cargo', 'test', '-j', '1', '--offline', '-p', 'cctg']
LIB = BASE + ['--lib']
E2E = BASE + ['--test', 'files_e2e']
UPD = BASE + ['--test', 'update_e2e']
M = [
 ('M1 a chunk out of order is taken', F,
  'if chunk.offset != self.bytes.len() as u64 {', 'if chunk.offset > self.bytes.len() as u64 + u64::MAX / 2 {', LIB),
 ('M2 a save overwrites an existing file', F,
  'match OpenOptions::new().write(true).create_new(true).open(&path) {',
  'match OpenOptions::new().write(true).create(true).truncate(true).open(&path) {', LIB),
 ('M3 a name keeps its path parts', F,
  "let last = raw.rsplit(['/', '\\\\']).next().unwrap_or_default();", 'let last = raw;', LIB),
 ('M4 a kept file does not hold the messages after it', S,
  '''            "file of a kept message being fetched for the session agent"
        );
        FileStep::Wait''',
  '''            "file of a kept message being fetched for the session agent"
        );
        FileStep::Gone { delivered: true }''', LIB),
 ('M5 a closed link loses the kept file', S,
  'if outcome == Fetched::LinkClosed {', 'if outcome == Fetched::LinkClosed && false {', LIB),
 ('M6 an agent without files is sent the file', S,
  'if !bound.files {', 'if false {', LIB),
 ('M7 a file announced too big is kept and fetched', S,
  '''                let size = media.file.size.unwrap_or_default();
                if size > files::MAX_DOWNLOAD {''',
  '''                let size = media.file.size.unwrap_or_default();
                if size > u64::MAX - 1 {''', LIB),
 ('M8 a picture goes as a document', S,
  'let photo = size <= files::MAX_PHOTO && files::is_photo(&bytes);', 'let photo = false && files::is_photo(&bytes);', LIB),
 ('M9 no busy limit for files from sessions', S,
  '} else if self.file_bytes + size > MAX_FILE_BYTES', '} else if false', LIB),
 ('M10 ingress drops file offers', I,
  '''                        | AgentMsg::FileOffer { .. }
''', '', E2E),
 ('M11 a resumed worker does not ask for the tools again', A,
  'output.write_all(&channel::tools_changed()).await?;', 'let _ = channel::tools_changed();', UPD),
 ('M12 a hub without files is offered one', A,
  'if !hub_files {', 'if false {', LIB),
 ('M13 a refused photo is not sent as a document', C,
  'Err(ApiError::Telegram { code: 400, .. }) => {', 'Err(ApiError::Telegram { code: 999, .. }) => {', E2E),
 ('M14 a lost link keeps the unfinished file', A,
  'if inbox.take().is_some() {', 'if inbox.is_some() {', LIB),
 ('M15 the agent logs where a file went', A,
  'info!(kind = kind.as_str(), size, "file from the topic saved");',
  'info!(kind = kind.as_str(), size, path = ?saved, "file from the topic saved");', E2E),
 ('M16 an idle accepted upload keeps its share', S,
  '.filter(|(_, upload)| now.duration_since(upload.touched) >= UPLOAD_IDLE)', '.filter(|_| false)', LIB),
 ('M17 a chunk does not keep its upload alive', S,
  '        upload.touched = Instant::now();\n        let broken', '        let broken', LIB),
 ('M18 a lost link does not end the wait for room', A,
  'event = uploads.recv() => match event {', 'event = std::future::pending::<Option<Upload>>() => match event {', LIB),
 ('M19 a flood wait is not tried again', H,
  'ApiError::RetryAfter(after) => (*after).min(MAX_FLOOD_WAIT),', 'ApiError::RetryAfter(_) => return Err(error),', LIB),
 ('M20 a refusal is tried again', H,
  '_ => return Err(error),\n        };\n        if tried == TRIES {', '_ => RETRY_WAIT,\n        };\n        if tried == TRIES {', LIB),
 ('M21 the inbox ignore file hides all of .cctg', F,
  '    keep_out_of_git(dir);', '    keep_out_of_git(dir.parent().unwrap_or(dir));', LIB),
 ('M22 two largest files may wait at once', S,
  'pub const MAX_FILE_BYTES: u64 = files::MAX_UPLOAD;', 'pub const MAX_FILE_BYTES: u64 = 100 << 20;', LIB),
 ('M23 photo sizes multiply without a bound', U,
  'size.width.saturating_mul(size.height)', 'size.width * size.height', LIB),]
only = set(sys.argv[2].split(',')) if len(sys.argv) > 2 else None
out = []
for name, rel, old, new, cmd in M:
    if only and name.split()[0] not in only:
        continue
    path = os.path.join(ws, rel)
    raw = open(path, 'rb').read()
    crlf = b'\r\n' in raw
    text = raw.decode('utf-8')
    o, n = (old.replace('\n', '\r\n'), new.replace('\n', '\r\n')) if crlf else (old, new)
    if text.count(o) != 1:
        out.append('%s: PATTERN NOT FOUND ONCE (%d)' % (name, text.count(o)))
        print(out[-1], flush=True)
        continue
    open(path, 'wb').write(text.replace(o, n).encode('utf-8'))
    try:
        r = subprocess.run(cmd, cwd=ws, capture_output=True, text=True, encoding='utf-8', errors='replace', timeout=1800)
        verdict = 'KILLED' if r.returncode != 0 else 'SURVIVED'
        failed = [l.split()[1] for l in r.stdout.splitlines() if l.startswith('test ') and l.endswith('FAILED')]
        if r.returncode != 0 and not failed:
            failed = ['(build failure or harness=false test)']
        out.append('%s: %s %s' % (name, verdict, ', '.join(failed[:4])))
    finally:
        open(path, 'wb').write(raw)
    print(out[-1], flush=True)
txt = '\n'.join(out) + '\n'
open(os.path.join(os.path.dirname(os.path.abspath(__file__)), 'mutations.rerun.out.txt' if only else 'mutations.out.txt'), 'w', encoding='utf-8', newline='\n').write(txt)
