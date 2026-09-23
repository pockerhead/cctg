# PCTX proposals (TASK-014)

## 2026-09-23 (plan-reviewer-2): wire compatibility rule for gated message types

Domain: hub / channel (wire.rs). Current rule in `wire.rs` and TASK-010 context: "a new message type bumps VERSION". Agents live as long as their Claude Code session and survive a hub upgrade, and `check_version` rejects any other version, so a bump cuts every running agent off. Proposed rule: a new optional field keeps VERSION, and so does a new message type that a peer sends only after the other side announced it in such a field (TASK-014: `Register.verdict_ack` gates `permission_ack`; `PermissionVerdict.verdict_id` is skipped when absent so the legacy line is byte-identical).

## 2026-09-23 (plan-reviewer-2): verdict delivery

Domain: channel. Proposed invariant after TASK-014 merges: a hub->agent `try_send` is not delivery. Verdicts carry `verdict_id` for agents with `verdict_ack`; the hub re-sends the same id until `permission_ack`; the agent keeps a 256-id cache across reconnects and passes each id to Claude Code once. Older agents: hand-off counts as delivery.

> RESOLVED: both folded into domains/hub.md and domains/channel.md on 2026-09-23.
