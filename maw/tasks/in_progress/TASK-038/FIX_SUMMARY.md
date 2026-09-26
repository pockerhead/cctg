# TASK-038 — FIX_SUMMARY (fixer)

Code commit: `2a0e687` on `feature/ask-user-question` (worktree `C:/Users/user/dev/cctg-038`).

## Preflight: the claim that could break correct code

Review item 2 says: use the typing fallback only when `reply_to` is `None`. That breaks every plain message if `reply_to` were `Some(topic root)`, because in a forum topic Telegram sets `reply_to_message` to the topic root on every message (risk lesson TASK-021). I checked `hub/updates.rs`: the root is already filtered (`reply_to: None` for the implicit root reply, test `the_implicit_reply_to_the_topic_root_carries_no_quote`). So the fix is safe, and I applied it.

A second trap: the reviewer's fix (a) for item 1 puts the notice into `on_permission_ask`. The new hook binary skipped the `PermissionRequest` of `AskUserQuestion` before any network call (`build_permission`). So on exactly the clients the notice is for (new binary after ⬆️ Обновить, old `settings.json`), the hub would never see the request. The Skip had to go as well.

## Fixed

1. **Clients without the question hook (review 1).** `hook.rs` `build_permission` no longer skips `AskUserQuestion`. The hub still gives it no decision at once and still shows no Allow/Deny. New `slots.rs` `hint_question_hook`: if no question hook of that session asked within `QUESTION_HOOK_WINDOW` (340 s, which covers the "no decision" of a real question hook under its 330 s timeout), the live slot topic gets `questions::NO_HOOK_NOTICE` once per session. The notice is plain text with no buttons and goes through `send_messages`. The flag is `SessionEntry.question_hint` (`#[serde(skip)]`). `on_question_ask` records each session's question hook in `question_hooks` (pruned by time).
   `docs/poc.md`: its settings example already had the `PreToolUse`/`AskUserQuestion` group (line 95). I added a paragraph that ⬆️ Обновить does not rewrite settings and describes what the notice says.
   Tests: slots `a_question_without_its_hook_is_told_once_per_session` checks two asks give one notice without buttons, and a session whose question hook asked first gets none. e2e `the_permission_hook_of_a_question_never_waits` now runs the real hook twice, each under 1.5 s, and asserts exactly one notice without buttons. hook `questions_carry_their_capped_texts_and_keep_the_input` asserts that `build_permission` now builds the post.
2. **Reply to another message while «Другое» is armed (review 2).** `questions.rs` `text_target`: `Some(reply_to)` matches only an open ask with that message id. Only `None` takes the newest typing ask. Test: the reviewer's repro as `a_reply_to_another_message_is_no_answer_while_typing` (asserts `text_target(100, Some(777)) == None`), plus cases in `the_book_finds_asks_by_message_and_text`. The `docs/poc.md` sentence about reply was updated too.
3. **Exact labels (review 3).** New `wire::Answered { options: Vec<usize>, text: Option<String> }`, with `QuestionAnswer.answers: Vec<Answered>`. The hub (`questions::Ask`) stores indexes plus the text as typed, capped at `MAX_OWN_TEXT` 16 KiB on a char boundary. It trims only for display (`shown_answer`). `hook.rs` `question_json` takes the labels from the original `tool_input` by index and appends the own text if it is not blank. It returns `None` on a wrong count, an unknown index or an empty answer. `MAX_QUESTION_ANSWER` was raised to 512 KiB (4 × 16 KiB, JSON-escaped). The first build's label-string answer now parses as `BadResponse`, which is safe because this path is unreleased. Tests: hook `answers_are_claude_code_pre_tool_use_output` covers a padded label and a label over `MAX_LABEL` coming back exactly, Cyrillic plus emoji own text as typed, and a bad index, a blank or an empty answer giving `None`. hook `question_answers_parse_strictly`, wire `question_posts_round_trip_and_are_checked` (the new JSON shape), and the questions unit tests (indexes, the cut at a char boundary) cover the rest.
4. **Question of a live session without a topic (review 4).** `on_question_ask` now also needs `session_topic(..)`, otherwise the hook gets no decision at once. As a fallback, `send_questions` ends a still-unsent ask whose topic is gone as `Expired` instead of skipping it for 300 s. Test: slots `a_question_of_a_session_without_a_topic_goes_to_the_terminal_at_once` (topic creation stalls, the answer is `None` in under 5 s).
5. **"Sent" shown after the hook left (review 5).** In `check_questions`, when `waiter.answer.send` returns `Err`, the new `Ask::answers_not_taken()` sets state `Gone` and bumps the version. The edit then shows `GONE_TITLE` («Вопрос закрыт в терминале»). Test: unit assertions in `a_single_choice_answers_and_moves_on`. An actor-level test is not possible without a race, because the window is one pump.

Test gaps (from the prompt):
- `install_e2e::install_update_and_uninstall_a_device` checks that the generated `PreToolUse` group with matcher `AskUserQuestion` equals the one in `docs/hook-settings.json` apart from the command path. That includes `timeout: 330` and `statusMessage`, so the install.sh heredoc and the docs cannot drift apart unnoticed.
- Two sessions asking at once: slots `questions_of_two_sessions_never_mix`. A text in B's topic goes to B's session while A has «Другое» armed. B's button pressed on A's message changes nothing. Each hook gets only its own answer.
- Cyrillic through the real hook's stdout: e2e `own_text_answers_after_other_or_as_a_reply` now answers «Бирюзовый, как море» and asserts the exact decision JSON.

## Skipped

- Review "Missing coverage": an e2e that kills the running hook (Esc). The slots test with a dropped receiver covers that logic, and killing a child mid-wait adds a timing-sensitive e2e for no new code path. A test for the full book through the actor is also skipped (not in scope, and the unit test covers `MAX_ASKS`).
- Nits (statusMessage wording, empty callback answer on an id mismatch, a press before `message_id` is stored, the `CLAUDE.md` path to `done/`): not in the fix scope. The CLAUDE.md path becomes right when the task moves to done.
- Known limit of fix 1: a question the PreToolUse hook skips (duplicate question texts, more than 8 options, a call inside a subagent) also reaches the hub only as `PermissionRequest`. Its session gets the same one-time notice, although its hook is installed. The notice text is written to be true in both cases ("вопрос ждёт ответа в терминале"), but its install.sh advice is unneeded there. Telling the two cases apart would need the permission hook to rerun the question-shape check. That is not worth it for a once-per-session text.

## Test results

Shared target `CARGO_TARGET_DIR=C:/Users/user/dev/cctg/target`, `CARGO_PROFILE_DEV_DEBUG=0`, `-j 1`. Before each test run I touched `crates/cctg/src/lib.rs`: another worktree's build shares the metadata hash and had replaced the test binary (log `dead_end`, PCTX proposal).

- `cargo fmt --all -- --check`: clean.
- `cargo clippy -j 1 --workspace --all-targets --locked -- -D warnings`: clean.
- `cargo test -j 1 --workspace --locked --no-fail-fast` (`scratch/fix_workspace_test.txt`): **866 passed, 0 failed, 3 ignored**, 44 `Running` binaries. That is the reviewer's 862 plus the 4 new tests. `install_e2e` 9/9, `question_hook_e2e` 6/6. I confirmed the new tests by name in the output.
- `install.sh` was not touched (still `i/lf`, `text eol=lf`).
