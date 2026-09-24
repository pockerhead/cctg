## Counter-example tested

If `UserPromptSubmit` is emitted for a prompt typed locally in the Claude session as well as for a Telegram-delivered prompt, then changing the pending Telegram message's reaction to ✍️ on any session-wide `UserPromptSubmit` can falsely mark an unrelated Telegram message as the turn being started; the stated reaction success predicate is therefore incomplete unless the event can be correlated to that exact inbound message.

## Primary-source investigation

- The raw hook fixture is a `UserPromptSubmit` whose prompt is the untagged local-looking text `private prompt text that must not reach the hub`; it has a Claude `prompt_id`, but no Telegram `message_id` or source marker (`crates/cctg/tests/fixtures/hook/user_prompt_submit.json:5-8`).
- A Telegram-originated user record is observably different: the raw jsonl fixture contains `<channel source="cctg" ... message_id="7">...` and is marked `isMeta: true` (`crates/transcript/tests/fixtures/final_answer.jsonl:8`). At delivery, the hub puts the Telegram `message_id` into channel metadata (`crates/cctg/src/hub/slots.rs:937-953`), and the channel forwards that metadata (`crates/cctg/src/channel.rs:176-180`).
- The hook conversion discards the prompt and emits only its unrelated Claude `prompt_id` for every `UserPromptSubmit` (`crates/cctg/src/hook.rs:186-188`; wire shape at `crates/cctg/src/wire.rs:406-409`). I ran `cargo test -p cctg each_event_carries_its_fields_and_nothing_else -j 1 -- --nocapture`; its real result was `test hook::build_tests::each_event_carries_its_fields_and_nothing_else ... ok` and `1 passed; 0 failed`. The asserted behavior is explicit at `crates/cctg/src/hook.rs:622-632`: output keys are only `type` and `prompt_id`, and the prompt text is absent.

## Did it hold

Yes. The primary sources contain both a generic, untagged `UserPromptSubmit` and a Telegram-originated prompt carrying a distinct `message_id`, while the event delivered to the hub preserves only `prompt_id`. Therefore a session-wide `UserPromptSubmit` is not positive evidence that a particular queued Telegram message started the turn. The acceptance criterion can be satisfied by tests that send Telegram message then hook in sequence while still mis-marking a pending Telegram message when the next prompt was entered locally.

## Verdict

PREMISE SUSPECT — `crates/cctg/tests/fixtures/hook/user_prompt_submit.json:5-8` shows an untagged `UserPromptSubmit`, while `crates/transcript/tests/fixtures/final_answer.jsonl:8` and `crates/cctg/src/hub/slots.rs:937-953` show that only Telegram-originated input carries its `message_id`, which `crates/cctg/src/hook.rs:186-188` does not preserve ; smallest implied reframing: require the ✍️ transition to be correlated to the exact Telegram `message_id`, and require non-Telegram `UserPromptSubmit` events not to consume or alter a pending Telegram receipt.
