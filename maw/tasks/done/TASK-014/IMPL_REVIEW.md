# Implementation review

## Verdict

**NEEDS_WORK** — permission requests can be reopened and acted on after their session has already ended because agent and hook ingress are independently ordered.

## Disconfirmation tested

I tested the concrete counter-example where two live sessions use the same five-letter request id and the first slot later moves on. It did **not** hold: callbacks are resolved by Telegram `message_id` and then checked against the request id, while verdict fallback additionally requires the same session, host and Claude pid (`crates/cctg/src/hub/slots.rs:817`, `crates/cctg/src/hub/slots.rs:967`). The corresponding two-session test also passes.

## Confirmed correct

- Wire v1 compatibility is preserved with optional `Register.verdict_ack` and optional, omitted-when-absent `verdict_id`; the exact legacy verdict line is pinned by a test (`crates/cctg/src/wire.rs:129`, `crates/cctg/src/wire.rs:205`, `crates/cctg/src/wire.rs:657`).
- The agent keeps a bounded verdict-id cache across reconnects, forwards each identified verdict once, and acknowledges every copy (`crates/cctg/src/agent.rs:193`).
- Callback routing uses Telegram message id plus request id, and reconnect fallback is scoped to session, host and pid (`crates/cctg/src/hub/slots.rs:803`, `crates/cctg/src/hub/slots.rs:967`).
- Stranger callbacks are removed at the allowlist gate before they reach the slot actor (`crates/cctg/src/hub/updates.rs:97`).
- Prompt text reserves UTF-16 space for the final mark, the prompt book is capped at 256, selected prompts are not eviction candidates, waiting state is derived from the whole prompt book, and final-edit failures retain their attempt counter (`crates/cctg/src/hub/permissions.rs:90`, `crates/cctg/src/hub/permissions.rs:240`, `crates/cctg/src/hub/permissions.rs:384`).
- Permission sends use the scheduler's priority lane and the ordering tests pass (`crates/cctg/src/hub/slots.rs:751`, `crates/cctg/src/hub/scheduler.rs:776`).
- Final edits remove the keyboard, retry up to five times, and treat the documented terminal 400 responses as applied (`crates/cctg/src/hub/slots.rs:781`, `crates/cctg/src/hub/slots.rs:1117`).
- `cargo fmt --all --check`, `cargo clippy --workspace --all-targets -j 1 -- -D warnings`, `cargo test --workspace --no-fail-fast -j 1`, and `git diff --check` all passed. No `Cargo.toml` or `Cargo.lock` changed. The single temporary Cargo target under `%TEMP%` was deleted.

## Issues

### Major — a late queued permission request can resurrect an ended session's prompt

**Location:** `crates/cctg/src/hub/slots.rs:367`, `crates/cctg/src/hub/slots.rs:648`, `crates/cctg/src/hub/slots.rs:751`

`close_ended_prompts()` runs only while handling a hook (`slots.rs:466`). If the actor processes `SessionEnd` and then processes an `AgentEvent::Message::PermissionRequest` that was already queued on the independent agent ingress, `on_permission_request()` checks only that the connection exists. It does not reject a known ended session. `send_prompts()` likewise resolves the retained slot/topic without checking `SessionEntry::ended`. The result is a fresh prompt with live buttons after SessionEnd; because the original connection can still be present and bound to that session, pressing it can send a verdict. This breaks the required SessionEnd terminality and makes the outcome depend on `tokio::select!` ordering between the separate hook and agent channels.

Suggested fix: reject permission requests for a known ended session before inserting them, and make sending conditional on the session still being live. Preserve the intentional agent-before-SessionStart case for genuinely unknown sessions. Add a regression test that explicitly drives SessionEnd first, then the queued permission request, and verifies no permission send and no verdict. Also audit `/clear` rebinding: an agent frame received before the rebind but consumed afterward must not be attributed to the new session merely because `Conn.session` has changed.

### Minor — pruning can erase the evidence needed to close prompts on start-first `/clear`

**Location:** `crates/cctg/src/hub/registry.rs:448`, `crates/cctg/src/hub/registry.rs:656`, `crates/cctg/src/hub/slots.rs:728`

At the 1024-session cap, a `/clear` SessionStart (or reused-pid start) can mark the old current session ended and then prune it inside `session_started()` before `Slots::close_ended_prompts()` runs. The sweep only closes prompts whose session entry still exists and is marked ended, so the old prompt can retain stale buttons until prompt-book eviction. This limitation is mentioned in the rollout notes but is not covered by a test and conflicts with the otherwise unconditional start-first `/clear` closure claim.

Suggested fix: return the ids of sessions ended/pruned by `apply_hook`, or close prompts from a pre/post transition set before pruning removes their registry entries. Add a test with `Registry::MAX_SESSIONS` reached and a prompt on the old `/clear` session.

## Missing coverage

- Permission request consumed after `SessionEnd`, including the independent-ingress ordering race described above.
- Permission request already queued when a `/clear` SessionStart rebinds the connection to a new session.
- Start-first `/clear` and reused-pid prompt closure while registry pruning is active at `MAX_SESSIONS`.
- Permission final-edit responses `message is not modified`, `message to edit not found`, and `message can't be edited` are handled in code but lack focused permission tests.
- The required live Telegram/Claude check remains manual and was not run, as instructed; unit/integration tests cannot confirm that a real Telegram press closes Claude Code's terminal dialog or that Telegram removes the keyboard for the empty inline keyboard payload.

## Nits

- The implementation summary says the plan was followed with no deviations, but the plan itself records the pruning behavior as an accepted limitation. Calling that out explicitly in the summary would make the handoff less absolute.
