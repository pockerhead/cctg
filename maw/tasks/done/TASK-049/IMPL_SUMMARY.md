# TASK-049 implementer summary

Pre-flight passed: wire.rs (Register / HubMsg / AgentMsg, KINDS lists, decode of unknown fields), agent.rs (`run` / `connect` / `serve` link loop), hub/ingress.rs (`serve_agents`, `agent_session`), tls.rs (`HubAddr::connect`, `Incoming::accept`), shim.rs all match the spec's assumptions. socket2 0.6.5 is already in Cargo.lock (through tokio).

Code commit: `b382792 fix: heartbeat and TCP keepalive on the agent link (TASK-049)`.

## 1. What was implemented

Wire (no VERSION bump, capability both ways):
- `Register.heartbeat: bool` (`#[serde(default)]`): the agent announces it. `run_stdio` sets it to true.
- `HubMsg::Registered.heartbeat: bool` (default, skipped when false): the hub answers `true`.
- New `HubMsg::Ping` and `AgentMsg::Ping` (`{"v":1,"type":"ping"}`), added to both KINDS lists. They are never answered.
- `wire::Heartbeat { interval, timeout }` with constants `HEARTBEAT_INTERVAL = 30 s` and `HEARTBEAT_TIMEOUT = 90 s` (the `Default`). `wire::Liveness` tracks the last read and the last write of one link end and gives the next due `Beat` (`Ping` or `Dead`). `wire::beat(next)` sleeps until it (pending forever when the heartbeat is off).

Rule on each end: the heartbeat runs only when the agent announced it AND the hub said so. It sends `ping` when that end wrote nothing for `interval` and drops the link when it read nothing for `timeout`. If a frame is already waiting in the reader mpsc when `Dead` fires, the frame goes first.

Agent (`agent.rs`):
- `LinkConfig.heartbeat: Heartbeat` (intervals; tests pass short ones).
- `connect` returns a `Linked` struct (reader, write, files, heartbeat). `serve` takes a `Link` struct and runs `Liveness` in the existing select loop. Hub pings are swallowed in `serve` and never reach the MCP loop, so a busy owner does not hold them up. On timeout it logs one info line, "hub silent past the heartbeat timeout; reconnecting", and returns `false`, which is the normal reconnect path (backoff, re-register, `LinkEvent::Down`/`Up`).

Hub (`hub/ingress.rs`):
- `serve_agents` keeps its signature and delegates to the new `serve_agents_with(listener, secret, events, heartbeat)`. No existing call site changed.
- `agent_session` runs `Liveness` (only if `register.heartbeat`) in its existing select loop. `AgentMsg::Ping` is consumed before the explicit forward list, so it never reaches slots and never logs "repeated its handshake". On timeout it logs one info line, "agent silent past the heartbeat timeout; unbinding", and breaks. The existing `AgentEvent::Disconnected` and "agent disconnected" follow, so slots unbinds exactly as on a close.

TCP keepalive (`tls.rs`): `keepalive(&TcpStream)` via `socket2::SockRef::set_tcp_keepalive` (idle 30 s, probe interval 10 s). It is called in `HubAddr::connect` (agent link, hook posts, spool replay) and `Incoming::accept` (both hub listeners), before TLS wraps the socket. A failure is logged at debug and ignored.

`channel.rs`: `HubMsg::Ping` was added to the ignored arm of the exhaustive match. It never arrives there.

Files (numstat, added/removed):
- crates/cctg/src/agent.rs +257/-17 (about 150 of that is tests)
- crates/cctg/src/wire.rs +201/-9
- crates/cctg/src/hub/ingress.rs +190/-17 (about 100 of that is tests)
- crates/cctg/src/tls.rs +63/-0
- crates/cctg/src/channel.rs +3/-1
- crates/cctg/Cargo.toml +3 (socket2 = "0.6", with a comment on why), Cargo.lock +1
- crates/cctg/src/hub/slots.rs +14 and tests/*.rs +30/-3: the new field in existing struct literals only (`Register { heartbeat: false }` keeps the old-agent behaviour; asserts of the real hub's answer are now `Registered { files: true, heartbeat: true }`).

New tests:
- wire: `heartbeats_stay_compatible_with_version_one_peers` (ping encoding; legacy `registered`/`register` read as no heartbeat), `liveness_pings_when_quiet_and_dies_when_deaf` (paused time).
- tls: `both_ends_keep_the_connection_alive` (SO_KEEPALIVE on the client and the accepted socket, plain and TLS, checked via socket2).
- ingress: `a_silent_agent_is_pinged_then_unbound` (ping arrives, `Disconnected` no earlier than the timeout, socket closed), `an_agent_that_pings_stays_bound`, `an_agent_without_heartbeat_gets_no_pings_and_stays`.
- agent: `a_frozen_hub_is_left_after_the_heartbeat_timeout` (a fake hub that registers, then never writes and never closes; the agent pings it, drops the link no earlier than the timeout, reconnects and registers again), `without_a_heartbeat_on_both_ends_nothing_is_sent_or_timed` (old hub, and agent without the flag: no ping, no drop for 2x timeout), `the_heartbeat_holds_a_quiet_or_one_way_link` (real ingress plus a real link with a short beat: 3x timeout quiet while the owner reads no link events, which is the TASK-040 hand-over/exit wait case; then 2x timeout of agent-to-hub-only traffic and 2x timeout of hub-to-agent-only traffic, the TASK-032 chunk case; no drop on either end).

## 2. Deviations / not implemented

- No pong: each side's own idle pings refresh the peer's read timer, so a pong adds nothing to detection (log.jsonl decision).
- The worker swap is not driven end to end with a real shim and heartbeat. Heartbeat state is per connection: the swap closes one link and opens a new one. The in-between states (LEAVE_WAIT 5 s, EXIT_WAIT 20 s) keep the link task running. The "owner reads nothing for 3x timeout" test covers that. Existing update_e2e/run_e2e pass.
- Known limit, same class as the TASK-010 lesson: if the link loop itself is blocked longer than 90 s, no pings go out and the peer drops the link. That happens on the agent when 256 real hub messages wait for a stuck MCP loop, and on the hub when the slots actor stops draining ingress events. The owner then gets the normal reconnect.
- A laptop that sleeps longer than the timeout reconnects on wake. The hub already dropped it. This is the intended behaviour.
- A PCTX proposal is in `PCTX_PROPOSALS.md` (hub domain entry for the heartbeat contract).

## 3. Test results

- `cargo fmt --all -- --check`: clean.
- `cargo clippy -j 1 --workspace --all-targets -- -D warnings`: clean.
- `cargo test -j 1 --workspace` (CARGO_TARGET_DIR=main target, CARGO_PROFILE_DEV_DEBUG=0): exit 0. Lib: 619 passed, 1 ignored. Every integration binary is ok. Log: `scratch/workspace_test.log`.
- The 9 new tests were run 5 more times in a row: 9/9 each time (about 4.2 s).

## 4. Manual verification

1. Build and deploy client and hub from this commit. The hub log shows "agent registered" as before.
2. Normal session, idle: `ping` lines go both ways every 30 s. On the hub, `RUST_LOG=debug` shows no "agent line ignored". The session keeps working after an hour idle.
3. Half-open simulation: with a live session, drop the agent's traffic without closing the socket, for example a firewall rule that drops (not rejects) packets to the hub port on the client, or pause the tunnel. Within about 90 s the agent log (Claude Code MCP log) shows "hub silent past the heartbeat timeout; reconnecting" and the hub shows "agent silent past the heartbeat timeout; unbinding" / "agent disconnected". Lift the rule: the agent registers again by itself and messages from the topic arrive.
4. Old client with a new hub (or the reverse): no ping lines, and behaviour is unchanged.
5. Keepalive: `ss -tno state established '( sport = :47291 )'` on the Linux hub shows `timer:(keepalive,...)` on agent connections.
