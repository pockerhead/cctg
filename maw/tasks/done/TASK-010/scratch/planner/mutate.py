import subprocess, os, sys
WS = os.path.join(os.path.dirname(os.path.abspath(__file__)), 'ws')
env = dict(os.environ, CARGO_TARGET_DIR=os.path.join(os.environ['TEMP'], 'cctg-task010-planner-target'))
M = [
 ("dedup disabled", "crates/cctg/src/hub/ingress.rs", "if dedup.contains(&post.event_id, now) {", "if false && dedup.contains(&post.event_id, now) {", ["--lib", "hub::ingress::tests::a_repeated_post"]),
 ("dedup keyed by session", "crates/cctg/src/wire.rs", "            event_id: EventId::new(),\n            host,", "            event_id: EventId::try_from(format!(\"{:0>32}\", \"5e551017\")).unwrap(),\n            host,", ["--lib", "hub::ingress::tests::"]),
 ("insert before try_send", "crates/cctg/src/hub/ingress.rs", "    match events.try_send(post) {", "    dedup.insert(id.clone(), now);\n    match events.try_send(post) {", ["--lib", "hub::ingress::tests::a_full_queue"]),
 ("hook body logged", "crates/cctg/src/hub/ingress.rs", "debug!(%error, \"hook body rejected\");", "debug!(%error, body = %String::from_utf8_lossy(body), \"hook body rejected\");", ["--test", "ingress_logs"]),
 ("secret compare skipped", "crates/cctg/src/hub/ingress.rs", "Ok(AgentMsg::Hello { secret: offered }) => secret.matches(offered.expose().as_bytes()),", "Ok(AgentMsg::Hello { secret: offered }) => { let _ = offered; true }", ["--lib", "hub::ingress::tests::a_wrong_secret"]),
 ("no backoff", "crates/cctg/src/agent.rs", "tokio::time::sleep(config.backoff.delay(attempt)).await;", "tokio::task::yield_now().await;", ["--lib", "agent::tests::the_agent_reconnects"]),
 ("no re-register", "crates/cctg/src/agent.rs", "        if events.is_closed() {\n            return;\n        }", "        if true {\n            return;\n        }", ["--lib", "agent::tests::the_agent_reconnects"]),
 ("TE allowed", "crates/cctg/src/hub/ingress.rs", "        } else if name.eq_ignore_ascii_case(\"transfer-encoding\") {\n            return Err(Status::BadRequest);", "        } else if name.eq_ignore_ascii_case(\"transfer-encoding\") {", ["--lib", "hub::ingress::tests::bad_requests"]),
 ("non-loopback default", "crates/cctg/src/hub/config.rs", "SocketAddr::V4(SocketAddrV4::new(Ipv4Addr::LOCALHOST, 47291));", "SocketAddr::V4(SocketAddrV4::new(Ipv4Addr::UNSPECIFIED, 47291));", ["--lib", "hub::config::tests::listeners_default"]),
 ("unknown kind as malformed", "crates/cctg/src/wire.rs", "        Some(_) => Err(WireError::UnknownKind),", "        Some(_) => Err(WireError::Malformed),", ["--lib", "wire::tests::"]),
]
out = []
for name, path, a, b, args in M:
    p = os.path.join(WS, path)
    src = open(p, encoding='utf-8').read()
    assert src.count(a) == 1, name
    open(p, 'w', encoding='utf-8', newline='\n').write(src.replace(a, b))
    try:
        r = subprocess.run(['cargo', 'test', '-p', 'cctg', '--offline'] + args, cwd=WS, env=env, capture_output=True, text=True, timeout=300)
        compiled = 'error[E' not in r.stderr
        verdict = 'KILLED' if r.returncode != 0 and compiled else ('COMPILE-ERROR' if not compiled else 'SURVIVED')
    except subprocess.TimeoutExpired:
        verdict = 'KILLED (timeout)'
    finally:
        open(p, 'w', encoding='utf-8', newline='\n').write(src)
    out.append(f'{verdict}: {name}')
    print(out[-1], flush=True)
open(os.path.join(os.path.dirname(os.path.abspath(__file__)), 'mutations.out.txt'), 'w', encoding='utf-8', newline='\n').write('\n'.join(out) + '\n')
