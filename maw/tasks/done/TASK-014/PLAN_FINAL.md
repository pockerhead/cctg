# TASK-014 PLAN FINAL: permission relay end to end

Reference implementation (final, built and tested): `maw/tasks/in_progress/TASK-014/scratch/reviewer2/ws/` (the planner's `scratch/planner/ws` plus every fix below, LF line endings).
Patch against the current repo HEAD: `scratch/reviewer2/task014.patch` (12 files). Hashes of every resulting file: `scratch/reviewer2/hashes.txt`. After applying, `bash maw/tasks/in_progress/TASK-014/scratch/reviewer2/verify_hashes.sh` must print only `OK` lines.
The planner's `scratch/planner/task014.patch` and `hashes.txt` are superseded; do not apply them.

## 1. Summary

The hub turns every `permission_request` an agent relays into a plain-text prompt with two buttons (`allow:<id>` / `deny:<id>`, at most 11 bytes of `callback_data`) in the topic of the requesting session's own slot. The prompt rides the scheduler's permission lane (ahead of other topics' backlog, never ahead of its own topic's earlier work) and is not counted against the 256-message reply cap. Presses come only from allowlisted users (the existing `updates::classify` gate) and are matched by Telegram `message_id` and request id, so two sessions with the same five letters never mix. The first press fixes the answer for good (`Selected`); the verdict goes only to an agent connection bound to the prompt's session (the relaying connection, or after a link drop the newest connection of the same host + claude pid **and** the same session). An agent built with this task announces `verdict_ack` in `Register`; to such an agent the hub sends the verdict with a random `verdict_id` and treats the prompt as decided only when `permission_ack { verdict_id }` comes back; until then the same verdict (same id) is sent again when the agent reconnects and on every retry tick, and the agent passes each id to Claude Code once (a 256-entry id cache that survives reconnects) and acks every copy. An older agent (no `verdict_ack`) gets the byte-identical v1 verdict and a successful hand-off counts as delivery, as before. Wire stays `VERSION = 1`: only optional fields are added, and the new `permission_ack` type is sent only to a hub that put a `verdict_id` in the verdict. A decided prompt is edited to its text plus a decision mark with an explicit empty keyboard; when the requesting session ends (its SessionEnd, `/clear`, or a new session on a reused pid; not an ignored nested-resume SessionEnd) every still-open prompt of it is edited to exactly `Сессия завершилась` without buttons, and a prompt not yet shown is never shown. A failed final edit is tried again on the retry tick (up to 5 attempts). The waiting icon is computed from the prompts of the session that are still active and were raised after its last Stop/UserPromptSubmit. The bounded book (256) forgets finished prompts first, then expires the oldest open prompt visibly (edit to `Запрос устарел`, no buttons), never a selected one, and refuses a new prompt only when every entry is still in use.

## 2. Implementation steps

All paths are relative to the repo root. The reference `ws/` contains every step; `task014.patch` is the exact diff. Apply it with `git apply maw/tasks/in_progress/TASK-014/scratch/reviewer2/task014.patch` and review the result against the steps below; if you retype instead, the hashes must still match.

1. **`crates/cctg/src/wire.rs`** (wire v1, backward compatible)
   - Module doc: a new message type that a peer sends only after the other side announced it in an optional field keeps `VERSION`.
   - `Register`: new `#[serde(default)] pub verdict_ack: bool` (doc: the agent answers a verdict with an id by `permission_ack`; older agents leave it out and the hub then takes a hand-off as delivered).
   - `AgentMsg::PermissionAck { verdict_id: u64 }`; `AgentMsg::KINDS` gains `"permission_ack"`.
   - `HubMsg::PermissionVerdict` gains `#[serde(default, skip_serializing_if = "Option::is_none")] verdict_id: Option<u64>`. Without an id the encoded line is byte-identical to the v1 line.
   - Tests: samples include the ack and a verdict with/without id; new `verdict_acks_stay_compatible_with_version_one_peers` (legacy verdict line decodes to `None`; `None` encodes to the exact legacy bytes; an id is carried under `"v":1`; `verdict_ack` decodes; an ack without id is `Malformed`); `a_register_without_claude_pid_still_decodes` expects `verdict_ack: false`.
   - Reason: the ack handshake without breaking agents that keep running across a hub upgrade (the orchestrator required v1 compatibility; PLAN_V2's `VERSION = 2` would have cut every running agent off).

2. **`crates/cctg/src/agent.rs`**
   - `run_stdio` registers with `verdict_ack: true`.
   - `run` owns `verdicts: VecDeque<u64>` (cap `RECENT_VERDICTS = 256`) across reconnects and passes it to `serve`.
   - In `serve`, a `HubMsg::PermissionVerdict` with `verdict_id: Some(id)`: if the id is not in the cache, send `LinkEvent::Message` first, then remember the id; in both cases write `AgentMsg::PermissionAck { verdict_id: id }` through the one writer (a write error ends the link, the id stays remembered). A verdict without id is passed on and not acked.
   - Module doc paragraph on ack and dedupe; test `register()` sets `verdict_ack: true`; relay test sends `verdict_id: None`.
   - New test `a_verdict_sent_again_after_a_reconnect_is_passed_on_once_and_acked_again` (raw TCP hub: ack, link drop, same id again gives only an ack, id-less verdict passes without ack, new id passes).

3. **`crates/cctg/src/channel.rs`**: the verdict match gets `..`; test constructions add `verdict_id: None`. JSON-RPC output stays `{request_id, behavior}`.

4. **`crates/cctg/src/hub/ingress.rs`**: after registration also forward `AgentMsg::PermissionAck { .. }` as `AgentEvent::Message`; doc of `AgentEvent::Message` updated; test `register()` gets `verdict_ack: false`.

5. **`crates/cctg/src/hub/registry.rs`**: `fn cut` becomes `pub(crate) fn cut` (unchanged from the planner).

6. **New `crates/cctg/src/hub/permissions.rs`** (pure): button data, parsing, keyboard, `no_keyboard()`, `prompt_text` (UTF-16-bounded so text + mark ≤ 4096), `decided_text`, answers, `CLOSED_TEXT = "Сессия завершилась"`, `MAX_PROMPTS = 256`, `MAX_EDIT_ATTEMPTS = 5`; `State { Open, Selected { behavior, verdict_id }, Decided(Behavior), Closed }`; `Edit { None, Due, InFlight, Failed, Done }`; `Prompt { conn, host, claude_pid, session, request_id, text, sent, message_id, state, waits, edit, edit_failures }` with `Prompt::new` and `final_text()`; book `Prompts` with `open -> Opened { Added { key, expired }, Duplicate, Full }` (eviction order: finished, then oldest open that is shown or unsent, never selected or send-in-flight), `remove` (drops the message index only if it points at this key), `delivered` (an ended prompt owes its edit once the id is known), `finish` (active only; unsent prompts are forgotten), `by_message`, `by_verdict`, `unsent`, `selected`, `active`, `due_edits`, `waiting`, `quiet`, `edit_done`, `edit_failed` (counts in `edit_failures`, gives up at 5), `retry_failed_edits`. Unit tests listed in section 3.

7. **`crates/cctg/src/hub/mod.rs`**: `pub mod permissions;`; `Routed::Callback(input)` goes to `Control::Callback(input)` (warn with fixed text if the actor stopped); routing test covers it (unchanged from the planner).

8. **`crates/cctg/src/hub/slots.rs`** (the actor; never awaits Telegram)
   - `Conn.acks` from `register.verdict_ack`. On `Registered`: `push_selected(Some(&session))` then `sync_waiting(&session)`.
   - `AgentMsg::PermissionRequest` → `on_permission_request` (valid id only; `Prompts::open`; an expired victim goes to `expire`: one best-effort `Op::Edit` to `ANSWER_EXPIRED` with no keyboard; `Full` warns; then `sync_waiting`). `AgentMsg::PermissionAck` → `on_verdict_ack` (only for a `Selected` prompt with that id and only from a connection bound to the prompt's session; then `finish(Decided)`).
   - `on_hook`: after `apply_hook`, Stop/UserPromptSubmit call `prompts.quiet(session)`; then `close_ended_prompts()` closes every active prompt whose session the registry now marks `ended` (so an ignored nested-resume SessionEnd closes nothing, and `/clear` or a reused pid closes too).
   - `on_callback`: answer at once via `Op::AnswerCallback`; `press` returns the answer: not a permission button → empty; no/unknown message or other id → `Запрос устарел`; `Closed` → `Запрос устарел`; `Selected`/`Decided` → push the same verdict again, `Уже решено`; `Open` → becomes `Selected { behavior, random verdict_id }`, then `push_verdict`: `Разрешено`/`Отклонено` if handed to an agent, else `Сессия не на связи, ответьте в терминале` (the answer stays fixed and is delivered when the agent returns).
   - `push_verdict`: `verdict_conn` (relaying conn if still bound to the session; else newest conn with same host, same pid and same session; no pid → no fallback); `try_send` of `PermissionVerdict { verdict_id: acks.then_some(id) }`; for a non-acking agent the hand-off decides the prompt.
   - `on_tick` (retry tick): `prompts.retry_failed_edits()` and `push_selected(None)`.
   - `pump`: topic work, then `send_prompts` (session's own slot topic, `Op::Send { permission: true }`), then `send_prompt_edits` (every due final edit: `Op::Edit { final_text, no_keyboard }`, `Work::PromptEdit(key)`), then view and snapshot.
   - `on_done`: `Done::Permission` records the message id or forgets the prompt and re-syncs waiting; `Done::PromptEdit` → success, `message is not modified`, `message to edit not found`, `message can't be edited` count as done; other failures count an attempt (warn on the first and on give-up, debug between); `Done::Callback` debug on error.
   - Logs: conn, short session, `?behavior` and fixed text only.
   - Tests: see section 3. The planner tests `a_prompt_goes_to_the_slot_of_its_session_after_the_slot_moved_on` (pinned the opposite of the orchestrator decision) and `a_press_while_the_agent_is_away_waits_for_its_process_to_return` (drove the removed `decide`) are replaced. Test infrastructure: `Fake.message_edit_errors`, `Rig::agent_with/agent_acking`, `live_slots`, `edits_of`, `request_id`.

9. **Tests that construct `Register`**: `tests/ingress_logs.rs`, `tests/message_logs.rs`, `tests/slots_logs.rs` add `verdict_ack: false`.

10. **New `crates/cctg/tests/permission_logs.rs`** (own binary, `.without_time()`): acking agent, stranger + allowlisted press through `route_batch`, verdict with id, no edit before the ack, ack, second prompt closed by SessionEnd; expected fixed log lines present; tool, description, preview, both request ids, the verdict id and both user ids absent.

11. **Checks** (one `CARGO_TARGET_DIR` under `%TEMP%`, `CARGO_PROFILE_DEV_DEBUG=0`, `-j 1`, one cargo at a time, delete the dir at the end):
    `cargo fmt --all --check`; `cargo clippy --workspace --all-targets -- -D warnings`; `cargo test --workspace --no-fail-fast -j 1`; `git diff --check`; no change to any `Cargo.toml` or `Cargo.lock`.

## 3. Test plan

Reference results on `scratch/reviewer2/ws` (evidence in `scratch/reviewer2/`):
- `workspace_test.txt`: `cargo test --workspace --no-fail-fast -j 1` exit 0; cctg lib 269 passed / 1 ignored; `permission_logs` 1 passed; all other binaries pass. fmt check and clippy `-D warnings` clean.
- `phase1_failing.txt`: the ten defect tests run against the planner reference: all 10 FAIL at their assertions (40 other slots tests pass).
- `mutations.out.txt`: 19 mutations (M1, M2, M4-M6 re-targeted planner ones; M7-M20 one or more per fix, including the agent dedupe), all KILLED by `cargo test -p cctg --lib`. The reproduction script is `phase1.py`, the mutation script `mutations.py`, the patch builder `build_patch.py`. `workspace_test.txt` was recorded before the last edit to `the_first_press_stays_the_answer_while_the_agent_is_away` (a second press added while the agent is away); after that edit `cargo fmt --check` and the full cctg lib run (269 passed) were repeated, and the mutation run used the final tree.

Acceptance criteria:
| Criterion | Tests |
|---|---|
| callback_data ≤ 64 bytes, exactly one matching verdict | `permissions::callback_data_fits_and_round_trips`, `a_prompt_reaches_its_topic_with_two_bounded_buttons`, `the_first_press_sends_one_verdict_and_later_presses_do_not` |
| stranger: no verdict, no details | `permission_logs.rs` (`Ignored::NotAllowed`, one answer only), `hub::updates` allowlist test |
| second/late press idempotent | `the_first_press_sends_one_verdict_and_later_presses_do_not`, `the_first_press_stays_the_answer_while_the_agent_is_away`, `an_acking_agent_decides_a_prompt_only_with_its_own_ack` |
| long preview ≤ 4096; permission traffic overtakes a full queue | `a_huge_prompt_and_its_final_texts_stay_within_the_limit`, `a_prompt_reaches_its_topic_with_two_bounded_buttons`, `a_prompt_overtakes_a_full_reply_backlog`, scheduler `permission_overtakes_other_topics_only` |
| prompt in the slot of its session although the slot moved on | `a_prompt_stays_in_its_slot_topic_when_clear_moves_the_slot_on`, `an_ended_sessions_prompt_never_reaches_the_topic_its_slot_moved_on_to`, `one_request_id_in_two_sessions_is_told_apart_by_its_message` |
| live: Telegram answer closes the terminal dialog | manual, section 4 |
| existing tests pass | workspace run |

Defect tests (each failed on the planner reference): `session_end_closes_its_open_prompt_and_a_late_press_does_nothing`, `a_prompt_stays_in_its_slot_topic_when_clear_moves_the_slot_on`, `an_ended_sessions_prompt_never_reaches_the_topic_its_slot_moved_on_to` (SessionEnd close); `a_verdict_lost_with_the_link_goes_again_to_the_reconnected_agent` (try_send is not delivery; with the ack part: no edit before the ack, same `verdict_id` on the new link); `a_verdict_never_reaches_another_session_on_the_same_pid` (pid fallback scoped to session); `the_waiting_icon_stays_while_another_prompt_of_the_session_is_open`, `a_prompt_telegram_refused_leaves_no_waiting_icon` (waiting from all prompts); `a_failed_decision_edit_is_tried_again_on_the_tick` (edit retry); `a_full_prompt_book_expires_its_oldest_prompt_visibly` (no silent eviction); `the_first_press_stays_the_answer_while_the_agent_is_away` (first press fixed).
Regression guards added after the fix: `an_acking_agent_decides_a_prompt_only_with_its_own_ack`, `a_nested_resume_ending_leaves_the_prompt_open`, `a_turn_boundary_stops_an_unanswered_prompt_from_holding_the_icon`, `the_closing_text_is_the_decided_wording`, the permissions unit tests (`the_answer_is_fixed_once_and_ends_in_one_final_edit`, `a_prompt_that_ends_before_its_message_id_is_edited_once_the_id_comes`, `a_failed_final_edit_is_due_again_until_it_is_given_up`, `waiting_counts_active_prompts_until_the_turn_ends`, `a_full_book_forgets_finished_then_expires_open_never_selected`, `duplicates_are_refused_per_session_and_the_same_id_lives_in_two`), the wire and agent tests of steps 1-2.

## 4. Rollout notes

- No migration, no new env var, no feature flag, no new crate; `registry.json` unchanged (prompts live in memory only; after a hub restart old buttons answer `Запрос устарел` and stay visible, the terminal still works).
- Mixed versions: wire stays v1. New hub + old running agent: old verdict bytes, hand-off counts as delivery (the TASK-013 behaviour; the link-drop window stays open for such agents until their Claude Code session restarts with the new binary). Old hub + new agent: the agent registers with an extra field the old hub ignores and never receives an id, so it never sends `permission_ack`. Update the binary, restart the hub; agents pick the ack up with their next session.
- Live check (orchestrator, after merge; acceptance 6): hidden-console interactive session per `docs/poc.md` with temporary `--mcp-config` and `--settings`. Trigger a permission: prompt with ❓ in the slot topic. Press Allow: Claude debug log shows the verdict matched the pending id, terminal dialog closes, tool runs, message gets the ✅ mark and no buttons (confirms `{"inline_keyboard": []}` on `editMessageText`), icon back to ⚡️; second press answers `Уже решено`. End a session with an open prompt: message becomes exactly `Сессия завершилась` without buttons. If Telegram keeps the buttons, file a follow-up to use `editMessageReplyMarkup`.
- Known limitations (accepted): a callback that races ahead of `Done::Permission` answers `Запрос устарел` (next press works); an ack means "queued in the agent", a crash of the agent process right after it loses the verdict (terminal still works, SessionEnd closes the prompt); a prompt answered only in the terminal stays open with live buttons until its session ends (a later press sends a verdict Claude Code ignores) but stops holding the icon after the next Stop/UserPromptSubmit; the expiry edit of an evicted prompt is sent once (stale buttons only answer `Запрос устарел`); a final edit that fails 5 times is given up with a warning; a session pruned from the registry in the same hook that ended it keeps its prompt open until evicted.

## 5. Review notes (changes against PLAN_V2 and the planner reference)

Disconfirmation tested first: the plan is wrong if a wire change breaks a v1 peer. PLAN_V2 step 1 bumps `VERSION` to 2 and makes `verdict_id` mandatory; `wire::check_version` rejects any other version, so every agent already running in a live session would be refused after a hub upgrade and lose its channel. The counter-example held; the design was changed to optional fields plus a capability flag (`Register.verdict_ack`) and is pinned by `verdict_acks_stay_compatible_with_version_one_peers`.

Kept from PLAN_V2 / planner: allowlist gate in `updates::classify`, `message_id` + request id matching, scheduler permission lane untouched, the 256-cap exemption, bounded UTF-16 prompt text, explicit empty keyboard, SessionEnd closing per the orchestrator decision, no new crates.

Changed:
1. Wire stays v1 (see above) instead of v2; ack type is gated by `verdict_ack`, old agents keep the v1 semantics.
2. Ack handshake simplified: no per-connection in-flight tracking. A `Selected` prompt is pushed again on reconnect of its session's agent, on every press and on the retry tick; the agent's id cache makes repeats harmless; any ack from an agent of the same session decides it.
3. Closing runs as a sweep after every hook over sessions the registry marks ended, not only on the raw `SessionEnd` variant: `/clear` with the new SessionStart first, and a reused pid, also end the old session (registry `allocate`/`session_started` set `ended`), and PLAN_V2's "compare before/after SessionEnd" missed those.
4. No revision numbers: an ended prompt has exactly one final rendering (`Decided` and `Closed` are terminal and never change into each other), so a single `Edit` state per prompt is enough.
5. Waiting icon: derived from active prompts, but Stop/UserPromptSubmit quiet the session's prompts (the registry already clears waiting there). Without this, a prompt answered in the terminal (never reported) would hold ❓ after the next Telegram decision until the turn ended.
6. Full book: PLAN_V2 refused new prompts when all 256 were active; since terminal-answered prompts stay open until SessionEnd, one long session would silently turn the relay off for every session. The oldest open (never selected) prompt is expired visibly instead; refusal only when every entry is selected or awaiting Telegram.
7. The planner's pid-fallback test could not show the defect as written: an agent with an unknown session id and a pid the registry maps to a live session is bound to that session by `agent_session` (TASK-013 `/clear` rule), so it is the right target. The reproducing test uses a SessionStart without `claude_pid`.
8. Planner mutation M3 (reply rule filter) became equivalent: an ended session's prompts are closed before any slot can move on, so it was dropped; M1, M2, M4, M5, M6 were re-targeted at the new code; M7-M20 cover each fix.
9. The planner's `permission_logs.rs` now covers the ack, closing and the verdict id in logs.

Research: Telegram Bot API docs (`callback_data` 1-64 bytes, `answerCallbackQuery.text` ≤ 200 chars, `CallbackQuery.message` may be inaccessible, edit text ≤ 4096); an empty `inline_keyboard` in the edit removes the buttons per community reports, still verified live; `message is not modified` is returned when text and markup are unchanged (treated as applied).
