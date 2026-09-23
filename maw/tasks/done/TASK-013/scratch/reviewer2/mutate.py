#!/usr/bin/env python3
# Mutation check for reviewer2/ws: each mutant must make at least one test
# fail. Sequential (one cargo at a time); the source is always restored.
# Env: CARGO_TARGET_DIR and CARGO_PROFILE_DEV_DEBUG=0 set by the caller.
import os, subprocess, sys

here = os.path.dirname(os.path.abspath(__file__))
ws = os.path.join(here, 'ws')
src = os.path.join(ws, 'crates', 'cctg', 'src')

LIB = ['cargo', 'test', '-p', 'cctg', '--offline', '-j', '1', '--lib']
STDIO = ['cargo', 'test', '-p', 'cctg', '--offline', '-j', '1', '--test', 'agent_stdio']

MUTANTS = [
    # Planner mutations, re-run on the fixed reference.
    ('M1_no_follow_pid', 'hub/slots.rs',
     'self.follow_pid(&post.host, pid);', 'let _ = pid;', [LIB + ['slots::']]),
    ('M2_no_pid_fallback_on_register', 'hub/slots.rs',
     'if !self.registry.is_live_top_level(&register.session_id)',
     'if false && !self.registry.is_live_top_level(&register.session_id)', [LIB + ['slots::']]),
    ('M3_no_headless_skip', 'agent.rs',
     'if entrypoint == Some("sdk-cli") {', 'if entrypoint == Some("never") {',
     [LIB + ['agent::'], STDIO]),
    ('M4_meta_unfiltered', 'channel.rs',
     '.filter(|(key, _)| is_meta_key(key))', '.filter(|_| true)', [LIB + ['channel::']]),
    ('M5_nested_bound', 'hub/registry.rs',
     'Some(entry) if entry.kind == SessionKind::TopLevel => {', 'Some(entry) => {',
     [LIB + ['registry::']]),
    # Reviewer-2 fixes.
    ('N1_no_ping', 'channel.rs',
     '            "ping" => result(id, json!({})),\n', '', [LIB + ['channel::'], STDIO]),
    ('N2_no_jsonrpc_check', 'channel.rs',
     'if msg.get("jsonrpc").and_then(Value::as_str) != Some("2.0") {', 'if false {',
     [LIB + ['channel::'], STDIO]),
    ('N3_jsonrpc_error_echoes_id', 'channel.rs',
     '            return vec![error(&Value::Null, INVALID_REQUEST, "Invalid Request")];\n        }\n        let id',
     '            return vec![error(msg.get("id").unwrap_or(&Value::Null), INVALID_REQUEST, "Invalid Request")];\n        }\n        let id',
     [LIB + ['channel::'], STDIO]),
    ('N4_scalar_params_accepted', 'channel.rs',
     'let structured = params.is_none_or(|params| params.is_object() || params.is_array());',
     'let structured = true;', [LIB + ['channel::'], STDIO]),
    ('N5_unknown_version_echoed', 'channel.rs',
     'let version = if SUPPORTED_PROTOCOLS.contains(&asked) {', 'let version = if true {',
     [LIB + ['channel::']]),
    ('N6_missing_version_falls_back', 'channel.rs',
     '.filter(|version| !version.is_empty())\n                else {',
     '.or(Some(LATEST_PROTOCOL))\n                else {', [LIB + ['channel::']]),
    ('N7_duplicate_permission_relayed', 'channel.rs',
     'if self.open_permissions.contains(&request_id) {', 'if false {', [LIB + ['channel::']]),
    # /clear hooks out of order (reviewer-2 test), aimed at that test only.
    ('C1_start_first_no_follow_pid', 'hub/slots.rs',
     'self.follow_pid(&post.host, pid);', 'let _ = pid;',
     [LIB + ['slots::tests::the_agent_follows_its_claude_process_when_the_new_start_comes_first']]),
    ('C2_start_first_no_pid_fallback', 'hub/slots.rs',
     'if !self.registry.is_live_top_level(&register.session_id)',
     'if false && !self.registry.is_live_top_level(&register.session_id)',
     [LIB + ['slots::tests::the_agent_follows_its_claude_process_when_the_new_start_comes_first']]),
    ('C3_late_end_clears_new_pid', 'hub/registry.rs',
     'if self.pids.get(&key).map(String::as_str) == Some(session) {', 'if true {',
     [LIB + ['slots::tests::the_agent_follows_its_claude_process_when_the_new_start_comes_first']]),
]
ONLY = sys.argv[1:]


def run(cmd):
    r = subprocess.run(cmd, cwd=ws, capture_output=True, text=True, encoding='utf-8', errors='replace')
    tail = [l for l in (r.stdout + r.stderr).splitlines() if l.startswith('test result') or 'error[' in l]
    return r.returncode, ' | '.join(tail[-2:])


survivors = 0
for name, rel, old, new, cmds in MUTANTS:
    if ONLY and not any(name.startswith(prefix) for prefix in ONLY):
        continue
    path = os.path.join(src, rel)
    orig = open(path, encoding='utf-8').read()
    if orig.count(old) != 1:
        print('%s: PATTERN NOT UNIQUE (%d)' % (name, orig.count(old)), flush=True)
        survivors += 1
        continue
    open(path, 'w', encoding='utf-8', newline='\n').write(orig.replace(old, new))
    try:
        killed, notes = False, []
        for cmd in cmds:
            code, tail = run(cmd)
            notes.append(tail)
            if code != 0:
                killed = True
                break
        print('%s: %s  %s' % (name, 'KILLED' if killed else 'SURVIVED', ' || '.join(notes)), flush=True)
        survivors += 0 if killed else 1
    finally:
        open(path, 'w', encoding='utf-8', newline='\n').write(orig)
sys.exit(1 if survivors else 0)
