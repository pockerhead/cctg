# PCTX proposals from TASK-049 (implementer)

## 2026-09-25: hub domain, agent link heartbeat (new invariant)

Proposed text for `domains/hub.md` (Implemented list):

- TASK-049: the agent link has a heartbeat behind a capability: `Register.heartbeat` from the agent, `Registered.heartbeat` from the hub, both needed. Each side sends `{"v":1,"type":"ping"}` when it wrote nothing for `wire::HEARTBEAT_INTERVAL` (30 s) and drops the link when it read nothing for `wire::HEARTBEAT_TIMEOUT` (90 s); pings are never answered and never leave the link loop (agent `serve`, ingress `agent_session`). Tests pass short values through `LinkConfig.heartbeat` and `ingress::serve_agents_with`. Both ends set TCP keepalive (socket2, idle 30 s, interval 10 s) in `tls.rs` before TLS.

Why: a new link message type must be consumed in both loops before the explicit forward lists (TASK-016 lesson), and any future blocking work inside those loops longer than the timeout now drops the link. Worth knowing before touching them.
