# TASK-039 implementer summary

Mode: small-fix. Code commit `818a2c4` on `fix/dead-session-reconcile`.

## 1. What was implemented

Design: the `SessionStart` and `SessionEnd` hooks send the list of live claude pids of their device. Before the hub handles such an event, it ends every non-ended session of that host whose `claude_pid` is not in the list. The end is the same state a `SessionEnd` leaves. Then the normal slot choice runs, so the new session takes the freed topic (ordinal 1).

Files (diff vs `be15578`):

| file | +/- | what |
|---|---|---|
| `crates/cctg/src/wire.rs` | +18/-0 | `HookPost.live_claude_pids: Option<Vec<u32>>`, `#[serde(default, skip_serializing_if = "Option::is_none")]`, set to `None` by `HookPost::new`. No `VERSION` bump; the field is top-level, not in `HookEvent`, so none of the ~93 `HookEvent::SessionStart/End {..}` literals change. Round-trip test extended. |
| `crates/cctg/src/proctree.rs` | +97/-0 | `live_claude_pids()` / `MAX_LIVE_PIDS = 1024`: every `claude(.exe)` and `node(.exe)` pid from one ToolHelp snapshot (Windows) or `/proc` (Linux); `None` on other OSes, on failure or above the cap. Pure filter `claude_pids` + tests. |
| `crates/cctg/src/hook.rs` | +57/-2 | `Probe.live_pids`; `build` fills the list for `SessionStart`/`SessionEnd` only. Test that only these two hooks read the list and carry the key. |
| `crates/cctg/src/spool.rs` | +22/-2 | `spool::save` drops the list: a replayed stale list would end sessions started after it. Test. |
| `crates/cctg/src/hub/registry.rs` | +226/-1 | `apply_hook` = `reap` + old body (renamed `apply_event`); reaped ids merged into `Followup.ended_sessions`, also in new `Followup.reaped`. `reap`: host must match, session not ended, has a `claude_pid` not in the list; skips the posting session and starts younger than `REAP_GRACE` (5 s, transient `recent_starts`, bounded by pruning on each insert). Ends like SessionEnd: `ended`, `waiting=false`, `agent=None`, `pids` key removed. A list without the posting event's own `claude_pid` ends nothing. 5 unit tests. |
| `crates/cctg/src/hub/slots.rs` | +61/-0 | `on_hook` logs each reaped session (`session ended: its claude process is gone`, short id) and drops its title scan. Test through the actor. |
| `crates/cctg/tests/reap_e2e.rs` | +193 (new) | e2e through the real `cctg hook SessionStart` binary with real `claude(.exe)` stand-in processes (a copy of the test binary), one of them killed. |

Point 2 of the task (agent link, 60 s): decided NOT to add a disconnect timer or candidate state. The only moment a slot is chosen is `SessionStart`, and that event now brings fresh data from the same host. `SessionEnd` also carries the list, so a dead session also turns dead in Telegram when any other session of the host ends. An agent disconnect alone proves nothing (hub restart, network), and the hub still does not guess at start (point 3): the reaping runs only on a fresh list from the same host.

Same paths as SessionEnd: `close_prompts` and `end_blocks` get the reaped ids through `ended_sessions`. Dead icon, kept-message buffer (TASK-017), Resume offer and stream (TASK-016) are all derived by `pump` from `entry.ended`, so they follow on their own.

## 2. Deviations / limits

- The list lives on `HookPost`, not in `HookEvent::SessionStart`, as the task text suggested ("добавляет в событие"). Reason in the table: 93 literals. It is still only filled for SessionStart/SessionEnd, and the hub ignores it on other events.
- Includes `node(.exe)` pids and Claude Desktop `claude.exe` pids. The orchestrator note said "only claude.exe pids". Node pids are included because `proctree::lineage` can report a node pid as a session's own `claude_pid` (npm claude nested in a native one). Without them that live session would be ended. Desktop pids are not filtered by image path: an extra pid can only keep a session alive, and filtering would cost one `OpenProcess` per Desktop process. Bounded by `MAX_LIVE_PIDS` (above it: no list, nothing ended).
- Known limits: a session that crashed within 5 s of its start is not ended by a start within that window (the next start after it will end it). A dead pid reused by another claude/node process keeps the dead session alive until the existing reused-pid path in `session_started` hits it. A device that never sends another SessionStart/End (a remote device switched off) keeps its dead sessions alive, by design (point 3). Old hooks (without the field) change nothing.
- The e2e test overrides the lineage of the captured post (`claude_pid` = its own stand-in, no parent). The real lineage depends on whether the test runs under claude (it does here, and a nested maw run would look nested). The part under test, the live list from the real hook, is not changed.

## 3. Tests

All with `CARGO_TARGET_DIR=%TEMP%/cctg-t039-target`, `CARGO_PROFILE_DEV_DEBUG=0`, `-j 1`:

- `cargo fmt --all -- --check`: clean.
- `cargo clippy -j 1 --workspace --all-targets -- -D warnings`: clean (Windows only; the Linux `/proc` branch was not compiled here).
- `cargo test -j 1 --workspace`: all green. cctg lib 451 passed / 1 ignored, `reap_e2e` 1 passed, `hook_cli` 8, `spool_e2e` 4, `stream_e2e` 12, the other binaries unchanged and green.
- `cargo test -j 1 -p cctg --test soak -- --ignored` (multi-slot gate with real hook/agent binaries and claude.exe stand-ins, fake Bot API): `soak: ok`, 3 topics (A, A #2, B), nothing wrongly ended.

New tests: `proctree::live_pids_are_claude_and_node_processes_only`, `the_live_list_is_readable_here`; `hook::only_session_start_and_end_carry_the_live_claude_pids`; `spool::a_kept_event_loses_its_live_claude_pids`; registry `a_start_takes_the_slot_of_a_session_whose_process_died`, `only_dead_pids_of_the_reporting_host_end_sessions`, `a_start_within_the_grace_is_not_ended_by_an_older_list`, `a_list_ends_nothing_unless_it_shows_the_reporting_process`, `a_resume_in_a_new_process_is_not_ended_by_its_own_list`; slots `a_session_whose_process_died_ends_as_by_its_session_end`; `tests/reap_e2e.rs::a_start_after_a_killed_session_takes_its_topic`.

## 4. Manual check

1. Install the new binary, start a claude session in a folder (topic `[host] folder`).
2. Kill its claude.exe (`taskkill /F /PID <pid>`, or close the window). No SessionEnd arrives, the topic stays alive.
3. Start a new claude in the same folder. Expected: hub log `session ended: its claude process is gone`, the old topic gets the separator `── session <new> · new ──` and the new session, no `#2` topic. A permission prompt of the killed session is closed as "Сессия завершилась".
4. Existing stale `#2` topics from before the fix are cleared the same way on the next SessionStart/SessionEnd of that host (their sessions end, the folder's next start takes ordinal 1).
