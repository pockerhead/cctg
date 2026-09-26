# PCTX proposals (TASK-054)

## 2026-09-26, domain hub, "Flood control" invariant and TASK-008 line

Current text says edits and topic mutations are only serialized and that the topic-mutation lane is unmetered. After TASK-054 this is no longer true. Proposed wording:

- Flood control: new messages ride the message bucket (<=20/min, 1 s gap); edits, reactions, pins, deletes and topic mutations ride a second group bucket `EDIT_BUCKET` (<=20/min), so the group sees <=40 requests/min. `answerCallbackQuery` takes no token. `merge` stream lines are debounced per topic (`DEBOUNCE` 1.5 s quiet, `DEBOUNCE_MAX` 4 s) and a burst goes as one message; permission prompts never wait. Numbers live in `hub/scheduler.rs` (`Limits::default()` is the hub's pacing; a bare `BucketConfig` given to `Scheduler::new` means the old behaviour: no debounce, edits unmetered, used by fast-bucket tests).

Why: live 429s (12-28 s pauses of the whole group) with sends at 20/min and edits unbounded, 2026-09-26.

## 2026-09-26 (fixer), domain hub, addendum to the flood-control wording above

Add to the proposed "Flood control" line: edits have two classes. `Op::Edit { background: true }` is only the periodic pinned-status refresh (slots `pump_status`); every other edit and reaction is foreground. Order inside the edit budget: callback answers (no token), then topic calls and foreground edits in turns, then background refreshes round-robin by message (coalesced per message). A ⏹ status edit is foreground and may replace a queued refresh of its message; the ⏹ question's 10 s window restarts when Telegram shows it. With a debounce, a permission prompt lets the `merge` lines of its own topic queued before it go first, at once. Test for the real pacing: `slots::tests::with_the_hubs_pacing_topics_prompts_and_stop_stay_quick_under_status_churn` (paused time, `Limits::default()`).

Why: review of TASK-054 showed createForumTopic starving and user-visible edits waiting 40+ s behind status refreshes once two or more slots were busy.
