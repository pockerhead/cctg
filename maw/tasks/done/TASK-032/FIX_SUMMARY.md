# TASK-032 FIX_SUMMARY

Code commit: `a30815a fix(files): keep a file of an ended session in its slot, bound re-sends, smaller chunks` (on `feature/files`, on top of `80c6ab6`).

## Preflight: the claim that would break correct code if applied verbatim

Minor-3 says "`CHUNK` 64 KiB". `files.rs` also uses `CHUNK` as the receiver's limit (`Assembly::push`: `encoded_len(CHUNK)`). With one constant cut to 64 KiB, a receiver refuses every 256 KiB piece from a peer built from `96f1e4b` as `Broken::Size`, and agents outlive hub upgrades. I split it: senders cut `CHUNK = 64 KiB`, receivers accept up to `MAX_CHUNK = 256 KiB`. Mutation M8 checks this. The other claims held up once I checked the code (major-1 via the reviewer's repro, which failed on `96f1e4b`).

## 1. Fixed

- **major-1: file downloaded while its session ends** (`hub/slots.rs`). `fetching` now holds a `Fetching { transfer_id, message_id, conn }`. `on_fetched` counts `Handed` only when `live_agent(slot)` has the same conn. Otherwise the message stays in the slot with no 👀, just like `LinkClosed`. Both "stays" cases now call `flush`, so an agent that registered during the download (TASK-040 hand-over, resume) gets the file right away instead of waiting for the next event. That was a latent stall in the old `LinkClosed` path too. `/clear` keeps the conn (rebind by pid), so the file counts there. Test: the reviewer's repro, extended with the resume: `a_file_downloading_when_its_session_ends_stays_in_the_slot`. Both messages go to the next session in order, and the 👀 lands only then.
- **minor-2: overflow during a download** (`hub/buffer.rs`, `hub/slots.rs park`). `Buffer::push(message, keep_front)` drops the next-oldest message when the front is the file in transit. The notice ("самые старые отбрасываются") stays true. Tests: `a_full_buffer_keeps_its_front_when_asked_and_drops_the_next_oldest` (buffer) and `an_overflow_while_a_file_downloads_drops_the_next_oldest_and_keeps_the_order` (slots: file first, texts 3..51 after, 👀 in the same order, one overflow notice).
- **minor-3: chunk size and endless re-send** (`files.rs`, `hub/slots.rs`, `hub/buffer.rs`). `CHUNK` is now 64 KiB (a line of about 90 KB) and `MAX_CHUNK` is 256 KiB on receive. `MAX_LINK_LOSSES = 3`: when a closing link cuts the same front file 3 times in a row, the file is dropped with the new `LINK_LOST_NOTICE` and the messages behind it go on. Any other outcome resets the counter. Tests: `a_file_whose_hand_over_is_cut_again_and_again_is_dropped_with_a_notice`, plus the extended `an_assembly_takes_only_the_next_piece_and_never_more_than_its_size` (a peer's 256 KiB piece is accepted, and its line is below `MAX_LINE/2`).
- **minor-4: sendPhoto fallback** (`hub/api.rs`, `hub/scheduler.rs`). New `ApiError::is_photo_refusal()`: a 400 whose description contains `PHOTO` or `IMAGE`. Only that triggers `sendDocument`. Tests: `only_a_refusal_of_the_picture_itself_is_a_photo_refusal` (unit) and `only_a_refused_picture_goes_again_as_a_document`: the real `Transport for BotApi` against a fake HTTP server. `PHOTO_INVALID_DIMENSIONS` leads to sendDocument. "message thread not found" returns that error, with no second call.
- **minor-5: file save blocked the agent loop** (`agent.rs`). A complete file is saved in a `tokio::spawn(deliver(..))`. Its result comes back through a `select!` branch. Hub link events are not read while a save runs, so later inbound messages stay behind the file. stdin frames, tool answers and deadlines keep running. Test: `a_file_saved_off_the_loop_still_reaches_claude_before_the_messages_after_it` (file, empty file, text arrive in that order).
- **Missing coverage: `BotApi::download`** (`hub/api.rs`). `a_download_stops_at_its_limit_by_length_or_by_bytes_and_a_refusal_is_told` uses a raw fake HTTP server. It covers: within the limit gives the bytes, `Content-Length` over the limit is refused before the body, a stream with no length over the limit, and a 404 gives `Telegram{404}` without the token.

Self-check `scratch/fixer/mutations.py`: each of 8 mutations undoes one fix. All are KILLED (`mutations.out.txt`, `mutations.rerun.out.txt`). M4 and M5 survived the first run because the tests were weak. I made the tests stricter (a 4th answer for a fallback that should not happen, and a body shorter than the limit behind `Content-Length: 1000`) and reran them. Log: 3 `decision` entries (chunk split, how the save is held, the major-1 approach).

## 2. Skipped

- **Hub restart during a download** and **simultaneous two-way transfer** tests: skipped as the task allows. Neither is cheap without a new rig.
- **Named pipe in `read_upload`**: not checked (the reviewer's probe was blocked too). Out of scope.
- **Nits** (`reserved` has no effect on inbox names, `metadata` vs `file.metadata()`): not in the task scope, left as is. The `receipt` nit went away with major-1: on `Handed` the live session is now the session the file went to.
- **Known cost of minor-5**: a permission verdict from the hub can wait behind one file save (it is read after the save). The terminal dialog is open in parallel. A per-kind hold queue was rejected as extra state (see the log).
- **Known cost of major-1**: the old link already got the bytes, so a dying agent may still save the file and send a channel message into the closing Claude. The next session gets the file again (at-least-once, like TASK-017 texts).

## 3. Test results

All with `CARGO_TARGET_DIR=C:/Users/user/dev/cctg/target CARGO_PROFILE_DEV_DEBUG=0`, `-j 1`, one at a time.
- `cargo fmt --all --check`: clean.
- `cargo clippy -j 1 --workspace --all-targets -- -D warnings`: clean.
- `cargo test -j 1 -p cctg --lib`: 549 passed, 0 failed, 1 ignored (was 541, +8 new).
- `cargo test -j 1 --workspace --no-fail-fast` (`scratch/fixer/workspace_test.txt`): rc=0, **711 passed, 0 failed, 3 ignored** (reference 703 + 8 new).
