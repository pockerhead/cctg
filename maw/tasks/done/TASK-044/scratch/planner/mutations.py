# Each mutation breaks one rule of the reference; the named lib test must
# fail. Usage: python mutations.py <reference workspace root>
# Runs cargo with the shared target dir (CARGO_TARGET_DIR from the env).
import subprocess, sys, os

ws = sys.argv[1]
MUTATIONS = [
    ('crates/cctg/src/keys.rs',
     '            terminal.write(&"\\u{8}".repeat(text.chars().count()));',
     '            let _ = text;',
     'keys::tests::a_line_goes_in_only_into_an_empty_box'),
    ('crates/cctg/src/keys.rs',
     '            if text.is_some() {\n                terminal.write("\\u{1b}");\n            }',
     '',
     'keys::tests::a_panel_is_read_and_closed'),
    ('crates/cctg/src/keys.rs',
     '        if !terminal.lines().is_some_and(|screen| exit_dialog(&screen)) {\n            return Typed::Agents;',
     '        if !terminal.lines().is_some_and(|screen| exit_dialog(&screen)) {\n            return Typed::Sent;',
     'keys::tests::the_background_work_dialog_of_exit_is_cancelled'),
    ('crates/cctg/src/term.rs',
     'Ask::Keys(text) => serde_json::to_vec(&write(text.as_bytes()))?,',
     'Ask::Keys(_) => serde_json::to_vec(&true)?,',
     'term::tests::asks_are_answered_one_line_each'),
    ('crates/cctg/src/proctree.rs',
     'dir.as_os_str() == "versions"',
     'dir.as_os_str() == "version"',
     'proctree::tests::a_native_claude_on_macos_is_named_claude'),
]
failed = 0
for path, old, new, test in MUTATIONS:
    full = os.path.join(ws, path)
    src = open(full, encoding='utf-8').read()
    assert src.count(old) == 1, (path, old)
    open(full, 'w', encoding='utf-8', newline='\n').write(src.replace(old, new))
    try:
        r = subprocess.run(['cargo', 'test', '-j', '1', '-p', 'cctg', '--lib', '--', test],
                           cwd=ws, capture_output=True, text=True, encoding='utf-8', errors='replace')
        caught = r.returncode != 0
        print(('CAUGHT ' if caught else 'MISSED ') + test)
        failed += 0 if caught else 1
    finally:
        open(full, 'w', encoding='utf-8', newline='\n').write(src)
sys.exit(failed)
