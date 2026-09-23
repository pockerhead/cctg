# TASK-014 PLAN: permission relay end to end

Reference implementation: `scratch/planner/ws/` (copy of HEAD `c002525`, LF). Patch against the repo root: `scratch/planner/task014.patch` (5 files, `git apply --check` passes on HEAD; applied to a clean HEAD export, all 5 resulting files match `scratch/planner/hashes.txt`). Check after applying: `bash maw/tasks/in_progress/TASK-014/scratch/planner/verify_hashes.sh`. Rebuild patch and hashes from `ws/`: `python scratch/planner/build_patch.py`.

Evidence in `scratch/planner/`: `workspace_test.txt` (full `cargo test --workspace --no-fail-fast` on the patched tree: 21 test binaries ok, cctg lib 250 passed / 1 ignored, new `permission_logs` 1 passed), `mutations.out.txt` (6 mutations of the new logic, all killed; M2 re-run after the overtake test stopped keying on the lane flag). `cargo fmt --all --check` and `cargo clippy --workspace --all-targets -- -D warnings` are clean on `ws/`. The temporary `CARGO_TARGET_DIR` (`%TEMP%\cctg-t014-target`) was deleted.

Build notes for the implementer: one `CARGO_TARGET_DIR` under `%TEMP%`, `CARGO_PROFILE_DEV_DEBUG=0`, `-j 1`, one cargo at a time, delete the dir at the end.

## 1. Understanding

What exists at HEAD `c002525`:

- Agent side is done (TASK-013). `crates/cctg/src/channel.rs:234-273` (`on_permission_request`) validates the 5-letter id, caps each field at 32 KiB, dedupes by a 256-entry FIFO and `try_send`s `AgentMsg::PermissionRequest` to the hub. `channel.rs:184-203` forwards every well-formed `HubMsg::PermissionVerdict` as `notifications/claude/channel/permission` and removes the id from the FIFO. So the agent forwards a duplicate verdict too: **deduplication of verdicts is the hub's job.** Live evidence (`maw/tasks/done/TASK-013/scratch/qa/live_evidence.txt`): `notifications/claude/channel/permission: yfsgn → allow (matched pending)`.
- Wire (`crates/cctg/src/wire.rs:125-131, 145, 160-165, 181-184`): `PermissionRequest { request_id, tool_name, description, input_preview }` without a session id; `HubMsg::PermissionVerdict { request_id, behavior }`. No wire change is needed.
- Hub ingress (`hub/ingress.rs:52-68`): every agent message arrives as `AgentEvent::Message { conn, msg }`, `conn` unique per hub run. That is the scope of a request.
- Slot actor (`hub/slots.rs`): `on_agent` (294-347) handles `PermissionRequest` only by `registry.set_waiting(session, true)` (331-333). Replies use `live_reply_slot` (541-554): only the live, current session of a slot may post, late replies are dropped. All Telegram work goes out through `hand_off` (637-641) to `dispatch_loop` (849-866) and comes back as `Done`. Outbound messages count against `MAX_QUEUED_MESSAGES` = 256 in `send_messages` (621-634). `Control` (82-90) has `TopicEdited` and `Message`.
- Registry (`hub/registry.rs`): `SessionEntry.slot` (287) is the session's own slot; `state()` (413-425) shows `Waiting` when the current session has `waiting`; `set_waiting` (801-805); Stop / UserPromptSubmit clear `waiting` (715-721); SessionEnd, agent disconnect, restart clear it too. `cut()` (190-208) cuts to N UTF-16 units with `…`, private.
- Updates (`hub/updates.rs:130-149`): a callback query from an allowlisted user becomes `Routed::Callback(CallbackInput { query_id, data, message_id })`; a stranger's press is `Ignored::NotAllowed` and goes nowhere (no answer, nothing logged but the reason). Test at 346-364.
- `hub/mod.rs:74`: `Routed::Callback(_) => info!("inbound button press")`, i.e. presses are dropped today.
- Scheduler (`hub/scheduler.rs:1-17, 329-348, 379-409`): `Op::Send { permission: true }` goes before ordinary messages of other topics, never before an older message of its own topic; `Op::Edit` and `Op::AnswerCallback` are the unmetered edit lane; edits of one message coalesce. Tests `permission_prompt_jumps_the_queue`, `permission_overtakes_other_topics_only` (593-607, 775-789).
- API (`hub/api.rs:215-229, 231-244, 280-292`): `send_message` and `edit_message_text` accept `reply_markup`; `answer_callback_query(query_id, text)` exists. `getUpdates` already asks for `callback_query` (206).

Facts checked for this plan:
- Bot API: `callback_data` is 1-64 bytes; `answerCallbackQuery.text` is 0-200 characters; `CallbackQuery.message` may be an `InaccessibleMessage` (https://core.telegram.org/bots/api, fetched 2026-09-23). `allow:abcde` is 11 bytes, `deny:abcde` 10.
- `editMessageText` without `reply_markup`: the fetched docs text says the markup is removed, a search summary says it is kept (e.g. yagop/node-telegram-bot-api#408 discussion). The plan sends an explicit `{"inline_keyboard": []}`, which removes the buttons under either reading.
- Background subagents keep running after the main turn's `Stop` and their permission prompts go through the same relay, so `Stop` does not prove that an open prompt was answered.

## 2. Approach

One new pure module `hub/permissions.rs` (text, buttons, callback parsing, a bounded prompt book) and wiring in the slot actor. The actor stays the only owner of sessions and connections and never awaits Telegram.

1. **Request.** `AgentMsg::PermissionRequest` from `conn` creates a `Prompt { conn, host, claude_pid, session, request_id, text, sent, message_id, decided }`, where `session` is the conn's session at that moment. The waiting icon is set as today. A second undecided prompt with the same session and request id is not shown again.
2. **Routing.** On every `pump()`, each unsent prompt goes to the topic of `registry.sessions[prompt.session].slot`, when that slot has a topic. This is the slot the session belongs to, whatever session the slot shows now. Unlike replies there is no "current live session" filter: the claude process that asked is blocked on the answer (acceptance 5). A prompt whose slot has no topic yet waits for it.
3. **Sending.** `Op::Send { permission: true, reply_markup: keyboard }` as a new `Work::Permission(key)`. It is **not** counted in `MAX_QUEUED_MESSAGES`, so a full reply backlog cannot drop it. It rides the existing permission lane (acceptance 4). The prompt book (`MAX_PROMPTS` = 256, the oldest decided one goes first) bounds memory. `Done::Permission` records Telegram's `message_id`. A failed send forgets the prompt with one warn: the terminal dialog is still there.
4. **Text.** Plain text, no parse mode: `Запрос разрешения: <tool>\n<description>\n\n<input_preview>`, cut with `registry::cut` to `4096 - len(longest decision mark)` UTF-16 units. The prompt and its edited form both stay ≤ 4096 no matter what Claude sent (acceptance 4).
5. **Buttons.** `callback_data` is `allow:<id>` / `deny:<id>` only (acceptance 1). A press is matched by the callback's `message_id` (hub map `message_id -> prompt`) **and** the id in the data must equal the prompt's id. Two sessions with the same 5-letter id are told apart by their messages (premise challenge counter-example).
6. **Decision.** First press on an undecided prompt: `try_send` one `PermissionVerdict` to the conn that relayed it, or, if that conn is gone, to the newest live conn with the same `host` + `claude_pid` (agent reconnected after a link drop). Only if the send succeeds: mark decided, clear waiting, `answerCallbackQuery("Разрешено"/"Отклонено")`, then `editMessageText(prompt + "\n\n✅ Разрешено из Telegram" | "⛔ Отклонено из Telegram", inline_keyboard: [])`. Any later press answers "Уже решено" and sends nothing (acceptance 3). Unknown message, mismatched id or inaccessible message: "Запрос устарел". No reachable agent or a full agent queue: "Сессия не на связи, ответьте в терминале", prompt stays open. Non-permission data gets an empty answer.
7. **Allowlist.** Unchanged gate in `updates::classify`: a stranger's press never becomes a `Control`, so no verdict is sent and no answer or detail is produced (acceptance 2). Logs carry only conn, short session id and fixed text, never tool, description, preview, request id or user id.

What stays unknowable, recorded honestly: Claude Code never tells the channel that a prompt was answered in the terminal. A Telegram press after a terminal answer still sends a verdict. Claude Code ignores it (it applies verdicts only to its pending id). The message then reads "Разрешено из Telegram", which is what the user did, not proof that it took effect. Prompts are not closed on `Stop`/`UserPromptSubmit` (background subagents). Only the icon follows those hooks, as it already does.

Rejected: a hub token in `callback_data` (the task fixes the data to action + id, and the message id already scopes it); routing prompts by the reply rule; expiring prompts on `Stop`; a second dispatch task for prompts (the scheduler mpsc of 1024 fills only while one Telegram call hangs, and the scheduler drains it whole on the next loop). Decisions are logged in `log.jsonl`.

## 3. Steps

All paths are relative to the repo root. `ws/` has every step applied, and `task014.patch` is the exact diff.

1. `crates/cctg/src/hub/registry.rs:190` — `fn cut` becomes `pub(crate) fn cut` so the prompt text reuses the UTF-16 cutter. No other change.
   Check: builds.
2. New `crates/cctg/src/hub/permissions.rs` (pure, no IO):
   - consts `MAX_PROMPTS = 256`, `MAX_CALLBACK_DATA = 64`, `ALLOWED_MARK`, `DENIED_MARK`, answers `ANSWER_ALLOWED|DENIED|DECIDED|EXPIRED|OFFLINE` (Russian UI text, see ws);
   - `callback_data(Behavior, &str) -> String`, `parse_callback(&str) -> Option<(Behavior, &str)>` (only `allow|deny` + `channel::is_request_id`), `keyboard(request_id) -> Value` (one row, two buttons), `no_keyboard() -> Value` (`{"inline_keyboard": []}`), `prompt_text(&PermissionRequest)`, `decided_text(&str, Behavior)`, `answer(Behavior)`;
   - `Prompt` struct (fields above) and `Prompts` book: `open` (refuses an undecided duplicate of session+id; evicts the oldest decided, else the oldest), `remove`, `get`, `get_mut`, `delivered(key, message_id)`, `by_message(message_id)`, `unsent()`, `len`, `is_empty`;
   - unit tests: data ≤ 64 bytes and round trip for both actions; foreign data (`allow:abcdl`, `resume:…`, extra `:`) is `None`; a 16 Ki-emoji preview gives prompt and both decided texts ≤ 4096 UTF-16; duplicate refusal and eviction order; `unsent`.
   Check: `cargo test -p cctg --lib hub::permissions`.
3. `crates/cctg/src/hub/mod.rs`: `pub mod permissions;`. In `route_inbound`, `Routed::Callback(input)` becomes `control.send(Control::Callback(input))` with `warn!("slot actor stopped; button press dropped")` on error, and the doc comment mentions button presses. The test `messages_go_to_the_slot_actor_and_commands_do_not` also routes one `CallbackInput` and asserts `Control::Callback`.
4. `crates/cctg/src/hub/slots.rs` (production part):
   - imports `permissions::{self, Prompt, Prompts}`, `updates::{CallbackInput, Inbound}`, `channel::is_request_id`, `wire::PermissionRequest`; module doc gets one paragraph about prompts;
   - `Control::Callback(CallbackInput)`; `Done::Permission { key, delivery }`, `Done::Callback(Option<Delivery>)`; `Work::Permission(u64)`, `Work::Callback`; `dispatch_loop` maps them; field `prompts: Prompts`;
   - `on_agent`: the `Message` arm checks `conns.contains_key(&conn)`, and `PermissionRequest(request)` calls `on_permission_request(conn, request)`, which drops invalid ids, builds the `Prompt` from the conn's session/host/claude_pid, sets waiting, `prompts.open`, and logs `"permission request queued for the topic"` (conn + short session);
   - `send_prompts()` called in `pump()` right after topic jobs (so a pending separator of the same slot is handed out first): each unsent prompt with a topic for `sessions[prompt.session].slot` is marked sent and handed off as `Work::Permission(key)` with `Op::Send { permission: true, reply_markup: Some(keyboard) }`;
   - `on_control`: `Control::Callback(input) => return self.on_callback(input)`; `on_callback` hands off `Op::AnswerCallback` first and then the decision `Op::Edit` (both `Work::Callback`, unmetered edit lane); `decide(&CallbackInput) -> (Option<&'static str>, Option<Op>)` implements Approach 6; `verdict_conn(&Prompt)` implements the conn/pid fallback; the log line on success is `"permission verdict forwarded to the session agent"` with `?behavior` and short session;
   - `on_done`: `Done::Permission` with `Outcome::Sent(m)` and `m.message_id != 0` calls `prompts.delivered`; any other outcome warns once per event and calls `prompts.remove(key)`; `Done::Callback` logs errors at debug.
   Check: `cargo build -p cctg`.
5. `crates/cctg/src/hub/slots.rs` (tests): the test `Fake` numbers sent messages from 1000 (`next_message`), which prompts need for their buttons. New tests:
   - `a_prompt_reaches_its_topic_with_two_bounded_buttons` (B's topic 101, huge emoji preview ≤ 4096, data `allow:abcde`/`deny:abcde` ≤ 64 B, icon waiting) — acceptance 1, 4;
   - `the_first_press_sends_one_verdict_and_later_presses_do_not` (allow, allow, deny: answers `[Разрешено, Уже решено, Уже решено]`, exactly one `Allow` verdict to B, none to A, one edit = prompt + `ALLOWED_MARK` with `no_keyboard()`, icon back to alive) — acceptance 1, 3;
   - `one_request_id_in_two_sessions_is_told_apart_by_its_message` — premise counter-example;
   - `stale_or_foreign_presses_send_no_verdict` (unknown message, other id, no message, invalid id, other button);
   - `a_prompt_goes_to_the_slot_of_its_session_after_the_slot_moved_on` (A asks before the topic exists; A ends, B takes the slot; topic created; prompt lands in topic 100; a late reply of A is still dropped) — acceptance 5;
   - `a_press_while_the_agent_is_away_waits_for_its_process_to_return` (offline answer, no edit; after reconnect of the same pid the next press delivers);
   - `a_prompt_overtakes_a_full_reply_backlog` (266 replies fill the 256 cap in topic 100; the prompt for 101 is sent at position ≤ 1; the prompt is found by its text, not by the flag) — acceptance 4, "checked on a full queue".
   Check: `cargo test -p cctg --lib hub::`.
6. New `crates/cctg/tests/permission_logs.rs` (own binary, global subscriber, `.without_time()`, fast bucket, as in `message_logs.rs`): SessionStart, agent, request with private tool/description/preview and id `qzxwv`; presses built from raw updates through `route_batch`: the stranger's is `Ignored::NotAllowed`, the allowlisted one gives exactly one verdict and exactly one `AnswerCallback`. Logs contain both new info lines and none of the private strings, the request id or either user id. Acceptance 2 and the secrets invariant.
7. Full check: `cargo fmt --all --check`, `cargo clippy --workspace --all-targets -- -D warnings`, `cargo test --workspace` (acceptance "existing tests pass").
8. Live check (orchestrator, after merge; acceptance 6), not something the implementer runs: a hidden-console interactive session per the project recipe (`docs/poc.md`, temporary `--mcp-config` + `--settings`). A prompt needing permission shows Allow/Deny in the slot topic with the ❓ icon. Press Allow. Expect in the claude debug log `notifications/claude/channel/permission: <id> → allow (matched pending)`, the terminal dialog closes, the tool runs, the message shows "✅ Разрешено из Telegram" without buttons, and the icon goes back to ⚡️. A second press answers "Уже решено". Also check that the buttons are really gone, which settles the `reply_markup` doc ambiguity.

## 4. Risk areas

- **Callback before `Done::Permission`.** A press can only come after Telegram showed the message, but the actor could process the callback before the dispatch task's `Done`. Then the user gets "Запрос устарел" for a live prompt. The window is one channel hop against a human click. Accepted; the next press works.
- **Hub restart.** Prompts live in memory. After a restart old buttons answer "Запрос устарел" and stay visible (the hub no longer has the text to edit). The terminal dialog still works.
- **Terminal answers are invisible.** The prompt stays undecided, so a later Telegram press sends a harmless verdict and marks the message "Разрешено из Telegram". The icon returns on the next Stop/UserPromptSubmit as today.
- **Icon with parallel prompts.** A decision clears `waiting` for the session even if another prompt (background subagent) is still open; the next request sets it again. Cosmetic.
- **`message_id` 0.** A `Sent` without a message id means buttons that can never match. The prompt is forgotten with a warning (the test Fakes in other tests return `Message::default()`, which is why the slots `Fake` now numbers messages).
- **Scheduler mpsc full.** If one Telegram call hangs, the scheduler's 1024 queue can fill and the single dispatch task blocks. A prompt then waits behind the blocked submit until the call returns, then overtakes everything queued. There is no worse starvation, because nothing is sent while the call hangs anyway.
- **Log privacy.** `description` and `input_preview` can contain secrets (commands, file contents). No log line may carry them. `permission_logs.rs` guards this; keep new log lines to conn, ordinal, short session and fixed text.
- **Verdict to the wrong process.** The pid fallback matches `host` + `claude_pid` among live conns only. A reused pid would need the old claude to die and a new one to get the same pid while the prompt is open; the verdict would then be ignored by Claude Code (no such pending id).

## 5. Open questions

- UI wording: button labels "Разрешить"/"Запретить", marks "✅ Разрешено из Telegram" / "⛔ Отклонено из Telegram", answers (see `permissions.rs`). Change freely; the tests use the constants.
- Should prompts of an ended session be closed (buttons removed) on SessionEnd? Not required by the task and not done. A press then answers "offline" or sends a harmless verdict.
- The live check should confirm that `{"inline_keyboard": []}` removes the buttons. If Telegram rejected it (not expected), the fallback is to omit `reply_markup` in the edit.
