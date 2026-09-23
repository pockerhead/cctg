# IMPL_REVIEW — TASK-009: hub, local transcript commands

Stage: code-reviewer (claude/opus, effort=medium). Code checked: `git diff main -- Cargo.toml Cargo.lock crates/` (commits 7986d36 and b0b86d4).

## 1. Verdict

**PASS.** Every acceptance criterion is met in code and covered by tests. fmt, clippy `-D warnings` and `cargo test --workspace --offline` pass on my own run: 140 passed, 1 ignored. The issues below are minor.

## Disconfirmation (done first)

The counter-example I tested: "the resolver picks a newer `<project>/<uuid>/subagents/<uuid>.jsonl` or a directory named `<uuid>.jsonl` as the newest session, and the 400-too-long fallback sends the chunk Telegram already accepted a second time."

- `sessions.rs:59-108` scans exactly two levels (`root/*` dirs, then direct children). Each child must have a lowercase-UUID stem (`is_session_id`, `:131`) and pass `metadata().is_file()`. A nested `subagents/` file cannot be reached. The test `subagent_transcripts_are_never_sessions` puts a newer UUID-named file under `<uuid>/subagents/`, and it is not chosen. A directory named `<uuid>.jsonl` is skipped (`non_session_entries_are_skipped`, `bad_paths_get_a_notice_and_the_worker_keeps_going`).
- `commands.rs:336-348`: the fallback sends `chunks[index..]`, starting at the rejected chunk, so chunks that were accepted are not repeated. It then `return`s, so there is no loop. `too_long_switches_to_a_document_once` checks `chunks[0] + document == body` and checks the "reject everything" case gives exactly [Send, SendDocument].
- I also ran a read-only scratch probe against the real `~/.claude/projects` (`scratch/crev/`). It printed only counts and timings. `locate(None)` took 1 ms. For brief/full with n=1/3/100 on the newest session, `chunks.concat() == body` held every time. The largest session (96,315,756 B) came in under the 256 MiB limit and rendered in 157 ms.

**The counter-example did not hold.**

`log.jsonl` has no `dead_end` entries. I checked the planner and reviewer-2 decisions (save before handling, next offset below the old one, 24 h staleness, 256 MiB, no header) against the code, and all of them are implemented.

## 2. Confirmed correct

- **AC1, output equals the library, order kept.** `commands.rs:242-258`: `body` is exactly `render_*(last_prompts(parse(lossy), n))`, with nothing added. `deliver` sends `split_for_telegram` chunks one after another and waits for each delivery. The single worker (`serve`, `:404`) keeps two replies from interleaving. Tests: `replies_match_the_library_on_fixtures` (7 fixtures × 3 commands, thread_id checked), `multi_chunk_reply_keeps_order`, `a_slow_command_does_not_hold_up_polling`.
- **AC2, document and a single 400 switch.** `prefer_file` sends one document (`large_reply_goes_as_one_document`: bytes, name and caption checked). `is_too_long` accepts only `400` + "too long". Any other error ends the reply without a document (`other_errors_do_not_switch_to_a_document`).
- **AC3, offset survives a restart.** `offset.rs:318-325`: temp file, `sync_all`, `rename`. `updates.rs:250-255` saves the offset before `for_each(handle)`. Tests: `saved_offset_prevents_handling_an_update_twice_after_restart` (with a control case that has no offset), `offset_is_saved_before_the_batch_is_handled`, `ids_restarted_below_the_saved_offset_are_handled_once`, `an_offset_older_than_a_day_is_ignored`, garbage/leftover `.tmp`. A failed save is retried twice and then does not stop polling (`failing_offset_saves_do_not_stop_polling`, `a_briefly_failing_offset_save_is_retried`). I also reasoned through the 24 h check against a running hub. While the hub runs, it keeps calling getUpdates with the offset, so Telegram treats everything up to it as confirmed. Dropping a stale file on start therefore replays nothing.
- **AC4, bad path gives a notice and polling continues.** NotFound, other `ErrorKind`, oversized, empty, no sessions, no match, ambiguous and missing root each produce their own notice with no path in it (`commands.rs:171-259`). A panic in `spawn_blocking` also becomes a notice (`:381`). The poll callback `route_inbound` (`mod.rs:53`) only does `send` into an unbounded channel.
- **AC5, no secrets or paths.** Logs carry only view, prompts, short id and `ErrorKind`. `io::Error` Display does not include a path. The `with_context` texts in `run` name the env var, not the path. `tests/command_logs.rs` has its own binary, uses `.without_time()` and a marker in the project name, and passes. `grep` of the diff for `Users[\/-]user|AppData` finds nothing. Commits 7986d36 and b0b86d4 have no trailers.
- **AC6/AC7, resolver.** `TranscriptLocator::locate(thread_id, prefix)` is a narrow seam that TASK-011 can replace. With no prefix it returns the newest by mtime, ties broken by id. A prefix gives `NoMatch`, one session, or `Ambiguous` (up to 10 candidates plus "… и ещё N"). The prefix is only compared with the stem and is never put into a path. Arguments are restricted to `[0-9a-f-]`, and `/full ../x` returns Usage.
- **AC8.** Existing tests are green. `last_prompts` only moved the predicate into `is_prompt` without changing behaviour (`render.rs:259-266`). `FINAL_ANSWER_BRIEF/FULL` are unchanged at n=2/99.
- Dependencies: only the local `transcript` was added to `cctg`. No foreign crates.
- Commit b0b86d4: `[profile.dev] debug = "line-tables-only"` in the root `Cargo.toml`. It affects dev/test only, keeps backtraces with line numbers, and does not touch release.
- All 13 files match `scratch/reviewer2/hashes.txt` (`sha256sum -c` gives 13 OK).

## 3. Issues

| # | Severity | Where | Problem | Fix |
|---|---|---|---|---|
| 1 | minor | `commands.rs:392-400` | If delivery fails with anything other than too-long, the user gets nothing: a document rejected by Telegram (for example over 50 MB), a 400 thread not found, the scheduler stopped. Only a `warn!` is written. From the user's side the command looks lost. | After a `DeliveryError::Api` that is not too-long, try one short notice ("Не удалось отправить транскрипт") without retrying it. Or leave it for TASK-011/016 and record the decision. |
| 2 | minor | `commands.rs:31-33` (`USAGE`) | `/brief 20260923` (a prefix made only of digits) returns Usage. The Usage text does not say that such a prefix needs an explicit n (`/brief 3 20260923`). The user will not work out why. | Add one sentence to `USAGE`: "если начало id из одних цифр, укажите n перед ним". |
| 3 | minor | `updates.rs:211-225`, `offset.rs:318` | `save_offset` runs synchronous `File::create` + `sync_all` + `rename` directly on the async task (the poll loop). On Windows, an fsync can take tens of ms under antivirus. On the multi-thread runtime this is not fatal, but it blocks a worker thread. | `tokio::task::spawn_blocking` around `store.save`, or accept it and state the choice in a comment. |
| 4 | minor | `mod.rs:55-59` | Any text starting with `/` (for example `/sessions`) goes to the worker and is dropped silently as `NotOurs`. Before, such messages were logged as `inbound message`. Now they leave no trace. | In `handle`, on `Parsed::NotOurs`, write `debug!/info!(thread, "inbound message")`, or send only `/brief|/full` to the worker. |
| 5 | minor | `commands.rs:185-190` | The Ambiguous list sends the names of project directories (`C--Users-<name>-…`, an encoded private path) to Telegram. The group is private and the plan accepted this (Rollout), but this is the one channel through which a private path leaves the machine. | Keep as is (per the plan) or show only the last path segment. Recorded for TASK-011. |
| 6 | minor | `offset.rs:318-325` | After `rename` the directory is not fsynced. On Linux, a power loss can bring back the previous value, which repeats one batch. On Windows this does not apply. | Acceptable for the MVP. If the hub moves to Linux, fsync the directory after the rename. |

## 4. Missing coverage

- No test for `LocateError::RootUnreadable` in `locate_notice`. It is only reachable through a permission error, but a unit test of the text is cheap.
- No test that the worker keeps handling the next command after a failed delivery (`other_errors_do_not_switch_to_a_document` runs a single command).
- No test that a document rejected after the too-long fallback does not start another fallback. The code guarantees this through `return send_document(...)`, but no test pins it down (the "reject everything" test answers the document with an error too, yet only checks the op count, which is 2, and that already covers it partly).
- No test of `route_inbound` with a text that does not start with `/`: it is not sent to the worker.

## 5. Nits

- The caption says "последние N" even when the session has fewer prompts than N.
- `read_limited`: the `limit + 1` in `take` cannot overflow at 256 MiB, but `u64::MAX` would. It is not reachable from outside, fine.
- The `Batches` test in `mod.rs` uses real time on a multi_thread runtime with 10 s timeouts. The flake risk is low (0/30 in the implementer's runs).
- Scratch probe: `maw/tasks/in_progress/TASK-009/scratch/crev/` (target dir outside the repo, `%TEMP%/cctg-crev009-target`).
