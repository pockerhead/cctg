# Mutations of install.sh and the doctor/ping code (planner's 10 plus
# reviewer2's), each must make a test fail. Windows-only kills: locale-*.
#  Usage: python run_mutations.py <workspace root> [name...]
# Changes one file at a time and restores it; runs one cargo at a time.
import os, subprocess, sys
ws = sys.argv[1]
only = set(sys.argv[2:])
env = dict(os.environ, CARGO_TARGET_DIR='C:/Users/user/dev/cctg/target', CARGO_PROFILE_DEV_DEBUG='0')
M = [
    # name, file, old, new, cargo test args
    ('put-always-rewrites', 'install.sh', '    if [ -f "$1" ] && cmp -s "$1.tmp" "$1"; then', '    if false; then',
     ['--test', 'install_e2e', 'install_update_and_uninstall_a_device']),
    ('checksum-not-checked', 'install.sh', '[ "$(sha256 "$tmp/$asset")" = "$want" ] ||', 'true ||',
     ['--test', 'install_e2e', 'a_bad_checksum_installs_nothing']),
    ('secret-printed', 'install.sh', '    [ -z "$secret" ] || check_secret\n', '    [ -z "$secret" ] || check_secret\n    say "secret $secret"\n',
     ['--test', 'install_e2e', 'install_update_and_uninstall_a_device']),
    ('secret-unquoted', 'install.sh', '"$1" "$(squote "$2")"', '"$1" "$2"',
     ['--test', 'install_e2e', 'install_update_and_uninstall_a_device']),
    ('running-exe-not-moved-aside', 'install.sh', '    if [ "$os" = windows ] && [ -e "$exe" ]; then', '    if false; then',
     ['--test', 'install_e2e', 'install_update_and_uninstall_a_device']),
    ('user-claude-settings-written', 'install.sh', '    put "$conf_dir/settings.json" 644 <<EOF', '    mkdir -p "$home/.claude"; put "$home/.claude/settings.json" 644 <<EOF',
     ['--test', 'install_e2e', 'install_update_and_uninstall_a_device']),
    ('hub-secret-regenerated', 'install.sh', '    hub_secret=$(value_of "$hub_env" CCTG_HUB_SECRET)', '    hub_secret=',
     ['--test', 'install_e2e', 'a_hub_is_set_up_with_docker_compose']),
    ('official-installer-without-asking', 'install.sh', '        interactive || answer=n', '        answer=y',
     ['--test', 'install_e2e', 'a_missing_claude_code_comes_from_anthropics_installer_when_asked']),
    ('ping-route-missing', 'crates/cctg/src/hub/ingress.rs', '        PING_PATH => Route::Ping,\n', '',
     ['--lib', 'ingress::tests::a_ping']),
    ('doctor-takes-a-rejected-secret', 'crates/cctg/src/doctor.rs',
     '            Err(PostError::Status(401)) => fail(\n                &mut lines,',
     '            Err(PostError::Status(401)) => lines.push(\n                ',
     ['--lib', 'doctor::']),
    # reviewer2
    ('locale-c-on-windows', 'install.sh', 'if [ "$os" = windows ]; then LC_ALL=C.UTF-8; else LC_ALL=C; fi', 'LC_ALL=C',
     ['--test', 'install_e2e', 'install_update_and_uninstall_a_device']),
    ('binary-not-of-the-tag', 'install.sh', 'windows-*) asset=cctg-$RELEASE-x86_64', 'windows-*) asset=cctg-x86_64',
     ['--test', 'install_e2e', 'install_update_and_uninstall_a_device']),
    ('client-line-from-main', 'install.sh', 'githubusercontent.com/$REPO/$RELEASE/install.sh', 'githubusercontent.com/$REPO/main/install.sh',
     ['--test', 'install_e2e', 'a_hub_is_set_up_with_docker_compose']),
    ('changed-compose-overwritten', 'install.sh', '        if [ ! -f "$hub_dir/$f" ] || cmp -s "$tmp/$f" "$hub_dir/$f"; then', '        if true; then',
     ['--test', 'install_e2e', 'a_hub_is_set_up_with_docker_compose']),
]
for name, path, old, new, args in M:
    if only and name not in only:
        continue
    p = os.path.join(ws, path)
    orig = open(p, 'rb').read()
    text = orig.decode('utf-8')
    assert text.count(old) == 1, (name, text.count(old))
    open(p, 'wb').write(text.replace(old, new).encode('utf-8'))
    try:
        r = subprocess.run(['cargo', 'test', '-j', '1', '-p', 'cctg', '--locked'] + args,
                           cwd=ws, env=env, capture_output=True, text=True, encoding='utf-8', errors='replace')
        out = r.stdout + r.stderr
        killed = r.returncode != 0
        print(('KILLED ' if killed else 'SURVIVED ') + name, flush=True)
        if not killed:
            print(out[-1500:])
    finally:
        open(p, 'wb').write(orig)
