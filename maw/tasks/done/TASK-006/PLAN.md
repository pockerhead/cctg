# PLAN — TASK-006: transcript, brief/full rendering and Telegram sizing

Stage: planner (claude/opus, effort=medium). Inputs: `TASK_FINAL.md` (amended after PREMISE SUSPECT), `PREMISE_CHALLENGE.md`, `OPEN_DECISIONS.md`, normative domain `transcript`, `crates/transcript/**`, TASK-005 artifacts, pending TASK-007/009/015/016 specs.

Paths are relative to the repo root `C:/Users/user/dev/cctg`. `T` = `maw/tasks/in_progress/TASK-006`. `P` = `T/scratch/planner`.

The whole design below was built and run as a reference crate in `P/proto/transcript/` (copy of `crates/transcript` plus the changes): `cargo fmt --all --check`, `cargo clippy --all-targets -- -D warnings` clean, 47 tests pass (`P/proto_test.out.txt`), with `CARGO_TARGET_DIR` outside the repo. Unified diff against the current crate: `P/proto.diff`. SHA-256 of every file: `P/proto_hashes.txt`.

## 1. Understanding

### What exists (TASK-005, commit 9d8d3dd)

- `crates/transcript/src/lib.rs` (200 lines), the only source file.
  - `Role`, `Block::{Text, ToolUse{id,name,input}, ToolResult{tool_use_id,content,is_error,agent_id}}` (lines 11-39). No thinking variant, so thinking can never leave the crate.
  - `Turn { role, blocks, is_meta, is_sidechain }` (lines 41-49). One jsonl record = one `Turn`; a thinking-only record becomes no `Turn` (blocks empty, lines 160-162).
  - `RawMessage` deserializes only `content` (lines 65-69). `stop_reason` is dropped. This is the premise gap: `PREMISE_CHALLENGE.md` proved `tool_use` and `end_turn` text parse to equal `Turn`s.
  - `parse` (117-122), `ai_title` (124-132), `to_turn` (142-169), `to_block`, `result_text`. Crate-level `deny(clippy::unwrap_used, expect_used, panic)` outside tests.
- `crates/transcript/Cargo.toml`: deps `serde`, `serde_json` only.
- Tests: `tests/parse_fixtures.rs` (9 tests; builds `Turn` struct literals in `turn()` at line 19 and `null_string_fields_fixture` at line ~185, so adding a field breaks them at compile time), `tests/parse_tolerance.rs` (14), `tests/purity.rs` (2; `include_str!("../src/lib.rs")` and bans `std::fs`, `std::io`, `.unwrap()`, `panic!`, `#[cfg(test)]`, ... in that one file; asserts deps are exactly `serde`, `serde_json`).
- Fixtures `tests/fixtures/*.jsonl` (6). Their `stop_reason` values (checked with a script): `plain_text` assistant text = `tool_use`; `thinking_ai_title` both assistant records = `tool_use` (the file ends on intermediate text, a ready-made unfinished tail); `tool_use_result` 4 assistant records = `tool_use`; `sidechain` assistant text = `null`; `null_fields` absent; user records never have it. No existing fixture has an `end_turn` text.
- `crates/cctg` does not depend on `transcript` (grep), so the `Turn` field addition breaks nothing outside this crate's tests.

### What real transcripts say (read-only surveys over 605 local jsonl, aggregates only)

`P/survey_stop_reason.py` -> `P/survey_stop_reason.out.txt`, `P/survey_null_stop.py` -> `P/survey_null_stop.out.txt`:
- Main transcripts: text records are 2132 `end_turn`, 1489 `tool_use`, 29 `null`, 18 `stop_sequence` (synthetic API-error records). A `tool_use` text is followed by a `tool_use` record of the same `message.id` in 1498 of 1504 cases.
- Subagent transcripts write `stop_reason` only on the LAST record of a response; earlier records are `null` (patterns like `null:thinking | null:text | tool_use:tool_use`, 10026 message ids with mixed values). 4858 subagent text records are `null`; 238 of 530 subagent files end in a `null` final text. So "only `end_turn` is an answer" taken literally would hide the final answer of almost half the subagents (TASK-007 renders exactly these in brief form).
- Null-stop text in main transcripts is followed by a user text record (interrupt), i.e. it is the last thing the assistant said.
- User text records (`P/survey_user_text.out.txt`): non-meta plain 1449, `<task-notification>` 751 (non-meta), `[Request interrupted by user...]` 175, meta plain 204, `<local-command-caveat>` 39 (meta), `<channel ...>` 3 (meta). Tool name `Task` occurs 0 times, `Agent` 412.

### Telegram length unit (research)

- Bot API `sendMessage.text`: "1-4096 characters after entities parsing"; entity offsets/lengths are in UTF-16 code units (https://core.telegram.org/bots/api, https://core.telegram.org/api/entities).
- MTProto `config.message_length_max`: "Maximum length of messages (length in utf8 codepoints)" (https://core.telegram.org/constructor/config, fetched 2026-09-22).
- TDLib does not check the length locally; the server returns `MESSAGE_TOO_LONG`, which TDLib maps to "Message is too long" (`td/telegram/MessagesManager.cpp`, `process_send_message_fail_error`). The Bot API server has no own check for message text (`telegram-bot-api/Client.cpp` grep).
- Folk reports disagree (some say UTF-16, e.g. https://github.com/latypova-alina/prompture/pull/144; others "4096 UTF8 characters", https://github.com/yagop/node-telegram-bot-api/issues/165). No primary source settles it for emoji-heavy text, and `CLAUDE.md` only verified the ASCII case (4096 ok, 4097 fails).
- Conclusion: count in UTF-16 code units. A string's UTF-16 length is always >= its code-point count, so a chunk that fits by UTF-16 fits under either server rule. Cost: a chunk of only astral emoji holds 2048 emoji instead of 4096. The unit lives in one public function `telegram_len` so the hub (TASK-008/009/014) uses the same definition, as the task asks.

## 2. Approach

1. **Parser**: add `pub stop_reason: Option<String>` to `Turn`, read from `message.stop_reason` as `serde_json::Value` (string -> `Some`, null/absent/other type -> `None`). Same tolerance pattern TASK-005 uses for `isMeta` (risk lesson: never a typed field that can fail the struct).
2. **Renderer** (`src/render.rs`): one private `render(turns, full)` behind `render_brief(&[Turn]) -> String` and `render_full(&[Turn]) -> String`. It works on any slice, so TASK-016 pushes `render_brief(&turns[old..])` and TASK-009 renders the whole file. Plain text, no Markdown/HTML entities.
3. **Answer rule** (decision logged): assistant text is the answer when `stop_reason == "end_turn"`; it is hidden in brief when `stop_reason == "tool_use"`; for any other value (`null`, `stop_sequence`, `refusal`, absent) it is the answer unless a tool call follows it before the next prompt. That follows the amended criterion for every record that has a real stop reason, and handles subagent null-stop records by structure instead of hiding their answers.
4. **In-progress marker**: after rendering, if the slice had anything visible and its last significant item is not a final answer (a prompt without reply, a tool call, a tool result, or intermediate text), append the line `в работе…` (`IN_PROGRESS_MARKER`). A `[Request interrupted by user` prompt counts as finished, otherwise every interrupted session would say "в работе…" forever. Both brief and full append it.
5. **Splitter** (`src/split.rs`): `split_for_telegram(&str, SplitLimits) -> Chunks`. Works on the rendered string: pack paragraphs (`\n\n`) greedily, a paragraph over the limit goes line by line, a line over the limit is hard-cut on a char boundary (prefer the last whitespace in the second half of the window, and do not cut next to ZWJ / variation selector / keycap / skin tone / tag / combining mark when a point within 16 chars back is safe). Blank pieces are dropped. `prefer_file = chunks.len() > max_chunks`; chunks are always returned. Linear: every length is computed once per piece and the packer keeps a running length.
6. **No new dependencies**. Grapheme segmentation (`unicode-segmentation`) would need a new dep, which the purity test forbids and the task does not require; the joiner guard covers ZWJ sequences and modifiers, see Risks.
7. **Modules**: `render.rs` and `split.rs` instead of growing `lib.rs` to ~550 lines. `purity.rs` scans all three files and fails if `src/` gets a `.rs` file it does not scan.

Rejected alternatives: strict `end_turn`-only (hides 238 subagent answers); counting code points (matches the doc, fails if the server counts UTF-16 as some users report); structure-aware splitting by `Turn` (needs a second API taking sections; the renderer already puts a blank line between exchanges, so splitting the string at `\n\n` gives the same boundaries with one simple function that the hub can reuse for other text, e.g. TASK-014 permission previews); a `RenderOptions` struct (no caller needs options today).

## 3. Steps

### Step 1. Baseline

```
cargo fmt --all -- --check
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace
```
All must pass before any change (27 tests at premise-challenge time). Files this task touches, nothing else:
- `crates/transcript/src/lib.rs` (edit)
- `crates/transcript/src/render.rs` (new)
- `crates/transcript/src/split.rs` (new)
- `crates/transcript/tests/fixtures/final_answer.jsonl` (new, copied)
- `crates/transcript/tests/parse_fixtures.rs`, `tests/parse_tolerance.rs`, `tests/purity.rs` (edit)
- `crates/transcript/tests/render.rs`, `tests/split.rs` (new)

The reference files are in `P/proto/transcript/` with the same relative paths. Copying them byte-for-byte is the intended way; the steps below say what each change is, so a reviewer can check the copy.

### Step 2. `crates/transcript/src/lib.rs`: carry `stop_reason`, declare modules

Exact diff is in `P/proto.diff` (first hunk set, 4 hunks):
- After `use serde_json::Value;` add `mod render; mod split;` and
  `pub use render::{IN_PROGRESS_MARKER, render_brief, render_full};`
  `pub use split::{Chunks, SplitLimits, TELEGRAM_TEXT_LIMIT, split_for_telegram, telegram_len};`
- `Turn`: add last field `pub stop_reason: Option<String>` with a doc comment: string `message.stop_reason`, `None` when absent or null; subagent transcripts set it only on the last record of a response.
- `RawMessage`: add `stop_reason: Value` (the struct is already `#[serde(default)]`, so a missing key is `Value::Null`; any JSON type deserializes into `Value`, so a wrong type cannot drop the turn).
- `to_turn`: bind `let message = record.message?;`, map `Value::String(reason) => Some(reason), _ => None`, match on `message.content` instead of `record.message?.content`, set `stop_reason` in the returned `Turn`.

Nothing else in `lib.rs` changes. Crate-level lint `deny(unwrap_used, expect_used, panic)` then covers the new modules too.

### Step 3. `crates/transcript/src/render.rs` (new, 237 lines in the reference)

Public: `IN_PROGRESS_MARKER: &str = "в работе…"`, `render_brief(&[Turn]) -> String`, `render_full(&[Turn]) -> String`. Private `render(turns, full)`:

Pre-passes, all O(n): `tool_calls: HashMap<&str tool_use_id, &str name>`; `agent_ids: HashMap<&str tool_use_id, &str agent_id>` from `ToolResult.agent_id`; `tool_after: Vec<bool>` computed backward, true when a `ToolUse` appears in this turn or later before the next prompt.

Per block, in turn order:
- User `Text` -> shown as a prompt `> {text}` only if `prompt_text` accepts it: trimmed, non-empty, and either `!is_meta` or a meta `<channel ...>body</channel>` (then only `body` is shown; Telegram-originated prompts must stay visible, other meta records are hidden in both modes). A prompt after earlier output is preceded by a blank line, which is the exchange separator the splitter prefers.
- Assistant `Text` (trimmed, empty skipped): `is_final` = `end_turn` -> true, `tool_use` -> false, anything else -> `!tool_after`. Brief prints only final text; full prints all text.
- `ToolUse` (both modes, exactly one line): `Agent` -> `↳ {subagent_type|agent}[ {agent_id}][: {description}]`, agent id from the result in the same slice when present. Any other tool -> `• {name}: {summary}` where summary is the first non-empty string among input keys `description, file_path, notebook_path, pattern, url, query, command, skill`, whitespace collapsed, cut to 120 chars; no summary -> `• {name}`. Full adds the input on the next line: compact JSON (`Value` Display, keys sorted since `serde_json` has no `preserve_order`), cut to 500 chars, indented by two spaces.
- `ToolResult`: brief prints nothing. Full prints `  ← {tool name|tool}: {content}` (label `error` when `is_error`), content trimmed and cut to 1500 chars, every line indented by two spaces.
- Truncation keeps the first N chars (char boundary, via `char_indices().nth(N)`) and appends `… [+{dropped} chars]`.
- Marker: track `finished` (prompt -> false unless it starts with `[Request interrupted by user`; final text -> true; intermediate text, tool call, tool result -> false). If anything was visible and `!finished`, append `IN_PROGRESS_MARKER` as the last line. An empty slice or a slice of hidden meta turns returns `""`.

Output for the new fixture (asserted exactly in the tests):
```
> Check the build 🚀 and explain.
• Bash: Run workspace tests
All 27 tests pass. Готово.

> Explore the crate
↳ Explore a0000000000000002: Explore crate
The crate has three modules.
```

### Step 4. `crates/transcript/src/split.rs` (new, 154 lines in the reference)

Public:
- `TELEGRAM_TEXT_LIMIT: usize = 4096`.
- `telegram_len(&str) -> usize` = sum of `char::len_utf16` (doc comment states why UTF-16, see Understanding).
- `SplitLimits { chunk_len: usize, max_chunks: usize }`, `Default` = `{4096, 4}`. `chunk_len < 2` is treated as 2, so one astral char always fits and the loop always progresses.
- `Chunks { chunks: Vec<String>, prefer_file: bool }`.
- `split_for_telegram(text, limits) -> Chunks`.

Algorithm: `text.split("\n\n")`; a paragraph with `telegram_len <= limit` goes to the packer with separator `"\n\n"`; else its lines (`split('\n')`) go with separator `"\n"`; a line over the limit is cut in a loop with `hard_cut`, pieces added with separator `""`. The packer appends to the current chunk while `current_len + separator + piece <= limit`, otherwise flushes and starts a new chunk; whitespace-only pieces and chunks are dropped. `hard_cut(line, limit)`: largest char-boundary prefix within `limit` UTF-16 units; if it is not the whole line, cut after the last whitespace if it lies in the second half of the window, else step back up to 16 chars while the char before or at the cut is a joiner (`U+200D`, `U+FE00..FE0F`, `U+20E3`, `U+1F3FB..1F3FF`, `U+E0020..E007F`, `U+0300..036F`), falling back to the plain boundary. No `unwrap`, no indexing that can panic (all slicing at indices from `char_indices` / `len_utf8`).

### Step 5. Fixture `final_answer.jsonl`

Copy `P/fixtures/final_answer.jsonl` byte-for-byte to `crates/transcript/tests/fixtures/final_answer.jsonl` (LF, UTF-8 without BOM, 12 lines, SHA-256 `1826c0f923a9d0e6e5491cf9ad7ff9a61cf463154f5f54926d4dda7ac06880b7`). Do not regenerate it; the implementer has no access to `~/.claude`. Built by `P/make_fixture.py` with the TASK-005 method (every string redacted, key names and JSON types kept, semantic fields restored with fake values) from real record shapes; privacy scan with the TASK-005 `scan_fixtures.py`: 0 hits (`P/scan_fixtures.out.txt`).

Content: user prompt with an emoji; assistant intermediate text (`tool_use`) and a Bash `tool_use` sharing `message.id`; tool result; thinking record (`end_turn`, SECRET markers) and final text (`end_turn`, Cyrillic) sharing `message.id`; meta `<local-command-caveat>`; meta `<channel ...>` prompt; `Agent` tool_use; result with `toolUseResult.agentId = a0000000000000002`; final text (`end_turn`); one `attachment` record.

Verify: `git diff --no-index --exit-code maw/tasks/in_progress/TASK-006/scratch/planner/fixtures/final_answer.jsonl crates/transcript/tests/fixtures/final_answer.jsonl` exits 0.

### Step 6. Update existing tests

- `tests/parse_fixtures.rs`: `turn()` sets `stop_reason: None`; new helper `stopped(reason, turn)`; wrap the assistant turns of `plain_text`, `tool_use_result` (4) and `thinking_ai_title` in `stopped("tool_use", ...)` (this is the "test on an existing fixture" for the parser change: `tool_use` read, `sidechain` null -> `None`); `null_string_fields_fixture` literal gets `stop_reason: None`; add `FINAL_ANSWER` to `ALL` (so the privacy and thinking checks cover it) and a test `final_answer_fixture_stop_reasons` asserting the 10 `(role, stop_reason)` pairs.
- `tests/parse_tolerance.rs`: add `stop_reason_is_tolerant`: `"end_turn"`, `"tool_use"` read; absent, `null`, `7`, `{"a":1}` give `None` and the turn is still parsed.
- `tests/purity.rs`: `SOURCES` = `lib.rs`, `render.rs`, `split.rs` (all via `include_str!`), the forbidden-token check runs over each; new test `every_source_file_is_scanned` lists `src/` with `std::fs::read_dir` (allowed in test code) and asserts it equals `SOURCES`. Dependency test unchanged.

### Step 7. New tests

`tests/render.rs` (9 tests): exact brief and full of `final_answer.jsonl`; `tool_use_result.jsonl` brief is exactly one line per call with no inputs/results and full contains inputs, results and the `error` label; no SECRET marker in brief or full of any fixture; unfinished tail: `thinking_ai_title.jsonl` brief is `> Why does the parser test flake?\nв работе…` (intermediate text hidden, shown in full), prompt-only, pending tool call, tool result without reply; finished exchange has no marker, empty slice is `""`, an interrupt prompt ends without marker; null stop reason: `sidechain.jsonl` shows its answer, null text followed by a tool call is hidden; full truncation of a 5000-char Cyrillic result says `… [+3500 chars]`; slices: `render_brief(&t[..6])` + blank line + `render_brief(&t[6..])` equals the whole brief (incremental use); performance: synthetic 5000 turns (1250 x prompt/Bash/result/end_turn) brief + full + split, best of 3, must take < 2 s (measured ~40 ms debug), and 20000 turns must take < 8x the 5000-turn time + 50 ms (measured 4.0-4.3x; an injected O(n) scan per line measured ~12x and failed, `P/mutation_quadratic.out.txt`, `P/timing_linear.out.txt`).

`tests/split.rs` (10 tests): `telegram_len` of ASCII, Cyrillic, astral emoji (2), ZWJ family (8); short text is one chunk, empty and blank give no chunks; exactly 4096 fits, 4097 gives 2; paragraph then line boundaries preferred; 50 KB ASCII line gives exactly 13 chunks and `prefer_file`, 50 KB Cyrillic gives 7, both valid and deterministic; `a*4095 + 🚀 + b` gives `[a*4095, "🚀b"]`; ZWJ family placed at every offset near the boundary stays whole in one chunk; 5000 astral emoji give 3 chunks, the first exactly 4096 units; chunk limits 0..3 still progress; `max_chunks` threshold is configurable. Shared check `assert_valid`: every chunk `telegram_len <= 4096` and `chars().count() <= 4096`, non-blank, non-whitespace content of the concatenation equals the input's, and a second call returns an equal `Chunks`.

### Step 8. Verify

```
cargo fmt --all -- --check
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace
cargo tree -p transcript --edges normal --depth 1
git diff --no-index --exit-code maw/tasks/in_progress/TASK-006/scratch/planner/fixtures/final_answer.jsonl crates/transcript/tests/fixtures/final_answer.jsonl
```
Expected: clean; transcript tests `parse_fixtures` 10, `parse_tolerance` 15, `purity` 3, `render` 9, `split` 10 (47), cctg tests unchanged; only `serde`, `serde_json` under `transcript`; fixture diff exit 0; `git status` shows only the Step 1 files plus the pre-existing untracked `.claude/` and task artifacts.

### Acceptance criteria -> evidence

| Criterion | Where |
|---|---|
| chunks <= 4096, no cut inside UTF-8 or surrogate pair | `split.rs` UTF-16 length + char-boundary cuts; `tests/split.rs` `assert_valid`, emoji/ZWJ/astral tests |
| brief one line per tool call, full adds inputs and truncated results | render Step 3; `brief_has_one_line_per_tool_call_and_no_io`, `final_answer_fixture_brief_and_full`, `full_truncates_long_results_safely` |
| no thinking in any mode | no thinking variant in `Block`; `thinking_never_rendered` over all 7 fixtures |
| 50 KB block and emoji on boundary: deterministic chunks or "file" | `fifty_kb_block_is_deterministic_and_prefers_file`, `emoji_on_the_boundary_is_never_cut`, determinism in `assert_valid` |
| 5000 turns, not quadratic, fixed time budget | `five_thousand_turns_render_in_linear_time` |
| public render over a set of turns | `render_brief/render_full(&[Turn])`; `slices_render_independently` |
| parser keeps `stop_reason` | Step 2; `parse_fixtures` (existing fixtures), `stop_reason_is_tolerant`, `final_answer_fixture_stop_reasons`; all old tolerance tests still pass |
| brief answer only from `end_turn`, `tool_use` text hidden in brief, shown in full | `is_final`; `final_answer_fixture_brief_and_full`, `finished_exchanges_have_no_marker` |
| unfinished tail -> `в работе…` | `unfinished_tail_is_marked_in_progress` (real fixture tail + synthetic) |
| existing tests pass | Step 8 |

## 4. Risk areas

- **Answer rule deviates from the literal criterion for non-`end_turn`, non-`tool_use` records.** `null` / `stop_sequence` / `refusal` text counts as the answer when no tool call follows. Without it, subagent answers and API-error messages vanish from brief. Every record with `end_turn` or `tool_use` follows the criterion exactly. Reviewers should confirm this reading (see Open questions 1).
- **Incremental push (TASK-016).** Rendering a slice that ends mid-exchange appends `в работе…` each time; a slice that starts with a tool result renders its tool line only if the call is in the slice (agent ids are looked up inside the slice only). TASK-016 decides slice boundaries (e.g. push after a final answer or per exchange); the library behaviour is deterministic and tested.
- **Length unit is conservative, not proven.** If Telegram really counts code points, emoji-dense chunks are smaller than needed; never too big. If Telegram counted something larger than UTF-16 (no evidence), the hub's 400 fallback in TASK-009 still applies.
- **Grapheme clusters.** Without `unicode-segmentation`, a regional-indicator flag pair or a long Indic cluster can still be cut in the middle when a single line exceeds 4096 units with no whitespace. Such a cut is valid UTF-8/UTF-16 (the criterion holds) but looks odd. The ZWJ/modifier guard covers the common emoji cases and is tested.
- **Timing test flakiness.** Budget is 50x above the measured debug time and the ratio bound is ~1.9x above measured linear behaviour; on a very loaded CI machine the ratio test is the one that could flake. Best-of-3 reduces noise.
- **Line endings.** `core.autocrlf=true`, no `.gitattributes`. A CRLF checkout of the fixture is handled by `parse` (`lines()` + trim); rustc normalizes CRLF in test sources, so the multi-line expected strings stay LF.
- **Public struct change.** `Turn` gains a field; struct literals elsewhere break at compile time. Only this crate's tests build `Turn` today.
- **Noise in brief.** Non-meta `<task-notification>` (751) and `<command-name>` user records render as prompts. Hiding them is a product decision, not in the criteria (Open question 2).

## 5. Open questions

1. Confirm the null/other stop-reason fallback (structural) instead of literal "end_turn only". Evidence: 238 of 530 subagent transcripts end in a null-stop answer. Default in this plan: the fallback.
2. Should brief hide or collapse system-generated non-meta user records (`<task-notification>`, `<command-name>`, `<local-command-stdout>`, `<bash-stdout>`)? Default: shown as prompts, unchanged (no criterion asks for it; one line per rule later if wanted).
3. Default `max_chunks = 4` for `prefer_file` (4 messages is 1/5 of the 20/min group budget from TASK-008). Hub config can override through `SplitLimits`. Confirm or pick another default.
4. Marker text `в работе…` is user-facing Russian inside an English code base; it is a `pub const` so the hub can compare or replace it. Confirm wording.
