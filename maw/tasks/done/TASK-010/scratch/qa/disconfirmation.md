# QA disconfirmation (written before running any new probe)

Counter-example that would make the fix wrong: a sustained stream of large lines
(256 KiB each, 64 per side) written by the hub (to_agent) and by the agent (outbox)
at the same time, while both consumers drain promptly. FIX_SUMMARY claims the reader
tasks keep draining input during outbound writes, so there is no deadlock. If a main
loop that is blocked in a write stops the other side from ever finishing its own
write, the 5 s write timeout fires, the link drops (LinkEvent::Down /
AgentEvent::Disconnected) and messages are lost. Expected (claim): all 128 messages
arrive in order, no Down, well under 5 s.

Second probe: a peer that registers and then never reads. The hub must drop the
link after WRITE_TIMEOUT (5 s) instead of hanging forever, and the agent symmetric.
