# Mutation check of the reference: each mutant must make its tests fail.
# Usage: python mutations.py <workspace root>  (one cargo at a time, shared
# CARGO_TARGET_DIR, as for every build of this repo).
import os, subprocess, sys
ws = sys.argv[1]
env = dict(os.environ, CARGO_TARGET_DIR='C:/Users/user/dev/cctg/target', CARGO_PROFILE_DEV_DEBUG='0')
MUTANTS = [
    ('crates/cctg/src/hub/devices.rs', 'removed && unix(now) < expires', 'removed',
     ['--lib', '--', 'hub::devices::tests::a_code_is_good_once']),
    ('crates/cctg/src/hub/devices.rs', 'let removed = std::fs::remove_file(&path).is_ok();', 'let removed = true;',
     ['--lib', '--', 'hub::devices::tests::a_code_is_good_once']),
    ('crates/cctg/src/hub/devices.rs', '.is_some_and(|device| bool::from(device.hash.as_bytes().ct_eq(hash.as_bytes())));',
     '.is_some_and(|_| !hash.is_empty());',
     ['--lib', '--', 'hub::devices::tests::a_device_secret_gets_in']),
    ('crates/cctg/src/hub/ingress.rs', '() = revoked(&mut changes, devices, &who) => {',
     '() = std::future::pending::<()>() => { let _ = &mut changes; let _ = &who;',
     ['--lib', '--', 'hub::ingress::tests::a_revoked_device_loses']),
    ('crates/cctg/src/hub/ingress.rs', """    let max = if route == Route::Join {
        MAX_JOIN_BODY""", """    let max = if route == Route::Join {
        MAX_HOOK_BODY""",
     ['--lib', '--', 'hub::ingress::tests::a_join_code_buys']),
    ('crates/cctg/src/hub/ingress.rs', """            // No fast guessing, as for a wrong secret.
            tokio::time::sleep(AUTH_FAIL_DELAY).await;""", """            // No fast guessing, as for a wrong secret.""",
     ['--lib', '--', 'hub::ingress::tests::a_join_code_buys']),
    ('crates/cctg/src/join.rs', '.filter(|line| !sets_secret(line))', '.filter(|_| true)',
     ['--lib', '--', 'join::tests::the_secret_line']),
    ('crates/cctg/src/hub/roster.rs', 'Press::Ask(id) => match devices.name(&id) {',
     'Press::Ask(id) => match devices.revoke(&id).map(|revoked| revoked.name) {',
     ['--lib', '--', 'hub::roster::tests::the_list_asks']),
    ('crates/cctg/tests/hub_reads_no_files.rs', """            if raw {
                i += 1; // the `r`
                while chars.get(i) == Some(&'#') {
                    hashes += 1;
                    i += 1;
                }
            }""", """            i += 1;
            while raw && chars.get(i) == Some(&'#') {
                hashes += 1;
                i += 1;
            }""",
     ['--test', 'hub_reads_no_files']),
]
out = []
for path, old, new, args in MUTANTS:
    full = os.path.join(ws, path)
    original = open(full, encoding='utf-8').read()
    assert original.count(old) == 1, (path, old)
    open(full, 'w', encoding='utf-8', newline='\n').write(original.replace(old, new))
    try:
        os.utime(os.path.join(ws, 'crates/cctg/src/lib.rs'))
        r = subprocess.run(['cargo', 'test', '-j', '1', '-p', 'cctg', '--locked'] + args,
                           cwd=ws, env=env, capture_output=True, text=True, encoding='utf-8', errors='replace')
        caught = r.returncode != 0 and 'test result: FAILED' in (r.stdout + r.stderr)
        built = 'error[' not in r.stderr
        out.append('%s  %s: %s -> %s' % ('CAUGHT' if caught else ('BUILD-ERROR' if not built else 'MISSED'), path, old.strip()[:60], new.strip()[:60]))
    finally:
        open(full, 'w', encoding='utf-8', newline='\n').write(original)
    print(out[-1], flush=True)
open(os.path.join(os.path.dirname(os.path.abspath(__file__)), 'mutations.out.txt'), 'w', encoding='utf-8', newline='\n').write('\n'.join(out) + '\n')
