# PCTX_PROPOSALS — TASK-035 (planner)

2026-09-25, planner. Proposals only; the project context is not edited here.

1. hub.md, Invariants, "Agent transport": add "Optionally TLS 1.3 on both listeners (`CCTG_TLS_CERT` + `CCTG_TLS_KEY`, rustls/aws-lc-rs, `crate::tls`); devices pin the certificate's sha256 (`CCTG_HUB_CERT_SHA256`) and talk plain TCP only to loopback (TASK-035)." Why: the transport invariant says plain TCP only; after TASK-035 that is the loopback case.
2. hub.md / channel.md: build identity. "An agent's `Client.build` and the hub's own build are the source commit baked by `build.rs` (`CCTG_BUILD_ID` in CI/Docker, else git; `-dirty` adds the exe sha256; no git = exe sha256). Outdated = builds differ. The agent's local newer-file check stays on the file hash." Why: TASK-040's "sha256 of the executable" text is no longer true.
3. hooks.md, Invariants: "Over TLS the hook budgets are 900 ms (600 ms prompt/tool events, 1000 ms permission connect); plain keeps 500/300/500. The status line keeps 80 ms." Why: the SessionEnd budget reasoning now has two cases.
4. Risk lesson (hooks/channel): "dotenvy rejects an unquoted value with a space: an unquoted `sha256 Fingerprint=AB:..` line breaks the whole device.env (no secret). Paste the hex only or quote it." Evidence: first tls_e2e run.
5. Risk lesson (hub/channel): "`tokio::io::split` halves keep the socket open while either half lives; a reader task spawned per connection must be aborted on drop (`tls::ReadTask`), or a cancelled connection task (a stopped `serve_agents`) never closes the link." Evidence: two agent tests failed until the guard.
6. Risk lesson (general): "Tests that copy the cctg binary must make the copy executable on Unix (`tests/common::write_program`); `std::fs::write` gives no execute bit." Evidence: first Linux run (supervise_e2e, update_e2e: PermissionDenied).
7. README invariant on builds: "Linux checks run in CI (ubuntu-latest) or, when Docker is available, in a `rust:1.95-bookworm` container with its own named target volume (it cannot share the Windows target dir)." Why: the shared-target rule has no Linux case.
