# PCTX proposals from TASK-048

## 2026-09-26 (implementer): hub domain, new invariant line

- TASK-048 (slots + buffer + stream): a text message for a slot with a live agent waits in `Slot.buffer` until no new one came for `Options::gather_quiet` (hub: `GATHER_QUIET` 1 s), at most `gather_max` (hub: `GATHER_MAX` 3 s) after the first; the waiting texts go as ONE `HubMsg::Inbound`: `Parked::content` of each in order joined by `buffer::PART_SEPARATOR`, meta `message_id` = last part, `message_ids` = all parts (only when >1), reply/target_agent from the last part that explicitly replies, `forwarded` only when all are forwards; 128 KiB of content per inbound at most. Every part gets 👀; all turn ✍ with the last part's channel record (`registry::Stream.parts`). A file or console command ends the burst (burst first, then the file/command). Messages kept while the slot had no live session go one by one on revival (TASK-017 unchanged). `Options::default()` has gathering off (ZERO): tests that need it set it.

Why: future work on inbound routing (e.g. TASK-019 headless resume, remote agents) must know that one inbound can stand for several Telegram messages and that `message_id` names only the last one.
