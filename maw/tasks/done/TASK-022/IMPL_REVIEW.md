# TASK-022 IMPL_REVIEW (code-reviewer, small-fix)

## Verdict

**PASS.** Every acceptance criterion holds in the code and in tests. fmt, clippy `-D warnings` and `cargo test --workspace` are green on my own run. The findings below are all minor: docs and a note on scope.

## Disconfirmation

What I tried to break: the same final answer reaching the topic twice, or a Stop reaching the wrong topic (nested run, ended session, a slot that moved on after `/clear`, a session with no topic).

- Duplicate by retry: `hook::post` is one-shot, and hub ingress dedups by `event_id` anyway. SubagentStop is a separate `HookEvent` variant, so the new `if let HookEvent::Stop` (`slots.rs:496-503`) never matches it. The only double path left is the model calling `reply` with its final text as well. The new INSTRUCTIONS and the `reply_tool` description address that. **Held.**
- Wrong topic: `current_slot` (`slots.rs:636-643`) requires `is_live_top_level` (not ended, kind TopLevel) and `slot.current_session == session`. Nested, unknown, ended and superseded sessions return `None`. A missing `topic_id` returns before the send. The test `only_a_live_top_level_current_session_with_a_topic_sends_its_answer` walks all of these and asserts exactly one op. **Held.**

## Confirmed correct

- `slots.rs:496-503`: Stop with `Some(answer)` goes to `on_turn_answer`. It runs after `apply_hook`, so it sees state after the hook, and `prompts.quiet` is unchanged.
- `slots.rs:645-674` `on_turn_answer`: blank or whitespace returns early. Routing goes through `current_slot` and needs no agent connection (decision logged, and it matches the spec: the answer comes from the hook).
- `slots.rs:708-738` `send_text`: split, document and cap logic moved out of `on_reply_for` without behaviour change. The document name is `reply-…` or `answer-…`. `send_messages` is still the one gate for the shared 256 cap, its atomic multi-chunk rejection and its single `overflow_warned` warning.
- `live_reply_slot` refactor (`slots.rs:624-632`): same predicate as before (agent == conn, live top-level, current session of the slot). Only the order of checks changed.
- The actor never awaits: `send_messages` → `hand_off` → unbounded `dispatch.send`. `a_stalled_telegram_never_stalls_turn_answers` pushes 1500 Stops through the bounded hook channel against stalled sends, and inbound still arrives.
- Logs: `turn answer queued` carries ordinal, short session id and part count. No text, no user id. `tests/message_logs.rs` asserts the answer reaches Telegram and its text never shows up in captured logs.
- `channel.rs:53-62` INSTRUCTIONS: the final answer is automatic, `reply` is only for extra messages, `target_agent` → SendMessage and "never ask for permissions through `reply`" are kept. The test asserts both the new text and the kept rules. `reply_tool` description is consistent with it.
- No new crates. Cargo.toml and Cargo.lock are unchanged.
- The flaky `one_slot_lives_through_hook_agent_end_and_the_next_session` (`slots.rs:3510`) sends no Stop and fails on a fixed 200 ms wait for `registry.json` on disk under `-j 1` load. The failure in `scratch/implementer/cargo-test.txt:391` is `current_session` still A on disk. This is pre-existing save timing, not TASK-022. It passed in my full run.

## Issues

1. **minor**, `docs/poc.md:84-106`: with `MSYS_NO_PATHCONV=1` Git Bash no longer converts paths. So `<tmp>` must be written Windows-style with forward slashes (`C:/Users/.../cctg-poc`). The natural Git Bash forms `/tmp/...`, `/c/...` or `$TMP` will reach native `claude.exe` unconverted and break `--mcp-config` / `--settings`. The section does not say this. Fix: one sentence saying the paths (and `CLAUDE_CONFIG_DIR`) must be in `C:/...` form.
2. **minor**, `docs/poc.md:112` (check item 2): "поэтому ответ не дублируется" is stated as a guarantee. It only holds if the model follows the instructions. Fix: "не должен дублироваться; если Claude всё же повторил ответ через `reply`, придут два сообщения".
3. **minor (scope note, no code change required by spec)**, `slots.rs:645`: the answer of every turn of every live top-level session goes to Telegram. That includes turns typed in the terminal and sessions without a channel (hooks installed without the flag). This matches the spec's wording ("каждого хода top-level сессии"). But combined with user-scope hooks, all terminal answers on the machine now end up in the forum. Worth stating once in `docs/poc.md` or the hub domain notes so it is a known behaviour, not a surprise.
4. **minor**, `slots.rs:1121`: the overflow warning text still says "new replies and notices are dropped". Answers are dropped by it too. Fix: "new replies, answers and notices".

## Missing coverage

- No slots test for the `/clear` transfer path (SessionEnd reason=clear → SessionStart source=clear, same pid) followed by a late Stop of the old session. The predicate is the same as the "slot moved on" case already covered, so the risk is low.
- No test where both an agent `reply` and a Stop in one turn arrive in reverse order (agent frame processed after the hook). The result is an "extra" message showing up after the final answer. This is cosmetic, and ordering across the two independent channels is not guaranteed by design.

## Nits

- `on_turn_answer` logs "slot without a topic yet" at `info` while the reply path uses `warn` for the same situation. Either level is fine; they are just inconsistent.
- A nested `claude -p --resume <A>` of a known top-level session A keeps A's kind (TASK-011 rule), so its Stop would post into A's topic. The hook's Stop carries no `claude_pid` to tell it apart. This is an edge case and arguably correct (it is A's turn).
