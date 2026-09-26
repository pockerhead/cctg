# PCTX proposals from TASK-048

## 2026-09-26 (implementer): hub domain, new invariant line

- TASK-048 (slots + buffer + stream): a text message for a slot with a live agent waits in `Slot.buffer` until no new one came for `Options::gather_quiet` (hub: `GATHER_QUIET` 1 s), at most `gather_max` (hub: `GATHER_MAX` 3 s) after the first; the waiting texts go as ONE `HubMsg::Inbound`: `Parked::content` of each in order joined by `buffer::PART_SEPARATOR`, meta `message_id` = last part, `message_ids` = all parts (only when >1), reply/target_agent from the last part that explicitly replies, `forwarded` only when all are forwards; 128 KiB of content per inbound at most. Every part gets 👀; all turn ✍ with the last part's channel record (`registry::Stream.parts`). A file or console command ends the burst (burst first, then the file/command). Messages kept while the slot had no live session go one by one on revival (TASK-017 unchanged). `Options::default()` has gathering off (ZERO): tests that need it set it.

Why: future work on inbound routing (e.g. TASK-019 headless resume, remote agents) must know that one inbound can stand for several Telegram messages and that `message_id` names only the last one.

## 2026-09-26 (fixer): amendments to the TASK-048 hub line above

- A burst is cut where the explicit reply changes (`(thread_id, reply_to)` of its first part): one inbound has one addressee (the session or one subagent via `target_agent`) and one `reply_to_message_id`; its meta is `inbound_meta` of its last part.
- A text left in the slot by a full link queue before a burst began goes alone at once (`Gather.start` = first part's message id), not with the burst.
- A console command is refused with `BUSY_NOTICE` while the slot keeps messages or topic texts went to the session less than `Options::inbound_settle` ago (hub: `INBOUND_SETTLE` 3 s; default ZERO): the turn a channel message starts is invisible to the hub at first (UserPromptSubmit for channel messages unverified).

Why: the first line said "reply/target_agent from the last part that explicitly replies" and "burst first, then command", both changed by the review fix.
