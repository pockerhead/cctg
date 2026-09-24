# TASK-043 implementer summary

Mode small-fix, code commit `1742f2f` on `feature/console-commands`.

## 1. What was implemented

Flow: topic message -> `Slots::on_topic_message` -> `hub::console::classify` -> (console command) `HubMsg::ConsoleCommand` to the agent of the slot's live session -> agent console worker -> `keys::type_line` -> `AgentMsg::ConsoleCommandTyped` -> hub reacts 👀 (sent) or replies to the message (draft / failed). The output comes back through the normal transcript stream.

Files (git diff --stat, insertions/deletions):
- `crates/cctg/src/hub/console.rs` (new, +142): `classify(text) -> Option<Result<String, Invalid>>`. `!` + non-blank text, or `/name` (`[A-Za-z0-9_:.-]+`, optional `@bot` stripped, then a space or the end) is a console command; `/tmp/x`, `/ hi`, `!` alone are messages. Validity via `keys::typable`. Russian notices, `COMMAND_WAIT` 30 s, `MAX_COMMAND_ASKS` 32. The module doc records the busy decision.
- `crates/cctg/src/keys.rs` (+151/-): `type_exit` generalised into `type_line(pid, text)` (type, read the box back, Enter only on an exact match, else one Backspace per typed char). `type_exit` now calls `type_line(pid, "/exit")`. `box_is_exit` became `box_shows(lines, text)`: it also accepts bash mode, where the `!` may replace the `❯` glyph. `typable`: one non-blank line, at most `MAX_LINE_CHARS` = 200 chars, no control chars, no U+2028/2029, BMP only (so erasing stays one Backspace per char). `ExitTyped` renamed to `Typed`.
- `crates/cctg/src/wire.rs` (+61): `Register.console_commands` (serde default), `HubMsg::ConsoleCommand { command_id, text }`, `AgentMsg::ConsoleCommandTyped { command_id, outcome }`, `CommandOutcome { Sent, Draft, Failed, Other(serde other) }`. Kinds lists and round-trip samples are updated. `VERSION` stays 1 (gated by the capability).
- `crates/cctg/src/agent.rs` (+195/-): `Presser` + new `Typist` bundled in `Console`. The key worker became `spawn_console`, one worker for `ConsoleJob::{Key, Line}`, one at a time. A line that is not `typable` is answered `Failed` without typing. Register announces `console_commands` with the same condition as `console_keys` (Windows plus known claude pid).
- `crates/cctg/src/hub/slots.rs` (+345): `Conn.commands`, `command_asks`, `on_console_command` (checks in order: invalid, no live agent, agent without the capability, permission prompt waiting, `busy`, then send), `on_command_typed` (validated like `on_key_written`), replies via `Op::Send.reply_to`. The module doc gets a paragraph. 3 unit tests.
- `crates/cctg/src/hub/ingress.rs` (+2): forwards `ConsoleCommandTyped` (risk lesson TASK-016 QA). `channel.rs` (+5/-): ignores `HubMsg::ConsoleCommand`. `hub/mod.rs` (+1), `hub/commands.rs` doc (+5/-), `update.rs` (rename only).
- `crates/transcript/src/stream.rs` (+99): `console_record`. `<bash-input>` becomes `Prompt("! cmd")`. `<bash-stdout>`+`<bash-stderr>` and `<local-command-stdout>` become a `Note` holding the output as a fenced code block. The fence is longer than any backtick run. ANSI CSI and control chars are dropped. The block is cut to 20 lines / 1500 graphemes with `…`. Blank output gives no event. Shapes were checked on real transcripts: 64 bash records and 36 local-command records. Only the structure was surveyed: a bash-stdout record is always stdout+stderr in one string, and 14/36 local outputs carry ANSI.
- `crates/transcript/tests/fixtures/console_commands.jsonl` (new, 10 lines, made-up content, generator `scratch/make_console_fixture.py`) and `tests/stream.rs` (+26).
- `crates/cctg/tests/status_e2e.rs` (+72): the test agent announces `console_commands`. New real-TCP test `a_console_command_goes_over_the_link_and_its_answer_comes_back`. Checked that it fails (times out) when the ingress arm is removed.
- 8 test files and in-crate tests: `console_commands: false` added to `Register` literals (mechanical).
- `docs/poc.md` (+1/-1): check item 3 mentions `!`/slash commands.

## 2. Deviations / decisions

- **Busy session: refuse, not queue** (log.jsonl decision). A queue would have to outlive the turn and stay ordered against the slot buffer. The user can just resend.
- **Output rendering through the existing `Note`** (markdown code block), no new `StreamItem` kind. Old hubs stay compatible. A side effect: `<local-command-stdout>` of commands typed in the terminal (e.g. `/model`, `/cost`) now also appears in the stream. Before, the stream hid it; brief/full are unchanged.
- **Forwarded messages** are never commands (someone else's words): they go to the model as before.
- A command is never parked in the slot buffer. A dead or offline session gets `OFFLINE_NOTICE`: buffered command text would otherwise reach the model as a prompt later.
- `type_exit` semantics changed a little. An unreadable screen now gives `Failed` instead of `Draft`, so the update notice reads "failed" instead of "draft in input". A mismatching box still gives `Draft`.
- Replies to refused or failed commands are not throttled by `notify` (they answer a specific message). They still count against `MAX_QUEUED_MESSAGES`.

## 3. Test results

- `cargo fmt --all --check`: ok.
- `cargo clippy -j 1 --workspace --all-targets -- -D warnings`: clean.
- `cargo test -j 1 --workspace` (CARGO_TARGET_DIR=main target, DEBUG=0): all green. cctg lib 509 passed / 1 ignored, status_e2e 7 passed, transcript stream tests included. stdout is saved in `scratch/workspace_test.out.txt`, with no FAILED.

## 4. Manual verification / open items (not verified live: no interactive claude allowed here)

1. In a slot topic of a live Windows session (new agent after "Обновить"), send `!echo hi`. Expected: 👀 on the message, then `> ! echo hi` and a code block `hi` in the topic, with no model turn.
2. Send `/compact` or `/cost`. Expected: typed in the terminal, and `> /cost` plus its output block appear.
3. Put unsent text in the terminal box, then send `!ls`. Expected: the reply "В поле ввода терминала есть неотправленный текст…", and the draft is intact.
4. Send during a turn. Expected: "Сессия занята…". Multi-line or `!echo 😀`: the invalid notice.

Unverified assumptions, worth a live probe in QA:
- (a) The exact look of Claude Code bash mode in the box (`!` in place of `❯`). Both forms are accepted, and any other shape is erased and answered as a draft.
- (b) Whether a `!` command or a local slash command fires `UserPromptSubmit` without a `Stop`. That would leave the hub's activity "busy" and refuse later commands until the next turn ends.
- (c) Whether Enter on a typed `/name` with the slash menu open picks the highlighted item rather than the exact text (proved only for `/exit` in TASK-040 P2).
- (d) Long commands that wrap in a narrow console fail the one-line box check. They are erased and answered as a draft.
