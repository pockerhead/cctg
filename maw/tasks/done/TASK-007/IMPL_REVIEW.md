# IMPL_REVIEW — TASK-007: transcript — subagent data and collapsed rendering

Stage: code-reviewer (claude/opus, effort=medium). Code under review: `git diff main -- crates/` on `feature/transcript-subagents` (HEAD `3baa099`).

## 1. Verdict

**PASS**: all 7 files match PLAN_FINAL byte for byte, all 7 acceptance criteria hold in code and tests, and a read-only run over 545 real subagent transcripts and 76 real parents found no regressions and no leaks.

## Disconfirmation (done first)

Counter-example I wrote down before evaluating: *"on real data, a finished subagent whose `last_assistant_message` equals its real final text still falls to `LastMessage` (the staleness rule is too strict), or the spawn prompt / thinking leaks into the body, or `render_*_with_subagents(turns, &[])` differs from the old renderers on a real parent."*

I tested it with a throwaway harness `scratch/crev/harness` (path dep on the repo crate, `CARGO_TARGET_DIR=%TEMP%\cctg-task007-crev-harness-target`, prints counts only, copies no real content). It ran over every `~/.claude/projects/**/subagents/agent-*.jsonl` plus `.meta.json`, and over every parent `*.jsonl`:
- 545 subagents: no input gives 544 `Transcript` and 1 `InProgress`. The `InProgress` file was modified 2 s earlier, so it is a live run in this session tree.
- `last_assistant_message` simulated as the real last assistant text: 534 `Transcript`, 9 `LastMessage`. I checked all 9 `LastMessage` cases. Seven end in `> [Request interrupted by user]`, and the text after the last assistant record is only tool calls or the interrupt. The other two are live, unfinished runs. `LastMessage` is the intended result for every one of them (PLAN_FINAL §5.1). No finished, uninterrupted transcript fell through.
- Spawn prompt in the body: 0. Body starting with `> `: 0. First user text marked `isMeta`: 0.
- Thinking in the rendered block: 0 of 545. In the parent full view with embedded blocks there was one hit. It is present in plain `render_full` too, and the same line occurs twice in the raw jsonl outside the thinking block (quoted content). This task did not introduce it.
- Parents: `render_brief == render_brief_with_subagents(.., &[])` for 76 of 76, and the same holds for full, 76 of 76. Real parents with sidechain turns: 0. In the 25 parents with embedded subagents, the full view with its body lines removed has the same line count as `render_full`.

**Did not hold.** The implementation survived the counter-example.

## 2. Confirmed correct

- **Plan fidelity.** SHA-256 of all 7 files equals `scratch/reviewer2/final_hashes.txt`. `Cargo.toml`, `Cargo.lock`, `crates/transcript/Cargo.toml`, `src/split.rs`, `tests/render.rs`, `tests/split.rs` and `tests/parse_fixtures.rs` are unchanged against `main`.
- **Checks, run by me in a fresh target dir `%TEMP%\cctg-task007-crev-target`:** `cargo fmt --check` clean. `cargo clippy --workspace --all-targets -D warnings` clean. `cargo test --workspace` gives 70 passed: cctg 1+1, parse_fixtures 10, parse_tolerance 15, purity 3, render 14, split 14, subagent 11, doc-test `compile_fail` 1. `cargo tree -p transcript --depth 1` shows only serde, serde_json and unicode-segmentation.
- **Criterion 1, sidechain isolation.** `render.rs:57-78`: `top_level` drops `is_sidechain` turns, using `Cow` and cloning only when such a turn exists. The embedded body goes under the `↳` line (`render.rs:123-130`). Tests: `sidechain_fixture_is_one_block_outside_the_parent_turns` and `sidechain_turns_never_reach_the_parent_top_level`.
- **Criterion 2, meta.** `subagent.rs:18-33` never fails. It strips a BOM, `unwrap_or(Value::Null)` handles bad JSON, `Value::get` on a non-object gives `None`, and blank or non-string fields become `None`. The header takes meta over the input, over the call, over `agent`, field by field (`subagent.rs:105-111`, `render.rs:278-288`). Tests cover `""`, `{`, `null`, `[]`, a string, wrong types and blank strings.
- **Criterion 3, report wins.** `subagent.rs:153-155` returns the trimmed report, and a blank report falls through.
- **Criterion 4, brief body in a full parent.** The body text comes from `render(turns, false, &[])` (`subagent.rs:188`), so it is always brief and never nests. In a full parent, subagent inputs and results do not appear (`body_is_brief_even_in_a_full_parent`, confirmed on 25 real parents).
- **Criterion 5, no IO.** `purity.rs` now scans `subagent.rs`. I grepped the file myself and found no `std::fs`/`io`/`env`, no `unwrap(`, no `expect(` and no `panic!`.
- **Criterion 6, order in types.** `Subagent` fields are private and there is no `Default` or public constructor other than `new` (`subagent.rs:91-118`). The doc-test is a well-typed literal (`agent_type: Some(String)`, `body: SubagentBody::Empty`), so privacy is its only compile error. `SubagentInput` has named fields only.
- **Criterion 7.** Existing tests are green, and old renderer output is unchanged on all 76 real parents.
- **Staleness rule.** `answer` must be both the last non-blank assistant text and the tail of the brief (`subagent.rs:195-197`). That rules out `not done` vs `done` and `Result:\ndone` vs `done`. Real data (above) shows the rule is not too strict.

## 3. Issues

No critical or major issues.

- **minor — `render.rs:131-155` (full parent), design note for the hub.** In full mode the parent's own `Agent` tool_result still renders as `  ← Agent: <up to 1500 chars>`. That is the subagent's final text, which the embedded body already shows. The full view therefore shows the answer twice. It does not break criterion 4: this is parent data, and it is truncated rather than expanded, and PLAN_FINAL/tests (`body_is_brief_even_in_a_full_parent`: removing the body gives `render_full`) make it intentional. Suggestion: if Telegram shows it as noise, TASK-015/016 can suppress the `← Agent` result line for known subagents. No change needed now.
- **minor — `subagent.rs:113`, `render.rs:297-311`: `agent_id` goes into the header unchanged.** A hub value with a newline or spaces would break the one-line header. The id comes from hooks/jsonl (`a` + hex), so the risk is low. Suggestion: the hub validates the id, or `Subagent::new` applies `one_line`. Not required for this task.
- **minor (accepted by plan) — `subagent.rs:158`.** With no `last_assistant_message`, a lagging transcript whose last written record is a null-stop text can look finished (`Transcript`) while the subagent is still running. This is documented in the planner log (lag rule) and needs hook data to resolve. It is noted here so the hub always passes `last_assistant_message` once `SubagentStop` has fired.

## 4. Missing coverage

- No test for a *background* `Agent` call, where the tool_result with `agentId` returns immediately while the subagent is still running. That case should give an `InProgress` body embedded in the parent. Synthetically it is the same code path as the `InProgress` row of `body_source_order_is_fixed` plus embedding, so this is low priority.
- No test for duplicate `agent_id` in `subagents` ("last wins", documented only).
- No test for `SubagentInput { agent_id: "" , .. }` (harmless, header `↳ agent `). Nit.

## 5. Nits

- `render.rs:333-338` `indent` turns blank body lines into `"  "`, which leaves trailing whitespace. It is harmless for Telegram, because `split_for_telegram` drops whitespace-only chunks.
- 373 of 545 real rendered blocks exceed 4096 UTF-16 units. That is expected under OPEN_DECISIONS 1, and the hub must split them or send a file.

Evidence: `C:/Users/user/dev/cctg/maw/tasks/in_progress/TASK-007/scratch/crev/harness/` (source only; the target dir is in `%TEMP%`).
