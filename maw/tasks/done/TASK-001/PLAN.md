# TASK-001 — Research report: decomposing the cctg MVP into pipeline tasks

Mode: deep-research. This file is the planner artifact; the plan-review stage folds it into `PLAN_FINAL.md`
(the acceptance criterion "report is written to PLAN_FINAL.md" is satisfied at the end of that stage — the
content below is already in `/maw-tasks` batch form, one block per task, and must be carried over verbatim).

Environment used for the local probes: Claude Code 2.1.278, Windows 11, `C:\Users\user\dev\cctg`.
Date of all web sources: 2026-09-22.

---

## 1. Understanding — what exists today

The repository contains no code. `rg --files` and the premise-challenge stage both confirm: no `Cargo.toml`,
no Rust sources, no tests. The whole project is currently three documents:

- `C:/Users/user/dev/cctg/CLAUDE.md` — architecture and verified platform facts (channels, jsonl, hooks).
- `C:/Users/user/dev/cctg/maw/project-context/domains/{transcript,channel,hub,hooks}.md` — normative domain law.
- `C:/Users/user/dev/cctg/maw/ROADMAP.md` — empty graph.

So "decompose the MVP" means: define the first 15 tasks of a greenfield Rust workspace, and clear as much
platform uncertainty as possible before an implementer touches a keyboard. That is what this report does.

Everything `CLAUDE.md` marks as verified was taken as given. Four places where primary sources **extend or
contradict** it are listed in section 5 and appended to `PCTX_PROPOSALS.md` — they are not silent edits.

---

## 2. Open questions from `CLAUDE.md` — answered or turned into spikes

### OQ-1. Does a nested `claude -p` overwrite `CLAUDE_CODE_SESSION_ID` for its own hooks and MCP servers?

**Not answerable from documentation → spike (TASK-003).**

What is confirmed: the inheritance half of the claim is real. Inside a Bash tool call of this very session the
environment contains:

```
CLAUDECODE=1
CLAUDE_CODE_SESSION_ID=1f2c01a2-63e9-464d-a70f-4a4283d3cd8b
CLAUDE_PID=15320
CLAUDE_CODE_CHILD_SESSION=1
CLAUDE_CODE_SESSION_ATTENDED=1
CLAUDE_CODE_MESSAGING_SOCKET=\\.\pipe\LOCAL\cc-msg-...
```

Note `CLAUDE_CODE_SESSION_ATTENDED=1` — undocumented, but if a headless `claude -p` sets it to `0` or drops it,
it is a second, cheaper nesting signal than the session-id comparison. The spike must record it either way.

What is **not** confirmed: the official hooks reference does not list `CLAUDE_CODE_SESSION_ID` among the
variables Claude Code adds to hook processes at all (the documented additions are `CLAUDE_PROJECT_DIR`,
`CLAUDE_PLUGIN_ROOT`, `CLAUDE_PLUGIN_DATA`, `CLAUDE_CODE_REMOTE`, `CLAUDE_CODE_BRIDGE_SESSION_ID`,
`CLAUDE_EFFORT`) — https://code.claude.com/docs/en/hooks. The variable is real but undocumented, therefore
unsupported and free to change. The nesting detection must not be the only mechanism; TASK-003 defines the
experiment and TASK-012 implements whichever of the two mechanisms wins, behind one function.

### OQ-2. Is `SubagentStop.last_assistant_message` enough, or must we always read `subagents/*.jsonl`?

**Answered, and the answer is "neither, exactly".** Three facts from https://code.claude.com/docs/en/hooks:

1. `SubagentStop` carries `agent_transcript_path` — the subagent's own transcript path, handed to us. We do
   **not** need to build `<session-id>/subagents/agent-<agent_id>.jsonl` by hand (`CLAUDE.md` says we do).
2. On Claude Code v2.1.271+, a subagent that reports through the `SubagentHandback` tool puts its report in
   that tool call's `message` input; `last_assistant_message` then holds only the closing text, which is *not*
   the report. Every MAW pipeline agent uses `SubagentHandback` — so for exactly our own use case
   `last_assistant_message` is the wrong field.
3. `SubagentStop` also fires for internal agents Claude Code runs for itself (prompt suggestions, `/btw`), with
   `agent_type` equal to the session's `--agent` value or an empty string. A hub that renders every
   `SubagentStop` as `↳ <type> <id>` will emit spurious blocks into the topic.

Design conclusion for TASK-007/TASK-015: render the subagent block from `agent_transcript_path` via the
`transcript` crate; use `last_assistant_message` only as a fallback when that file is missing or empty; drop
events with an empty `agent_type`.

Corroborating local evidence: `~/.claude/projects/<enc>/<sid>/subagents/agent-<id>.meta.json` exists and
contains `{"agentType","description","toolUseId","spawnDepth","model"}` — `spawnDepth` is a ready-made nesting
depth and `description` is a ready-made one-line title for the collapsed block.

### OQ-3. How does `--dangerously-load-development-channels` behave with `claude --resume`?

**Not documented → spike (TASK-004).** The channels docs never mention resume. What they do state, and what
changes the shape of the problem:

- "Being in `.mcp.json` isn't enough to push messages: a server also has to be named in `--channels`."
  https://code.claude.com/docs/en/channels
- Custom servers are not on the Anthropic allowlist, so the dev flag is required every launch, on every
  session, forever during the research preview. https://code.claude.com/docs/en/channels-reference
- Registration is per session at startup; there is no runtime attach.

So the real risk is not resume semantics, it is that **a session started without the flag has no channel and
no way to get one**. The hub must treat "agent never connected" as a normal state (topic exists from the hook,
inbound is buffered, the topic says so) rather than an error. TASK-004 measures resume behaviour; TASK-011
implements the degraded state regardless of the outcome.

### OQ-4. Does `.mcp.json` need a consent dialog in every new folder?

**Answered: yes for project scope, no for user scope — so register `cctg` at user scope.**

- Project-scoped servers from `.mcp.json` require an approval prompt per project; approvals live in untracked
  settings; `claude mcp reset-project-choices` clears them. User-scoped servers (top-level `mcpServers` in
  `~/.claude.json`, added with `claude mcp add --scope user`) load in all projects with no approval.
  https://code.claude.com/docs/en/mcp
- The channels walkthrough says the same in passing: "The first time you start a session in this project,
  Claude Code also asks for consent before using the new server from `.mcp.json`."
  https://code.claude.com/docs/en/channels-reference
- Local corroboration: this machine's `~/.claude.json` has four top-level `mcpServers` that work everywhere,
  while `enabledMcpjsonServers` / `disabledMcpjsonServers` are empty for all 57 projects.

Decision: `cctg agent` is installed once with `claude mcp add --scope user`, absolute path to the binary. No
`.mcp.json` in user repos. TASK-013 ships that install step and documents it.

### OQ-5. Telegram limits on topic creation (mass session start)

**Answered for the binding constraints; the topic-creation flood limit itself is undocumented by design.**

| Constraint | Value | Source |
|---|---|---|
| Message text | 1–4096 chars after entity parsing | https://core.telegram.org/bots/api#sendmessage |
| Topic name | 1–128 chars (`createForumTopic.name`) | https://core.telegram.org/bots/api#createforumtopic |
| `editForumTopic` | takes `name` (0–128) and `icon_custom_emoji_id` only — **no `icon_color`** | https://core.telegram.org/bots/api#editforumtopic |
| `callback_data` | 1–64 **bytes** | https://core.telegram.org/bots/api#inlinekeyboardbutton |
| Flood control response | HTTP 429 with `ResponseParameters.retry_after` seconds | https://core.telegram.org/bots/api#responseparameters |
| Same chat | ~1 message/second | https://core.telegram.org/bots/faq |
| **Same group** | **20 messages/minute** | https://core.telegram.org/bots/faq |
| Bulk | ~30 messages/second overall | https://core.telegram.org/bots/faq |
| Topics per group | up to 1,000,000 | https://limits.tginfo.me/en |
| Document upload | 50 MB | https://core.telegram.org/bots/api#senddocument |

The number that actually shapes the architecture is **20 messages per minute per group**. A forum is one chat:
every topic of every session on every device shares that budget. Telegram does not document a per-topic
allowance, and none should be assumed. Consequences, which belong in the plan and not in a later post-mortem:

- Streaming a transcript live into a topic is not viable. Send on `Stop` (one message per turn), on demand
  (`/brief`, `/full`), and for permission prompts — nothing else.
- All outbound traffic goes through one queue with a token bucket per chat plus a global bucket, and a 429
  handler that honours `retry_after` exactly (TASK-008). Retrying without honouring it escalates the ban.
- Prefer `editMessageText` on a "live" message over new messages for progress — but note edits are API calls
  too and count against the ~30 req/s global budget, so coalesce edits (no more than one per few seconds).
- Topic creation at mass start (say ten sessions resuming after a reboot) must be serialized through the same
  queue. `createForumTopic` has no documented rate limit, which means it is subject to undocumented flood
  control; treat it like any other call and back off on 429.
- State indicators: since `editForumTopic` cannot change `icon_color`, the alive/dead/waiting state has to ride
  in the topic **name** (a leading glyph) or in a custom emoji id from `getForumTopicIconStickers`. The colour
  is fixed at creation. This contradicts `CLAUDE.md`'s "иконка темы по состоянию" as literally written.

---

## 3. Workspace layout and crate set — confirmed with two amendments

Confirmed as written in `CLAUDE.md` / project context: one cargo workspace, one binary `cctg` with subcommands
`hub` / `agent` / `hook`, a pure `transcript` library, no Node, hand-rolled JSON-RPC (no `rmcp`), newline-JSON
over TCP between hub and agent.

```
Cargo.toml                 # [workspace] members, shared [workspace.dependencies], resolver = "2"
crates/transcript/         # lib: parse + render_brief + render_full + chunking. No IO, no tokio.
crates/proto/              # lib: hub <-> agent wire types + framing + auth handshake.  (amendment 1)
crates/hub/                # lib: bot api client, registry, topics, routing, command handlers
crates/agent/              # lib: channel MCP server (stdio JSON-RPC), hub client
crates/hook/               # lib: hook event parsing, nesting detection, fire-and-forget POST
crates/cctg/               # bin: arg parsing + dispatch into hub/agent/hook. Thin.
```

**Amendment 1 — add `crates/proto`.** `hub` and `agent` both need the same wire structs. Without a shared
crate one has to depend on the other, which is worse than a 150-line crate of serde structs. This is not a
speculative layer; it is the minimum that keeps the dependency graph acyclic.

**Amendment 2 — the crate list in `CLAUDE.md` omits four dependencies that the code will need anyway.**
Naming them now prevents an implementer inventing something heavier:

| Crate | Why | Alternative rejected |
|---|---|---|
| `thiserror` | typed errors where a caller branches (Telegram 429 vs 400; JSON-RPC method-not-found) | `anyhow` everywhere — loses the branch |
| `clap` (derive) | three subcommands, each with flags; `cctg hook <event>` must be stable | hand-rolled `args().nth()` as hdcd does — cheaper to compile, worse to extend |
| `dotenvy` | `.env` is already the stated config mechanism | reading the file by hand |
| `tracing-subscriber` | `tracing` alone emits nothing; the agent must log to **stderr/file only**, stdout is the JSON-RPC transport | — |

Optional, decide at TASK-008: `governor` for the token bucket, or ~40 lines of `tokio::time` by hand. Given
that the bucket must also serialize a FIFO per chat and honour `retry_after`, hand-rolling is likely simpler
than bending `governor` to it. Not a blocker either way.

### teloxide vs bare reqwest — recommendation: **bare `reqwest` behind a small `BotApi` module**

Current state of the candidates, checked today:

| | teloxide | frankenstein | bare reqwest |
|---|---|---|---|
| Latest release | 0.17.0, **2025-07-11** (14 months ago) | 0.52.0, **2026-08-28** | n/a |
| Bot API covered | 9.1 released; 9.2 on unreleased master | **10.3** | whatever we write |
| Repo activity | commits Aug 2026, pushed Sep 2026, 4.2k★, 74 open issues | commits Aug 2026, 371★, 14 open issues | — |
| Recent downloads (90d) | ~446k | ~14k | — |
| Forum topics | `create_forum_topic` / `edit_forum_topic` payloads present | present (tracks current API) | ~30 lines per method |
| Extras | dispatcher (dptree), dialogue storage, **`Throttle` adaptor** | typed models only | none |

Sources: https://crates.io/crates/teloxide/versions, https://github.com/teloxide/teloxide/blob/master/CHANGELOG.md
(`## unreleased` → "Support for TBA 9.2"), https://crates.io/crates/frankenstein,
https://github.com/ayrat555/frankenstein (commit "telegram bot api 10.3", 2026-08-28),
https://docs.rs/teloxide/latest/teloxide/payloads/struct.CreateForumTopic.html,
https://docs.rs/teloxide/latest/teloxide/adaptors/index.html.

Forum-topic API coverage is not a differentiator: topics landed in Bot API 6.3 and all three options have them.
The real arguments:

- cctg needs roughly twelve methods: `getUpdates`, `sendMessage`, `sendDocument`, `editMessageText`,
  `answerCallbackQuery`, `createForumTopic`, `editForumTopic`, `closeForumTopic`, `reopenForumTopic`,
  `deleteForumTopic`, `setMyCommands`, `getMe`. The Apache-2.0 reference implementation hdcd-telegram does
  exactly this set in one 18 KB `src/telegram/api.rs` on raw `reqwest`, ships at 3.5 MB and ~5 MB RAM
  (https://github.com/gohyperdev/hdcd-telegram). That is the proof that the hand-rolled path is small.
- teloxide's headline value is the dispatcher and dialogue storage. cctg routes by
  `message_thread_id → registry → session`, which is our own table, not a dptree tree. We would pay for a
  framework and then not use its organizing idea.
- teloxide's one genuinely relevant feature is the `Throttle` adaptor. But we need our own outbound queue
  regardless — for buffering messages addressed to dead sessions, for coalescing edits, and for ordering
  within a topic. Once that queue exists, `Throttle` is redundant.
- Freshness: teloxide has not cut a release in 14 months while the Bot API moved from 9.1 to 10.3. The project
  is alive, not abandoned, but a pinned 0.17 means being a year behind the API surface and depending on the
  maintainers' release cadence for anything new. frankenstein is the opposite trade: current, thin, small
  community.

**Recommendation:** write `crates/hub/src/api.rs` on `reqwest` (rustls, `default-features = false`), all calls
behind one internal trait so the implementation is swappable, and keep `frankenstein` — not teloxide — as the
named fallback if typed models start paying for themselves. Revisit only on evidence: if `api.rs` passes ~600
lines or we find ourselves reimplementing update deserialization for more than the handful of update kinds we
actually consume (`message`, `callback_query`), switch to `frankenstein` for the types and keep our own
transport. This decision is cheap to reverse because the whole surface is one module.

---

## 4. Proposed tasks

Numbering continues from TASK-001. Each block below is in `/maw-tasks` intake form.

---

### TASK-002: Bootstrap cargo workspace and cctg CLI skeleton

Type: chore
Mode: small-fix
Priority: high
Branch: chore/bootstrap-workspace
Domains: transcript, hub, channel, hooks

**Scope.** Create the workspace described in section 3: root `Cargo.toml` with `resolver = "2"`, members
`transcript`, `proto`, `hub`, `agent`, `hook`, `cctg`, and a `[workspace.dependencies]` block pinning the
shared crates in one place. `crates/cctg` gets `clap` derive with three subcommands (`hub`, `agent`,
`hook <event>`) that currently print "not implemented" and exit 0, plus `tracing-subscriber` initialization
that writes to stderr only. `.gitignore` gains `target/`, `.cctg/`, `registry.json`, `.env`. No logic.
Everything else in this plan compiles into these slots, so the layout is the only thing that matters here.

- blocked by: nothing.

Acceptance criteria:
- [ ] `cargo build --workspace` and `cargo test --workspace` succeed from a clean checkout on Windows
- [ ] `cargo clippy --workspace --all-targets -- -D warnings` is clean
- [ ] `cctg --help` lists exactly `hub`, `agent`, `hook`; `cctg hook --help` shows the event argument
- [ ] `crates/transcript/Cargo.toml` depends on `serde`/`serde_json` only — no `tokio`, no `reqwest`
- [ ] `.gitignore` covers `target/`, `.cctg/`, `registry.json`, `.env`, and `git status` is clean after a build
- [ ] Every dependency version is declared once in `[workspace.dependencies]`; member crates use `workspace = true`

---

### TASK-003: Spike — nested `claude -p` session identity and nesting detection

Type: chore
Mode: small-fix
Priority: high
Branch: chore/spike-nested-session-detection

**Scope.** Settle OQ-1 by experiment, because the documentation does not cover it (the hooks reference does not
even list `CLAUDE_CODE_SESSION_ID` as a hook variable). Write a throwaway probe hook — a script or a
`cctg hook probe` stub — registered in `~/.claude/settings.json` for `SessionStart`, that appends the stdin
JSON plus the full `CLAUDE*` environment to a file under the task's `scratch/`. Then, from inside a normal
interactive session, run `claude -p "say ok"` through the Bash tool and compare: does the nested run's hook see
`CLAUDE_CODE_SESSION_ID` equal to the parent's id or to its own `session_id`? Does `CLAUDE_CODE_CHILD_SESSION`
survive? Does `CLAUDE_CODE_SESSION_ATTENDED` differ between interactive and `-p`? Also capture the `ppid` chain
and `CLAUDE_PID` so the fallback mechanism (`.cctg/<CLAUDE_PID>` + ppid walk) can be judged on evidence.
Deliverable is a findings file plus a recommended `fn detect_nesting()` contract — no production code.

- blocked by: nothing (needs no workspace; the probe can be a batch file).
- unblocks: TASK-012, which implements the chosen mechanism.

Expected observable outcome: either (a) parent id preserved → compare with stdin `session_id`, one-line
detection; or (b) overwritten → fall back to `CLAUDE_PID`/ppid; or (c) the variable is absent for hooks
entirely → nesting must be detected by the ppid chain alone.

Acceptance criteria:
- [ ] `scratch/` holds raw captured stdin JSON + environment for: interactive session start, nested `claude -p` start, and a subagent-spawning turn
- [ ] The report states, with the captured evidence quoted, whether `CLAUDE_CODE_SESSION_ID` in a nested run's hook equals the parent's or the child's session id
- [ ] The behaviour of `CLAUDE_CODE_CHILD_SESSION` and `CLAUDE_CODE_SESSION_ATTENDED` in both cases is recorded
- [ ] A single recommended detection rule is stated as a function contract (inputs: hook stdin + env; output: `Option<parent_session_id>`), with the fallback named
- [ ] The probe hook is removed from `~/.claude/settings.json` at the end and the report says so
- [ ] No secrets, user ids, or private paths in the captured files

---

### TASK-004: Spike — development channel lifecycle: resume, restart, missing flag

Type: chore
Mode: small-fix
Priority: high
Branch: chore/spike-channel-lifecycle

**Scope.** Settle OQ-3 and pin down the operational envelope of the channel before `cctg agent` is written.
Register a minimal echo channel (30 lines, any language, thrown away afterwards) at **user scope** via
`claude mcp add --scope user`, then observe four launches: `claude --dangerously-load-development-channels
server:probe`, the same with `--resume <id>`, the same with `--continue`, and one launch **without** the flag.
For each, record whether the startup banner shows the channels notice, whether `/mcp` lists the server, whether
an inbound `notifications/claude/channel` actually reaches Claude, and whether a `permission_request` arrives.
Also confirm the user-scope conclusion of OQ-4 by starting a session in a brand-new folder and checking that no
consent dialog appears.

- blocked by: nothing.
- unblocks: TASK-013 (agent) and TASK-011 (hub must render the "no channel" state).

Expected observable outcome: the flag is a per-launch opt-in unrelated to resume, so `--resume` with the flag
behaves like a fresh launch; without the flag the server still connects as a plain MCP server but no channel
notification is delivered (the docs state events are then dropped silently).

Acceptance criteria:
- [ ] A table in the report covers four launch modes × {banner notice, `/mcp` status, inbound delivered, permission relay}
- [ ] The behaviour of `--resume` and `--continue` with the dev flag is stated from observation, not inference
- [ ] It is recorded whether a session launched without the flag still spawns the server (and therefore whether the hub sees a useless connection it must distinguish from a real channel)
- [ ] A new folder with a user-scoped server is confirmed to start with no MCP consent dialog
- [ ] The probe server and its `~/.claude.json` entry are removed afterwards
- [ ] The report names the launch command cctg users will actually type, and whether a wrapper is needed

---

### TASK-005: transcript — tolerant jsonl parser with real fixtures

Type: feature
Mode: full
Priority: high
Branch: feature/transcript-parser
Domains: transcript

**Scope.** `crates/transcript`: `parse(&str) -> Vec<Turn>` over the session jsonl. Keep only `type: "user"` and
`type: "assistant"`, deserialize only the fields the renderers need (`uuid`, `parentUuid`, `timestamp`,
`sessionId`, `cwd`, `gitBranch`, `isSidechain`, `isMeta`, `message.content[]` as `text | tool_use |
tool_result | thinking`), everything `#[serde(default)]`. Unknown record types and unknown block types must be
skipped, never fail the parse — a local probe of a real 245-line transcript found record types beyond the list
in `CLAUDE.md`: `attachment` (86 of 245 records), `atis-latch`, `queue-operation`, `file-history-delta`,
`last-prompt`. A denylist will rot; use an allowlist of the two types we render. Fixtures are anonymized slices
of real files from `~/.claude/projects`, committed under `crates/transcript/tests/fixtures/`.

- blocked by: TASK-002.

Acceptance criteria:
- [ ] `parse` on a fixture with unknown record types and unknown content blocks returns the expected turns and no error
- [ ] `parse` on a truncated final line (the writer is asynchronous, so this happens) returns everything before it and no error
- [ ] `parse` on an empty string returns an empty `Vec`, and on a file of only ignored types returns an empty `Vec`
- [ ] At least three fixtures: a plain text session, a session with tool calls + results, and a session with thinking blocks and an `ai-title` record
- [ ] `ai-title` is exposed separately (for the topic title) without becoming a turn
- [ ] `grep` over `tests/fixtures/` finds no absolute user path, token, or Telegram id; no `unwrap()` on parsed input anywhere in the crate

---

### TASK-006: transcript — render_brief, render_full, 4096-char chunking

Type: feature
Mode: full
Priority: high
Branch: feature/transcript-renderers
Domains: transcript

**Scope.** On top of TASK-005: `render_brief(&[Turn]) -> Vec<String>` (user prompts, final assistant text, one
line per tool call — `Bash: description`, `Edit: file`) and `render_full` (adds tool inputs and truncated
results, never `thinking`). Both return chunks that each fit Telegram's 4096-character limit *after* entity
parsing, splitting on turn boundaries first, then lines, then hard-splitting; a chunk must never cut a Markdown
entity in half or the send fails with a 400. Expose a threshold above which the caller should send a file
instead of chunks. No IO — the file decision is the hub's, the crate only reports the size.

- blocked by: TASK-005.

Acceptance criteria:
- [ ] Every chunk from both renderers is ≤ 4096 characters for all fixtures, including one synthetic fixture with a single 50 KB tool result
- [ ] A tool result longer than the truncation budget is cut with a visible marker and the chunk still parses as valid Telegram Markdown (or the renderer emits plain text — whichever is chosen, a test asserts it)
- [ ] `render_brief` output for a fixture with 3 tool calls contains exactly 3 tool lines and no tool inputs
- [ ] `render_full` never contains any text from a `thinking` block (asserted against a fixture that has one)
- [ ] Splitting is deterministic: rendering the same fixture twice yields byte-identical chunks
- [ ] Rendering a 5000-turn synthetic fixture completes in under a second (guards against accidental quadratic string building)

---

### TASK-007: transcript — subagent transcripts and collapsed blocks

Type: feature
Mode: full
Priority: medium
Branch: feature/transcript-subagents
Domains: transcript

**Scope.** Same parser, applied to `agent-<agent_id>.jsonl`, plus the sibling `.meta.json`, whose real shape was
confirmed locally: `{"agentType","description","toolUseId","spawnDepth","model"}`. Produce the collapsed block
`↳ <agent_type> <short id>` with the meta `description` as its title and the brief rendering as its body. The
path must be taken from `SubagentStop.agent_transcript_path` when available rather than constructed — see
section 5. Also handle the `SubagentHandback` case: when the last assistant message is not the report, the
report is the `SubagentHandback` tool call's `message` input inside the subagent transcript, and that is what
the block should show.

- blocked by: TASK-005.
- prefer after: TASK-006 (reuses the brief renderer).

Acceptance criteria:
- [ ] A subagent fixture (with `isSidechain: true` and `agentId`) renders as one collapsed block, never as top-level turns
- [ ] The block title uses `.meta.json` `description` when the file exists and falls back to `agentType` when it does not
- [ ] A subagent fixture whose final action is a `SubagentHandback` call renders the handback `message` as the block body, not the trailing closing text
- [ ] A missing or unreadable `.meta.json` degrades to a rendered block and no error
- [ ] Subagent blocks always render in brief form even when reached through `render_full`

---

### TASK-008: hub — Bot API client and outbound queue with flood control

Type: feature
Mode: full
Priority: high
Branch: feature/hub-bot-api
Domains: hub

**Scope.** `crates/hub/src/api.rs`: the twelve methods listed in section 3 on `reqwest` (rustls, no default
features), each returning a typed result, with Telegram's `{ok, description, error_code, parameters}` envelope
mapped to a `thiserror` enum that distinguishes 429 (carrying `retry_after`) from 4xx from transport errors.
On top of it, the single outbound queue every other hub component must use: FIFO per `chat_id` +
`message_thread_id`, a token bucket sized to the documented limits (≈1 msg/s per chat, **20 msg/min for the
whole supergroup**, ≈30 req/s global), exact `retry_after` honouring on 429, and coalescing of repeated edits
to the same message. This is the component that decides whether the bot survives ten sessions starting at once.

- blocked by: TASK-002.

Acceptance criteria:
- [ ] A unit test with a mocked clock shows that 40 enqueued messages to one supergroup are released over ≥ 2 minutes (20/min), in enqueue order per topic
- [ ] A mocked 429 with `retry_after: 7` causes exactly one retry, no sooner than 7 seconds, and no retry storm on repeated 429s
- [ ] Three edits of the same message enqueued within the coalescing window result in one `editMessageText` call carrying the last text
- [ ] The error type lets a caller branch on 429 / bad request / transport without string matching
- [ ] The bot token never appears in any `tracing` output, including error paths (asserted by a test that captures logs on a failed call)
- [ ] `createForumTopic` goes through the same queue as messages, proven by a test that interleaves topic creation and sends

---

### TASK-009: hub — config, allowlist gate, long polling, `/brief` from a local transcript

Type: feature
Mode: full
Priority: high
Branch: feature/hub-polling-and-brief
Domains: hub, transcript

**Scope.** The first end-to-end slice of the hub with no agent involved: load `.env` (bot token, supergroup id,
allowlisted user ids, hub secret), run the `getUpdates` long-polling loop with proper `offset` handling and
backoff, gate every update on `from.id` against the allowlist (never on chat id — this is the documented
prompt-injection boundary), and implement `/brief [n]` and `/full [n]`, which read a jsonl path directly from
disk and reply through the TASK-008 queue, sending a file when the rendered output exceeds the chunk threshold.
Session discovery at this stage may be a hardcoded path or a `--transcript` argument; the registry arrives in
TASK-011.

- blocked by: TASK-008, TASK-006.

Acceptance criteria:
- [ ] A message from a non-allowlisted `from.id` in an allowlisted chat is dropped and logged without the user id
- [ ] `getUpdates` offset handling is proven by a test: an update is never delivered twice across a simulated restart
- [ ] `/brief` on a real fixture transcript produces the same chunks as `render_brief` and sends them in order
- [ ] Output above the threshold is sent as a document, not as N messages
- [ ] A missing or unreadable transcript path answers with a readable error in the topic and does not kill the polling loop
- [ ] The process starts with an absent `.env` by failing with a message naming the missing key, and logs no secret values

---

### TASK-010: proto — hub↔agent newline-JSON protocol, auth, TCP transport

Type: feature
Mode: full
Priority: high
Branch: feature/proto-transport
Domains: hub, channel

**Scope.** `crates/proto`: the serde types for both directions (`Hello{secret, role, host, pid, session_id?}`,
`Register{session_id, cwd, transcript_path, host, parent_session_id?, agent_id?, agent_type?, nested}`,
`Inbound{session_id, text, meta}`, `Outbound{session_id, text}`, `PermissionRequest`, `PermissionVerdict`,
`Bye`), newline framing with a maximum line length, and the shared-secret handshake as the first line with a
constant-time comparison. Plus the two thin halves: a `tokio` TCP listener for the hub and a reconnecting
client for the agent/hook. One crate, no business logic, so both sides can be built in parallel afterwards.

- blocked by: TASK-002.
- unblocks: TASK-012, TASK-013.

Acceptance criteria:
- [ ] Round-trip tests: every message variant serializes and deserializes to an identical value
- [ ] A client sending a wrong secret is disconnected before any other message is processed, and the failure log contains no fragment of the secret
- [ ] A line longer than the cap is rejected by closing the connection instead of allocating
- [ ] A client that loses the connection reconnects with backoff and re-sends its `Register` (proven against a listener that is stopped and restarted)
- [ ] Unknown message variants from a newer peer are ignored, not fatal (forward compatibility test)
- [ ] The listener binds to loopback by default; binding to another interface requires an explicit config value

---

### TASK-011: hub — registry.json, topic lifecycle, session state

Type: feature
Mode: full
Priority: high
Branch: feature/hub-registry-topics
Domains: hub

**Scope.** The architectural core: `session_id -> (device, folder, topic_id, alive, transcript_path,
parent_session_id?)` with the `(device, folder) -> [session_id]` index, persisted to `registry.json` atomically
(write temp + rename, never a partial file) and reconciled with live connections on start. Topic lifecycle:
create on first `Register`, reuse on `--resume` of a known session, rebind when the session id changes under a
live agent (`/clear`, resume picker — hdcd-telegram's `reconcile_session` handles exactly this and is worth
reading first), close or mark dead on `SessionEnd`. Titles are `[host] folder · ai-title`, ≤ 128 characters,
truncated in the middle rather than the end so the session id stays visible. State is shown in the title, since
`editForumTopic` cannot change `icon_color` (section 2, OQ-5). A `TOPIC_ID_INVALID` on reuse means the user
deleted the topic: forget the entry and create a new one.

- blocked by: TASK-009, TASK-010.

Acceptance criteria:
- [ ] Restarting the hub with a populated `registry.json` creates zero new topics for sessions that still exist
- [ ] A session that re-registers with a new `session_id` from the same agent connection rebinds to the existing topic instead of creating a second one
- [ ] A simulated `TOPIC_ID_INVALID` from `editForumTopic`/`reopenForumTopic` results in exactly one new topic and a cleaned registry entry
- [ ] Topic titles are always ≤ 128 characters for a pathological 300-character folder path, and still contain host and session id
- [ ] `registry.json` is never left truncated: a test that kills the process mid-write leaves the previous valid file
- [ ] A session with no agent connection (channel flag missing, per TASK-004) still gets a topic from the hook and is marked as such in the title

---

### TASK-012: hook — `cctg hook <event>` for all five events

Type: feature
Mode: full
Priority: high
Branch: feature/hook-subcommand
Domains: hooks

**Scope.** `crates/hook` + the `cctg hook` subcommand: read the event JSON from stdin, extract `session_id`,
`cwd`, `transcript_path`, `source`, and for subagent events `agent_id`, `agent_type`, `agent_transcript_path`,
`last_assistant_message`; apply the nesting rule chosen in TASK-003; send one `proto` message to the hub with a
short timeout; always exit 0, even when the hub is down, logging to stderr only. Handle the documented timing
constraints: `SessionEnd` hooks share a 1.5-second budget across all hooks
(https://code.claude.com/docs/en/hooks), so the SessionEnd path must be the fastest of the five. Also ship the
`~/.claude/settings.json` snippet that registers all five events, and document that it is user-scoped so every
folder on the machine is covered.

- blocked by: TASK-010.
- prefer after: TASK-003 (nesting rule), TASK-011 (something to register against).

Acceptance criteria:
- [ ] Each of the five events, fed as JSON on stdin, produces the expected `proto` message (golden tests with recorded payloads)
- [ ] With no hub listening, every event exits 0 within the timeout and prints one line to stderr and nothing to stdout
- [ ] A `SubagentStop` with an empty `agent_type` is dropped, not forwarded (internal Claude Code agents — section 5)
- [ ] The nesting flag is set exactly per the TASK-003 rule, with a test for both the nested and the top-level case
- [ ] `SessionEnd` completes in well under 1.5 s with an unreachable hub (measured in the test, not assumed)
- [ ] Malformed or empty stdin exits 0 without panicking

---

### TASK-013: agent — channel MCP server over stdio

Type: feature
Mode: full
Priority: high
Branch: feature/agent-channel-server
Domains: channel

**Scope.** `crates/agent` + `cctg agent`: hand-rolled JSON-RPC 2.0 over stdin/stdout. Implement `initialize`
(returning `capabilities.experimental["claude/channel"] = {}`, `["claude/channel/permission"] = {}`, `tools`,
and the `instructions` string that tells Claude how `<channel source=... chat_id=...>` tags arrive and to
answer with the reply tool), `notifications/initialized`, `tools/list`, `tools/call` for `reply`; everything
else answers method-not-found. Outbound `notifications/claude/channel` with `{content, meta}` where meta keys
match `[A-Za-z0-9_]+` (other keys are silently dropped — https://code.claude.com/docs/en/channels-reference).
Hub connection via `proto`. Absolute rule: **stdout carries JSON-RPC and nothing else**; all logging goes to
stderr or a file. Ship the `claude mcp add --scope user` install step from OQ-4.

- blocked by: TASK-010.
- prefer after: TASK-004.

Acceptance criteria:
- [ ] A scripted stdin session (`initialize` → `notifications/initialized` → `tools/list` → `tools/call reply`) produces valid JSON-RPC responses, one object per line, asserted by a test that parses every stdout line
- [ ] No test run produces a single non-JSON byte on stdout, including on panic and on hub-connection failure
- [ ] An unknown method returns JSON-RPC error `-32601` and the server stays alive
- [ ] Meta keys containing a hyphen are rejected or normalized before sending, with a test showing what is emitted
- [ ] A real `claude --dangerously-load-development-channels server:cctg` session shows the channels banner and a message pushed from the hub appears in the session (manual step, recorded in the task notes)
- [ ] Losing the hub connection does not terminate the server; it reconnects and the session keeps working

---

### TASK-014: permission relay end to end

Type: feature
Mode: full
Priority: high
Branch: feature/permission-relay
Domains: channel, hub

**Scope.** Close the loop: `notifications/claude/channel/permission_request` ({`request_id`, `tool_name`,
`description`, `input_preview`}) → hub → a message in the session's topic with Allow/Deny inline buttons →
`answerCallbackQuery` → `notifications/claude/channel/permission` with `{request_id, behavior}`. Callback data
is limited to **64 bytes**, which fits `a:<5-letter id>` comfortably — do not put tool names in it. Only
allowlisted `from.id` may decide; the docs are explicit that anyone who can reply through the channel can
approve tool use. The terminal dialog stays open in parallel and the first answer wins, so a verdict for an
already-resolved request must be handled gracefully (edit the message to "resolved elsewhere"), and
`input_preview` must be truncated to fit 4096 characters.

- blocked by: TASK-013, TASK-011.

Acceptance criteria:
- [ ] A relayed request renders one message with two buttons whose `callback_data` is ≤ 64 bytes (asserted in a test)
- [ ] Tapping Allow sends exactly one `notifications/claude/channel/permission` with the matching `request_id` and `behavior: "allow"`; Deny sends `"deny"`
- [ ] A callback from a non-allowlisted user is answered with a refusal and produces no verdict
- [ ] A second tap on the same request (or a request already answered in the terminal) sends no second verdict and edits the message instead
- [ ] A `input_preview` of 100 KB is truncated so the sent message is ≤ 4096 characters
- [ ] End-to-end manual check recorded: a `Bash` permission prompt in a live session is approved from Telegram and the tool runs

---

### TASK-015: subagents and nested runs inside the parent topic

Type: feature
Mode: full
Priority: high
Branch: feature/subagents-in-parent-topic
Domains: hub, hooks, transcript

**Scope.** Step 4's hard part, and the one architectural law most easily broken: a subagent or a nested
`claude -p` must never get its own topic. On `SubagentStart`, post a `↳ <agent_type> <short id>` placeholder in
the parent topic; on `SubagentStop`, edit it to carry the brief block built by TASK-007 from
`agent_transcript_path` (with `last_assistant_message` only as a fallback), dropping events whose `agent_type`
is empty. A nested run registers through its own `SessionStart`, is recognised by the TASK-003 rule, and is
attached to the parent's topic as `⇣ nested <id>`; its own channel is not started. A reply to a subagent block
becomes inbound to the **parent** session with meta `target_agent=<agent_id>`, and the channel `instructions`
tell Claude to forward it with SendMessage.

- blocked by: TASK-012, TASK-011.
- prefer after: TASK-007, TASK-014.

Acceptance criteria:
- [ ] A session that spawns three subagents produces exactly one topic and three blocks inside it
- [ ] A nested `claude -p` started from a Bash tool call produces zero new topics and one `⇣ nested` block in the parent topic
- [ ] `SubagentStop` events with an empty `agent_type` produce nothing (no ghost blocks from `/btw` or prompt suggestions)
- [ ] A reply to a subagent block arrives in the parent session with `target_agent` present in the `<channel …>` tag
- [ ] Killing the hub mid-subagent and restarting it leaves the parent topic intact and the block is still updated or marked stale — no duplicate topic
- [ ] Message volume for one subagent is at most two API calls (post + edit), verified against the TASK-008 queue counters

---

### TASK-016: multi-session, multi-folder routing soak

Type: chore
Mode: full
Priority: medium
Branch: chore/multi-session-soak
Domains: hub

**Scope.** The QA gate for the MVP. Run four simultaneous sessions — two in the same folder, one in a different
folder, one nested — plus a fifth started while the hub is down, and verify routing, topic identity, message
ordering and rate-limit behaviour under the 20 msg/min ceiling. Produce a written result with the observed
message counts, not just a pass/fail. This is where the real architecture either holds or reveals that the hub
needs per-topic priority (permission prompts must overtake transcript chunks when the budget is tight).

- blocked by: TASK-015.

Acceptance criteria:
- [ ] Four concurrent sessions produce exactly four topics (nested run excluded) and no cross-routing: a message sent in topic A never reaches session B
- [ ] A session started while the hub is down registers correctly after the hub returns, with no duplicate topic
- [ ] Under a burst of all four sessions finishing turns at once, no 429 is left unhandled and the bot is not flood-limited (counters reported)
- [ ] A permission prompt issued during that burst reaches Telegram within a stated time bound, ahead of queued transcript traffic
- [ ] `registry.json` after the run matches the observed topics exactly
- [ ] The report records message counts per minute against the 20/min ceiling

---

## 5. Contradictions and extensions to `CLAUDE.md` / project context

Each has a primary source and is appended to `PCTX_PROPOSALS.md`; none was silently applied.

1. **`SubagentStop` carries `agent_transcript_path`.** `CLAUDE.md` and the transcript domain module say to
   construct `<session-id>/subagents/agent-<agent_id>.jsonl`. The hook gives us the path.
   https://code.claude.com/docs/en/hooks
2. **`last_assistant_message` is not the subagent's report when `SubagentHandback` is used** (v2.1.271+), which
   is every MAW agent. The report is the tool call's `message` input. Same source.
3. **`SubagentStop` also fires for Claude Code's internal agents** with an empty `agent_type`. Unfiltered, this
   puts ghost `↳` blocks in topics. Same source.
4. **`transcript_path` lags the conversation** — the file is written asynchronously and may not yet contain the
   current turn when a hook fires. A hub that renders a turn on `Stop` by reading the jsonl can render a stale
   last turn; use `last_assistant_message` for the current turn. Same source.
5. **`editForumTopic` cannot change `icon_color`** — only `name` and `icon_custom_emoji_id`. The planned
   state-by-icon scheme needs the title or a custom emoji instead.
   https://core.telegram.org/bots/api#editforumtopic
6. **Real transcripts contain record types the ignore-list does not mention** (`attachment`, `atis-latch`,
   `queue-operation`, `file-history-delta`). The parser must allowlist `user`/`assistant`, not denylist.
   Local probe of `~/.claude/projects/C--Users-user-dev-cctg/1f2c01a2-….jsonl`: 245 records, 86 of them
   `attachment`.
7. **`CLAUDE_CODE_SESSION_ID` is undocumented** as a hook environment variable. It is observably present, but
   nothing promises it, which is why TASK-003 exists and why the detection lives behind one function.

---

## 6. Parallelism and critical path

```
TASK-002 bootstrap
 ├── TASK-005 parser ──► TASK-006 renderers ──► TASK-009 polling+/brief ──┐
 │        └────────────► TASK-007 subagents                               │
 ├── TASK-008 bot api + queue ────────────────────────────────────────────┤
 └── TASK-010 proto ──────────────────────────────────────────────────────┤
                                                                          ▼
                                                              TASK-011 registry+topics
                                                                  ├── TASK-012 hooks
                                                                  └── TASK-013 agent
                                                                          ▼
                                                              TASK-014 permission relay
                                                                          ▼
                                                              TASK-015 subagents/nested
                                                                          ▼
                                                              TASK-016 soak
TASK-003 spike (independent)  ──► informs TASK-012
TASK-004 spike (independent)  ──► informs TASK-013, TASK-011
```

**Critical path:** 002 → 005 → 006 → 009 → 011 → 013 → 014 → 015 → 016 (nine tasks). TASK-008 and TASK-010 sit
just off it and will join it if they slip, so treat them as near-critical.

**Run in parallel:**
- TASK-003 and TASK-004 from day one — they need no code and they de-risk two later tasks. Do them first.
- After TASK-002: three independent lanes — transcript (005 → 006 → 007), Telegram (008), transport (010).
- After TASK-011: TASK-012 and TASK-013 are independent of each other (both depend only on `proto` and the
  registry).

**Do not parallelize:** TASK-011 with anything else that touches the registry; TASK-014 and TASK-015 both edit
the hub's message-rendering path and will conflict.

---

## 7. Risk areas

- **The 20 messages/minute per group ceiling is shared by every topic.** This is the single largest threat to
  the design as written. If TASK-008's queue is weak, everything above it degrades under exactly the condition
  the project was built for (several sessions at once). Treat the queue as a first-class component, not plumbing.
- **The research-preview channel API can change or disappear.** `--dangerously-load-development-channels` is a
  flag Anthropic explicitly marks as dangerous and temporary; `capabilities.experimental` is experimental by
  name. Keep the channel surface confined to `crates/agent` so a protocol change is one crate's problem.
- **Users forgetting the flag.** A session started without it looks normal but silently delivers nothing, and
  the docs confirm the events are dropped with no error to the server. The hub must show that state; otherwise
  the first bug report will be "messages disappear".
- **Undocumented environment variables** (OQ-1) — one Claude Code release can break nesting detection. Hence a
  single detection function and a fallback.
- **Windows specifics**: atomic rename over an existing file, path encoding with drive letters and backslashes
  in the `<encoded-cwd>` scheme, and process-tree walking for the ppid fallback are all places where a
  Linux-shaped reference implementation will mislead. tebis exists precisely because of these.
- **Secret leakage through `tracing`.** The bot token is in a URL, which means it is one `?err` away from a log
  line. TASK-008 has an explicit test for this; keep that test as the pattern for hub and agent.

---

## 8. Open questions that need a human decision before implementation

1. **Launch ergonomics.** Every session must be started with `claude --dangerously-load-development-channels
   server:cctg`. Do we ship a `cctg run` wrapper / shell alias, or document the flag and accept that a plain
   `claude` gives a topic with no channel? This affects TASK-004's deliverable and the hub's dead-state UI.
2. **State indicator.** Title glyph or custom emoji id, given that `icon_color` is immutable after creation?
   Title is simpler and searchable; emoji is prettier and costs a `getForumTopicIconStickers` call.
3. **When to push a turn into the topic at all.** "On every `Stop`" is the obvious rule, but with the 20/min
   ceiling and four sessions it is also the rule that gets the bot flood-limited. Alternatives: only when the
   user is subscribed to that topic, or only a one-line summary with `/full` on demand. This shapes TASK-009.
4. **Dead sessions.** Buffering is agreed; the retention rule is not. How long, how many messages, and what
   happens when the buffer overflows — drop oldest, or refuse with a message in the topic?
5. **Do we want `frankenstein` after all?** The recommendation in section 3 is bare `reqwest`, with the switch
   criterion named. If the preference is for typed models from the start, say so before TASK-008 begins — it is
   a one-line dependency change then and a rewrite later.
