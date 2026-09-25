# TASK-031 fixer: each mutation of install.sh must make its test fail.
# install_e2e reads install.sh at run time, so no rebuild per mutation.
# Run from the repo root: python maw/tasks/in_progress/TASK-031/scratch/fixer/mutations.py
import os
import subprocess

SCRIPT = 'install.sh'
ENV = dict(os.environ, CARGO_TARGET_DIR='C:/Users/user/dev/cctg/target', CARGO_PROFILE_DEV_DEBUG='0')

MUTATIONS = [
    ('uninstall deletes the whole device.env', 'uninstall_keeps_what_the_script_did_not_write',
     '    strip_device_env\n    remove "$exe"', '    remove "$env_file"\n    remove "$exe"'),
    ('an empty --secret-file is accepted', 'missing_or_bad_settings_without_a_terminal_write_nothing',
     '        [ -n "$secret" ] || die "$secret_file is empty (the secret is its first line)"\n', ''),
    ('the moved-aside wrapper is not put back', 'install_update_and_uninstall_a_device',
     '        if [ ! -e "$w" ] && [ -e "$w.before-cctg-install" ]; then', '        if false; then'),
    ('~/.local/bin is removed when empty', 'uninstall_keeps_what_the_script_did_not_write',
     'for d in "$conf_dir" "$bin_dir/cctg-workers" "$bin_dir" "$root"; do',
     'for d in "$conf_dir" "$bin_dir/cctg-workers" "$bin_dir" "$root" "$wrap_dir"; do'),
    ('an unedited compose.yml is not updated', 'a_hub_is_set_up_with_docker_compose',
     '            || [ "$(sha256 "$hub_dir/$f")" = "$(cat "$hub_dir/$f.installed-sha256" 2>/dev/null)" ]; then',
     '            ; then'),
    ('the client line ignores the published ports', 'a_hub_is_set_up_with_docker_compose',
     '    if [ "$agent_port:$hook_port" = "$AGENT_PORT:$HOOK_PORT" ]; then', '    if true; then'),
    ('compose runs latest', 'a_hub_is_set_up_with_docker_compose',
     '        "CCTG_IMAGE_TAG=${RELEASE#v}" \\\n', ''),
    ('no --public-host without a terminal prints a placeholder', 'a_hub_that_does_not_start_or_has_no_address_is_an_error',
     '        interactive || die "no address for the devices: use --public-host HOST (this server\'s name or IP)"\n', ''),
    ('no proxy is not remembered', 'a_hub_that_does_not_start_or_has_no_address_is_an_error',
     '        [ -n "$proxy$(line_of "$hub_env" HTTPS_PROXY)" ] || printf \'%s\\n\' "$PROXY_NONE"\n', ''),
    ('a failed hub start is not an error', 'a_hub_that_does_not_start_or_has_no_address_is_an_error',
     '            *"Error: "*) break ;;', '            *"Error: "*) return 0 ;;'),
]

original = open(SCRIPT, 'rb').read()
text = original.decode('utf-8')
try:
    for name, test, old, new in MUTATIONS:
        assert text.count(old) == 1, name
        open(SCRIPT, 'wb').write(text.replace(old, new).encode('utf-8'))
        result = subprocess.run(
            ['cargo', 'test', '-j', '1', '-p', 'cctg', '--locked', '--test', 'install_e2e', '--', '--exact', test],
            env=ENV, capture_output=True, text=True, encoding='utf-8', errors='replace')
        verdict = 'KILLED' if result.returncode != 0 else 'SURVIVED'
        print(f'{verdict}: {name} ({test})', flush=True)
finally:
    open(SCRIPT, 'wb').write(original)
