# TASK-048 implementer summary

Pre-flight: small-fix mode, no plan. Everything the orchestrator notes name exists with the assumed shape: `Slots::on_topic_message`/`park`/`flush`/`pump`/`next_deadline`/`on_tick`, `buffer::Parked::content`, `stream::receipt` + `registry::Stream.receipts`, `console::classify` + `on_console_command`, `hand_file`, `HubMsg::Inbound { content, meta }`. No mismatch found.

## 1. What was implemented

Code commit `fae99d2` on `fix/coalesce-inbound` (5 files, +575 / -17):

- `crates/cctg/src/hub/slots.rs` (+505/-17, ~190 of that is code and docs, the rest is tests)
  - `GATHER_QUIET` = 1 s, `GATHER_MAX` = 3 s, `MAX_GATHER_BYTES` = 128 KiB (a link line is 1 MiB and a JSON escape takes at most 6 bytes per byte).
  - `Options::gather_quiet` / `gather_max`. The default is `ZERO`, which turns gathering off. `hub/mod.rs` turns it on, the same way `status_every` works.
  - In-memory `Slots.gathers: HashMap<SlotId, Gather { first, due }>`. The messages themselves stay in the persisted `Slot.buffer`, so delivery is still at-least-once across a hub crash. `next_deadline` wakes the actor at `due`. `on_tick` → `pump` → `flush_all` sends the burst.
  - `on_topic_message`: a text calls `gather(slot)`, which starts a burst or extends it to `min(now + quiet, first + max)`. It only does this when a live agent is bound; a burst that is already due is not extended. A file calls `end_gather` (due = now) before it is parked. A console command calls `end_gather`, then `flush`, then the command.
  - `flush`: a front text waits while the slot's gather is not due. Once it is due, the leading text run up to the first file goes as one `Inbound` (`burst()`, capped at 128 KiB, the first text always goes). With no gather it sends one message at a time as before. The gather is dropped once no text of it is left at the front, and also when the slot has no live agent: messages kept while the slot is dead or offline go one by one on revival, so TASK-017 behaviour is unchanged. Each part gets 👀. One log line per burst (`parts = N`), with no text in it.
  - `burst_meta`:
    - `message_id` is the last part's id.
    - `message_ids` lists all parts in order, comma-separated, and is only present when there is more than one.
    - `reply_to_message_id` and `target_agent` come from the last part that explicitly replies.
    - `forwarded` is set only when every part is a forward.
    - A single part gets exactly its old meta.
    - Why the last id: the transcript shows only the channel tag's `message_id`, so that is the receipt key; the last part is normally the user's own note. `message_ids` tells the model that one tag stands for several Telegram messages.
  - `on_chunk` `Step::Working(id)`: also reacts ✍ on the burst's other parts (`stream::take_parts`).
  - Module doc: new "Bursts (TASK-048)" paragraph.
- `crates/cctg/src/hub/buffer.rs` (+30): `PART_SEPARATOR = "\n\n---\n\n"` and `burst_content(parts)`, which joins each part's unchanged `Parked::content()` (quotes and `(переслано)` marks as in TASK-030). Includes a unit test.
- `crates/cctg/src/hub/stream.rs` (+51): `receipt_parts(stream, ids)` makes the last id the receipt and remembers the others; `take_parts(stream, key)`. `receipt` prunes parts whose key was evicted. Includes a unit test.
- `crates/cctg/src/hub/registry.rs` (+4): `Stream.parts: Vec<(i64, Vec<i64>)>` with serde default, skipped when empty, so older `registry.json` files still load.
- `crates/cctg/src/hub/mod.rs` (+2): the hub sets `gather_quiet: GATHER_QUIET, gather_max: GATHER_MAX`.

New tests in `slots.rs`. The first five use paused tokio time and a fake link queue:
- `three_forwards_and_a_note_within_300_ms_reach_the_session_as_one_inbound`: one Inbound, exact content and meta, nothing at 999 ms after the note, 👀 on 1..4, receipts `[4]` with parts `[(4,[1,2,3])]`, sent once.
- `a_lone_message_goes_after_the_quiet_window_and_a_steady_stream_by_its_limit`: a lone message goes exactly at the quiet window with its old meta. A message every 500 ms is flushed at 3 s (`10..15`), and the rest go a quiet window after the last one.
- `a_file_in_a_burst_lets_the_burst_go_first_and_keeps_its_place`: order is text burst, file, then the later text after its own window. 👀 on all four.
- `a_command_in_a_burst_lets_the_burst_go_first`: order is Inbound(1,2), then ConsoleCommand, then the later text after its window.
- `messages_kept_while_the_slot_had_no_live_session_go_one_by_one`: a burst started, then the session ended before it was due, then more messages came while the slot was dead. On revival all four go as separate inbounds.
- `every_message_of_a_burst_turns_writing_with_its_channel_record`: real rig and transcript. The channel record of the last id gives ✍ on all three.

Task artifacts: `log.jsonl` (4 `decision` entries), `PCTX_PROPOSALS.md` (a new hub invariant line), `scratch/gather_tests.rs.txt` (the test block as inserted), `scratch/append_log.py`, `scratch/test_full.log`.

## 2. Deviations / not implemented

- ✍ for all parts needed a small persisted addition (`Stream.parts`). The task text only asked for 👀; without it, every part except the last would keep 👀 forever.
- The unsupported-message and too-big notices do not touch a burst. The task does not name them.
- Under link backpressure, a text that arrives after a file that ended the burst goes on its own after the file instead of starting a new burst. This is graceful degradation and is documented on `Gather.due`.

## 3. Test results

- `cargo fmt --all -- --check`: OK.
- `cargo clippy -j 1 --workspace --all-targets -- -D warnings`: OK.
- `cargo test -j 1 --workspace` (log in `scratch/test_full.log`): every suite passes except `tests/update_e2e.rs` (3 tests). That suite failed on a build-id mismatch: the binary in the shared target reported `d4d3445…-dirty`, but this tree is `20b2874…`. Another agent at a different commit was building into the shared target at the same time (cargo showed "Blocking waiting for file lock", and `maw/tasks/in_progress/TASK-044/*` changed in the working tree during the run; I did not touch or commit those files). Re-running `cargo test -j 1 -p cctg --test update_e2e` right after rebuilt the binary: 3 passed. The lib suite ran 637 passed, 1 ignored, including the 6 new slot tests and 2 new unit tests.

## 4. How to verify manually

1. Deploy the hub from this branch. Open a live session topic and forward three messages from another chat into it, then type a note within a second.
2. The session gets one `<channel ... message_id="<note>" message_ids="a,b,c,note">` whose body has three `(переслано)` parts and the note, separated by `---`.
3. All four Telegram messages get 👀, then ✍ once Claude takes the channel message in.
4. A single message arrives about 1 s later than before. Nonstop typing is flushed every 3 s.
5. Send `!ls` or a photo right after a text: the text arrives first, then the command or file.
6. With the session ended, messages still wait and a Resume button appears as before. After a resume they arrive one by one.
