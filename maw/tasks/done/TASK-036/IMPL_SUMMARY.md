# TASK-036 implementer summary

Commit `de13c4b` on `feature/team-mode` (19 files, +372 / -16).

## 1. What was implemented

Team mode is decided in one place: `Allowlist::is_team()` (more than one id). `updates::classify` fills `from_name` only then, so with one allowlisted user every downstream text and meta is byte-identical to before.

- `crates/cctg/src/hub/config.rs` (+6): `Allowlist::is_team`.
- `crates/cctg/src/hub/api.rs` (+3/-1): `User.first_name`; `Message.from` is now `Option<Box<User>>`, because the larger `User` pushed `scheduler::Outcome::Sent(Message)` over clippy `large_enum_variant`.
- `crates/cctg/src/hub/updates.rs` (+121): `NAME_LIMIT = 32` (UTF-16 units), `author_name(&User)`, which takes the username, else the first name. Any whitespace becomes a space and runs collapse to one. Control characters, invisible format characters (bidi overrides, zero-width, U+2060..206F, BOM, soft hyphen) and `<>"` are dropped, and the result is cut with `registry::cut`. It returns `None` when nothing is left and never uses the id. `Inbound.from_name` and `CallbackInput.from_name` are set in `classify` only for a team. Two tests.
- `crates/cctg/src/hub/buffer.rs` (+58): `Parked.from_name` (serde default, skipped when `None`, so old `registry.json` loads). `Parked::content` order: quote block, then `Имя: `, then `(переслано)` line, then the text. A file with no caption gives `Имя:`. `burst_content` keeps each part's own author. One test.
- `crates/cctg/src/hub/slots.rs` (+120, of which about 12 are non-test lines): `on_topic_message` carries `from_name` into `Parked`. `inbound_meta` adds `from_name`, which is used for single messages and files. `burst_meta` drops `from_name` when the parts have different authors. `press` sets `prompt.decided_by` on the press that moves an Open prompt (agent or hook) to decided; later presses change nothing. Two tests: a burst with one author and a burst with two, and a signed hook-prompt decision where a later press by someone else changes neither the answer nor the name.
- `crates/cctg/src/hub/permissions.rs` (+60): `BY = " · "`, `Prompt.decided_by`, `decided_text(prompt, behavior, by: Option<&str>)`. `prompt_text` now reserves room for the mark plus `BY` plus `NAME_LIMIT`, so a signed edit stays within 4096. The huge-prompt test checks this with a 32-emoji name. One new test.
- `crates/cctg/src/channel.rs` (+3): `INSTRUCTIONS` now say the topic may be shared by an equal team, that each message or `---` part starts with `Name:`, and that `from_name` is present when one person wrote all of it.
- `README.md` (+1): a short note on team mode.
- Test literals got `from_name: None`: `commands.rs`, `mod.rs`, `registry.rs`, and `tests/{buffer_e2e,command_logs,overflow_logs,permission_hook_e2e,reads_e2e,soak,status_e2e,stream_e2e}.rs`. This was done by `scratch/add_from_name.py` from the rustc E0063 list in `scratch/missing_fields.txt`.

The resulting edit looks like: `…\n\n✅ Разрешено из Telegram · anna_k`.

## 2. Deviations and choices (also in log.jsonl as decisions)

- Name order is username, then first name. A username is unique inside a team, and first names can collide.
- Burst meta `from_name` is set only when one person wrote every part. It is omitted when authors differ, because "the author of the last part" would mislabel the earlier parts. The content names each part's author either way.
- Signed presses: only permission decisions are signed, because they are the only press with an edited result message. Resume only answers a toast, and its keyboard is removed at period end with a fixed text. Status ⏹ and ⬆️ answer toasts and later send notices without an edit. None of these has a result message to sign.
- Decisions are signed only in team mode, the same rule as for messages. One person pressing their own button does not need a signature, and existing single-user texts stay unchanged. If the reviewer reads acceptance criterion 2 as unconditional, this is the line to change: `.filter(|_| allowlist.is_team())` in `classify` for callbacks.
- User ids: `author_name` never reads `from.id`, and no new log line carries a name.
- TASK-038 merge: the only non-test changes in `slots.rs` are in `on_topic_message`, `inbound_meta`, `burst_meta` and `press`. In `permissions.rs`, `decided_text` gained a parameter, and its only caller is `Prompt::final_text`.

## 3. Test results

Environment: `CARGO_TARGET_DIR=C:/Users/user/dev/cctg/target`, `CARGO_PROFILE_DEV_DEBUG=0`, `-j 1`.
- `cargo fmt --all --check`: clean.
- `cargo clippy --workspace --all-targets -j 1 -- -D warnings`: clean.
- `cargo test --workspace -j 1`: all green. The lib has 661 passed and 1 ignored (existing); every integration binary is ok.
- New tests: `updates::tests::{a_team_names_the_author_of_messages_and_presses_and_one_person_does_not, an_author_name_is_cleaned_and_bounded}`, `buffer::tests::a_team_member_is_named_before_the_words_and_after_the_quote`, `permissions::tests::a_team_members_decision_is_signed`, `slots::tests::{a_team_burst_names_each_part_and_its_meta_names_only_a_sole_author, the_decision_is_signed_by_the_team_member_whose_press_fixed_it}`.

## 4. How to verify manually

1. Put two user ids in `CCTG_ALLOWED_USER_IDS` on the hub and restart it.
2. Each user writes in a live session's topic. The session sees `<channel ... from_name="anna_k">anna_k: текст</channel>`. Two messages from different people within the gather window arrive as one inbound with `Анна: …\n\n---\n\nИван: …` and without `from_name`.
3. Trigger a permission prompt and press Allow. After the agent acks, the message ends with `✅ Разрешено из Telegram · <name>`. A second person's later press gets "Уже решено", and the name stays.
4. With one id in the allowlist, texts and meta look as before (no prefix, no `from_name`, no signature).
