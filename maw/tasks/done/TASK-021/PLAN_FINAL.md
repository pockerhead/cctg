# PLAN FINAL — TASK-021: hub routes topic messages to the session agent and replies back

## 1. Summary

The hub closes the missing PoC link. Every allowlisted, non-command message in a forum topic goes from the update poll to the slot actor as `Control::Message(Inbound)`. The actor resolves `thread_id -> slot -> current live top-level session -> bound agent connection` and does a non-blocking `try_send(HubMsg::Inbound { content, meta })` with meta `chat_id`, `message_id`, `thread_id` and, only for an explicit reply, `reply_to_message_id`. A message nobody can take gets a fixed notice (`OFFLINE_NOTICE`, or `TEXT_ONLY_NOTICE` for non-text), at most once per slot per notice kind per 60 s (`Options::notice_every`); a delivered message re-arms the offline notice at once. An `AgentMsg::Reply` from a bound agent goes to its session's slot topic, split by `transcript::split_for_telegram` into ordered `sendMessage` ops or one document, handed to the existing dispatch task; the actor never awaits Telegram. At most 256 messages wait for Telegram (`MAX_QUEUED_MESSAGES`), with one warning per overflow episode. `docs/poc.md` gives the live recipe, isolated from the user's Claude Code config with `CLAUDE_CONFIG_DIR`.

The implementer does NOT write this code from prose. The complete, built and tested reference is `maw/tasks/in_progress/TASK-021/scratch/reviewer2/ws/` (a copy of the repo tree, LF line endings). The implementer copies the changed files from it, or applies `scratch/reviewer2/task021.patch`, and verifies the result with sha256 hashes.

## 2. Implementation steps

All paths are relative to the repo root `C:/Users/user/dev/cctg`. `R` = `maw/tasks/in_progress/TASK-021/scratch/reviewer2`. The branch is `feature/hub-message-routing`; the patch was checked against HEAD `961daf8` (the source files are identical to `cfcf737`, where the planner started).

### Step 0. Apply

Either:

```
git apply maw/tasks/in_progress/TASK-021/scratch/reviewer2/task021.patch
```

or copy these 9 files from `R/ws/<path>` to `<path>` (byte copy; the files are LF). Then run `bash maw/tasks/in_progress/TASK-021/scratch/reviewer2/verify_hashes.sh`; every line must say `OK` (it hashes with CR stripped, so a CRLF checkout also passes).

sha256 of the LF bytes (`R/hashes.txt`):

```
5288952075d2af1e7476309fdc01e0ed97897b95c2a6ef9153f33f0ac8b19363  crates/cctg/src/hub/api.rs
36b3df8b93593c475f3190676711b828647833ef9d13e075dfe3a61bee90b9c1  crates/cctg/src/hub/commands.rs
c9e379f41ac9ffecf9b6bdb31200ee249b10e0cf94a84062b92ab1e978e2ccd1  crates/cctg/src/hub/mod.rs
81dcd727d1629a1a963bfed84599de9c1cc1852a87755c78c3844d86366b49d2  crates/cctg/src/hub/slots.rs
7f4d3d3fa99c43585ab1f34e70c94f96ba94e0ece2a298a958aae23427147ff8  crates/cctg/src/hub/updates.rs
de4a352af02d058b82fff29f37f451f38afd627dc6d5240f726e58a9bfc437d8  crates/cctg/tests/command_logs.rs
c80cf1ed7f1060191b45e1a9cd58d2f72fd4fb7cc53183e4c550d99f75a8db6e  crates/cctg/tests/message_logs.rs
7b13cadd082794e1fcd9ac77af0c3240b17ba7a60ee2b25febd32d7825d346a1  crates/cctg/tests/overflow_logs.rs
098c6840d65304c64e64d58a588cbe2eab4d352202854528bc201e585a2a0944  docs/poc.md
```

What each file contains (the reason for each change). Nothing else in the repo changes.

1. `crates/cctg/src/hub/api.rs` — `Message.reply_to_message: Option<MessageRef>` and `#[derive(Debug, Clone, Copy, Default, Deserialize)] #[serde(default)] pub struct MessageRef { pub message_id: i64 }`. Narrow struct, no recursion into `Message`. Reason: explicit-reply meta.
2. `crates/cctg/src/hub/updates.rs` — `Inbound.reply_to: Option<i64>`. `classify` computes `thread_id` first, then `reply_to = reply_to_message.map(|r| r.message_id).filter(|&id| id != 0 && Some(id) != thread_id)`. Reason: in a forum topic Telegram sets `reply_to_message` to the topic root on every message; only another id is an explicit reply. Test `only_an_explicit_reply_is_a_reply`; the existing allowlist test gets `reply_to: None`.
3. `crates/cctg/src/hub/commands.rs` (test literal) and `crates/cctg/tests/command_logs.rs` (`fn input`) — `reply_to: None` in `Inbound` literals. Nothing else.
4. `crates/cctg/src/hub/mod.rs` — `route_inbound`: `Routed::Input(input)` that is not a command goes to `control.send(Control::Message(input))`, `warn!("slot actor stopped; message dropped")` on error (replaces the old `info!(thread = ..., "inbound message")`); doc comment updated. `run` passes `chat_id: config.chat_id` into `slots::Options`. Test `messages_go_to_the_slot_actor_and_commands_do_not`.
5. `crates/cctg/src/hub/slots.rs`, production:
   - module doc: routing paragraph, the notice rule, "never ... message text".
   - constants `MAX_QUEUED_MESSAGES = 256`, `OFFLINE_NOTICE = "Сессия этой темы не на связи, сообщение не доставлено."`, `TEXT_ONLY_NOTICE = "В сессию пока доходят только текстовые сообщения."`.
   - `Options`: `pub chat_id: i64` (default 0), `pub notice_every: Duration` (default 60 s).
   - `Control` loses `Copy`, gains `Message(Inbound)`. `Work::Message`, `Done::Message(Option<Delivery>)`; `dispatch_loop` maps one to the other.
   - `Conn._to_agent` renamed `to_agent`.
   - `Slots` fields `queued_messages: usize`, `overflow_warned: bool`, `notices: HashMap<(SlotId, &'static str), Instant>`.
   - `on_agent`: `AgentMsg::Reply { text } => self.on_reply(conn, &text)`.
   - `on_control`: `Control::Message(input) => return self.on_topic_message(input)`; the `TopicEdited` body is unchanged.
   - `on_topic_message`: no thread -> `debug!`, return; topic that is not a slot -> `debug!`, return; no text -> `notify(slot, thread, TEXT_ONLY_NOTICE)`; `live_agent(slot)` is `None` -> `info!(ordinal, "message for a session that is not on line; not delivered")` + `notify(slot, thread, OFFLINE_NOTICE)`; otherwise build the meta and `try_send`. `Ok` -> `self.notices.remove(&(slot, OFFLINE_NOTICE))` + `info!(ordinal, session, "message forwarded to the session agent")`; `Err` -> `warn!(ordinal, session, "agent queue full or closed; not delivered")` + `notify(slot, thread, OFFLINE_NOTICE)`.
   - `live_agent(slot)`: the slot's `current_session`, `is_live_top_level`, `sessions[s].agent`, and that conn still in `conns`.
   - `on_reply(conn, text)`: session from `conns[conn]`; requires `sessions[session].agent == Some(conn)`; its `slot`; `slot.topic_id` (none -> `warn!(ordinal, "reply for a slot without a topic yet; dropped")`); `split_for_telegram(text, SplitOptions::default())`; `prefer_file` -> one `Op::SendDocument` named `reply-<short id>.txt` with the whole text, else one `Op::Send` per chunk in order; `send_messages(ops)`; `info!(ordinal, session, parts, "agent reply queued")`.
   - `notify(slot, thread_id, notice: &'static str)`: if `notices[(slot, notice)]` is younger than `notice_every`, `debug!` and return; else `send_messages(one op)` and, only if accepted, record `now`.
   - `send_messages(ops) -> bool`: all or nothing against `MAX_QUEUED_MESSAGES`; on refusal one `warn!("too many messages wait for Telegram; new replies and notices are dropped")` per episode (`overflow_warned`); on accept `hand_off(Work::Message, op)` in order.
   - `on_done`: `Done::Message` -> `queued_messages -= 1` (saturating), `overflow_warned = false` at 0, `warn!(%error, "message to a topic not delivered")` or `warn!("message to a topic got no answer")`.
   - free fn `message_op(thread_id, text) -> Op::Send { thread_id: Some(..), reply_markup: None, permission: false }`.
6. `crates/cctg/src/hub/slots.rs`, tests (`Fake.stall_sends`; helpers `CHAT`, `say`, `message_options`, `received`, `sent_to`, `two_live_slots`, `reply`, `stalled_slots`):
   - `a_topic_message_reaches_only_the_agent_of_its_slot`
   - `a_message_nobody_can_take_gets_one_notice_each` (runs with `notice_every: ZERO`: it checks each path, not the rate)
   - `an_ended_session_gets_no_inbound_even_with_its_agent_still_linked`
   - `after_clear_the_new_session_of_the_slot_gets_the_message`
   - `a_reply_goes_to_its_session_topic_in_split_order`
   - `a_reply_from_an_agent_without_a_slot_is_dropped`
   - `a_stalled_telegram_never_stalls_inbound`
   - `a_burst_to_a_dead_slot_gets_one_notice_a_minute` (new, `start_paused`): 10 messages to a dead slot -> 1 notice; 5 photos -> 1 text-only notice; a burst to the other slot -> 1 notice; +59 s -> none; +61 s -> one more; a delivered message then a closed agent queue -> one notice at once, the next one suppressed.
   - `the_backlog_of_messages_for_telegram_is_capped` (runs with `notice_every: ZERO`, uses `stalled_slots`).
7. `crates/cctg/tests/message_logs.rs` (new binary): real update JSON with a distinctive user id through `route_batch`; offline notice, forwarded inbound, reply; the three fixed log lines appear, the texts, the word `private` and the user id do not.
8. `crates/cctg/tests/overflow_logs.rs` (new binary, global subscriber, `.without_time()`): a gated transport holds every send; 261 messages to a dead slot (`notice_every: ZERO`) give exactly one overflow warning; after the gate opens and 256 sends are answered no new warning; the gate closes, 261 more messages give exactly one more (total 2).
9. `docs/poc.md` (new): the live recipe. Hub from the repo root with `.env`; `~/.cctg/device.env` with the secret; temp `mcp.json` and `settings.json` (hooks with the absolute quoted path); `claude --mcp-config ... --strict-mcp-config --settings ... --dangerously-load-development-channels server:cctg` with `CLAUDE_CONFIG_DIR=<tmp>/cctg-poc/claude-config` set in that terminal only. It states the cost of that (one login kept in that dir, no personal settings/memory/plugins, transcript under that dir; `/brief` still works because it takes the path from the hook) and what stays in the real config without it (`projects[<folder>]` key and transcript). Fallback for hooks: the same `hooks` block in `<config dir>/settings.json`. Six-point checklist and cleanup. No secrets, no machine paths.

## 3. Test plan

Build rules (host memory is tight): `CARGO_TARGET_DIR` under `%TEMP%` (for example `%TEMP%\cctg-t021-target`), `CARGO_PROFILE_DEV_DEBUG=0`, `-j 1`, one cargo at a time; delete the target dir afterwards.

1. `cargo fmt --all -- --check` — clean.
2. `cargo clippy -j 1 --workspace --all-targets -- -D warnings` — clean.
3. `cargo test -j 1 --workspace` — every binary `ok`. Expected: 20 test binaries `ok`; cctg lib 233 passed, 1 ignored (the planner run had 232: +1 burst test); the new `overflow_logs` binary 1 passed; all other binaries unchanged. Recorded run: `R/workspace_test.txt`.
4. Acceptance map:
   - 1 (inbound with meta, General/foreign reach nobody): `a_topic_message_reaches_only_the_agent_of_its_slot`, `only_an_explicit_reply_is_a_reply`.
   - 2 (reply split in order via the scheduler): `a_reply_goes_to_its_session_topic_in_split_order`.
   - 3 (one short notice, no errors; after `/clear` by pid): `a_message_nobody_can_take_gets_one_notice_each`, `a_burst_to_a_dead_slot_gets_one_notice_a_minute`, `an_ended_session_gets_no_inbound_even_with_its_agent_still_linked`, `after_clear_the_new_session_of_the_slot_gets_the_message`.
   - 4 (commands, `forum_topic_*`, strangers): `messages_go_to_the_slot_actor_and_commands_do_not`, existing `classify` allowlist tests.
   - 5 (no Telegram wait on ingress/actor): `a_stalled_telegram_never_stalls_inbound`, `the_backlog_of_messages_for_telegram_is_capped`, `overflow_logs`.
   - 6 (no text, user id, secrets in logs): `message_logs`.
   - 7 (existing tests pass): the workspace run.
5. Mutations (`R/mutate.py`, output `R/mutations.out.txt`), every one must be killed: M1 live filter removed; M2 topic-root reply kept; M3 cap removed; M4 commands forwarded as messages; MF1 notice cooldown removed; MF2 delivery does not re-arm the offline notice; MF3 one cooldown per slot for every notice kind; MF4 overflow warns every time; MF5 overflow warning never re-armed.
6. Live PoC (`docs/poc.md`): run by the orchestrator/user after merge, not in the pipeline (needs the real bot).

## 4. Rollout notes

- No migration: `registry.json` format unchanged (`SessionEntry.agent` stays `#[serde(skip)]`), no wire change (`HubMsg::Inbound`/`AgentMsg::Reply` already exist, `wire::VERSION` stays 1).
- No new env var, no new dependency, no feature flag. `notice_every` is an `Options` field (tests set it), not configuration.
- Behaviour change users will see: non-command text in a slot topic now reaches Claude; `/anything` still goes to the command worker (a Claude Code slash command typed in Telegram does not reach the session).
- Known limitations (accepted, not fixed here): no buffer for a dead session (TASK-017; the notice says "не доставлено"); within one minute further messages to the same dead slot are dropped with no extra notice; a reply in the first ~0.4 s of a brand-new slot (topic not created yet) is dropped with a warning; a reply may land above the session separator; replies over the 256 backlog are dropped whole while the `reply` tool already told Claude "sent"; a service message other than `forum_topic_*` from the user (for example pinning a message in a topic) has no text and gets the text-only notice (at most once a minute).
- `docs/poc.md` with `CLAUDE_CONFIG_DIR` needs one login in that dir (claude.ai `/login` or `ANTHROPIC_API_KEY`).

## 5. Review notes (plan-reviewer-2)

Disconfirmation tested first: "a burst of messages to a dead slot yields one notice per message". It held on the planner reference: the new test `a_burst_to_a_dead_slot_gets_one_notice_a_minute` failed with `left: 10, right: 1` (`R/repro_finding1.txt`). Fixed as above.

Changes against PLAN_V2 / the planner reference:

1. Finding 1 (OPEN_DECISIONS 3, notice rate): added `Options::notice_every` (60 s) and the `notices` map keyed by `(slot, notice)`. Also covers `TEXT_ONLY_NOTICE`: a Telegram album is one message per photo, so 10 photos gave 10 notices, the same budget problem the orchestrator named. Keyed per kind so a text-only notice never hides the offline one. A delivered message clears the offline entry, otherwise a session that dies again within the minute would lose the user's next message without any word. Two tests that checked paths, not rate, run with `notice_every: ZERO`. Log texts "user notified" became "not delivered" (a suppressed notice made them false). Mutations MF1-MF3 killed.
2. Finding 2 (overflow warning untested): new integration binary `overflow_logs.rs` (log capture needs its own binary, TASK-008 lesson). Mutations MF4, MF5 killed.
3. Finding 3 (`docs/poc.md` promise): the promise was false. `CLAUDE_CONFIG_DIR` isolation is now the recipe. Evidence: the docs (https://code.claude.com/docs/en/claude-directory) say every `~/.claude` path moves; a probe on 2.1.280 (`R/cfgprobe/probe.txt`) showed `.claude.json` is created inside `CLAUDE_CONFIG_DIR` and the user-scope servers of the real `~/.claude.json` are not visible. The doc states the login cost and what stays without isolation. The hooks fallback moved from the working folder's `.claude/settings.local.json` to `<config dir>/settings.json` (inside the isolated dir).
4. Routing attacked again, nothing else broken: registry `agent` ids are not persisted (no conn-id collision after a hub restart); a stale duplicate connection of a session cannot reply (`agent == Some(conn)` check) and its disconnect does not unbind the new one; a nested run never has `current_session` of a slot; a whitespace-only reply yields zero ops (the agent already rejects blank text); a `/clear` that lands in another slot moves the connection with `follow_pid`, and the old slot answers with the offline notice. Pinned-message service messages getting the text-only notice is listed as a limitation, not fixed (task scope, rate-limited now).
5. The patch and hashes were regenerated from `R/ws` against the current HEAD (the planner's `hashes.txt` no longer matches `slots.rs`, `docs/poc.md`, and misses `overflow_logs.rs`). Use only `R/`, not `scratch/planner/`.
