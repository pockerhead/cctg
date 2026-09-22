# QA REPORT — TASK-007: transcript, subagent data and collapsed rendering

Stage: qa (claude/opus, effort=medium). Code: `git diff main -- crates/` on `feature/transcript-subagents`, HEAD `9f436ac` (code commit of the fixer: `8ee8575`).

## 0. Disconfirmation (done first)

Counter-example I set out to find: "a finished subagent block in a full-mode parent shows thinking, the spawn prompt or the subagent's tool inputs/results, or `render_brief`/`render_full` on a real parent differs from `main`".

Search: an independent harness `scratch/qa` (synthetic jsonl with markers in every hidden place, plus a read-only sweep over all real transcripts, printing counts only) and an oracle built from `main`: `git archive main crates/transcript`, renamed `transcript_main`, in `%TEMP%`, compared byte for byte.

Result: **did not hold.** 0 thinking, 0 spawn prompt in 546 real blocks; 76/76 real parents give identical `render_brief`/`render_full` against `main`. Two narrow edge cases are listed in section 4 (low, not blocking).

## 1. Environment

- No docker-compose, no dev server. Pure library: `cargo` directly on the working tree `C:/Users/user/dev/cctg`.
- Target dirs outside the repo, one build at a time: `%TEMP%\cctg-task007-qa-target` (workspace), `%TEMP%\cctg-qa007-harness-target` (QA harness), `%TEMP%\cctg-qa007-mut-target` (mutation copy).
- Oracle: `%TEMP%\cctg-qa007-main` (main's transcript crate). Mutation copy: `%TEMP%\cctg-qa007-mut` (HEAD's crate). All removed after the run.
- No services or containers were started.

Reproduce (Git Bash):
```
export CARGO_TARGET_DIR="$TEMP/cctg-task007-qa-target"
cargo fmt --all -- --check
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace
# oracle + harness
M="$TEMP/cctg-qa007-main"; mkdir -p "$M"; git archive main crates/transcript | tar -x -C "$M"
sed -i 's/^name = "transcript"/name = "transcript_main"/' "$M/crates/transcript/Cargo.toml"
# add a minimal [workspace] Cargo.toml in $M (members crates/transcript, edition 2024, serde/serde_json/unicode-segmentation)
cd maw/tasks/in_progress/TASK-007/scratch/qa
CARGO_TARGET_DIR="$TEMP/cctg-qa007-harness-target" cargo run --release -- --real
```

## 2. Test results

### Existing suite (fresh target dir)
- `cargo fmt --all -- --check`: clean.
- `cargo clippy --workspace --all-targets -- -D warnings`: clean.
- `cargo test --workspace`: **73 passed, 0 failed** (cctg 1+1, parse_fixtures 10, parse_tolerance 15, purity 3, render 14, split 14, subagent 14, doc-test `compile_fail` 1).
- `cargo tree -p transcript --edges normal --depth 1`: serde, serde_json, unicode-segmentation only.
- `grep std::fs|std::io|std::env|std::net|std::process|File::` over `crates/transcript/src`: 0 hits.

### New QA tests (`scratch/qa/src/main.rs`, not in the repo crate)
Synthetic: **78 PASS, 0 FAIL, 4 NOTE**.
- S1–S2: subagent transcript with thinking, signature, meta reminder, Bash input and result; parent with its own thinking. Brief parent, full parent and standalone block: none of the markers appear.
- S3: spawn prompt absent from the block and the brief parent.
- S4–S5: one `↳` line, indented brief body; full parent minus body lines == `render_full(parent)`; `*_with_subagents(&[])` == old renderers.
- S6: parent slice mixed with sidechain turns (including a sidechain `Agent` call with its own `agentId`): no sidechain text or id in brief/full `*_with_subagents`; plain `render_brief` unchanged (still shows them, as documented).
- S7: 13-row fallback matrix written independently (report beats everything, blank report falls through, newer hook text beats a finished transcript, padded/CRLF hook text matches, suffix `answer` vs `QA final answer` is not a match, unfinished+hook, unfinished alone, no transcript, garbage transcript with and without hook, whitespace hook, prompt only, CRLF transcript).
- S8: meta variants: non-string `agentType` falls back to hook type, multi-line description is one-lined, meta type wins, 7 broken metas give `↳ agent a1`, meta overrides the call's type/description on the parent line.
- S9: a nested `Agent` inside a subagent shows as one header line, not expanded.
- S12: all 8 old fixtures: HEAD `render_brief`/`render_full` == `main`, `*_with_subagents(&[])` == plain.

Real data, read-only (`--real`, counts only): 76 parents, 546 subagent files.
- `render_brief`/`render_full` HEAD vs `main`: 76/76 identical; `*_with_subagents(&[])` vs plain: 76/76 identical.
- Blocks from real meta+jsonl (no hook data): 545 `Transcript`, 1 `InProgress` (a live run).
- Thinking (60-char windows) in any block: 0. Spawn prompt in any body: 0. New thinking in parent views: 0.
- Header linkage: 448 ids behave as expected, 98 appear only inside another block's body (nested subagents, not embedded by design), 0 bad.
- Eye check of two real blocks from parent #15: `↳ <type> <id>: <description>` then one-line tool calls, no prompt, no thinking.

### Mutation tests (on a copy in `%TEMP%`, repo untouched)
| Mutation | Result |
|---|---|
| `Subagent` fields made `pub` | doctest: "Test compiled successfully, but it's marked `compile_fail`" → the guard is privacy only |
| report check moved after `LastMessage` | 3 subagent tests fail |
| staleness check removed (`if brief.finished`) | 2 tests fail |
| sidechain filter disabled (`top_level`) | `sidechain_turns_never_reach_the_parent_top_level` fails |
| body rendered in full mode | 4 tests fail |

### Fixer's changes checked
- `agent_header` now runs `agent_id` through `one_line`; lookup still uses the raw id (`Subagent.agent_id` unchanged). Verified by the fixer's test and by S10 below.
- "Null-tolerant deserializer": the fixer did not touch `lib.rs` (commit `8ee8575` changes only `render.rs` +3/−1 and `tests/subagent.rs`). `string_or_default` and per-item `to_block` decoding are byte-identical to `main`; parsing of 76 real parents gives identical renders to `main`. No path exists for thinking into a public item: `Block` has no thinking variant, `RawBlock` maps `thinking` to `Ignored`.

## 3. Acceptance criteria

| Criterion | Test performed | Result |
|---|---|---|
| sidechain fixture renders as one `↳ <type> <id>` block and is not in parent top-level turns | repo tests + S4, S6 (mixed slice incl. sidechain `Agent`), mutation of `top_level`, 546 real blocks | PASS |
| `.meta.json` description used when present; missing/broken meta gives safe fallback | S8 (7 broken metas, wrong types, BOM), real metas | PASS |
| passed report replaces the transcript final text | S7 rows 1–2, mutation of order | PASS |
| subagent body always brief even when parent is full | S2, S5, mutation to full-mode body, 25+ real parents with blocks | PASS |
| API takes `&str` and typed values, no filesystem calls in the crate | grep of src, purity test, `cargo tree` | PASS |
| fallback order (report → transcript brief → `last_assistant_message`) in types, hub cannot mix it up | private fields + `compile_fail` doctest proven by mutation; only `Subagent::new`; S7 matrix | PASS |
| Existing tests pass | 73/73; old renderers identical to `main` on fixtures and 76 real parents | PASS |

## 4. Bugs found

No critical or major bugs. Findings, all low:

1. **Low — `subagent.rs` `after_spawn_prompt`: a meta record before the spawn prompt makes the prompt show in the body.** The function drops the first user turn with a text block, `isMeta` or not. Repro (S11): subagent jsonl `meta user "<system-reminder>r</system-reminder>"`, then `user "QA-LATE-PROMPT"`, then `end_turn` text `done`. Expected body `done`; actual `> QA-LATE-PROMPT\ndone`. Not seen in real data (546/546 have the prompt first, 0 leaks), so not blocking. Fix if wanted: skip `is_meta` turns when looking for the spawn prompt.
2. **Low — `render.rs` `agent_header` (fixer change): plain `render_brief`/`render_full` output changes for exotic agent ids.** `one_line` also truncates at 120 chars and collapses whitespace. Repro (S10): parent `Agent` result with `agentId` `"a1 \n b2"` gives `↳ X a1 b2` (main: `↳ X a1 \n b2` split over lines); a 130-char id is cut to 120 + `… [+10 chars]`. Real ids (`a` + 16 hex) are unaffected: 76/76 real parents identical to main. Arguably an improvement; noted only because the task says old output is unchanged.
3. **Info — full parent shows the spawn prompt via the parent's own `Agent` input line** (first 500 chars of the input JSON). Pre-existing `render_full` behavior, parent data, not the subagent body; the approved design keeps `render_full` compatible.

Accepted by orchestrator decisions, not defects: bodies above 4096 UTF-16 units, no `SubagentHandback` mining, no description fallback from the parent call, a lagging transcript without hook data can look finished.

## 5. Verdict

**SHIP.** All 7 criteria pass with independent tests, the `compile_fail` guard is proven to rest on privacy, mutation tests show the repo tests catch reordering, sidechain leaks and full-mode bodies, and the real-data sweep shows no thinking, no spawn prompt and no change to the old renderers. The two low findings are edge cases not present in real data.

## Cleanup

No services or containers started. Removed `%TEMP%\cctg-qa007-main`, `%TEMP%\cctg-qa007-mut`, `%TEMP%\cctg-qa007-mut-target`, `%TEMP%\cctg-qa007-harness-target`, `%TEMP%\cctg-task007-qa-target`. QA harness source stays in `scratch/qa` (no real content, no target dir).
