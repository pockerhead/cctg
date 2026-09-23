# QA disconfirmation case (TASK-021)

Counter-example: after `/clear` (SessionEnd reason=clear + SessionStart source=clear
with the same claude_pid), the channel connection keeps the OLD session id. If the
hub's reply gate (`live_reply_slot`) or inbound gate (`live_agent`) looked at the
connection's original session, then (a) a topic message in that slot would get
the offline notice instead of reaching Claude, and (b) Claude's reply would be
dropped. Same for an agent that re-registers after a hub-link drop with the old
env session id. Tested end to end through `Slots::run` (qa/e2e, steps 7-8).
