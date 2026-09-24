# Disconfirmation (plan-reviewer-2, written before evaluation)

Counterexample to test: the plan (V1 and V2) deletes the "bot pinned a message"
service notice when it arrives in getUpdates as `pinned_message`. If the bot's
OWN pinChatMessage never produces an update for the bot (Telegram does not
deliver a bot's own messages), the acceptance item "служебное «закрепил»
удаляется" cannot be met by the update path at all, and V2's
`sender_is_bot` filter guards a path that never fires for the bot.

Where to look: the existing forum_topic_edited deletion (CLAUDE.md says the
bot's own editForumTopic service message DOES arrive), updates.rs
classification of service messages, and public reports on bot-authored
pinned_message updates.

## Result

Not reproduced, not refuted offline. The only verified analogue is in
CLAUDE.md: the bot's OWN editForumTopic produces a forum_topic_edited
message in getUpdates, which the hub already deletes. Bot API docs describe
pinned_message as an ordinary service Message; nothing says the bot's own pin
is filtered out. No live call was allowed (no real Telegram API). The design
degrades safely: if the notice never arrives, one "pinned" notice stays per
slot (pin happens once per slot). Kept as a live-check item for the
orchestrator. The review also found the opposite hole: a human's pin notice
of the status message WAS deleted (fixed: sender must be getMe.id).
