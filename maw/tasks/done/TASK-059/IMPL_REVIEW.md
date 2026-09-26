# TASK-059 code review (commit 1ff5a9a, branch feature/send-album)

## 1. Verdict

**SHIP.** No defect that loses or corrupts a file or breaks old behaviour. The findings below are minor and can go to a follow-up or be fixed now.

## Disconfirmation (done first)

Counter-example tried: an album whose announced `parts` sizes do not match the bytes actually sent. A file can change size between stat and read, or be read short. The hub would then slice `bytes[offset..end]` at the wrong place and either panic or send one file's tail as another file.

Result: it does not hold.
- The agent reads each file once into memory (`files::read_upload`, `take(MAX_UPLOAD + 1)`, empty rejected). `FilePart.size` and the offer `size` are both computed from those in-memory bytes (`agent.rs` `transfer`: `file.bytes.len()` and `bytes.len()` of the concatenation). A size change on disk after the read has no effect.
- On the hub, `album_fits` (`slots.rs:481`) requires 2..=10 parts, each 1..=MAX_UPLOAD bytes, summing exactly to `size`, before `Accepted`. `files::Assembly::new(size)` completes only at exactly `size` bytes. Slicing in `send_album` therefore stays in bounds. A malformed offer is refused before any byte (slots test cases 3-6: 1 part, 11 parts, sum mismatch, zero part).

## Merge with main / scheduler fit

- The branch is based on the tip of `main` (`c4d4cea`, after TASK-054/057). `git merge-tree --write-tree main HEAD` is clean.
- `Op::SendAlbum` is in every scheduler match that matters: `Lane::Message(0)`, `metered()` (one token), `topic()`, and `next_permission`, where it counts as an ordinary non-permission message, the same as `SendDocument`. The TASK-054 debounce, merge and edit-class code touches only `Op::Stream` and edits, so an album takes no part in them. A permission prompt does not overtake an album of its own topic. The photo album and the document album are handed off in order to the same topic FIFO, so pictures go first.

## Verification run

(`CARGO_TARGET_DIR=C:/Users/user/dev/cctg/target`, `CARGO_PROFILE_DEV_DEBUG=0`, `-j 1`, after touching lib.rs/main.rs)
- `cargo fmt --all -- --check`: clean. `cargo clippy -j 1 --workspace --all-targets -- -D warnings`: clean.
- `cargo test -j 1 -p cctg --lib`: 756 passed, 1 ignored. `--test files_e2e`: 1 passed.
- `cargo test -j 1 --workspace --no-fail-fast` (grep in `scratch/crev-test.log`): everything green except `reap_e2e a_start_after_a_killed_session_takes_its_topic` (panic at reap_e2e.rs:178). Rerun alone: passed. This is a pid/process timing test that the diff does not touch. It is a flake, not a regression.
- No new crates. Nothing new writes to stdout in the MCP server. Logs carry sizes and counts only (checked every new `info!`/`warn!`; `files_e2e` asserts that names do not appear in the logs).

## 2. Confirmed correct

- **Bot API contract** (`hub/api.rs` `send_media_group`): each file is a multipart field `file<i>`, and `media[i].media = "attach://file<i>"`. `type` is `photo` or `document` for the whole group. `caption` is set per item (the first one when a caption is given). `message_thread_id` and `disable_notification` are form fields. The answer is decoded as `Vec<Message>`, which matches "an Array of Message objects". `files_e2e` checks the exact media JSON and the multipart filenames.
- **Caption placement** (`slots.rs` `send_album`): the caption is cut to 1024 and goes on the first item of the first message only (the photo album when there are pictures). Without a caption each photo carries its own cleaned name (TASK-051), and documents carry none.
- **Photo refusal fallback** (`scheduler.rs` `Transport for BotApi`, `SendAlbum` arm): only when `photos` and `is_photo_refusal()` is true. The same items are sent once more as a document album inside the same job, so one token. If the documents fail as well, that error is the job's delivery, `on_album_done` marks those parts `Failed`, and nothing is retried a third time. `api.rs` `a_refused_photo_album_goes_again_as_documents` covers refusal→documents, a non-photo error (no fallback) and a document album refusal (no fallback).
- **One transfer per album**: this fits the hub's one upload per connection (`uploads: HashMap<conn, Upload>`). The agent caps an album at `files::MAX_UPLOAD` bytes, which equals `MAX_FILE_BYTES` (`slots.rs:273`), so an album is never Busy on size alone. The byte budget `file_bytes` is released per message by that message's own size (the photo part plus the document part add up to the whole).
- **Per-file outcome**: `albums: HashMap<(conn, transfer_id), Album>` collects per-part results. The final `FileAnswer` carries `parts` in offer order, and `outcome=Sent` when any file went. If the connection disappears, `answer` goes nowhere and the map entry is still removed on the last `Done`, so nothing leaks.
- **Unreadable files** (`agent.rs` `upload_several`): each path goes through `read_file` → `files::read_upload`. That gives the device-path ban, the not-a-file and empty checks, and the 50 MB limit, applied to every path. A failure is recorded as `not sent: <reason>` and the other files still go (`files_e2e` id 17, agent test with `missing.txt`).
- **Mixed versions**:
  - Old hub, new agent: `Registered` has no `albums`, so `hub_albums = files && albums = false`. The agent sends one offer per file with no `parts`, and those lines are byte-identical to the single-file lines (wire test). The answer says "one by one".
  - New hub, old agent: `albums` in `Registered` is an extra field. The wire enums have no `deny_unknown_fields` (grep), so an old agent ignores it and never sends `parts`.
  - `VERSION` stays 1 and no new message type is added, so no `ingress.rs` arm is needed.
- **Tool schema**: `path` and `paths` (array, minItems 2, maxItems 10), with no `required` and no top-level `oneOf`. That is correct: the Messages API refuses `oneOf`/`anyOf`/`allOf` at the top of `input_schema`. `Server::send_file` enforces the rule: both given → "not both"; `paths` that is not an array, has 1 or 11 items, or has a blank or non-string item → `BAD_PATHS`; a null field counts as absent. All of these are covered in the channel test.
- **Running sessions**: a session started before the update keeps the old tool list (`required: ["path"]`, `additionalProperties: false`), so Claude can only send `path`, and the new agent handles that exactly as before (`upload` single branch: same texts, same caption fallback). Nothing breaks, and `paths` only appears after a restart.
- **Single `path` unchanged**: `upload` → `read_file` + `transfer(&[file], Some(caption))`. The offer has no `parts`. The texts "Sent to the Telegram topic of this session.", `PENDING` and `refused()` are the same as before.

## 3. Issues

1. **minor — `slots.rs` `send_album` (`self.queued_messages += ops.len()`), with the gates at `on_file_offer` ~3267 and `on_file_chunk` ~3333.** The gate only checks `queued_messages >= MAX_QUEUED_MESSAGES`, and a mixed album adds 2. With 255 queued, an album pushes the count to 257. The summary admits this. The effect is harmless because every decrement saturates and nothing asserts `<= 256`, but the documented cap is no longer exact. Fix: in `on_file_chunk`, gate with `queued_messages + messages_needed > MAX_QUEUED_MESSAGES`, where `messages_needed` is 2 when the parts mix pictures and other files. Or keep it and document the cap as "+1 for a mixed album".

2. **minor — `agent.rs` `start_upload` / `spawn_sender`: the album capability is fixed when a call is queued, not when it is sent.** `(call, hub_albums)` is captured at `try_send`. A call can wait behind up to `UPLOADS = 4` other calls (each up to OFFER_WAIT + SENT_WAIT). If during that time the link reconnects to a hub without `albums` (a hub rolled back, or a second hub address), the agent sends `parts` to a hub that ignores the field. That hub then sees one file of `size` bytes named after the first part and sends the concatenated bytes as one broken document. This needs a hub downgrade during a queued call, so it is unlikely. The fix is cheap: read the current `hub_albums` when the transfer starts, for example from a `watch::Receiver<bool>` given to the sender, or have `LinkEvent::Up` push the value into the sender's `events`.

3. **minor — `hub/scheduler.rs` `SendAlbum` = one token.** This follows the spec ("one sendMediaGroup = one token"). Telegram does not say whether an album counts as one message or N toward the per-group flood limit. If it counts as N, a 10-item album can produce 429s sooner than the bucket predicts. The 429 path (`retry_after` pauses the queue) contains that, so nothing is lost. If live use shows 429s right after albums, meter by item count. Nothing to change now. This is a risk note.

4. **minor — `hub/api.rs` `is_photo_refusal` for a group.** When a group fails, the description Telegram gives is usually of the form `failed to send message #N with the error message "IMAGE_PROCESS_FAILED"` (contains IMAGE/PHOTO), so the fallback triggers. A group refusal whose text names neither PHOTO nor IMAGE (for example a generic media or group error) goes unretried, and the whole photo album is reported `not sent`. This is consistent with the single `SendPhoto` path. Record it as a known limit. There is no live probe of a real group-refusal text (the Bot API excerpt in scratch does not cover error texts).

5. **minor — `agent.rs` `upload_several` + `transfer`: peak memory.** A group is held twice at once, as the separate `Readable`s and as the `Cow::Owned` concatenation, and the next file is read before the full group goes. That is up to about 150 MB in the agent for a 50 MB album. The hub's `send_album` also does `to_vec()` per part on top of `into_bytes()`, 2x on the hub. Acceptable for a user-triggered action. Streaming chunks straight from the parts would avoid the concat copy.

## 4. Missing coverage

- The byte cap split in `upload_several`: files adding up to more than `files::MAX_UPLOAD` must go as several offers, and the caption only with the first. No test covers it, and the `most`/`bytes` condition is the only logic there. A unit test with a tiny fake cap is not possible because the constant is fixed. A test using 2 × 30 MB sparse files in a TempDir is feasible (read cost only).
- `Transfer::Pending` inside a `paths` call: the first group is silent past SENT_WAIT, then the second group is offered. The answer should list the first group as pending and `isError=false` (`sent == 0 && pending > 0`). Not tested.
- `Heard::Lost` in the middle of an album (link drop after `Accepted`): each file of the group should be `not sent: <LOST>`. The existing LOST test covers the single path only.
- Mixed version at the e2e level: `files_e2e` runs only new with new. The old-hub branch is covered by the agent unit test with a raw fake hub, which is enough. The reverse (old agent, new hub) is covered only by the absence of `deny_unknown_fields`. There is no test decoding a `registered` with an unknown extra field into `HubMsg`. A one-line decode test would pin it.
- A photo album that is refused and whose document album is refused too, end to end (`on_album_done` then reports all parts `Failed`). Covered at the api level, not at the slots level.

## 5. Nits

- `channel.rs` `send_file`: with no arguments at all, the error says "send_file needs a non-empty `path` string" and does not mention `paths`.
- After the photo→document fallback, a caption-less album keeps the file-name captions that were put on the photos, so each document shows its name twice (as the file and as the caption).
- `slots.rs` `on_album_done`: `delivery: None` (unclear send) is reported as `Failed` / "Telegram did not take it", though the album may have gone. This matches the single path (`on_file_done`), but the agent text states it more strongly than the single path's `refused(Failed)` text.
- `agent.rs` `upload_several`: status is compared through the answer string (`*result == SENT`, `== PENDING`). A small enum per file would be clearer than comparing prose.
