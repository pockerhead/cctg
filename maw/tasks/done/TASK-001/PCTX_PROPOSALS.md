# Project-context proposals — TASK-001 (planner, 2026-09-22)

Each item contradicts or extends the current project context. Sources are primary. Not applied anywhere.

1. `transcript` domain / CLAUDE.md "Субагенты": the subagent transcript path does not have to be constructed.
   `SubagentStop` delivers `agent_transcript_path` directly. Source: https://code.claude.com/docs/en/hooks
> RESOLVED: folded into domains/hooks.md + domains/transcript.md on 2026-09-22
2. `hooks` domain: `SubagentStop.last_assistant_message` is NOT the subagent's report when the subagent uses
   the `SubagentHandback` tool (Claude Code v2.1.271+) — the report is that tool call's `message` input, and
   `last_assistant_message` holds only trailing closing text. Every MAW agent uses SubagentHandback, so this
   is our normal case, not an edge case. Source: https://code.claude.com/docs/en/hooks
> RESOLVED: folded into domains/hooks.md on 2026-09-22
3. `hooks` / `hub` domains: `SubagentStop` also fires for Claude Code's own internal agents (prompt
   suggestions, `/btw`) with `agent_type` empty or equal to the session's `--agent`. Events with an empty
   `agent_type` must be dropped or the parent topic collects ghost `↳` blocks. Source: same.
> RESOLVED: folded into domains/hooks.md on 2026-09-22
4. `transcript` / `hub` domains: `transcript_path` is written asynchronously and may lag the in-memory
   conversation, so reading the jsonl on `Stop` can miss the turn that just ended. Use `last_assistant_message`
   for the current turn. Source: same.
> RESOLVED: folded into domains/transcript.md + domains/hooks.md on 2026-09-22
5. `hub` domain: "topic icon by state" is only partly possible. `editForumTopic` accepts `name` and
   `icon_custom_emoji_id` only — `icon_color` is fixed at creation.
   Source: https://core.telegram.org/bots/api#editforumtopic
> RESOLVED: folded into domains/hub.md (risk lessons) on 2026-09-22
6. `hub` domain: the binding Telegram constraint for a one-forum design is 20 messages per minute per group,
   shared by all topics of all sessions on all devices; also ~1 msg/s per chat and ~30 req/s overall, with
   `retry_after` on 429. Source: https://core.telegram.org/bots/faq ,
   https://core.telegram.org/bots/api#responseparameters
> RESOLVED: folded into domains/hub.md on 2026-09-22
7. `transcript` domain: the list of record types to ignore is incomplete — a real transcript on this machine
   (245 records) also contains `attachment` (86), `atis-latch`, `queue-operation`, `file-history-delta`.
   The rule should be an allowlist of `user`/`assistant`, not a denylist. Source: local probe of
   `~/.claude/projects/C--Users-user-dev-cctg/1f2c01a2-....jsonl`.
> RESOLVED: folded into domains/transcript.md on 2026-09-22
8. `channel` / `hooks` domains: `CLAUDE_CODE_SESSION_ID` is observably present in child processes but is not
   documented as a hook environment variable (documented additions are CLAUDE_PROJECT_DIR, CLAUDE_PLUGIN_ROOT,
   CLAUDE_PLUGIN_DATA, CLAUDE_CODE_REMOTE, CLAUDE_CODE_BRIDGE_SESSION_ID, CLAUDE_EFFORT). Treat it as
   unsupported: one detection function, one fallback. Source: https://code.claude.com/docs/en/hooks
> RESOLVED: folded into domains/hooks.md on 2026-09-22
9. `channel` domain, worth recording: registering `cctg` at user scope (`claude mcp add --scope user`,
   top-level `mcpServers` in `~/.claude.json`) avoids the per-project `.mcp.json` consent dialog entirely.
   Source: https://code.claude.com/docs/en/mcp , corroborated by this machine's `~/.claude.json` (4 user-scope
   servers, empty `enabledMcpjsonServers` across all 57 projects).
> RESOLVED: folded into domains/channel.md on 2026-09-22

## 2026-09-22, plan-reviewer-2 (TASK-001)

10. `hub` domain: the line "Telegram: `teloxide` with long polling ... fallback is bare `reqwest`" should be
    inverted. Evidence: teloxide's last release is 0.17.0 (2025-07-11) covering Bot API 9.1, master's
    CHANGELOG caps at 9.2, while the live Bot API is 10.3 (2026-08-24). The project needs 11 long-stable
    methods and its own outbound scheduler regardless, and the universal invariant already says "never model
    the whole ... Bot API". Proposal: primary = bare `reqwest` with narrow `#[serde(default)]` structs,
    named fallback = `frankenstein` (0.52.0, 2026-08-28, Bot API 10.3). Deviation sanctioned by the user's
    resolved decision (5) in TASK_FINAL.md. Sources: crates.io teloxide/frankenstein,
    https://github.com/teloxide/teloxide/blob/master/CHANGELOG.md , https://core.telegram.org/bots/api
> RESOLVED: folded into domains/hub.md + agents/implementer,fixer,code-reviewer.md on 2026-09-22
11. `hooks` domain: `source` is documented only for `SessionStart` (it is the matcher value). CLAUDE.md and
    the hooks domain attribute it to `SessionStart`/`SessionEnd`/`Stop`/`UserPromptSubmit` alike. Code must
    not require it outside SessionStart. Source: https://code.claude.com/docs/en/hooks
> RESOLVED: folded into domains/hooks.md on 2026-09-22
12. `hooks` domain, missing constraint with teeth: `SessionEnd` hooks share a 1.5-second budget; other hooks
    are far more generous (`UserPromptSubmit` 30 s, `Stop` and command hooks 600 s). The hook's POST timeout
    must be set under that budget or session-death events are lost. Source: same.
> RESOLVED: folded into domains/hooks.md on 2026-09-22
13. `transcript` / `hooks` domains: the subagent report source has no documented contract. There is no
    `agent_transcript_path` field in the hook schema, no documented statement that the subagent transcript
    file lags at SubagentStop, and no documented recipe for capturing `SubagentHandback` via a tool matcher.
    Documented: `SubagentStop` carries `last_assistant_message`; `SubagentHandback` exists as a tool from
    Claude Code 2.1.271. The hub must build the subagent transcript path itself from cwd + session_id +
    agent_id and degrade through a fallback chain. Source: https://code.claude.com/docs/en/hooks ,
    https://code.claude.com/docs/en/tools-reference
> REJECTED 2026-09-22: direct fetch of hooks.md shows agent_transcript_path, the lag warning, the SubagentHandback tool_input.message recipe and the internal-agent agent_type rule verbatim. See PLAN_FINAL.md section 0.1.
14. `hub` domain, operational gap: every `editForumTopic` produces a `forum_topic_edited` service message the
    hub must delete via `deleteMessage` (needs `can_delete_messages`), while `forum_topic_created` cannot be
    deleted. Since state is shown by icon changes, without this the topic fills with service noise. Already
    verified in CLAUDE.md "Telegram Bot API" but absent from the hub domain invariants.
> RESOLVED: folded into domains/hub.md (risk lessons) on 2026-09-22
