# Premise challenge — TASK-032

## 1. Counter-example tested

The premise says non-text messages (document/photo/video/voice) already reach the hub and are answered by a "text only" stub, and that the agent link can carry the new traffic as a capability-gated message in chunks under `wire::MAX_LINE` without a VERSION bump. Counter-example: the update parser drops any message without `text` before it reaches slots (so there is no stub to replace and media never arrives), or the ingress/wire layer has no capability mechanism that an additive `AgentMsg`/`HubMsg` can ride, so the "no VERSION bump" success predicate cannot be met as framed.

## 2. Primary-source investigation

- `crates/cctg/src/hub/updates.rs:114-155` (`classify`): any `message` from the configured chat that is not a service message and whose sender is allowlisted becomes `Routed::Input(Inbound { text: message.text, .. })`. `text` is an `Option`; a photo/document message (text `None`, caption present) is not filtered out here. `Ignored::Unsupported` (`updates.rs:178`) applies only to update kinds other than `message`/`callback_query`.
- `crates/cctg/src/hub/slots.rs:1496-1498` (`on_topic_message`): `let Some(text) = input.text else { self.notify(slot, thread_id, TEXT_ONLY_NOTICE); return; };`
- `crates/cctg/src/hub/slots.rs:158`: `TEXT_ONLY_NOTICE = "В сессию пока доходят только текстовые сообщения."` — the stub the task says goes away.
- `crates/cctg/src/wire.rs:13-20`: module doc states a new message type keeps `VERSION` when a peer sends it only after the other side announced it in a `Register` field; existing precedents `verdict_ack` (`wire.rs:133`), `transcript_reads` (`wire.rs:137`), `console_keys`, `console_commands`. `VERSION = 1` (`wire.rs:36`), `MAX_LINE = 1 << 20` (`wire.rs:38`).
- `crates/cctg/src/hub/api.rs:159-164, 279-303`: `Document` + `send_document` (multipart, 120 s timeout) already exist; `sendPhoto`/`getFile` are not there, which is exactly the gap the task names.

## 3. Did it hold

No. Media messages from allowlisted users do reach slots today and get the text-only stub (`slots.rs:1496-1498`); the stub exists and is the thing to replace. The wire layer has an established capability-in-`Register` mechanism for additive message types without a VERSION bump (`wire.rs:13-20`), with four prior uses, so "capability, chunks under MAX_LINE, no VERSION bump" is achievable as framed. The outbound half has a real existing base (`send_document`) and a real gap (no `sendPhoto`, no `getFile`). I found no primary-source evidence that the problem, the assumed current behavior, or the acceptance criteria are mis-framed.

## 4. Verdict

PREMISE HOLDS — `crates/cctg/src/hub/slots.rs:1496-1498` shows non-text topic messages already reach slots and receive `TEXT_ONLY_NOTICE` (`slots.rs:158`), and `crates/cctg/src/wire.rs:13-20` documents (with `verdict_ack`/`transcript_reads`/`console_*` precedents at `wire.rs:133,137`) that a capability-gated new message type keeps `VERSION = 1`; the counter-example (media dropped before slots, or no capability path) is false.
