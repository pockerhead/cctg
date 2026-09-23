# TASK-021 PLAN: hub routes topic messages to the session agent and replies back

Reference implementation: `scratch/planner/ws/` (copy of HEAD `cfcf737`, LF). Patch against the repo root: `scratch/planner/task021.patch` (8 files, `git apply --check` passes on HEAD). Hashes of the resulting files (LF bytes): `scratch/planner/hashes.txt`, check with `bash maw/tasks/in_progress/TASK-021/scratch/planner/verify_hashes.sh` after applying. Rebuild patch and hashes from `ws/`: `python scratch/planner/build_patch.py`.

Evidence in `scratch/planner/`: `workspace_test.txt` (full `cargo test --workspace` on the patched tree: 19 test binaries ok, lib 232 passed), `mutations.out.txt` (4 mutations, all killed), `probe/cmd.txt` (`--settings` hooks probe). `cargo fmt --check` and `cargo clippy --workspace --all-targets -- -D warnings` are clean on `ws/`.

Build notes for the implementer (host memory is tight): `CARGO_TARGET_DIR` outside the repo (e.g. `%TEMP%\cctg-t021-target`), `CARGO_PROFILE_DEV_DEBUG=0`, `-j 1`, one cargo at a time.

## 1. Understanding

What exists today (HEAD `cfcf737`):

- `crates/cctg/src/hub/updates.rs:34-40, 94-119` — `classify` builds `Routed::Input(Inbound { message_id, thread_id, text })` only for allowlisted senders in the configured chat; forum service messages are `Routed::Service` before the allowlist check; `thread_id` is `None` in General (`is_topic_message` filter). No reply-to field.
- `crates/cctg/src/hub/api.rs:77-90` — `Message` has no `reply_to_message`.
- `crates/cctg/src/hub/mod.rs:56-82` — `route_inbound`: commands (`commands::is_command`, any text starting with `/`, `commands.rs:72-78`) go to the command worker; every other `Routed::Input` is only logged (`info!(thread = ..., "inbound message")`, line 69). This is the missing link.
- `crates/cctg/src/hub/slots.rs` — the only owner of the registry. `Control` (60-68) carries only `TopicEdited` and is `Copy`. `Conn` (167-174) keeps the agent's `mpsc::Sender<HubMsg>` as unused `_to_agent`. `on_agent` (261-313) handles `PermissionRequest`; `Reply` falls into `"agent message not routed yet"` (301). `agent_session`/`follow_pid` (315-366) rebind a connection to the new session of the same claude pid after `/clear` (premise challenge re-ran `the_agent_follows_its_claude_process*`: 2 passed). All Telegram work leaves the actor through `hand_off` (431-436) to `dispatch_loop` (624-640), which awaits room in the scheduler queue and reports back as `Done`.
- `crates/cctg/src/hub/registry.rs` — `slot_by_topic` (398), `is_live_top_level` (786), `SessionEntry.agent` (301, set by `agent_connected` 765 only for top-level sessions), `SessionEntry.slot` (287), `Slot.current_session`/`topic_id` (250-252).
- `crates/cctg/src/hub/ingress.rs:33, 187, 228-235` — per agent a bounded `to_agent` queue (`TO_AGENT_QUEUE` = 64) drained by the connection task with 5 s write timeout.
- `crates/cctg/src/wire.rs:141-145, 176-180` — `AgentMsg::Reply { text }` (no session id: identity comes from the connection), `HubMsg::Inbound { content, meta: BTreeMap<String,String> }`. No wire change needed.
- `crates/cctg/src/channel.rs:174-180, 394-412` — agent side already turns `Inbound` into `notifications/claude/channel` and drops meta keys outside `[A-Za-z0-9_]`; `reply` tool text is capped at 128 KiB (48).
- `crates/cctg/src/hub/scheduler.rs` — one outbound queue; `Op::Send`/`Op::SendDocument` are FIFO per topic in the message lane; `Outbox::submit` waits for queue room (so only the dispatch task may call it from the actor side).
- `transcript::split_for_telegram` (`crates/transcript/src/split.rs`) — chunks within 4096 UTF-16 units, `prefer_file` when more than 4 chunks; `commands::deliver` follows the same rule for `/brief`.

Platform facts used (verified, not re-researched): meta keys `[A-Za-z0-9_]` (CLAUDE.md "Channels"); `/clear` keeps the channel server and the hub rebinds by `claude_pid` (channel domain, TASK-013).

New facts found by this planner:

- In a forum topic Telegram sets `reply_to_message` on every message to the topic root, so a naive reply-to meta would mark every message as a reply ([hermes-agent #118678](https://github.com/NousResearch/hermes-agent/issues/118678), [Bot API: Message.reply_to_message](https://core.telegram.org/bots/api#message)). Explicit reply = `reply_to_message.message_id != message_thread_id`.
- `claude --settings <file>` with a `hooks` block: the [CLI reference](https://code.claude.com/docs/en/cli-reference) says hooks are not loaded from `--settings`, the [settings page](https://code.claude.com/docs/en/settings) says `--settings` can set any user-settings key. Probe on 2.1.280 (`scratch/planner/probe/cmd.txt`): a `SessionStart` hook from `--settings` fired in `claude -p`. The recipe uses it, with `.claude/settings.local.json` of the probe folder as the named fallback.

## 2. Approach

One path in, one path out, both through the existing slot actor, no new task and no new owner of bindings.

**Inbound (Telegram -> agent).** `route_inbound` sends every non-command `Routed::Input` to the slot actor as `Control::Message(Inbound)` over the existing unbounded control channel (the poll callback never waits). The actor resolves `thread_id -> slot -> current_session` and requires a live top-level session with a bound agent connection, then does `to_agent.try_send(HubMsg::Inbound { content, meta })`. `try_send` never waits: a full (64) or closed agent queue counts as "not on line". Meta: `chat_id`, `message_id`, `thread_id`, and `reply_to_message_id` only for an explicit reply. General and topics that are not slots: nothing is forwarded and nothing is answered (debug log). Slot topic but no live session / no agent / queue full or closed: one fixed notice per message, `OFFLINE_NOTICE`. A message without text (photo, sticker): `TEXT_ONLY_NOTICE`. Commands stay on the command worker exactly as today; service messages never become `Routed::Input` (`classify`).

**Outbound (agent reply -> Telegram).** `AgentMsg::Reply` from connection `conn` goes to the topic of `conns[conn].session`'s slot, only if the registry has that session's `agent == Some(conn)` (an unbound or nested-run agent is dropped). After `/clear` the connection already follows its claude pid (`follow_pid`), so replies land in the right slot with no extra code. Text is split with `split_for_telegram(SplitOptions::default())`: chunks as ordered `Op::Send`, or one `Op::SendDocument` (`reply-<short id>.txt`) when `prefer_file`, the same rule `/brief` uses. Every chunk and notice is handed to the existing `dispatch_loop` as a new `Work::Message`; the loop submits in order, so the scheduler's per-topic FIFO keeps chunk order, and answers come back as `Done::Message`, never awaited by the actor.

**Backlog cap.** Replies used to be unrouted; now an agent can fill the unbounded dispatch channel while Telegram is in `retry_after`. The actor counts messages handed out and not answered (`queued_messages`) and refuses a reply or notice that would pass `MAX_QUEUED_MESSAGES` = 256, whole (never half a reply), with one warning per overflow episode. 256 is ~13 minutes of the group's 20 messages/min budget; anything beyond that is stale anyway.

**Why not the alternatives.** A separate router task with its own copy of bindings would be a second owner of slot/session/agent state (TASK-011 made `Slots` the only owner). Reusing `commands::deliver` per reply awaits each chunk's answer; one spawned task per reply lets two replies interleave. The too-long fallback of `deliver` is not needed: `split_for_telegram` guarantees each chunk fits.

**Logging.** Log lines carry the slot ordinal, the short session id, chunk count and fixed text. Never message text, never the user id (the `Inbound` struct has none), never the chat id in logs (it is only in meta sent to Claude). `ApiError` Display for send errors carries Telegram's description, not our text.

## 3. Steps

All paths relative to the repo root. The reference `ws/` has each step applied; `task021.patch` is the exact diff.

1. `crates/cctg/src/hub/api.rs` — add `#[derive(Debug, Clone, Copy, Default, Deserialize)] #[serde(default)] pub struct MessageRef { pub message_id: i64 }` and `pub reply_to_message: Option<MessageRef>` on `Message` (doc comment: in a topic every message points at the topic root). Narrow struct, no recursion into `Message`.
2. `crates/cctg/src/hub/updates.rs` — `Inbound` gets `pub reply_to: Option<i64>`. In `classify`, compute `thread_id` first, then `reply_to = reply_to_message.map(|r| r.message_id).filter(|&id| id != 0 && Some(id) != thread_id)`. Update the existing `allowlisted_text_is_input_and_strangers_are_dropped` expectation (`reply_to: None`) and add `only_an_explicit_reply_is_a_reply` (topic root -> `None`, id 42 -> `Some(42)`, absent -> `None`, General reply -> `Some(42)`).
3. `crates/cctg/src/hub/commands.rs` (test `run`, `Inbound { .. }` literal) and `crates/cctg/tests/command_logs.rs` (`fn input`) — add `reply_to: None` to the struct literals. Nothing else in commands changes.
4. `crates/cctg/src/hub/slots.rs`, production part:
   - module doc: add the routing paragraph and "never ... message text".
   - imports: `BTreeMap`, `transcript::{SplitOptions, split_for_telegram}`, `super::api::Document`, `super::updates::Inbound`.
   - constants: `pub const MAX_QUEUED_MESSAGES: usize = 256;`, `pub const OFFLINE_NOTICE: &str = "Сессия этой темы не на связи, сообщение не доставлено.";`, `pub const TEXT_ONLY_NOTICE: &str = "В сессию пока доходят только текстовые сообщения.";`.
   - `Options`: `pub chat_id: i64` (default 0), doc "passed to Claude as `chat_id` meta".
   - `Control`: drop `Copy`, add `Message(Inbound)`.
   - `Done::Message(Option<Delivery>)`, `Work::Message`; `dispatch_loop` maps `Work::Message` to `Done::Message`.
   - `Conn`: rename `_to_agent` to `to_agent` (struct field and the `on_agent` constructor).
   - `Slots` fields `queued_messages: usize`, `overflow_warned: bool` (init 0/false).
   - `on_agent`: `AgentMsg::Reply { text } => self.on_reply(conn, &text)`.
   - `on_control`: match; `Control::Message(input) => return self.on_topic_message(input)`; the `TopicEdited` body stays as is.
   - new `on_topic_message(&mut self, input: Inbound)`: `thread_id None` -> `debug!` return; `slot_by_topic` None -> `debug!` return; `text None` -> `notify(TEXT_ONLY_NOTICE)`; `live_agent(slot)` None -> `info!(ordinal, "message for a session that is not on line; user notified")` + `notify(OFFLINE_NOTICE)`; else build meta (`chat_id`, `message_id`, `thread_id`, optional `reply_to_message_id`) and `try_send`; `Ok` -> `info!(ordinal, session, "message forwarded to the session agent")`; `Err` -> `warn!(ordinal, session, "agent queue full or closed; user notified")` + notice.
   - new `live_agent(&self, slot) -> Option<(String, u64)>`: slot's `current_session`, `is_live_top_level`, `sessions[s].agent`, and the conn still present in `conns`.
   - new `on_reply(&mut self, conn, text)`: session from `conns[conn]`; require `sessions[session].agent == Some(conn)`, take `entry.slot`, then `slot.topic_id` (no topic yet -> `warn!(ordinal, "reply for a slot without a topic yet; dropped")`); split; ops as described; `send_messages(ops)`; `info!(ordinal, session, parts, "agent reply queued")`.
   - new `notify(&mut self, thread_id, text)` and `send_messages(&mut self, ops: Vec<Op>) -> bool` (all-or-nothing against the cap, one `warn!` per overflow episode, `hand_off(Work::Message, op)` in order).
   - `on_done`: `Done::Message(delivery)` -> `queued_messages -= 1` (saturating), reset `overflow_warned` at 0, `warn!(%error, "message to a topic not delivered")` / `warn!("message to a topic got no answer")`.
   - free fn `message_op(thread_id, text) -> Op::Send { thread_id: Some(..), reply_markup: None, permission: false }`.
5. `crates/cctg/src/hub/slots.rs`, tests (fake transport, fake agent link = the `to_agent` receivers of `Rig`):
   - `Fake` gets `stall_sends: bool` (sends never return, topic calls still answer).
   - helpers `CHAT`, `say`, `message_options`, `received`, `sent_to`, `two_live_slots`, `reply`.
   - `a_topic_message_reaches_only_the_agent_of_its_slot` (exact `Inbound` + meta for two messages incl. `reply_to_message_id`, keys pass `channel::is_meta_key`; General and topic 999 reach nobody and cause no op; the other slot's agent gets nothing).
   - `a_message_nobody_can_take_gets_one_notice_each` (no agent, closed agent queue, non-text: `[OFFLINE, OFFLINE, TEXT_ONLY]`).
   - `an_ended_session_gets_no_inbound_even_with_its_agent_still_linked` (kills mutation M1).
   - `after_clear_the_new_session_of_the_slot_gets_the_message` (acceptance 3, `/clear` by pid, same topic).
   - `a_reply_goes_to_its_session_topic_in_split_order` (3 x 3000-char paragraphs -> exact `split_for_telegram` chunks then `"short"`, only in topic 101; a 5 x 4096 reply -> one `SendDocument` with the whole text in topic 100).
   - `a_reply_from_an_agent_without_a_slot_is_dropped` (unbound conn and unknown conn: no op).
   - `a_stalled_telegram_never_stalls_inbound` (sends stall; 1500 replies through the bounded agent channel and 1500 offline notices, then an inbound still reaches the agent; acceptance 5).
   - `the_backlog_of_messages_for_telegram_is_capped` (direct actor, stalled scheduler: counter stops at 256, one freed place is taken again; kills M3).
   - Test rule: wait for the effect of an input before sending the next input on another channel (`select!` has no order between channels).
6. `crates/cctg/src/hub/mod.rs` — `route_inbound`: `Routed::Input(input) => control.send(Control::Message(input))` with `warn!("slot actor stopped; message dropped")` on error, replacing the `info!(thread = ...)` line; update its doc comment. `run`: `slots::Options { icons, chat_id: config.chat_id, can_delete, .. }`. Test `messages_go_to_the_slot_actor_and_commands_do_not` (text and non-text go to control, `/brief 2` only to commands, `TopicCreated`/`TopicClosed` to neither; kills M4).
7. `crates/cctg/tests/message_logs.rs` (new, own binary, global subscriber, `.without_time()`, fast `BucketConfig`) — real update JSON with a distinctive user id through `route_batch` -> `Control::Message`; offline notice, forwarded inbound, reply; asserts the three fixed log lines appear and that the texts, the word `private` and the user id do not (acceptance 6).
8. `docs/poc.md` (new) — the local end-to-end recipe: repo `.env`, `~/.cctg/device.env` with the secret (cctg's own file, not Claude Code config), `cargo build --release`, `target/release/cctg hub` from the repo root, a temp `mcp.json` (`cctg agent`) and temp `settings.json` (hooks of `docs/hook-settings.json` with an absolute quoted path), `claude --mcp-config ... --strict-mcp-config --settings ... --dangerously-load-development-channels server:cctg` in one fixed folder, a six-point checklist, cleanup. No secrets, no machine paths (placeholders only). Says that `--settings` hooks were observed working on 2.1.280 despite the CLI reference, and names the `.claude/settings.local.json` fallback. The orchestrator runs it after merge; nobody runs it during the pipeline.

Checks: `cargo fmt --all -- --check`; `cargo clippy --workspace --all-targets -- -D warnings`; `cargo test --workspace` (all pre-existing tests pass unchanged apart from the two `reply_to: None` literals and one expectation in step 2).

Acceptance map: 1 -> step 5 first test + step 2; 2 -> reply test; 3 -> notice test, ended test, after-clear test; 4 -> step 6 test + step 5 (General/foreign) + existing `classify` allowlist tests; 5 -> stalled test + cap test + `try_send` only; 6 -> step 7; 7 -> `workspace_test.txt`.

## 4. Risk areas

- **Reordering across actor inputs.** Control, agent and hook events are separate channels in one `select!`. A message sent right after SessionStart/agent registration may be judged before the binding exists and get an offline notice. In production this is a window of milliseconds; in tests it is a flake unless each test waits for the previous effect (hit once while building `message_logs.rs`, logged as dead_end).
- **Separator vs first reply.** The session separator is a topic job that goes out only when the slot is not busy; a reply queued in the same instant can land above the separator. Cosmetic; not fixed here.
- **Reply before the topic exists.** A reply for a slot whose `createForumTopic` is still in flight is dropped with a warning. Happens only if Claude replies within the first ~0.4 s of a brand-new slot.
- **Silent drops by design.** Agent queue full (64) means the agent link is stuck for seconds; the user gets the offline notice, the message is not retried (buffering is TASK-017). The 256 cap drops whole replies under a long Telegram stall; the agent's `reply` tool already told Claude "sent", so Claude does not know. Warn is logged once per episode.
- **Topic deleted by hand.** A reply to a deleted topic fails with "message thread not found" and is only logged; the replacement topic comes from the next topic edit, not from the reply path.
- **`--settings` hooks.** Docs contradict each other; only one `-p` observation. If the live PoC shows no topic at start, use the `.claude/settings.local.json` fallback written in `docs/poc.md`.
- **Commands swallow every `/text`.** Any text starting with `/` goes to the command worker, so a Claude Code slash command typed in Telegram (e.g. `/compact`) never reaches the session. Intended by the task, may surprise users.
- **Meta values are not escaped by us.** Values are numeric strings; content is passed verbatim, as Claude Code expects.

## 5. Open questions

1. Non-text messages: the plan answers with `TEXT_ONLY_NOTICE` instead of silence. Alternative: forward the caption of media when present. Kept out (task says text; `Message` has no `caption` field yet).
2. General and non-slot topics get no notice. If the user wants "no session here" feedback in General too, it is one line in `on_topic_message`.
3. The notice goes once per undelivered message, not once per burst. With the group limit of 20/min, ten quick messages to a dead topic spend half a minute of budget. A per-slot rate limit (e.g. one notice per 30 s) is cheap to add if the live PoC shows it matters.
4. `reply_to_message_id` carries only the id. Claude cannot see which bot message was answered; adding a short quote of the replied text (from `reply_to_message.text`) is possible later without a wire change.
