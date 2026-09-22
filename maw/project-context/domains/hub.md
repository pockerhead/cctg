# Domain: hub
# NORMATIVE when active — a constraint to satisfy, not a claim for you to audit.

## Invariants
- One hub process per main device. It owns the bot token; the bot is admin in one private supergroup with forum topics enabled.
- A forum topic is a slot `(device, folder, ordinal)`. A new session occupies the first slot of its folder with no live session; a concurrent session gets the next ordinal (`[host] folder #2`). Title `[host] folder · ai-title` (short session id until ai-title appears). Icon via `icon_custom_emoji_id` reflects state: alive / dead / waiting for permission / no channel. Session change inside a slot is rendered as a separator line, never as a per-message prefix. Resume (`claude -p --resume <id>` via the device agent) and handoff (summary from the old session as first prompt of a new one) both stay in the same slot. The `General` topic is dashboard plus commands.
- Subagents and nested `claude -p` runs get NO topic. They render inside the parent topic (`↳ <type> <id>` collapsed block for subagents, `⇣ nested <id>` for nested runs). Nested runs are recognised by the nesting flag sent by the hook.
- Registry: `slot -> (device, folder, ordinal, topic_id, current_session_id?, state)` plus `session_id -> (slot, transcript_path, parent_session_id?)`. Persisted in `registry.json`, reconciled with live agent connections on start.
- Agent transport: TCP, newline-delimited JSON, first message is shared-secret auth. No WebSocket until a browser client exists.
- Telegram client: bare `reqwest` (rustls) behind one `BotApi` module with narrow `#[serde(default)]` structs for the ~11 methods we use (getUpdates long polling, sendMessage, sendDocument, editMessageText, deleteMessage, answerCallbackQuery, createForumTopic, editForumTopic, getForumTopicIconStickers, getChatMember, getMe). Named fallback if typed models start paying for themselves: `frankenstein` (tracks Bot API 10.x). `teloxide` is not used (last release 2025-07, Bot API 9.1; its dispatcher and Throttle are redundant with our registry routing and outbound queue). Decided 2026-09-22.
- Flood control: documented limits are ~1 message/s per chat and 20 messages/min per group, shared by every topic of every session on every device. All sends go through one outbound queue: token bucket, FIFO per topic, permission requests first, edits coalesced, 429 honoured via `retry_after`. Edits and topic mutations have no published numeric limit; serialize them and handle 429.
- Security gate is by `from.id` allowlist, never by chat. Permission Allow/Deny buttons carry `request_id` in callback data and are honoured only from allowlisted users.
- Inbound routing: message in topic goes to the current session of that slot. Dead session: never close the topic; buffer up to 50 messages (drop oldest, warn once), offer a Resume button.
- Local transcripts are read directly from `transcript_path` (works for dead sessions); remote devices serve the file through their agent on request.
- Messages longer than 4096 are split or sent as a file; measure with `transcript::telegram_len` and split with `transcript::split_for_telegram` (see transcript domain).

## Risk lessons
- 2026-09-22: bot needs admin right `can_manage_topics` for createForumTopic; check via `getChatMember` at hub start. Bot API chat id is `-100` + web-client id. Bot-admin CAN post into a closed topic, users cannot: never close a dead session's topic. Service messages `forum_topic_*` arrive as `message` updates and must be ignored. Text limit is exactly 4096. `icon_custom_emoji_id` is editable, `icon_color` is not. Service messages for rename/icon change are deleted by the hub via `deleteMessage` (message_id from getUpdates); the `forum_topic_created` one cannot be deleted (it is the topic root). See CLAUDE.md "Telegram Bot API".

## Pointers
- `CLAUDE.md` (repo root), section "Архитектура / 1. cctg hub".
- https://github.com/robertelee78/claude-telegram-mirror : reference for daemon/hook/cli single binary and topic-per-session layout (Linux/tmux, organisation only).
