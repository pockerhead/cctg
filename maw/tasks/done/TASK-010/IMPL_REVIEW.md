# IMPL_REVIEW — TASK-010 (code-reviewer, claude/opus, effort=medium)

Reviewed: `git diff main -- Cargo.toml Cargo.lock crates/` (11 files, commit `46c6740`), against `TASK_FINAL.md`, `PLAN_FINAL.md`, `OPEN_DECISIONS.md`, `IMPL_SUMMARY.md`.

## 1. Verdict

**NEEDS_WORK**: both ends of the agent link silently lose a whole message when a line arrives in more than one TCP read while the other `select!` branch fires. `wire::read_line` is not cancel-safe. Two reproduction tests fail on the current code (see I1). Everything else matches the plan and holds up under attack.

## Disconfirmation (written before evaluation: `scratch/code-reviewer/disconfirmation.md`)

Counter-example 1: a first agent line with `v=1,type=hello` and a field that fails to deserialize, or a `register` whose wrong-typed field holds the secret. If `WireError::Malformed` carried the `serde_json::Error` text, serde would quote the secret (`invalid type: string "..."`) into the hub log. **Did not hold.** `wire::decode` and `decode_hook` (wire.rs:202-207, 420-425) drop the serde error with `map_err(|_| WireError::Malformed)`. `WireError` has only fixed texts. `tests/ingress_logs.rs` exercises exactly these lines under a global TRACE subscriber.

Counter-example 2: a pipelined request, where the body slice is taken as `buf[head_end..]` and bytes past Content-Length get parsed. **Did not hold.** ingress.rs:505-508 rejects `body.len() > length` with 400. `one_connection_carries_at_most_one_event` covers it.

While looking for the second case I found a different real defect (I1).

## Verification run (by me, not from the summary)

- `cargo test --workspace --offline`: 184 passed, 0 failed, 1 ignored (the old config test). Output in `scratch/code-reviewer/test.txt`.
- `cargo clippy --workspace --all-targets --offline -- -D warnings`: clean. `cargo fmt --all -- --check`: clean.
- Reproduction crate `scratch/code-reviewer/cancel_repro/` (outside the workspace, path dependency on `crates/cctg`). 2 of 2 fail on HEAD. Output in `scratch/code-reviewer/repro_out.txt`.
- Dependencies: only `subtle` was added (already in the tree through rustls, 2.6.1, one line in the lock). The orchestrator sanctioned it. No `teloxide` or `rmcp`, nothing printed to stdout.

## 2. Confirmed correct

- **Versioned wire (AC1).** Every direction round-trips (`wire.rs` tests at 527-559). The decode order is version, then type (`Kinds::KINDS`), then fields, with typed `Version` / `UnknownKind` / `Malformed` and no panic on external input. The only `expect`s serialize our own types (wire.rs:196, hook.rs:39).
- **Secret before Register (AC2).** `agent_session` (ingress.rs:137-158) reads Register only after `Hello` matched. A `Register` as the first line gets `Rejected{Auth}` with no event (test at ingress.rs:616-640). Comparison uses `subtle::ConstantTimeEq`, which short-circuits on length only (wire.rs:103-105).
- **Bounded line (AC2).** `read_line` reads through `take(MAX_LINE)`, and `capacity <= 2*MAX_LINE` is asserted (wire.rs:653-670). A registered peer that floods without a newline is disconnected (ingress.rs:701-733).
- **Reconnect (AC3).** The hub-restart test is real: it aborts `serve_agents`, uses a hang-up listener to observe 5 attempts with growing gaps, then rebinds and checks re-Register plus delivery of the message queued while down (agent.rs:282-351). The equal-jitter bounds are checked across 40 attempts.
- **Hook POST and idempotency (AC4, AC5).** `EventId` is minted in `HookPost::new` from random SipHash keys, not from payload fields. Dedup is bounded by count and TTL (`Dedup::expire`, ingress.rs:273-283, with exact boundary tests). An id is remembered only after `try_send` succeeds, so a 503 followed by a re-send delivers the event (ingress.rs:393-423, test 1035-1055). A resume with the same `session_id`/`source`, or two `Stop`s of one prompt, stay two events.
- **Strict HTTP/1.1.** Version must be exactly `HTTP/1.1`. Names must be tchar only. Values reject any CTL except HTAB. TE is refused. Content-Length must be a single all-digits value; overflow gives 413. Auth is checked before the body is allocated. Early 401 with a lingering drain of `MAX_HEAD + MAX_HOOK_BODY`. 2 s deadline. At most one request per connection. The hook client accepts only a complete `HTTP/1.1 NNN[ ...]\r\n` status line (hook.rs:73-80). All of this is covered by `bad_requests_get_errors_and_no_event` and the `status_lines` tests.
- **Loopback default (AC6).** `127.0.0.1:47291/47292`. Overrides accept only a literal `ip:port`. A non-loopback bind logs a warning. Both listeners bind before `BotApi` (hub/mod.rs:95-103). Config and bind are tested.
- **No secret in logs (AC7).** `Secret`, `LinkConfig` and `Config` redact in `Debug`. `ConfigError::Secret` carries only the reason. The log capture sits in its own test binary with `.without_time()` and positive assertions for 6 log lines, so the test cannot pass empty.
- The implementation matches REF byte for byte, as the plan requires (the summary claims 11/11 hashes; the diff matches the plan's table of changes).

## 3. Issues

### I1 (major, blocking): `wire::read_line` inside `tokio::select!` drops a partially received line, on both ends

- **Where:** `crates/cctg/src/wire.rs:226-241` (`buf.clear()` at the start of every call, and a new `take(MAX_LINE)` per call). Used as a `select!` branch at `crates/cctg/src/hub/ingress.rs:191-220` and `crates/cctg/src/agent.rs:165-195`.
- **Mechanism:** tokio `read_until` is resumable only if the caller keeps `buf`. The docs say "Any partially read bytes are appended to `buf`, and the method can be called again to continue". The bytes are already `consume`d from the `BufReader` (tokio 1.53.1 `read_until.rs:56-66`). When the `outbound.recv()` / `outbox.recv()` branch wins while half a line is buffered, the future is dropped. The next call clears `buf`, the first half is gone, and the second half fails to decode ("agent line ignored" / "hub line ignored").
- **Impact:** silent loss of a `reply`, `permission_request` (agent to hub), `inbound` or `permission_verdict` (hub to agent) whenever traffic goes both ways and a line spans more than one read. That happens for any line over the 8 KiB `BufReader` capacity whose tail has not arrived yet, and routinely over Tailscale. The loss is not even at-most-once "on write failure", which is the only loss the plan accepts (PLAN_FINAL §4).
- **Proof:** `scratch/code-reviewer/cancel_repro/tests/repro.rs`. Each test writes half of a 1 KB line, lets the other side's outbound branch fire, then writes the second half.
  - `hub_side_split_line_survives_outbound`: the reply never reaches `AgentEvent::Message` (timeout).
  - `agent_side_split_line_survives_outbox`: `LinkEvent::Message(inbound)` never arrives.
  - Both FAIL on HEAD.
- **Why the existing tests miss it:** every test writes whole lines in one `write_all`, and no test sends in both directions at the same time.
- **Suggested fix (smallest):** make `read_line` resumable. Do not clear at entry; the caller clears after it consumes a complete line. Bound the read with `take((MAX_LINE - buf.len()) as u64)` and check `buf.len() >= MAX_LINE` for `TooLong`. Update the handshake callers to clear between lines. Alternative: one reader task per connection that feeds complete lines into an mpsc, so `select!` waits only on cancel-safe `recv()`. Add both repro tests (hub side in `hub::ingress::tests`, agent side in `agent::tests`) as regression tests.

### I2 (minor): the head limit can overshoot `MAX_HEAD` by up to 1023 bytes

- **Where:** `crates/cctg/src/hub/ingress.rs:432-448`.
- **Problem:** the size check runs before a read, and the terminator search covers the whole buffer. A head that ends at about 9 KiB is accepted even though the limit is 8 KiB. Memory stays bounded; only the documented limit is soft.
- **Fix:** check `end > MAX_HEAD` after `position` finds the terminator, or cap the read at `MAX_HEAD + 4 - buf.len()`.

### I3 (minor): with duplicate `Authorization` headers the last one wins

- **Where:** `crates/cctg/src/hub/ingress.rs:490-496`.
- **Problem:** a request with one bad and one good Authorization header is accepted. This bypasses nothing, since the secret is still needed, but it is inconsistent with the strict "a repeated Content-Length is an error" stance.
- **Fix:** reject a second `Authorization` with 400, the same way as a repeated Content-Length.

### I4 (minor, latent): the two sides can deadlock when both write large lines at once

- **Where:** `ingress.rs:212-217` and `agent.rs:185-191`.
- **Problem:** both loops do `write_msg(...).await` inside a `select!` arm, so neither reads while it writes. If both peers send lines of up to 1 MiB at once and both socket buffers fill, each side blocks in `write_all` forever. There is no write timeout.
- **Status:** unlikely with today's traffic. The fix for I1 via a reader task removes it too. Otherwise add a write timeout that drops the link.

### I5 (minor): an agent whose owner dropped only the outbox keeps reconnecting

- **Where:** `crates/cctg/src/agent.rs:120-125`.
- **Problem:** between attempts the loop checks only `events.is_closed()`. If the owner dropped the `Sender<AgentMsg>` but kept the receiver, the task keeps reconnecting until the next successful `serve` returns `true`. The doc says "Dropping either one stops the task", and that is true only once the link is up.
- **Fix:** also check `outbox.is_closed()`, or reword the doc.

## 4. Missing coverage

- Split-line delivery with concurrent traffic in the other direction, for both hub and agent (the I1 repros). This is the most important gap.
- A line longer than the 8 KiB `BufReader` buffer that arrives in several segments, both during the handshake and after it.
- A head that ends past `MAX_HEAD` but inside the last 1 KiB chunk, expected 431 (I2).
- Duplicate `Authorization` headers (I3).
- The agent link with the outbox dropped while the hub is unreachable: the task should stop (I5).

## 5. Nits

- `drain_ingress` (hub/mod.rs:71-86) drops `Registered.to_agent` right away, so hub-to-agent traffic is impossible until TASK-011. That is expected, but worth a one-line comment so nobody mistakes it for a bug.
- `Status::PayloadTooLarge` reason is "Content Too Large" (RFC 9110 name), while the variant name is the old one. Harmless.
- `random_u64` builds a new `RandomState` per call. std reuses one per-thread key and increments it, so this is fine for uniqueness, but the doc comment "randomly keyed" overstates independence between calls.
