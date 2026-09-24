# TASK-016 IMPL_REVIEW (code-reviewer)

## 1. Verdict

**NEEDS_WORK**: the design and the code match PLAN_FINAL and every acceptance criterion has a test. One new buffer is unbounded, though: `Live.waiting` gets a new barrier on every idle read while a stream message waits for Telegram. The fix is about 5 lines, plus one test.

## 0. Disconfirmation (done first)

Counter-example I wrote down before reading the code: "a stream message Telegram refuses (not 429), possibly merged with other lines, still advances the persisted offset. Or the rewind after it duplicates the TASK-022 `Stop` answer, or lets tool lines land after it."

What I checked in the code:
- `scheduler.rs` `run_job`: `Merged` goes to the merged receivers only when `accepted`. Otherwise they are dropped, the dispatch gets `None`, and `on_stream_done` marks the message `Refused`. `Live::advance` never passes a `Refused` entry. `stuck()` → `rewind()` restores the registry offset and calls. **Held.** Tests: `a_refused_merged_message_answers_none_of_its_lines_as_merged`, `a_refused_message_stops_the_offset_until_the_stream_rewinds`, `a_refused_stream_message_comes_again_after_a_restart`.
- Stop duplication: a held answer leaves `Live.held` only through `pop_front` (in `on_chunk` Action::Release, in `pump_streams` on timeout or when there is no target, in the missing branch, or on MAX_HELD overflow), so it is sent at most once. `rewind` keeps `held`. **No duplicate answer.** A rewind re-reads lines that were already sent, and they can come after an answer that already went out. That is the at-least-once behaviour the plan declares (PLAN_FINAL §5, residual risks). A re-read `TurnEnd` can raise `ends_unclaimed`, and then the next `Stop` goes out without waiting. Also documented.

The counter-example did not hold. While attacking the offset/barrier machinery I found the issue below.

## 2. Confirmed correct

- Pure extraction `crates/transcript/src/stream.rs`. BOM and `trim` are handled (l.53). Sidechain records are skipped. `Channel` is emitted only for `<channel source="cctg" ... message_id="digits">` (l.129-138), and a foreign server never counts. A `queue-operation enqueue` does not count as "taken into work" (fixture l.1). I checked real `stop_reason` data on the 150 newest local transcripts: there is exactly one text record with a non-`tool_use` stop_reason per `message.id` (2614 of 2614), so there is one `TurnEnd` per answer. Thinking records never give a `TurnEnd`.
- Agent side `crates/cctg/src/tail.rs`. Only whole lines are taken (l.114-118). BOM/CRLF keep byte offsets. `reset` fires when `from > len` or byte `from-1 != \n` (l.89, 188-196). The path gate works on the canonical path: exactly `<canon root>/<one dir>/<session_id>.jsonl`, the id must be plain, and the target must be a regular file (l.165-184). The error answer is `missing`, with no path and no OS text. Frame bounds are checked before a line is taken (l.132-137), so a chunk stays under `MAX_LINE` (test `no_record_makes_a_chunk_longer_than_a_link_line`).
- `agent.rs`: one reader task with a request slot of capacity 1 and one `spawn_blocking` at a time. `TranscriptRead` never reaches `channel::Server` (`channel.rs` match arm), and the test proves Claude sees nothing. Nothing new is printed to stdout.
- Wire (`wire.rs`): `VERSION` is still 1. `Register.transcript_reads` is `#[serde(default)]`, and the hub sends `TranscriptRead` only to `Conn.reads` (`slots.rs` `stream_target`). `StreamItem::Other` via `#[serde(other)]`. An old agent with the new hub is never asked. A new agent with an old hub only adds an ignored field (`unknown_fields_are_ignored`).
- Scheduler: `Op::Stream` is metered and uses the Message FIFO, so a merged message takes one token. `next_permission` does not count stream lines, so a permission prompt overtakes them. Merging only happens when `message.len()+1 > tokens`, stops at the first non-mergeable message of the same topic, and stays ≤4096 by `telegram_len`. A 429 keeps `job.merged` on retry. `Op::React` supersede answers the replaced job with `Superseded`, so the `reactions` counter cannot leak.
- Slots actor: nothing is awaited and no file is read. Reads are `try_send`, sends are `hand_off`. A chunk is accepted only from the connection it was asked on, only when `from == read_at`, and only for the bound session. The offset is committed only across fully accepted barriers (`stream_answered`), and `dirty` is set only on change. The stream of a new session starts only after its separator was accepted (`pending_separator` guard), so there is exactly one separator.
- Reactions: 👀 only after a successful `try_send` to the agent. ✍ only from a matching receipt, and only once (`apply_line` removes it). Terminal prompts and `UserPromptSubmit` never touch reactions. A reaction error gives one warn and routing is not affected.
- Bounds: `held` ≤8, `receipts` ≤32, calls ≤64, unanswered stream messages per session ≤64 and 128 overall, reactions in flight ≤64, one read in flight per session, one blocking read per agent. The one exception is below.
- Logs: short session id and fixed text only. No path, text or message id (`stream_logs.rs`). Fixture `stream.jsonl` is anonymized.
- My own run (`CARGO_TARGET_DIR` under %TEMP%, `-j 1`, `CARGO_PROFILE_DEV_DEBUG=0`): `cargo clippy --workspace --all-targets -D warnings` clean, `cargo fmt --check` clean, `cargo test --workspace --no-fail-fast` all green (cctg lib 358 passed / 1 ignored, stream_logs 1, transcript stream 4). No new crates.

## 3. Issues

### M1 (major): `Live.waiting` grows without bound while a stream message waits
`crates/cctg/src/hub/stream.rs:239-245` (`Live::barrier`) together with `crates/cctg/src/hub/slots.rs` `pump_streams` (read condition around diff l.381-388) and `on_chunk` (`live.barrier(read_to)` after every chunk).

`pump_streams` keeps asking for a new read every `stream_every` (300 ms) while `unanswered() < 64`. Every chunk, including an empty one, pushes `Entry::Barrier { to, calls: self.calls.clone() }`. `advance()` pops only from the front and stops at a `Waiting` message. So while one message waits for Telegram, a barrier (with a clone of the open calls) piles up every 300 ms. That happens when the bucket is empty at 20/min shared across all topics, the queue is long, or a 429 `retry_after` pauses the whole queue for minutes. Growth is limited only by how long the wait lasts. That breaks "every new buffer bounded", and the snapshots can be large (up to 64 calls × up to 16 KiB line each).

Reproduced: `scratch/crev/barrier_growth.rs`, linked against the built `cctg` rlib. One `sent()` followed by 200 `barrier(100)` calls (60 s of idle polling) gives `barriers_in_waiting=200`, Debug size 223 → 6625 bytes, and `advance()` is still `None`.

Fix (smallest): in `Live::barrier`, if `waiting.back()` is already an `Entry::Barrier`, replace it instead of pushing a new one. Nothing lies between two consecutive barriers, so only the later one matters: `advance` would pass both together anyway and keep the last. This bounds barriers by the number of messages (≤64+1). Add a unit test: one `sent()`, N× `barrier()`, then accept → `advance()` returns the last `to`, and `waiting` has ≤2 entries. Also consider skipping the read entirely while the queue is paused. That is not required once barriers coalesce.

### m1 (minor): a one-line oversized record drops items silently, possibly its `Result` or `TurnEnd`
`crates/cctg/src/tail.rs:139-144`. When one line alone exceeds `MAX_CHUNK_TEXT` or `MAX_CHUNK_ITEMS`, `retain`/`truncate` drop the tail items. A lost `Result` leaves its call open until the next flush, so the line is never shown. A lost `TurnEnd` (always the last item) makes a held answer wait the full 5 s. No real session writes such a line (the plan says so), so this is only a robustness note. If you touch it, keep `TurnEnd` when truncating.

### m2 (minor, likely): each new session logs a "session transcript not found" WARN at startup
`crates/cctg/src/hub/registry.rs` (`offset: Some(0)` for `startup|clear`) together with `slots.rs` `on_chunk` missing branch. The stream starts polling as soon as the session has an agent and its separator is out. If Claude Code creates the jsonl only at the first record (I did not verify this; the stream_logs test models exactly "file appears later"), every normal session start logs one WARN that is not an error. Suggest `debug!` for the first missing episode of a stream that has never read a byte (`offset == Some(0)`, nothing committed yet). Keep the warn for a file that disappears later.

### m3 (minor): `pump_streams` clones the registry calls and the path on every pump for every session
`slots.rs` `pump_streams`: `.map(|stream| (stream.offset, stream.calls.clone()))` runs before `or_insert_with`, so it happens even when `Live` already exists. `stream_target` also clones `transcript_path` each pump. `pump` runs on every actor event. The cost is small (≤64 short lines), but it is avoidable: move the lookup into `or_insert_with`.

## 4. Missing coverage

- Barrier coalescing, or any bound on `Live.waiting` while a message waits (M1).
- Hub side: a `transcript_chunk` whose `session_id` is not the connection's bound session is dropped (`on_agent` guard `if session_id == session`). There is no direct test.
- Hub side: a chunk carrying `StreamItem::Other` from a newer agent does not disturb calls or order. Only the wire decode is tested.
- `on_turn_answer` for a streamed session before its `Live` exists (the first pump has not run yet): the answer goes out at once. That behaviour is fine but untested.
- A 4xx (non-topic-gone) skip on a stream message: it counts as accepted and the offset moves. Only the 502 path is tested.

## 5. Nits

- `transcript/src/stream.rs`: `[Request interrupted by user]` streams as a prompt `> [Request interrupted by user]` and counts as `NewTurn` (it is asserted in `tests/stream.rs`). That is intended, but it is a Claude Code service line rather than a user prompt, so `/brief`-style hiding may read better.
- The `Agent` call line gets `✓` as soon as the async launch result lands, not when the subagent finishes. The orchestrator decided to keep it, but the ✓ can be misread. The TASK-015 block shows the real state.
- `tail.rs` can hold one record up to `MAX_RECORD` = 64 MiB in memory, plus its lossy UTF-8 copy and its parse, per read. That is fine for real transcripts. The comment "never a line the stream shows" does not hold for 4-64 MiB lines, which are read whole.
