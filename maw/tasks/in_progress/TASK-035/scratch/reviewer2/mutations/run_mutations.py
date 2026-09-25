"""Mutation checks of the TASK-035 reference (or of the implementation): the
planner's 12 and 7 from the plan review (reviewer2).

    python run_mutations.py <workspace root> [name ...]

Each mutation replaces one exact text in one file, runs the named tests and
expects at least one of them to FAIL; the file is restored afterwards. A
mutation whose tests still pass means the tests do not guard that line.
Build: the shared target dir, -j 1 (project rule).
"""
import os
import subprocess
import sys
from pathlib import Path

ENV = dict(os.environ, CARGO_TARGET_DIR="C:/Users/user/dev/cctg/target", CARGO_PROFILE_DEV_DEBUG="0")

MUTATIONS = [
    {
        "name": "pin-accepts-any-certificate",
        "file": "crates/cctg/src/tls.rs",
        "from": "        if self.pin.matches(end_entity.as_ref()) {",
        "to": "        if true || self.pin.matches(end_entity.as_ref()) {",
        "tests": ["--lib", "--", "tls::"],
    },
    {
        "name": "tls13-signature-not-checked",
        "file": "crates/cctg/src/tls.rs",
        "from": """        rustls::crypto::verify_tls13_signature(
            message,
            cert,
            dss,
            &self.provider.signature_verification_algorithms,
        )""",
        "to": """        let _ = (message, cert, dss);
        Ok(HandshakeSignatureValid::assertion())""",
        "tests": ["--lib", "--", "tls::"],
    },
    {
        "name": "plain-to-any-address",
        "file": "crates/cctg/src/device.rs",
        "from": "            None if tls::is_loopback_addr(addr) => Ok(HubAddr::plain(addr)),",
        "to": "            None => Ok(HubAddr::plain(addr)),",
        "tests": ["--lib", "--", "device::", "agent::tests::link_plan"],
    },
    {
        "name": "broken-pin-falls-back-to-plain",
        "file": "crates/cctg/src/device.rs",
        "from": "            Some(Err(problem)) => Err(*problem),",
        "to": "            Some(Err(_)) => Ok(HubAddr::plain(addr)),",
        "tests": ["--lib", "--", "device::"],
    },
    {
        "name": "hook-ignores-tls",
        "file": "crates/cctg/src/tls.rs",
        "from": "        match &self.tls {\n            None => Ok(Stream::Plain(tcp)),",
        "to": "        match &None::<(TlsConnector, ServerName<'static>)> {\n            None => Ok(Stream::Plain(tcp)),",
        "tests": ["--test", "tls_e2e"],
    },
    {
        "name": "clean-commit-hashes-the-file",
        "file": "crates/cctg/src/client.rs",
        "from": "    } else {\n        Some(source.to_owned())\n    }",
        "to": "    } else {\n        file_hash().or_else(|| Some(source.to_owned()))\n    }",
        "tests": ["--lib", "--", "client::", "hub::slots::tests::one_commit"],
    },
    {
        "name": "dirty-build-ignores-the-file",
        "file": "crates/cctg/src/client.rs",
        "from": "        file_hash().map(|hash| format!(\"{source}.{hash}\"))",
        "to": "        Some(source.to_owned())",
        "tests": ["--lib", "--", "client::"],
    },
    {
        "name": "reader-task-survives-a-cancelled-connection",
        "file": "crates/cctg/src/tls.rs",
        "from": "impl Drop for ReadTask {\n    fn drop(&mut self) {\n        self.0.abort();",
        "to": "impl Drop for ReadTask {\n    fn drop(&mut self) {\n        let _ = &self.0;",
        "tests": ["--lib", "--", "agent::tests::the_agent_reconnects"],
    },
    {
        "name": "tls-files-error-quotes-nothing-check",
        "file": "crates/cctg/src/hub/config.rs",
        "from": "            _ => return Err(ConfigError::TlsPair),",
        "to": "            _ => None,",
        "tests": ["--lib", "--", "hub::config::"],
    },
    {
        "name": "probe-logs-a-warning",
        "file": "crates/cctg/src/hub/ingress.rs",
        "from": "            Err(WireError::Closed) if line.is_empty() => return Ok(None),",
        "to": "            Err(WireError::Closed) if line.is_empty() => return Err(Rejection::Protocol),",
        "tests": ["--test", "ingress_logs"],
    },
    {
        "name": "bot-api-ignores-the-proxy",
        "file": "crates/cctg/src/hub/api.rs",
        "from": "            .connect_timeout(Duration::from_secs(10))\n            .build()",
        "to": "            .connect_timeout(Duration::from_secs(10))\n            .no_proxy()\n            .build()",
        "tests": ["--test", "proxy_e2e"],
    },
    {
        "name": "proxy-url-logged",
        "file": "crates/cctg/src/hub/mod.rs",
        "from": '        info!("Bot API requests go through the proxy of the environment");',
        "to": '        info!(proxy = %std::env::var("HTTPS_PROXY").or_else(|_| std::env::var("HTTP_PROXY")).unwrap_or_default(), "Bot API requests go through the proxy of the environment");',
        "tests": ["--test", "proxy_e2e"],
    },
    # ---- reviewer2 (TASK-035 plan review)
    {
        "name": "hook-post-without-flush",
        "file": "crates/cctg/src/hook.rs",
        "from": "        stream.flush().await.map_err(io)?;\n        let mut status_line",
        "to": "        let mut status_line",
        "tests": ["--lib", "--", "hook::tests::a_post_over_tls"],
    },
    {
        "name": "no-cap-on-unauthenticated-agents",
        "file": "crates/cctg/src/hub/ingress.rs",
        "from": "const MAX_PENDING_AGENTS: usize = 16;",
        "to": "const MAX_PENDING_AGENTS: usize = 100_000;",
        "tests": ["--lib", "--", "hub::ingress::tests::unauthenticated_agents"],
    },
    {
        "name": "agent-deadline-restarts-after-tls",
        "file": "crates/cctg/src/hub/ingress.rs",
        "from": "    let handshake = tokio::time::timeout_at(pre_auth.deadline, async {",
        "to": "    let handshake = tokio::time::timeout_at(tokio::time::Instant::now() + HANDSHAKE_TIMEOUT, async {",
        "tests": ["--lib", "--", "hub::ingress::tests::the_tls_handshake_counts"],
    },
    {
        "name": "hook-deadline-restarts-after-tls",
        "file": "crates/cctg/src/hub/ingress.rs",
        "from": "    let status = match tokio::time::timeout_at(deadline, read_request(&mut stream, secret)).await {",
        "to": "    let status = match tokio::time::timeout(REQUEST_TIMEOUT, read_request(&mut stream, secret)).await {",
        "tests": ["--lib", "--", "hub::ingress::tests::the_tls_handshake_counts"],
    },
    {
        "name": "hello-read-up-to-max-line",
        "file": "crates/cctg/src/hub/ingress.rs",
        "from": "        match wire::read_line_max(&mut reader, &mut line, MAX_HELLO_LINE).await {",
        "to": "        match wire::read_line(&mut reader, &mut line).await {",
        "tests": ["--lib", "--", "hub::ingress::tests::a_long_first_line"],
    },
    {
        "name": "agent-wrong-secret-without-pause",
        "file": "crates/cctg/src/hub/ingress.rs",
        "from": "                // No fast guessing; the place stays taken meanwhile.\n                tokio::time::sleep(AUTH_FAIL_DELAY).await;",
        "to": "                // No pause.",
        "tests": ["--lib", "--", "hub::ingress::tests::a_wrong_secret_is_answered"],
    },
    {
        "name": "hook-wrong-secret-without-pause",
        "file": "crates/cctg/src/hub/ingress.rs",
        "from": "        // No fast guessing; the request place stays taken meanwhile.\n        tokio::time::sleep(AUTH_FAIL_DELAY).await;",
        "to": "        // No pause.",
        "tests": ["--lib", "--", "hub::ingress::tests::a_wrong_secret_is_answered"],
    },
]


def run_tests(root, args):
    cmd = ["cargo", "test", "-j", "1", "-p", "cctg", "--locked"] + args
    out = subprocess.run(cmd, cwd=root, env=ENV, capture_output=True, text=True)
    return out.returncode, out.stdout[-3000:] + out.stderr[-3000:]


def main():
    root = Path(sys.argv[1])
    wanted = set(sys.argv[2:])
    survived = []
    for m in MUTATIONS:
        if wanted and m["name"] not in wanted:
            continue
        path = root / m["file"]
        original = path.read_text(encoding="utf-8")
        assert original.count(m["from"]) == 1, (m["name"], "anchor not found once")
        path.write_text(original.replace(m["from"], m["to"]), encoding="utf-8", newline="\n")
        try:
            code, tail = run_tests(root, m["tests"])
        finally:
            path.write_text(original, encoding="utf-8", newline="\n")
        verdict = "KILLED" if code != 0 else "SURVIVED"
        print(f"{m['name']}: {verdict}", flush=True)
        if code == 0:
            survived.append(m["name"])
    print("survived:", survived or "none")
    sys.exit(1 if survived else 0)


if __name__ == "__main__":
    main()
