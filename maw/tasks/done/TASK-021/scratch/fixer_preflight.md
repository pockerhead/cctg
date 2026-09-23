# Fixer preflight disconfirmation claim

The most concrete risky reading of the review is to gate `on_reply` against the
session originally registered by a connection. Implemented literally, that
would drop a valid reply after `/clear`, because Claude keeps the channel MCP
process while the session id changes. Before accepting the proposed helper, the
cited `slots.rs` must show that pid-based rebinding updates the connection's
session mapping first, so the live/current checks apply to the new session and
not permanently to the old one.
