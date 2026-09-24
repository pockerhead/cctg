# TASK-016 QA_REPORT

Verdict: **NO_SHIP**

Streaming does not work at all in the real hub. `serve_agents` (`crates/cctg/src/hub/ingress.rs`) forwards only `Reply`, `PermissionRequest` and `PermissionAck` after registration. Every `TranscriptChunk` from a real agent lands in the catch-all arm and is dropped as "agent repeated its handshake; ignored" (WARN). No test caught this: every stream test in slots.rs and `tests/stream_logs.rs` pushes `AgentEvent::Message` straight into `Slots` and skips ingress. With a one-line local ingress patch (reverted) the rest of the feature mostly works end to end. I also found two more bugs.

## 0. Disconfirmation

Before testing I picked this counter-example: "a resumed session (offset `None`) whose transcript does not end in `\n` at the first read gets its whole history streamed again". `read_chunk` with `from: None` starts at `len` even when `len` is inside a line (tail.rs l.86-88). The next read from that offset fails `at_line_start`, so the agent sends `reset`, and the hub goes back to byte 0.
**It held**: `e2e_resume_at_a_torn_end_does_not_replay_history` streamed `> history 0..4`, `> being written` and `> new after resume` (log `scratch/qa/e2e.probe_ingress_patched.out.txt`). That is bug M1 below. The bigger blocker B1 turned up while I was building the end-to-end environment.

## 1. Environment

- No docker-compose or dev server. I ran cargo directly with one `CARGO_TARGET_DIR=%TEMP%/cctg-qa016-target`, `CARGO_PROFILE_DEV_DEBUG=0`, `-j 1` and one cargo at a time. The directory was deleted at the end.
- End-to-end harness: `scratch/qa/e2e/`, a separate cargo package (its own `[workspace]`, path deps on `crates/cctg` and `crates/transcript`, `--offline` with the repo `Cargo.lock`). In each test:
  - the real `cctg agent` binary runs as a child process, with env `CCTG_HUB_SECRET`, `CCTG_HUB_AGENT_ADDR`, `CCTG_HOST`, `CLAUDE_CODE_SESSION_ID`, `CLAUDE_CONFIG_DIR=<temp>/cfg`, and `USERPROFILE`/`HOME` set to a temp dir, so the real `~/.cctg/device.env` is never read and `CLAUDE_CODE_ENTRYPOINT` is removed;
  - the real `serve_agents` runs on a TCP port on 127.0.0.1, with the real `Slots` actor, the real `Scheduler` and a fake `Transport` (it records every op and can refuse stream sends with 502);
  - hooks (SessionStart, Stop, UserPromptSubmit) go in as `HookPost` on the hooks channel;
  - the transcript is `<temp>/cfg/projects/C--qa-w/<session>.jsonl`, written in pieces.
- Reproduce: `cargo build -p cctg --bin cctg`, then in `scratch/qa/e2e`: `CCTG_BIN=<target>/debug/cctg.exe cargo test --offline -- --test-threads=1` (add `QA_TRACE=1 ... --nocapture` for hub logs).
- No Telegram, no `.env`, no `~/.claude.json` changes, no interactive claude, no windows.

## 2. Test results

| Run | Result | Evidence |
|---|---|---|
| `cargo fmt --all -- --check` | rc=0 | `scratch/qa/fmt.out.txt` |
| `cargo clippy -j 1 --workspace --all-targets -- -D warnings` | rc=0 | `scratch/qa/clippy.out.txt` |
| `cargo test -j 1 --workspace --no-fail-fast` | all ok; cctg lib 361 passed / 1 ignored, stream_logs 1, transcript stream 4 | `scratch/qa/workspace_test.txt` |
| QA e2e at HEAD (6 tests) | **0 of 6 pass**: after `agent registered`, every chunk is logged `WARN agent repeated its handshake; ignored`, and the hub re-asks every 10 s | `scratch/qa/e2e.head.out.txt` |
| QA e2e with a local ingress patch (TranscriptChunk added to the forwarded arm, reverted afterwards) | 4 pass; `refused` fails (M2); `resume` fails (M1) | `scratch/qa/e2e.probe_ingress_patched.out.txt` |
| Non-vacuity: with the ingress patch, `tail.rs` mutated to take a partial last line as whole | `e2e_order` FAILS (`> go` repeated 25 times: offset inside a line, reset loop) | `scratch/qa/e2e.mutation_partial_line.out.txt` |
| reviewer2 mutation script run against the repo (`scratch/qa/mutations_repo.py`; 7 multi-line patterns rerun CRLF-aware by `mutations_repo_crlf.py`) | 35/35 KILLED, tree clean afterwards | `scratch/qa/mutations_repo.log.txt`, `mutations_repo_crlf.log.txt` |
| Real channel records through `transcript::stream_events` | 3 of 3 real cctg records give one `Channel` event | `scratch/qa/real_channel_records.out.txt` |

The mutation score says the unit tests pin the pure logic well. It says nothing about ingress, and that is where B1 is.

## 3. Acceptance criteria

| # | Criterion | Test | Result |
|---|---|---|---|
| 1 | Appended lines reach the right slot topic in order | `e2e_order_partial_line_and_stop_after_lines` | **FAIL at HEAD** (B1). PASS with the ingress patch: `> go`, `A ✓`, `B ✓` (B waits for A), `C ✗ boom failed`, then the answer |
| 2 | A partial last line is never sent and never lost | same test: half a result line, 1.5 s wait, then the rest | **FAIL at HEAD** (B1; nothing is sent at all). PASS with the patch. The mutation kills it |
| 3 | A hub restart neither re-sends nor loses | `e2e_hub_restart_neither_repeats_nor_loses`: offset committed, hub aborted, 3 lines appended, new hub on the same port and state dir | **FAIL at HEAD** (B1). PASS with the patch: hub 2 sent only `two ✓` and `> while down` |
| 3b | A refused send does not lose lines | `e2e_refused_sends_lose_nothing` (first 3 stream sends get 502) | **FAIL at HEAD** (B1). With the patch no line is lost, but the visible order breaks (M2) |
| 4 | Scheduler: 20/min, FIFO in the topic, yields to permissions | `e2e_merge_under_the_limit` (capacity 3, refill 700 ms, 15 calls); unit tests + M6/M7/M8/R3 killed | **FAIL at HEAD** (B1). PASS with the patch: 16 lines in 2 messages, exact order |
| 5 | Session rotation: new stream, one separator | unit tests only (`a_new_session_in_the_slot_streams_after_its_one_separator`, `/clear` variant; M5 killed) | not verified end to end; at HEAD there is no stream to rotate |
| 6 | Missing/deleted file: one warning, polling goes on | `tests/stream_logs.rs` (skips ingress) | **FAIL at HEAD** in practice: the hub never sees `missing` and logs one WARN "agent repeated its handshake" per read, every 10 s, for as long as the session lives |
| 7 | 👀 on hand-off, ✍ only for that message's own cctg channel record | `e2e_reactions`: 👀 at once; UserPromptSubmit, a terminal prompt, a `webhook` record with the same id and a cctg record with another id all leave it alone; its own record gives ✍ once, a repeat gives nothing | 👀 **PASS at HEAD** (it does not use chunks); ✍ **FAIL at HEAD** (B1), PASS with the patch |
| 8 | Every finished call is its own message in call order; merged without loss at the limit | e2e order + merge tests | **FAIL at HEAD** (B1); PASS with the patch |
| 9 | Lag measured, source choice justified | planner's `scratch/planner/lag_probe.*`, `passive_lag.*`, `hook_cost.out.txt`, OPEN_DECISIONS | PASS (documents only; I did not re-measure, no live claude allowed) |
| 10 | Existing tests pass | full workspace run | PASS |

Open question about the shape of a channel message that arrives during a turn:
- In all of the user's transcripts, a real cctg delivery appears in two record shapes: `queue-operation` with `.content` starting `<channel source="cctg" ...>` (3), and `user` with `isMeta: true`, `origin.kind: "channel"` and a string `message.content` starting with the tag (3, attributes `chat_id, message_id, source, thread_id`). The code correctly ignores the first shape (enqueue is not "taken into work"). All 3 of the second shape give `Channel` (`real_cctg_channel_records_give_a_channel_event`).
- No `queued_command` attachment with a channel prompt exists yet. The closest analogue, peer messages that arrive mid-turn, shows up as `attachment.type == "queued_command"` with `isMeta: true` and `origin.kind: "peer"` (58), and also as `user` isMeta records (309). The code handles both forms for channel (tag check on `attachment.prompt`).
- Not verified: whether a channel message sent mid-turn really takes one of these two shapes. One `queued_command` has a list `prompt` (image paste), which the code ignores; such a message would keep 👀.
- Scripts and outputs: `scratch/qa/channel_shapes*.py` / `*.out.txt`. They record structure only, never text.

## 4. Bugs found

### B1 (blocker): ingress drops every `transcript_chunk`, so nothing is streamed and the answer waits 5 s
- Where: `crates/cctg/src/hub/ingress.rs` l.212-221. The forwarded arm lists only `Reply | PermissionRequest | PermissionAck`. `TranscriptChunk` falls into `Some((_, Ok(_))) => warn!("agent repeated its handshake; ignored")`. The diff of ingress.rs in this task only adds `transcript_reads: false` to a test literal.
- Repro: `scratch/qa/e2e` at HEAD, any test with `QA_TRACE=1 --nocapture`. Output: `agent bound to its session`, then `WARN agent repeated its handshake; ignored`, and after 10 s `transcript read not answered; asking again`, forever.
- Expected: chunks reach `Slots::on_chunk`. Actual:
  - no tool lines, prompts or ✍ in any topic;
  - one WARN every 10 s per streamed session;
  - since `stream_target` is `Some` (the agent announced `transcript_reads`), every `Stop` answer is held until `hold_answer` (5 s) because no `TurnEnd` ever arrives. That is a regression of TASK-022 answers: +5 s on every turn.
- Fix: add `| AgentMsg::TranscriptChunk { .. }` to the forwarded arm. With exactly that change, 4 of my 6 e2e tests pass. Add a test that goes through `serve_agents` (for example my harness, or an ingress test that sends a `transcript_chunk` frame and expects `AgentEvent::Message`).

### M1 (major): a resume whose transcript ends mid-line replays the whole history into the topic
- Where: `crates/cctg/src/tail.rs` l.86-88 (`None => len`), together with the reset path (`at_line_start`), plus `hub/slots.rs` `on_chunk` (`stream.offset = Some(from)`, then reset → `read_at = 0`).
- Repro: `e2e_resume_at_a_torn_end_does_not_replay_history` (needs B1 fixed): 5 history prompts plus half a line, SessionStart `resume`, first read, then the rest of the line and one new prompt.
- Expected: only new lines. Actual: `> history 0` … `> history 4`, `> being written`, `> new after resume`.
- When it happens:
  - (a) Claude Code is writing a record at the moment of the first read after resume (small window);
  - (b) deterministically: a transcript whose last line was torn by a crash or kill. Every resume of that session then replays everything, and at 20 msgs/min shared by the whole group a long session floods it for hours and delays other topics.
- Fix idea: for `from: None`, start after the last `\n` at or before `len` (or treat a missing trailing newline as "wait"), never at a mid-line `len`.

### M2 (minor): after a refused stream message, later lines show up before it, then everything repeats
- Where: `hub/slots.rs` `on_stream_done` / `hub/stream.rs` `rewind`. Messages queued after the refused one are still sent. The rewind then re-sends from the last committed barrier.
- Repro: `e2e_refused_sends_lose_nothing` (with B1 fixed; the first 3 stream sends get 502).
- Topic order: `refused 2 ✓`, `refused 3 ✓`, `> refuse run`, `refused 0 ✓`, `refused 1 ✓`, `refused 2 ✓`, `refused 3 ✓`.
- Nothing is lost. But PLAN_FINAL §4/§5 says "потерь и нарушения порядка нет", and the user does see lines out of order. Either document this honestly or stop sending a session's later stream messages once one of them is refused.

### Observation (from reading the code, not tested)
`Step::Working` and `Step::NewTurn` set `ends_unclaimed = 0`. The unverified case: a turn-1 `TurnEnd` is read before its `Stop`, and then a queued Telegram channel record (or a typed prompt) is read before that `Stop` arrives. The claim is lost, and answer 1 waits for turn 2's `TurnEnd` (after turn 2's lines) or for the 5 s timeout. I have not reproduced it.

## 5. Verdict

**NO_SHIP.** B1 disables the whole feature in the real process and adds a 5 s delay to every turn answer. M1 can flood a topic with a full history replay. The rest of the design holds up under test once B1 is patched: order, partial lines, restart without repeats, merging under the limit, reactions, and 35/35 mutations killed. The fix for B1 is one line plus an ingress-level test, and M1 is a small change in `tail.rs`. After those, rerun `scratch/qa/e2e`: all 6 tests should pass except `refused` (M2), until M2 is decided.

## Cleanup

- Services: none were long-lived. Each e2e test killed its own `cctg agent` child (Drop) and aborted its hub tasks.
- Deleted: `%TEMP%/cctg-qa016-target`, `%TEMP%/qa016-logs`, `%TEMP%/qa016-*` session dirs, `%TEMP%/ingress.rs.orig`, `%TEMP%/tail.rs.orig`, `%TEMP%/mut.py`.
- Repo sources are unchanged (`git status`: only `log.jsonl` and `scratch/qa/`). The temporary ingress patch and the tail mutation were reverted.
