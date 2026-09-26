# TASK-059 fix summary

## Preflight: the claim that would break code if taken verbatim

Review item 2 says to fix it by reading the current `hub_albums` "when the transfer starts". If the flag is read before `transfer()` drains stale `Upload` events, a race is left. The sequence is: read `true`, then `Down` (flag set to false, `Lost` sent), then the drain throws the `Lost` away, then the offer goes into the outbox, which survives reconnects, then the link comes `Up` to an old hub. The album offer with `parts` then reaches a hub that glues the parts into one document. I checked `agent.rs`: `transfer` starts with `while uploads.try_recv().is_ok() {}`, and `run()` keeps one outbox across reconnects. So the flag is read after the drain. A link lost after the read leaves its `Lost` in the queue, and `hear` ends the transfer before any chunk goes out.

## Fixed

1. **MAX_QUEUED_MESSAGES and a mixed album** (`hub/slots.rs`). The reviewer's point is real: the gate checked only `>= MAX`, and `send_album` added `ops.len()` (up to 2).
   - At the offer, the new `offer_messages(parts)` reserves 1 message for a single file and 2 for an album. Kinds are known only from the bytes, so the album count is conservative.
   - At completion, `send_album` builds its ops and then checks `queued_messages + ops.len() > MAX_QUEUED_MESSAGES`. If they do not fit, it frees `file_bytes`, answers `Busy` and hands nothing off.
   - New test `an_album_is_accepted_only_with_room_for_its_messages`: an offer with 1 free message gets Busy; a mixed album with 1 free message at completion gets Busy with nothing sent; a one-kind album fits the last free message.
   - Checked by mutation: with the old gates the test fails.
2. **Album vs one-by-one decided at transfer time** (`agent.rs`).
   - `hub_albums` is now a `watch::channel`. The loop sets it on `Up` (`files && albums`) and sets it to false on `Down`, before it sends `Upload::Lost`.
   - The sender's queue carries only `FileCall`.
   - `transfer()` reads the watch after the drain. A group of more than one file on a hub without albums returns `Transfer::NoAlbum`, and `send_group` then sends each file alone: the caption goes on the first, each other file gets its own name.
   - The "(one by one ...)" note now appears only when a group really went one by one.
   - New test `a_queued_paths_call_follows_the_hub_on_line_when_it_goes`: call 1 waits for an album hub's answer while call 2 (`paths`) is queued, and `tools/list` confirms the order. The hub drops and an old hub (`albums: false`) comes. Call 1 gets LOST, and call 2 goes as two single offers without `parts`, with "one by one" in the answer.
   - Checked by mutation: with the flag check disabled, the test fails.
3. **Item 4, any 400 on a photo album falls back to documents once** (`hub/scheduler.rs`).
   - The `SendAlbum` arm now matches `Err(ApiError::Telegram { code: 400, .. }) if *photos`.
   - `is_photo_refusal` stays as it was for single `SendPhoto`.
   - `api.rs` `a_refused_photo_album_goes_again_as_documents` is extended:
     - an IMAGE refusal goes again as documents;
     - a 400 whose text names no picture goes again as documents;
     - a 500 is not retried;
     - a document album refusal is not retried.
   - Six requests in all.
4. **Missing coverage from the review, the cheap parts:**
   - `paths_past_the_byte_cap_go_as_several_offers` covers the byte cap. The files are `MAX_UPLOAD - 10` (set_len), 10 and 1 bytes. The first offer is an album of exactly `MAX_UPLOAD` with the caption, and the second is a single offer named by its own file. Both offers are refused with Busy, so no bytes are sent.
   - `wire.rs`: a `registered` line with an unknown `"later":[1]` field still decodes.

## Skipped

- **Item 3 (album = one token):** the review itself says to change nothing and calls it a risk note. The spec says one sendMediaGroup is one token, and a 429 pauses the queue.
- **Item 5 (peak memory, about 3x the album size):** the review calls this acceptable. Streaming chunks from the parts would be a refactor outside what the task asks.
- **Nits** (the no-argument error text, double names after a fallback, `delivery: None` wording, comparing results as strings) and the remaining coverage items (Pending or Lost in the middle of an album, a failure after fallback at the slots level): not in the fix list for this run.

## Test results

All with `CARGO_TARGET_DIR=C:/Users/user/dev/cctg/target CARGO_PROFILE_DEV_DEBUG=0 -j 1`, after touching lib.rs/main.rs.
- `cargo fmt --all -- --check`: clean.
- `cargo clippy -j 1 --workspace --all-targets -- -D warnings`: clean.
- `cargo test -j 1 --workspace --no-fail-fast`: exit 0. Log: `scratch/fixer/test-full.log`. cctg lib: 759 passed, 1 ignored. Every integration binary and the transcript tests pass. No flakes this run.
