# FIX SUMMARY — TASK-005

## Fixed

- Review minor: a known block with a JSON `null` in a string field was dropped wholesale. Added a null-tolerant string deserializer for `text`, tool-use `id`/`name`, and `tool_use_id`. Missing and null values now become an empty string; other wrong types still fail only that item. Added the anonymized `null_fields.jsonl` fixture and an exact parser test covering all four fields.
- Review nit: `privacy_detectors_fire` used a tautological direct `str::contains` assertion. Extracted `has_private_path`, used it in the fixture scan, and made the self-test call the same detector.
- Review nit: the purity guard matched the token `unsafe` in doc-comment prose. The test now removes line comments and checks `unsafe` as a Rust identifier in the remaining source.

## Skipped

- BOM-prefixed first line: unchanged. This is an accepted quirk in `PLAN_FINAL.md`, and the orchestrator explicitly directed the fixer to record it rather than change it.
- `image` blocks: unchanged. Dropping them is the current allowlist behavior; an image placeholder belongs to TASK-006 renderer scope. Adding a public `Block` variant here would expand the API and break exhaustive downstream matches.
- Real-sized mixed-transcript regression fixture: not added. The review reported no defect, and a test reading live `~/.claude` data would violate the hermetic fixture boundary. The existing anonymized fixtures remain the repository coverage.
- Two `tool_result` blocks sharing one record-level `toolUseResult.agentId`: no test added. The current record-level behavior is documented, real records contain one such block, and the review identified no incorrect implementation to fix.

## Test results

- `cargo fmt --all -- --check` — exit 0, no output.
- `cargo clippy --workspace --all-targets -- -D warnings` — exit 0; `Finished dev profile`.
- `cargo test --workspace` — exit 0. Results: cctg unit 1 passed; stdout integration 1 passed; transcript fixtures 9 passed; transcript tolerance 14 passed; purity 2 passed; doc-tests 0 failed.
- `git diff --check` — exit 0; only line-ending warnings from the configured Windows checkout.
