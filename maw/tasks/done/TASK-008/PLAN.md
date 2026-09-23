# PLAN — TASK-008: hub — Telegram Bot API client and outbound scheduler

Stage: planner (claude/opus, effort=medium). Paths are relative to the repo root `C:/Users/user/dev/cctg`.
`T` = `maw/tasks/in_progress/TASK-008`. `R` = `T/scratch/planner/ws` is a `git archive HEAD` copy of the workspace (HEAD `a2ff7cb`) plus the changes below. The planner built and tested it with `CARGO_TARGET_DIR` under `%TEMP%`.

## 1. Understanding

### What exists (HEAD `a2ff7cb`)

- `Cargo.toml` (workspace, 18 lines): members `crates/cctg`, `crates/transcript`. Workspace deps are `anyhow, clap, serde, serde_json, tokio (macros, rt-multi-thread), tracing, tracing-subscriber, unicode-segmentation`. There is no `reqwest`, `thiserror` or `dotenvy` yet.
- `crates/cctg/Cargo.toml`: bin only. Deps are `anyhow, clap, tokio, tracing, tracing-subscriber`.
- `crates/cctg/src/main.rs` (65 lines): `Command::{Hub, Agent, Hook{event}}` (lines 10-21). All three arms are no-ops (27-30). `init_tracing` writes to stderr (35-40). Unit test `parses_all_subcommands` (42-65) matches `Command::Hub` as a unit variant.
- `crates/cctg/tests/stdout.rs` (23 lines): runs `cctg hub|agent|hook SessionStart` and asserts exit success and empty stdout.
- `crates/transcript`: not touched by this task.
- `.env` (gitignored) has `CCTG_BOT_TOKEN` and `CCTG_CHAT_ID` only. No allowlist variable exists anywhere.
- Baseline `cargo test --workspace`: 59 tests (cctg 2, transcript 57). Premise challenge: `T/PREMISE_CHALLENGE.md`.

### Real Bot API (planner probe, read-only, redacted: `T/scratch/planner/probe_shapes.py` → `probe_shapes.out.txt`)

- `getMe`, `getChatMember` (bot itself), `getForumTopicIconStickers` (112 stickers, all with `custom_emoji_id`) and `getWebhookInfo` were called. The output holds key names and JSON types only. The script asserts that no token, chat id or user id is in it.
- The bot is `status: "administrator"`, `can_manage_topics: true`, `can_delete_messages: true`, `is_anonymous: true`. `getChatMember` returns about 20 `can_*` fields; the hub reads 3.
- Error envelope: `{ok:false, error_code:int, description:str}`, e.g. `400 Bad Request: PARTICIPANT_ID_INVALID`, `404 Not Found`.
- `getWebhookInfo`: no webhook, **2 pending updates**. So the planner did not run the real poll loop, because it would confirm them (see Step 3).
- The service-message fields (`forum_topic_created/edited/closed/reopened`, `is_topic_message` is "Optional. True") were checked against the saved Bot API 10.3 page `maw/tasks/done/TASK-001/scratch/botapi.html`.

### Research

- Telegram flood control: the only correct reaction to 429 is to wait `retry_after` and retry. The limits are about 1 msg/s per chat and 20 msg/min per group (grammY "Flood Limits", https://grammy.dev/advanced/flood; Bot FAQ, already verified in TASK-001). grammY also advises against proactive throttling. The project overrides that on purpose (domain law: token bucket 20/min, a decision from TASK-001). The bucket below is tuned so it does not slow things down more than the law requires.
- Retry storms: a streaming client that ignores `retry_after` floods the API (agno issue #7360, https://github.com/agno-agi/agno/issues/7360). Countermeasure: one attempt per `retry_after`, and a queue-wide pause.
- `reqwest::Error` "may include the full URL used to make the Request … be sure to remove it" (https://docs.rs/reqwest/latest/reqwest/struct.Error.html, `without_url()`). The planner mutation confirms that the token leaks without it (`T/scratch/planner/mutations.out.txt`, first block).
- Token bucket bound: sends in any window W are at most `capacity + W / refill_interval`. So capacity 5 plus one token per 4 s gives at most 20 per minute.

## 2. Approach

Everything lives in `crates/cctg`. It becomes a lib+bin package: `src/lib.rs` has `pub mod hub;`, and `main.rs` calls `cctg::hub::run`. Reason: the Bot API surface (sendDocument, createForumTopic, the `Outbox`) is used only by TASK-009/011. In a bin-only crate it is dead code, and `clippy -D warnings` fails without `#[allow(dead_code)]`. The lib target also lets the scratch RSS probe link the real code. No separate crate, same as the TASK-010 plan (`src/hub/`).

Module tree `crates/cctg/src/hub/`:

| File | Content |
|---|---|
| `mod.rs` | `run(env_file)`: config, `getMe`, `getChatMember`, `check_topic_rights` (startup error), `can_delete_messages` warning, spawn scheduler, `updates::poll`. `RightsError`. |
| `config.rs` | `Config::load(Option<&Path>)` (`--env-file` or `./.env` if present, process env wins), pure `Config::from_vars`. `BotToken` and `Allowlist` have redacting `Debug`. `ConfigError` names the variable, never the value. Chat id must be `-100<digits>`. |
| `api.rs` | `BotApi` (token only inside `base`, manual `Debug`). 11 methods plus `parse_envelope`. Narrow `#[serde(default)]` types: `User, Chat, Message, CallbackQuery, Update, ChatMember, ForumTopic, Sticker`. `ApiError::{Http (always without_url), RetryAfter(Duration), Telegram{code,description}, Decode}`. |
| `updates.rs` | `classify(Update, chat_id, &Allowlist) -> Routed`, `route_batch(Vec<Value>, offset, ..)`, `poll(api, allowlist, handler)`. `Routed::{Input, Callback, Service, Ignored}`. Its payloads carry no user id. |
| `scheduler.rs` | `Op`, `Outcome`, `Delivery`, `Transport` trait (`BotApi` implements it; tests use a fake), `BucketConfig`, `Scheduler<T>`, `Outbox::submit(op) -> oneshot::Receiver<Delivery>`. |

Key rules:

- **Secrets.** The token appears only in `BotApi.base`. Every `reqwest::Error` goes through `ApiError::http` → `without_url()`. Bodies are read with `.bytes()` and decoded by hand, never with `Response::json` or `error_for_status`, which keep the URL. `BotToken`, `Allowlist` and `BotApi` have redacting `Debug`. The only thing logged about the bot is its `@username`.
- **Allowlist gate.** `classify` returns `Routed::Input/Callback` only for `from.id` in the allowlist. `Inbound`/`CallbackInput` have no user field, so handlers cannot log one. `route_batch` logs only the `Ignored` reason or the service kind.
- **Service messages.** They are detected by the `forum_topic_*` fields **before** the allowlist check, because the bot is their sender. They are returned as `Routed::Service{kind, message_id, thread_id}`, so TASK-011 can `deleteMessage` them. They are never `Input`.
- **Tolerant polling.** `getUpdates` returns `Vec<Value>`, and each update is parsed on its own. Unknown update types give `Ignored::Unsupported`. Wrong-typed fields give `Ignored::Malformed`. The offset advances past both. `allowed_updates = ["message","callback_query"]`. Long poll 50 s, request timeout 65 s. Errors: 429 sleeps `retry_after`, others back off 1 s → 30 s. The loop never exits.
- **Scheduler.** One actor task, one request in flight. Four lanes, checked in this order on each pick:
  1. `Permission`: `Op::Send{permission:true}`. Metered.
  2. `Edit`: `editMessageText`, coalesced per `message_id` in place (the older waiter gets `Outcome::Superseded`), plus `answerCallbackQuery`.
  3. `Topic`: `createForumTopic`, `editForumTopic`, `deleteMessage`. No numeric limit anywhere.
  4. `Message`: `sendMessage`/`sendDocument`. Metered, one FIFO. That keeps the order within a topic.

  Metered means one token from `Bucket` (capacity 5, one token per 4 s, min gap 1 s: at most 20 per 60 s, about 1/s). Unmetered lanes never touch the bucket. Any `ApiError::RetryAfter(d)` sets `paused_until = now + d` for the whole queue and pushes the job back to the head of its lane. So there is exactly one attempt per `retry_after` and no storm. A 429 without `retry_after` maps to a 5 s fallback in `parse_envelope`. Time is `tokio::time::Instant`/`sleep_until`, so tests run on paused time (`start_paused = true`, auto-advance).
- **Startup check.** `check_topic_rights`: `creator` is ok, `administrator` needs `can_manage_topics`, anything else is `RightsError::NotAdmin(status)`. The error text names "Manage Topics"/`can_manage_topics`. It runs before the scheduler and the poller, so it fails at start, not at the first `createForumTopic`.

Alternatives rejected (also in `T/log.jsonl`): a bin-only crate with `allow(dead_code)`; a bucket with capacity 20 at 1 per 3 s (up to ~39 sends in a minute; the mutation fails the test); a per-lane 429 pause; a typed `Vec<Update>` for the batch; `teloxide` (domain law); reqwest 0.12/ring (0.13 builds fine offline).

## 3. Steps

### Step 0. Baseline

Target dir outside the repo for every cargo command. Git Bash: `export CARGO_TARGET_DIR="$TEMP/cctg-task008-target"`. PowerShell: `$env:CARGO_TARGET_DIR = "$env:TEMP\cctg-task008-target"`. Then:

```
cargo fmt --all -- --check
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace
```

Expected: all pass, 59 tests. Stop if not. Leave `.claude/` and the task artifacts alone.

### Step 1. Copy the reference files

Copy each `R/<path>` to `<path>` byte for byte (LF, UTF-8, no BOM). Then check SHA-256 (the same list is in `T/scratch/planner/proto_hashes.txt`; `sha256sum -c` from `R` passes):

| Path | Kind | SHA-256 |
|---|---|---|
| `Cargo.toml` | edit: + `dotenvy = "0.15"`, `reqwest = { version = "0.13", default-features = false, features = ["json", "multipart", "rustls"] }`, `thiserror = "2"` | `c431c758f33f38e2a86dee30466f2aa2a1bcdec0586b4bcc7d91f9953a5d8b96` |
| `Cargo.lock` | regenerated (50 → 188 packages) | `6cce2b5eb6aa0aced15b69a5c5016f098a3102d516bbdcafa87d4ab9cc9eddc7` |
| `crates/cctg/Cargo.toml` | edit: + `dotenvy, reqwest, serde, serde_json, thiserror`; tokio `+ ["sync", "time"]`; dev-dep tokio `["test-util"]` | `e112906b03a5d093468ce5dbe2341302e89dd31385368ca99e0399f79868b26f` |
| `crates/cctg/src/main.rs` | edit: `Hub { #[arg(long)] env_file: Option<PathBuf> }`, the arm calls `cctg::hub::run(env_file.as_deref()).await?`, the unit test matches `Hub { env_file: None }` and parses `--env-file x.env` | `d7d41364e639477102a58ebe69804c3c438b52804251fd201d30bb78d1894d17` |
| `crates/cctg/src/lib.rs` | new, `pub mod hub;` | `4f7bf56a27eec147c94eb0bfb24630549281743f8970e91a3d25c0565c176761` |
| `crates/cctg/src/hub/mod.rs` | new (108 lines) | `ee1078d93e35a4273202b688eb8d9c7ea76b466419ee245c19622b803b8849e2` |
| `crates/cctg/src/hub/api.rs` | new (452 lines) | `3bf39a963b984f0f739325a9ea530547cfe635ea4757fc761080888033de527d` |
| `crates/cctg/src/hub/config.rs` | new (222 lines) | `d56778505a8acc2626a47547238a6bcdb808f631acc634bd7e889f5c2f3dbd32` |
| `crates/cctg/src/hub/scheduler.rs` | new (656 lines) | `2055b94ba51c60a0f0373878abee0a3d55b18e42364d5e7dd29ea85cfc229ddd` |
| `crates/cctg/src/hub/updates.rs` | new (396 lines) | `1de64a9c8e34afcaa91beefb0469c08616ad5756e626197f0483dd844d246a22` |
| `crates/cctg/tests/stdout.rs` | edit, see below | `c3c0a346bc76bd9f3bce44950fb5effcdef9c190f102e165b64f563525a6897e` |

Exact diff of the edited files: `T/scratch/planner/proto.diff`.

`tests/stdout.rs` change, and why: `cctg hub` is no longer a no-op. Without config it must fail. So `hub` leaves the success loop (which keeps `agent`, `hook SessionStart`). A new test `hub_without_config_fails_on_stderr_only` runs `cctg hub` in an empty `CARGO_TARGET_TMPDIR/hub-no-config` dir with `CCTG_*` removed from env. It asserts non-zero exit, empty stdout, and stderr containing `CCTG_BOT_TOKEN is not set`. The empty cwd matters: `Config::load` reads `./.env`, and running in the crate dir must never pick up the real token. (`dotenvy::dotenv()` walks up to parent dirs and would find the repo `.env`, so it is deliberately not used; `from_path` reads only the given file.)

`Cargo.lock`: the implementer sandbox may have no network. Copying the lock byte for byte and building with `--offline` works if `~/.cargo/registry` is readable. All crates were already in the local cache (the planner built everything with `--offline`). If the sandbox cannot read the registry, stop and report. Do not change versions.

### Step 2. Verify

```
cargo fmt --all -- --check
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace
cargo tree -p cctg --edges normal --depth 1
git status --short
```

Expected (planner run: `T/scratch/planner/workspace_verify.out.txt`):
- fmt and clippy are clean.
- Tests: cctg lib **22**, cctg main 1, cctg `tests/stdout.rs` 2; transcript unchanged at 57 (10+15+3+14+14+14 + 1 doc-test). Total 82.
- `cargo tree`: `anyhow, clap, dotenvy, reqwest, serde, serde_json, thiserror, tokio, tracing, tracing-subscriber`. All are in the agreed set.
- `git status`: only the 11 files above plus the pre-existing untracked `.claude/` and task artifacts. No `target/` in the repo.

### Step 3. Measurements into task notes

Copy this table into `T/IMPL_SUMMARY.md` (acceptance criterion 8). The planner measured it on this host (16 threads, rustc 1.95.0, `--offline`, empty target dir each time), and the implementer does not need to re-measure. If the implementer does re-measure, add their row next to it, one build at a time.

| Metric | HEAD `a2ff7cb` (hub no-op) | TASK-008 | Evidence |
|---|---|---|---|
| Release `cctg.exe` size | 994,304 B | **5,485,056 B** | `ls -l` of `release/cctg.exe` |
| Clean release build `cargo build --release -p cctg` | 8.5 s (40 crates) | **45.6 s** (127 crates, aws-lc-sys dominates) | `T/scratch/planner/build_base.log`, `build_ref.log` |
| Idle memory (Windows, after TLS calls, scheduler running) | n/a | **working set ≈ 18.7 MB, private ≈ 5.3 MB**, flat from 5 s to 40 s | `T/scratch/planner/rss_probe.out.txt` |

Idle memory method: `T/scratch/planner/rss_probe` links the reference `cctg` lib. It does what `hub::run` does before polling (real `getMe`, `getChatMember`, `check_topic_rights`, plus `getForumTopicIconStickers`, over rustls/aws-lc), then idles with the scheduler spawned. It does not call `getUpdates`, because 2 updates are pending and must not be confirmed. A pending long-poll request adds one kept-alive connection; that is expected to be small, but it was not measured. Real-path checks with the reference binary: `real_wrong_chat.stderr.txt` shows `getChatMember … chat not found` as a clear error, `real_no_allowlist.stderr.txt` shows the `CCTG_ALLOWED_USER_IDS` error, and neither holds the token or bot id (grep counts 0).

Commit on `feature/hub-telegram-foundation`, English message, no generated-by / co-author trailers (project law).

## 4. Test plan (all in the copied files)

| Test | Criterion | What it proves |
|---|---|---|
| `updates::allowlisted_text_is_input_and_strangers_are_dropped` | 1 | messages and callbacks from a non-allowlisted `from.id` give `Ignored::NotAllowed`, never `Input`/`Callback` |
| `updates::other_chats_and_senderless_messages_are_ignored` | 1 | other chat → `OtherChat`; no `from` → `NoSender` |
| `updates::routing_logs_never_contain_user_ids` | 1 | a captured tracing subscriber at TRACE over a batch with stranger/allowed/bot/malformed updates: logs exist and contain none of the ids |
| `api::transport_errors_never_contain_the_token` | 1 | a real reqwest error (loopback port 9) with a token built at run time: `Display`, `Debug`, the anyhow `{:#}` chain and `BotApi` Debug hold neither the secret nor the bot id. Mutation: without `without_url()` it fails |
| `config::debug_hides_token_and_user_ids`, `config::missing_and_bad_values_are_named_but_not_echoed` | 1 | config errors and Debug never echo the token or allowlist values |
| `scheduler::group_limit_and_topic_order_hold` | 2 | 60 sends over 3 topics on paused time: every 60 s window ≤ 20, gap ≥ 1 s, each topic's sequence 0..19 in order, finishes by 225 s. Mutation capacity 20 fails |
| `scheduler::default_bucket_fits_twenty_per_minute` | 2 | `capacity + 60/refill_every ≤ 20`, `min_gap ≥ 1 s` |
| `scheduler::permission_prompt_jumps_the_queue` | (priority) | a permission send queued after 10 messages goes first |
| `scheduler::repeated_edits_of_one_message_coalesce` | 3 | edits 7:a, 8:x, 7:b, 7:c → exactly `c`, `x` sent; the first two waiters get `Superseded` |
| `scheduler::retry_after_pauses_everything_and_retries_once` | 3 | 429 (7 s) on the first op: nothing goes out until 7 s, then a single retry, then the queue resumes. Mutation "no pause" fails |
| `scheduler::repeated_429_is_one_attempt_per_retry_after` | 3 | three 429s of 3 s: attempts at exactly 0, 3, 6, 9 s, next message at 10 s |
| `api::maps_429_to_retry_after` | 3 | `parameters.retry_after` → `RetryAfter(7 s)`; 429 without it → 5 s fallback |
| `scheduler::topic_mutations_and_edits_do_not_spend_message_tokens` | 4 | 40 create/edit topic, delete and edit ops all run at t=0 (no bucket, no gap), and the 5-message burst is still full afterwards (t = 0..4 s). Mutation "everything but edits metered" fails |
| `updates::forum_service_messages_are_never_input` | 5 | all four `forum_topic_*` from the bot and from an allowlisted admin → `Routed::Service{kind, message_id, thread_id}` |
| `updates::unknown_types_fields_and_bad_shapes_do_not_stop_the_batch` | 6 | unknown update type, unknown fields, `text: 5`, no `update_id`, a non-object: the batch continues, the offset reaches 9. Mutation "stop on bad update" fails |
| `api::decodes_ok_result_and_ignores_unknown_fields` | 6, 7 | the real `getChatMember` key set (fake values) decodes to 3 fields |
| `hub::tests::missing_manage_topics_is_a_startup_error` | 7 | admin without the right → `NoManageTopics` (text names `can_manage_topics`); member/left/kicked/restricted → `NotAdmin`; creator ok |
| `stdout::hub_without_config_fails_on_stderr_only` | 7, 9 | a misconfigured hub fails at start with a named variable and writes nothing to stdout |
| `api::maps_other_errors_with_description`, `config::*`, `scheduler::stops_after_outbox_is_dropped_and_queue_drained` | support | error mapping, `-100` chat-id rule, clean shutdown |

Mutation evidence (each mutation made one or two target tests fail): `T/scratch/planner/mutations.out.txt`. Criterion 4 "not hardcoded": `scheduler.rs` has no constant or config for the `Topic`/`Edit` lanes, and `BucketConfig` is consulted only when `Op::metered()`.

## 5. Risk areas

- **aws-lc-sys build.** `reqwest 0.13` `rustls` uses the aws-lc-rs provider. It needs a C toolchain. It builds on this host (MSVC present); a fresh machine or CI needs the same. Fallback if it breaks: `rustls-no-provider` + ring. That needs a direct `rustls` dependency, so record it in the task notes.
- **Implementer sandbox and the registry.** See Step 1. Without read access to `~/.cargo/registry`, the offline build fails. The lock must not be regenerated with other versions.
- **Anonymous admins.** If the user posts as an anonymous admin, `from` is `GroupAnonymousBot`, and the gate drops the message (`NotAllowed`). This is the correct security behaviour, but it will look like "the bot ignores me". The hub's `is_anonymous: true` is the bot's own flag and does not matter here.
- **Global FIFO for messages.** Order within a topic is kept, but one chatty session delays all others (20/min shared). Fairness across topics (round-robin) is not required by this task. TASK-016 (push every turn) may need it.
- **Permission priority breaks topic order on purpose.** A permission prompt overtakes older queued messages of its own topic.
- **Edits are unmetered.** Telegram publishes no edit limit. A burst of distinct-message edits relies on 429 handling alone. Coalescing bounds it by the number of distinct messages.
- **`message is not modified` (400).** An edit with identical text returns `Telegram{400}` to the caller. The scheduler does not treat it as success. TASK-011/015 callers should.
- **Unbounded 429 retries.** A job is retried until Telegram accepts it, at one attempt per `retry_after`. A never-ending 429 stalls the queue by design, and the warn log shows it.
- **Stale cargo artifacts.** Restoring a file with an older mtime (as a planner mutation run did) makes cargo skip the rebuild and report old results. `touch` the sources after any restore. Evidence that the reference is clean: `sha256sum -c` and the re-run in `workspace_verify.out.txt`.
- **Two pending updates in the real bot.** The first real `cctg hub` run (TASK-009 or a manual run) confirms them. They are old and nothing consumes them yet.

## 6. Open questions

1. **Allowlist variable.** The plan introduces `CCTG_ALLOWED_USER_IDS` (comma-separated, required, the hub refuses to start when it is empty). The user has to add it to `.env` before the hub runs. Is the name fine? (Recorded in `T/PCTX_PROPOSALS.md`.)
2. **Bucket shape.** Capacity 5, one token per 4 s, min gap 1 s. A 3-chunk reply goes out in about 2 s; a sustained stream runs at 15/min. Capacity 1 at 3 s would give an even 20/min but no burst. The plan keeps 5/4 s; flip it if steady throughput matters more than latency.
3. **Fairness across topics.** Global FIFO now, round-robin later if TASK-016 needs it?
4. **Idle RSS during a live long poll** was not measured, to protect the 2 pending updates. Is it acceptable to measure it once the user confirms those updates can be dropped?
