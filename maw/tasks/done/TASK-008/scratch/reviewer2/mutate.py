# Mutation runner for the reviewer-2 reference. Mutates scratch/reviewer2/mut only
# (a fresh copy of scratch/reviewer2/ws), restores and touches each file after the run.
import subprocess, os, re, sys

ROOT = os.path.dirname(os.path.abspath(__file__))
WS = os.path.join(ROOT, 'mut')
env = dict(os.environ, CARGO_TARGET_DIR=os.path.join(os.environ['TEMP'], 'cctg-task008-reviewer2-target'))
API = 'crates/cctg/src/hub/api.rs'
SCH = 'crates/cctg/src/hub/scheduler.rs'
UPD = 'crates/cctg/src/hub/updates.rs'
CFG = 'crates/cctg/src/hub/config.rs'

# (label, path, [(old, new), ...])
M = [
    # planner's original set
    ('no without_url', API, [('Self::Http(error.without_url())', 'Self::Http(error)')]),
    ('capacity 20', SCH, [('capacity: 5,', 'capacity: 20,')]),
    ('everything but edits metered', SCH, [('matches!(self, Op::Send { .. } | Op::SendDocument { .. })',
                                           '!matches!(self, Op::Edit { .. })')]),
    ('no 429 pause', SCH, [('self.paused_until = Some(Instant::now() + wait);', 'let _ = wait;')]),
    ('no allowlist on messages', UPD, [('        if !allowlist.contains(from.id) {\n'
                                       '            return Routed::Ignored(Ignored::NotAllowed);\n'
                                       '        }\n'
                                       '        return Routed::Input',
                                       '        return Routed::Input')]),
    ('stop batch on bad update', UPD, [('Err(_) => Routed::Ignored(Ignored::Malformed),',
                                        'Err(_) => return (next, routed),')]),
    # reviewer-2 fixes
    ('permission ignores its topic', SCH, [('if permission && !busy_topics.contains(&thread_id) {',
                                            'if permission {')]),
    ('no permission priority', SCH, [('let permission = self.next_permission();',
                                      'let permission: Option<usize> = None;')]),
    ('429 job to the back', SCH, [('self.lane_mut(lane).push_front(job);',
                                   'self.lane_mut(lane).push_back(job);')]),
    ('token only on success', SCH, [('if job.op.metered() {\n'
                                     '            self.bucket.take(Instant::now());\n'
                                     '        }\n',
                                     'let metered = job.op.metered();\n'),
                                    ('            result => {\n',
                                     '            result => {\n'
                                     '                if metered {\n'
                                     '                    self.bucket.take(Instant::now());\n'
                                     '                }\n')]),
    ('dotenvy error text in EnvFile', CFG, [('        path: path.display().to_string(),\n'
                                             '        reason: match error {',
                                             '        path: format!("{} {:?}", path.display(), '
                                             'dotenvy::from_path_iter(path).map(|i| i.collect::<Vec<_>>())),\n'
                                             '        reason: match error {')]),
    ('log raw update', UPD, [('        let item = match serde_json::from_value::<Update>(value) {',
                              '        debug!(%value, "raw update");\n'
                              '        let item = match serde_json::from_value::<Update>(value) {')]),
]

out = []
for label, path, pairs in M:
    full = os.path.join(WS, path)
    src = open(full, encoding='utf-8').read()
    mutated = src
    for old, new in pairs:
        assert mutated.count(old) == 1, (label, old)
        mutated = mutated.replace(old, new)
    open(full, 'w', encoding='utf-8', newline='\n').write(mutated)
    try:
        r = subprocess.run(['cargo', 'test', '-p', 'cctg', '--offline', '--no-fail-fast'], cwd=WS, env=env,
                           capture_output=True, text=True, encoding='utf-8', errors='replace')
        text = r.stdout + r.stderr
        failed = sorted(set(re.findall(r'^test (\S+) \.\.\. FAILED', text, re.M)))
        compile_err = 'could not compile' in text
    finally:
        open(full, 'w', encoding='utf-8', newline='\n').write(src)
        os.utime(full, None)
    status = 'COMPILE ERROR' if compile_err else ('KILLED' if failed else 'SURVIVED')
    out.append(f'== {label} ({path})\n   {status}: {", ".join(failed)}')
    print(out[-1], flush=True)

open(os.path.join(ROOT, 'mutations.out.txt'), 'w', encoding='utf-8', newline='\n').write('\n'.join(out) + '\n')
