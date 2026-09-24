# Disconfirmation case

Concrete counter-example tested: a buffered Telegram message is accepted by the
hub-to-agent queue and reaches the agent, then the hub process stops before the
registry snapshot that removes the message is durably saved. If the persisted
registry still contains that message and neither the wire protocol nor the agent
deduplicates it, the next hub process flushes it again. That would violate the
acceptance criterion that the buffer is not duplicated after recovery.

Status: held. In `scratch/planner/ws/crates/cctg/src/hub/slots.rs`, `flush`
calls `try_send` and immediately `pop_front` (lines 1107-1123), while `pump`
only publishes an asynchronous save snapshot afterward (lines 2694-2698).
`HubMsg::Inbound` has no delivery id/ack and the agent only deduplicates
permission verdict ids. Therefore a process loss after agent receipt but before
the durable save can redeliver the same Telegram message after restart. The
reference restart test covers a saved pending buffer followed by normal
delivery; it does not inject this crash point. The orchestrator explicitly
accepts at-least-once across that crash window, so the revised plan must state
the narrowed guarantee instead of claiming crash-safe exactly-once.
