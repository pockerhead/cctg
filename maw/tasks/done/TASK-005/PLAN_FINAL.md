# PLAN FINAL — TASK-005: transcript, tolerant JSONL parser

Stage: plan-reviewer-2 (claude/opus, effort=medium). Inputs: `TASK_FINAL.md`, `PLAN_V2.md` (and `PLAN.md` for the fixture table), prepared fixtures in `scratch/fixtures/`, normative domain `transcript`.

## 1. Summary

Implement the pure `transcript` library API `parse(&str) -> Vec<Turn>` and `ai_title(&str) -> Option<String>` in `crates/transcript/src/lib.rs`, with no new dependencies (only `serde`, `serde_json`). Parsing is line by line: every non-blank line is deserialized independently into a private minimal record type; lines that fail, and records whose top-level `type` is not `user`/`assistant`, are skipped silently. `message.content` is kept as `serde_json::Value`: a string becomes one `Block::Text`, an array is decoded item by item through a private internally tagged `RawBlock` enum with a `#[serde(other)] Ignored` variant, so `thinking`, `redacted_thinking`, `image` and any unknown or malformed block drops only itself. The public model (`Role`, `Block`, `Turn`) has no thinking variant, so thinking cannot leave the crate. `ai_title` is a separate scan over a separate private record type. The five prepared anonymized fixtures are copied byte-for-byte into `crates/transcript/tests/fixtures/`, and three integration test files cover fixtures, tolerance/fuzz-like inputs, and source/dependency purity. The complete design (library plus all tests below) was compiled and run by this reviewer in `scratch/rev2_crate/`: `cargo fmt --check`, `cargo clippy --all-targets -- -D warnings` and 24 tests pass (`scratch/rev2_crate.out.txt`).

## 2. Implementation steps

All paths are relative to the repo root `C:/Users/user/dev/cctg`. Task dir `T` = `maw/tasks/in_progress/TASK-005`.

### Step 1. Baseline

Run from the repo root and note the results (all three passed at review time on cargo 1.95.0):

```
cargo fmt --all -- --check
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace
```

Confirm `crates/transcript/src/lib.rs` still contains only `//! Pure transcript parsing and rendering primitives.` and `crates/transcript/Cargo.toml` has only `serde.workspace = true` and `serde_json.workspace = true` under `[dependencies]`. If either differs, stop and report.

Files this task creates or changes (nothing else):
- `crates/transcript/src/lib.rs` (rewrite)
- `crates/transcript/tests/fixtures/{plain_text,tool_use_result,thinking_ai_title,sidechain,string_content}.jsonl` (new, copied)
- `crates/transcript/tests/parse_fixtures.rs` (new)
- `crates/transcript/tests/parse_tolerance.rs` (new)
- `crates/transcript/tests/purity.rs` (new)

Do not touch the workspace `Cargo.toml`, `crates/transcript/Cargo.toml`, or `crates/cctg/**`.

### Step 2. Copy the fixtures byte-for-byte

Copy the five files from `T/scratch/fixtures/` into `crates/transcript/tests/fixtures/` without reformatting or regenerating them (they are LF, UTF-8 without BOM, one JSON object per line, trailing newline). Then:

```
git diff --no-index --exit-code maw/tasks/in_progress/TASK-005/scratch/fixtures crates/transcript/tests/fixtures
```

must exit 0 (exit 1 means a real difference; fix it). Never read `~/.claude` during implementation and never regenerate fixtures from live transcripts.

Expected content per fixture (checked by this reviewer against the files):

| file | records | expected `parse` / `ai_title` |
|---|---|---|
| `plain_text.jsonl` | 10: permission-mode, mode, file-history-snapshot, attachment, user (string content), queue-operation, assistant text, system, last-prompt, atis-latch | 2 turns: User `Text("Summarize the build status.")`, Assistant `Text("The build is green.")`; flags false; `ai_title` None |
| `tool_use_result.jsonl` | 8: four assistant tool_use / user tool_result pairs | 8 turns, see `tool_use_result_fixture` below; only the Agent result has `agent_id = Some("a0000000000000001")` |
| `thinking_ai_title.jsonl` | 5: ai-title, user string, assistant thinking (`SECRET-THINKING-MARKER…`, signature `SECRET-SIGNATURE-MARKER`), ai-title (later), assistant text | 2 turns: User `Text("Why does the parser test flake?")`, Assistant `Text("The test depends on HashMap order.")`; `ai_title` = `Some("Fix flaky parser test")` |
| `sidechain.jsonl` | 3: user string (`isSidechain:true`), attachment, assistant text | 2 turns, both `is_sidechain = true`: `"List the modules of the crate."`, `"Modules: lib, parse."` |
| `string_content.jsonl` | 3 user records | meta string `<local-command-caveat>Caveat: demo meta record.</local-command-caveat>` (`is_meta=true`); `"Привет, add a test for empty input."` (`is_meta=false`); array text `"Array-form prompt text."` (`is_meta=true`) |

### Step 3. Write `crates/transcript/src/lib.rs`

Replace the file with exactly this content (it is the verified, rustfmt-formatted version from `T/scratch/rev2_crate/src/lib.rs`; copying that file is equivalent):

```rust
//! Pure transcript parsing and rendering primitives.
#![cfg_attr(
    not(test),
    deny(clippy::unwrap_used, clippy::expect_used, clippy::panic)
)]

use serde::Deserialize;
use serde_json::Value;

/// Author of a turn, taken from the record's top-level `type`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Role {
    User,
    Assistant,
}

/// A visible content block. Thinking blocks are intentionally not representable.
#[derive(Debug, Clone, PartialEq)]
pub enum Block {
    /// Plain text; a string `message.content` becomes exactly one `Text`.
    Text(String),
    /// A tool call with its raw JSON input.
    ToolUse {
        id: String,
        name: String,
        input: Value,
    },
    /// A tool result.
    ToolResult {
        tool_use_id: String,
        /// Result text; for array content, the `text` items joined with `\n`, other items skipped.
        content: String,
        /// True only when the record says `"is_error": true`.
        is_error: bool,
        /// Id of the subagent spawned by an `Agent` call, from the record-level `toolUseResult.agentId`.
        agent_id: Option<String>,
    },
}

/// One `user` or `assistant` jsonl record. One API response may span several turns.
#[derive(Debug, Clone, PartialEq)]
pub struct Turn {
    pub role: Role,
    /// Never empty.
    pub blocks: Vec<Block>,
    pub is_meta: bool,
    pub is_sidechain: bool,
}

#[derive(Default, Deserialize)]
#[serde(default)]
struct RawTurnRecord {
    #[serde(rename = "type")]
    kind: String,
    message: Option<RawMessage>,
    #[serde(rename = "isMeta")]
    is_meta: Value,
    #[serde(rename = "isSidechain")]
    is_sidechain: Value,
    #[serde(rename = "toolUseResult")]
    tool_use_result: Value,
}

#[derive(Default, Deserialize)]
#[serde(default)]
struct RawMessage {
    content: Value,
}

#[derive(Default, Deserialize)]
#[serde(default)]
struct RawTitleRecord {
    #[serde(rename = "type")]
    kind: String,
    #[serde(rename = "aiTitle")]
    ai_title: Value,
}

#[derive(Deserialize)]
#[serde(tag = "type")]
enum RawBlock {
    #[serde(rename = "text")]
    Text {
        #[serde(default)]
        text: String,
    },
    #[serde(rename = "tool_use")]
    ToolUse {
        #[serde(default)]
        id: String,
        #[serde(default)]
        name: String,
        #[serde(default)]
        input: Value,
    },
    #[serde(rename = "tool_result")]
    ToolResult {
        #[serde(default)]
        tool_use_id: String,
        #[serde(default)]
        content: Value,
        #[serde(default)]
        is_error: Value,
    },
    #[serde(other)]
    Ignored,
}

/// Parses jsonl text into turns. Bad lines, other record types and unknown blocks are skipped.
pub fn parse(jsonl: &str) -> Vec<Turn> {
    records::<RawTurnRecord>(jsonl)
        .filter_map(to_turn)
        .collect()
}

/// Returns the first non-empty `aiTitle` of an `ai-title` record.
pub fn ai_title(jsonl: &str) -> Option<String> {
    records::<RawTitleRecord>(jsonl)
        .filter(|record| record.kind == "ai-title")
        .find_map(|record| match record.ai_title {
            Value::String(title) if !title.is_empty() => Some(title),
            _ => None,
        })
}

fn records<'a, T: Deserialize<'a>>(jsonl: &'a str) -> impl Iterator<Item = T> + 'a {
    jsonl
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty())
        .filter_map(|line| serde_json::from_str(line).ok())
}

fn to_turn(record: RawTurnRecord) -> Option<Turn> {
    let role = match record.kind.as_str() {
        "user" => Role::User,
        "assistant" => Role::Assistant,
        _ => return None,
    };
    let agent_id = record
        .tool_use_result
        .get("agentId")
        .and_then(Value::as_str);
    let blocks: Vec<Block> = match record.message?.content {
        Value::String(text) => vec![Block::Text(text)],
        Value::Array(items) => items
            .into_iter()
            .filter_map(|item| to_block(item, agent_id))
            .collect(),
        _ => Vec::new(),
    };
    if blocks.is_empty() {
        return None;
    }
    Some(Turn {
        role,
        blocks,
        is_meta: record.is_meta.as_bool().unwrap_or(false),
        is_sidechain: record.is_sidechain.as_bool().unwrap_or(false),
    })
}

fn to_block(item: Value, agent_id: Option<&str>) -> Option<Block> {
    match serde_json::from_value(item).ok()? {
        RawBlock::Text { text } => Some(Block::Text(text)),
        RawBlock::ToolUse { id, name, input } => Some(Block::ToolUse { id, name, input }),
        RawBlock::ToolResult {
            tool_use_id,
            content,
            is_error,
        } => Some(Block::ToolResult {
            tool_use_id,
            content: result_text(&content),
            is_error: is_error.as_bool().unwrap_or(false),
            agent_id: agent_id.map(str::to_owned),
        }),
        RawBlock::Ignored => None,
    }
}

fn result_text(content: &Value) -> String {
    match content {
        Value::String(text) => text.clone(),
        Value::Array(items) => items
            .iter()
            .filter(|item| item.get("type").and_then(Value::as_str) == Some("text"))
            .filter_map(|item| item.get("text").and_then(Value::as_str))
            .collect::<Vec<_>>()
            .join("\n"),
        _ => String::new(),
    }
}
```

Rules the implementer must keep if editing this code:
- No `#[cfg(test)]` module in `lib.rs` (all tests live in `tests/`; `purity.rs` rejects the token).
- Doc comments and code must not contain any token from the `purity.rs` forbidden list (Step 6), e.g. do not write "never uses std::io" in a doc comment. `.unwrap_or(..)` is allowed; `.unwrap()` / `.expect(` are not.
- Do not merge `RawTurnRecord` and `RawTitleRecord`, and do not flatten `RawBlock` into one struct with all fields: both reintroduce the "irrelevant wrong-typed field drops valid content" bug (tests 5 and 6 of Step 5 lock this).
- Do not add `deny_unknown_fields`, `uuid`, timestamps, cwd, session id, git branch, `message.id`, or any thinking representation. Do not enable serde_json `unbounded_depth`.
- Behaviour locked by tests: a known block with missing fields keeps defaults (`{"type":"text"}` gives `Text("")`); a string `message.content` always becomes one `Text`, even when empty; a record with no visible block (thinking-only, `content` null/number/`[]`, missing `message`) produces no turn; `message: null` or a non-object `message` produces no turn.

### Step 4. Write `crates/transcript/tests/parse_fixtures.rs`

Copy `T/scratch/rev2_crate/tests/parse_fixtures.rs` verbatim (verified to compile, pass, and be rustfmt-clean). It loads fixtures with `include_str!("fixtures/<name>.jsonl")` and contains:

1. `plain_text_fixture`: `parse` equals exactly the two turns from the table; `ai_title` is `None`.
2. `tool_use_result_fixture`: `parse` equals exactly 8 turns: Assistant ToolUse `toolu_demo01` `Bash` `{"command":"cargo test","description":"Run tests"}`; User ToolResult `toolu_demo01` `"test result: ok"` false None; Assistant ToolUse `toolu_demo02` `Read` `{"file_path": r"C:\work\demo\src\lib.rs"}`; User ToolResult `toolu_demo02` `"fn main() {}"` false None; Assistant ToolUse `toolu_demo10` `Bash` `{"command":"false","description":"Fail on purpose"}`; User ToolResult `toolu_demo10` `"Exit code 1"` true None; Assistant ToolUse `toolu_demo20` `Agent` `{"description":"Explore crate","prompt":"List the modules.","subagent_type":"Explore"}`; User ToolResult `toolu_demo20` `"Async agent launched.\nagentId: a0000000000000001"` false `Some("a0000000000000001")`. Use a raw string `r"C:\work\demo\src\lib.rs"` for the Windows path (a plain `"C:\work..."` literal does not compile).
3. `thinking_ai_title_fixture`: two turns from the table; `ai_title` = `Some("Fix flaky parser test")` (the later title is ignored).
4. `thinking_never_escapes`: asserts the raw fixture contains `SECRET-THINKING-MARKER` and `SECRET-SIGNATURE-MARKER`, then for every fixture `format!("{:?} {:?}", parse(src), ai_title(src))` contains neither marker.
5. `sidechain_fixture`: two turns, both `is_sidechain = true`, exact texts.
6. `string_content_fixture`: exact three turns including `"Привет, add a test for empty input."` with `is_meta = false` and both meta turns `is_meta = true`.
7. `fixtures_have_no_private_data`: for every fixture, the lowercased source does not contain `users\` (Rust literal `"users\\"`, catches both raw and JSON-escaped Windows home paths), `users/`, `/home/`, `.claude`; no Telegram supergroup id (`-100` followed by at least 6 digits, hand-coded helper `has_supergroup_id`); no bot-token shape (8 to 10 digits, `:`, at least 30 of `[A-Za-z0-9_-]`, hand-coded helper `has_bot_token_shape`); every line parses as `serde_json::Value`.
8. `privacy_detectors_fire`: the two helpers return true on obviously fake samples (`"x 123456789:AAabcdefghijklmnopqrstuvwxyz0123456 y"`, `"chat -1001234567890"`), so the privacy test cannot pass vacuously. Do not use any real chat id or token in these samples.

### Step 5. Write `crates/transcript/tests/parse_tolerance.rs`

Copy `T/scratch/rev2_crate/tests/parse_tolerance.rs` verbatim. It contains 14 tests:

1. `empty_and_ignored_only_inputs_give_nothing`: `""`, `"\n\n"`, whitespace with CRLF, three ignored-type records (`mode`, `summary`, `cost-state`), and the 8 non-user/assistant lines of `plain_text.jsonl` all give an empty vector and `ai_title == None`.
2. `unknown_record_and_bad_lines_keep_neighbours`: valid user, unknown record type, non-JSON line, valid assistant gives texts `["first", "second"]`.
3. `truncated_last_line_keeps_earlier_turns`: `plain_text.jsonl` plus `\n{"type":"assistant","mess` gives the same 2 turns as the fixture.
4. `unknown_and_malformed_blocks_drop_only_themselves`: one assistant record with `image`, `{"type":"text","text":5}`, `thinking`, an untagged block, and `{"type":"text","text":"keep me","id":5}` yields only `"keep me"`; the preceding turn stays.
5. `irrelevant_wrong_typed_fields_do_not_drop_a_turn`: user record with `"aiTitle":5,"uuid":7,"timestamp":[]` still yields `"still here"`.
6. `both_content_shapes_for_both_roles`: string and array content for user and assistant, roles and texts in order.
7. `unusable_message_shapes_are_skipped`: missing `message`, `message: null`, `message: 5`, `content: null`, `content: 5`, `content: []`, thinking-only content: all give no turn.
8. `known_block_with_missing_fields_keeps_defaults`: `{"type":"text"}` gives `Text("")`.
9. `flags_are_tolerant`: `isMeta`/`isSidechain` true, false, missing, and wrong-typed (`"true"`, `1`) map to true/false/false/false without dropping the turn.
10. `tool_result_shapes`: string content; array with a `tool_reference` item between two text items gives `"a\nb"`; object content gives `""`; `is_error: "yes"` gives false; `is_error: true` gives true; `toolUseResult` as a string, or with a non-string `agentId`, gives `agent_id None`; `{"agentId":"a1"}` gives `Some("a1")`.
11. `crlf_matches_lf`: normalizes the fixture to LF first (`replace("\r\n", "\n")`, so the test is robust if git `core.autocrlf=true` checks the fixture out with CRLF), then CRLF and LF variants parse identically, 2 turns.
12. `ai_title_rules`: empty title, numeric title, missing title and a user record carrying `aiTitle` are skipped; the first real title `"Real title"` wins over a later one; an `ai-title` record never becomes a turn.
13. `deep_nesting_is_skipped_without_panic`: a complete assistant record whose tool_use `input` is nested 200 and 100_000 levels deep, between two valid lines, is skipped (serde_json recursion limit 128) and both neighbours survive.
14. `garbage_never_panics`: 15 inputs (`{`, `}`, `null`, `[]`, `"str"`, `42`, user without message, content `[1,null,{"type":7}]`, BOM-prefixed record, NUL/control/U+FFFD, text block without text, positional array record `["user",{"content":"positional"}]`, 100_000 `[`, 1 MiB of `x`, `String::from_utf8_lossy` over bytes `0..=255`) each passed to both `parse` and `ai_title`, then all joined by `\n` once more. Only "no panic" is asserted.

### Step 6. Write `crates/transcript/tests/purity.rs`

Copy `T/scratch/rev2_crate/tests/purity.rs` verbatim. It contains:
1. `library_source_has_no_io_or_panicking_calls`: `include_str!("../src/lib.rs")` contains none of `std::fs`, `std::io`, `std::net`, `std::process`, `std::env`, `std::thread`, `File::`, `print!`, `println!`, `eprint!`, `eprintln!`, `dbg!`, `.unwrap()`, `.expect(`, `panic!`, `unreachable!`, `todo!`, `unimplemented!`, `unsafe`, `#[cfg(test)]`.
2. `library_has_only_serde_dependencies`: the `[dependencies]` section of `include_str!("../Cargo.toml")` lists exactly `serde`, `serde_json` (parses both `serde.workspace = true` and `serde = {...}` forms).

### Step 7. Verify

From the repo root:

```
cargo fmt --all
cargo fmt --all -- --check
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace
cargo tree -p transcript -e normal --depth 1 --prefix none
git diff --no-index --exit-code maw/tasks/in_progress/TASK-005/scratch/fixtures crates/transcript/tests/fixtures
git diff --check
git status --short
```

Expected: fmt and clippy clean; transcript tests `parse_fixtures` 8 passed, `parse_tolerance` 14 passed, `purity` 2 passed; existing cctg tests still pass; `cargo tree` shows only `serde` and `serde_json` under `transcript`; fixture diff exit 0; `git status` shows only the Step 1 file list as new/modified plus the pre-existing untracked `.claude/`.

## 3. Test plan

| Acceptance criterion (TASK_FINAL.md) | Test(s) |
|---|---|
| unknown record, unknown block and truncated last line keep earlier turns, no panic | tolerance 2, 3, 4, 5, 7 |
| empty and ignored-only input give an empty vector | tolerance 1 |
| fixtures: plain text, tool_use + tool_result, thinking + ai-title, sidechain; no private path, token, Telegram id | Step 2 copy + fixtures 1, 2, 3, 5, 7, 8 |
| `thinking` parsed only enough to never expose it; no public API returns it | public model has no thinking variant (Step 3); fixtures 4; tolerance 4, 7 |
| no IO, no `unwrap()`/`expect()` on input (clippy lint or grep test) | `#![cfg_attr(not(test), deny(clippy::unwrap_used, clippy::expect_used, clippy::panic))]` + clippy in Step 7; purity 1 and 2 |
| fuzz-like arbitrary input does not panic | tolerance 13, 14 |
| `message.content` string or array for `user` and `assistant`; `isMeta` carried as a flag | tolerance 6, 9; fixtures 6 |
| string-content user fixture keeps its text | fixtures 6 (exact `"Привет, add a test for empty input."`) |
| existing tests pass | Step 1 baseline and Step 7 `cargo test --workspace` |

Manual check: none needed beyond Step 7. All tests are hermetic (fixtures embedded via `include_str!`, no env, no filesystem reads at runtime).

## 4. Rollout notes

- No migrations, env vars, feature flags, or config. No change to `cctg` binary; nothing consumes the crate yet (TASK-006, TASK-007, TASK-016 will).
- API contract for consumers: one `Turn` per jsonl record, file order preserved, no merging by `message.id` (one API response can span several assistant turns, thinking sits in its own record and is dropped). `Turn.blocks` is never empty. `ToolResult.agent_id` is record-level metadata attached to every tool_result in that record; real records carry one block each, so it is unambiguous today. `parse` works on any chunk of whole lines, which is what TASK-016 incremental tailing needs.
- Known accepted quirks (tested for no panic, deliberately not "fixed"): a UTF-8 BOM before the first record makes that line skip; a JSON array line such as `["user",{"content":"x"}]` deserializes positionally into a turn (serde derive accepts sequence form for structs). Claude Code writes neither.
- Thinking text exists transiently inside the intermediate `serde_json::Value` of a line; the guarantee is non-exposure through the public API, not zero allocation.
- Line endings: fixtures are LF in the index and working tree; with `core.autocrlf=true` a fresh checkout may convert them to CRLF, which the parser and tests handle.
- Commit messages: no "Generated with" / "Co-Authored-By" trailers (project rule).

## 5. Review notes (changes from PLAN_V2 and why)

Disconfirmation tested first: "a per-item internally tagged `RawBlock` with `#[serde(other)]`, deserialized from a `Value`, loses a valid block or a whole record (as PLAN.md claimed), or the recursion limit does not protect a complete record". Probe `scratch/rev2_probe/` (output `scratch/rev2_probe.out.txt`): `{"type":"text","text":"keep me","id":5}` gives `Text`, `thinking`/`redacted_thinking` give `Ignored`, `"text":5`, missing tag, `"type":7`, `"type":null` fail only that item; a complete record nested 128+ deep fails with the recursion-limit error while neighbours survive (depth 120 passes on a 2 MiB test thread); clippy with the deny lints is clean over the serde derives. The counter-example did not hold: PLAN_V2's core design is correct. It was then built as a full crate with all tests (`scratch/rev2_crate/`, 24 passing).

Changes:
1. **Privacy check was wrong as specified.** PLAN_V2 Step 2.3 names the marker `C:\\Users\\`. Fixture files store Windows paths JSON-escaped (`C:\\work\\demo` in raw text), so a check for the unescaped form misses a leaked escaped path, and the escaped form misses an unescaped one. Replaced with lowercase substrings `users\` and `users/` (match both forms), plus hand-coded `-100…` and bot-token detectors and a self-test (`privacy_detectors_fire`) so the check cannot pass vacuously. The self-test uses a fake id, not the real supergroup id from `CLAUDE.md`.
2. **Purity guard was vague** ("explicit runtime IO/process/environment APIs"). Now an exact token list, plus the explicit rule that `lib.rs` has no `#[cfg(test)]` and doc comments avoid those tokens; `.unwrap_or` stays legal. Dependency guard made exact (`[dependencies]` must be exactly `serde`, `serde_json`) instead of "inspect cargo tree" only.
3. **CRLF test made robust to `core.autocrlf=true`** (set on this machine): normalize to LF first, then build CRLF. Replacing `\n` with `\r\n` on an already-CRLF checkout would test nothing new.
4. **Truncated-line test appends `\n` before the partial record**, so it does not depend on whether the fixture ends with a newline.
5. **Deep-nesting depths fixed** to 200 and 100_000 inside a complete record's `input`; PLAN_V2 said only "deeper than the limit".
6. **Ambiguous behaviours pinned** (PLAN_V2 risk list asked for it but did not choose): known block with missing fields keeps defaults; empty string content still yields one `Text`; thinking-only / null / `[]` content yields no turn.
7. **Rust literal pitfall called out**: the Windows path in `tool_use_result_fixture` must be a raw string.
8. **Reference implementation embedded.** `lib.rs` is given verbatim and the test files are referenced by their verified scratch paths, so the implementer copies, not re-derives.
9. Kept from PLAN_V2 unchanged: public API (`Role`, `Block`, `Turn`, `parse`, `ai_title`), separate `RawTurnRecord`/`RawTitleRecord`, `Value` for soft fields, no `uuid`, `agent_id` from `toolUseResult.agentId`, first-title-wins, cfg_attr lint line, file list.
10. Process note, not a plan change: plan-reviewer-1 left a build `target/` inside `T/scratch/disconfirmation_probe/`; it is ignored by the root `.gitignore` (`target/`), so it will not be committed, but it can be deleted.
