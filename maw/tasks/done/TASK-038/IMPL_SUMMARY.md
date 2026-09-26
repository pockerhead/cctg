# TASK-038 — IMPL_SUMMARY

Verdict: implemented (reference patch applied as is, plus the doc steps the probe called for).

## Pre-flight

`main` is still 0a1aee2 (the base of the reference patch; TASK-052 is on its own branch and not in `main`), so there was nothing to port by hand. `git apply --ignore-whitespace --3way scratch/planner/task038.patch` fell back to a direct apply (the repo lacks the patch blobs) and applied every hunk cleanly. All 12 files matched `scratch/planner/hashes.txt` (LF bytes). `docs/poc.md` differs now only because of the probe paragraph below. Every presupposition in PLAN.md section 1 matched the code at 0a1aee2, because the patch applies without fuzz on it.

## 1. What was implemented (commit 731b497, `git diff --numstat` +/-)

| file | + | - |
|---|---|---|
| crates/cctg/src/hook.rs | 538 | 15 |
| crates/cctg/src/hub/ingress.rs | 119 | 12 |
| crates/cctg/src/hub/mod.rs | 4 | 1 |
| crates/cctg/src/hub/questions.rs (new) | 734 | 0 |
| crates/cctg/src/hub/slots.rs | 699 | 9 |
| crates/cctg/src/hub/status.rs | 1 | 1 |
| crates/cctg/src/wire.rs | 124 | 0 |
| crates/cctg/tests/hook_cli.rs | 15 | 8 |
| crates/cctg/tests/question_hook_e2e.rs (new) | 465 | 0 |
| docs/hook-settings.json | 11 | 0 |
| docs/poc.md | 6 | 1 |
| install.sh | 4 | 1 |
| CLAUDE.md | 2 | 0 |

Plan steps 1-9 come from the reference patch unchanged. Step 10, done after the probe (branch a=yes, b=yes; d = Esc kills the hook and rejects the call with no dialog):
- `docs/poc.md`: added to the AskUserQuestion paragraph that Esc in the terminal cancels the question (the hook is killed, no dialog, the call is rejected and the turn is interrupted, the topic message becomes «Вопрос закрыт в терминале»), and that ⏹ is not shown while a question is open.
- `CLAUDE.md`, "Hooks": one paragraph with the verified AskUserQuestion facts (probe a/b/c/d/e, 2.1.283).
- `PCTX_PROPOSALS.md`: a dated entry that fills in the probe results for the planner's hooks-domain proposal.
- `statusMessage` is left as in the reference (decision in log.jsonl).

## 2. Review of the patched code (orchestrator's list)

I read the code for each point and found no defect that needed a code change:
- **Callback data**: `ask:<id>:<q>:<o N|x|d|t>` is at most 16 bytes; `parse_callback` is strict (one or two digits, no leading zero, valid 5-letter id, exactly 4 parts). Tested in `questions::buttons_round_trip_and_fit_callback_data`.
- **Allowlist**: callbacks and messages reach `press`/`on_topic_message` only after `updates::classify` checked `from.id`, so question buttons and the text answer are gated like everything else.
- **Question lifetimes**: `check_questions` runs on every pump. An open ask ends as `Closed` (session not live top-level), `Gone` (waiter missing or its oneshot closed, meaning the hook's connection dropped and ingress `wait_for` returned), or `Expired` (`question_wait`). The waiter is removed and answered in the same pass, and the ask is forgotten once `finished()` (no send or edit in flight, last version shown). `next_deadline` wakes every `HOOK_CHECK_EVERY` while waiters exist.
- **TASK-048 batch vs «Другое»**: the intercept sits at the top of `on_topic_message`, before `console::classify`, `gather` and `park`, and looks only at `input.text` (captions live in `media.caption`, so a file with a caption is never taken as an answer). A text that answers is never parked or gathered. A closed question does not intercept, and a reply to a closed question goes to the session.
- **Esc kills the hook** (probe d): the TCP connection closes, the ingress `gone()` wins the `select!` and drops the oneshot receiver, `answer.is_closed()` becomes true, and the ask ends `Gone` within about 1 s (tested with a dropped receiver in `a_question_closes_when_its_hook_leaves_or_its_session_ends`).
- **Hub restart with an open question**: the hook's read ends without a status line, which gives `BadResponse`. The hook prints nothing, exits 0 and the dialog opens. The Telegram message keeps its buttons (the book is in memory, like prompts), and a press answers «Вопрос уже закрыт». This is an accepted limitation (PLAN section 7). On a graceful stop every waiter ends `Expired` before the last pump (e2e `a_stopped_hub_means_no_decision_and_a_quick_exit`).
- **PermissionRequest Skip**: probe (c) confirms the `PermissionRequest` hook fires for AskUserQuestion after "no decision". `build_permission` skips it before any network call, and the hub also drops the channel request and the hook ask of `AskUserQuestion` (for older hooks). Tested in `permission_requests_of_a_question_get_no_buttons` and e2e `the_permission_hook_of_a_question_never_waits`.

Minor things I noticed and left alone (not defects): a press whose id does not match the message's ask gets an empty callback answer instead of «Вопрос уже закрыт». A reply longer than one Telegram message after «Другое» only uses its first part as the answer, and the rest goes to the session.

## 3. What deviated

- Line endings: see Test results. There were no code deviations from the reference.

## 4. Test results

Shared target `CARGO_TARGET_DIR=C:/Users/user/dev/cctg/target`, `CARGO_PROFILE_DEV_DEBUG=0`, `-j 1`:
- `cargo fmt --all -- --check`: clean (run before and after the line-ending fix).
- `cargo clippy -j 1 --workspace --all-targets --locked -- -D warnings`: clean.
- `cargo test -j 1 --workspace --locked --no-fail-fast` (`scratch/impl_workspace_test.txt`): 861 passed, 1 failed, 3 ignored in 42 binaries. The one failure was `install_e2e::the_script_has_unix_line_endings`. Applying the CRLF-checked-out patch had written CRLF into the index and into install.sh lines 542-545, and mixed endings into the other patched files. I rewrote every patched file with LF and restaged it (`git ls-files --eol`: all `i/lf`), and the hashes still match. After that I reran `install_e2e`, `question_hook_e2e` and `hook_cli` (`scratch/impl_rerun_after_eol.txt`): 9 + 6 + 8 passed. The other binaries were not rerun, because only line endings changed.
- The unix cross check was not run: no unix-only code changed.

## 5. How to verify manually

1. Run `cargo test -p cctg --test question_hook_e2e`. It covers choices, ticks + Done, «Другое» and reply, «В терминале», timeout, the permission hook of a question and a stopped hub, all through the real `cctg hook PreToolUse` binary.
2. Live, after deploy: rerun `install.sh` so the new `PreToolUse` group with matcher `AskUserQuestion`, `timeout: 330` and `statusMessage` lands in `~/.cctg/claude/settings.json`, then restart the session (the Обновить button; `--settings` is not reread). Ask Claude to ask a multiSelect question plus a single-choice one. In the topic, tick two options and press ✅ Готово, answer the next question with ✏️ Другое and a text, and check that Claude gets `"A, B"` and your text with no terminal dialog. Then repeat with ⌨ В терминале (the dialog opens at once), with a reply to the question message, and with Esc in the terminal (the message becomes «Вопрос закрыт в терминале»). No Allow/Deny message should appear for AskUserQuestion.
