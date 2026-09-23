# TASK-022 IMPL_SUMMARY

Mode: small-fix. Pre-flight: every pointer checked against code (`HookEvent::Stop` in `wire.rs:410`, `on_hook`/`prompts.quiet`, `on_reply_for`/`send_messages`/`message_op`/`MAX_QUEUED_MESSAGES` in `slots.rs`, `INSTRUCTIONS` and its test in `channel.rs`, `tests/message_logs.rs`, `docs/poc.md`). No mismatch.

## 1. What was implemented

`crates/cctg/src/hub/slots.rs` (+257/-27 incl. tests)
- `on_hook`: a `Stop` with `last_assistant_message: Some(_)` calls `on_turn_answer(session, text)`.
- `on_turn_answer`: blank or whitespace-only text sends nothing; otherwise needs `current_slot(session)` (live top-level, the current session of its slot) and a slot topic; then queues via `send_text(.., "answer")` and logs `turn answer queued` (ordinal, short session id, parts). Nested, unknown, ended sessions and a slot that moved on: `debug`, nothing sent. No topic yet: `info`, nothing sent. No agent connection is required (the answer comes from the hook).
- `current_slot`: new helper; `live_reply_slot` now uses it (same predicate as before plus the agent/conn check it had).
- `send_text`: the split / document / `send_messages` part, factored out of `on_reply_for`; both reply and answer use it. Document name is `<kind>-<short id>.txt` (`reply-…` unchanged, `answer-…` for Stop). The shared `MAX_QUEUED_MESSAGES` cap, dispatch task and atomic multi-chunk rejection are unchanged and shared.
- Module doc and `MAX_QUEUED_MESSAGES` doc mention turn answers.
- Tests: `a_turn_answer_goes_to_its_session_topic_in_split_order` (split order into the right topic, >4 chunks as one document `answer-aaaaaaaa.txt`), `only_a_live_top_level_current_session_with_a_topic_sends_its_answer` (no topic, None/""/blank, nested, unknown, live without agent -> 1 message, ended, slot reused), `turn_answers_and_replies_share_the_message_cap`, `a_stalled_telegram_never_stalls_turn_answers` (1500 Stops through the bounded hook channel with stalled sends, inbound still reaches the agent).

`crates/cctg/src/channel.rs` (+10/-4)
- `INSTRUCTIONS`: the final answer of each turn goes to the topic automatically, do not repeat it through `reply`; `reply` only for extra messages while working. `target_agent` -> SendMessage and "never ask for permissions through `reply`" kept.
- `reply_tool()` description changed the same way (it said "use it to answer messages", which contradicts the new rule). Logged as a decision.
- `initialize_declares_the_channel` asserts the new wording and the kept rules.

`crates/cctg/tests/message_logs.rs` (+15/-4): the existing log-capture binary also sends a Stop with a private answer; asserts the answer reaches Telegram, `turn answer queued` is logged and the answer text never appears in logs.

`docs/poc.md` (+26/-1): section "Из Git Bash" with `MSYS_NO_PATHCONV=1` and why, a `claude-cctg` wrapper script (placeholders only, passes `--mcp-config`, `--strict-mcp-config`, `--settings`, `--dangerously-load-development-channels server:cctg`, `"$@"`); check item 2 now says the answer arrives from the Stop hook.

## 2. Deviations / not implemented

- No Stop dedup (per pointer; decision logged).
- The reply tool description change is beyond the literal task text but needed to avoid double answers (decision logged).

## 3. Test results

Target dir `%TEMP%\cctg-t022-target`, `CARGO_PROFILE_DEV_DEBUG=0`, `-j 1`; deleted afterwards.
- `cargo fmt --all -- --check`: OK.
- `cargo clippy -j 1 --workspace --all-targets -- -D warnings`: OK.
- `cargo test -j 1 --workspace`: all pass (cctg lib 277 passed, 1 ignored; all integration binaries pass). Output: `scratch/implementer/cargo-test-2.txt`.
- First full run (`scratch/implementer/cargo-test.txt`) had one failure in the pre-existing `one_slot_lives_through_hook_agent_end_and_the_next_session` (asserts `registry.json` on disk after a fixed 200 ms sleep; it has no Stop event). It passed 5/5 alone and in the next full run: a pre-existing timing flake, not touched.

## 4. Manual verification

Follow `docs/poc.md`: start hub, start a session (from Git Bash via `claude-cctg`), write in the topic. The model answers in the terminal without calling `reply`; the final answer appears in the topic once. A long answer arrives as ordered chunks, a very long one as `answer-<id>.txt`. A nested `claude -p` inside the session posts no answer of its own.
