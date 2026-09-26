# TASK-038 — IMPL_REVIEW (code-reviewer)

Reviewed: `git diff 0a1aee2 731b497 -- crates docs install.sh CLAUDE.md` in the worktree `C:/Users/user/dev/cctg-038` (branch `feature/ask-user-question`). I read every changed file in full: `hook.rs` (diff), `wire.rs` (diff), `hub/ingress.rs` (diff and the `hook_request`/`permission_request` region), `hub/questions.rs` (whole), `hub/slots.rs` (diff plus `on_topic_message`, `send_prompts`), `hub/mod.rs`, `hub/status.rs`, `tests/question_hook_e2e.rs` (whole), `tests/hook_cli.rs`, `docs/hook-settings.json`, `docs/poc.md`, `install.sh`, `CLAUDE.md`.

## 1. Verdict

**SHIP.** The hook's output, the timeout chain, the fallbacks and the permission Skip are correct, and I reproduced the test results myself. What is left is five minor findings (none blocking) and some missing tests.

Own run (shared target, `CARGO_PROFILE_DEV_DEBUG=0`, `-j 1`): `cargo fmt --all -- --check` clean, `cargo clippy -j 1 --workspace --all-targets --locked -- -D warnings` clean, `cargo test -j 1 --workspace --locked --no-fail-fast`: **862 passed, 0 failed, 3 ignored, 42 binaries** (`scratch/crev_{fmt,clippy,test}.txt`). No `Cargo.toml`/`Cargo.lock` change, no new crate, no stdout writes besides the decision JSON.

## Disconfirmation (done first)

Counter-example I tried to break it with: *"«✏️ Другое» is armed, then the hook dies (Esc) or the question expires. The user's next text in the topic is swallowed as an answer and never reaches the session."*

Result: **it held (no defect).** `slots.rs` `answer_question` checks `question_waiters[key].answer.is_closed()` first. If nobody listens, it ends the ask `Gone` and returns `false`, so the text continues into console/gather/park. `check_questions` also ends `Gone`/`Expired` asks within `HOOK_CHECK_EVERY` (`next_deadline` wakes every 1 s while `question_waiters` is non-empty), and `text_target` only returns asks that are `is_open()`. Ingress `wait_for` drops the oneshot receiver as soon as `gone(stream)` fires. The slots test `a_question_closes_when_its_hook_leaves_or_its_session_ends` and `a_reply_answers_and_the_terminal_button_lets_the_hook_go` ("hello" reaches the session after close) cover it.

Dead-end log triage: the one `dead_end` (CRLF from `git apply`) was checked. `install_e2e` (including `the_script_has_unix_line_endings`) passes in my run.

## 2. Confirmed correct

- **updatedInput shape** (`hook.rs` `question_json`): it clones the original `tool_input` (extra fields such as `metadata` are kept) and inserts `answers` keyed by the exact, uncapped `questions[i].question` from the original input. It prints `hookSpecificOutput{hookEventName:"PreToolUse", permissionDecision:"allow", permissionDecisionReason, updatedInput}`. Answer count mismatch or a blank answer gives `None` (no output). The shape matches the docs and probe (b). Tests: `answers_are_claude_code_pre_tool_use_output` and e2e `assert_answers` (exact JSON).
- **multiSelect** is joined with `", "` (`questions.rs` `press` Done and `type_answer`). Own text is kept as typed (trimmed). Ticks plus own text give `"Pear, and a fig"`. This matches the terminal format in PLAN §2.
- **Timeout ordering**: hub `slots::QUESTION_WAIT` 300 s (checked each pump, 1 s granularity) < ingress `QUESTION_WAIT_CAP` 305 s < hook `QUESTION_WAIT` 310 s (read phase) + connect 0.5 s / 1 s TLS + stdin ≤ 0.3 s ≈ 311.3 s < settings `timeout: 330` (docs/hook-settings.json, install.sh:543, poc.md:95). The hook cannot block longer than about 311 s.
- **Hub down / old hub**: nothing listening costs `PERMISSION_CONNECT_TIMEOUT` 500 ms (e2e `a_stopped_hub_means_no_decision_and_a_quick_exit` < 4 s). A hub without the path answers 404 on both `serve_hooks` and `serve_hooks_and_permissions` (unit `a_question_returns_the_hub_answers_or_none`), giving `Err(Status(404))`, empty stdout and exit 0. A hub killed mid-wait gives a read EOF, then `BadResponse` and no decision. A graceful stop ends all waiters `Expired` before the last `pump` (slots.rs run epilogue).
- **Callback data**: `ask:<5 letters>:<q>:<o0..o7|x|d|t>` is at most 14 bytes. `parse_callback` is strict (exactly 4 parts, `is_request_id`, 1-2 digits without a leading zero). A press is found by `message_id` first, then the id must match (`press_question`). Stale steps and ended asks answer `ANSWER_STALE`. An out-of-range option or `Done` on a single-select gives `ANSWER_STALE`.
- **Allowlist**: callbacks and messages reach `press`/`on_topic_message` only as `Control::*` from `updates::classify`, which checks `from.id` (updates.rs:202, 242).
- **PermissionRequest Skip**: `build_permission` returns `Skip` for exactly `tool_name == "AskUserQuestion"` before any network call. The hub also drops the channel `permission_request` (before `note_relayed`, so no twin state is left) and old hooks' `PermissionAsk` of that tool. Other tools are unaffected: `permission_requests_of_a_question_get_no_buttons` shows a following `Bash` prompt, and e2e `the_permission_hook_of_a_question_never_waits` finishes in under 1.5 s with no prompt.
- **Session checks**: `on_question_ask` refuses a session that is not live top-level (nested, subagent, ended, unknown) at once. `check_questions` closes an open ask when the session stops being live top-level (`Closed`), and closes it `Gone` when the hook leaves. Subagent calls are skipped in the hook (`agent_id`).
- **TASK-048 / TASK-043 ordering**: the intercept sits before `console::classify`, `gather` and `park`. It only takes `input.text` from a message that is not forwarded (captions live in `media.caption`). `/brief`/`/full` are commands before `on_topic_message`, so they are never intercepted. A console command typed after «Другое» becomes the answer, which is intended per PLAN §7.
- **Logs**: no question, label or answer text in any `tracing` call on either side (hook: reasons, `PostError`; ingress: fixed `WireError`, short session; slots: short session, count, state). e2e `assert_clean` checks the hook's stderr under `RUST_LOG=trace` for the secret and for the texts.
- **Book / edits**: `Asks` holds at most 16 (overflow gives no decision at once). One edit is in flight per ask, with a version, retry on tick and give-up after 5. An ask is forgotten only when `finished()` (not sending, not editing, last version shown). Ending an ask while it is still being sent is handled: the version bump, then the edit after the message id arrives.
- **Waiting icon / ⏹**: `sync_waiting` = prompts or open questions. `waiting` is `#[serde(skip)]` in the registry, so a crash cannot leave a ❓ icon stuck.

## 3. Issues

1. **minor — deployment gap: installs updated with ⬆️ Обновить lose every Telegram trace of AskUserQuestion until `install.sh` is rerun.**
   Where: `hub/slots.rs` `on_permission_ask` (early return for `QUESTION_TOOL`, about line 4028), `hook.rs` `build_permission` Skip; `update.rs`/`download.rs` (TASK-040/050) replace the binary and never rewrite `~/.cctg/claude/settings.json`.
   Proof: at 0a1aee2 there was no `AskUserQuestion` handling anywhere (`git show 0a1aee2:crates/cctg/src/hub/slots.rs | grep AskUserQuestion` finds nothing), so the `PermissionRequest` hook of a question produced an Allow/Deny message. Probe (c) shows that hook fires. After this change the new hub drops it for **any** client, and the new binary skips it. A device whose settings predate TASK-038 has no `PreToolUse`/`AskUserQuestion` group, so the remote user gets neither the question nor any hint that the session waits in a terminal dialog. PLAN §7 mentions that the hook needs an `install.sh` rerun, but not that the old signal goes away.
   Fix (pick one): (a) in `on_permission_ask` for `QUESTION_TOOL`, when this session had no question ask open or ended in the last few seconds (so no PreToolUse hook is registered), send one plain notice without buttons, e.g. "Claude задаёт вопрос в терминале; чтобы отвечать из Telegram, перезапустите install.sh"; or (b) at minimum put the `install.sh` rerun into the release notes / update answer text. Not blocking: the acceptance criteria hold, and the old Allow on a question had unknown semantics.

2. **minor — a reply to another message is swallowed while «Другое» is armed.** `hub/questions.rs:448-463` `text_target`: when `reply_to` is `Some(x)` and `x` is not an open ask's message, `.or_else` still returns the newest `typing` ask. A reply to a subagent block (meant for `target_agent`), to an older Claude answer or to an already closed question is taken as the answer and never reaches the session.
   Proof: repro `scratch/crev_repro_questions.rs.txt`, run in a %TEMP% copy (since deleted): `book.text_target(100, Some(777)) == Some(key)` passes.
   Fix: use the typing fallback only when `reply_to` is `None`: `reply_to.map_or_else(|| typing_ask, |id| replied_ask(id))`. An explicit reply to something else is a stronger signal than "next text".

3. **minor — answers are trimmed and capped labels, not the labels Claude offered.** `hook.rs` `build_question` caps labels at `MAX_LABEL` 256 bytes with `…` (`cap_to`), and `questions.rs` `press`/`picked_labels` use `label.trim()`. The answer sent back is that capped/trimmed text. Proof: the repro above, where `"  Padded label  "` is answered as `"Padded label"`. A label over 256 bytes comes back as `"<prefix>…"`, which differs from what the terminal would return. Impact is low (labels are 1-5 words by the tool's schema).
   Fix: the hub returns option indices (plus own text), and the hook resolves labels from the original `tool_input`. Or: do not cap labels in the post; the 1 MiB body leaves room.

4. **minor — a question of a live session whose slot has no topic waits silently for the full 300 s.** `hub/slots.rs` `send_questions` skips asks with no `topic_id` and nothing ends them early. The terminal shows only the `statusMessage` for 5 minutes, and Telegram shows nothing. Permission prompts have the same pattern, but they wait 90 s. It is rare, because the topic is created at SessionStart and a question comes after a prompt. Fix: end `Expired` when the ask is still unsent after a short grace (e.g. 10 s), or when the slot's topic creation failed.

5. **minor — "✅ Ответ отправлен в Claude" can be shown when the hook already left.** `check_questions`: if the last answer lands in the same pump as the hook's disconnect (Esc), `waiter.answer.send` fails and only a log line says so. The message keeps `ANSWERED_TITLE`. The window is about 1 s. Fix: on `Err(_)` from `send`, re-state the ask as `Gone` (bump the version) so the edit is truthful.

## 4. Missing coverage

- `install_e2e` checks only the event set and the command prefix of the generated `settings.json`. Nothing asserts that `install.sh` generates the `PreToolUse` group with `matcher: "AskUserQuestion"`, `timeout: 330` and `statusMessage` (only `docs/hook-settings.json` is checked in `hook_cli.rs`). The three copies (docs JSON, install.sh heredoc, poc.md) can drift.
- No e2e that kills the running `cctg hook PreToolUse` process (the Esc path) and asserts the topic edit becomes `GONE_TITLE` and a later text reaches the session. The slots test simulates it by dropping the receiver.
- No test with two sessions each having an open question in different topics: a text or press in topic A must not touch B's ask. `text_target` is unit-tested per thread, but the actor routing is not.
- No test for a reply to a different message while typing (finding 2).
- e2e answers are ASCII only. A Cyrillic own-text answer through the real hook binary's stdout (UTF-8 JSON, which Git Bash passes through on Windows) is not covered. Probe (b) also used ASCII.
- No test for the book-full path (17th concurrent ask gives no decision at once) through the actor.

## 5. Nits

- `statusMessage` "ответьте там или кнопкой «В терминале»": the button is in Telegram, so for a person at the terminal the only local action (Esc) cancels the call without a dialog. The implementer's log entry chose to document this only in `docs/poc.md`. Acceptable, but the spinner text does not tell a terminal user how to get the dialog.
- A press whose ask id does not match the message's ask returns `None` (an empty callback answer) instead of `ANSWER_STALE` (`press_question`, `.filter(|ask| ask.id == id)?`).
- A press that lands before `on_question_done` has stored `message_id` answers "Вопрос уже закрыт" (same small window as permission prompts).
- A question the hook skips (duplicate question texts, more than 8 options) is also skipped by the `PermissionRequest` hook, so Telegram gets no hint at all for it.
- `CLAUDE.md` points at `maw/tasks/done/TASK-038/...`, which is correct only after the task moves to `done`.
