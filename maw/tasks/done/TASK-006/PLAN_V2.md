# PLAN V2 — TASK-006: brief/full rendering and Telegram sizing

Stage: plan-reviewer-1 (codex/gpt-5.6-sol, effort=medium). Paths are relative to `C:/Users/user/dev/cctg`. The orchestrator decisions in `OPEN_DECISIONS.md` are folded in; there are no remaining open questions.

## 1. Review notes

### Disconfirmation result

The pre-review counter-example was an assistant text turn with `stop_reason: null` and no later tool call. The prototype renders that text in brief because `is_final` falls back to `!tool_after`. The counter-example **held** against `scratch/planner/proto/transcript/src/render.rs` and the supplied `sidechain.jsonl` fixture.

Taken alone, that contradicts the literal acceptance wording “only `end_turn`”. However, `OPEN_DECISIONS.md` explicitly accepts this structural fallback because real subagent transcripts often leave their final text at null: the supplied survey reports 238 of 530 subagent files ending this way. The implementation must retain the exception, document it as an orchestrator-approved interpretation, and test both sides: null text followed by a tool call is hidden; terminal null text is shown.

### Problems in the original plan

1. **The hard splitter does split valid grapheme clusters.** `split.rs::hard_cut` backs up at most 16 scalar values and then returns the original boundary. The independent probe in `scratch/reviewer1_probe/` produced `combining_split=true` for a base plus 24 combining marks and `zwj_split=true` for a longer ZWJ emoji sequence. The existing family-emoji test is too short to expose this. Replace the hand-maintained joiner ranges with UAX #29 extended grapheme segmentation. Unicode recommends extended grapheme clusters for general processing, and `unicode-segmentation` exposes their byte offsets: <https://www.unicode.org/reports/tr29/> and <https://docs.rs/unicode-segmentation/latest/unicode_segmentation/trait.UnicodeSegmentation.html>.

2. **The plan did not fold in orchestrator decision 2.** The prototype treats every non-meta text record as a prompt. A synthetic `<task-notification>` therefore renders in brief as a prompt followed by `в работе…`; this was reproduced by the independent probe. Brief must hide verified non-meta service wrappers while full shows them. A hidden service record must not reset completion state.

3. **The Telegram length claim is overstated.** The primary Bot API documentation says only “1-4096 characters after entities parsing”; it specifies UTF-16 units for entity offsets and lengths, not for the message-size limit: <https://core.telegram.org/bots/api#sendmessage> and <https://core.telegram.org/bots/api#messageentity>. `CLAUDE.md` verifies 4096/4097 only for ASCII. Continue counting UTF-16 code units as a conservative policy requested by this task: it cannot admit more Unicode scalar values than a 4096-code-point interpretation, but it may produce smaller emoji-heavy chunks. Do not claim Telegram formally defines the message limit in UTF-16 units.

4. **The public API is wider than required.** The prototype exports `IN_PROGRESS_MARKER`, `TELEGRAM_TEXT_LIMIT`, `telegram_len`, `SplitLimits`, and `Chunks`. Consumers need the two renderers, one splitter, a configurable file threshold, and the splitter result. Keep constants and length helpers private; do not expose a configurable Telegram message limit, which is fixed by the protocol.

5. **The splitter preservation assertion is too weak.** `assert_valid` removes all whitespace before comparing input and output, so it cannot detect lost paragraph/line whitespace. For nonblank rendered input, make chunks exact slices whose concatenation equals the input. Blank-only input may intentionally return no chunks.

6. **“Copy byte-for-byte” is no longer safe.** The supplied hashes match the prototype, and the prototype independently passes fmt, clippy, and all 47 tests, but the two behavioral defects above are real. Reuse its parser change, fixture, renderer formatting, and most tests; rewrite its prompt classification, splitter, public exports, and dependency/purity expectations.

7. **The reference artifact and baseline claims otherwise checked out.** Current `crates/transcript` has one source file, only `serde`/`serde_json`, and 25 transcript tests; workspace total is 27. The current workspace passes fmt, clippy with `-D warnings`, and tests. All nine supplied prototype hashes match `proto_hashes.txt`; the prototype passes 47 tests with `CARGO_TARGET_DIR` outside the repository.

## 2. Updated understanding

- `crates/transcript/src/lib.rs` currently defines `Role`, `Block`, `Turn { role, blocks, is_meta, is_sidechain }`, tolerant JSONL parsing, and `ai_title`. Thinking and image blocks never enter the public model.
- One assistant API response may span several one-block records sharing `message.id`; `stop_reason` is per record. Main transcript final text normally has `end_turn`; intermediate text normally has `tool_use`. Subagent records commonly use null until the last record, and some files end on null text.
- The required parser amendment is small: add `Turn.stop_reason: Option<String>` and deserialize `message.stop_reason` through `serde_json::Value`, mapping only strings to `Some`. Absent, null, or wrong-typed values remain `None` without dropping the turn.
- Brief output contains genuine user/channel prompts, one line per tool call, and final assistant text. Full additionally contains intermediate assistant text, compact tool input, and truncated tool result. Neither mode can emit thinking.
- Approved prompt policy:
  - show ordinary non-meta user prompts in both modes;
  - unwrap and show meta `<channel ...>...</channel>` prompts in both modes;
  - hide other meta text in both modes;
  - hide verified non-meta service wrappers in brief but show them in full;
  - keep user payload wrappers such as `<pasted_content>` visible.
- Approved answer policy: `end_turn` is final; `tool_use` is intermediate; null/other reasons use the structural fallback `no later tool call before the next genuine prompt`. This is the explicit exception approved in `OPEN_DECISIONS.md`.
- The marker is fixed at `в работе…`. The default file preference threshold is four chunks. Both remain implementation details unless a caller actually needs a public symbol; the threshold is supplied through a minimal options type.
- The splitter operates on rendered plain text. It measures chunks in UTF-16 units conservatively, prefers paragraph and line boundaries, then whitespace, then extended grapheme boundaries, and always returns deterministic chunks plus `prefer_file`.
- TASK-006 should not add IO, network access, subagent-file loading, or a nested transcript API. Subagent transcript expansion belongs to the hub/TASK-007 composition; this crate only renders the turns it receives.

## 3. Revised approach

### Parser

Apply the prototype's tolerant `stop_reason` change. Keep the existing allowlist parser and its wrong-type tolerance unchanged.

### Rendering

Use one private rendering engine behind:

```rust
pub fn render_brief(turns: &[Turn]) -> String;
pub fn render_full(turns: &[Turn]) -> String;
```

Precompute tool-name and agent-id maps and the “tool follows before the next genuine prompt” flags in linear passes. Render blocks once in source order. Classify user text separately from completion state so that a service record shown only in full does not turn a completed exchange back into “in progress”.

Use an explicit, verified service-prefix allowlist for brief suppression: `<task-notification>`, `<command-name>`, `<command-message>`, `<local-command-stdout>`, `<bash-input>`, and `<bash-stdout>`. Do not suppress arbitrary angle-bracket content; `<pasted_content>` may contain user material. Tests must lock this distinction down.

Keep the prototype's concise tool summaries, Agent line, input/result truncation, plain-text formatting, and O(n) maps. Keep `IN_PROGRESS_MARKER` private.

### Splitting

Add `unicode-segmentation` and replace the bounded joiner heuristic. The minimal public surface is:

```rust
pub struct SplitOptions {
    pub max_chunks: usize, // Default: 4
}

pub struct SplitResult {
    pub chunks: Vec<String>,
    pub prefer_file: bool,
}

pub fn split_for_telegram(text: &str, options: SplitOptions) -> SplitResult;
```

Keep the 4096-unit constant and UTF-16 length helper private. Do not expose `chunk_len`, `telegram_len`, or marker/constants merely for tests.

For each remaining nonblank slice:

1. Find the largest prefix not exceeding 4096 UTF-16 units while iterating extended grapheme clusters.
2. If the whole remainder fits, emit it unchanged.
3. Otherwise prefer the last paragraph boundary (`\n\n`), then line boundary (`\n`), then whitespace boundary in the latter half of that prefix.
4. If no soft boundary is suitable, cut at the grapheme boundary.
5. If the first single grapheme itself exceeds 4096 units, fall back to the largest Unicode-scalar boundary that fits; splitting that pathological cluster is unavoidable if the hard Telegram limit must hold.

Emit exact slices, including boundary whitespace, so concatenating chunks reproduces every nonblank input byte. Compute `prefer_file = chunks.len() > options.max_chunks`. This remains linear: each emitted region is scanned once, and soft-boundary searches are bounded by one 4096-unit window.

## 4. Revised steps

### Step 1 — Baseline and scope

Run with `CARGO_TARGET_DIR` under `%TEMP%`:

```text
cargo fmt --all -- --check
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace
```

Expected baseline: 27 workspace tests pass. Preserve unrelated changes (`metrics.md`, `.claude/`, and task scratch artifacts).

Planned project files:

- `Cargo.toml` and `Cargo.lock` — add/lock `unicode-segmentation` using the workspace dependency convention;
- `crates/transcript/Cargo.toml`;
- `crates/transcript/src/lib.rs`;
- new `crates/transcript/src/render.rs` and `src/split.rs`;
- new anonymized `crates/transcript/tests/fixtures/final_answer.jsonl`;
- update `tests/parse_fixtures.rs`, `parse_tolerance.rs`, and `purity.rs`;
- new `tests/render.rs` and `tests/split.rs`.

Touch nothing else.

### Step 2 — Carry `stop_reason`

In `lib.rs`:

- add private `render` and `split` modules;
- re-export only `render_brief`, `render_full`, `split_for_telegram`, `SplitOptions`, and `SplitResult`;
- add `pub stop_reason: Option<String>` to `Turn`;
- add `stop_reason: Value` to `RawMessage`;
- in `to_turn`, extract the message once and map only `Value::String` to `Some`;
- leave absent/null/wrong-typed values as `None` and retain every otherwise-valid turn.

Update existing struct literals. Add parser tests over existing fixtures and synthetic wrong types (`null`, absent, number, object) to prove tolerance did not regress.

### Step 3 — Add the renderer

Create `render.rs` based on the reference implementation, with these corrections and fixed rules:

- brief and full accept any `&[Turn]`;
- ordinary prompts render as `> text`, with blank lines between exchanges;
- meta channel prompts are unwrapped and visible; other meta text is hidden;
- recognized non-meta service wrappers are hidden in brief, visible in full, and never change completion state;
- `<pasted_content>` and ordinary angle-bracket user content are not classified as service merely by syntax;
- `end_turn` assistant text is final; `tool_use` text is brief-hidden/full-visible;
- null/other assistant text uses the approved structural fallback;
- every tool call gets exactly one summary line in brief;
- full adds compact JSON input (500 scalar-value limit) and an indented tool result (1500 scalar-value limit), with an error label when applicable;
- no renderer path can observe thinking because it is absent from `Block`;
- append private marker `в работе…` when the last meaningful state is a real unanswered prompt, intermediate assistant text, tool call, or tool result;
- `[Request interrupted by user...]` is terminal and does not receive the marker;
- empty/fully hidden input returns an empty string.

Use the prototype's maps rather than repeated scans, but make the prompt-boundary calculation use genuine prompts/channel prompts only, not service records.

### Step 4 — Add UAX #29 splitting

- Add `unicode-segmentation = "1.13"` at workspace level and consume it from `crates/transcript`.
- Implement private UTF-16 measurement with `encode_utf16().count()` or equivalent `char::len_utf16` accumulation.
- Implement the exact-slice algorithm in the revised approach with `UnicodeSegmentation::grapheme_indices(..., true)`.
- Preserve paragraph/newline/whitespace delimiters in one adjacent chunk so `chunks.concat() == input` for nonblank input.
- Return no chunks for empty or whitespace-only input.
- Keep 4096 fixed internally; `SplitOptions::default().max_chunks == 4`.
- Document UTF-16 as a conservative project policy, not a proven Bot API definition.

### Step 5 — Add the fixture

Copy `scratch/planner/fixtures/final_answer.jsonl` byte-for-byte to `crates/transcript/tests/fixtures/final_answer.jsonl`. Its verified SHA-256 is `1826c0f923a9d0e6e5491cf9ad7ff9a61cf463154f5f54926d4dda7ac06880b7`.

Retain the fixture privacy checks and include it in the all-fixtures arrays. Do not regenerate from `~/.claude`; the supplied fixture is the only allowed real-shape input for the implementer.

### Step 6 — Update purity and parser tests

- Make `purity.rs` scan every `.rs` source module and fail if a new source file is omitted from the scan.
- Continue forbidding filesystem, IO, network, process, environment, unsafe, panicking calls, and source-local test modules.
- Update the dependency assertion to exactly `serde`, `serde_json`, and `unicode-segmentation`; the latter is CPU-only and does not violate crate purity.
- Assert all expected `stop_reason` values in the new fixture and existing fixtures.
- Assert wrong-typed `stop_reason` never drops a valid turn.

### Step 7 — Renderer tests

Add exact-output tests for brief and full using `final_answer.jsonl`, then cover:

- exactly one brief line per Bash/Read/Agent call and no tool input/result leakage;
- full input, normal result, error result, and Unicode-safe truncation;
- no thinking/signature marker in either mode across every fixture;
- `tool_use` text hidden in brief and shown in full;
- `end_turn` text shown without an in-progress marker;
- unfinished real fixture tail, prompt-only tail, tool-call tail, and tool-result tail end with `в работе…`;
- interrupt prompt is terminal;
- terminal null-stop sidechain text is shown, while null-stop text followed by a tool call is hidden;
- each verified non-meta service wrapper is absent from brief and present in full without changing completion state;
- meta local caveat remains hidden, meta channel text is shown, and `<pasted_content>` remains visible;
- empty slices and meaningful independent slices render deterministically.

For performance, retain a 5000-turn fixed budget test and a 4x-size ratio check with best-of-three timings. The test must exercise brief, full, and splitting, and keep generous bounds (5000 turns under 2 seconds; 20000 turns under `8 * small + 50 ms`) to detect accidental quadratic work without depending on the planner's machine speed.

### Step 8 — Splitter tests

Test all of the following with the public splitter and local test-side UTF-16 counting:

- ASCII, Cyrillic, astral emoji, and ZWJ UTF-16 lengths;
- empty/blank input and short exact preservation;
- exactly 4096 units fits; 4097 ASCII units split;
- an astral emoji at the boundary moves intact to the next chunk and no surrogate/UTF-8 boundary is cut;
- paragraph, line, then whitespace preference;
- a line longer than the limit;
- 50 KB ASCII and 50 KB Cyrillic single blocks are deterministic, bounded, and set `prefer_file`;
- 5000 astral emoji split into bounded chunks;
- the short family emoji case from the prototype;
- the independently failing long ZWJ sequence and base-plus-24-combining-marks case now remain whole when the cluster itself fits;
- regional-indicator flags and an Indic extended cluster stay intact;
- a deliberately oversized single grapheme falls back to scalar boundaries, terminates, and still respects 4096;
- for every nonblank case, every chunk is nonempty, at most 4096 UTF-16 units, deterministic, and `chunks.concat() == input`;
- `max_chunks` controls `prefer_file`, with default 4.

### Step 9 — Final verification

Run with an external target directory:

```text
cargo fmt --all -- --check
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace
cargo tree -p transcript --edges normal --depth 1
git diff --no-index --exit-code maw/tasks/in_progress/TASK-006/scratch/planner/fixtures/final_answer.jsonl crates/transcript/tests/fixtures/final_answer.jsonl
git status --short
```

Verify that `transcript` has only `serde`, `serde_json`, and `unicode-segmentation`; the fixture matches exactly; no target directory appeared under the repository; and only the scoped files changed.

### Acceptance criteria mapping

| Acceptance criterion | Implementation and proof |
|---|---|
| Every chunk ≤4096; no invalid UTF-8 or surrogate split | private UTF-16 measurement, grapheme/scalar byte boundaries; exact, astral, long-line, and invariant tests |
| Brief one line per tool call; full adds inputs/truncated results | renderer block rules and exact-output/tool-result tests |
| Thinking never rendered | no `Block::Thinking`; all-fixture brief/full secret-marker test |
| 50 KB and emoji boundary deterministic or file-preferred | 50 KB, astral, ZWJ, combining, oversized-grapheme, and repeat-call tests |
| 5000 turns within fixed budget and not quadratic | best-of-three fixed-budget plus 4x ratio test |
| Public renderer for turn slices | `render_brief(&[Turn])`, `render_full(&[Turn])`, slice tests |
| Parser preserves `stop_reason` without weakening tolerance | `Value` extraction, existing-fixture assertions, wrong-type tests |
| Brief final-answer rule | `end_turn`/`tool_use` tests plus approved null/other structural exception tests |
| Unfinished tail has `в работе…` | real unfinished fixture and synthetic prompt/tool/result tails |
| Existing tests pass | full workspace verification |

## 5. Risk areas

- **Null/other stop reasons remain inherently ambiguous.** The orchestrator-approved structural fallback preserves real subagent answers but can temporarily classify a null text as final when rendering a slice before its following tool call arrives. Keep this documented and make TASK-016 push only stable exchange boundaries where possible.
- **Telegram's character unit is undocumented.** UTF-16 is conservative relative to code-point counting and aligned with Telegram entity indexing, but emoji-heavy chunks may be smaller than necessary. The policy is centralized privately in the splitter so it can be changed without breaking public API.
- **A grapheme can exceed 4096 units.** It is impossible both to preserve such a cluster and meet the hard message limit. The explicit scalar fallback prioritizes deliverability and termination; test this pathological case.
- **Service wrapper taxonomy can evolve.** Use only verified prefixes and keep the list small. Unknown wrappers remain visible rather than silently hiding possible user content; future additions require a fixture/test.
- **Timing tests can be noisy.** Best-of-three and wide absolute/ratio margins reduce flakiness. Do not tighten them to the local 40 ms reference measurement.
- **Public struct change.** Adding `Turn.stop_reason` breaks external struct literals at compile time. Current repository search finds such literals only in transcript tests; update all of them and let workspace compilation guard future callers.
- **Dependency scope.** `unicode-segmentation` is justified by a reproduced correctness failure, but it must remain the only new dependency and must not introduce IO/network behavior.
