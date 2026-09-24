# TASK-041 implementer summary

Mode: small-fix (no plan; the spec was implemented directly). Pre-flight: `Op::Send`, `Op::SendDocument`, `Op::Stream` (hub/scheduler.rs), `BotApi::send_message` / `send_document` (hub/api.rs) and the merge in `Scheduler::merge_lines` all exist in the shape the spec assumes. `editMessageText` has no notification and `pinChatMessage` already sends `disable_notification: true`, so both are left alone.

## 1. What was implemented

- `crates/cctg/src/hub/scheduler.rs` (+72/-2): a `notify: bool` field on `Op::Send`, `Op::SendDocument` and `Op::Stream`, passed through to `BotApi` by `Transport for BotApi`. `merge_lines` stops at the first queued line of its topic whose `notify` differs from the head line (it breaks rather than skips, so line order is kept). New test `loud_and_quiet_lines_never_share_a_message`; the existing test constructors were updated.
- `crates/cctg/src/hub/api.rs` (+10): `send_message(.., notify)` sets `"disable_notification": true` when `!notify`. `send_document(.., notify)` does the same with a multipart field `disable_notification=true`.
- `crates/cctg/src/hub/slots.rs` (+47/-6): every place that builds a send now sets `notify` explicitly. `send_text` takes a `notify` argument.
  - Loud (`notify: true`):
    - Stop turn answers: `send_text(.., "answer", true)`, both on the direct path and on `release`. This includes the long-answer document.
    - Streamed Stop answers: the `Op::Stream` built in `answer_ops`.
    - Permission prompts (`send_prompts`). The TASK-028 hook prompts go through the same book, so they are covered too.
    - The TASK-033 "subagent/nested finished" reply to a block.
  - Silent (`notify: false`):
    - Stream lines and thinking (💭) lines.
    - Subagent and nested blocks (`message_op`), plus the documents that hold long blocks.
    - Session separators.
    - The status message (TASK-029).
    - Buffer overflow and queued notices, other fixed notices, and the Resume offer.
    - Agent `reply`-tool messages: `send_text(.., "reply", false)`.
- `crates/cctg/src/hub/commands.rs` (+3): `/brief` and `/full` output (text and document), usage text and the delivery-failure notice are silent.
- Tests:
  - `tests/formatting_e2e.rs` (+103/-10): the fake Telegram now keeps non-JSON (multipart) bodies as strings. The setup is shared as `scheduler()`. The new test `only_loud_sends_go_without_disable_notification` sends quiet and loud Send, Stream and SendDocument through the real `BotApi` and checks `disable_notification` in each request body (JSON and multipart).
  - `tests/stream_e2e.rs` (+19): the answer test asserts that only "FINAL ANSWER one" had a sound. Stream lines, the separator and the status in that run are all silent.
  - `tests/status_e2e.rs` (+12): asserts that status messages go without a sound.
  - `src/hub/slots.rs` tests: a new `loud()` helper. They assert that turn answers are loud and replies are quiet (including the reply document), that permission prompts are loud, that separators are quiet, and that for a nested block only the "закончил" reply is loud.
  - `tests/soak.rs` (+1): a pattern gets `..`.

## 2. Deviations / not implemented

- The update warning from TASK-040 does not exist yet (TASK-040 is still pending). When it is added, its send must pass `notify: true`. The field has no default, so it cannot be forgotten silently.
- Decision (log.jsonl): replies from the agent's `reply` tool are silent. The spec lists only the Stop answer as loud, and the reply tool is a compatibility duplicate. `/brief` and `/full` output is also silent: it is not in the loud list.

## 3. Test results

With `CARGO_TARGET_DIR=%TEMP%/cctg-task041-target`, `CARGO_PROFILE_DEV_DEBUG=0` and `-j 1`:
- `cargo fmt --all --check`: clean.
- `cargo clippy -j 1 --workspace --all-targets -- -D warnings`: clean.
- `cargo test -j 1 --workspace`: exit 0, 634 passed, 0 failed. This includes cctg lib 479 passed / 1 ignored, and the harness-less `soak` and `supervise_e2e`, which print "ok". The `deploy: failed/rolled back` lines in the output are scenario output of `supervise_e2e`, not failures.

## 4. Manual verification

With a hub on this build, in a slot topic:
- A tool line or 💭 line, the pinned status, a separator or a buffer notice arrives with no sound.
- The turn's final answer, an Allow/Deny prompt and "✓ <type> <id> закончил" each arrive with a sound.
- To check a raw request, point `BotApi::with_api_url` at a local server: quiet sends carry `"disable_notification": true` and loud ones have no such field.
