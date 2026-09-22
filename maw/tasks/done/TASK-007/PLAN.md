# PLAN — TASK-007: transcript — subagent data and collapsed rendering

Stage: planner (claude/opus, effort=medium). Paths are relative to the repo root `C:/Users/user/dev/cctg`.
`T` = `maw/tasks/in_progress/TASK-007`. `R` = `T/scratch/planner/ws/crates/transcript` (reference crate, built and tested by the planner inside `T/scratch/planner/ws`, a `git archive HEAD` copy of the workspace plus the changes below).

## 1. Understanding

### What exists (HEAD `3d81b4f`)

- `crates/transcript/src/lib.rs` (216 lines): `parse(&str) -> Vec<Turn>` (lines 127-132), `ai_title` (134-142), `Turn { role, blocks, is_meta, is_sidechain, stop_reason }` (47-58), `Block::{Text, ToolUse, ToolResult{.., agent_id}}` (24-45). `agent_id` is copied from the record-level `toolUseResult.agentId` (158-161). Sidechain records parse like any other record; `is_sidechain` is only a flag (178-184).
- `crates/transcript/src/render.rs` (282 lines): `render_brief`/`render_full` (43-51) call private `render(turns, full) -> String` (53-126). The `Agent` tool line `↳ {subagent_type|agent}[ {agent_id}][: {description}]` is built inside `tool_line` (224-249), with `agent_id` looked up from the `tool_use_id -> agent_id` map (`agent_ids`, 209-222). The in-progress marker `в работе…` is a private const (10) appended when the slice ends unfinished (122-125). No subagent body exists anywhere.
- `crates/transcript/src/split.rs`: unchanged by this task.
- Tests: `tests/{parse_fixtures,parse_tolerance,purity,render,split}.rs`. `purity.rs` lists every `src/*.rs` file in `SOURCES` and fails if `src/` has an unlisted file (`every_source_file_is_scanned`), and pins deps to `serde`, `serde_json`, `unicode-segmentation`.
- Fixtures relevant here: `tool_use_result.jsonl` (parent; `Agent` `toolu_demo20` → `a0000000000000001`, `Explore`, `Explore crate`), `sidechain.jsonl` (that subagent: spawn prompt + null-stop text `Modules: lib, parse.`), `final_answer.jsonl` (parent; `Agent` `toolu_demo32` → `a0000000000000002`, `Explore`, `Explore crate`).
- `crates/cctg` does not depend on `transcript`; hub is still a placeholder (premise challenge, `T/PREMISE_CHALLENGE.md`).
- Baseline in the planner's copy: `cargo test --workspace` = 58 tests (cctg 2; transcript parse_fixtures 10, parse_tolerance 15, purity 3, render 14, split 14).

### Real data (planner surveys, all of `~/.claude/projects`, outputs in `T/scratch/planner/`)

- `survey_subagents.out.txt`: 538 subagent jsonl, each with a `.meta.json`. Meta always has string `agentType`, `description`, `toolUseId`, int `spawnDepth`; others optional. 0 broken. Every subagent record is `isSidechain: true` with matching `agentId`. Last record: `end_turn` text 283, null-stop text 238, user text (interrupt) 10, other 7.
- `survey_parent.out.txt`: 0 of 76 parent files contain sidechain user/assistant records. 419 `Agent` results, all `async_launched` with `toolUseResult.agentId`; for all 419 the subagent file exists, `meta.toolUseId` equals the `Agent` tool_use id, and meta `agentType`/`description` equal the call's `subagent_type`/`description`.
- `survey_sub_user_text.out.txt`: the first user record of every subagent file is the spawn prompt (non-meta string, 538/538); `<system-reminder>` records are `isMeta` (hidden by brief already). Spawn prompt length: median 3273 chars, 212/538 over 4096.
- `survey_handback.out.txt`: 222/538 subagent files contain their own `SubagentHandback` `tool_use` with string `input.message`; after it comes a farewell text (median 200 chars).
- TASK-003 captures (`maw/tasks/done/TASK-003/scratch/capture_C_mixed.jsonl`, `capture_unknown.jsonl`): for both real `Explore` stops, `last_assistant_message` equals the final text record of the subagent jsonl (`...**cctg** and **transcript**.` and `4`).

### Research

- Claude Code hooks: `SubagentStop` carries `agent_id`, `agent_type`, `agent_transcript_path`, `last_assistant_message`; no `agent_output` field exists (orchestkit issue #4158 / PR #4232, https://github.com/yonatangross/orchestkit/issues/4158; https://code.claude.com/docs/en/agent-sdk/hooks). Matches `domains/hooks.md`.
- Rust API Guidelines C-CUSTOM-TYPE: "Arguments convey meaning through types, not bool or Option"; several same-typed optional inputs belong in a struct with named fields (https://rust-lang.github.io/api-guidelines/type-safety.html). This is the basis for `SubagentInput` instead of five positional `Option<&str>`.
- Invariant enforcement by privacy (only a constructor can build the value; "make illegal states unrepresentable") is checked by a `compile_fail` doctest, which `cargo test` runs.
- Telegram supports collapsed (expandable) blockquotes since Bot API 7.3 (https://core.telegram.org/bots/api, `expandable_blockquote` entity). The library returns plain text; the hub (TASK-015) chooses the entity.

## 2. Approach

New private module `src/subagent.rs`, a small refactor of `src/render.rs`, one new test file and two new fixtures. No new dependencies, no IO.

Public API added (re-exported from `lib.rs`):

```rust
pub struct SubagentMeta { pub agent_type: Option<String>, pub description: Option<String> }
pub fn parse_subagent_meta(json: &str) -> SubagentMeta;          // never fails

#[derive(Clone, Copy, Default)]
pub struct SubagentInput<'a> {                                   // what the hub read; all named
    pub agent_id: &'a str,
    pub agent_type: Option<&'a str>,             // hook agent_type / parent subagent_type
    pub meta: Option<&'a str>,                   // .meta.json text, None if missing
    pub report: Option<&'a str>,                 // SubagentHandback tool_input.message
    pub transcript: Option<&'a str>,             // agent-<id>.jsonl text, None if missing
    pub last_assistant_message: Option<&'a str>, // SubagentStop
}

pub enum SubagentBody { Report(String), Transcript(String), LastMessage(String), InProgress(String), Empty }
impl SubagentBody { pub fn text(&self) -> &str }

pub struct Subagent { /* private: agent_id, agent_type, description, body */ }
impl Subagent {
    pub fn new(input: SubagentInput<'_>) -> Self;
    pub fn agent_id(&self) -> &str;
    pub fn body(&self) -> &SubagentBody;
    pub fn render(&self) -> String;              // "↳ <type> <id>[: <description>]\n<body>"
}

pub fn render_brief_with_subagents(turns: &[Turn], subagents: &[Subagent]) -> String;
pub fn render_full_with_subagents(turns: &[Turn], subagents: &[Subagent]) -> String;
```

Rules, all in `Subagent::new`:

- Header type: meta `agentType` → `input.agent_type` → `agent`. Description: meta `description` only; missing or broken meta gives a header without description. Both pass through the existing `one_line` (whitespace collapsed, ≤120 chars). The header string comes from one shared `agent_header` helper, which `tool_line` now uses too, so the standalone block header and the parent's `Agent` line have one format.
- Body order (criterion 6), in this order:
  1. non-blank `report` → `Report(trimmed)`;
  2. transcript brief that is finished, and, when `last_assistant_message` is given, ends with it → `Transcript(brief)`;
  3. non-blank `last_assistant_message` → `LastMessage(trimmed)`; this covers a missing, unfinished or lagging file;
  4. non-empty unfinished transcript brief → `InProgress(brief ending with в работе…)`; used for live progress before any stop data;
  5. otherwise `Empty`.
- Transcript brief = `render(turns_after_spawn_prompt, brief)`. The spawn prompt (first user turn with a text block) is dropped, because it is the parent's `Agent` prompt, which the parent already shows (full view) and which is over 4096 chars in 40% of real files. A transcript that holds only the prompt gives the marker alone (`InProgress("в работе…")`). Empty or unparseable text gives `Empty`.
- The body is built only from `render(.., full = false, ..)`. No full variant exists, so a subagent body is brief in every context (criterion 4).

Why the order is fixed by types (criterion 6): the hub never orders sources. It fills named fields and calls `Subagent::new`. `Subagent`'s fields are private, so the hub cannot build a `Subagent` with a body it chose itself. A `compile_fail` doctest proves that. The planner's mutation (fields made `pub`) makes the doctest fail: `T/scratch/planner/mutation_pub_fields.out.txt`. `SubagentBody` is public only so the hub and tests can see which source won.

Parent embedding: `render` gains a `subagents: &[Subagent]` argument and returns `(String, bool in_progress)`. When an `Agent` call's `tool_use_id` maps to an `agent_id` with a non-empty body in `subagents`, the body is pushed right after the `↳` line, every line indented two spaces, in both modes. In full mode the call's input and result lines follow as before. The existing `render_brief`/`render_full` pass `&[]` and produce byte-identical output (asserted). Criterion 1: a subagent never appears as parent turns. The parent file carries none of its records (0/76 real parents), and the block is a separate value keyed by `agent_id`. The test checks that the parent brief has exactly one `↳` block, and that the subagent's spawn prompt and records are not in it.

Alternatives rejected (also in `T/log.jsonl`):
- Positional `fn body(report, transcript, last)`, or a public enum the hub picks from. Both compile when sources are swapped or reordered.
- Filtering `is_sidechain` turns inside `render`. No real parent contains them. It would change the existing `render::null_stop_reason_falls_back_to_structure` expectations for `sidechain.jsonl`, and it would need a second flag for subagent rendering.
- Changing the signatures of `render_brief`/`render_full`. That breaks every existing call for no gain.
- Keeping the spawn prompt in the body. Size data above.

## 3. Steps

### Step 0. Baseline

Target dir outside the repo for every cargo command. Git Bash: `export CARGO_TARGET_DIR="$TEMP/cctg-task007-target"`. PowerShell: `$env:CARGO_TARGET_DIR = "$env:TEMP\cctg-task007-target"`. Then:

```
cargo fmt --all -- --check
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace
```

Expected: all pass, 58 tests (cctg 2, transcript 56). Stop if not. Leave `.claude/` and task artifacts alone.

Files this task changes (nothing else; `Cargo.toml`/`Cargo.lock` do NOT change):

| File | Kind |
|---|---|
| `crates/transcript/src/lib.rs` | replace from `R` (+`mod subagent;`, re-exports) |
| `crates/transcript/src/render.rs` | replace from `R` (refactor below) |
| `crates/transcript/src/subagent.rs` | new, from `R` |
| `crates/transcript/tests/purity.rs` | replace from `R` (+1 `SOURCES` entry) |
| `crates/transcript/tests/subagent.rs` | new, from `R` |
| `crates/transcript/tests/fixtures/subagent_handback.jsonl` | new, from `R` |
| `crates/transcript/tests/fixtures/subagent_handback.meta.json` | new, from `R` |

### Step 1. Copy the reference files

Copy each `R/<path>` to `crates/transcript/<path>` byte for byte (LF, UTF-8, no BOM; `core.autocrlf=true` normalizes on commit, fine). Check SHA-256 (same list in `T/scratch/planner/proto_hashes.txt`):

| Path | SHA-256 |
|---|---|
| `src/lib.rs` | `9dc827e78fc5d6f6cf99f2817f166bf4b97f4e878641ea1a1364ffc8165ecb77` |
| `src/render.rs` | `bd9897b603d2a825392bb82d84e81ab5b36675a553f7599898863c33f618470b` |
| `src/subagent.rs` | `3cac20d858cef2f4c357ab717964844facb17fe16aaa48d86ff329582c9b5165` |
| `tests/purity.rs` | `e4699d4a213d9a2289e1a3b26ea6cde3c1647039905dbb7346926b1d2629d3b4` |
| `tests/subagent.rs` | `71336e5fd46211c4b9278f5530321c5fe1ccc1411ce00cd418856fd94ad2b4e7` |
| `tests/fixtures/subagent_handback.jsonl` | `33da1f0ba36f37fd106e9da4b6fee44ba36ead6bb3744e48cdb4954af91b20b7` |
| `tests/fixtures/subagent_handback.meta.json` | `ebfe1f1dbc6c32d858067e767ba3d9ad3941b55dc7367cfad67df2b263fea0a7` |

The fixtures are identical to `T/scratch/planner/fixtures/` (built by `T/scratch/planner/make_fixture.py`, TASK-005/006 method; privacy scan `T/scratch/planner/scan_fixtures.py` → `scan_fixtures.out.txt`, 0 hits against 1152 real session/agent/toolUse ids). Do not regenerate them from `~/.claude` (the implementer sandbox has no access to it anyway).

What each file contains, so a reviewer can check the copy (exact diff for the edited files: `T/scratch/planner/proto.diff`):

**`src/lib.rs`**: the HEAD file plus `mod subagent;` after `mod split;`. The `render` re-export becomes `render_brief, render_brief_with_subagents, render_full, render_full_with_subagents`. New line `pub use subagent::{Subagent, SubagentBody, SubagentInput, SubagentMeta, parse_subagent_meta};`. Nothing else.

**`src/render.rs`** (HEAD plus only these changes):
- `use crate::{Block, Role, Subagent, Turn};`; `IN_PROGRESS_MARKER` becomes `pub(crate)`.
- `render_brief`/`render_full` call `render(turns, _, &[]).0`. New `pub fn render_brief_with_subagents` / `render_full_with_subagents` call `render(turns, _, subagents).0`.
- `render` becomes `pub(crate) fn render(turns, full, subagents: &[Subagent]) -> (String, bool)`. It builds `bodies: HashMap<&str, &str>` (agent_id → body text) once. In the `ToolUse` arm: `let agent_id = agents.get(id).copied()`, then the tool line, then, if `bodies` has a non-empty body for that id, `push_line(indent(body))`, then the unchanged full-mode input line. The tail computes `in_progress = any && !finished`, pushes the marker if set, and returns `(out, in_progress)`.
- `tool_line` takes `Option<&str>` (was `Option<&&str>`). Its `Agent` branch returns `agent_header(&kind, agent_id, field("description").as_deref())`.
- New `pub(crate) fn agent_header(kind, agent_id: Option<&str>, description: Option<&str>) -> String` holds the exact code moved out of `tool_line`. `one_line` becomes `pub(crate)`.

**`src/subagent.rs`** (new, ~190 lines): the API in section 2. `parse_subagent_meta` strips a leading BOM, runs `serde_json::from_str::<Value>(..).unwrap_or(Value::Null)`, and reads `agentType`/`description` with `Value::get(..).and_then(as_str)`, trimmed, blank → `None`. A non-object gives `None`s. The body logic is `body()`, `transcript_brief()` and `after_spawn_prompt()`; `non_blank()` trims. The `compile_fail` doctest sits on `Subagent`. It uses no `unwrap()`/`expect(`/`panic!`, no IO; the purity test covers it.

**`tests/purity.rs`**: `SOURCES` becomes `[(&str, &str); 4]` with `("subagent.rs", include_str!("../src/subagent.rs"))`. Nothing else.

**`tests/subagent.rs`** (8 tests, section 4).

**Fixtures**: `subagent_handback.jsonl`, 9 records of the subagent `a0000000000000002` spawned by `final_answer.jsonl`: the spawn prompt (non-meta string), `<system-reminder>` (meta), an attachment, thinking (`SECRET-THINKING-MARKER`), `Bash` tool_use `List source files` + result, a `SubagentHandback` tool_use (`input.message = "Modules: lib, render, split."`) + result, and the `end_turn` farewell `Report handed back.`. All records are `isSidechain: true`. `subagent_handback.meta.json` has the real key set with fake values: `agentType Explore`, `description Explore crate`, `toolUseId toolu_demo32`.

### Step 2. Verify

```
cargo fmt --all -- --check
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace
cargo tree -p transcript --edges normal --depth 1
git status --short
```

Expected:
- fmt and clippy are clean.
- Tests: cctg 2; transcript parse_fixtures 10, parse_tolerance 15, purity 3, render 14, split 14, **subagent 8**, **doc-tests 1** (the `compile_fail` one), which is 67 in total. The planner's run is in `T/scratch/planner/workspace_test.out.txt`.
- `cargo tree` shows only `serde`, `serde_json`, `unicode-segmentation`.
- `git status` shows only the 7 files of the Step 0 table plus the pre-existing untracked `.claude/` and task artifacts; no `target/` in the repo.
- Existing `tests/render.rs` is untouched and passes. That is the check that `render_brief`/`render_full` output did not change.

Commit on `feature/transcript-subagents`, English message, no generated-by / co-author trailers (project law).

## 4. Test plan (`tests/subagent.rs`)

| Test | Criterion | What it proves |
|---|---|---|
| `sidechain_fixture_is_one_block_outside_the_parent_turns` | 1, 7 | `parse(tool_use_result)` has no sidechain turn. `Subagent` from `sidechain.jsonl` gives `Transcript("Modules: lib, parse.")` and renders `↳ Explore a0000000000000001\nModules: lib, parse.`. The parent brief with it is exactly the old brief with `  Modules: lib, parse.` under the `↳` line, one `↳` in total, and the spawn prompt is absent. With `&[]`, both `*_with_subagents` equal `render_brief`/`render_full`. |
| `meta_fixture_is_parsed` | 2 | fixture meta gives `Explore` / `Explore crate`; a BOM-prefixed copy parses the same |
| `meta_description_and_type_win` | 2 | meta `Explore` beats hook `general-purpose`; the header is `↳ Explore a0000000000000002: Explore crate`; multiline or padded meta values are collapsed to one line |
| `missing_or_broken_meta_falls_back_without_error` | 2 | `""`, `{`, `null`, `[]`, a JSON string, wrong types, blank strings, non-JSON each give `SubagentMeta::default()`; with meta broken or `None`, the header is `↳ Explore a1` from the hook type; with no type at all, `↳ agent a1` |
| `report_replaces_the_transcript_final_text` | 3 | report present gives `Report`; `render()` is header + report, and the farewell `Report handed back.` is absent; the report is trimmed; a blank report falls through to the transcript |
| `body_is_brief_even_in_a_full_parent` | 4 | `render_full_with_subagents(final_answer, [subagent])` has the header, then the indented brief body, then the call's input line. The subagent's tool input, tool results, thinking and meta reminder appear neither in the parent full view nor in `render()`. Removing the body lines gives exactly `render_full(parent)`. The report variant sits under the `↳` line in the brief parent. |
| `body_source_order_is_fixed` | 6 | a table of 11 combinations: report beats everything; finished transcript that agrees with `last_assistant_message` gives `Transcript`; finished but disagreeing (lagging) gives `LastMessage`; unfinished (first 6 records) + last message gives `LastMessage`; missing transcript + last message gives `LastMessage`; unfinished without stop data gives `InProgress("• Bash: List source files\nв работе…")`; prompt only gives `InProgress("в работе…")`; `""`, garbage and a blank last message give `Empty`; the empty block renders header only |
| `only_known_agents_get_a_body` | 1 | a subagent whose id matches no `Agent` call changes nothing; an `Empty` body adds no line |
| doc-test on `Subagent` (`compile_fail`) | 5, 6 | a `Subagent` struct literal does not compile outside the crate |
| `purity::*` (existing, extended) | 5 | `subagent.rs` has no `std::fs`/`io`/`net`/`process`/`env`, no `unwrap`/`expect`/`panic`; deps unchanged |

Extra planner evidence (not copied into the repo): `T/scratch/planner/probe/` runs `Subagent::new` over all 539 real subagent files in 1.5 s release, with no panic and every header starting with `↳ ` (`probe_real.out.txt`).

## 5. Risk areas

- **Block size.** Real transcript bodies are large: median 6075 UTF-16 units, 367/539 over 4096. Tool lines dominate (median 49 lines, about 3300 units), and final reports alone exceed 4096 in 77/539 (`probe_real.out.txt`). The library does not truncate. TASK-015 must send `render()` through `split_for_telegram` (or a document) or trim it. See open question 1.
- **Lag heuristic.** `ends_with(last_assistant_message)` assumes the hook text equals the jsonl's final text block (2/2 real captures). If a final answer spans several text records, the brief joins them with `\n` and the check can fail. The result is then `LastMessage`: the correct final text, but without tool lines. That is safe, but worth a TASK-015 test against real hook captures.
- **Spawn-prompt rule.** It assumes the first user text is the spawn prompt (538/538 today). If Claude Code ever writes a meta record with a text block first, that record is dropped instead, and the real prompt shows in the body. The result is visible but harmless.
- **Handback without the hook.** If the hub misses the `SubagentHandback` hook (restart, hook not installed), the body falls to the transcript brief. There the report shows only as `• SubagentHandback` and the final line is the farewell (222/538 real files). See open question 2.
- **Duplicate ids in `subagents`.** The last one wins (HashMap). The hub should pass one `Subagent` per id.
- **Embedded body in an incremental slice (TASK-016).** The body is attached only when the `Agent` call's result, which carries `agentId`, is in the same slice as the call. A slice that ends between call and result shows no body. Same limitation as the existing id display.

## 6. Open questions

1. Should the collapsed block body be only the final answer (report / last text) instead of the whole brief, to fit one Telegram message more often? The task text and domain law say "brief", so the plan keeps brief. The size data says most blocks will need splitting. Decision for the orchestrator/user; changing it later is local to `transcript_brief`.
2. Should the transcript step prefer the subagent's own last `SubagentHandback` `input.message` (present in 222/538 files) over its farewell text? That would make the report survive a missed hook. It is outside the order named by the task, so the plan does not do it; proposed for TASK-015 (recorded in `T/PCTX_PROPOSALS.md`).
3. The hub may know the parent `Agent` call's `description` while the meta is missing. Add `description: Option<&str>` to `SubagentInput` as the second fallback? Not needed by any criterion; meta and parent agree 419/419.
