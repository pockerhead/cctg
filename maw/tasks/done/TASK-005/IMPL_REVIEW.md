# IMPL_REVIEW — TASK-005: transcript, tolerant JSONL parser

Stage: code-reviewer (claude/opus, effort=medium). Inputs read from disk: `TASK_FINAL.md`, `PLAN_FINAL.md`, `IMPL_SUMMARY.md`, `log.jsonl`, `git diff main -- crates/` (all 9 files read fully).

## 1. Verdict

**PASS** — the code is byte-identical to the verified reference in PLAN_FINAL, every acceptance criterion is covered by a passing test, and a full run over all 601 real transcripts on this machine loses no visible turn and leaks no thinking.

## Disconfirmation (done first)

Counter-example looked for: "a real `user`/`assistant` record with at least one visible block (string content, or an array item of type `text`/`tool_use`/`tool_result`) that `parse` drops, or a thinking text that shows up in `parse` output".

Harness: `scratch/crev_harness/` (throwaway crate, path dependency on `crates/transcript`, target dir outside the repo; it prints counts and file names only, no content). For every `*.jsonl` under `~/.claude/projects` (sessions and `subagents/agent-*.jsonl`) it counts expected turns and visible blocks with an independent `serde_json::Value` walk and compares them to `transcript::parse`. It also takes the first 40 chars of every thinking block longer than 40 chars and searches the `Debug` of the parsed turns for them.

Result:
- `files=601 expected_turns=95761 got_turns=95761 mismatching_files=0 bad_json_lines=0`; visible block counts match per file too.
- Block kinds seen: `assistant:text 8848`, `assistant:tool_use 41626`, `assistant:thinking 22919`, `user:tool_result 41624`, `user:text 278`, `user:image 15`, `assistant:fallback 1`. `fallback` has only `from`/`to` objects (model-fallback marker, no text), so dropping it is correct.
- One thinking prefix hit, in `c--Users-user-dev-formic/9c9651f2….jsonl`. Traced: it is a plain user prompt (string content, not meta) where the user pasted text that starts like an earlier thinking block. It is user input, not a thinking block passing through the parser. No leak.
- `toolUseResult.agentId` appears only on results of the `Agent` tool (527 of 527), so `ToolResult.agent_id` is not attached to unrelated tools in practice.

The counter-example did not hold.

## 2. Confirmed correct

- Code matches the plan exactly: `diff` of `crates/transcript/{src/lib.rs,tests/parse_fixtures.rs,tests/parse_tolerance.rs,tests/purity.rs}` against `scratch/rev2_crate/` is empty (CR-stripped); `git diff --no-index` of fixtures against `scratch/fixtures` exits 0. No other file under `crates/` changed; `Cargo.toml` files untouched.
- Commands rerun by this reviewer (target dir in scratchpad): `cargo fmt --all -- --check` clean; `cargo clippy --workspace --all-targets -- -D warnings` clean; `cargo test --workspace` passes: cctg 1+1, `parse_fixtures` 8, `parse_tolerance` 14, `purity` 2. `cargo tree -p transcript -e normal --depth 1` shows only `serde`, `serde_json`. The summary's claims hold.
- Allowlist of record types: `to_turn` returns `None` for anything but `user`/`assistant` (`crates/transcript/src/lib.rs:134-139`); `ai-title` is handled only by the separate `ai_title` (`lib.rs:117-124`) over a separate `RawTitleRecord`, first non-empty title wins.
- Thinking cannot leave the crate: public `Block` has no thinking variant (`lib.rs:19-38`), `RawBlock` maps `thinking`/`redacted_thinking`/unknown tags to `Ignored` (`lib.rs:105-106`), and thinking-only records give no turn (`lib.rs:152-154`). Test `thinking_never_escapes` checks both markers against every fixture.
- Tolerance: per-line `from_str(...).ok()` (`lib.rs:126-132`) and per-item `from_value(...).ok()` (`lib.rs:164`) mean a bad line or block drops only itself. Soft fields are `Value` so wrong types do not kill a record. Covered by tolerance tests 2-5, 7-10, 13-14.
- Both content shapes for both roles (`lib.rs:144-151`), `isMeta` and `isSidechain` carried as tolerant flags (`lib.rs:158-159`). Tests `both_content_shapes_for_both_roles`, `flags_are_tolerant`, `string_content_fixture` (exact `"Привет, add a test for empty input."`).
- No IO and no unwrap/expect/panic: `#![cfg_attr(not(test), deny(clippy::unwrap_used, clippy::expect_used, clippy::panic))]` (`lib.rs:2-5`) plus the grep guard in `tests/purity.rs`. Only `.unwrap_or(false)` on `Option<bool>` is used, which is fine.
- Recursion: serde_json's default depth limit 128 is kept; test 13 shows a 100_000-deep record is skipped with neighbours intact. The real-data run found zero lines over the limit.
- Fixtures: read all 29 lines. All strings are `redacted` or synthetic, UUIDs are zeroed, the only path is `C:\work\demo`, ids are `toolu_demo*`/`msg_demo*`/`a0000000000000001`. Grep for `@`, the user's name, e-mail, hostname, real `toolu_`/`msg_`/`req_` ids found nothing. The privacy test catches escaped and unescaped Windows home paths and has a non-vacuous self-test.

## 3. Issues

No critical or major issues.

- **minor** `crates/transcript/src/lib.rs:126-132` — a UTF-8 BOM before the first line (`\u{FEFF}` is not trimmed by `str::trim`) drops that line silently. Claude Code does not write a BOM and PLAN_FINAL lists this as an accepted quirk; `garbage_never_panics` only checks for no panic. Suggested fix (optional, later): `line.trim_start_matches('\u{FEFF}')` in `records`. Not blocking.
- **minor** `crates/transcript/src/lib.rs:144-151` — `user` records with image blocks: the image is dropped without a trace, and an image-only user message produces no turn (15 image blocks in real data). This matches the spec (only `text|tool_use|tool_result` are visible), but TASK-006 will not be able to show "[image]". Suggested fix: decide in TASK-006 whether a placeholder block is wanted; nothing to do here.
- **minor** `crates/transcript/src/lib.rs:81-107` — a known block with a `null` field (e.g. `{"type":"text","text":null}`) is dropped instead of taking the default, because `#[serde(default)]` only covers a missing key. Not seen in 95k real records. No change needed now.

## 4. Missing coverage

- No test runs the parser against a real-sized mixed transcript for a turn-count invariant. The reviewer harness did that once (`scratch/crev_harness/`), but it reads `~/.claude` and cannot live in the repo. Acceptable; noted for TASK-006, where render tests will exercise larger fixtures.
- No test for a record with two `tool_result` blocks sharing one `toolUseResult.agentId` (behaviour documented in PLAN_FINAL rollout notes; real records carry one block each).

## 5. Nits

- `crates/transcript/tests/parse_fixtures.rs:225` — `assert!("c:\\users\\someone".contains("users\\"))` tests `str::contains`, not the privacy check. The real check is inline in `fixtures_have_no_private_data`; harmless but tautological.
- `crates/transcript/tests/purity.rs` — the forbidden token `unsafe` also matches prose in doc comments; the PLAN rule already warns about this. Fine as is.
