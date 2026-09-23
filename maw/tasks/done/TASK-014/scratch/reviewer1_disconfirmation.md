# Disconfirmation case

Before evaluating the plan, test this concrete counter-example:

1. A live session opens a permission prompt and Telegram returns its `message_id`.
2. The hub receives `SessionEnd` for that session before any button is pressed.
3. The user presses the old Allow button.

The reviewed design is wrong if it leaves the prompt/buttons open or can forward a verdict after SessionEnd. The orchestrator decision requires closing prompts on SessionEnd, so the expected behavior is: remove the buttons, clear waiting state as part of normal end handling, and make a late callback harmless (answer stale/already closed, no verdict).

## Result

The counter-example held. `OPEN_DECISIONS.md` requires closing the requesting session's open prompts on `SessionEnd`, but the reference `slots.rs::on_hook` only removes transcript scan state and the prompt book has no closed state. Its test `a_prompt_goes_to_the_slot_of_its_session_after_the_slot_moved_on` explicitly keeps the old prompt open after SessionEnd. The original plan also calls this behavior intentional, so it must be revised.

## Independent build

Copied `scratch/planner/ws` to `scratch/reviewer1_ws` and ran:

`CARGO_TARGET_DIR=%TEMP%\cctg-t014-reviewer1-target CARGO_PROFILE_DEV_DEBUG=0 cargo test --workspace --no-fail-fast -j 1`

Result: exit 0; cctg library 250 passed / 1 ignored, all integration, transcript, and doc tests passed. The target directory was deleted after the run. Passing tests do not cover the SessionEnd counter-example or the hub-to-agent delivery-loss window.
