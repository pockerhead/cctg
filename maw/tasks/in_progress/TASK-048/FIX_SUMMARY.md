# TASK-048 fix summary (review of fae99d2, NEEDS_WORK)

Mode: small-fix, no plan. The spec is `TASK_FINAL.md`. The review is `IMPL_REVIEW.md`. The review says `TASK_FINAL.md`, `IMPL_SUMMARY.md` and `log.jsonl` are missing. That is wrong: they are in the worktree task folder, and the review looked at a `git archive` copy of fae99d2, which does not contain them. It does not affect the code findings.

## Preflight: the claim that would break correct code

Review issue 3 proposes: "create a gather only when the buffer is empty (or front already in gather)". Applied verbatim, it breaks this case. A file is at the front of the slot, still downloading, and a text arrives behind it. The buffer is not empty, so no gather is created, and every text after it goes one by one without a window. The existing test `a_file_in_a_burst_lets_the_burst_go_first_and_keeps_its_place` expects "after" to wait for its own window, so it would fail. The defect behind the claim is real: an old kept text did wait for the new window. So item 3 is fixed differently (see below).

## Fixed

1. **major, `target_agent` and `reply_to_message_id` of one part applied to the whole burst** (`burst_meta`). Confirmed by code. `burst_meta` took `inbound_meta` of the last part that replied and put it on the whole burst. Fix: `Slots::burst` cuts a burst where `(thread_id, reply_to)` differs from its first part. One inbound now has one addressee (the session or one subagent) and one reply target. `burst_meta` takes `inbound_meta` of the last part, which is now correct for every part. The rest of the burst goes as the next inbound in the same `flush`: the gather stays while the front is text, and it is already due. The module doc was updated.
   Test `a_burst_is_cut_where_its_addressee_or_reply_target_changes`, with gathering on. Six messages in one window: two replies to the subagent block, a reply to 999, a plain text plus a forward, then a reply to the block again. That covers both orders, subagent then session and session then subagent. The result is four inbounds, each with the exact meta of its own parts: `target_agent` only on the first and last, `reply_to_message_id` 999 only on "on 999". Checked: without the fix the test fails.
2. **minor, several explicit replies in one burst lose their targets.** Fixed together with item 1: the burst is cut at the reply boundary. The alternative, "keep mixed parts and render the reply target inline", was rejected. A mixed burst would still have to be split for `target_agent`, and a reply to a message without words (a sticker) has no quote to render. Forwards and the user's note carry no explicit reply, so the main case of the task stays one inbound.
3. **minor, an old kept text waits for a new burst's window and merges with it.** Confirmed: the `Some(gather) if gather.due > now => break` branch was taken for any front text. Fix: `Gather.start` is the message id of the burst's first part (`gather(slot, message_id)`). In `flush`, a front text is older than the burst when it is not `start` and `start` still waits behind it (`before_burst`). Such a text goes alone as soon as the link queue has room, as before TASK-048. Test `a_message_the_link_queue_left_behind_does_not_wait_for_a_new_burst`: 1 and 2 are kept while there is no agent, the agent's queue holds 1, and a new 3 starts a burst. Once the queue has room, 2 goes alone at once, and 3 keeps its window. Checked: without the fix the test fails, because 2 waited and went with 3.
4. **+ 5. a console command after a burst.**
   - (4) Confirmed. If `try_send` had no room, `flush` stopped and `on_console_command` still sent `ConsoleCommand` before the texts.
   - (5) Confirmed as a risk. `busy()` comes only from the hooks (`UserPromptSubmit`/`Stop`, tool hooks), and whether `UserPromptSubmit` fires for a channel message is not verified (TASK-016 OPEN_DECISIONS). So the actor cannot see the turn a burst starts.
   - Fix, following the orchestrator's rule. `on_console_command` refuses with `BUSY_NOTICE` (keeping the TASK-043 wording) when the session is busy, or when the slot still keeps messages (`kept`: link queue full, a file on its way), or when topic texts went to this session less than `Options::inbound_settle` ago (`settling`). `mark_handed` is called on every successful text `try_send` in `flush`. The hub sets `INBOUND_SETTLE` = 3 s. The default is ZERO, the same pattern as `gather_quiet`, so earlier tests keep an immediate command.
   - Tests:
     - `a_command_in_a_burst_lets_the_burst_go_first_and_is_refused_while_it_settles` (renamed from `a_command_in_a_burst_lets_the_burst_go_first`). The burst goes first, then the command is not typed and gets `BUSY_NOTICE`. A later text keeps its window. After `INBOUND_SETTLE`, a command is typed.
     - `a_command_behind_messages_the_link_queue_had_no_room_for_is_refused`, with gathering off and settle ZERO. The queue holds 8 and 9 texts arrive, then `!ls` is refused with BUSY. No `ConsoleCommand` comes before or among the 9 texts. Once the buffer is empty, a command is typed.
5. **Missing coverage (the cheap items):**
   - Real TCP with gathering on: `tests/buffer_e2e.rs::a_burst_reaches_a_live_session_over_tcp_as_one_inbound`. It runs the real `serve_agents`, `Slots`, `Scheduler` and a raw wire peer, with `GATHER_QUIET` and `GATHER_MAX` as in the hub. A probe message first proves the agent is bound. Then 3 texts arrive as one inbound (`message_ids` "2,3,4"), and nothing more comes for `GATHER_MAX` + 0.5 s.
   - Link lost in the window: `a_link_lost_in_the_window_loses_and_repeats_nothing`. After a disconnect the messages stay in the slot. After reconnect, 1 and 2 go once, in order. Nothing comes twice.
   - Burst over 128 KiB: `a_burst_over_the_content_limit_goes_as_two_inbounds`. The messages go as [1,2] and [3], each with its own `message_id`, and `message_ids` only on the first. 👀 is on all three, `receipts` are `[2, 3]` and `parts` is `[(2,[1])]`, so ✍ reaches both inbounds.
6. **Nit** `(now + quiet).min(now + max)` became `now + quiet.min(max)`. That line was being changed anyway (the new `start` field).

## Skipped

- **Nit about the `Stream.parts` comment (Vec, not map; upper bound).** This is cosmetic and the code is correct. I left it alone to keep the diff small.
- **Nit about the first part's size counted with the separator.** The review itself says this is conservative and not a bug.
- **Missing test "/clear rebind in the window".** The prompt did not list it among the cheap tests. `flush` takes the session from `live_agent(slot)` at the moment of sending, and the gather belongs to the slot, not the session. After a rebind the burst goes to the new session. That is the same routing as a single message before TASK-048.
- **Missing test "full link queue during the window, then a command".** Covered by `a_command_behind_messages_the_link_queue_had_no_room_for_is_refused`, which uses the same `kept` rule.
- **`mark_handed` for files.** A file handed to the agent does not start the settle timer. Item 4+5 is about text bursts. While a file is still in the slot, `kept` already refuses the command.
- **Review note that the artifacts are missing:** not true for the worktree, see the top.

## Test results

Build: `CARGO_TARGET_DIR=C:/Users/user/dev/cctg/target CARGO_PROFILE_DEV_DEBUG=0`, `-j 1`.

- `cargo fmt --all -- --check`: OK.
- `cargo clippy -j 1 --workspace --all-targets -- -D warnings`: OK.
- Mutation check: I temporarily turned off both new conditions in `flush`/`burst` (`false && ...`), and `a_burst_is_cut_where_its_addressee_or_reply_target_changes` and `a_message_the_link_queue_left_behind_does_not_wait_for_a_new_burst` both FAILED. The source was then restored from a copy (diff checked).
- `cargo test -j 1 --workspace`: exit 0, every binary green. The cctg lib has 642 passed and 1 ignored (6 new or renamed gather/command tests). `buffer_e2e` has 2 passed (the new TCP test included), `stream_e2e` 12, `status_e2e` 7 and `update_e2e` 3. The binaries `run_e2e`, `soak` and `supervise_e2e` have 0 tests on this platform or ignored, as before.
