# PCTX proposals — TASK-011

## 2026-09-23 (planner): hub domain, what TASK-011 implements (after merge)

Proposed addition to `domains/hub.md` implementation notes:
- TASK-011: `hub/registry.rs` (pure) + `hub/slots.rs` (the only owner of the registry, an actor). `registry.json` in `<CCTG_STATE_DIR|.cctg>` (temp + fsync + rename, written by a separate save task from the latest snapshot). An unparsable or foreign-version file stops the hub. Slot identity `(host, folder_key(cwd), ordinal)`; `folder_key` is lexical (strip `\\?\`, `\\?\UNC\` -> `\\`, `\` -> `/`, trailing `/` dropped, case-fold for drive-letter and UNC paths). Nesting = `SessionStart.parent_claude_pid` looked up in the persisted `pids` map `"<host>/<pid>" -> session_id`. Topic work is a diff of desired vs applied name/icon; TOPIC_NOT_MODIFIED counts as applied; TOPIC_ID_INVALID / "message thread not found" replaces the topic once. Default icons: alive `5312016608254762256`, dead `5408906741125490282`, waiting `5377316857231450742`, no channel `5357121491508928442`, checked against `getForumTopicIconStickers` at start.

Why: TASK-012..018 build on these names and rules.

## 2026-09-23 (planner): channel domain, agent cwd must match the hook's

Proposed invariant for `domains/channel.md`: `cctg agent` must report `Register.cwd` canonicalized exactly like the hook (TASK-012: `std::fs::canonicalize` with fallback, no `\\?\`). The hub adopts an agent-only session under the agent's `cwd`; a different spelling of the same folder through a junction would otherwise get a second slot.

Why: TASK-011 `folder_key` only fixes lexical variants; the agent path is a second source of `cwd`.

## 2026-09-23 (plan-reviewer-2): supersedes parts of the planner entries above

- hub: the hub never adopts a session it has not seen a SessionStart for. An agent of an unknown session waits (bound when the SessionStart arrives, dropped on disconnect); unknown Stop/UserPromptSubmit are ignored. Reason: without SessionStart a nested run cannot be told from a top-level one, and nested = no topic. The planner entry "The hub adopts an agent-only session under the agent's cwd" no longer holds; the channel-domain proposal about agent cwd canonicalization stays useful for TASK-013 but is not load-bearing for slots.
- hub: topic icons come only from a successful `getForumTopicIconStickers`; a lookup error stops the hub start, a missing preferred id is replaced by the smallest offered spare id.
- hub: at most one topic call per slot is in flight; the session separator stays in `registry.json` until Telegram accepted it (a lost answer can duplicate it, never lose it).
- hub: the slot actor hands jobs to a dispatch task over an unbounded channel; it never awaits the scheduler queue (TASK-010 QA lesson).

- 2026-09-23 (qa, domain hub, Risk lessons): tests that drive the `Slots` actor through the real `Scheduler` must pass a fast `BucketConfig` (large capacity, `min_gap` 0) or wait longer than `min_gap`: with the default 1 s gap a second separator arrives after a short "no new ops" window and the test flakes (QA saw 1 of 3). Also: the hook wire `SessionEnd` has no `claude_pid`, so the hub cannot tell a nested run's SessionEnd from the real one for the same session id; TASK-012 should send it.

## 2026-09-23 (fixer round 2): hooks + hub domains, SessionEnd carries the pid

Proposed invariant (hooks and hub): `HookEvent::SessionEnd` has an optional `claude_pid` (`CLAUDE_PID` of the hook). The hub ignores a SessionEnd whose pid is present and differs from the pid recorded at the session's latest SessionStart: that is the end of a nested `claude -p --resume <id>`, not of the session. The hook must fill it (TASK-012). Also: only `SessionStart(source=clear)` inherits the slot of the previous session of the same claude pid; any other start with a reused pid marks that previous session ended and takes the first free slot.

Why: QA s5b/s5c (a nested resume's SessionEnd freed a live session's topic) and s4b (Windows pid reuse after a lost SessionEnd steered an unrelated startup into an old slot). Supersedes the "SessionEnd has no claude_pid" note in the QA entry above.

> RESOLVED: accepted by the user in /maw-context --review; folded on 2026-09-23 into domains/hub.md (implementation line incl. plan-reviewer-2 and fixer round 2 corrections, QA test lesson) and domains/channel.md (agent cwd/host helper).
