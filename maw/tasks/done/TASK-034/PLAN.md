# PLAN — TASK-034: hub never reads session files (client/server split, part 1)

Line numbers refer to the repo at `d871693` (branch `feature/agent-serves-transcripts`).

A full reference implementation of this plan was built and tested outside the repo (a `%TEMP%` copy, shared `target/`). It is kept as evidence under `scratch/planner/`:

- `reference.patch`: `git diff` against the repo root. `git apply --check` passes on `d871693`. sha256 `13763489afbb8a16201925ca3bc12ce08d8f2569abdaea715c11907d8f52c02a`.
- `reference.sha256` + `reference.deleted` + `verify_hashes.sh`: per-file hashes (CR removed). Run `bash maw/tasks/in_progress/TASK-034/scratch/planner/verify_hashes.sh` from the repo root to see where a tree differs from the reference.
- `mutations.sh` + `mutations.log`: four mutants, all killed (see Steps 13-14).

Results of the reference: `cargo fmt --check` clean, `cargo clippy -p cctg --all-targets` with no warnings, `cargo test --workspace` green (cctg lib: 550 passed, 1 ignored; every integration binary green; the slots tests passed 3 runs in a row after the race fix in Step 12), fake soak (`cargo test -p cctg --test soak -- --ignored`) prints `soak: ok`.

The implementer may apply the patch as a starting point. The steps below are the contract. The patch is one way to meet it.

## 1. Understanding

### What reads session files today (complete inventory, confirmed by grep and PREMISE_CHALLENGE §2)

| Feature | Hub code that reads | What it reads |
|---|---|---|
| `/brief`, `/full` | `hub/commands.rs:169-209` (`read_limited`, `relative_age`, `candidate_title`), `:248-307` (`prepare`), run by `handle` on `spawn_blocking` (`:451`) | the whole transcript (≤256 MiB) plus file mtime and head for ambiguous-prefix candidates |
| Session choice for commands | `hub/sessions.rs` (whole file): `ProjectsDir` scans `<CCTG_PROJECTS_DIR\|~/.claude/projects>/*/*.jsonl` (`:68-117`); `SlotLocator` (`:141-172`) maps a topic to `(session, transcript_path)` through the `TopicView` watch that `Slots` publishes (`slots.rs:738, 4835-4842`, `registry.rs:1670-1688`) | directory listings and mtimes |
| ai-title | `slots.rs:541-575` (`read_title`, `first_ai_title`), started from `on_hook` (`:1274-1276`, `Followup::read_title` from `registry.rs:1048-1067`), result `Done::Title` (`:4519-4542`), offsets in `Slots::scanned`/`reading` | transcript tail since the last scan |
| TASK-015 correlation | `subagents.rs:88-144` (`scan`, `scan_lines`), started by `check_candidates` (`slots.rs:1380-1409`), result `Done::Index` (`:4543-4553`) into `AgentIndex` | parent transcript after an offset |
| TASK-015 block text | `subagents.rs:385-430` (`read_body`, `read_capped`), `start_body_reads` (`slots.rs:1504-1526`), result `Done::Body` (`:4554-4561`) | `subagents/agent-<id>.jsonl` (≤64 MiB) and `.meta.json` |

Everything else under `hub/` that touches files is the hub's own state: `offset.rs`, `registry.rs` (`RegistryStore`), `config.rs` (`.env`), `testdir.rs` (tests), and `mod.rs:214` (`client::own_build`, the hub's own exe). `hub/fetch.rs` only moves Telegram downloads to agents.

`hub/mod.rs:167-169` refuses to start without a projects dir (`CCTG_PROJECTS_DIR` or home), and `:245` wires `SlotLocator::new(view, ProjectsDir::new(projects_dir))`.

### The precedent: the TASK-016 stream is already served by the agent

- `wire.rs:136-139` `Register.transcript_reads`; `:464-473` `HubMsg::TranscriptRead{session_id, path, from}`; `:208-220` `AgentMsg::TranscriptChunk`.
- `tail.rs:44-61` `projects_root()` = `<CLAUDE_CONFIG_DIR|~/.claude>/projects`. `:164-185` `open_transcript` is the path gate: plain session id, canonical path must be `<canonical root>/<project>/<session_id>.jsonl`, and it must be a file. It opens the canonical path.
- `agent.rs:1278-1304` `spawn_reader`: one worker, `spawn_blocking`, queue of 1. `:802-812` is the loop arm.
- `ingress.rs:218-235` forwards only an explicit list of `AgentMsg` variants after registration (TASK-016 QA lesson).
- `slots.rs:2532-2647` `stream_target` / `pump_streams`: the hub asks only the bound agent with `reads`, keeps `live.reading = (conn, sent)`, retries after `READ_TIMEOUT`, and drops chunks from another connection (`:2676`).

### Who has an agent link

- `agent.rs:521-538` `link_plan`: `CLAUDE_CODE_ENTRYPOINT=sdk-cli` (headless `claude -p`, including nested) never links.
- `registry.rs:1107-1115` `agent_connected` binds only top-level sessions. `:1014-1020` `SessionEnd` clears `entry.agent`. `slots.rs:3766-3773` an agent leaving for an update (TASK-040) is unbound (`leaving = true`, `agent_disconnected`).
- After `/clear` the agent keeps its env `CLAUDE_CODE_SESSION_ID`. The hub rebinds it by pid (`slots.rs:1123-1199`). Only the hub knows which session an agent serves now.

### Wire compatibility rules

`wire.rs:13-23`: a new message type goes behind a capability announced in `Register`. `check_kind` (`:574-580`) rejects unknown `type`s, and `read_hub_frames` (`agent.rs:318-327`) only warns about them. `VERSION` stays 1.

## 2. Approach

All session reads go to the session's bound agent over the existing link. There is one new request/answer pair behind one new capability. The hub keeps every piece of state and policy it has now: `AgentIndex`, candidates and backoff, title offsets, block bookkeeping, and the sequential command worker. Only the file IO and the parsing that needs the file move into a new agent-side module, `crates/cctg/src/reads.rs`. It sits next to `tail.rs` and reuses its gate.

Wire (`wire.rs`):

```rust
Register { ..., #[serde(default)] session_reads: bool }
HubMsg::SessionRead { read_id: u64, session_id: String, path: String, ask: SessionAsk }        // "session_read"
AgentMsg::SessionAnswer { read_id: u64, answer: SessionAnswer }                               // "session_answer"
#[serde(tag = "kind")] enum SessionAsk {
    Render { view: TranscriptView, prompts: u32 }, Title { from: u64 }, Calls { from: u64 },
    Subagent { agent_id, agent_type?, description?, header?, last? },  #[serde(other)] Other }
#[serde(tag = "kind")] enum SessionAnswer {
    Text { text, more }, Title { title?, scanned }, Calls { offset, calls: Vec<SpawnCall>, links: Vec<SpawnLink>, more },
    Missing, Unreadable, TooLarge, Unsupported, #[serde(other)] Other }
```

Why these choices (details and rejected alternatives in OPEN_DECISIONS 2-11 and `log.jsonl`):

- **One pair with a `kind` enum** instead of four pairs. There is one capability check, one ingress arm and one correlation map. Older and newer peers read unknown kinds as `Other` through `#[serde(other)]`, the same way `StreamItem` does (`wire.rs:373-404`).
- **The hub asks; the agent never pushes.** After `/clear` only the hub knows the agent's current session and its `transcript_path`. The hub also already knows when to ask: on a prompt or stop hook for the title, on candidate due times for calls, on `SubagentStop` for block text.
- **Request ids and a pending map.** Every read is keyed by `read_id` and bound to the `conn` it went to. A late answer or one from another connection is dropped. Every read fails on a timeout, on link close, or when its agent leaves for an update. Common practice for multiplexed request/response over one connection: correlate by id, bound in-flight requests and their lifetimes, and fail pending requests when the peer disconnects ([microsoft/agent-host-protocol #470](https://github.com/microsoft/agent-host-protocol/issues/470), [json-rpc.dev best practices](https://www.json-rpc.dev/learn/best-practices), [JSON-RPC guide](https://jsonic.io/guides/json-rpc-guide)).
- **Bounded lines.** A text answer (render or block) comes in pieces of at most `PIECE = 128 KiB`. Even with every byte escaped as `\u00XX` (×6) a piece stays under `wire::MAX_LINE` (1 MiB); `tail.rs:31-34` uses the same argument. The total per read is at most `MAX_TEXT = 16 MiB`: the agent answers `TooLarge` beyond that, and the hub drops an agent that sends more. `calls` answers carry at most 64 KiB of string weight (plus 32 bytes per entry), then `more: true`. Fields are cut to 256 UTF-16 units as before. `last` is at most 128 KiB because the hook caps it (`hook.rs:68, 523, 550`), so a `Subagent` ask fits one line.
- **Timeout.** `Options::read_wait`, default `READ_WAIT = 20 s`. Each text piece restarts the wait, so a large `/full` over a slow link is not cut. The command worker keeps a 120 s safety net (`ANSWER_WAIT`) only for a stopped actor.
- **Path gate.** Canonicalize, then check that the canonical path sits under the canonical base with the exact expected shape, then open the canonical path ([OWASP-style guidance via PortSwigger](https://portswigger.net/web-security/file-path-traversal), [PVS-Studio V5332 / OWASP](https://pvs-studio.com/en/docs/warnings/v5332/)). The check happens after symlink/junction resolution, so a link that points outside, or into another session's folder, fails the shape check (a pre-resolution symlink check would be wrong, see [soft-canonicalize notes](https://docs.rs/soft-canonicalize) and [std::fs::canonicalize](https://doc.rust-lang.org/std/fs/fn.canonicalize.html)). The session id comes from the hub: it is the session the hub bound this agent to. See Risk R5 for TOCTOU.
- **Degradation.** A session without a reading agent: ended, headless or nested (never linked), live without a link, or an agent without `session_reads`.
  - `/brief` gets a notice that says which of these it is. An ended session's notice gives `claude --resume <id>`.
  - The ai-title stays the short id.
  - A typed subagent's block opens on its `SubagentStop` from the hook data alone: report > `last_assistant_message`. There is no running block before the stop. The hook has already dropped stops that left no subagent files (`hook.rs:536-545`).
  - No brief cache (OPEN_DECISIONS 2).
- **Separate agent worker.** A `/full` of a 92 MiB session parses for seconds. It must not hold up the 300 ms stream reads, so session reads get their own worker next to `spawn_reader`.

## 3. Steps

Order: wire → agent side → hub side → wiring/removals → tests. Each step names what must be true when done.

### Step 1: `crates/cctg/src/wire.rs`: the new messages
- Add `Register.session_reads: bool` (`#[serde(default)]`, doc: agents built before leave it out; the hub then shows no `/brief`, no ai-title and builds blocks from hooks alone).
- Add `AgentMsg::SessionAnswer { read_id, answer: SessionAnswer }` and `"session_answer"` in `AgentMsg::KINDS`.
- Add `HubMsg::SessionRead { read_id, session_id, path, ask: SessionAsk }` and `"session_read"` in `HubMsg::KINDS`.
- Add `SessionAsk`, `TranscriptView {Brief, Full}`, `SessionAnswer`, `SpawnCall {id, subagent_type?, description?}` and `SpawnLink {agent_id, tool_use_id}` as sketched in §2. Optional fields use `skip_serializing_if`. Add the pair to the module doc list (`:13-23`).
- Tests: add samples to `agent_samples`/`hub_samples` (the round-trip and kind-list tests then cover them). Add `session_reads` to the `Register` literals. New test `session_reads_stay_compatible_with_version_one_peers`: an old register decodes with `false`; an unknown `ask.kind` decodes as `SessionAsk::Other`; an unknown answer kind decodes as `SessionAnswer::Other`; a last piece without `more` decodes with `more: false`.

### Step 2: `crates/cctg/src/tail.rs`: share the gate
- Make `open_transcript` `pub(crate)`. Move its session-id check into `pub(crate) fn is_plain_session_id(&str) -> bool` and reuse it. Behaviour does not change: the existing tail tests must pass unchanged.

### Step 3: new `crates/cctg/src/reads.rs` (agent side), plus `pub mod reads;` in `lib.rs`
- Public: `answer(root: Option<&Path>, session_id, path, ask) -> Vec<SessionAnswer>` (blocking) and `pieces(&str) -> Vec<SessionAnswer>`. Constants: `MAX_TRANSCRIPT_BYTES = 256 MiB`, `MAX_AGENT_BYTES = 64 MiB`, `MAX_META_BYTES = 64 KiB`, `MAX_TEXT = 16 MiB`, `PIECE = 128 KiB`, `MAX_CALLS_WEIGHT = 64 KiB`, `MAX_PROMPTS = 100`.
- `root == None` → `[Missing]`. `SessionAsk::Other` → `[Unsupported]`.
- `Render`: `open_transcript` (None → `Missing`). Then `read_limited`, moved from `commands.rs:169-179`: over the limit → `TooLarge`, IO error → `Unreadable`. Then `transcript::parse` (lossy UTF-8), `last_prompts(prompts.clamp(1,100))`, `render_brief`/`render_full`, then `pieces`. The output must equal the library exactly, as the old reply did.
- `Title{from}`: `first_ai_title`, moved verbatim from `slots.rs:555-575`, with the same 256 MiB absolute cap. Answer `Title{title: cut(title, 1024), scanned: from + complete-line bytes}`. Missing → `Missing`.
- `Calls{from}`: `scan_lines`, moved from `subagents.rs:102-144`. It fills a `Calls` answer: the `"Agent"`/`agentId` prefilter, `cut(field, 256)`, ids longer than 256 skipped. Stop when the weight reaches `MAX_CALLS_WEIGHT` (`more: true`). A torn last line is left for the next ask. Within one line, entries beyond twice the weight are dropped.
- `Subagent{...}`: `is_agent_id(agent_id)` and `is_plain_session_id` or `[Missing]`. Then `open_subagent_file(root, session_id, path, "agent-<id>.jsonl")` and the same for the sibling `.meta.json`. Each path is canonicalized separately and must be `<canonical root>/<one component>/<session_id>/subagents/<exact name>` and a file. A gate failure means "no file", not an error. Read capped (64 MiB / 64 KiB) and hand to `hub::subagents::body_text(&BodyInput{report: None, ...}, meta, transcript)`, then `pieces`. So a missing or foreign file gives the stop's last message, as `read_body` did for a missing file.
- Nothing in `reads.rs` logs.
- Tests (in the module): library equality on the 7 fixtures (moved from `commands.rs` tests); pieces fit a line with control chars, concatenate back, `more` only on non-last, `TooLarge` over 16 MiB; missing/foreign/`..`/other session id/traversal id/no root/a directory named like a transcript → `Missing`; `read_limited` limit; title past a 5 MiB head, from an offset, torn line (moved from `slots.rs:9258-9304`); calls found and torn line (moved from `subagents.rs:467-515`); 2000 calls come in several batches that each encode under `MAX_LINE` and add up to 2000/2000; block text from the session's files; another session id, a file beside `subagents/`, another agent's name and `..` all fall back to the last message; agent id `../x` → `Missing`; `#[cfg(windows)]` junction `<root>/C--proj/<SESSION>` → `<root>/C--proj/<OTHER>` is not followed (skipped if `mklink /J` is unavailable).

### Step 4: `crates/cctg/src/hub/subagents.rs`: drop file IO
- Delete `MAX_TRANSCRIPT_BYTES`, `MAX_AGENT_BYTES`, `MAX_META_BYTES`, `MAX_CALL_FIELD`, `AGENT_TOOL`, `scan`, `scan_lines`, `read_body` and `read_capped`, and the `std::io`, `serde_json::Value`, `Block` and `parse` imports. Keep `Scan` (now the shape of a `calls` answer), `AgentIndex`, `Candidates`, `Reports`, `BodyInput` (its `agent_path` is now "the agent's path, only the agent opens it") and `body_text` (now the pure core both sides use). Update the module doc.
- Add `Candidates::take_stopped(&mut self, session) -> Vec<(String, Candidate)>`: the candidates of that session whose stop came, sorted by id.
- Tests: replace the scan-based index tests with hand-built `Scan`s: link across two scans, a new path resets the index, eviction at `MAX_INDEX_ENTRIES`. Delete `the_body_is_read_from_the_subagent_files` (moved to `reads.rs`).

### Step 5: `crates/cctg/src/agent.rs`: serve session reads
- `Register { ..., session_reads: true }` in `run_stdio` (`:465-476`).
- New `spawn_session_reader(outbox, projects) -> mpsc::Sender<(u64, String, String, SessionAsk)>`, queue `SESSION_READS = 8`. One read at a time on `spawn_blocking(reads::answer)`, then `outbox.send(SessionAnswer{read_id, answer})` for each answer in order (backpressure). It stops when the outbox is gone. It is separate from `spawn_reader`.
- `serve_channel`: create it next to `reads` (`:628-635`). New loop arm `HubMsg::SessionRead` → `try_send`; if full, drop with a `debug!` (the hub's wait runs out). Nothing reaches Claude Code.
- Module doc: a "Session reads (TASK-034)" paragraph.
- Test `session_reads_are_answered_in_pieces_over_the_link_and_never_reach_claude`: a raw hub sends a `Render` for a transcript whose prompt is longer than `PIECE`, plus a `Title`. The agent answers with several `Text` pieces in order and then the `Title`. The next line Claude Code reads is its own ping answer.

### Step 6: `crates/cctg/src/channel.rs`
- Add `HubMsg::SessionRead { .. }` to the "agent loop's, not the channel's" arm (`:240-253`). The match is exhaustive, so it does not compile without this.

### Step 7: `crates/cctg/src/hub/ingress.rs`
- Add `| AgentMsg::SessionAnswer { .. }` to the forwarded list (`:220-229`). Without it answers are dropped as a repeated handshake (TASK-016 QA lesson). `tests/reads_e2e.rs` covers it over real TCP.

### Step 8: `crates/cctg/src/hub/commands.rs`: the worker asks the actor
- Remove `read_limited`, `relative_age`, `candidate_title`, `locate_notice`, `prepare`, `prepare_limited`, `MAX_TRANSCRIPT_BYTES`, `CANDIDATE_TITLE_BYTES` and the `sessions` import.
- Add:
  - `pub trait TranscriptSource { fn prepare(&self, thread_id, TranscriptCommand) -> impl Future<Output = Prepared> + Send; }`, same shape as `scheduler::Transport`.
  - `pub struct TranscriptAsk { thread_id, command, answer: oneshot::Sender<Prepared> }`.
  - `pub struct Asks(pub mpsc::Sender<TranscriptAsk>)`, implementing the trait. A send error or `ANSWER_WAIT` (120 s) gives `Notice(NO_ACTOR)`.
  - `View::wire()`.
  - `pub fn resolve(&Registry, thread_id, prefix) -> Result<String, String>`: a prefix matches over `registry.sessions` (0: "Нет известной hub сессии с таким началом id."; 1: that session; more: newest-first list `short · идёт|завершена[ · title]`, at most 10, then `… и ещё N`); otherwise a slot topic gives its `current_session` ("В этой теме ещё не было сессии." when none); otherwise the running top-level session with the highest `seen` ("Запущенных сессий нет.").
  - `pub enum Unavailable {Ended, Nested, NoAgent, OldAgent, NoTranscript, Missing, Unreadable, TooLarge, NoAnswer, LinkLost, Failed}` and `pub fn unavailable(why, session_id) -> Prepared`: short id only; only `Ended` ends with `claude --resume <full id>`.
  - `pub fn transcript_reply(command, session_id, body) -> Prepared`: blank → "пока нечего показывать", else `Reply` with the same file name and caption as today.
- `handle`/`serve` become generic over `S: TranscriptSource`. `handle` awaits `source.prepare(..)` (no `spawn_blocking`). Delivery code is unchanged.
- `USAGE`: "Без id в теме берётся её текущая сессия, в General самая свежая запущенная. Транскрипт отдаёт агент запущенной сессии с её машины."
- Module doc: say that the hub reads no transcript.
- Tests: keep `only_our_commands_are_commands` and `parses_arguments`. The delivery tests (multi-chunk order, one document, too-long switches once, other errors give one notice and the worker goes on) run against a fake source `Rendered(jsonl)` that returns `transcript_reply(library render)`. New: `a_command_finds_its_session_in_the_registry` (Registry via `apply_hook`), `notices_name_the_short_id_only`, `a_stopped_actor_gives_a_notice`.

### Step 9: `crates/cctg/src/hub/slots.rs`: pending reads in the actor
Production changes:
1. Remove `use std::io::{...}`, `TITLE_SCAN_BYTES`, `read_title()`/`first_ai_title()` (`:541-575`), and `Done::{Title, Index, Body}` with their `on_done` arms (`:349-365`, `:4519-4561`). Remove the `view` field, the `watch` view channel (`:738`) and its publishing in `pump` (`:4835-4842`). `Slots::new` returns `Self`.
2. `Options.read_wait` (default `READ_WAIT = 20 s`). `MAX_READ_TEXT = crate::reads::MAX_TEXT`.
3. `Conn.session_reads` from `Register`.
4. New state: `transcript_asks: Option<mpsc::Receiver<TranscriptAsk>>`, `reads: HashMap<u64, Pending>`, `read_ids: u64`. `struct Pending { conn, until, purpose }` and `enum Purpose { Command{ask, session, text}, Title{session, path}, Calls{session, path, from}, Body{input, text} }`.
5. `pub fn transcript_asks(&mut self) -> mpsc::Sender<TranscriptAsk>` (queue 16), like `permission_asks`. `run` polls it in the `select!`. `next_deadline` includes `min(reads.until)`.
6. `fn reader(&self, session) -> Option<u64>`: `entry.agent`, conn open, `session_reads`, not `leaving`, `bound.session == session`.
7. `fn ask_read(conn, session, path, ask, purpose)`: mint a `read_id`, then `try_send(HubMsg::SessionRead)`. On failure (queue full or gone) call `read_failed(purpose, LinkLost)` at once; otherwise insert a `Pending` with `until = now + read_wait`.
8. `fn on_session_answer(conn, read_id, answer)`: only the conn that was asked counts, otherwise `debug!` and drop.
   - `Text` pieces append to the purpose's `text`. Going over `MAX_READ_TEXT` fails the read with `TooLarge` and a `warn!`. `more` restarts `until`.
   - The last piece completes the read: Command → `transcript_reply`; Body → `body_done`.
   - `Title` → `on_title`, the old `Done::Title` logic.
   - `Calls` → a `Scan` built from the answer. If `more` and the reader is still there, merge it into the index and ask again from `offset`; otherwise `on_scan`, the old `Done::Index` logic.
   - Any other answer → `read_failed(purpose, failure(&answer))`.
9. `fn read_failed(purpose, why)`: Command → `commands::unavailable(why, session)` plus `info!` with short id and `why`; Title → forget `reading`; Calls → `on_scan(Scan::nothing(path, from))`, so candidates back off as before; Body → `body_done(body_text(input, None, None))`.
10. `fn fail_reads_of(conn)`, called first in `AgentEvent::Disconnected` and right after `bound.leaving = true` in `on_update_answer` (`:3766-3773`). `on_tick` fails reads whose `until` has passed with `NoAnswer`.
11. `read_title(session, path)`: no reader → return (the title stays the short id). Otherwise the same `reading`/`scanned` logic, then `ask_read(Title{from})`.
12. `check_candidates`: for each due session not indexing: no reader → `open_from_stops(session)` then `match_candidates`; empty path → `match_candidates`; otherwise `ask_calls(conn, session, path, from)`, which sets `indexing`.
13. `open_from_stops(session)`: for `candidates.take_stopped(session)` run `confirm_subagent(id, session, header(id, Some(stop.agent_type), None))`, then `finish_block(Agent(id), body_text(BodyInput{agent_type, report: reports.take(id), last: stop.last, header, ..}, None, None))` and `info!("subagent block opened from its stop")`.
14. `start_body_reads`: for each waiting input, if there is no report and the parent has a reader and `agent_path` is not empty, insert `bodies_reading` and `ask_read(Subagent{agent_id, agent_type, description, header, last})` to the parent's session with `path = agent_path`. Otherwise `finish_block(body_text(input, None, None))` at once. `body_done(agent_id, text)` keeps the old stale rule: show only when no newer stop waits, then start more.
15. `on_transcript_ask(ask)`: `commands::resolve`, then `transcript_reader(session)`, then `ask_read(Render{view.wire(), prompts})`. `transcript_reader` returns `Err(Unavailable)` in this order: Nested, Ended, NoAgent (no open non-leaving bound conn of that session), OldAgent (bound conn without `session_reads`), NoTranscript (empty path). A failed ask gets `unavailable(...)` and `info!("transcript not available")` with short id and reason.
16. Module doc paragraph "Session files (TASK-034)".

Logs: short ids, `?why`, fixed text. Never a path or title.

### Step 10: `crates/cctg/src/hub/registry.rs`
- Delete `topic_view` and `TopicView` (`:1669-1688`); nothing uses them any more.

### Step 11: wiring and removals
- Delete `crates/cctg/src/hub/sessions.rs` and `pub mod sessions` in `hub/mod.rs`.
- `hub/mod.rs::run`: remove the `projects_dir` requirement (`:167-169`) and `PROJECTS_VAR` from the import. Build `let mut slots = Slots::new(..)`, `let transcript_asks = slots.transcript_asks();`, then `commands::serve(commands_rx, outbox, Arc::new(commands::Asks(transcript_asks)), me.username.clone())`.
- `hub/mod.rs` test `a_slow_command_does_not_hold_up_polling`: replace the `Gated` locator with a `Gated` `TranscriptSource` that awaits a `tokio::sync::Mutex<UnboundedReceiver<()>>` gate and then renders an in-memory jsonl (no temp file).
- `hub/config.rs`: delete `PROJECTS_VAR`, its doc line, `Config.projects_dir`, the home/projects computation (`:176-184`) and the `projects_dir` parts of `paths_have_defaults_and_overrides`. The test becomes state-dir default plus override.
- `tests/supervise_e2e.rs:417`: drop the `CCTG_PROJECTS_DIR` env line.
- All `Slots::new` callers (19 sites in `src/hub/slots.rs` tests and `tests/*.rs`): `let (slots, _view) = Slots::new(..)` becomes `let slots = Slots::new(..)`, and `.0` suffixes go. Remove the `view` field from the slots test `Rig` and the `rig.view` assertion (`slots.rs:8142-8145`; the saved-registry check above it stays).
- All `Register { .. }` struct literals (34 sites: `src/agent.rs`, `src/hub/ingress.rs`, `src/hub/slots.rs`, `src/wire.rs`, 9 files in `tests/`) get `session_reads: false` unless the test wants a reader.
- `docs/poc.md:128,147`: "`/brief` в теме слота читает агент сессии (путь транскрипта из хука, корень `CLAUDE_CONFIG_DIR`), hub файлы сессии не открывает, поэтому работает без настройки."

### Step 12: rewrite the slots unit tests that relied on the hub reading files
Add these helpers to the slots `tests` module:
- `projects(dir)`, `parent_file(dir, session, jsonl)` → `<dir>/projects/C--w/<session>.jsonl`.
- `agent_file(dir, session, agent, jsonl?, meta?)` → `<...>/<session>/subagents/agent-<agent>.jsonl`.
- `reads_register(session, pid)`.
- `Rig::files_agent(conn, session, pid)`: an agent task that answers every `SessionRead` with `crate::reads::answer(Some(<dir>/projects), ..)` and keeps other messages.
- `connect_reader(slots, conn, session)` and `answer_reads(slots, conn, from_hub, dir)` for the directly driven actor. `answer_reads` answers only the reads already queued, not the ones its answers trigger.
- `bound(&rig)`: waits for the alive icon (Create or EditTopic with `ICON_ALIVE`).

**Race rule:** agent registration and hooks reach the actor on separate channels. Every test that needs the reader bound before a hook calls `bound(&rig).await` after both the start hook and `files_agent`. Without it `a_stop_before_its_call_is_visible_still_gets_its_block` failed once in a full run.

Rewrites (file layout as above, reader agent added):
- `three_explicit_subagents_make_three_blocks_and_internal_agents_none`: same expectations; the INTERNAL agent still gets no block, through correlation.
- `a_stop_before_its_call_is_visible_still_gets_its_block`: the "no block before match" check counts Send/Edit ops (0) instead of all ops.
- `a_reply_to_a_subagent_block_goes_to_the_parent_with_its_agent_id`: A's inbound comes through the `files_agent` receiver; B's nested agent is `received(&mut rig, 0)`.
- `a_legacy_subagent_record_never_becomes_a_block`, `a_first_send_with_an_unclear_answer_is_not_sent_again`, `a_refused_first_send_is_tried_again`, `a_huge_call_description_still_fits_one_message`: add a reader; paths as above.
- `a_block_confirmed_after_its_session_ended_is_marked_lost` → `a_subagent_of_a_session_that_ended_before_its_match_gets_no_block` (new behaviour, OPEN_DECISIONS 11).
- `the_agent_calls_of_an_ended_session_are_forgotten`, `a_late_body_read_never_overwrites_a_newer_one`: directly driven with `connect_reader` + `answer_reads` instead of `drain_until` (delete `drain_until`).
- `a_title_scan_that_ends_after_session_end_keeps_nothing`: call `slots.on_title(A, "t.jsonl", None, 10)`.
- `a_title_less_transcript_is_scanned_only_past_the_last_scan`, `the_ai_title_replaces_the_short_id`: reader added. The second one creates the topic first, then the agent (icon edit), then Stop: three ops, the last a name-only edit.
- Delete `the_ai_title_is_found_past_the_head_of_a_long_transcript` and `a_title_scan_goes_on_from_where_the_last_one_stopped` (moved to `reads.rs`).

New slots tests:
- `without_an_agent_that_reads_a_block_opens_on_its_stop_from_the_hooks`: an old agent; no block before the stop; S1 → `↳ Explore S1\nDone.`, S2 with a handback → `↳ Plan S2\n<report>`; the agent was never asked.
- `without_an_agent_that_reads_the_title_stays_the_short_id`: no name edit; never asked.
- `brief_is_rendered_by_the_sessions_agent_or_says_why_not`:
  - none → "Запущенных сессий нет."; no agent → NoAgent; old agent → OldAgent;
  - a reader → body equals `render_full` exactly, file name `full-aaaaaaaa.txt`;
  - pieces `> one\n` + `two` join; more than `MAX_READ_TEXT` → TooLarge and the map is empty; `Missing` → "не найден";
  - nested → Nested; ended → ends with `claude --resume <id>`; nothing more was asked.
- `a_brief_read_out_when_its_agent_leaves_or_is_late_gets_a_notice`: `read_wait` 50 ms, then tick → "не ответил"; `UpdateAnswer::Reloading` → "прервалась" at once; the next worker's `Disconnected` → "прервалась"; `reads` is empty.
- `a_session_read_fails_on_timeout_and_on_a_lost_or_leaving_link` (block text): timeout → the stop's text; disconnect → the stop's text; a late answer of the closed conn changes nothing.

### Step 13: integration tests
- **New `tests/reads_e2e.rs`** (acceptance criterion 1, "the hub cannot see the path"). Real `cctg agent` via `common::cctg(home)`, `current_dir(<root>/w)`, `CLAUDE_CONFIG_DIR=<root>/cfg`. Real `serve_agents` + `Slots` + `Scheduler` + `commands::serve(Asks(slots.transcript_asks()))`, fake Transport.
  - Hooks carry `transcript_path = ../cfg/projects/C--qa-w/<id>.jsonl` and `agent_transcript_path = ../cfg/projects/C--qa-w/<id>/subagents/agent-<aid>.jsonl`. Relative to the agent's cwd they resolve; relative to the test process (the hub) they do not. `Session::write` asserts both facts.
  - `brief_title_and_blocks_come_from_the_agent_when_the_hub_cannot_see_the_files`: repeat prompt hooks until the ai-title is in a topic name (this also proves the agent is bound). Subagent start/stop → block `↳ Explore a…02: Explore crate\n• Bash: List source files\n• SubagentHandback\nReport handed back.`. `/brief` in the topic and `/full 1 0a16` in General equal the library render.
  - `old_agents_missing_agents_and_ended_sessions_get_notices`: no agent → NoAgent notice. A raw TCP agent registering without `session_reads` → OldAgent notice, a block from the stop alone, and it never receives `session_read`. After SessionEnd → notice ends with `claude --resume <id>`.
  - `a_read_the_agent_does_not_answer_gets_a_notice` (`read_wait` 500 ms, raw agent with `session_reads` that never answers).
  - `a_read_whose_agent_goes_away_gets_a_notice_at_once` (`read_wait` 600 s, raw agent closes after the ask → "прервалась"). This is the TASK-040 swap case: the old worker's link closes with the read out.
- **New `tests/hub_reads_no_files.rs`** (acceptance criterion 2): scans every `src/hub/*.rs` except `config.rs`, `offset.rs`, `registry.rs`, `testdir.rs`, up to `#[cfg(test)]\nmod tests`, for `std::fs`, `tokio::fs`, `File::`, `OpenOptions`, `read_dir`, `read_to_string`, `canonicalize`, `crate::tail`, `reads::answer` (comments ignored). It checks at least 14 files and includes a self-test.
- **Rewrite `tests/command_logs.rs`**: global subscriber. Real `Slots` with `transcript_asks`, a fake agent answering through `cctg::reads::answer` from a projects dir named with a private marker, and a second session without an agent. `/brief 5e55`, `/full 1 5e55`, `/brief dead`, `/<marker>`. Logs contain "transcript command answered", "transcript not available", both short ids and "unknown slash command", and never the marker. Three notices or replies, none with the marker.
- **Update `tests/slots_logs.rs`**: register an agent with `session_reads: true` that answers the `Title` ask with the private title. Assert an `EditTopic` carries the title (so the title really went through) while the logs still never contain it.

### Step 14: verification (the implementer runs; all were green on the reference)
- `CARGO_TARGET_DIR=C:/Users/user/dev/cctg/target CARGO_PROFILE_DEV_DEBUG=0`, `-j 1`, one cargo at a time.
- `cargo fmt --all -- --check`; `cargo clippy -j 1 -p cctg --all-targets` (no warnings; `ask_read` returns `()` because a `Result<(), Purpose>` hits `clippy::result_large_err`).
- `cargo test -j 1 --workspace`; run `cargo test -p cctg --lib hub::slots` 3 times (race check).
- Fake soak: `cargo test -j 1 -p cctg --test soak -- --ignored` → `soak: ok` (the soak writes no ai-title and no subagents, so it is unaffected).
- Mutation check (`scratch/planner/mutations.sh`, in a throwaway copy): M1 `reader()` ignoring `session_reads` → `old_agents_…` fails; M2 subagent gate without the session-name check → 2 `reads::` tests fail; M3 `Disconnected` without `fail_reads_of` → `a_read_whose_agent_goes_away…` fails; M4 the hub reading the transcript for the title → guard fails.

## 4. Risk areas

- **R1: behaviour change for sessions without a reading agent** (accepted by the orchestrator's decision). No ai-title and no `/brief` for them. Blocks open only on the stop (no "в работе…" block). A typed stop that left files but was never an `Agent` call gets a block; this used to be filtered by correlation. The known source is an `--agent` session's main agent, which `hook.rs:539-541` says leaves no files. Old agents degrade the same way until updated through TASK-040's ⬆️ button (the notice says so).
- **R2: dead-session `/brief` regression.** Before, `/brief` of yesterday's session worked from the file; now it is a notice. Any-agent-on-the-device serving would break the "only files of its own session" rule. A brief cache was rejected (OPEN_DECISIONS 2).
- **R3: `/brief <prefix>` and General only find sessions the hub knows.** `registry.json` prunes ended sessions beyond `MAX_SESSIONS`. Sessions older than cctg or pruned are "not known". The ambiguous list loses the file age (the registry has no timestamps) and shows running/ended instead.
- **R4: pending-read leaks or double answers.**
  - Every `Pending` leaves the map through exactly one of: its final answer, a non-text answer, `read_failed` on timeout, disconnect, leave, or a failed `try_send`.
  - The command oneshot is consumed inside `Purpose::Command`, so it is answered at most once, and the worker's 120 s timeout covers a stopped actor.
  - `start_body_reads` → `ask_read` → `read_failed` → `body_done` → `start_body_reads` recurses, bounded by the waiting bodies. Keep it; the reference tests cover it.
- **R5: TOCTOU on the gate.** A junction or symlink swapped between `canonicalize` and `File::open` could redirect the open. Mitigation: open the canonical path (as `tail.rs` does). Exploiting it needs write access to `~/.claude/projects`, which already means owning the device. A hard link to an outside file inside the projects dir is not detectable by canonicalization (same precondition). Documented, not fixed.
- **R6: link load.** A 16 MiB `/full` is 128 lines of ≤128 KiB on the agent outbox (queue 256). It delays other agent frames of that session, such as permission requests, by the transfer time: milliseconds on loopback, maybe seconds over Tailscale later. Writes are capped at 5 s each (`agent.rs:78`). If this becomes visible, lower `MAX_TEXT` or send a render as a document by reference; out of scope now.
- **R7: memory.** The hub holds at most one command read (the sequential worker) plus 2 body reads plus titles and calls, each ≤16 MiB or small. The agent renders like the hub did (256 MiB input cap, parsing ~×2). The agent is now the process that briefly uses that memory: one per session, and only on `/brief`.
- **R8: test races.** Hooks versus agent registration on separate channels (see the Step 12 race rule). The e2e waits on observable effects (title, block, notices), never on sleeps alone.
- **R9: `Register` literal churn** (34 sites) and `Slots::new` signature churn (19 sites) touch many test files mechanically. A missed site is a compile error, not a silent bug.
- **R10: the stream is untouched.** `transcript_read` and `tail.rs` behaviour are unchanged apart from the extracted helper. Existing `tail` and `stream_e2e` tests must pass unchanged, and did on the reference.

## 5. Open questions

None blocks implementation; all design choices are decided in OPEN_DECISIONS 2-11. For the user's attention, not for the implementer:

1. Should an ended session's `/brief` ever be answerable, for example by another running agent of the same device reading that session's file with an explicit "same device, any session" rule? That would relax the "only its own session" gate the task sets. Proposed as a follow-up, not in this task.
2. PCTX proposals were filed in `PCTX_PROPOSALS.md`: the hub-domain invariant "Local transcripts are read directly from `transcript_path`" and the transcript-domain "IO lives in hub" become false with this task.

## Sources

- PortSwigger Web Security Academy, path traversal prevention (canonicalize, then verify the base): https://portswigger.net/web-security/file-path-traversal
- PVS-Studio V5332 (OWASP path traversal): https://pvs-studio.com/en/docs/warnings/v5332/
- Rust `std::fs::canonicalize` (resolves symlinks; Windows `\\?\` form): https://doc.rust-lang.org/std/fs/fn.canonicalize.html
- soft-canonicalize (TOCTOU and symlink notes): https://docs.rs/soft-canonicalize
- Request correlation, bounded lifetimes, fail pending on disconnect: https://github.com/microsoft/agent-host-protocol/issues/470, https://www.json-rpc.dev/learn/best-practices, https://jsonic.io/guides/json-rpc-guide
