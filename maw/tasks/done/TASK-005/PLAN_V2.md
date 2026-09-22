# PLAN V2 — TASK-005: tolerant JSONL parser

## 1. Review notes

1. **The proposed shared `RawRecord` can discard a valid turn because of a field that `parse` does not need.** The plan puts `ai_title: Option<String>` into the same raw record used by both public functions. A throwaway build under `scratch/disconfirmation_probe/` tested a valid user record with string content and `"aiTitle": 5`; `serde_json` rejected the whole record with `invalid type: integer 5, expected a string`. Thus `parse` would silently lose valid content. The prepared fixtures do not contain this adversarial shape, so fixture success does not disprove it. The counter-example holds. `parse` and `ai_title` need separate minimal raw record types.

2. **The flat all-variants `RawBlock` has the same cross-variant coupling.** A text block `{"type":"text","text":"keep me","id":5}` is valid for the text variant, but the proposed flat struct tries to deserialize irrelevant `id` as `String` and drops the whole block. The same throwaway probe reproduced the failure. Deserializing each array item independently is correct, but each item should use variant-specific fields. An internally tagged enum with a unit `#[serde(other)]` variant accepts this exact text block and ignores the irrelevant `id`. Serde explicitly supports `#[serde(other)]` on internally tagged enums ([variant attributes](https://serde.rs/variant-attrs.html)); the original plan's objections concern whole-array or untagged deserialization and do not apply to a per-item tagged enum.

3. **`#[serde(default)]` was credited with more tolerance than it provides.** It supplies a value for a missing field; it does not turn a present value of the wrong JSON type into the default ([Serde field attributes](https://serde.rs/field-attrs.html)). The revised design therefore omits fields irrelevant to a function/variant and uses `Value` only for deliberately polymorphic fields (`message.content`, `input`, result content, `toolUseResult`, and tolerant flags).

4. **The claim that thinking text is “never even allocated” is false for the proposed implementation.** `RawMessage.content: serde_json::Value` first constructs the entire content tree, including `thinking` and `signature` strings. What the design can and must guarantee is that no public `Block`, `Turn`, or function result represents or returns them. Avoiding the intermediate allocation would require a custom streaming visitor and is not justified by this task.

5. **The deep-nesting test in the plan does not exercise the parser's recursive field.** A root string consisting only of 100,000 `[` characters can be rejected immediately because a record object is expected. Replace it with a syntactically complete user/assistant record whose `message.content` or block `input` contains nesting beyond serde_json's default recursion limit. The current serde_json documentation warns that disabling the limit requires separate stack-overflow protection; this crate must leave the limit enabled ([serde_json `Deserializer`](https://docs.rs/serde_json/latest/serde_json/struct.Deserializer.html#method.disable_recursion_limit)).

6. **The proposed fixture privacy regression is narrower than the actual audit.** The five prepared fixtures were read in full and their record counts, flags, content forms, expected text/tool values, fixed demo IDs, and title order match the table in the original plan. No private home path, `.claude` path, bot-token-shaped value, or `-100…` Telegram id was found. However, the planned test checks only four substrings, while `scratch/scan_fixtures.py` checks more path/name/token patterns. The committed test should cover generic Windows home paths, Unix home paths, `.claude`, Telegram supergroup ids, and Telegram bot-token shape without adding a regex dependency.

7. **The purity check should supplement, not substitute for, compiler and dependency checks.** A source-substring test is useful for the explicit acceptance criterion, but it cannot prove absence of all indirect panics or IO. Keep the library-level Clippy restrictions, run Clippy, inspect `cargo tree -p transcript`, and retain the small source guard. Official Clippy documents `unwrap_used` and `panic` as allow-by-default restriction lints, so explicitly denying them in production library code is appropriate ([Clippy `unwrap_used`](https://rust-lang.github.io/rust-clippy/master/index.html#unwrap_used), [Clippy `panic`](https://rust-lang.github.io/rust-clippy/master/index.html#panic)).

8. **The `uuid` open question is speculative and should not expand this task.** The actual pending TASK-016 persists a byte offset for deduplication and does not require record UUIDs. Do not model `uuid` in TASK-005. The `ToolResult.agent_id` field remains justified by the normative future brief renderer (`Agent` must render with its spawned id), but it should be documented as record-level metadata observed in the prepared real-shape fixture.

9. **Verified claims that remain valid:** TASK-002 is complete; the workspace has exactly `cctg` and `transcript`; `crates/cctg` does not depend on `transcript` yet; `crates/transcript` currently contains only its crate doc comment and depends directly only on `serde` and `serde_json`. On rustc/cargo 1.95.0, baseline `cargo fmt --check`, `cargo clippy --workspace --all-targets -- -D warnings`, and `cargo test --workspace` pass (one cctg unit test, one cctg integration test, zero transcript tests).

## 2. Updated understanding

- This task changes only `crates/transcript/**`. No workspace, binary crate, hub, filesystem, or network work is needed.
- The parser is line-oriented and lossy by contract: each complete JSONL line is independent; malformed lines, non-object JSON, ignored record types, and malformed/unknown blocks are skipped without aborting earlier or later valid turns.
- Only records whose top-level `type` is `user` or `assistant` can become turns. One JSONL record becomes at most one `Turn`; split assistant API messages remain separate turns in file order.
- For both roles, `message.content` may be a string or an array. A string becomes one public `Block::Text`. Array elements are decoded independently, so a bad element cannot discard valid siblings.
- The public model needs `Role`, visible blocks (`Text`, `ToolUse`, `ToolResult`), `is_meta`, and `is_sidechain`. It must contain no thinking variant or accessor. A turn with no visible blocks is omitted, so thinking-only records disappear.
- `tool_result.content` is polymorphic: a JSON string is preserved; for an array, only string-valued `text` items are joined with newline and other item kinds are ignored. Missing or non-boolean `is_error` safely means `false`.
- For Agent results, the fixture confirms the spawned id is at record-level `toolUseResult.agentId`; preserve it as optional `ToolResult.agent_id`. Do not parse ids from prose.
- `ai_title(&str)` is a separate pure scan that returns the first non-empty string `aiTitle` from an `ai-title` record. It does not create a turn.
- The prepared fixtures contain 10/8/5/3/3 records respectively for `plain_text`, `tool_use_result`, `thinking_ai_title`, `sidechain`, and `string_content`; their expected parsed results in the original plan are accurate. They use only fixed demo paths and fake identifiers and passed the supplied scan (`hits 0`).
- `parse(&str)` cannot directly accept invalid UTF-8 bytes. The fuzz-like byte test must explicitly pass `String::from_utf8_lossy(bytes)` to the API and verify that conversion plus parsing does not panic.

## 3. Revised approach

Keep the implementation in the existing `crates/transcript/src/lib.rs`, with no dependency changes.

Public API:

```rust
pub enum Role { User, Assistant }

pub enum Block {
    Text(String),
    ToolUse {
        id: String,
        name: String,
        input: serde_json::Value,
    },
    ToolResult {
        tool_use_id: String,
        content: String,
        is_error: bool,
        agent_id: Option<String>,
    },
}

pub struct Turn {
    pub role: Role,
    pub blocks: Vec<Block>,
    pub is_meta: bool,
    pub is_sidechain: bool,
}

pub fn parse(jsonl: &str) -> Vec<Turn>;
pub fn ai_title(jsonl: &str) -> Option<String>;
```

Derive `Debug`, `Clone`, and `PartialEq` on public model types; also derive `Copy` and `Eq` for `Role`. Document that one turn corresponds to one JSONL record, blocks are non-empty, result-array text is newline-joined, Agent ids come from record-level metadata, and thinking is intentionally unrepresentable.

Use two private top-level types rather than a shared one:

- `RawTurnRecord`: only `type`, `message`, `isMeta`, `isSidechain`, and `toolUseResult`.
- `RawTitleRecord`: only `type` and `aiTitle`.

Both derive `Deserialize` and use `#[serde(default)]` for their fields. Keep the polymorphic/soft fields as `Value` and interpret them with `as_bool`/`as_str`, defaulting safely when the shape is wrong. This prevents an irrelevant malformed title from breaking `parse`, and a malformed turn-only field from breaking `ai_title`.

Represent array blocks with a private internally tagged enum, deserializing **one `Value` at a time**:

```rust
#[derive(Deserialize)]
#[serde(tag = "type")]
enum RawBlock {
    #[serde(rename = "text")]
    Text { #[serde(default)] text: String },
    #[serde(rename = "tool_use")]
    ToolUse {
        #[serde(default)] id: String,
        #[serde(default)] name: String,
        #[serde(default)] input: Value,
    },
    #[serde(rename = "tool_result")]
    ToolResult {
        #[serde(default)] tool_use_id: String,
        #[serde(default)] content: Value,
        #[serde(default)] is_error: Value,
    },
    #[serde(other)]
    Ignored,
}
```

Unknown variants, including `thinking`, `redacted_thinking`, and `image`, become `Ignored`. A malformed known block produces an error only for that item and is skipped by `filter_map`; valid siblings survive. Variant-specific fields mean an irrelevant wrong-typed field does not poison a valid block. The intermediate `Value` may contain thinking data in memory, but no mapping copies it into the public model and no public API can return it.

Process `jsonl.lines()` in order, trim each line, skip blank lines, and call `serde_json::from_str(...).ok()` per line. Do not return an error type: silent loss of a bad line is the explicit parser contract. Leave serde_json's recursion limit enabled. Keep production lint guards for explicit panic/unwrap/expect, and do not add IO calls.

## 4. Revised steps

### Step 1 — Establish the exact change set and baseline

1. Confirm TASK-002 remains complete and the working tree has no overlapping user edits under `crates/transcript/**`.
2. Record the baseline results of:
   - `cargo fmt --all -- --check`
   - `cargo clippy --workspace --all-targets -- -D warnings`
   - `cargo test --workspace`
3. Limit implementation changes to:
   - `crates/transcript/src/lib.rs`
   - `crates/transcript/tests/fixtures/*.jsonl`
   - `crates/transcript/tests/parse_fixtures.rs`
   - `crates/transcript/tests/parse_tolerance.rs`
   - `crates/transcript/tests/purity.rs`

### Step 2 — Copy and audit the prepared fixtures

1. Copy byte-for-byte from `maw/tasks/in_progress/TASK-005/scratch/fixtures/` into `crates/transcript/tests/fixtures/`:
   - `plain_text.jsonl`
   - `tool_use_result.jsonl`
   - `thinking_ai_title.jsonl`
   - `sidechain.jsonl`
   - `string_content.jsonl`
2. Verify the source and destination directories are byte-identical with `git diff --no-index` (an exit code of 1 means a real difference; do not ignore it).
3. Re-run a fixture-only audit that validates every line as JSON and rejects generic private-home markers (`C:\\Users\\`, `/home/`, `.claude`), Telegram supergroup ids (`-100` followed by digits), and bot-token-shaped strings (8–10 digits, colon, then at least 30 token characters). Do not read live transcripts during implementation.

### Step 3 — Define the public model and production lint boundary

1. Keep the existing crate documentation line.
2. Add `#![cfg_attr(not(test), deny(clippy::unwrap_used, clippy::expect_used, clippy::panic))]` so production library code is checked while integration tests can use assertion helpers normally.
3. Add the public `Role`, `Block`, and `Turn` types and the two public pure functions exactly as described in the revised approach.
4. Add concise rustdoc documenting the record-to-turn relationship, non-empty block invariant, result text normalization, Agent id source, and absence of thinking from the public model.

### Step 4 — Implement independent tolerant record parsing

1. Add `RawMessage { content: Value }` with defaults.
2. Add `RawTurnRecord` containing only turn-required fields. Store `isMeta`, `isSidechain`, and `toolUseResult` as defaulted `Value`; interpret non-booleans/non-objects as safe absence/default rather than failing the record.
3. Add `RawTitleRecord` containing only defaulted `type` and `aiTitle` fields; use `Value` for `aiTitle` so a malformed title line is skipped without affecting subsequent titles.
4. Do not create a shared typed record containing both turn and title fields.
5. Implement separate private line iterators/helpers for the two raw types, or one generic line helper if it stays local and simple. Each line must fail independently.

### Step 5 — Implement per-item block decoding and mapping

1. Add the internally tagged private `RawBlock` enum with `Text`, `ToolUse`, `ToolResult`, and `#[serde(other)] Ignored` variants.
2. Deserialize each array element separately from `Value`; on failure or `Ignored`, return `None` for that element only.
3. Convert string content directly to one `Block::Text` for both user and assistant records.
4. Convert result content as follows:
   - string → unchanged;
   - array → collect only items with `type == "text"` and string `text`, joined by `\n`;
   - any other shape → empty string.
5. Interpret `is_error` as true only for JSON boolean `true`; absent, null, string, or numeric values become false.
6. Extract optional `agent_id` only from `RawTurnRecord.toolUseResult.agentId` when it is a string, and attach it to tool-result blocks in that record. Do not scrape result prose.
7. Drop records whose mapped visible block vector is empty. Preserve file order; do not merge records by `message.id`.
8. Implement `ai_title` as its own scan returning the first non-empty string title.

### Step 6 — Add exact fixture tests

Use `include_str!` so fixtures are embedded at compile time and the production crate performs no IO. Assert full values, not only counts:

1. `plain_text.jsonl`: exactly two turns (user string text and assistant array text), both flags false, no title.
2. `tool_use_result.jsonl`: exactly eight turns with all four tool-use/result pairs; assert ids, names, full inputs, normalized result strings, default/true `is_error`, and only the Agent result's `agent_id`.
3. `thinking_ai_title.jsonl`: exactly two visible turns; first non-empty title wins; input contains both secret markers but formatted/debug public results contain neither marker.
4. `sidechain.jsonl`: ignored attachment; exactly two turns with `is_sidechain == true`.
5. `string_content.jsonl`: exactly three user turns; assert both meta flags, non-meta flag, array conversion, and the exact non-ASCII `Привет` string.
6. Add a fixture privacy test using the broader checks from Step 2. The test itself must not depend on live paths, ids, environment variables, or external files.

### Step 7 — Add focused tolerance and regression tests

Cover every acceptance criterion with explicit assertions:

1. Empty, whitespace-only, and ignored-record-only inputs return an empty vector and no title.
2. An unknown record between two valid turns preserves both valid turns.
3. An invalid JSON line in the middle and a truncated final line preserve all earlier and later complete valid lines as applicable.
4. An unknown block and a malformed known block beside a valid block remove only themselves; valid siblings and earlier turns remain.
5. Regression for the disconfirmed design: a valid user record with irrelevant `"aiTitle": 5` still yields its text turn.
6. Regression for the flat-block flaw: `{"type":"text","text":"keep me","id":5}` remains a text block; a truly malformed text field (`"text":5`) is skipped without losing a valid sibling.
7. Both string and array content work for both user and assistant. Null, numeric, missing, or invalid `message` shapes are skipped without panic.
8. `isMeta` and `isSidechain` true/false/missing are mapped correctly; wrong-typed flags safely default false without dropping visible content.
9. Tool-result string/array/unknown content, missing/wrong-typed `is_error`, and ignored `tool_reference` array items behave as documented.
10. CRLF input produces the same turns as LF input.
11. `ai_title` skips malformed/empty title records, returns the first later non-empty string, and returns `None` when absent.
12. Run at least twelve garbage inputs through both public functions, including scalar JSON, incomplete JSON, embedded NUL/control/replacement characters, a 1 MiB junk line, missing fields, and `String::from_utf8_lossy(&(0..=255).collect::<Vec<_>>())`. Plain calls are sufficient: any panic fails the test.
13. Build a **complete** record with nesting deeper than serde_json's default recursion limit inside `message.content` or a block input. Assert it is skipped without panic and surrounding valid lines survive. Do not enable `unbounded_depth`.

### Step 8 — Add purity guards

1. In `tests/purity.rs`, embed `src/lib.rs` with `include_str!` and reject explicit runtime IO/process/environment APIs and explicit `unwrap`, `expect`, `panic`, `unreachable`, and `todo` calls in production source.
2. Inspect `cargo tree -p transcript --prefix none` and confirm no new dependency was added; the only direct dependencies remain `serde` and `serde_json`.
3. Keep all fixture loading in integration tests via `include_str!`; do not put file reads in the library.

### Step 9 — Format and verify the complete workspace

1. Run `cargo fmt --all`, then `cargo fmt --all -- --check`.
2. Run `cargo clippy --workspace --all-targets -- -D warnings`.
3. Run `cargo test --workspace`.
4. Run the fixture privacy audit and the source/dependency purity checks once more.
5. Inspect `git diff --check`, `git diff -- crates/transcript`, and `git status --short`. Confirm only the intended `crates/transcript/**` files changed, apart from pre-existing unrelated untracked files.

Acceptance mapping:

| Criterion | Concrete coverage |
|---|---|
| Unknown record/block and truncated last line preserve turns, no panic | Step 7.2–7.6 |
| Empty/ignored-only input is empty | Step 7.1 |
| Required anonymized fixtures and no private data | Steps 2 and 6 |
| Thinking never leaves a public API | Public model in Steps 3/5; marker test in Step 6.3 |
| No IO and no input `unwrap`/`expect` | Steps 3.2 and 8; Clippy in Step 9 |
| Fuzz-like arbitrary input does not panic | Step 7.12–7.13 |
| String and array content; `isMeta` carried | Step 6.5 and Step 7.7–7.8 |
| String-user fixture retains text | Step 6.5 |
| Existing tests pass | Baseline Step 1 and final Step 9 |

## 5. Risk areas

- **Silent lossy behavior is intentional but easy to broaden accidentally.** Keep line and block independence explicit in tests. Do not convert one malformed field into failure of the entire file or content array.
- **Intermediate thinking allocation remains.** Because `message.content` is first held as `Value`, secret thinking strings exist transiently in memory. The guarantee is non-exposure through the public model, not zero allocation. A streaming visitor would be a separate measured optimization.
- **Large line memory use.** `Value` allocates one complete line's modeled content plus the output model. Current surveyed files are modest, but a giant tool result can be expensive. Preserve serde_json's recursion limit and avoid cloning `Value` trees unnecessarily; measure before redesigning.
- **Agent id is record-level metadata.** Current surveyed records have one content block, so attaching the record's `toolUseResult.agentId` to its result is unambiguous. If future transcripts batch multiple tool results in one record, association may need `tool_use_id`-aware metadata. Document this rather than inventing a speculative abstraction now.
- **Empty known blocks.** Defaults allow a known block with missing fields to become an empty string/id/value. This is tolerant and non-panicking but may produce a semantically weak visible block. Tests must lock the chosen behavior; do not silently switch between “keep empty” and “drop malformed” during implementation.
- **Fixture privacy checks are heuristic.** The prepared fixtures have been manually and mechanically checked, but no finite pattern list proves absence of every possible secret. Keep fixtures fixed, review diffs byte-for-byte, and never regenerate them from live transcripts as part of implementation.
- **BOM handling is not required.** A UTF-8 BOM before the first JSON object causes that first line to be skipped; the parser still does not panic. Claude Code transcripts are not expected to contain BOMs. Treat support as out of scope unless a real fixture demonstrates it.
- **No message merging.** Multiple assistant records may share a message id. `parse` preserves record order and leaves final-text/grouping policy to TASK-006; consumers must not assume one `Turn` equals one API response.
- **Public API scope.** Do not add `uuid`, timestamps, cwd, session id, git branch, thinking, or renderer behavior. TASK-016 uses offsets, and TASK-006/007 own rendering/subagent composition.
