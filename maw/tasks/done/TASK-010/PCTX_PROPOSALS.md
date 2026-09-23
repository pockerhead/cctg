# PCTX proposals — TASK-010

## 2026-09-23 (planner): hub, what TASK-010 adds to the "Implemented" lines

Proposed addition to `domains/hub.md` Invariants after the TASK-009 line, once TASK-010 is merged:
"TASK-010: wire contracts in `crates/cctg/src/wire.rs` (`VERSION = 1`, every agent-link line carries `v` and `type`; `hello{secret}` then `register` before anything is processed; `MAX_LINE` 1 MiB via `take`). Hub ingress in `hub/ingress.rs`: `serve_agents` (TCP, `CCTG_AGENT_LISTEN`, default `127.0.0.1:47291`) and `serve_hooks` (`POST /v1/hook`, Bearer secret, `CCTG_HOOK_LISTEN`, default `127.0.0.1:47292`), handing `AgentEvent` / `HookPost` to the hub over bounded mpsc. `CCTG_HUB_SECRET` (16+ visible ASCII) is required by `cctg hub`. Hook events are deduplicated by the hook-minted `event_id` (4096 ids / 10 min), never by payload fields. Agent reconnect (`agent::spawn`, equal-jitter backoff 250 ms..30 s) and the one-shot hook POST (`hook::post`, raw TCP, one overall timeout) are library pieces for TASK-012/013."

## 2026-09-23 (planner): hub risk lesson, serde errors quote input

`serde_json` error texts quote the offending value (`invalid type: string "..."`). On the agent link and hook endpoint that value can be the shared secret, so decode errors are mapped to fixed-text `WireError` variants and the serde error is dropped. `tests/ingress_logs.rs` guards this with markers.

## 2026-09-23 (planner): hub risk lesson, reset after an early answer

A server that answers (401, `rejected`) and closes while unread client bytes remain sends a TCP reset, and the client may lose the answer. Ingress shuts down its write side and drains up to 64 KiB for at most 250 ms before closing ("lingering close").

## 2026-09-23 (plan-reviewer-2): correction to "reset after an early answer"

The 64 KiB drain in the planner entry above is wrong: on Windows a wrong-bearer hook POST with a 1 MiB body lost its 401 to a reset (reproduced; mutation R2 in `scratch/reviewer2/mutations.out.txt`). The drain must cover the largest in-contract request, `MAX_HEAD + MAX_HOOK_BODY`, still read through a fixed 4 KiB buffer and capped at 250 ms. Proposed lesson text: "An early HTTP answer survives only if the drain before close covers the whole allowed request; size the lingering close from the request limits, not a round number."

## 2026-09-23 (plan-reviewer-2): hub risk lesson, hand-rolled HTTP subset

For the hand-rolled hook endpoint, "strict" has to mean the RFC grammar, not a prefix check: exactly `HTTP/1.1`, field names only `tchar`, field values with no CR/LF/NUL/other CTL except HTAB (RFC 9110 5.5 MUST), one all-digit `Content-Length`, no `Transfer-Encoding`. The client side is equally strict: only a complete `HTTP/1.1 NNN ...\r\n` status line counts, since `Ok` means "the hub has the event".

## 2026-09-23 (qa): hub risk lesson, ingress backpressure becomes link loss after 5 s

Both ends of the agent link cap every write at 5 s and tear the link down on timeout. The hub connection loop awaits `events.send(AgentEvent::Message)`; if the hub-side consumer stops draining for more than ~5 s (for example while the Telegram outbound queue honours a long `retry_after`), the hub's reader stops, the agent's write times out, the agent reconnects, and the one message whose write was cut is lost (QA probe `scratch/qa/probe/tests/link.rs::qa_stalled_hub_consumer_characterisation`: 199 of 200 delivered, one `Down`). Proposed lesson: "The consumer of `AgentEvent`/`HookPost` (TASK-011) must never block ingress; hand events to an unbounded or large queue, or to the registry, and do Telegram I/O elsewhere."

> RESOLVED: folded into domains/hub.md on 2026-09-23 (with the plan-reviewer-2 correction replacing the 64 KiB drain lesson).
