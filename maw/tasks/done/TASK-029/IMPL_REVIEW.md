# IMPL_REVIEW — TASK-029 (pinned status message + interrupt)

Reviewed: `git diff 6c52fc5 (merge-base with main) -- crates/ docs/` on `feature/status-interrupt` (b919d1b). `main` has since moved on (TASK-039 reaper); diffing against `main` directly shows the reaper as spurious "removed" lines. That is not part of this task. `IMPL_SUMMARY.md` is missing. The implementation is the PLAN_FINAL reference patch applied whole (metrics.md, OPEN_DECISIONS.md), so I checked the code against PLAN_FINAL and TASK_FINAL.

## 1. Verdict

**NEEDS_WORK.** Wire, security, pin and statusline handling are sound, and the build is green. Two gaps in how the turn state is tracked mean ⏹ is often missing, or hidden for the rest of a turn, in common real flows: a turn started from Telegram, and a permission prompt answered in the terminal.

Evidence, run by me: `cargo fmt --check` clean. `cargo clippy -j 1 --workspace --all-targets -D warnings` clean. `cargo test -j 1 --workspace`: **614 passed, 0 failed, 1 ignored**, which matches PLAN_FINAL. I used one target dir, `%TEMP%/cctg-crev029-tgt`, and deleted it afterwards.

## Disconfirmation

Counter-example tested: "a forged or stale `status:confirm` callback, or a late agent answer, sends Esc into, or posts into, a session other than the live current session of the pressed message's slot."

Result: **not found; it holds.**
- `press_status` (slots.rs:2708) resolves the slot only by the pressed `message_id` through `status_slot`, and requires `live_agent(slot)` plus `conn.keys`.
- A bare `status:confirm` without an armed `(session, until)` for that same session only arms the confirmation (the `Press::Stop | Press::Confirm` arm).
- Callbacks from users outside the allowlist stop in `updates::classify`.
- `on_key_written` (2822) needs `ask.conn == conn`, `ask.session == frame session` and `live_agent(ask.slot) == (session, conn)`.
- The agent presses only into `proctree::current_lineage(..).claude_pid`, its own claude, never env `CLAUDE_PID`.
- Headless/nested agents have no hub link (`link_plan`), so they never receive a key.

## 2. Confirmed correct

- **Wire compatibility** (wire.rs): `VERSION` is still 1. `Register.console_keys` is `serde(default)`. `console_key` goes only to agents that announced `keys`. `ConsoleKeyWritten` is in the ingress forward list (ingress.rs:220). Old `register` lines decode, and an unknown key is `Malformed` (test).
- **Agent** (agent.rs, keys.rs): the presser runs in one worker with a bounded queue of 4 and `spawn_blocking`. A process-wide mutex serializes the presses. `FreeConsole`/`AttachConsole`/`CONIN$`/`WriteConsoleInputW` checks `written == 2`, and the handle is closed. `ConsoleKey` never reaches Claude Code (channel.rs, test). There is no stdout output.
- **Pin handling** (hub/mod.rs, updates.rs, slots.rs `on_pinned`): only a `pinned_message` from `getMe.id` about a known status message id is deleted. A person's pin stays (test in `hub::tests`). `can_pin` is checked at start with one warn. A failed pin is not retried, with one warn.
- **Status message lifecycle** (slots.rs:2885 `pump_status`, `on_status_done`):
  - one job per slot;
  - Create only for a live session after its separator;
  - Pin once;
  - edits paced by `status_every`, and the `content` equality check skips no-op edits;
  - "not found" / "can't be edited" sends a new message;
  - `topic_invalid` resets `Slot.status`;
  - dead slot shows 🏁 with no buttons;
  - nested runs and subagents get nothing (`is_live_top_level` gate in `track_activity` and in `apply_hook(StatusLine)`).
  - Restart keeps the message and the metrics (`SessionEntry.metrics`, e2e test).
- **Permission prompt**: ⏹ is hidden while `waiting`. A press returns `ANSWER_WAITING`, drops the armed confirmation and sends nothing (e2e test).
- **Statusline** (statusline.rs):
  - the POST runs in parallel with an 80 ms budget;
  - nothing is sent without a secret;
  - `CCTG_STATUSLINE` guard, so a user command of `cctg statusline` does not recurse;
  - output is passed byte-exact with the command's exit code;
  - its own line when there is no command;
  - model/effort capped at 64 chars;
  - percentages limited to 0..=100;
  - panic text is fixed;
  - the cli tests isolate `USERPROFILE`/`HOME`.
- **ToolStatus hook** (hook.rs `build_tool_status`): subagent calls, handbacks, other events and missing ids are skipped before any POST. There is no process-tree walk. It uses the 300 ms timeout. It is registered `async` in docs/hook-settings.json and poc.md.
- **Logs**: `ServiceMessage.from` is never logged. Frequent hook kinds are logged at debug. Only short ids and fixed text are logged.
- No Ctrl+B / ⏬ code is left (grep `Background|status:bg|background_button`: nothing).

## 3. Issues

### Major

**M1. A turn started from Telegram (channel inbound) may never set `turn`, so there is no "💭 Думает" and no ⏹ until a tool starts. A pure-text turn is never interruptible.**
- Location: slots.rs:955 `track_activity`; status.rs:131 `prompt()` is fed only by `HookEvent::UserPromptSubmit`.
- The task's main flow is a user typing in the topic. A channel message is injected as a `queued_command` / `isMeta` user record, not a submitted prompt. Nothing in the repo or the task artifacts shows that `UserPromptSubmit` fires for it, and no test covers a channel-started turn.
- The hub already sees this start: it hands the message to the agent, and the stream reads the session's own `<channel source="cctg" ... message_id=N>` record (the ✍ path, TASK-016).
- Fix: call `activity.prompt()` (or set `turn`) when the ✍ receipt matches in `on_chunk` (the channel record of the session). Alternatively, prove live that `UserPromptSubmit` fires for channel messages and write that in CLAUDE.md. Add an actor test: inbound hand-off, then channel record in the transcript, then "💭 Думает" with ⏹ for an agent with keys.

**M2. A permission prompt answered in the terminal keeps `SessionEntry.waiting = true` until `Stop`/`UserPromptSubmit`, so for the rest of that turn the status says "❓ Ждёт разрешения", ⏹ is hidden and presses answer `ANSWER_WAITING`.**
- Location: slots.rs `status_view` / `press_status` (the `waiting` gate); registry.rs clears `waiting` only on Stop/UserPromptSubmit/end/disconnect.
- Terminal answers are invisible to the hub (TASK-014). Before this task, the only effect was the ❓ topic icon. Now it also takes away the interrupt for the rest of a long turn.
- The same happens after Esc typed in the terminal at a prompt: the `[Request interrupted by user for tool use]` note stops `Activity`, but `waiting` stays.
- log.jsonl (plan-reviewer-2) claims "A turn ended by Esc ... also quiets the session permission prompts like Stop does". The code does not do this: `interrupted_at` only touches `Activity`. The claim and the code disagree.
- Fix, smallest option: when the stream sees an interrupt note, call `self.prompts.quiet(session)` and clear `waiting`, as Stop does. For prompts answered in the terminal, a `ToolEnd` / stream `Result` for the session means its blocking prompt was answered: quiet that session's prompts, or at least stop treating it as waiting for the ⏹ gate. Add tests for both.

### Minor

**m1. A late `ToolStart` after `Stop` re-opens the turn.**
- Location: status.rs:151-162. `tool_start` sets `turn = true` unconditionally.
- The hooks are async, separate processes. If a `ToolStart` lands after the turn's `Stop` and its `ToolEnd` later, `turn` stays true: "💭 Думает" with ⏹ until the next prompt, and a confirmed press then writes Esc into an idle prompt.
- Fix: do not set `turn` in `tool_start`. Leave the turn to prompt/stream signals, and let `running` alone drive the ⚙️ phase and `busy`. Or ignore starts that arrive after `stop()` until the next `prompt()`.

**m2. The confirm press re-renders "⏹ Прервать" and the answer is shown late.**
- Location: slots.rs:2738 (confirm arm) and 2822 `on_key_written`.
- After the confirm, `next_at = now` makes an edit while `busy` is still true, so a fresh "⏹ Прервать" appears. `written=true` then sets `interrupt_written()` but does not reset `next_at`, so "⏹ Esc отправлен" appears only after the 5 s pace.
- A press in between answers `ANSWER_IDLE`, which is harmless, but the UI lies for up to 5 s.
- Fix: set `shown.next_at = Some(now)` for the slot in `on_key_written` (both branches). Optionally, hide ⏹ while a `KeyAsk` for that session is outstanding.

**m3. A chained user status line that prints more than 64 KiB waits the full 10 s, then falls back to cctg's line.**
- Location: statusline.rs:218. After `take(MAX_OUTPUT)` stops reading, the child blocks on a full pipe, so `child.wait()` hangs until `CHAIN_TIMEOUT`.
- Fix: after the cap, drain the rest to a sink, or `start_kill()` the child before `wait()`.

**m4. `activity` entries are removed only for `followup.ended_sessions` from hooks.**
- Location: slots.rs:943.
- `main` now has the TASK-039 reaper, which ends sessions without `SessionEnd`. After the merge, those sessions' `Activity` leaks (small), and any other end path must also drop it.
- Fix: on the merge, route every end path through the same cleanup. `status_view` itself is fine because it uses the slot state.

**m5. The repeated warn on Create failure.**
- Location: `on_status_done`. A permanently failing Create warns every `retry_every` for the life of the hub.
- Fix: warn once per slot per failure episode, like the other notices.

### Process

- `IMPL_SUMMARY.md` is absent. The reviewer had to rebuild the "what was done" story from metrics.md and OPEN_DECISIONS.md.

## 4. Missing coverage

- Channel-started turn: inbound hand-off, then the transcript channel record, then Thinking with ⏹ (M1).
- Prompt answered in the terminal, then a later `ToolEnd` / `Result`: ⏹ should come back (M2).
- Interrupt note while `waiting`: ❓ should clear (M2).
- `ToolStart` after `Stop`, then `ToolEnd`: the status should go back to Idle with no ⏹ (m1).
- After a confirmed press and `written=true`: an immediate edit to "⏹ Esc отправлен" (m2).
- statusline with a chained command printing more than 64 KiB: should finish well under 10 s (m3).
- Not testable here and correctly left to the live check: `keys::press` itself, and whether Telegram delivers `pinned_message` for the bot's own pin in a forum topic, and pins it per topic.

## 5. Nits

- `run_stdio` now walks the process tree even for `sdk-cli` agents that never connect. One snapshot, harmless, but it could stay inside the `Ok` arm.
- `ANSWER_OFFLINE` is also returned when the agent queue is full (`send_key` false). Its text, "Сессия не на связи", is misleading in that case.
