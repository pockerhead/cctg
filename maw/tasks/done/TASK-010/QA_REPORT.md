# QA REPORT — TASK-010 (qa, claude/opus, effort=medium)

Code under test: branch `feature/transport-contracts`, HEAD `16c91bb` (fix commit `3fe417b`), `git diff main -- Cargo.toml Cargo.lock crates/` (11 files).
Telegram was not called. `.env` was not read. No code files were changed.

## 1. Environment

- No docker-compose, no dev server. The hub needs a real bot for `run()` past start-up, so QA used (3) the existing cargo test runner plus (4) a separate probe crate with a path dependency on `crates/cctg`: `maw/tasks/in_progress/TASK-010/scratch/qa/probe/`. Real TCP sockets on loopback. Real `serve_agents`, `serve_hooks`, `agent::spawn` and `hook::post`, no mocks except hand-written fake peers (a hub that never reads, a raw agent, a raw HTTP client).
- The `cctg` binary was run only on start-up failure paths that stop before `BotApi` (fake env files in `scratch/qa/run/`, fake token `1:fake-not-a-token`, `HTTPS_PROXY`/`HTTP_PROXY`/`ALL_PROXY=http://127.0.0.1:9` as a safety net so an accidental Bot API call would fail locally).
- `CARGO_TARGET_DIR=$TEMP/cctg-task010-qa-target`, one cargo command at a time.

Reproduce:

```bash
export CARGO_TARGET_DIR="$TEMP/cctg-task010-qa-target"
cd C:/Users/user/dev/cctg
cargo fmt --all -- --check
cargo clippy --workspace --all-targets --offline -- -D warnings
cargo test --workspace --offline
cd maw/tasks/in_progress/TASK-010/scratch/qa/probe
cargo test --offline -- --nocapture            # link.rs, hooks.rs, agent_logs.rs
cd ../run && HTTPS_PROXY=http://127.0.0.1:9 "$CARGO_TARGET_DIR/debug/cctg.exe" hub --env-file no-secret.env
```

Nothing was left running: every test tears its own listeners down. No containers.

## 2. Test results

### Existing suite

| Check | Result |
|---|---|
| `cargo fmt --all -- --check` | clean |
| `cargo clippy --workspace --all-targets --offline -- -D warnings` | clean |
| `cargo test --workspace --offline`, 6 runs | 189 passed, 0 failed, 1 ignored (the old isolated config test) every run |
| `cargo test -p cctg --lib -- wire:: agent:: hook:: hub::ingress:: hub::config::`, 12 runs | 52 passed, 0 failed, 1 ignored every run |
| `cargo test -p cctg --test ingress_logs`, 5 runs | 1 passed every run |

No flakes seen in 23 runs of the timing-sensitive tests.

### New QA probes (`scratch/qa/probe/tests/`)

Disconfirmation, written first (`scratch/qa/disconfirmation.md`): with a steady stream of large lines both ways at once, a main loop blocked in its own write would stop the peer from finishing its write, the 5 s write timeout would fire and messages would be lost. **It did not hold**: 64 × 256 KiB each way arrived complete and in order in 0.44 s, with no `Down` and no `Disconnected`.

`link.rs` (10 tests, all pass):

| Probe | What it shows |
|---|---|
| `qa_simultaneous_large_writes_both_ways_lose_nothing` | the disconfirmation case above. No deadlock, no loss, order kept |
| `qa_byte_by_byte_lines_with_crossing_traffic` | three 20 KiB lines sent one byte per write, hub→agent and agent→hub, while the other direction sends a message every 1 ms. All delivered whole. The I1 fix holds for lines larger than the 8 KiB `BufReader` |
| `qa_hub_drops_a_peer_that_never_reads` | a registered peer that never reads: hub sends `Disconnected` after 5.06 s (write timeout), not never |
| `qa_agent_drops_a_hub_that_never_reads_and_reconnects` | a fake hub that registers and never reads: agent goes `Down` after 5.06 s and connects again |
| `qa_hub_restart_reconnects_and_reregisters` | hub task aborted, port refused for 1.2 s, new `serve_agents` bound on the same port: `Down` → `Up` 0.36 s after the rebind, new `Register` with the same session id, traffic both ways on the new link |
| `qa_outbox_dropped_while_connected_stops_the_task` | hub sees `Disconnected`, events channel closes, no re-registration |
| `qa_outbox_dropped_during_backoff_stop_latency` | with a 4 s backoff the task stopped 1.7 s after the drop, at the end of the current sleep (see B3) |
| `qa_events_dropped_while_unreachable_stops_the_task` | the task ends (the outbox sender reports closed) |
| `qa_registered_and_inbound_in_one_segment` | `registered` and `inbound` in one TCP write: both delivered, the handshake's `BufReader` is carried into the reader task |
| `qa_stalled_hub_consumer_characterisation` | hub event channel of 4, consumer paused 7 s while the agent streams 200 × 256 KiB: agent `Down` after the write timeout, 199 of 200 delivered after reconnect (see B2) |

`hooks.rs` (6 tests; 5 pass, `qa_http_edge_cases` fails on one case, see B1):

- 21 HTTP edge cases: two identical good `Authorization` → 400; good then bad → 400; lowercase `authorization: bearer` → 204; `Bearer<TAB>` → 401; `Basic` → 401; secret plus a suffix or minus the last char → 401; CL shorter than the body → 400; `000N` and `\tN\t` → 204 (valid per the RFC grammar); empty CL → 400; `?x=1` and absolute-form target → 404; `post` → 405; bare-LF request → no answer, closed at the 2 s deadline; non-UTF-8 value → 400; header without colon or with an empty name → 400; `transfer-encoding: identity` → 400; no auth plus a huge CL → 401 with no body read. **`Content-Length: <NBSP>N` → 204** (expected 400).
- `qa_header_limit_exact_even_byte_by_byte`: a head of exactly 8192 bytes → 204, 8193 → 431, and 8192 sent in 97-byte writes → 204.
- `qa_concurrent_resends_of_one_event_deliver_once`: 40 parallel `hook::post` calls with one `event_id` → 40 × `Ok`, exactly 1 event.
- `qa_hook_client_reports_503_and_redelivers`: full queue → `Err(Status(503))`, the re-send after draining → `Ok`, delivered.
- `qa_hook_client_status_line_split_across_segments_is_ok`: `HTTP/1.1 20` + 100 ms + `4 No Content\r\n` → `Ok`.
- `qa_hook_timing_budget`: worst of 20 POSTs to a live hub is 0.8 ms. The 1.5 s `SessionEnd` budget is not at risk.

`agent_logs.rs` (1 test, passes; its own binary with a global TRACE subscriber). This covers the agent side and the hook client, which `tests/ingress_logs.rs` does not capture. Paths: an agent with a secret the hub rejects (70+ attempts), a fake hub that sends back lines carrying the secret (unknown type, wrong-typed field, not JSON, `v=3`), a hook POST with a wrong secret, a hook POST to a closed port. The expected log lines are present. The real and the other secret appear in no log line, no error `Display`/`Debug`, no `LinkConfig`/`AgentMsg::Hello` `Debug`.

### Binary start-up (no Telegram reached)

| Env | Output | Exit |
|---|---|---|
| no `CCTG_HUB_SECRET` | `CCTG_HUB_SECRET is not set; ...` | 1 |
| secret `short-qa-marker` | `CCTG_HUB_SECRET is invalid: the shared secret must be at least 16 characters` (value not echoed) | 1 |
| `CCTG_HOOK_LISTEN=localhost:47999` | `CCTG_HOOK_LISTEN must be an ip:port address ...` | 1 |
| agent port held by another socket | `cannot listen for agents; check CCTG_AGENT_LISTEN` + OS error 10048 | 1 |

### Secret, token and user-id leak hunt (code reading plus the probes)

- `Secret::expose()` has exactly two non-test callers: the `Authorization` header in `hook::post` (it goes only to the socket) and the comparison in `agent_session`. `head` is never logged.
- Every error type (`WireError`, `SecretError`, `PostError`, `ConnectError`, `ConfigError::{Secret, ListenAddr}`) has fixed text, an `ErrorKind` or a number. `decode`/`decode_hook` drop the serde error, which would quote input.
- `Secret`, `LinkConfig`, `Config` (token, allowlist, hub secret) and `AgentMsg::Hello` redact in `Debug`. `AgentEvent` `Debug` has no secret.
- Non-test `expect`/`unwrap`: only `wire::encode` and `hook::post` serialize our own types. No `unwrap` on external input. The only panic text is fixed.
- Log fields are the connection number, the peer address, the first 8 chars of the session id (after auth only), the event kind, the status code. No token or user id is anywhere in the new code.

## 3. Acceptance criteria

| Criterion | Test performed | Result |
|---|---|---|
| Every TCP message variant round-trips through serde; an unknown version or kind is a controlled error, no panic | Read `wire::decode`: `v`, then `type` against `KINDS`, then fields. Existing `every_link_message_round_trips…`, `kind_lists_match_the_enums`, `bad_lines_are_typed_errors`. QA: the fake hub sends `v=3`, an unknown type, a wrong-typed field and non-JSON to a live agent; logged and ignored, no panic (`agent_logs.rs`) | PASS |
| A wrong secret is rejected before Register and never logged; an over-long line closes the connection with bounded allocation | `agent_session` reads `register` only after `hello` matched. Existing `a_wrong_secret_is_rejected_before_register`, `an_overlong_line_closes_the_link`, `an_endless_line_stops_at_the_limit` (capacity ≤ 2 MiB). QA: 70+ rejected attempts, secret in no log (`agent_logs.rs`) | PASS |
| Reconnect with backoff and a new Register on the agent side, proven by a test with a hub restart | Existing `the_agent_reconnects_and_registers_again_after_a_hub_restart` (read: real abort and rebind, growing gaps). QA `qa_hub_restart_reconnects_and_reregisters` on a refused port, plus write-timeout reconnects | PASS |
| The hook endpoint takes one authenticated POST and answers fast; a re-delivery of the same event is idempotent by the event key | QA: worst POST 0.8 ms; 40 concurrent re-sends → 1 event; 503 → re-send delivered; one event per connection (existing pipelining test) | PASS |
| The key is an `event_id` minted by `cctg hook` once per call, random, reused only on re-send; bounded window by size and time; payload fields are not the key | `HookPost::new` mints `EventId::new()` (OS-keyed SipHash); `Dedup` is 4096 ids / 10 min with exact-boundary unit tests; existing `a_repeated_post_is_delivered_once` (a resume with the same `session_id`/`source` is a new event) and `two_stops_of_one_prompt_are_two_events` | PASS |
| Loopback by default and an explicit non-loopback config are covered by tests | `listeners_default_to_loopback`, `non_loopback_listeners_need_an_explicit_address`, `listeners_bind_loopback_and_explicit_addresses`; QA binary run with `localhost:…` → start-up error | PASS |
| The shared secret is never logged on any path, parse errors included | `tests/ingress_logs.rs` (hub side, 5 runs) plus QA `agent_logs.rs` (agent side and hook client) plus the code audit above | PASS |
| Existing tests pass | 6 full workspace runs: 189 passed, 1 ignored (pre-existing) | PASS |

Fixer claims, checked against the code and by probe:

| Claim | Verified by | Result |
|---|---|---|
| I1: a reader task per connection plus mpsc on both ends; resumable `read_line` | `read_hub_frames`/`read_agent_frames`; `select!` waits only on `mpsc::recv`; `read_line` keeps `buf` and limits the read to `MAX_LINE - buf.len()`; QA byte-by-byte crossing-traffic probe | PASS |
| I4: no deadlock under simultaneous large writes; 5 s write timeout on every hub↔agent write | `write_hub_msg`/`write_agent_msg` wrap hello, register, registered, rejected and all frames; QA 2×64×256 KiB probe and both never-reads probes (teardown at 5.06 s, the agent reconnects) | PASS |
| I3: duplicate `Authorization` → 400 | QA: identical good pair and good-then-bad → 400 | PASS |
| I2: exact header limit | QA: 8192 → 204, 8193 → 431, also in pieces | PASS |
| I5: the agent stops when its outbox is closed | QA: connected → immediate; unreachable → at the end of the current backoff sleep | PASS (see B3) |
| Strict HTTP/1.1 ingress, strict hook status line | existing adversarial tests plus 21 QA cases, split status line | PASS except B1 |

## 4. Bugs found

**B1 (nit): Unicode whitespace around header values is accepted.** `read_request` uses `str::trim()` (hub/ingress.rs:524 and 540), which strips Unicode `White_Space`, not only the RFC OWS (SP/HTAB).
- Repro: `POST /v1/hook HTTP/1.1\r\nAuthorization: Bearer <secret>\r\nContent-Length: \u{a0}<n>\r\n\r\n<body>` (`hooks.rs::qa_http_edge_cases`).
- Expected 400 (not `1*DIGIT` after OWS). Actual 204, event delivered. The same trim applies to the bearer token.
- Impact: none on security. The secret is still required, and there is one request per connection with no proxy in front. It only breaks the stated "strict RFC grammar". Fix: `trim_matches(|c| c == ' ' || c == '\t')`.

**B2 (minor, latent; design consequence, not a spec violation): ingress backpressure longer than 5 s becomes link loss.** The hub connection loop awaits `events.send(...)`. If the hub-side consumer stalls for more than about 5 s, the hub's reader stops, the agent's write times out, the agent reconnects, and the message whose write was cut is lost.
- Repro: `link.rs::qa_stalled_hub_consumer_characterisation` (event channel of 4, 7 s pause): 199 of 200 delivered, one `Down`.
- Today's `drain_ingress` never stalls, so this is not reachable in this task. TASK-011 must not do Telegram I/O (429 `retry_after` pauses) inline on this channel. Recorded in `PCTX_PROPOSALS.md`.

**B3 (nit): the stop after the outbox closes waits out the current backoff sleep.** `agent::run` checks `outbox.is_closed()` before and after each attempt, but `sleep(backoff.delay(attempt))` cannot be interrupted, so with the default cap the task can outlive its owner by up to 30 s (one idle task, no connection attempt). A `select!` on `outbox.closed()`/`events.closed()` around the sleep would make it immediate. The doc ("Dropping either one stops the task") is otherwise true.

Observations, not bugs:
- The hub logs `WARN agent rejected` on every retry of a wrong-secret agent (the agent itself logs one warning, then `debug`). At the 30 s backoff cap that is two lines a minute per misconfigured agent.
- A version `rejected` after registration is written without a linger, so a peer that still has unread input may get a reset instead of the frame. The link closes either way.
- Bare-LF requests get no answer until the 2 s deadline, never an event. This is acceptable for an endpoint whose only client is `cctg hook`.

## 5. Verdict

**SHIP.**

All eight acceptance criteria pass on independent tests. So does every fixer claim: the reader tasks, the resumable `read_line`, the 5 s write timeout and what it tears down, duplicate `Authorization`, the exact header limit, the outbox stop, reconnect/re-register across a hub restart, `event_id` dedup (including 40 concurrent re-sends), strict HTTP and the hook status line. The main disconfirmation case (simultaneous large writes) did not hold. fmt and clippy are clean, and the suite was stable across 23 repeated runs. No path puts the hub secret, the bot token or a user id into logs, errors or panics.

B1 and B3 are nits. B2 is a latent constraint for TASK-011, not a defect of this task's contract. None needs a fix before merge. B1 and B3 are one-line changes if the orchestrator wants them anyway.
