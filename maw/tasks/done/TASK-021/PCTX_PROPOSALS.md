# PCTX proposals from TASK-021 (planner)

## 2026-09-23 — hub domain: implicit reply in forum topics

Add to Risk lessons: in a forum topic Telegram sets `reply_to_message` on every message to the topic root (`message_id == message_thread_id`, the `forum_topic_created` message). Only a `reply_to_message` with another id is an explicit reply. Source: NousResearch/hermes-agent issue #118678; covered by `updates::tests::only_an_explicit_reply_is_a_reply`.

## 2026-09-23 — channel domain: hooks from `--settings`

Add to Risk lessons (observed, Claude Code 2.1.280): a `SessionStart` hook defined in a file passed with `claude --settings <file>` fires (probe: `maw/tasks/in_progress/TASK-021/scratch/planner/probe/cmd.txt`), although the CLI reference says hooks are not loaded from `--settings`. Live probes can carry both the MCP server (`--mcp-config`) and the hooks (`--settings`) in temporary files; `.claude/settings.local.json` of the fixed probe folder is the fallback if a later version stops honouring it.

## 2026-09-23 — hub domain: topic message routing (after TASK-021 merges)

Add to the hub Invariants after the TASK-011 bullet: TASK-021 routes allowlisted non-command topic messages through `Control::Message` to the slot actor, which forwards them with `try_send` to the agent of the slot's live top-level session (meta `chat_id`, `message_id`, `thread_id`, optional `reply_to_message_id`) or answers with one fixed notice; General and non-slot topics are ignored. Agent replies go to the session's slot topic through the actor's dispatch task (split chunks or one document), at most `MAX_QUEUED_MESSAGES` = 256 messages waiting for Telegram.

## 2026-09-23 — channel domain: isolate live probes with `CLAUDE_CONFIG_DIR` (plan-reviewer-2)

Add to Risk lessons: `CLAUDE_CONFIG_DIR=<dir>` moves the whole Claude Code config, including `.claude.json` (observed on 2.1.280: `CLAUDE_CONFIG_DIR=<scratch> claude mcp list` created `<scratch>/.claude.json` and saw none of the user-scope servers; the docs at https://code.claude.com/docs/en/claude-directory only promise that every `~/.claude` path moves). A live probe run with it leaves no `projects[<dir>]` key and no transcript in the user's real config. Cost: a fresh login (claude.ai `/login` or `ANTHROPIC_API_KEY`) kept in that dir, and no user settings, memory or plugins in the probe session. Evidence: `maw/tasks/in_progress/TASK-021/scratch/reviewer2/cfgprobe/probe.txt`.

## 2026-09-23 — hub domain: notice rate limit (after TASK-021 merges)

Amend the TASK-021 routing bullet: a slot gets each fixed notice (`OFFLINE_NOTICE`, `TEXT_ONLY_NOTICE`) at most once per `slots::Options::notice_every` (60 s); a message delivered to the agent re-arms the offline notice at once.

> RESOLVED: all five folded into domains/hub.md and domains/channel.md on 2026-09-23 (routing line includes the orchestrator post-QA fixes).
