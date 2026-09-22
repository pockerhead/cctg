# QA_REPORT — TASK-005: transcript, tolerant JSONL parser

Stage: qa (claude/opus, effort=medium). Branch `feature/transcript-parser`, HEAD `7e8f598` (fix commit `dca9701`).

Preflight: read `scratch/` listing (crev_harness, rev2_*, probe, fixtures, make/scan scripts) as a coverage map; read TASK_FINAL, PLAN_FINAL, IMPL_SUMMARY, IMPL_REVIEW, FIX_SUMMARY, `log.jsonl` (no `dead_end` entries; the fixer's `decision` refs were checked against `crates/transcript/src/lib.rs` and the new fixture), and `git diff main -- crates/` plus `git diff 08fb15b dca9701 -- crates/` (the fixer's delta).

## Disconfirmation (done first)

Counter-example: "after the fixer swapped the field deserializer to `string_or_default`, a block with a wrong-typed (non-null) string field is no longer dropped per item: it either gets coerced into a visible block or takes the whole record down; or a thinking/signature string reaches a public `Block` through that new path or through `tool_result` content."

Code: `string_or_default` is `Option::<String>::deserialize(d).map(Option::unwrap_or_default)` (`crates/transcript/src/lib.rs:109-114`). It accepts only a string or null; any other type is an error, so `to_block`'s `from_value(item).ok()?` drops that one item. No `to_string()`-style coercion of objects exists.

Tested in `scratch/qa_probe` (independent crate, path dependency, target dir outside the repo): `text` = 5 / true / {} / ["a"], `tool_use.id` = 5, `tool_use.name` = [], `tool_result.tool_use_id` = {} are each placed before a valid `{"type":"text","text":"keep"}` in the same record. Every case yields exactly `["keep"]`. Nulls in all four fields give empty strings. Nine thinking-leak shapes (thinking with `text`/`signature` siblings, `redacted_thinking`, `text:null` with a `thinking` sibling, a thinking item inside `tool_result.content`, `Thinking`/`THINKING` tags, record-level and message-level `thinking` keys, `ai-title` with content) never show the secret in `Debug` of `parse` or `ai_title`.

The counter-example did not hold.

## 1. Environment

- No docker-compose, no dev server. The crate is a pure library, so I used the cargo test runner directly on the working directory `C:/Users/user/dev/cctg/`. No services were started, so there is nothing to clean up.
- Target dirs were outside the repo (session scratchpad): `qa-target`, `qa-probe-target`, `crev-target`.
- Reproduce:
  ```
  cargo fmt --all -- --check
  cargo clippy --workspace --all-targets -- -D warnings
  cargo test --workspace
  cd maw/tasks/in_progress/TASK-005/scratch/qa_probe && cargo run --release -- --real
  cargo run --release --example eye -- <path to a real session jsonl>
  ```

## 2. Test results

Existing suite (rerun by QA):
- `cargo fmt --all -- --check`: exit 0.
- `cargo clippy --workspace --all-targets -- -D warnings`: exit 0.
- `cargo test --workspace`: exit 0. cctg unit 1, cctg `stdout` 1, `parse_fixtures` 9, `parse_tolerance` 14, `purity` 2, doc-tests 0. All passed.

New QA tests (`scratch/qa_probe/src/main.rs`, 36 synthetic checks, all PASS):
- 7 wrong-typed-field cases dropped per item, plus the null-to-default case.
- 9 thinking-leak shapes.
- 8 record-type allowlist cases (`User`, `ASSISTANT`, `system`, `attachment`, `summary`, `ai-title`, `"user "` with a trailing space, and the empty string). All are skipped.
- An unknown record, an unknown block and a truncated final line keep their neighbours. A final line truncated inside a multibyte (Cyrillic) string is also covered.
- Empty input, whitespace or CRLF-only input, and ignored-only input (including `attachment` with message content) all give an empty vector.
- String and array content for both roles. `isMeta` is carried. An empty string content gives one `Text("")`.
- `ai_title`: an empty title is skipped and the first real title wins.
- 20,000 fuzz inputs: 10,000 random byte strings (lossy UTF-8) and 10,000 fixtures mutated with 1 to 8 JSON-syntax bytes, including the thinking fixture. No panic, and the thinking/signature markers never appear in the output. The first run had a bug in my own probe (alphabet index out of bounds), not in the crate. After fixing it, the run passed.

Real data, read-only, only counts and file names printed:
- `qa_probe --real` over 602 jsonl files under `~/.claude/projects`: 95,828 turns, 0 panics, 24,249 thinking/redacted strings of 24 or more chars collected. The first 60 chars of each were searched for in every public block field. There were 2 hits, and I traced both:
  - `C--Users-user-dev/f112fe97….jsonl`: a `ToolResult` from a `Bash` call that dumped session files to stdout. Thinking text that a tool prints as output is tool output, not a parser leak.
  - `9c9651f2….jsonl`: a plain user string prompt with pasted text. This is the same case the reviewer found.
- The reviewer harness `crev_harness`, re-run after the fix (a coverage cross-check, not evidence): 603 files, expected_turns = got_turns = 95,857, 0 mismatching files, 0 bad lines.
- Eye check of a real cctg session with `examples/eye.rs`, printing the role, flags, block kind and a 70-char text prefix: no thinking, and the order is correct. Two things to note for TASK-006. First, channel inbound messages (`<channel source=...>`) are `user` records with `isMeta: true`. Second, a `ToolSearch` result whose content is made only of `tool_reference` items becomes `content: ""`.

## 3. Acceptance criteria

| Criterion | Test performed | Result |
|---|---|---|
| Unknown record, unknown block and a truncated last line lose no earlier turns and do not panic | tolerance 2/3/4; QA "neighbours survive + truncated tail", "truncated multibyte tail", 7 wrong-typed item cases | PASS |
| Empty and ignored-only input give an empty vector | tolerance 1; QA empty / ws / ignored-only | PASS |
| Fixtures for plain text, tool_use + tool_result, thinking + ai-title and sidechain exist, with no private path, token or Telegram id | files present (plus `string_content`, `null_fields`); `fixtures_have_no_private_data` + `privacy_detectors_fire` (now using the same `has_private_path` helper); I read `null_fields.jsonl` by eye and it holds synthetic data only | PASS |
| `thinking` is parsed only enough to be dropped, and no public API returns it | public `Block` has no thinking variant; `RawBlock` has no thinking fields; `thinking_never_escapes`; 9 QA leak shapes; 20k mutation fuzz; real-data scan (2 hits, both traced to user or tool text) | PASS |
| No IO and no `unwrap()`/`expect()` on input (clippy lint or grep) | `cfg_attr(not(test), deny(unwrap_used, expect_used, panic))` + clippy clean; `purity.rs` grep; lib.rs imports only serde/serde_json; `cargo tree` limited to serde/serde_json according to the review, and I confirmed the manifest by reading it | PASS |
| `parse` does not panic on arbitrary bytes | tolerance 13/14; QA 20,000 random or mutated inputs | PASS |
| `message.content` is accepted as a string and as an array for both roles; `isMeta` becomes a flag | tolerance 6/9; fixture 6; QA "shapes+meta" | PASS |
| An anonymized string-content user fixture exists and its text is kept | `string_content_fixture` checks exact `"Привет, add a test for empty input."` | PASS |
| Existing tests pass | `cargo test --workspace` green | PASS |

Fixer claims, checked against the code:
- Null-tolerant string fields: done, and per-item rejection of other wrong types still holds (see Disconfirmation).
- The `privacy_detectors_fire` self-test is no longer tautological: it calls `has_private_path`, which the fixture scan also uses. Done.
- `purity` checks `unsafe` only as an identifier outside `//` comments. Done. This weakens the guard slightly (see bug 2).

## 4. Bugs found

No critical or major bugs.

1. **minor / accepted quirk**: `crates/transcript/src/lib.rs:136-141`. A UTF-8 BOM before the first line makes that record skip silently. It is documented in PLAN_FINAL and Claude Code does not write a BOM. Repro: `parse("\u{FEFF}{\"type\":\"user\",\"message\":{\"content\":\"x\"}}")` gives `[]`, while a turn is expected.
2. **minor (test guard)**: `crates/transcript/tests/purity.rs:4-10`. `code_without_line_comments` cuts every line at the first `//`, including `//` inside a string literal (for example `"http://…"; unsafe {`), and it does not strip `/* */` blocks. So `unsafe` could get past the grep. Clippy does not cover `unsafe` either. The fix is a one-liner: add `#![forbid(unsafe_code)]` to `lib.rs`. Not blocking, since the current lib has no unsafe.
3. **observation for TASK-006, not a defect**: image-only user messages and `tool_result`s built only from `tool_reference` items become no turn or empty content. Channel inbound prompts are `isMeta: true`, so a renderer must not hide `isMeta` turns wholesale. I added a PCTX proposal for this.

## 5. Verdict

**SHIP.** All acceptance criteria pass on the actual code, and fmt, clippy and tests are clean. The fixer's null-tolerant deserializer did not weaken per-item tolerance and opened no path for thinking text. Independent fuzzing and a read-only pass over 602 real transcripts found no panic and no parser leak. The remaining issues are minor, and one of them is a documented, accepted quirk.
