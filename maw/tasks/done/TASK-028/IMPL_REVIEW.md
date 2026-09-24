# TASK-028 code review (small-fix, against TASK_FINAL.md)

## Verdict

PASS. I found no path that prints a decision without a real allowlisted press on that exact prompt. Every no-decision path (timeout, 204, 404, hub down, hub stop, SessionEnd, gone hook) leaves stdout empty. clippy and the tests are green. The remaining findings are minor, plus one premise that only a live QA check can settle.

Verification (my own run, one `CARGO_TARGET_DIR` under %TEMP%, `CARGO_PROFILE_DEV_DEBUG=0`, `-j 1`, deleted afterwards):
- `cargo clippy -j 1 --workspace --all-targets -- -D warnings`: clean.
- `cargo test -j 1 --workspace`: all binaries ok (lib 440 passed / 1 ignored, `permission_hook_e2e` 5 passed, `hook_cli` 8 passed), exit 0.
- No Cargo.toml changes, so no new crates.

## Disconfirmation

The counter-example I tested: a hook prints `allow` when the user did not press Allow on that same prompt. Variants: a press from a non-allowlisted user, a stale or other-topic message, another session's verdict, id collisions, timeout, hub stop.
Result: it held. The code is safe:
- `Some(behavior)` reaches the hook only from `check_hook_asks` (`slots.rs`) when the prompt state is `State::Decided`. For a hook prompt that state is set only by `press_hook`. `press_hook` is reached from `press` only after three checks: the `updates.rs` allowlist gate (unchanged, applies to every callback), `prompts.by_message(message_id)`, and `request_id == prompt.request_id`. So the message id decides which prompt a press belongs to, and ids never mix across sessions.
- The waiter is keyed by the prompt key (a monotonic u64, never reused). The prompt is built from the same `PermissionPost` whose oneshot it holds, so the verdict always matches the content that was shown.
- Hook prompts use `conn = 0`, and ingress conn ids start at 1 (`ingress.rs` `next_conn = 1`). `push_verdict` and `verdict_conn` only act on `Selected` prompts, which a hook prompt never becomes. A hook answer can never go to an agent, and an agent ack can never decide a hook prompt (`by_verdict` matches `Selected` only).
- Stop path: `hook_asks.clear()`, `drop(asks)` and `finish(Expired)` on the waiters, then pump. A dropped oneshot means `None` at ingress, which answers 204, and the hook prints nothing.

## Confirmed correct

- Hook side (`crates/cctg/src/hook.rs`):
  - `run` branches to `permission()` before `build_here` and the spool, so this hook is never spooled.
  - stdout is written only on `Ok(Some(_))`. 204, 404, 401, `BadResponse`, IO errors and timeouts all print nothing and exit 0.
  - The decision JSON matches the spec shape.
  - `parse_answer` is strict: a malformed 200 is an error, not `allow`.
  - Logs: `PostError` is fixed text. The e2e test asserts that stderr has no secret, command or description with `RUST_LOG=trace`.
- Ingress (`crates/cctg/src/hub/ingress.rs`):
  - The secret is checked in `read_request` for both routes before the body is read. An unauthenticated `/v1/permission` gets 401 (tested with a wrong secret).
  - A waiting request drops its general hook permit and takes one of 16 own permits. A 17th request gets 204 at once, so other hooks are never starved.
  - The `gone()` read detects a client that left. Nothing in the hook client half-closes, so the connection stays open while it waits.
  - A 95 s cap backstops the actor. `serve_hooks` keeps 404 for older callers.
- Slot actor (`crates/cctg/src/hub/slots.rs`):
  - The actor never awaits a hook. Asks arrive over a bounded mpsc, and the answer is a oneshot settled in `pump`. The actor wakes at least every 1 s while hooks wait. Retry work stays gated by `next_retry`, and `pump` saves only when dirty.
  - Only live top-level sessions get a prompt.
  - Twin matching is one-to-one in both directions within 1.5 s.
  - Timeout, gone hook, SessionEnd (`close_prompts` gives `Closed`) and a full book all end as no decision and the buttons are removed.
  - A press after the hook left answers "Запрос устарел". A second press answers "Уже решено" without an agent verdict.
- Wire (`crates/cctg/src/wire.rs`): additive, `VERSION` not bumped, version checked, decode errors are fixed text.
- Docs: `docs/hook-settings.json` and `docs/poc.md` set `"timeout": 100`. `hook_cli.rs` asserts that only PermissionRequest has a timeout.
- Acceptance criteria: 1 (e2e: channel first, hook exits in < 1.5 s, one prompt), 2 (e2e allow/deny with the real binary), 3 (SessionEnd, Stop, nothing listening) and 4 are covered by tests. Criterion 5 is green.

## Issues

1. **major (risk, needs live QA, not a code bug)**: the whole design depends on an unverified premise.
   - Where: `slots.rs` `on_permission_ask`/`check_hook_asks`, together with `docs/hook-settings.json`.
   - Problem: if a waiting PermissionRequest hook blocks the terminal dialog, the channel `permission_request` probably comes only after the hook returns. Then every permission prompt, including ordinary ones in channel sessions, takes the hook path: 1.5 s later hook buttons appear, and the terminal user cannot answer for up to 90 s unless they press in Telegram. It is safe (no wrong decision, one button set at a time), but it is a UX regression for someone sitting at the terminal. The summary and spec already name this as unverified.
   - Suggested fix: before deploy, QA runs a hidden-console live probe that records the order of the hook stdin and the channel `permission_request`, and whether the dialog is visible while the hook waits. If the hook blocks, reconsider: for example, match on "the session's agent has a channel" and answer no-decision at once for ordinary prompts, or shorten the wait.
2. **minor**: every permission prompt waits 2 s while the hub is down.
   - Where: `hook.rs` `PERMISSION_CONNECT_TIMEOUT = 2 s`.
   - Problem: on Windows, a connect to a dead loopback port lasts the full timeout (known risk lesson). With the hook registered at user scope, every permission prompt on the machine waits 2 s whenever the hub is down. The other hooks use 300-500 ms.
   - Suggested fix: about 500 ms for connect+send. The long wait only needs to cover the answer.
3. **minor**: a resent channel request can consume another request's hook ask.
   - Where: `slots.rs` `on_permission_request`, where `note_relayed` is called before `prompts.open`.
   - Problem: a request the book reports as `Duplicate` (an agent re-sending an already shown request) still consumes a waiting hook ask or records a "relayed" entry. That hook may belong to a different request of the same tool that has no twin. Its dialog then gets no Telegram buttons and can only be answered in the terminal. Safe, but a missed relay.
   - Suggested fix: call `note_relayed` only for `Opened::Added` (and maybe `Full`).
4. **minor**: an id collision silently hides a channel prompt.
   - Where: `slots.rs` `show_hook_prompt` and `permissions.rs` `Prompts::open`.
   - Problem: a hook prompt's hub-minted id avoids the ids of the session's active prompts, but a later channel request with the same id (chance about 1 in 9.8 million per pair) is treated as `Duplicate` and not shown.
   - Suggested fix: the duplicate check could ignore prompts with `hook == true` (or compare `hook` flags). Low priority.
5. **minor (accepted deviation, documented)**: pairing by (session, tool_name) only. With parallel same-tool requests, the buttons may show the request that the channel also relays, while the unrelayed one gets no buttons. The shown content always matches the hook that receives the verdict, so there is no mis-decision. The deviation is recorded in the log.

## Missing coverage

- No test that a 17th concurrent permission request gets 204 at once while 16 wait (`MAX_PERMISSION_WAITS`), and that ordinary `/v1/hook` posts still succeed during that time.
- No test that a press on a hook prompt with the right request id but a different or stale `message_id` gets "expired" and decides nothing. The generic TASK-014 tests cover this path, but none uses a hook prompt.
- On hub `Control::Stop`, the e2e test checks only the empty stdout, not that the shown prompt gets the `Expired` edit.
- The e2e suite covers only the channel-first twin order; the hook-first order is covered only in the slots unit test.
- No test for a duplicate channel request consuming a hook ask (issue 3).

## Nits

- `slots.rs` `check_hook_asks`: the session for the log line is read after `finish`, which may already have removed an unsent prompt, so the log shows an empty session. Cosmetic.
- `ingress.rs` `gone()` reads 1 byte at a time and ignores the bytes. That is fine for a client that sends nothing more; a client with the secret that keeps sending is bounded only by the 95 s cap.
