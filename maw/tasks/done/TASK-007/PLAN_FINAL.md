# PLAN FINAL — TASK-007: transcript — subagent data and collapsed rendering

Stage: plan-reviewer-2 (claude/opus, effort=medium). Paths are relative to the repo root `C:/Users/user/dev/cctg`.
`T` = `maw/tasks/in_progress/TASK-007`.
`R` = `T/scratch/reviewer2/ws/crates/transcript`: the **fixed** reference crate. It lives in `T/scratch/reviewer2/ws`, a copy of the planner's workspace (`git archive` of HEAD `3d81b4f` plus the changes below). HEAD is now `6ce29c1`, which touches no file under `crates/`, `Cargo.toml` or `Cargo.lock`. `R` was fully built and tested (fmt, clippy `-D warnings`, `cargo test --workspace`). Do **not** copy from `T/scratch/planner/ws`: that crate has the three defects listed in section 5.

## 1. Summary

Add a pure module `crates/transcript/src/subagent.rs` that turns what the hub has read about one subagent into a collapsed block `↳ <type> <id>[: <description>]` plus a body. The hub supplies named optional inputs: `.meta.json` text, the `SubagentHandback` report, the subagent jsonl text and `last_assistant_message`. The crate does no IO. `Subagent::new` is the only way to build a block, so the fallback order (report, then finished transcript brief, then `last_assistant_message`, then in-progress brief, then empty) is fixed by the library. A `compile_fail` doctest guards this. `render.rs` gains `render_brief_with_subagents` / `render_full_with_subagents`. They render a parent transcript with sidechain turns dropped. Each known subagent's header replaces its `Agent` line, and its body (always brief) is indented below. The existing `render_brief` / `render_full` output does not change. No new dependencies.

## 2. Implementation steps

### Step 0. Baseline (fresh target dir, mandatory)

Use a **new, empty** target dir outside the repo. Do NOT reuse `%TEMP%\cctg-task007-target`, `...-planner-target`, `...-reviewer1-*` or `...-rev2-*`. Cargo hashes path packages by their workspace-relative path, so a copy of this workspace in `scratch/` and the repo share artifact names. Freshness is mtime-based, so a dir that one of those copies built can serve its stale build to the repo. The reviewer hit this for real (section 5, item 5).

Git Bash:
```
export CARGO_TARGET_DIR="$TEMP/cctg-task007-impl-target"
rm -rf "$CARGO_TARGET_DIR"
cargo fmt --all -- --check
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace
```
PowerShell equivalent: `$env:CARGO_TARGET_DIR = "$env:TEMP\cctg-task007-impl-target"`, then `Remove-Item -Recurse -Force $env:CARGO_TARGET_DIR -ErrorAction SilentlyContinue`.

Expected: all pass, 58 tests (cctg 2; transcript parse_fixtures 10, parse_tolerance 15, purity 3, render 14, split 14). Stop if not. Do not run cargo builds in parallel (host memory is tight). Leave `.claude/` and the task artifacts alone.

Files this task changes. Nothing else changes, and `Cargo.toml`/`Cargo.lock` stay as they are:

| File | Kind |
|---|---|
| `crates/transcript/src/lib.rs` | replace from `R` |
| `crates/transcript/src/render.rs` | replace from `R` |
| `crates/transcript/src/subagent.rs` | new, from `R` |
| `crates/transcript/tests/purity.rs` | replace from `R` |
| `crates/transcript/tests/subagent.rs` | new, from `R` |
| `crates/transcript/tests/fixtures/subagent_handback.jsonl` | new, from `R` |
| `crates/transcript/tests/fixtures/subagent_handback.meta.json` | new, from `R` |

`tests/reviewer1_adversarial.rs` is NOT copied. Its three cases are folded into `tests/subagent.rs`, extended, and pass there.

### Step 1. Copy the reference files byte for byte

Copy each `R/<path>` to `crates/transcript/<path>` with a binary copy (`cp`), not by retyping. They are LF, UTF-8, no BOM. `core.autocrlf=true` normalizes on commit, which is fine. Then check SHA-256 on the copied working-tree files. The same list is in `T/scratch/reviewer2/final_hashes.txt`:

| Path | SHA-256 |
|---|---|
| `src/lib.rs` | `9dc827e78fc5d6f6cf99f2817f166bf4b97f4e878641ea1a1364ffc8165ecb77` |
| `src/render.rs` | `47f984a4ccc1c281d44822affff525f71f181d47da925b8f4976548648cca17e` |
| `src/subagent.rs` | `16655e5fdc9a31164d9d17ca1cabccf561eb9c2f614d2a996e32d3777461f36b` |
| `tests/purity.rs` | `e4699d4a213d9a2289e1a3b26ea6cde3c1647039905dbb7346926b1d2629d3b4` |
| `tests/subagent.rs` | `6977aa2f4ccf5d526f0cdb1ba599a555d9003504c2a5491e37e00d2cdea88b58` |
| `tests/fixtures/subagent_handback.jsonl` | `33da1f0ba36f37fd106e9da4b6fee44ba36ead6bb3744e48cdb4954af91b20b7` |
| `tests/fixtures/subagent_handback.meta.json` | `ebfe1f1dbc6c32d858067e767ba3d9ad3941b55dc7367cfad67df2b263fea0a7` |

`lib.rs`, `purity.rs` and both fixtures are identical to the planner's (same hashes as `T/scratch/planner/proto_hashes.txt`). `render.rs`, `subagent.rs` and `tests/subagent.rs` differ: `T/scratch/reviewer2/rev2_vs_planner.diff`. The fixtures were built by `T/scratch/planner/make_fixture.py` and privacy-scanned (`T/scratch/planner/scan_fixtures.out.txt`: 0 hits against 1152 real ids). Do not regenerate them.

Contents, so a code reviewer can check the copy:

**`src/lib.rs`**: the HEAD file plus `mod subagent;` after `mod split;`. The `render` re-export becomes `render_brief, render_brief_with_subagents, render_full, render_full_with_subagents`. Plus `pub use subagent::{Subagent, SubagentBody, SubagentInput, SubagentMeta, parse_subagent_meta};`. Nothing else.

**`src/render.rs`** (HEAD plus only these changes):
- `use std::borrow::Cow;`, `use crate::{Block, Role, Subagent, Turn};`, and `IN_PROGRESS_MARKER` becomes `pub(crate)`.
- `render_brief` / `render_full` call `render(turns, _, &[]).0`, so they behave exactly as at HEAD.
- New `pub fn render_brief_with_subagents(turns, subagents)` / `render_full_with_subagents` call `render(&top_level(turns), _, subagents).0`.
- New private `top_level(turns) -> Cow<[Turn]>`. It is `Borrowed` when no turn is `is_sidechain`, the case for every real parent (0/76). Otherwise it is `Owned`, with the sidechain turns filtered out.
- `render` becomes `pub(crate) fn render(turns, full, subagents: &[Subagent]) -> (String, bool)`. It builds `known: HashMap<&str, &Subagent>` by `agent_id` once. In the `ToolUse` arm it looks up `agent_id`, then the known `subagent`, and pushes `tool_line(name, input, agent_id, subagent)`. If the subagent's body text is non-empty, it pushes that text indented two spaces. The unchanged full-mode input line follows. The tail sets `in_progress = any && !finished`, pushes the marker when set, and returns `(out, in_progress)`.
- `tool_line(name, input, agent_id: Option<&str>, subagent: Option<&Subagent>)`. In the `Agent` branch, the type is `subagent.agent_type()`, else the call's `subagent_type`, else `agent`. The description is `subagent.description()`, else the call's `description`. It returns `agent_header(..)`.
- New `pub(crate) fn agent_header(kind, agent_id: Option<&str>, description: Option<&str>) -> String` holds the header code moved out of `tool_line`. `one_line` becomes `pub(crate)`.

**`src/subagent.rs`** (new, 233 lines):
- `SubagentMeta { agent_type, description: Option<String> }` and `parse_subagent_meta(&str)`. It never fails: it strips a leading BOM, uses `serde_json::from_str::<Value>(..).unwrap_or(Value::Null)`, reads `agentType`/`description` via `Value::get(..).and_then(as_str)`, trims them, and turns blank into `None`.
- `SubagentInput<'a> { agent_id: &str, agent_type, meta, report, transcript, last_assistant_message: Option<&str> }`. It derives `Clone, Copy, Default`, and every field is named.
- `SubagentBody::{Report, Transcript, LastMessage, InProgress(String), Empty}` with `text()`.
- `Subagent { agent_id: String, agent_type: Option<String>, description: Option<String>, body: SubagentBody }`. All fields are private. It has public `new`, `agent_id()`, `body()`, `render()` and `pub(crate)` `agent_type()`, `description()`. `render()` = `agent_header(agent_type or "agent", Some(id), description)` plus `\n<body>` when the body is non-empty.
- The `compile_fail` doctest on `Subagent` is a struct literal whose field types all match (`agent_type: Some("Explore".to_owned())`), so privacy is its only compile error (proved below).
- Body order in private `body()`:
  1. `report`, when non-blank, gives `Report(trimmed)`.
  2. If the transcript brief is finished, and `last_assistant_message` is absent or blank or equals its `answer`, the result is `Transcript(brief)`.
  3. A non-blank `last_assistant_message` gives `LastMessage(trimmed)`.
  4. A non-empty brief gives `InProgress(brief)`.
  5. Otherwise `Empty`.
- `transcript_brief(jsonl) -> TranscriptBrief { text, finished, answer }`. It parses, drops everything up to and including the spawn prompt (the first user turn with a text block), and renders brief with `&[]`. Empty parse gives the default. A prompt-only transcript gives `text = "в работе…"` with `finished = false`. `answer` is the last non-blank assistant text block, trimmed (`last_assistant_text`), and is kept only when `text == answer` or `text` ends with `"\n" + answer`, meaning it is the last thing rendered.
- No `unwrap()`/`expect(`/`panic!`, no IO. `purity.rs` enforces this.

**`tests/purity.rs`**: `SOURCES` becomes `[(&str, &str); 4]` with `("subagent.rs", include_str!("../src/subagent.rs"))`. Nothing else.

**`tests/subagent.rs`**: 11 tests, see section 3.

**Fixtures**: `subagent_handback.jsonl` has 9 records of subagent `a0000000000000002`, spawned by `final_answer.jsonl`: the spawn prompt, a `<system-reminder>` meta record, an attachment, thinking (`SECRET-THINKING-MARKER`), a `Bash` tool_use `List source files` with its result, a `SubagentHandback` tool_use (`input.message = "Modules: lib, render, split."`) with its result, and the `end_turn` farewell `Report handed back.`. All records are `isSidechain: true`. `subagent_handback.meta.json` has the real key set with fake values: `agentType Explore`, `description Explore crate`, `toolUseId toolu_demo32`.

### Step 2. Verify

```
cargo fmt --all -- --check
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace
cargo tree -p transcript --edges normal --depth 1
git status --short
```

Expected (the reviewer's run is in `T/scratch/reviewer2/workspace_test.out.txt`):
- fmt and clippy are clean.
- Tests: cctg 2; transcript parse_fixtures 10, parse_tolerance 15, purity 3, render 14, split 14, **subagent 11**, **doc-tests 1** (`subagent::Subagent - compile fail ... ok`). That is **70** in total.
- `cargo tree` lists only `serde`, `serde_json`, `unicode-segmentation`.
- `git status` shows only the 7 files of the Step 0 table, plus the pre-existing untracked `.claude/` and task artifacts. No `target/` appears in the repo.
- `tests/render.rs` is untouched and passes, which confirms that `render_brief`/`render_full` output is unchanged.

Commit on `feature/transcript-subagents` with an English message and no generated-by or co-author trailers (project law).

## 3. Test plan (`tests/subagent.rs`)

| Test | Criterion | What it proves |
|---|---|---|
| `sidechain_fixture_is_one_block_outside_the_parent_turns` | 1, 7 | `parse(tool_use_result)` has no sidechain turn. `Subagent` from `sidechain.jsonl` gives `Transcript("Modules: lib, parse.")`. The parent brief is the old brief with `  Modules: lib, parse.` under the `↳` line, which keeps the call's description `Explore crate` because no meta was given. There is one `↳` and no spawn prompt. With `&[]`, both `*_with_subagents` equal `render_brief`/`render_full`. |
| `sidechain_turns_never_reach_the_parent_top_level` | 1 | parent + sidechain turns mixed in one slice: in brief and in full, the spawn prompt is absent and `Modules: lib, parse.` appears once. With no block, sidechain text is still absent. The mixed result equals the parent-only result. (reviewer-1 defect 2) |
| `embedded_header_is_the_block_header` | 1, 2 | meta `Plan`/`From meta` gives `↳ Plan a0000000000000001: From meta\n  body` in the parent, and `Explore crate` is gone. With no meta and no type, the call's `Explore`/`Explore crate` stays. (reviewer-1 defect 3) |
| `meta_fixture_is_parsed` | 2 | the fixture meta gives `Explore` / `Explore crate`, and a BOM-prefixed copy parses the same |
| `meta_description_and_type_win` | 2 | meta `Explore` beats hook `general-purpose`, and multiline or padded values collapse to one line |
| `missing_or_broken_meta_falls_back_without_error` | 2 | `""`, `{`, `null`, `[]`, a JSON string, wrong types, blank strings and non-JSON each give `SubagentMeta::default()`. With broken or `None` meta, the header is `↳ Explore a1`. With no type at all, it is `↳ agent a1`. |
| `report_replaces_the_transcript_final_text` | 3 | a report gives `Report`, the farewell is absent, the report is trimmed, and a blank report falls through to the transcript |
| `body_is_brief_even_in_a_full_parent` | 4 | in `render_full_with_subagents`, the header comes first, then the indented brief body, then the call's input line. Subagent tool inputs, results, thinking and meta reminders do not appear. Removing the body lines gives `render_full(parent)`. |
| `body_source_order_is_fixed` | 6 | 11-row table: report beats everything; finished + agreeing gives `Transcript`; finished + newer hook text gives `LastMessage`; unfinished + last gives `LastMessage`; missing + last gives `LastMessage`; unfinished alone gives `InProgress("• Bash: List source files\nв работе…")`; prompt only gives `InProgress("в работе…")`; empty or garbage gives `Empty` |
| `last_message_must_equal_the_final_answer` | 6 | a suffix (`not done` vs `done`) is not a match, and neither is the last line of a longer answer (`Result:\ndone` vs `done`); both give `LastMessage`. Padded text on both sides still matches. An interrupted subagent with a hook value gives `LastMessage`; without one, it gives `Transcript("done\n\n> [Request interrupted by user]")`. (reviewer-1 defect 1) |
| `only_known_agents_get_a_body` | 1 | an unknown id changes nothing, and an `Empty` block with no meta or type leaves the full view identical |
| doc-test `Subagent` (`compile_fail`) | 5, 6 | a `Subagent` struct literal does not compile outside the crate |
| `purity::*` (extended) | 5 | `subagent.rs` is scanned: no fs/io/net/process/env, no unwrap/expect/panic, deps unchanged |

Evidence produced by this review (in `T/scratch/reviewer2/`):
- `repro_before.out.txt`: `reviewer1_adversarial` on the planner crate, 0 passed / 3 failed (reproduced).
- `reviewer1_adversarial_after.out.txt`: the same unmodified test on `R`, 3 passed.
- `new_tests_vs_planner.out.txt`: the new `tests/subagent.rs` against the planner's `src/`. The 3 new tests fail and the 8 old ones pass, so the new tests detect each defect.
- `mutation_pub_fields.out.txt`: with `Subagent` fields made `pub`, the doctest reports "Test compiled successfully, but it's marked `compile_fail`". The literal is otherwise well-typed, so privacy is the only thing keeping it from compiling.
- `workspace_test.out.txt`: the full green run (70) plus `cargo tree`.

## 4. Rollout notes

- No migrations, env vars, feature flags or dependency changes. This is a pure library addition. `crates/cctg` does not depend on `transcript` yet.
- Backward compatibility: `render_brief`/`render_full` keep their signatures and output (`tests/render.rs` unchanged and green). Plain `render_brief`/`render_full` still render sidechain turns if given them, as before. Only the new `*_with_subagents` functions drop them, so the hub must use those for a parent view.
- For the hub (TASK-015/016), documented and not enforced:
  - The body is not truncated. Real bodies have a median of 6075 UTF-16 units, and 367/539 exceed 4096 (`T/scratch/planner/probe_real.out.txt`). Send `render()` through `split_for_telegram` or as a file. This is an orchestrator decision (OPEN_DECISIONS 1).
  - Pass one `Subagent` per `agent_id`. With duplicates, the last one wins.
  - The body is attached only when the `Agent` call and its result (which carries `agentId`) are in the same slice.
  - Named fields stop the hub from reordering sources, but not from putting a value in the wrong field. Fill `report` only from `PreToolUse SubagentHandback.tool_input.message`.
- Nested subagents (a subagent's own `Agent` calls) are not embedded. `Subagent` bodies render with `&[]`, and calling `*_with_subagents` on a subagent transcript returns an empty view because every turn is sidechain. Use `render_brief`/`render_full` for a subagent's own transcript.

## 5. Review notes (changes from PLAN_V2 and why)

Disconfirmation target tested first: "the `compile_fail` doctest passes for a reason other than privacy (for example a field type mismatch), so criterion 6 is unguarded". **Did not hold** for the planner's literal (the planner's mutation run shows it). It **would have held** after fix 3 below changed `agent_type` to `Option<String>`, so the literal was updated to `Some(..)`, and the pub-fields mutation was re-run on `R` and compiled. The three reviewer-1 defects were reproduced (0/3) before any change.

1. **Staleness check (reviewer-1 defect 1, confirmed).** `brief.ends_with(last)` accepted `not done` for `done`. A line-bounded suffix would still accept `done` against the multi-line answer `Result:\ndone`. Now `Transcript` needs `last_assistant_message == trimmed last assistant text block`, and that block must be the brief's final line(s). An interrupt after the answer therefore also falls to `LastMessage`. A mismatch is safe because it yields the full hook text. Residual: if Claude Code ever joins several text blocks of one response into `last_assistant_message`, the result is `LastMessage`, not `Transcript`. The hooks docs only say "final assistant text of the current turn" (checked 2026-09-23 at code.claude.com/docs/en/hooks), and the 2 real TASK-003 captures matched a single final text record.
2. **Sidechain leak (reviewer-1 defect 2, confirmed).** The planner relied on "real parents contain no sidechain turns", which the API did not enforce. Now `*_with_subagents` drop `is_sidechain` turns via `top_level` (`Cow`, no clone in the real case). `render_brief`/`render_full` are unchanged, which keeps `render::null_stop_reason_falls_back_to_structure` as is. That was the planner's reason for rejecting filtering inside `render`.
3. **Embedded header ignored meta (reviewer-1 defect 3, confirmed).** The parent line used the call's `subagent_type`/`description` even when the block's `.meta.json` said otherwise (criterion 2). Now a known `Subagent` overrides each field separately, falling back to the call's values. `Subagent.agent_type` became `Option<String>` so an unknown type does not hide the call's `subagent_type`. The standalone `render()` still shows `agent` in that case.
4. **Tests.** `reviewer1_adversarial.rs` was folded into `tests/subagent.rs` as 3 tests extended with brief+full, the multi-line and interrupt cases, and the no-meta fallback: 8 tests became 11, and the workspace total went from 67 to 70. New hashes for `render.rs`, `subagent.rs`, `tests/subagent.rs`.
5. **Build hygiene.** PLAN_V2 told the implementer to use `%TEMP%\cctg-task007-target`. The reviewer saw a stale cross-copy build: a fixed crate reported the old failures because another copy of the workspace had built into the same target dir. Step 0 now requires a new, emptied `cctg-task007-impl-target`.
6. **Kept from PLAN_V2 unchanged:** the API shape, meta parsing, spawn-prompt drop, the body order and its privacy guarantee, fixtures, `lib.rs`, `purity.rs`, and the orchestrator decisions in `OPEN_DECISIONS.md` (brief body kept, no handback mining from the jsonl, no parent-description fallback in `SubagentInput`, though the embedded line now naturally falls back to the call's description).
