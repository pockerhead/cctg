# Disconfirmation case

Concrete counter-example: after a session reconnects and supersedes its old agent
connection, the stale connection sends `AgentMsg::Reply`. The implementation is
wrong if that reply is accepted and delivered to the slot topic. Verify in the
actual `slots.rs` code and tests that reply routing requires the session's current
bound connection to equal the sender connection.
