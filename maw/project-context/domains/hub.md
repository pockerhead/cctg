# Domain: hub
# NORMATIVE when active — a constraint to satisfy, not a claim for you to audit.

## Invariants
- One hub process per main device. It owns the bot token; the bot is admin in one private supergroup with forum topics enabled.
- One forum topic per Claude Code session. Title: `[host] folder · ai-title` (short session id until ai-title appears). Topic icon reflects state: alive / dead / waiting for permission. Resuming a known session reuses its topic. The `General` topic is dashboard plus commands.
- Subagents and nested `claude -p` runs get NO topic. They render inside the parent topic (`↳ <type> <id>` collapsed block for subagents, `⇣ nested <id>` for nested runs). Nested runs are recognised by the nesting flag sent by the hook.
- Registry: `session_id -> (device, folder, topic_id, alive, transcript_path, parent_session_id?)` plus index `(device, folder) -> [session_id]`. Persisted in `registry.json`, reconciled with live agent connections on start.
- Agent transport: TCP, newline-delimited JSON, first message is shared-secret auth. No WebSocket until a browser client exists.
- Telegram: `teloxide` with long polling; `create_forum_topic`, `edit_forum_topic`, inline keyboards, `message_thread_id`. If teloxide proves too heavy, fallback is bare `reqwest` to Bot API. Decide with evidence, not taste.
- Security gate is by `from.id` allowlist, never by chat. Permission Allow/Deny buttons carry `request_id` in callback data and are honoured only from allowlisted users.
- Inbound routing: message in topic goes to the session of that topic. Dead session: buffer, offer headless resume (phase 2).
- Local transcripts are read directly from `transcript_path` (works for dead sessions); remote devices serve the file through their agent on request.
- Messages longer than 4096 chars are split or sent as a file (see transcript domain).

## Risk lessons
<!-- dated, one line each -->

## Pointers
- `CLAUDE.md` (repo root), section "Архитектура / 1. cctg hub".
- https://github.com/robertelee78/claude-telegram-mirror : reference for daemon/hook/cli single binary and topic-per-session layout (Linux/tmux, organisation only).
