# TASK-059 implementer summary

## Bot API contract (checked 2026-09-26)

Source: https://core.telegram.org/bots/api#sendmediagroup, excerpt saved in
`scratch/implementer/botapi-sendmediagroup-2026-09-26.txt`.

- "Use this method to send a group of photos, live photos, videos, documents or audios as an album. Documents and audio files can be only grouped in an album with messages of the same type. On success, an Array of Message objects that were sent is returned."
- `media`: "A JSON-serialized Array describing messages to be sent, must include 2-10 items"; `message_thread_id` and `disable_notification` exist as on the other send methods.
- `InputMediaPhoto` / `InputMediaDocument`: `type`, `media` ("pass 'attach://<file_attach_name>' to upload a new one using multipart/form-data under <file_attach_name> name"), optional `caption` (0-1024 characters).
- Telegram does not say which item of a group it refused, and the whole request fails. So when a photo album is refused as photos (`is_photo_refusal`), the same files go again as a document album inside the same job. That costs one token, like the existing `SendPhoto` to `sendDocument` fallback.

## 1. What was implemented

Wire (`crates/cctg/src/wire.rs`, +87/-): additive changes, `VERSION` stays 1.
- `HubMsg::Registered.albums` (skipped when false): the hub takes album offers.
- `AgentMsg::FileOffer.parts: Vec<FilePart{name,size}>` (skipped when empty), sent only to a hub with `albums`. The files' bytes follow one another in a single transfer: `size` is their sum and `name` is the first file's.
- `HubMsg::FileAnswer.parts: Vec<FileOutcome>` (skipped when empty): the last answer to an album carries a per-file `sent`/`failed`. `outcome` is `sent` if any file went.
- `MAX_ALBUM = 10`. Tests: round-trip samples, old `registered` without `albums`, and byte-identical single-file offer/answer lines.

Channel (`crates/cctg/src/channel.rs`, +108/-):
- `FileCall.path: String` is now `paths: Vec<String>`.
- `send_file` accepts exactly one of `path` or `paths`. `paths` must be 2..=10 non-empty strings, and a null field counts as absent. Violations get tool errors right away.
- The schema describes both fields (`paths`: array, minItems 2, maxItems 10). It has no `required` and no top-level `oneOf`, because the Messages API does not accept `oneOf` at the top of `input_schema`. The server checks the "one of" rule instead.
- The tool description and instructions mention albums.

Agent (`crates/cctg/src/agent.rs`, +520/-, most of it the new test):
- `LinkEvent::Up { files, albums }`.
- The sender gets `(FileCall, hub_albums)`.
- `upload` for one path behaves as before, with the same texts. The code is split into `read_file` + `transfer`.
- `upload_several` reads the files in order and skips unreadable ones, noting the reason.
  - Hub with albums: files are grouped into album offers of at most 10 files and at most `files::MAX_UPLOAD` bytes each. That byte cap equals the hub's `MAX_FILE_BYTES`, so an album never counts as Busy on size alone.
  - Old hub: files go one offer at a time. The caption goes with the first transfer; a lone file without one gets its own name as caption.
- The answer is "N of M files sent to the Telegram topic of this session:" plus one line per path (`sent` / `not sent: <why>` / not confirmed yet). With an old hub it adds "(one by one: ... sends no albums)". `isError` is set only when nothing went and nothing is pending.

Hub:
- `api.rs` (+113): `send_media_group(thread_id, items, photos, notify)` uploads the files as multipart `file<i>` parts, lists them in `media` as `attach://file<i>`, puts each item's caption on it, and returns the first message.
- `scheduler.rs` (+48): `Op::SendAlbum { thread_id, items, photos, notify }` is metered (one message token), uses the Message lane, has a topic, and is handled in `next_permission`. On a photo refusal it retries once as a document album.
- `slots.rs` (+491, about 250 of it test):
  - The `Offer` struct and `album_fits` check parts (2..=10, each 1..=MAX_UPLOAD, sum == size), otherwise `failed`.
  - `Upload.parts`.
  - `send_album` splits the bytes and puts pictures (`is_photo`, <= 10 MB) in a photo album first, then the rest in a document album. A kind with only one file goes as `SendPhoto` / `SendDocument`. The caption sits on the first item of the first message. Without a caption, each photo shows its file name (TASK-051).
  - `Work::Album`/`Done::Album` and the `albums` map collect the per-file results, and after the last message the agent gets one `FileAnswer` with `parts`.
  - Logs carry sizes and counts, never names.
- `ingress.rs`: the hub answers `albums: true`. The rest of the change there is test constructions.

Tests: `tests/files_e2e.rs` (+90) runs the real `BotApi` + `Scheduler` + `Slots` + TCP link + agent loop against the fake Telegram, which now handles `sendMediaGroup`:
- 2 pictures + 2 files become exactly 2 `sendMediaGroup` requests, with the caption on the first photo (`attach://file0`) and no single sends.
- A refused photo album is resent as a document album.
- A missing file shows in the answer and does not stop the others.
- No names or markers appear in the logs.

Mechanical updates: new fields in existing constructions and patterns (script `scratch/implementer/add_fields.py`), `soak.rs` `describe` arm, `buffer_e2e`/`status_e2e`/`statusline_agent_e2e`/`update_e2e`.

## 2. Deviations / not done

- The acceptance criterion says "mixed set: two albums". The code does that when each kind has at least 2 files. A kind with one file goes as a single `sendPhoto`/`sendDocument`, because `sendMediaGroup` requires 2-10 items.
- A set larger than 50 MB in total (or more than 10 files, which `paths` does not allow anyway) goes as several albums in order, so it can be more than two messages. This follows from the hub file budget (`MAX_FILE_BYTES` = 50 MB). Raising that budget was left out as out of scope.
- The hub's `MAX_QUEUED_MESSAGES` check at completion is still "at least one slot free". A mixed album takes 2, so the cap can be exceeded by one message.
- There are no per-item captions beyond the first one (or photo names when no caption is given).

## 3. Test results

All commands ran with `CARGO_TARGET_DIR=C:/Users/user/dev/cctg/target CARGO_PROFILE_DEV_DEBUG=0 -j 1`, after touching lib.rs/main.rs and the changed tests.
- `cargo fmt --all -- --check`: clean.
- `cargo clippy -j 1 --workspace --all-targets -- -D warnings`: clean.
- `cargo test -j 1 --workspace --no-fail-fast` (log: `scratch/implementer/test-full.log`): lib 756 passed / 1 ignored. Every other binary passed except two, and both passed when rerun alone:
  - `transcript --test purity every_source_file_is_scanned`: `read_dir` NotFound. The shared target held a binary compiled with another worktree's `CARGO_MANIFEST_DIR`. After touching it: 3/3 passed. The transcript crate is untouched.
  - `cctg --test permission_hook_e2e a_press_in_the_topic_is_the_hooks_decision`: empty hook decision after 90 s under full-suite load. Rerun alone: 5/5 passed in 3 s. This is a timing flake, and the test does not touch files.
- New tests:
  - `hub::api::tests::a_refused_photo_album_goes_again_as_documents`
  - `hub::slots::tests::an_album_goes_as_a_photo_album_and_a_document_album_and_says_what_went`
  - `agent::tests::send_file_with_paths_offers_an_album_or_one_file_after_another` (album hub and old hub)
  - the `channel` tests (schema, `paths` validation)
  - the `wire` compatibility test
  - the `files_e2e` album section

## 4. Manual verification

1. Build and install hub and client from this branch, then start `claude-cctg` in a folder with a topic.
2. Ask Claude to call `send_file` with `paths` of 2 screenshots and 2 text files plus a caption. The topic should show a photo album with the caption on the first photo, then a document album. The tool answer should list "4 of 4 files sent".
3. Add a nonexistent path to the list. The answer names it "not sent: ..." and the others still arrive.
4. With an older hub (no `albums` in `registered`), the same call sends the files one by one, and the answer says so.
5. A single `path` call behaves exactly as before.
