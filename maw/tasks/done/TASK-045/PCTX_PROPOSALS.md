# PCTX proposals from TASK-045 (planner)

## 2026-09-26 — hub domain, agent transport invariant

The hub domain says "Agent transport: TCP, newline-delimited JSON, first message is shared-secret auth". After TASK-045 the first message is still `hello{secret}`, but the secret is either the shared `CCTG_HUB_SECRET` (while `CCTG_SHARED_SECRET` is on) or a per-device secret `cctgd_<id>_<64 hex>` from `cctg join`; the hub keeps only its sha256 in `<state>/devices.json`. Proposed wording: "first message is `hello` with the shared secret or the device's own secret (TASK-045, `hub/devices.rs`); a revoked device's links close at once". Also worth a line: `POST /v1/join` is the only hook-listener route without `Authorization` (body <= 1 KiB, 403 after 250 ms for any bad code).

## 2026-09-26 — risk lesson: stale test binaries from the shared target

Seen again in planning: a `cargo test` run right after editing a %TEMP% reference tree reported green from a test binary built from another tree (same metadata hash, the new tests were not in it). Always `touch crates/cctg/src/lib.rs` before a test run and check that the new test names appear in the output, not only `test result: ok`. The orchestrator note already says the first half; the second half (look for the test names) is what catches it.

## 2026-09-26 plan-reviewer-2 (TASK-045)

- Universal invariants, shared cargo target: touching `crates/cctg/src/lib.rs` and `main.rs` is not enough when a %TEMP% reference workspace and the main tree build the same package into one `CARGO_TARGET_DIR`. Integration test binaries get the same file names (e.g. `install_e2e-f992769144e721bf.exe`) and the same fingerprints, whose dep-info points at the other tree's sources, so cargo can run the other tree's test binary as "fresh". Seen: `install_e2e` ran 10 tests without the new `a_device_joins_with_a_code` until `tests/install_e2e.rs` was touched. Rule: touch every changed file under `crates/cctg/tests/` too, and check that the new test names appear in the output.
- Hub domain, pre-auth limits (TASK-035): a request answered after `AUTH_FAIL_DELAY` must keep its hook request place (the `permit`) during the pause. Dropping it before the sleep (as a waiting permission hook does) lets unauthenticated requests go past the 64-place limit. Probe: `scratch/plan-reviewer-2/join_place_probe.rs.txt`.
