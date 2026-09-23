# TASK-022 QA_REPORT

Verdict: **SHIP**

## 1. Environment

No docker, no dev server. Direct cargo on the branch `fix/stop-answer-delivery` (working dir `C:/Users/user/dev/cctg`, clean tree + untracked `scratch/qa/`).

- One target dir `%TEMP%\cctg-qa022-target`, `CARGO_PROFILE_DEV_DEBUG=0`, `-j 1`, one cargo at a time. Deleted at the end (checked: `Test-Path` = False). Temp state dirs `%TEMP%\cctg-qa022-*` removed.
- No real Telegram: a fake `Transport` records every `Op`. Real `Scheduler`, real `Slots` actor, real `ingress::serve_hooks` HTTP listener on 127.0.0.1:0, real `cctg hook Stop` binary (`CARGO_BIN_EXE_cctg`) with an isolated `HOME`/`USERPROFILE` (empty `device.env`) and config only via process env. No `.env`, no `device.env` values, nothing in `~/.claude.json`, no interactive claude, no windows.
- The QA test `scratch/qa/qa_turn_answer_e2e.rs` was copied into `crates/cctg/tests/` only for its runs and removed afterwards (git status shows only `scratch/qa/`).

Reproduce:
```
set CARGO_TARGET_DIR=%TEMP%\cctg-qa022-target & set CARGO_PROFILE_DEV_DEBUG=0
copy maw\tasks\in_progress\TASK-022\scratch\qa\qa_turn_answer_e2e.rs crates\cctg\tests\
cargo test -j 1 -p cctg --test qa_turn_answer_e2e -- --nocapture
del crates\cctg\tests\qa_turn_answer_e2e.rs
cargo fmt --all -- --check
cargo clippy -j 1 --workspace --all-targets -- -D warnings
cargo test -j 1 --workspace
```

## 2. Test results

Existing suite (without the QA file): `fmt --check` exit 0 (`scratch/qa/fmt.txt`), `clippy -D warnings` exit 0 (`scratch/qa/clippy.txt`), `cargo test --workspace` exit 0, cctg lib 277 passed / 1 ignored, every integration binary and the transcript crate green (`scratch/qa/cargo-test.txt`). The pre-existing flake `one_slot_lives_through_hook_agent_end_and_the_next_session` did not show up in this run.

New tests (`scratch/qa/qa_turn_answer_e2e.rs`, output `scratch/qa/e2e.txt`), 2/2 pass:

- `stop_answer_reaches_topic_in_order_via_real_hook_binary`: SessionStart by HTTP POST, topic 100 created, then the real `cctg hook Stop` binary with a Claude-Code-shaped Stop payload (fields from the TASK-003 real captures: `stop_hook_active`, `background_tasks`, ...). Sent in order: a short Cyrillic answer, a 3-paragraph 9000-char answer, a 2100-emoji answer (4200 UTF-16 units), a 20480-char answer, then absent / `""` / blank answers. Topic 100 got exactly: the short answer, the `split_for_telegram` chunks of the long one in order, the emoji chunks, one document `answer-aaaaaaaa.txt` with the full byte length, and nothing for the three empty ones. Every chunk `telegram_len <= 4096`. Hook exit 0, empty stdout, stderr free of the secret and the answer.
- `nested_ended_unknown_and_late_clear_stop_send_nothing`: live A with an agent. Nested run N (`parent_claude_pid=10`) Stop: nothing, and no second topic. Stop of a session with no SessionStart: nothing. Agent `Reply` then Stop in one turn: `progress reply` then `final A`. `/clear` (SessionEnd reason=clear, SessionStart source=clear, same pid 10): separator. Late Stop of old A after that: nothing. Stop of B: in topic 100 after the separator. SessionEnd of B, then Stop of B: nothing. Final topic 100: `["progress reply", "final A", "── session bbbbbbbb · new ──", "answer B"]`.

The test is not vacuous. Mutation: in `on_turn_answer` I replaced `self.current_slot(session)` with a plain session-to-slot lookup. The second test then failed: topic 100 also got `nested answer`, `late A after clear` and `after end B`. The file was restored with `git checkout` and the full suite ran on the committed code.

## 3. Acceptance criteria

| Criterion | Test performed | Result |
|---|---|---|
| Stop of a live top-level current session with non-empty answer -> message in the slot topic, split and order per `split_for_telegram`, >4 chunks as one document | E2E test 1 (real HTTP + real hook binary + real Scheduler), plus unit test `a_turn_answer_goes_to_its_session_topic_in_split_order` | PASS |
| Nested run, ended session, session without topic: nothing sent; empty/absent answer: nothing | E2E test 2 (nested, unknown, late-after-/clear, after-end), E2E test 1 (absent/""/blank), unit test `only_a_live_top_level_current_session_with_a_topic_sends_its_answer` (no topic yet). Mutation check shows the test catches a routing regression | PASS |
| Hook ingress and slots actor never wait for Telegram (dispatch task), queue cap shared with `Reply` | Code: `on_turn_answer` -> `send_text` -> `send_messages` (the one gate on `MAX_QUEUED_MESSAGES`, same as reply) -> `hand_off` over an unbounded channel. Unit tests `a_stalled_telegram_never_stalls_turn_answers` (1500 Stops through the bounded hook channel with stalled sends, inbound still delivered) and `turn_answers_and_replies_share_the_message_cap` pass in my run | PASS |
| Channel instructions no longer ask to answer through `reply`; `initialize` test updated | Diff of `channel.rs`: `INSTRUCTIONS` and `reply_tool` description changed; `initialize_declares_the_channel` asserts the new text, the absence of "answer each such message with", and the kept SendMessage/permission rules. Passes | PASS |
| Answer text, user id and secrets not in logs | `tests/message_logs.rs` (global subscriber, `turn answer queued` present, answer text absent) passes. Code: the new `info!`/`debug!` carry only ordinal, short id, parts. E2E: hook stderr has neither the secret nor the answer | PASS |
| `docs/poc.md`: Git Bash launch with `MSYS_NO_PATHCONV=1` and a wrapper example | Read the section: why the variable is needed, the `C:/...` path rule, the command, and a `claude-cctg` script with placeholders only (no real paths or secrets). Check item 2 updated | PASS |
| Existing tests pass | fmt, clippy `-D warnings`, `cargo test --workspace` all exit 0 | PASS |

## 4. Bugs found

No blocking bugs. Observations, none needs a fix for this task:

1. **low**: Claude Code can fire Stop more than once in a turn when another Stop hook blocks the stop (`stop_hook_active: true` on the repeat). Each Stop with an answer is sent, so a user with a blocking Stop hook of their own would see several messages for one turn. The PoC settings have no such hook. The implementer dropped dedup on purpose (two Stops are two events).
2. **low (as documented)**: every turn of every live top-level session with hooks installed goes to its topic, including turns typed in the terminal and sessions started without the channel flag. `docs/poc.md` check 2 now says so.
3. **low**: an answer is dropped (log at `info`) when the slot topic is not created yet. This matches the spec. In practice createForumTopic (~0.36 s) finishes long before the first Stop.
4. **info (reviewer nit, confirmed by code)**: a nested `claude -p --resume <A>` of a known top-level session keeps A's kind, so its Stop posts into A's topic. The Stop hook has no pid to tell it apart.

The two gaps the reviewer named are now covered by E2E test 2. The late Stop of the old session after `/clear` moved the slot sends nothing. Reply then Stop in one turn gives two messages in that order, and the separator comes before the new session's answer. The reverse order (hook processed before a late agent frame) is not guaranteed by design and was not tested.

## 5. Verdict

**SHIP.** Every acceptance criterion passes with my own runnable evidence: a real HTTP hook plus the real `cctg hook Stop` binary into the real Slots actor and Scheduler. Routing is strict (a mutation proves the test catches a regression), and the workspace is green. Not executed: the live check in Telegram (out of scope, the user runs it after merge). What that check should confirm: the model answers without calling `reply`, the answer arrives once, and a Git Bash launch through `claude-cctg` brings the channel dialog up.

Cleanup: no services or containers were started. The cargo target dir and temp state dirs were deleted.
