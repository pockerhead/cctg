# PCTX proposals — TASK-016 (planner)

## 2026-09-24 — CLAUDE.md / transcript domain: when records reach the jsonl (measured)

"Transcript files are written asynchronously and may lag" is too vague for planning. Measured on Claude Code 2.1.281 (`scratch/planner/lag_probe.*`, `passive_lag.*`): a record is visible 0.1-0.3 s after its `timestamp` (`claude -p` main transcript, and a live interactive subagent transcript). Exception: in an interactive main transcript the assistant record of a tool call (its text and `tool_use`) is written only when the tool finishes, together with the `tool_result` (4.6-6.6 s after its timestamp for a 3 s command with the auto-mode classifier). So a finished tool call is visible ~0.1-0.2 s after it ends; a tool call in progress is not visible at all. Proposed line for the transcript domain and CLAUDE.md "Транскрипты сессий".

## 2026-09-24 — transcript domain: channel messages in the transcript

Observed in real transcripts: a channel message is `queue-operation` enqueue (content = the `<channel ...>` tag + text), then dequeue, then a `user` record with `isMeta: true` when Claude takes it. Prompts queued during a busy turn otherwise become `attachment` records of type `queued_command` (`prompt`, `origin.kind`); a channel message delivered mid-turn has not been observed yet. TASK-016 treats both shapes as "taken into work" (`transcript::stream_events` -> `StreamEvent::Channel`).

## 2026-09-24 — hub domain: Bot API method list and reactions

The hub now also calls `setMessageReaction` (one emoji, `👀` and `✍` without U+FE0F are in the Bot API 10.3 `ReactionTypeEmoji` list; bots set at most one reaction per message; it runs on the scheduler's unmetered edit lane, coalesced per message). Proposed: add it to the "~11 methods" list.

## 2026-09-24 — hub/channel domain: TASK-016 implemented line (to fold after QA)

Proposed: "TASK-016 (`tail.rs` in the agent, `hub/stream.rs` + slots + scheduler, `transcript::stream_events`): an agent that registers with `transcript_reads` answers `transcript_read{session_id, path, from}` with one `transcript_chunk` of complete lines only (path must be `.../projects/<p>/<session_id>.jsonl`; `from: None` = end of file). The hub owns `registry.sessions[..].stream` (offset = bytes whose messages Telegram answered, pending tool calls, 👀 receipts) and polls every 300 ms, one read in flight, ≤64 unanswered messages per session. Topic gets `> prompt` for terminal prompts, text before tool calls, `<brief line> ✓|✗ error` per finished tool call; the final answer stays the Stop hook's and waits ≤1.5 s for the lines read after the Stop. Stream lines ride `Op::Stream` (metered; permission prompts overtake them; `merge` lines join when more messages wait than tokens). A message handed to an agent gets 👀, and ✍ only when that session's transcript shows its `<channel ... message_id=N>`. A new transcript (startup/clear) streams from byte 0, any other first start from its end."

## 2026-09-24 (plan-reviewer-2) — transcript domain: `is_error` is reliable for built-in tool errors

The domain says "`is_error` is often absent". Measured on the 400 newest local transcripts (9,661 `tool_result` blocks): every `<tool_use_error>` result (10) and every result whose top-level `toolUseResult` is a string starting `Error:` or `User rejected` (313) carries `is_error: true`. `is_error` is absent only on `tool_reference` results (ToolSearch) and on MCP tool results, whose text may say "Error" without the call having failed in protocol terms. Proposed wording: "`is_error: true` marks every failed built-in tool call; it is absent on ToolSearch and MCP results, whose text must not be read as failure."

## 2026-09-24 (plan-reviewer-2) — transcript domain: the end of a turn and other channel servers

Every record of one API response carries the same `message.stop_reason`, the `thinking` record included (a final response is `thinking` + `text`, both `end_turn`). An end-of-turn marker must therefore come from the record with a text block, or it counts twice. Other channel servers appear in real transcripts (`plugin:fakechat:fakechat`, `webhook`, probe servers), with numeric `message_id` attributes of their own: correlation of a Telegram message with its channel record must check `source="cctg"`.

## 2026-09-24 — hub domain (QA): stream/agent-message tests must pass through serve_agents

TASK-016 QA: every slots-level test and `tests/stream_logs.rs` injected `AgentEvent::Message` straight into `Slots`, so nobody noticed that `hub/ingress.rs` forwards only an explicit allowlist of `AgentMsg` variants after registration and drops any new one as "agent repeated its handshake". Proposed risk lesson: a new `AgentMsg` variant needs (1) its arm in the `serve_agents` forward list and (2) at least one test that sends the frame over a real TCP link to `serve_agents` (see `scratch/qa/e2e`).

> RESOLVED: all folded on 2026-09-24: lag, channel shapes, is_error, end-of-turn and other channel servers into domains/transcript.md (lag also into CLAUDE.md); setMessageReaction and the implemented line (updated to the shipped design) plus the serve_agents lesson into domains/hub.md.
