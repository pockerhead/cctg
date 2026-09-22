# PLAN — TASK-005: transcript, tolerant JSONL parser

Stage: planner (claude/opus, effort=medium). Spec: `TASK_FINAL.md` (amended after premise challenge).
Normative context read: repo `CLAUDE.md` ("Транскрипты сессий", "Субагенты"), `maw/project-context/domains/transcript.md`.

## 1. Understanding

### Code today
- `Cargo.toml` (workspace, lines 1-15): members `crates/cctg`, `crates/transcript`; edition 2024; workspace deps include `serde` (derive) and `serde_json`.
- `crates/transcript/Cargo.toml` (lines 1-8): deps `serde.workspace`, `serde_json.workspace`. No IO/network deps. Nothing to add.
- `crates/transcript/src/lib.rs`: one line, `//! Pure transcript parsing and rendering primitives.` No API, no tests.
- `crates/cctg/**`: CLI skeleton, does not use `transcript` yet. Not touched by this task.
- Baseline on this machine (2026-09-22, rustc/cargo 1.95.0): `cargo clippy --workspace --all-targets -- -D warnings` is clean; `cargo test --workspace` passes (0 tests in transcript, per PREMISE_CHALLENGE.md).

### Real input shape (measured, not assumed)
Probe `scratch/survey_shapes.py` over all 13 top-level + 14 subagent jsonl of this project (output `scratch/survey_shapes.out.txt`, structural counts only):
- Record types: `user`, `assistant`, plus `attachment`, `atis-latch`, `cost-state`, `file-history-delta`, `file-history-snapshot`, `last-prompt`, `mode`, `permission-mode`, `queue-operation`, `system`, `ai-title`. `cost-state` is new vs the domain list; it is skipped by the allowlist anyway.
- **Every assistant record carries exactly one content block** (1225/1225). One API message is split across several records with the same `message.id` (432 of 602 ids span >1 record). A thinking block lives in its own record.
- `message.content` shapes: assistant always array; user array 630, string 87 (of which 30 `isMeta`). `isMeta` is present on 32 user records only; absent means false.
- Block keys: `text {type,text}`, `thinking {type,thinking,signature}`, `tool_use {type,id,name,input(object),caller}`, `tool_result {type,tool_use_id,content,is_error?}`. `is_error` is absent on 182 results.
- `tool_result.content`: string 591, array 36; array items are `{type:"text",text}` (29) and `{type:"tool_reference",...}` (10).
- Agent calls: `tool_use.name == "Agent"`, input keys `description, prompt, subagent_type[, model, run_in_background]`. The spawned agent id is in the result record's top-level `toolUseResult.agentId` (12/12), and also inside the result text as prose (`agentId: a… (internal ID - do not mention to user…)`). `toolUseResult` is an object for Agent, but a string for error results.
- `ai-title` records: `{type, aiTitle, sessionId}`; up to 87 per file, all identical within a file in current data (`scratch/survey_agent_title.out.txt`).

### What consumers need (TASK-006, TASK-007)
- TASK-006 `render_brief/full`: role, text, tool name + input (for `Bash: description`, `Edit: file`, `↳ <type> <agent_id>`), tool result text + error flag, `isMeta` flag, and a guarantee that thinking is unreachable.
- TASK-007: parses subagent jsonl with the same `parse`; needs `isSidechain`. Its own agent id and type come from the hook and `.meta.json`, not from records.
- Not needed by either: `uuid`, `parentUuid`, `timestamp`, `cwd`, `sessionId`, `gitBranch`, `message.id`, usage. They are not modelled (see Open questions for `uuid`).

## 2. Approach

Line-by-line, two-level tolerant deserialization, all in `crates/transcript/src/lib.rs` (~150 lines, one head):

1. `jsonl.lines()` (handles `\n` and `\r\n`), `trim()`, skip empty lines.
2. Each line: `serde_json::from_str::<RawRecord>(line)`; on `Err` skip the line (covers truncated last line, garbage, non-object JSON, wrong field types). No error type is exposed: the spec wants silent skipping, and no caller branches on it.
3. Allowlist on `RawRecord.kind`: `"user"` / `"assistant"`, everything else is skipped.
4. `message.content` is taken as `serde_json::Value`: `String` becomes one `Block::Text`; `Array` is mapped item by item through `RawBlock::deserialize(item).ok()`, so one malformed or unknown block drops only itself; any other shape gives no blocks.
5. `RawBlock` is a private struct with `#[serde(default)]` and **no `thinking`/`signature` fields**, so thinking text is never even allocated; `kind == "thinking"` (and `redacted_thinking`, `image`, anything unknown) maps to `None`. The public `Block` enum has no thinking variant. This is the structural guarantee for the thinking criterion.
6. A turn whose visible block list is empty (thinking-only record, null content) is dropped. Invariant for consumers: every `Turn` has at least one block.
7. `ai_title(&str) -> Option<String>`: same line loop, first record with `type == "ai-title"` and a non-empty `aiTitle`.

Why this and not the alternatives:
- Private raw structs + mapping, not public `#[derive(Deserialize)]` on `Turn`/`Block`: the public model stays independent of the jsonl schema, and thinking cannot leak through a derived variant.
- Not `#[serde(tag = "type")]` enums with `#[serde(other)]`: an internally tagged enum fails the whole content array on one variant with wrong field types, and `untagged` gives poor errors and surprising matches (serde issues [#2672](https://github.com/serde-rs/serde/issues/2672), [#2447](https://github.com/serde-rs/serde/issues/2447); [Serde enum representations](https://serde.rs/enum-representations.html)). A flat struct with `#[serde(default)]` plus a string match is simpler and per-block tolerant.
- `content: Value` per block costs one allocation tree per record; real files are ~5 MB, the probe parses all 27 files instantly in release. Measure before optimizing ([CLAUDE.md]: "измеряй").
- No merging by `message.id` inside `parse`: grouping is rendering, and file order already keeps the sequence (log decision).
- Deep nesting (`[[[[…` 100k deep) is rejected by serde_json's default recursion limit with an error, not a stack overflow ([serde_json Deserializer docs](https://docs.rs/serde_json/latest/serde_json/struct.Deserializer.html), `disable_recursion_limit` is opt-in behind `unbounded_depth`); verified by the probe.

The whole design was run as a throwaway probe (`scratch/probe/src/main.rs`, output `scratch/probe.out.txt`): 1564 turns from real files, no panic, 12 Agent results with `agent_id`, 0 thinking leaks in fixtures, garbage set and truncated last line pass. (One real subagent file shows `leak=true` only because a previous agent literally typed the marker string into a Bash/Write tool input; confirmed not a thinking block. Hence tests check the marker only on fixtures.)

## 3. Steps

### Step 1. Copy the prepared fixtures
Copy byte-for-byte (do not regenerate, do not reformat) the five files from
`maw/tasks/in_progress/TASK-005/scratch/fixtures/` to `crates/transcript/tests/fixtures/`:
`plain_text.jsonl`, `tool_use_result.jsonl`, `thinking_ai_title.jsonl`, `sidechain.jsonl`, `string_content.jsonl`.

They were produced at plan time by `scratch/make_fixtures.py` from real records of this project: every string value replaced by `redacted` or a fixed demo value, key sets and JSON types kept (so unknown fields like `toolUseResult`, `usage`, `wireToolInputs`, `serverClassifierContext` are present to exercise tolerance), ids replaced by `00000000-0000-4000-8000-…`, cwd `C:\work\demo`, agent id `a0000000000000001`. `scratch/scan_fixtures.py` found 0 hits for `Users`, `.claude`, `-100…`, bot-token pattern, real session/agent ids, real `toolu_` ids (`scratch/scan_fixtures.out.txt`). Contents (one record per line):

| file | records | expected `parse` result |
|---|---|---|
| `plain_text.jsonl` | permission-mode, mode, file-history-snapshot, attachment, **user (string)**, queue-operation, **assistant text**, system, last-prompt, atis-latch | 2 turns: User `Text("Summarize the build status.")`, Assistant `Text("The build is green.")`; `ai_title` = None |
| `tool_use_result.jsonl` | 4 pairs assistant tool_use / user tool_result | 8 turns. `toolu_demo01` Bash `{command:"cargo test",description:"Run tests"}` -> result `"test result: ok"`, `is_error=false`; `toolu_demo02` Read `{file_path:"C:\\work\\demo\\src\\lib.rs"}` -> `"fn main() {}"`; `toolu_demo10` Bash -> `"Exit code 1"`, `is_error=true` (record `toolUseResult` is a string); `toolu_demo20` Agent `{description,prompt,subagent_type:"Explore"}` -> array content joined as `"Async agent launched.\nagentId: a0000000000000001"`, `agent_id=Some("a0000000000000001")`; all other results `agent_id=None` |
| `thinking_ai_title.jsonl` | ai-title "Fix flaky parser test", user string, assistant thinking (`SECRET-THINKING-MARKER…`, signature `SECRET-SIGNATURE-MARKER`), ai-title "Later retitle that must be ignored", assistant text (same `message.id` as thinking) | 2 turns: User text, Assistant `Text("The test depends on HashMap order.")`; thinking record dropped; `ai_title` = `Some("Fix flaky parser test")` |
| `sidechain.jsonl` | user string (`isSidechain:true`, `agentId`), attachment, assistant text | 2 turns, both `is_sidechain=true` |
| `string_content.jsonl` | user string `isMeta:true` (`<local-command-caveat>…`), user string non-meta `"Привет, add a test for empty input."`, user array text `isMeta:true` | 3 turns: meta string -> `is_meta=true`; plain -> `Text("Привет, add a test for empty input.")`, `is_meta=false`; array -> `Text("Array-form prompt text.")`, `is_meta=true` |

Check: `git diff --no-index maw/tasks/in_progress/TASK-005/scratch/fixtures crates/transcript/tests/fixtures` is empty.

### Step 2. Public model in `crates/transcript/src/lib.rs`
Keep the existing `//!` line. Add at crate top:
```rust
#![cfg_attr(not(test), deny(clippy::unwrap_used, clippy::expect_used, clippy::panic))]
```
Public items (derive `Debug, Clone, PartialEq`; `Role` also `Eq, Copy`):
```rust
pub enum Role { User, Assistant }

pub enum Block {
    Text(String),
    ToolUse { id: String, name: String, input: serde_json::Value },
    ToolResult { tool_use_id: String, content: String, is_error: bool, agent_id: Option<String> },
}

pub struct Turn {
    pub role: Role,
    pub blocks: Vec<Block>,   // never empty
    pub is_meta: bool,
    pub is_sidechain: bool,
}

pub fn parse(jsonl: &str) -> Vec<Turn>;
pub fn ai_title(jsonl: &str) -> Option<String>;
```
Doc comments (one line each): `Block` says thinking is never represented; `ToolResult.content` is the text of the result (array items of type `text` joined with `\n`, other item types skipped); `ToolResult.agent_id` is the id of the subagent spawned by an `Agent` call, from record-level `toolUseResult.agentId`; `Turn.blocks` is never empty; `Turn` itself: "one Turn per jsonl record; one API response may span several Turns".

### Step 3. Private raw types and mapping in the same file
- `RawRecord` with `#[derive(Deserialize, Default)] #[serde(default)]`: `#[serde(rename = "type")] kind: String`, `message: Option<RawMessage>`, `#[serde(rename = "isMeta")] is_meta: bool`, `#[serde(rename = "isSidechain")] is_sidechain: bool`, `#[serde(rename = "toolUseResult")] tool_use_result: serde_json::Value`, `#[serde(rename = "aiTitle")] ai_title: Option<String>`. Nothing else: serde skips unknown fields by default (do not add `deny_unknown_fields`).
- `RawMessage { content: serde_json::Value }` with `#[serde(default)]`.
- `RawBlock` with `#[serde(default)]`: `kind: String` (rename `type`), `text: String`, `id: String`, `name: String`, `input: Value`, `tool_use_id: String`, `content: Value`, `is_error: Option<bool>`. **Must not have `thinking` or `signature` fields.**
- `fn records(jsonl) -> impl Iterator<Item = RawRecord>`: `lines()`, `trim()`, skip empty, `serde_json::from_str(line).ok()`. Shared by `parse` and `ai_title`.
- `fn to_block(value: Value, agent_id: Option<&str>) -> Option<Block>`: `RawBlock::deserialize(value).ok()?` then match `kind`: `"text"`, `"tool_use"`, `"tool_result"` (`is_error.unwrap_or(false)`, `agent_id.map(str::to_owned)`), `_ => None`.
- `fn result_text(&Value) -> String`: `String(s)` -> `s`; `Array` -> `text` of items with `type == "text"`, joined by `"\n"`; else empty.
- `parse`: for each record with kind user/assistant and `Some(message)`: `agent_id = record.tool_use_result.get("agentId").and_then(Value::as_str)`; content `String(s)` -> `vec![Text(s)]`, `Array(items)` -> `filter_map(to_block)`, else empty; skip if empty; push `Turn`.
- `ai_title`: `records(jsonl).find_map(|r| (r.kind == "ai-title").then_some(r.ai_title).flatten().filter(|t| !t.is_empty()))`.

Reference implementation of the same logic (validated against real data): `scratch/probe/src/main.rs`, functions `parse`, `ai_title`, `to_block`, `result_text`. Its `main` does file IO and is probe-only; nothing of it goes into the crate.

Check: `cargo build -p transcript`, `cargo clippy -p transcript --all-targets -- -D warnings`.

### Step 4. Fixture tests: `crates/transcript/tests/parse_fixtures.rs`
Fixtures loaded with `include_str!("fixtures/<name>.jsonl")` (compile-time, no runtime IO). Tests may use `unwrap`/`assert!` freely (integration tests are a separate crate, the lint is not applied there). One test per fixture asserting exactly the table in Step 1 (turn count, roles, block variants and values, flags, `ai_title`). Additionally:
- `thinking_never_escapes`: for every fixture, `format!("{:?}", parse(src))` contains neither `SECRET-THINKING-MARKER` nor `SECRET-SIGNATURE-MARKER`; and `thinking_ai_title` fixture itself does contain them (sanity that the marker is really in the input).
- `string_user_text_is_kept`: `string_content.jsonl` turn 2 text equals `"Привет, add a test for empty input."` exactly (covers non-ASCII).
- `fixtures_have_no_private_data`: for every fixture source, assert it does not contain `Users`, `.claude`, `-100`, `toolu_0` (plain `contains`, no regex crate; all four verified absent in the prepared files).

### Step 5. Tolerance tests: `crates/transcript/tests/parse_tolerance.rs`
Inline `&str` inputs, each asserting no panic and the stated result:
- empty `""`, `"\n\n"`, whitespace -> empty vec; `ai_title` None.
- only ignored types (take the 8 non-user/assistant lines of `plain_text.jsonl` via `include_str!` and filter lines not containing `"type":"user"`/`"type":"assistant"`, or write 3 short literal records like `{"type":"mode"}`, `{"type":"summary"}`, `{"type":"cost-state"}`) -> empty vec.
- unknown record type between two valid turns -> both turns kept.
- unknown block type (`{"type":"image","source":{}}`) and a malformed block (`{"type":"text","text":5}`) next to a valid text block in one record -> the record yields only the valid block; previous turns kept.
- truncated last line: full `plain_text.jsonl` content with an extra `{"type":"assistant","mess` appended -> same 2 turns as the untruncated fixture.
- `\r\n` line endings: `plain_text.jsonl` with `\n` replaced by `\r\n` -> same result.
- content shapes: assistant with string content -> one `Text`; user/assistant with `content: null`, `content: 5`, missing `message`, `message: 5` -> skipped, no panic.
- `is_error` absent -> false; tool_result with array content containing a `tool_reference` item -> only text items joined.
- garbage set (loop over at least 12 inputs, call both `parse` and `ai_title`): `"{"`, `"null"`, `"[]"`, `"\"str\""`, `"42"`, `"{\"type\":\"user\"}"`, `"{\"type\":\"user\",\"message\":{\"content\":[1,null,{\"type\":7}]}}"`, `"\u{feff}{}"`, `"\u{0}\u{1}\u{fffd}"`, `"[".repeat(100_000)`, `"{\"type\":\"assistant\",\"message\":{\"content\":[{\"type\":\"text\"}]}}"` (text defaults to empty string, which is fine), 1 MB of `"x"`, and a string from `String::from_utf8_lossy` over bytes `0..=255`. Assert no panic (plain calls; any panic fails the test).
- `ai_title`: first non-empty wins (`thinking_ai_title.jsonl`), record with `"aiTitle":""` before a real one is skipped, file without titles -> None.

### Step 6. Purity test: `crates/transcript/tests/purity.rs`
`const SRC: &str = include_str!("../src/lib.rs");` Assert `SRC` contains none of: `std::fs`, `std::io`, `std::net`, `std::process`, `std::env`, `File::`, `println!`, `eprintln!`, `.unwrap()`, `.expect(`, `panic!`, `unreachable!`, `todo!`. (Keep `lib.rs` free of an inline `#[cfg(test)]` module so the grep is exact; all tests live in `tests/`.) Together with the clippy lint in Step 2 this covers the "no IO, no unwrap/expect on input" criterion both ways (lint needs `cargo clippy`; the grep runs in plain `cargo test`).

Also assert `include_str!("../Cargo.toml")` has no `tokio`, `reqwest`, `teloxide` (one-line guard that the crate stays IO-free by dependency).

### Step 7. Verify
From repo root:
```
cargo fmt --all -- --check
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace
git status --porcelain   # only crates/transcript/** changed (plus pre-existing ?? .claude/)
```
No change to `crates/cctg`, workspace `Cargo.toml`, or `crates/transcript/Cargo.toml` is expected.

## 4. Risk areas

- **Fixture drift**: if the implementer regenerates or hand-edits fixtures, test expectations in Step 1 break or private data sneaks in. Mitigation: byte-for-byte copy, diff check in Step 1, private-data test in Step 4.
- **Silent loss by whole-record failure**: a known-type record with an unexpected type on a modelled field (for example `isMeta: "true"`, `aiTitle: 5`) fails `RawRecord` and the whole record is skipped. Modelled fields are few and stable in all 2,700+ real records, so acceptable; `content`/`toolUseResult` are `Value` precisely to avoid this on the volatile fields.
- **Multi-block records**: `toolUseResult.agentId` is attached to every `tool_result` in the record. Real data has exactly one block per record, so no ambiguity today; if Claude Code starts batching several results in one record, a non-Agent result could get an agent id. Renderer only reads `agent_id` for `Agent` calls, so the damage is nil.
- **Split assistant messages**: consumers must not assume one Turn = one API response. TASK-006 must group consecutive assistant turns itself if it needs "final assistant text". Mitigation: the `Turn` doc comment says "one Turn per jsonl record; one API response may span several Turns" (Step 2).
- **BOM**: a UTF-8 BOM at file start makes the first line fail and that record is skipped. Claude Code does not write a BOM; hub reading with `read_to_string` keeps it if present. Not handled (surgical), covered only by the no-panic garbage test.
- **`is_meta` semantics**: meta records include `<local-command-caveat>`, skill bodies, command outputs; rendering choice is TASK-006. Parser only carries the flag.
- **Performance**: `Value` for `content` and `toolUseResult` allocates the whole subtree (Read results can be large). Fine for 5 MB files; TASK-006 has a perf budget test and will show if this matters.
- **Lint scope**: `cfg_attr(not(test), deny(...))` does not guard integration tests, which is intended; `clippy::panic` also forbids `panic!` in lib code, which none of the design needs.

## 5. Open questions

1. `uuid` on `Turn`: TASK-016 (incremental push) may need a stable per-record id for dedup. Not added now (not needed by TASK-006/007, spec says minimal). Adding it later is a non-breaking field addition. Decision needed only when TASK-016 is planned.
2. `ai_title` returns the first title per spec. Current data never retitles within a file, so first == last. If Claude Code starts retitling, the topic title would stay stale; switching to last is a one-line change. Keep first per spec unless the user says otherwise.
3. `Block::ToolResult.agent_id` is the one field not literally named in the spec; it exists because the domain's `render_brief` rule `↳ <type> <agent_id>` needs it and the only other source is prose in the result text. If the reviewers consider it TASK-007 scope, it can be cut with no other change.
