# -*- coding: utf-8 -*-
# Runs each mutation against `cargo test -p cctg --lib hub::` in ws/ and
# restores the file. M1-M8 are the planner's; M9-M18 guard reviewer-2 fixes.
# Expects CARGO_TARGET_DIR, CARGO_PROFILE_DEV_DEBUG=0 in the env.
import io, os, subprocess
here = os.path.dirname(os.path.abspath(__file__))
ws = os.path.join(here, 'ws')
S = 'crates/cctg/src/hub/slots.rs'
R = 'crates/cctg/src/hub/registry.rs'
A = 'crates/cctg/src/hub/subagents.rs'
M = [
 ('M1 any Agent call matches any agent id', A,
  'self.calls.get(self.links.get(agent_id)?)', 'self.calls.values().next()'),
 ('M2 reply loses target_agent', S,
  'meta.insert("target_agent".to_owned()', 'meta.insert("target_agent_x".to_owned()'),
 ('M3 a send cut off by a restart is sent again', R,
  'None if block.sending => {', 'None if false && block.sending => {'),
 ('M4 no lost mark on restart', S,
  '        registry.lose_blocks(&ended);\n', '        let _ = &ended;\n'),
 ('M5 a stop does not reopen the window', A,
  '                candidate.deadline = now + window;\n', ''),
 ('M6 handed-back report ignored', S,
  'report: self.reports.take(agent_id),', 'report: None,'),
 ('M7 nested run gets no block', R,
  'None => entry.block = Some(Block::running(nested_header(session))),', 'None => {}'),
 ('M8 subagent of a confirmed block re-confirmed (duplicate)', R,
  '        if self.subagents.contains_key(agent_id) {\n            return false;\n        }\n', ''),
 ('M9 legacy subagent records kept on load', R,
  '.retain(|_, agent| !agent.block.header.is_empty());', '.retain(|_, _| true);'),
 ('M10 block jobs in flight not capped', S,
  'let room = MAX_BLOCK_JOBS.saturating_sub(self.block_jobs);', 'let room = usize::MAX;'),
 ('M11 a stale body read is applied', S,
  'if !text.is_empty() && !self.bodies_waiting.contains_key(&agent_id) {', 'if !text.is_empty() {'),
 ('M12 an unclear first send is retried', S,
  '(BlockJob::Send { key, .. }, delivery) if !send_refused(delivery.as_ref()) => {',
  '(BlockJob::Send { key, .. }, delivery) if false && !send_refused(delivery.as_ref()) => {'),
 ('M13 nested answer kept only in memory (not stored)', S,
  '            self.registry.set_nested_answer(session, answer);\n', ''),
 ('M14 a block confirmed after its session ended stays running', S,
  '                self.registry.lose_blocks(&[candidate.session]);\n', '                let _ = candidate.session;\n'),
 ('M15 subagent records not bounded', R,
  'if self.subagents.len() >= MAX_SUBAGENTS {', 'if false {'),
 ('M16 index calls not bounded', A,
  'while self.call_order.len() > MAX_INDEX_ENTRIES {', 'while false {'),
 ('M17 a lost nested run keeps its answer', R,
  '                self.take_nested_answer(session);\n', ''),
 ('M18 body reads not limited to one per agent', S,
  '.find(|agent_id| !self.bodies_reading.contains(*agent_id))', '.find(|_| true)'),
]
import sys
M=[m for m in M if m[0].split()[0] in sys.argv[1:]]
out = []
for name, f, old, new in M:
    p = os.path.join(ws, f)
    src = io.open(p, encoding='utf-8', newline='').read()
    assert src.count(old) == 1, name
    io.open(p, 'w', encoding='utf-8', newline='\n').write(src.replace(old, new))
    try:
        r = subprocess.run(['cargo', 'test', '-j', '1', '--offline', '-p', 'cctg', '--lib', 'hub::'],
                           cwd=ws, capture_output=True, text=True, encoding='utf-8', errors='replace')
        text = r.stdout + r.stderr
        failed = [l.strip() for l in text.splitlines() if l.strip().endswith('FAILED') and l.startswith('test ')]
        verdict = 'KILLED' if r.returncode != 0 else 'SURVIVED'
        if r.returncode != 0 and not failed:
            failed = ['(did not compile)'] if 'error[' in text else ['(nonzero exit)']
        out.append('%s: %s %s' % (name, verdict, failed))
    finally:
        io.open(p, 'w', encoding='utf-8', newline='\n').write(src)
    print(out[-1], flush=True)
io.open(os.path.join(here, 'mutations.rerun.out.txt'), 'w', encoding='utf-8', newline='\n').write('\n'.join(out) + '\n')
