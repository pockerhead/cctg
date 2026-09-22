# PCTX proposals — TASK-006

## 2026-09-22 (planner): stop_reason semantics in real transcripts (domain: transcript)

Proposed invariant line: "`message.stop_reason` is `end_turn` on the final answer and `tool_use` on text that precedes a tool call in main transcripts. In subagent transcripts only the last record of a response carries it; earlier records, and often the final text itself, have `null`. A renderer must not treat null as 'not an answer'."

Why: survey over 605 local jsonl files (`scratch/planner/survey_stop_reason.out.txt`, `survey_null_stop.out.txt`): 4858 subagent text records have null stop_reason; 238 of 530 subagent files end in a null-stop final text; 10026 message ids have mixed stop_reason across their records (null, then the real value on the last one).

## 2026-09-22 (planner): "exactly one content block per assistant record" is not universal

The current invariant says every real `assistant` record carries exactly one block. The same survey found subagent records with 2-8 blocks (`thinking,text,tool_use`, `thinking,tool_use,tool_use,...`), about 20 records. The rule holds for main transcripts in this sample. Suggest "almost always one block; code must handle several".

## 2026-09-22 (planner): Telegram length unit

Evidence for the curated "Telegram Bot API" facts: `core.telegram.org/constructor/config` defines `message_length_max` as "length in utf8 codepoints"; entity offsets are UTF-16 code units; folk reports disagree. TASK-006 counts chunks in UTF-16 code units (`transcript::telegram_len`), which fits under either rule. Hub code that measures Telegram text should reuse `telegram_len` rather than `chars().count()`.

## 2026-09-23 (plan-reviewer-2): transcript crate dependency set (domain: transcript)

Proposed invariant line: "`crates/transcript` depends only on `serde`, `serde_json` and `unicode-segmentation` (CPU-only UAX #29 tables, no transitive deps); `tests/purity.rs` enforces the exact list."

Why: TASK-006 hard-cuts over-long lines for Telegram. A hand-maintained joiner table split regional-indicator flags, long ZWJ sequences and base+combining clusters (`scratch/reviewer2/probe_old`, `scratch/reviewer1_probe`). The crate is used only through `GraphemeCursor` at cut points. Also: `transcript::telegram_len` / `TELEGRAM_TEXT_LIMIT` are the single definition of Telegram text length; hub code (TASK-008/009/014) should reuse them.

> RESOLVED: all four folded on 2026-09-23 into domains/transcript.md, domains/hub.md, agents/{implementer,fixer,code-reviewer}.md and CLAUDE.md.
