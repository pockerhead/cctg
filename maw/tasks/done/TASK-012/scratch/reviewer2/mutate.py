# Applies one mutation at a time to ws/, runs the named tests, restores the file.
import subprocess, sys, os
ws = os.path.join(os.path.dirname(os.path.abspath(__file__)), 'ws')
env = dict(os.environ, CARGO_TARGET_DIR=os.path.join(os.environ['TEMP'], 'cctg-task012-rev2-target'))
P = 'crates/cctg/src/proctree.rs'; H = 'crates/cctg/src/hook.rs'
M = [
 ('M1 no node exception', P, 'if env < named && has_stem(&ancestors[env].name, "node")', 'if false && env < named', ['--lib', 'proctree']),
 ('M2 any name by env pid', P, 'if env < named && has_stem(&ancestors[env].name, "node")', 'if env < named', ['--lib', 'proctree']),
 ('M3 env pid wins even when farther', P, 'if env < named && has_stem(&ancestors[env].name, "node")', 'if has_stem(&ancestors[env].name, "node") || env > named', ['--lib', 'proctree']),
 ('M4 typed stop without path passes', H, '.filter(|path| !path.is_empty() && has_agent_files(path, probe.exists))\n                .ok_or(Skip("internal agent"))?;', '.filter(|path| !path.is_empty() && has_agent_files(path, probe.exists))\n                .unwrap_or_default();', ['--lib', 'internal_agents']),
 ('M5 one timeout for all', H, 'HookEvent::UserPromptSubmit { .. } => PROMPT_POST_TIMEOUT,', 'HookEvent::UserPromptSubmit { .. } => POST_TIMEOUT,', ['--lib', 'post_timeout']),
 ('M6 stdin waits 5 s', H, 'pub const STDIN_TIMEOUT: Duration = Duration::from_millis(300);', 'pub const STDIN_TIMEOUT: Duration = Duration::from_millis(5000);', ['--test', 'hook_cli', 'open_silent_stdin']),
]
for name, f, old, new, args in M:
    path = os.path.join(ws, f)
    src = open(path, encoding='utf-8', newline='').read()
    assert src.count(old) == 1, name
    open(path, 'w', encoding='utf-8', newline='').write(src.replace(old, new))
    try:
        r = subprocess.run(['cargo', 'test', '-p', 'cctg', '--offline', '-j', '2'] + args[:-1] + ['--', args[-1]], cwd=ws, env=env, capture_output=True, text=True, encoding='utf-8', errors='replace')
        out = r.stdout + r.stderr
        failed = [l for l in out.splitlines() if l.endswith('FAILED') and l.startswith('test ')]
        compile_err = 'error[' in out
        verdict = 'KILLED' if (r.returncode != 0 and failed) else ('COMPILE_ERROR' if compile_err else 'SURVIVED')
        print(f'{name}: {verdict} {failed}')
    finally:
        open(path, 'w', encoding='utf-8', newline='').write(src)
