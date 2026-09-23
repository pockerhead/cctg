# Re-runs the planner's 10 mutations against the fixed reference, plus one
# mutation per reviewer fix (each one restores the defective planner code).
import subprocess, os
HERE = os.path.dirname(os.path.abspath(__file__))
WS = os.path.join(HERE, 'ws')
env = dict(os.environ, CARGO_TARGET_DIR=os.path.join(os.environ['LOCALAPPDATA'], 'Temp', 'cctg-task010-reviewer2-target'))
ING = "crates/cctg/src/hub/ingress.rs"
M = [
 # planner mutations (anchors unchanged)
 ("P1 dedup disabled", ING, "if dedup.contains(&post.event_id, now) {", "if false && dedup.contains(&post.event_id, now) {", ["--lib", "hub::ingress::tests::a_repeated_post"]),
 ("P2 dedup keyed by session", "crates/cctg/src/wire.rs", "            event_id: EventId::new(),\n            host,", "            event_id: EventId::try_from(format!(\"{:0>32}\", \"5e551017\")).unwrap(),\n            host,", ["--lib", "hub::ingress::tests::"]),
 ("P3 insert before try_send", ING, "    match events.try_send(post) {", "    dedup.insert(id.clone(), now);\n    match events.try_send(post) {", ["--lib", "hub::ingress::tests::a_full_queue"]),
 ("P4 hook body logged", ING, "debug!(%error, \"hook body rejected\");", "debug!(%error, body = %String::from_utf8_lossy(body), \"hook body rejected\");", ["--test", "ingress_logs"]),
 ("P5 secret compare skipped", ING, "Ok(AgentMsg::Hello { secret: offered }) => secret.matches(offered.expose().as_bytes()),", "Ok(AgentMsg::Hello { secret: offered }) => { let _ = offered; true }", ["--lib", "hub::ingress::tests::a_wrong_secret"]),
 ("P6 no backoff", "crates/cctg/src/agent.rs", "tokio::time::sleep(config.backoff.delay(attempt)).await;", "tokio::task::yield_now().await;", ["--lib", "agent::tests::the_agent_reconnects"]),
 ("P7 no re-register", "crates/cctg/src/agent.rs", "        if events.is_closed() {\n            return;\n        }", "        if true {\n            return;\n        }", ["--lib", "agent::tests::the_agent_reconnects"]),
 ("P8 TE allowed", ING, "        } else if name.eq_ignore_ascii_case(\"transfer-encoding\") {\n            return Err(Status::BadRequest);", "        } else if name.eq_ignore_ascii_case(\"transfer-encoding\") {", ["--lib", "hub::ingress::tests::bad_requests"]),
 ("P9 non-loopback default", "crates/cctg/src/hub/config.rs", "SocketAddr::V4(SocketAddrV4::new(Ipv4Addr::LOCALHOST, 47291));", "SocketAddr::V4(SocketAddrV4::new(Ipv4Addr::UNSPECIFIED, 47291));", ["--lib", "hub::config::tests::listeners_default"]),
 ("P10 unknown kind as malformed", "crates/cctg/src/wire.rs", "        Some(_) => Err(WireError::UnknownKind),", "        Some(_) => Err(WireError::Malformed),", ["--lib", "wire::tests::"]),
 # reviewer fixes reverted to the planner code = reproduction of each defect
 ("R1 HTTP/1.x accepted (planner version check)", ING, "version != \"HTTP/1.1\"", "!version.starts_with(\"HTTP/1.\")", ["--lib", "hub::ingress::tests::bad_requests"]),
 ("R2 64 KiB linger (planner drain size)", ING, "const LINGER_BYTES: usize = MAX_HEAD + MAX_HOOK_BODY;", "const LINGER_BYTES: usize = 64 * 1024;", ["--lib", "hub::ingress::tests::an_early_401"]),
 ("R3 field-name only rejects whitespace (planner)", ING, "!name.bytes().all(is_tchar)", "name.bytes().any(|byte| byte.is_ascii_whitespace())", ["--lib", "hub::ingress::tests::bad_requests"]),
 ("R4 CR/LF/NUL in field value accepted (planner)", ING, "            .any(|byte| byte.is_ascii_control() && byte != b'\\t')", "            .any(|byte| byte.is_ascii_control() && false)", ["--lib", "hub::ingress::tests::bad_requests"]),
 ("R5 lenient hook status line (planner parse_status)", "crates/cctg/src/hook.rs", "    let rest = line.strip_suffix(b\"\\r\\n\")?.strip_prefix(b\"HTTP/1.1 \")?;\n    let (code, tail) = rest.split_at_checked(3)?;\n    if !code.iter().all(u8::is_ascii_digit) || !(tail.is_empty() || tail[0] == b' ') {\n        return None;\n    }\n    std::str::from_utf8(code).ok()?.parse().ok()", "    let line = std::str::from_utf8(line).ok()?;\n    let mut parts = line.split(' ');\n    if !parts.next()?.starts_with(\"HTTP/1.\") {\n        return None;\n    }\n    parts.next()?.trim().parse().ok()", ["--lib", "hook::tests::"]),
 ("R6 body limit off by one", ING, "if length > MAX_HOOK_BODY {", "if length >= MAX_HOOK_BODY {", ["--lib", "hub::ingress::tests::a_body_of_exactly"]),
 ("R7 no request deadline (slowloris)", ING, "match tokio::time::timeout(REQUEST_TIMEOUT, read_request(&mut stream, secret)).await {", "match tokio::time::timeout(Duration::from_secs(3600), read_request(&mut stream, secret)).await {", ["--lib", "hub::ingress::tests::a_slow_request"]),
 ("R8 secret compare by length only", "crates/cctg/src/wire.rs", "    bool::from(a.ct_eq(b))", "    a.len() == b.len()", ["--lib", "wire::tests::secret_rules"]),
 ("R9 duplicate Content-Length accepted", ING, "if length.is_some() || value.is_empty()", "if value.is_empty()", ["--lib", "hub::ingress::tests::bad_requests"]),
]
out = []
for name, path, a, b, args in M:
    only = os.environ.get('MUT_ONLY')
    if only and not any(name.startswith(o) for o in only.split(',')):
        continue
    p = os.path.join(WS, path)
    src = open(p, encoding='utf-8', newline='').read()
    if src.count(a) != 1:
        out.append(f'ANCHOR-MISSING: {name}'); print(out[-1], flush=True); continue
    open(p, 'w', encoding='utf-8', newline='').write(src.replace(a, b))
    try:
        r = subprocess.run(['cargo', 'test', '-p', 'cctg', '--offline'] + args, cwd=WS, env=env, capture_output=True, text=True, timeout=600)
        compiled = 'error[E' not in r.stderr and 'error: could not compile' not in r.stderr
        verdict = 'KILLED' if r.returncode != 0 and compiled else ('COMPILE-ERROR' if not compiled else 'SURVIVED')
        failed = [l for l in r.stdout.splitlines() if l.endswith('FAILED') and l.startswith('test ')]
    except subprocess.TimeoutExpired:
        verdict, failed = 'KILLED (timeout)', []
    finally:
        open(p, 'w', encoding='utf-8', newline='').write(src)
    out.append(f'{verdict}: {name} {failed}')
    print(out[-1], flush=True)
open(os.path.join(HERE, os.environ.get('MUT_OUT', 'mutations.out.txt')), 'w', encoding='utf-8', newline='\n').write('\n'.join(out) + '\n')
