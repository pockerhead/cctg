# Runs each mutation of the TASK-018 logic against the named test targets in
# ws/ and restores the file. Expects CARGO_TARGET_DIR etc. in the env.
import io, os, subprocess
here = os.path.dirname(os.path.abspath(__file__))
ws = os.path.join(here, 'ws')
H = 'crates/cctg/src/hook.rs'
SP = 'crates/cctg/src/spool.rs'
AG = 'crates/cctg/src/agent.rs'
DV = 'crates/cctg/src/device.rs'
SC = 'crates/cctg/src/hub/scheduler.rs'
SL = 'crates/cctg/src/hub/slots.rs'
RG = 'crates/cctg/src/hub/registry.rs'
LIB = lambda f: ['--lib', f]
E2E = ['--test', 'spool_e2e']
SOAK = ['--test', 'soak', '--', '--ignored']
M = [
 ('M1 an undelivered start is not kept', H,
  '|root| spool::save(&root, &hook_post, SystemTime::now()),',
  '|_root| Err(spool::SpoolError::NotKept),', [E2E]),
 ('M2 a hook does not replay its session spool', H,
  '    if let Some(root) = spool {\n', '    if let Some(root) = spool.filter(|_| false) {\n', [LIB('hook::'), E2E]),
 ('M3 the own event goes although a kept one failed', H,
  'let sent = spool::replay(root, &hook_post.session_id, addr, secret, deadline).await?;',
  'let sent = spool::replay(root, &hook_post.session_id, addr, secret, deadline).await.unwrap_or(0);', [LIB('hook::')]),
 ('M4 every event is kept (texts too)', SP,
  '        HookEvent::SessionStart { .. } | HookEvent::SessionEnd { .. }\n', '        _\n', [LIB('spool'), E2E]),
 ('M5 no per-session cap', SP,
  'if prune(root, now) >= MAX_FILES || files(&dir).len() >= MAX_PER_SESSION {',
  'if prune(root, now) >= MAX_FILES {', [LIB('spool'), E2E]),
 ('M6 expired files are replayed', SP,
  'Some(post) if !expired(stamp, now) => out.push((file, post)),', 'Some(post) => out.push((file, post)),', [LIB('spool')]),
 ('M7 a delivered file stays', SP,
  '        post(addr, secret, &kept, left).await?;\n        // Another replay of the same file may have removed it already.\n        let _ = std::fs::remove_file(&file);\n',
  '        post(addr, secret, &kept, left).await?;\n        let _ = &file;\n', [LIB('spool'), E2E]),
 ('M8 the agent does not replay on registration', AG,
  '                spawn_replay(&config);\n', '', [E2E]),
 ('M9 a relative CCTG_STATE_DIR is used', DV,
  '            .filter(|dir| dir.is_absolute())\n', '', [LIB('device::')]),
 ('M10 soak: 429 does not pause the queue', SC,
  '                self.paused_until = Some(Instant::now() + wait);\n', '                let _ = wait;\n', [SOAK]),
 ('M11 soak: permission prompts are not first', SC,
  '    fn next_permission(&self) -> Option<usize> {\n', '    fn next_permission(&self) -> Option<usize> {\n        if true {\n            return None;\n        }\n', [SOAK]),
 ('M12 soak: forum_topic_edited is not deleted', SL,
  '        self.hand_off(Work::Delete, Op::Delete { message_id });\n', '        let _ = message_id;\n', [SOAK]),
 ('M13 soak: nesting is ignored', RG,
  '        let Some(pid) = parent_pid else {\n            return SessionKind::TopLevel;\n        };\n',
  '        let Some(pid) = parent_pid.filter(|_| false) else {\n            return SessionKind::TopLevel;\n        };\n', [SOAK]),
 ('M14 soak: a freed slot is not reused', RG,
  'if let Some(&(_, id)) = ordinals.iter().find(|(_, id)| self.is_free(*id)) {',
  'if let Some(&(_, id)) = ordinals.iter().find(|(_, id)| self.is_free(*id) && false) {', [SOAK]),
]
out = []
for name, f, old, new, runs in M:
    p = os.path.join(ws, f)
    src = io.open(p, encoding='utf-8', newline='').read()
    assert src.count(old) == 1, name
    io.open(p, 'w', encoding='utf-8', newline='\n').write(src.replace(old, new))
    verdict, failed = 'SURVIVED', []
    try:
        for args in runs:
            cmd = ['cargo', 'test', '-j', '1', '--offline', '-p', 'cctg'] + args
            r = subprocess.run(cmd, cwd=ws, capture_output=True, text=True, encoding='utf-8',
                               errors='replace', timeout=1200)
            text = r.stdout + r.stderr
            if r.returncode != 0:
                verdict = 'KILLED'
                hits = [l.strip() for l in text.splitlines() if l.strip().endswith('FAILED') and l.startswith('test ')]
                lines = text.splitlines()
                hits += [lines[i + 1].strip() for i, l in enumerate(lines[:-1]) if 'panicked at' in l][:2]
                if not hits:
                    hits = ['(did not compile)'] if 'error[' in text else ['(nonzero exit)']
                failed.append('%s: %s' % (' '.join(args), hits))
    except subprocess.TimeoutExpired:
        verdict = 'KILLED'
        failed.append('(timeout)')
    finally:
        io.open(p, 'w', encoding='utf-8', newline='\n').write(src)
    out.append('%s: %s %s' % (name, verdict, failed))
    print(out[-1], flush=True)
io.open(os.path.join(here, 'mutations.out.txt'), 'w', encoding='utf-8', newline='\n').write('\n'.join(out) + '\n')
