# TASK-060 implementation summary (small-fix)

## 1. What was implemented

| Test | Root cause | Fix |
|---|---|---|
| `question_hook_e2e::the_terminal_button_gives_no_decision_at_once` (CI 36246604360, 36253847637) | **Hub race, in product code.** `press_question` finds the ask only by `message_id`, and that id is set when the send's `Delivery` reaches the slots actor. The Delivery comes in on `done`, and the press comes in on `control`. `select!` picks either one. A press processed first got `ANSWER_STALE` and was lost. The hook then waited the whole `question_wait` (60 s), which matches the CI log's `60.0065518s`. | `Asks::in_flight(id)`: the single ask with this id that is `sending` and has no `message_id` yet (`None` when there are several). `press_question` falls back to it. The later Delivery sets `message_id`, and because `version > shown` the final edit still goes out. |
| same file, test side | The fake answered at once, so the race window was microseconds wide. | The fake holds back Telegram's answer to a question send for `QUESTION_ANSWER_LAG` (300 ms). Every press now lands before the id is known, so the race is deterministic. Before the fix: 3/6 tests failed (terminal took 60.003 s, like CI). After: 6/6 pass. A reply to the question message (`own_text...`) waits for the first edit of that message. A real user cannot reply to a message before it exists, so this is correct. |
| `hub::slots::tests::three_explicit_subagents_make_three_blocks_and_internal_agents_none` (CI 36248773154) | **Test race.** The extra `Send "✓ Explore … закончил"` is the legitimate TASK-033 reply to a finished block. With the default bucket it trails the edits by `min_gap` = 1 s, so it usually fell just after the 900 ms "nothing more comes" window. Deterministic proof: with a 2 s window the test always fails with 2–3 such replies. | The test now waits for the 3 edits and the 3 replies, checks the replies' `reply_to` = {1000, 1001, 1002} (none for the internal agent), filters the replies out before the block checks, and then keeps a 1.5 s quiet window. That window is a negative assertion and has to be a wall-clock one. It is longer than `min_gap`, so a pending metered send would show up. |
| `hook_cli::every_event_reaches_the_hub`, `statusline_cli` (several tests) | **Test isolation.** Homes lived at `CARGO_TARGET_TMPDIR/<test name>`. The target dir is shared by all worktrees, so two concurrent runs overwrote each other's `device.env` (the hub port) and the hooks posted to the other run's hub. A spool left by a failed run was also replayed into the next run. Plain CPU load ×20 did not reproduce it. Two concurrent copies did: hook_cli failed 2/10, statusline_cli 4/10. | `common::own_tmp()` = `CARGO_TARGET_TMPDIR/<pid>` (numeric dirs older than 1 h are removed). Used by the six tests that write a `device.env` there: hook_cli, statusline_cli, question_hook_e2e, permission_hook_e2e, status_e2e, statusline_agent_e2e. |
| `spool_e2e::a_silent_hub_costs_a_hook_one_budget_however_much_is_kept` | **Test race.** Under load it failed `accepted == 0`, meaning the test's accept task had not run yet. It was not a product miscount. The absolute `took < 1500ms` included process start-up. | Connections are now counted with a sentinel connection. Accept is FIFO, so once the sentinel is taken every earlier connection has been counted. The test asserts `<= 1` (0 is legitimate when a starved hook spent its budget before connecting). The time bound is now relative: `took < baseline + 2 × POST_TIMEOUT`, where baseline is the same hook with an empty spool against the same silent hub, so start-up cancels out. The bug this guards against (a timeout per kept file) would add about 7.5 s. |

Files (numstat, added/removed):
- `crates/cctg/src/hub/questions.rs` +33/-0 (`Asks::in_flight` plus the unit test `a_press_before_the_message_id_finds_the_ask_in_flight`)
- `crates/cctg/src/hub/slots.rs` +30/-7 (`press_question` fallback, subagent test)
- `crates/cctg/tests/common/mod.rs` +31/-1 (`own_tmp`)
- `crates/cctg/tests/question_hook_e2e.rs` +24/-2
- `crates/cctg/tests/spool_e2e.rs` +82/-37
- `crates/cctg/tests/{hook_cli,permission_hook_e2e,status_e2e,statusline_agent_e2e,statusline_cli}.rs` one or two lines each

## 2. Deviations, not done
- Permission prompts (`prompts.by_message`) have the same press-before-Delivery window, and `permission_hook_e2e` presses right after the send is recorded. The task does not list it and it has not failed on CI, so I left it alone. It is recorded in PCTX_PROPOSALS.md.
- `spool_e2e` (and other tests) use `free_port()`: bind to port 0, drop, bind again later. With two concurrent copies this failed once in 20 with `AddrInUse` at `spool_e2e.rs:137` (another process took the port in between). This is an old pattern and appears only with concurrent runs of the same binary. Not fixed.
- No product timeouts were changed.

## 3. Tests
- `cargo fmt --all --check`: ok. `cargo clippy -j 1 --workspace --all-targets -- -D warnings`: ok.
- `cargo test -j 1 --workspace --no-fail-fast` (CARGO_TARGET_DIR shared, DEBUG=0): exit 0, all 44 test binaries `ok` (`scratch/runs/full_test.txt`).
- Load verification, `scratch/load_verify.sh`: 20 CPU burners on 16 cores, a parallel `cargo build --release -j 1`, and each CLI binary run as two concurrent copies. Results in `scratch/runs/verify/*.summary`:
  - hook_cli 20/20 + twin 20/20
  - statusline_cli 20/20 + twin 20/20
  - question_hook_e2e 20/20 + twin 20/20
  - spool_e2e 20/20 + twin 19/20 (the one failure is the `free_port` AddrInUse above, a different test)
  - subagent test 20/20
  - terminal-button test 20/20
- Before the fixes: the question tests failed deterministically with the lag (3/6). The subagent test failed deterministically with a 2 s window. Two concurrent hook_cli / statusline_cli runs failed 2/10 and 4/10. spool_e2e under load failed 1/8 (`scratch/runs/par_*`).

## 4. Manual verification
- `cargo test -p cctg --test question_hook_e2e`: all six pass in about 3 s. Revert the `.or_else(|| self.questions.in_flight(id))` line in `slots.rs` `press_question` and three of them hang for 60 s.
- `sh scratch/load_verify.sh` repeats the load run.
