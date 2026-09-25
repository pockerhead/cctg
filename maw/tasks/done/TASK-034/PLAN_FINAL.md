# PLAN_FINAL — TASK-034: hub never reads session files (client/server split, part 1)

Base: repo HEAD `6024465` (branch `feature/agent-serves-transcripts`). Line numbers below refer to that tree.

The definitive reference is the planner's reference plus six fixes and new tests from this review. It was built and tested outside the repo (a `%TEMP%` clone, shared `target/`). Evidence lives in `maw/tasks/in_progress/TASK-034/scratch/reviewer2/`:

- `reference.patch`: `git diff 6024465` of the fixed tree. `git apply --check` passes on `6024465`. Its sha256 is in `reference.patch.sha256`.
- `reference.sha256`, `reference.deleted`, `verify_hashes.sh`: per-file hashes (CR removed). From the repo root run `bash maw/tasks/in_progress/TASK-034/scratch/reviewer2/verify_hashes.sh`. It prints `same`/`differs`/`MISSING`/`PRESENT` per file and exits 0 only when the tree equals the reference.
- `mutations.sh` + `mutations.log`: 23 mutants. 21 are killed. The two survivors are equivalent in effect (see §5).

Results on the fixed reference: `cargo fmt --all -- --check` is clean. `cargo clippy -j 1 -p cctg --all-targets` gives no warnings. `cargo test -j 1 --workspace` is green. `cargo test -p cctg --lib hub::slots` passed 3 runs in a row. The fake soak printed `soak: ok`. Exact numbers are in §3. Patch sha256 `823565e0549454f3d8f1b8c68e46afa7b13b0fe5118d68e16cf1975c350d0d2a`; a fresh clone of `6024465` plus the patch passes `verify_hashes.sh` (exit 0).

Supersedes `PLAN.md` and `scratch/planner/`. Do not apply the planner's patch.

## 1. Summary

The hub stops opening any file that belongs to a session's machine. Today it reads files for `/brief`, `/full`, the ai-title, TASK-015 subagent correlation and subagent block text. All of that moves to the session's own agent over the existing agent link.

There is one new request/answer pair behind one new `Register` capability, and `wire::VERSION` stays 1:

- `HubMsg::SessionRead{read_id, session_id, path, ask}` with `ask` one of `render | title | calls | subagent`.
- `AgentMsg::SessionAnswer{read_id, answer}` with `answer` one of `text | title | calls | missing | unreadable | too_large | unsupported`.
- The capability is `Register.session_reads`. Unknown kinds decode as `Other`.

The hub keeps all state and policy: the pending-read map, timeouts, the correlation index and backoff, title offsets, block bookkeeping, and the sequential command worker. The agent does only file IO and parsing. That code is in the new `crates/cctg/src/reads.rs`, behind the same canonical path gate as the TASK-016 stream (`tail::open_transcript`), plus a subagent-file gate.

Some sessions have no agent that reads: ended, nested, headless or unlinked, or an old agent. They degrade as TASK_FINAL describes:

- `/brief` answers a notice that says why.
- The title stays the short id.
- A subagent block opens on its `SubagentStop` from hook data alone.

`hub/sessions.rs`, `CCTG_PROJECTS_DIR` and `TopicView` go away. A source-scanning guard test enforces "no file reads in `hub/`".

## 2. Implementation steps

### Step 0: apply the reference and check it

1. `git apply maw/tasks/in_progress/TASK-034/scratch/reviewer2/reference.patch` from the repo root, on `6024465` with a clean tree.
2. `bash maw/tasks/in_progress/TASK-034/scratch/reviewer2/verify_hashes.sh` must exit 0 (every line `same`, `crates/cctg/src/hub/sessions.rs` absent).
3. Build and test per §3.

The steps below are the contract the patch implements. A reviewer checks the diff against them. An implementer who writes the code by hand instead of applying the patch must meet every point, and `verify_hashes.sh` then shows where the tree differs.

### Step 1: `crates/cctg/src/wire.rs`, the new messages (additive, VERSION stays 1)

- `Register.session_reads: bool`, `#[serde(default)]`. An agent built before this change leaves it out.
- `AgentMsg::SessionAnswer{read_id: u64, answer: SessionAnswer}` (`"session_answer"` in `AgentMsg::KINDS`).
- `HubMsg::SessionRead{read_id: u64, session_id, path, ask: SessionAsk}` (`"session_read"` in `HubMsg::KINDS`).
- `SessionAsk` (`#[serde(tag = "kind", rename_all = "snake_case")]`):
  - `Render{view: TranscriptView, prompts: u32}`, `Title{from: u64}`, `Calls{from: u64}`
  - `Subagent{agent_id, agent_type?, description?, header?, last?}`
  - `#[serde(other)] Other`
- `SessionAnswer`, tagged the same way:
  - `Text{text, #[serde(default)] more}`, `Title{title?, scanned}`, `Calls{offset, calls: Vec<SpawnCall>, links: Vec<SpawnLink>, more}`
  - `Missing`, `Unreadable`, `TooLarge`, `Unsupported`, `#[serde(other)] Other`
- `TranscriptView{Brief, Full}`, `SpawnCall{id, subagent_type?, description?}`, `SpawnLink{agent_id, tool_use_id}`. Optional fields are `skip_serializing_if = "Option::is_none"`.
- Add the pair to the module-doc capability list.
- Tests:
  - samples in `agent_samples`/`hub_samples`, and `session_reads: false|true` in every `Register` literal;
  - `session_reads_stay_compatible_with_version_one_peers`: an old register decodes `false`; an unknown ask or answer kind decodes as `Other`; a last piece without `more` decodes `more: false`.

### Step 2: `crates/cctg/src/tail.rs`, share the gate (no behaviour change)

`open_transcript` becomes `pub(crate)`. Its id check moves into `pub(crate) fn is_plain_session_id(&str) -> bool` (non-empty, ≤64, ASCII alphanumerics and `-`). The existing `tail` tests pass unchanged.

### Step 3: new `crates/cctg/src/reads.rs` (agent side) and `pub mod reads;` in `lib.rs`

- `pub fn answer(root: Option<&Path>, session_id, path, ask) -> Vec<SessionAnswer>` (blocking) and `pub fn pieces(&str) -> Vec<SessionAnswer>`.
- Constants: `MAX_TRANSCRIPT_BYTES` 256 MiB, `MAX_AGENT_BYTES` 64 MiB, meta 64 KiB, `MAX_TEXT` 16 MiB, `PIECE` 128 KiB, `MAX_CALLS_WEIGHT` 64 KiB (+32 per entry), field cut 256 UTF-16, id ≤256, title cut 1024, `MAX_PROMPTS` 100.
- `root == None` gives `[Missing]`. `Other` gives `[Unsupported]`.
- `Render`:
  1. `open_transcript` (None gives `Missing`).
  2. `read_limited` (over the cap gives `TooLarge`, an IO error gives `Unreadable`).
  3. `transcript::parse` on lossy UTF-8, then `last_prompts(prompts.clamp(1, 100))`, then `render_brief`/`render_full`.
  4. `pieces`. The result equals the library output exactly.
- `Title{from}`: seek, then `first_ai_title` (moved from `slots.rs`), which streams complete lines up to `256 MiB - from`. Answer `Title{title: cut(1024), scanned: from + complete-line bytes}`.
- `Calls{from}`: `scan_lines` (moved from `subagents.rs`):
  - `"Agent"`/`agentId` prefilter;
  - stops at `MAX_CALLS_WEIGHT` with `more: true`;
  - a torn last line is left for the next ask.
- `Subagent`:
  1. `is_agent_id` and `is_plain_session_id`, else `[Missing]`.
  2. `open_subagent_file` for `agent-<id>.jsonl` and its `.meta.json`. Each opens only when the canonical path is exactly `<canonical root>/<one component>/<session_id>/subagents/<exact name>` and is a file. A gate failure means "no file".
  3. `hub::subagents::body_text(input with report None, meta, transcript)`, then `pieces`.
- Nothing in `reads.rs` logs.
- Tests (module `reads::tests`):
  - library equality on the 7 fixtures;
  - **new: `a_render_shows_at_most_the_last_hundred_prompts`**;
  - pieces fit a link line even fully escaped, `more` only on non-last, `TooLarge` over 16 MiB;
  - missing/foreign/`..`/other id/traversal id/no root/a directory named like a transcript give `Missing`;
  - `read_limited`;
  - title past a 5 MiB head, from an offset, torn line;
  - calls found and torn line;
  - 2000 calls in several batches, each under `MAX_LINE`;
  - block text from the session's files. Every foreign path falls back to the stop's last message: another session id, a file beside `subagents/`, another agent's name, `..`, **the same layout outside the root (new)** and **a session folder other than `subagents` (new)**. The fallback case now passes `last` equal to the subagent's final text, so a file that was wrongly read would show its tool lines. Before this change a read file and the fallback rendered the same text, so a bypass went unnoticed.
  - agent id `../x` gives `Missing`;
  - `#[cfg(windows)]` **new `case_and_separators_name_the_same_file_but_the_id_must_be_the_files`**: a path with `/` and upper case reads the same file; an upper-cased id gives `Missing`;
  - `#[cfg(windows)]` junction out of the session is not followed.

### Step 4: `crates/cctg/src/hub/subagents.rs`, drop file IO

- Delete `MAX_TRANSCRIPT_BYTES`, `MAX_AGENT_BYTES`, `MAX_META_BYTES`, `MAX_CALL_FIELD`, `AGENT_TOOL`, `scan`, `scan_lines`, `read_body`, `read_capped` and their imports.
- Keep `Scan`, `AgentIndex`, `Candidates`, `Reports`, `BodyInput` and `body_text` (pure).
- Add `Candidates::take_stopped(session) -> Vec<(String, Candidate)>`: candidates of that session that have a stop, sorted by id.
- Tests: the index tests now use hand-built `Scan`s. The file-reading test moved to `reads.rs`.

### Step 5: `crates/cctg/src/agent.rs`, serve session reads

- `Register{.., session_reads: true}` in `run_stdio`.
- `spawn_session_reader(outbox, projects) -> mpsc::Sender<(u64, String, String, SessionAsk)>`:
  - queue 8 (`SESSION_READS`);
  - one read at a time on `spawn_blocking(reads::answer)`;
  - each answer is `outbox.send`-ed in order;
  - it is separate from the stream's `spawn_reader`.
- New `serve_channel` arm for `HubMsg::SessionRead`: `try_send`; when the queue is full, a `debug!` and a drop (the hub's wait runs out). Nothing reaches Claude Code.
- Test `session_reads_are_answered_in_pieces_over_the_link_and_never_reach_claude`.

### Step 6: `crates/cctg/src/channel.rs`

Add `HubMsg::SessionRead { .. }` to the "agent loop's, not the channel's" arm. The match is exhaustive.

### Step 7: `crates/cctg/src/hub/ingress.rs`

Add `| AgentMsg::SessionAnswer { .. }` to the forwarded list (TASK-016 QA lesson). `tests/reads_e2e.rs` covers it over real TCP.

### Step 8: `crates/cctg/src/hub/commands.rs`, the worker asks the actor

- Remove `read_limited`, `relative_age`, `candidate_title`, `locate_notice`, `prepare`, `prepare_limited`, `MAX_TRANSCRIPT_BYTES`, `CANDIDATE_TITLE_BYTES` and the `sessions` import.
- Add:
  - `trait TranscriptSource` (`prepare(thread_id, command) -> impl Future<Output = Prepared> + Send`);
  - `struct TranscriptAsk{thread_id, command, answer: oneshot::Sender<Prepared>}`;
  - `struct Asks(pub mpsc::Sender<TranscriptAsk>)`: a send error or `ANSWER_WAIT` (120 s, a guard against an actor bug) gives the `NO_ACTOR` notice;
  - `View::wire()`;
  - `resolve(&Registry, thread_id, prefix)`, working over registry sessions only:
    - a prefix match: none, one, or a newest-first list of at most 10 with `идёт|завершена[ · title]`;
    - else a slot topic's `current_session`;
    - else the newest running top-level session;
  - `enum Unavailable{Ended, Nested, NoAgent, OldAgent, NoTranscript, Missing, Unreadable, TooLarge, NoAnswer, LinkLost, Failed}` and `unavailable(why, id)`: short id only, and only `Ended` adds `claude --resume <full id>`;
  - `transcript_reply(command, id, body)`: blank gives "пока нечего показывать"; otherwise a `Reply` with the same file name and caption as before.
- `handle`/`serve` become generic over `S: TranscriptSource`, and `handle` awaits `source.prepare` (no `spawn_blocking`).
- Update `USAGE` and the module doc.
- Tests:
  - `only_our_commands_are_commands`, `parses_arguments`;
  - the delivery tests over a fake source;
  - `a_command_finds_its_session_in_the_registry`, `notices_name_the_short_id_only`, `a_stopped_actor_gives_a_notice`.

### Step 9: `crates/cctg/src/hub/slots.rs`, pending reads in the actor

1. Remove `std::io` imports, `TITLE_SCAN_BYTES`, `read_title()`/`first_ai_title()`, `Done::{Title, Index, Body}` with their arms, the `view` watch and its publishing in `pump`. `Slots::new` returns `Self`.
2. `Options.read_wait`, default `READ_WAIT = 20 s`. `MAX_READ_TEXT = crate::reads::MAX_TEXT`. `Conn.session_reads` comes from `Register`.
3. State:
   - `transcript_asks: Option<mpsc::Receiver<TranscriptAsk>>`;
   - `reads: HashMap<u64, Pending{conn, until, purpose}>`;
   - `enum Purpose{Command{ask, session, text}, Title{session, path}, Calls{session, path, from}, Body{input, text}}`.

   **There is no read-id counter** (see 5).
4. `transcript_asks()` (queue 16) is polled in `run`'s `select!`. `next_deadline` includes the earliest `reads.until`.
5. `ask_read(conn, session, path, ask, purpose)`:
   - **`read_id = crate::wire::random_u64()`**, as for `update_id`/`key_id`/`command_id`. A per-run counter is wrong: the agent's outbox keeps answers it could not write and sends them after the next registration (`agent.rs:9-12`). After a hub restart, a stale answer for read 1 took the new run's read 1 (reproduced, §5 D1).
   - `try_send(HubMsg::SessionRead)`. A full or closed queue calls `read_failed(purpose, LinkLost)` at once. Otherwise insert `Pending` with `until = now + read_wait`.
6. `reader(session)`: `entry.agent` is set, the conn is open, `session_reads` is true, it is not `leaving`, and `bound.session == session`.
7. `on_session_answer(conn, read_id, answer)`:
   - only the conn that was asked counts; anything else is a `debug!` and a drop;
   - `Text` pieces append, and more than `MAX_READ_TEXT` fails the read with `TooLarge` and a `warn!`;
   - each `more` piece restarts `until`;
   - the last piece completes the read;
   - `Title` goes to `on_title`;
   - `Calls` builds a `Scan`: with `more` and the reader still bound, merge it and ask again from `offset`; otherwise `on_scan`;
   - any other answer calls `read_failed(purpose, failure(&answer))`.
8. `read_failed`:
   - Command: `unavailable(why)` and an `info!` with the short id and `?why`;
   - Title: forget `reading`;
   - Calls: `on_scan(Scan::nothing)`, so backoff goes on as before;
   - Body: `body_done(body_text(input, None, None))`.
9. `fail_reads_of(conn)` runs first in `AgentEvent::Disconnected` and right after `leaving = true` in `on_update_answer`. `on_tick` fails reads past `until` with `NoAnswer`.
10. `read_title`: with no reader it returns and the title stays the short id. Otherwise the same `reading`/`scanned` logic as before, then `ask_read(Title{from})`. It is asked only while the title is unknown, on `UserPromptSubmit`/`Stop` (`registry.rs:1059-1066`), incrementally from the last scanned offset.
11. `check_candidates`, for each due session that is not indexing:
    - no reader: `open_from_stops` then `match_candidates`;
    - empty path: `match_candidates`;
    - otherwise `ask_calls`.
12. `open_from_stops(session)`: `candidates.take_stopped`, then `confirm_subagent` with `header(id, Some(stop.agent_type), None)`, then `finish_block(body_text(report or last))` and `info!("subagent block opened from its stop")`.
13. `start_body_reads`: a report, a parent without a reader, or an empty `agent_path` gives `finish_block(body_text(input, None, None))` at once. Otherwise `ask_read(Subagent{..})` to the **parent** session's reader with `path = agent_path`. `body_done` keeps the "newest stop wins" rule.
14. `on_transcript_ask`: `resolve`, then `transcript_reader`, then `ask_read(Render)`. `transcript_reader` checks in this order: `Nested`, `Ended`, `NoAgent`, `OldAgent`, `NoTranscript`.
15. Logs carry short ids, `?why` and fixed text: never a path, a title or a text.
16. Tests: see Step 12 and §3.

### Step 10: `crates/cctg/src/hub/registry.rs`

Delete `topic_view` and `TopicView`.

### Step 11: wiring and removals

- Delete `crates/cctg/src/hub/sessions.rs` and `pub mod sessions`.
- `hub/mod.rs::run`:
  - drop the projects-dir requirement and `PROJECTS_VAR`;
  - `let mut slots = Slots::new(..)`, `let transcript_asks = slots.transcript_asks();`, `commands::serve(.., Arc::new(commands::Asks(transcript_asks)), ..)`;
  - test `a_slow_command_does_not_hold_up_polling` uses a gated `TranscriptSource`.
- `hub/config.rs`: delete `PROJECTS_VAR`, `Config.projects_dir` and the home computation. `paths_have_defaults_and_overrides` keeps only the state dir.
- `tests/supervise_e2e.rs`: drop the `CCTG_PROJECTS_DIR` env line.
- Mechanical edits: all 19 `Slots::new` call sites drop the view, and all 34 `Register { .. }` literals get `session_reads`.
  - Both are forced. `Register` literals are exhaustive struct literals, so a new field is a compile error in each. The view has no reader left after `sessions.rs` goes, and the surgical-change rule says to remove what this change made unused.
- `docs/poc.md:128,147`: `/brief` is read by the session's agent.

### Step 12: slots unit tests

This follows the planner's Step 12: helpers `projects`, `parent_file`, `agent_file`, `reads_register`, `Rig::files_agent`, `connect_reader`, `answer_reads` and `bound`, the race rule, and the listed rewrites and new tests. Reviewer additions:

- **`an_answer_left_from_an_earlier_hub_run_answers_no_read`** (D1): two `Slots` runs. The first run's read id, answered in the second run on the same conn number, must not answer the second run's read.
- **`a_long_answer_keeps_its_read_alive_piece_by_piece`**: `read_wait` 1 s, and three pieces 600 ms apart, so the read takes longer than the wait. The read is still answered, with the joined text. An answer with the right `read_id` from **another registered link** (conn 3) is ignored (kills mutant C1).

### Step 13: integration tests

- **`tests/reads_e2e.rs`** as planned. The real `cctg agent` runs with its own home and `CLAUDE_CONFIG_DIR`. Hooks give paths relative to the agent's cwd, so the hub cannot see them (`Session::write` asserts both facts). Four tests:
  - brief, title and blocks over the link;
  - old agent, no agent and ended sessions get notices;
  - a silent agent gets `NoAnswer`;
  - a closed link gets `LinkLost` at once (the TASK-040 swap).
- **`tests/hub_reads_no_files.rs`, rewritten** (D2). It scans every `src/hub/*.rs` except `config.rs`, `offset.rs`, `registry.rs` and `testdir.rs`, up to `#[cfg(test)]\nmod tests`, with `//` comments stripped.
  - Whole identifiers `fs`, `OpenOptions`, `read_dir`, `read_to_string`, `canonicalize`, `metadata`, `symlink_metadata`, `exists`, `try_exists`, `is_file`, `read_link`, `tail`, `spool`, `proctree` and `device` are forbidden.
  - The substrings `File::`, `reads::answer`, `reads::pieces`, `files::save`, `files::read_upload` and `files::inboxes` are forbidden.
  - It checks at least 14 files.
  - Self-test `the_guard_sees_a_file_read` covers `std::fs::read`, `use std::{fs as disk}`, `tokio::fs`, `File::open`, `.exists()`, `.metadata()`, `crate::reads::answer`, `crate::spool::…`, and no false positive on `api::File`, `crate::reads::MAX_TEXT` or comments.
  - The planner's token list missed `use std::{fs as disk}; disk::metadata(p)` inside `slots.rs` (reproduced, mutant F2).
  - Remaining `spawn_blocking` calls in `hub/` are own state only: `slots.rs` registry save, `updates.rs` offset save, `mod.rs` `client::own_build`.
- **`tests/command_logs.rs`** (rewritten) and **`tests/slots_logs.rs`** (updated), as planned: logs never carry the private marker or the title.

## 3. Test plan

All commands run from the repo root with `CARGO_TARGET_DIR=C:/Users/user/dev/cctg/target CARGO_PROFILE_DEV_DEBUG=0`, `-j 1`, one cargo at a time. If a process test fails only because another agent rebuilt `target/debug`, touch the crate and rerun.

| What | Command | Expected |
|---|---|---|
| Tree equals reference | `bash maw/tasks/in_progress/TASK-034/scratch/reviewer2/verify_hashes.sh` | exit 0 |
| Format | `cargo fmt --all -- --check` | clean |
| Lints | `cargo clippy -j 1 -p cctg --all-targets` | no warnings |
| Everything | `cargo test -j 1 --workspace` | all green: 38 test binaries, cctg lib 554 passed / 1 ignored on the reference |
| Slots race check | `cargo test -j 1 -p cctg --lib hub::slots` ×3 | green 3/3 (157 passed each on the reference) |
| AC1 (hub cannot see the files) | `cargo test -j 1 -p cctg --test reads_e2e` | 4 passed |
| AC2 (no file reads in `hub/`) | `cargo test -j 1 -p cctg --test hub_reads_no_files` | 2 passed |
| AC3 (gate) | `cargo test -j 1 -p cctg --lib -- reads:: tail::` | green, incl. junction and case tests on Windows |
| AC4 (old agent, VERSION 1) | `wire::tests::session_reads_stay_compatible_with_version_one_peers`, `reads_e2e::old_agents_missing_agents_and_ended_sessions_get_notices`; `grep -n "pub const VERSION" crates/cctg/src/wire.rs` | green; `VERSION: u32 = 1` |
| AC5 fake soak | `cargo test -j 1 -p cctg --test soak -- --ignored` | prints `soak: ok` |
| Mutation check (optional, in a throwaway clone with the patch committed) | `bash .../scratch/reviewer2/mutations.sh` | every mutant but G5 and C5 killed, as in `mutations.log` |

What the key tests prove:
- Pieces and batches always fit `wire::MAX_LINE`, even fully escaped.
- The hub caps a read at 16 MiB and drops a longer one.
- A read fails on timeout, on link close and on the TASK-040 leave.
- A stale or foreign answer never completes a read.
- Correlation still gives exactly the planned blocks with a reader, and blocks from the stop without one.
- `/brief` output equals `transcript::render_*` byte for byte.

## 4. Rollout notes

- **No migration.** The `registry.json` format is unchanged. `Slots` read state is in memory only. `CCTG_PROJECTS_DIR` is no longer read; a leftover value in an env file is ignored. The hub no longer needs a home directory to start.
- **Wire:** additive only, and `VERSION` stays 1.
  - An old agent with a new hub: no `/brief` (the "старой версии, обновите ⬆️" notice), no ai-title, subagent blocks only on their stop.
  - A new agent with an old hub: works as before, since the old hub reads files itself.
- **Order.** The hub is the build reference for TASK-040 updates, so update the hub first. Then each live session's agent through the topic's ⬆️ button, or new sessions. Until an agent is updated, its session degrades as above. This is accepted (TASK_FINAL, R1).
- **Accepted behaviour changes (R1-R3):**
  - Sessions without a reading agent get notices, keep the short-id title, and get blocks from stops only.
  - `/brief` of an ended session is a notice with `claude --resume <id>`.
  - `/brief <prefix>` and General find only sessions the registry knows, and the ambiguous list shows running/ended instead of file age.
  - A typed stop without an `Agent` call (an `--agent` session's own agent) can now get a block when there is no reader.
  - A subagent whose parent ended before its match and that never stopped gets no block.
- **Load and memory:**
  - A `/full` of a big session is parsed in the **agent** process, the child of the user's claude. It spikes like the hub did: ≤256 MiB input plus parse.
  - Only one render runs machine-wide at a time, because the hub's command worker is sequential.
  - Hub memory is at most one command text plus two block texts (≤16 MiB each).
  - A 16 MiB answer is 128 lines of ≤128 KiB on that session's link. It delays that session's other agent frames by the transfer time.
- **Known limits, not fixed:**
  - A read whose `try_send` finds the hub→agent queue (64) full, for example during a big file transfer to the agent, answers "Связь прервалась, повторите" at once.
  - A read trickling in slower than 120 s in total gives the worker's `NO_ACTOR` text ("hub останавливается") while the actor still collects. That needs ~128 KiB/s or slower for a full 16 MiB.
  - Titles are read once (as before) and never refreshed.
  - Hard links into the projects dir and a junction or symlink swap between `canonicalize` and `open` (TOCTOU) are not detected. Both need write access to `~/.claude/projects`, which already means owning the device.
- **Follow-up (not this task, orchestrator decision):** any live agent of a device serving a dead session's transcript.
- **PCTX:** the hub-domain bullets for TASK-009 and TASK-015 and the transcript-domain line "IO lives in hub" become false. The planner filed proposals in `PCTX_PROPOSALS.md`. One lesson from this review is not yet filed and is left for the orchestrator: "hub-minted ids on the agent link are `wire::random_u64`, never per-run counters, because the agent outbox replays unsent frames after a reconnect."

## 5. Review notes (changes from `PLAN.md` and why)

**Disconfirmation first.** The counter-example I tested: the hub names a session that is not the agent's own claude session (`HubMsg::SessionRead{session_id: B, path: <root>/<p>/B.jsonl}` sent to session A's agent). The agent serves B's transcript.

It held. `reads::answer` and `tail::open_transcript` never learn the agent's own session. They accept any plain id whose file sits at the gate's exact canonical shape.

**Decision: acceptable, kept.** Reasons:

1. TASK_FINAL defines "only files of its own session" as "тот же path gate", which is the TASK-016 gate. That gate takes the id from the request too.
2. After `/clear` the agent keeps its stale env `CLAUDE_CODE_SESSION_ID` (verified TASK-013). Only the hub knows the agent's current session, so any "own session" list on the agent side would come from the hub anyway.
3. The hub is trusted by design: it holds the secret, injects prompts and types into the console (TASK-043). The gate's job is to keep a hub bug from reaching files other than Claude session files.
4. The hub enforces ownership. `reader()` and `transcript_reader()` ask only the open, non-leaving agent that is bound to exactly that session and announced `session_reads`. Mutants M1, C2 and C4 guard this.

Stricter options were rejected: an agent-side cwd-project check (encoding rules, resume from another folder) and an agent-side bound-session list (it comes from the hub itself).

Defects found, each reproduced by a failing test before the fix:

- **D1 (real bug): stale answers after a hub restart took new reads.** `read_ids` was a per-run counter starting at 1. The agent's outbox keeps answers it could not write and sends them after the next registration (`agent.rs:9-12`), possibly to a restarted hub whose conn numbering also restarted. Any stale `session_answer` then completed the new run's read with the same id. The effects: a `/brief` answered with another command's text, or an index offset from an old `calls` batch that skips `Agent` calls, which loses subagent blocks.
  - Fix: `read_id = crate::wire::random_u64()`, the existing pattern for `update_id`/`key_id`/`command_id`, and the `read_ids` field is removed.
  - Test: `an_answer_left_from_an_earlier_hub_run_answers_no_read`. It failed on the planner's reference.
- **D2: the AC2 guard missed file reads.** `use std::{fs as disk}; disk::metadata(p)` and `path.exists()` in `hub/slots.rs` passed the planner's guard, whose substring list lacked `fs` aliases and file probes.
  - Fix: whole-identifier matching plus agent/hook module names; the self-test covers the forms.
  - Mutant F2 is killed now. It survived before.
- **D3: the subagent-gate test could not see a bypass.** Its fallback cases used `last = "Final."`, and with no meta a wrongly read file renders the same `↳ agent <id>\nFinal.`. A mutant without the root check survived (G2), and so did one without the `subagents` check (G3). Only the junction test and the id check held the gate.
  - Fix: fallback cases use the subagent's real final text, so a read file shows its tool lines, plus two new foreign layouts (outside the root, another session folder).
- **D4: the 100-prompt clamp in the agent was untested (L4 survived).** Added `a_render_shows_at_most_the_last_hundred_prompts`.
- **D5: "only the conn asked may answer" was untested (C1 survived), and so was "each piece restarts the wait" (L5).** Added `a_long_answer_keeps_its_read_alive_piece_by_piece`.
- **D6: Windows case and separator spelling of the gate was untested.** Added a `#[cfg(windows)]` test: another spelling of the same file reads it, and an id cased unlike the file is refused.

Mutants: the planner had 4. There are now 23: M1-M4, F1-F2, gate G1-G5, limits L1-L5, correlation, timeouts and links C1-C6. Two survive, both equivalent in effect:

- **G5:** `is_plain_session_id` dropped from `open_transcript`. An id with a separator never equals a canonical file name. The check stays as defence in depth.
- **C5:** calls past a full batch are read at the next lookup from the index offset instead of at once: later, not lost.

Checked and left as the planner had it:

- **Path gate:**
  - canonicalize both sides, exact shape (depth-1 project folder), exact file name, `is_file`, open the canonical path;
  - `..`, junctions, 8.3 names, `\\?\` and UNC spellings, and ADS all resolve to the file's own canonical name or fail;
  - Rust `std::fs::canonicalize` uses `CreateFile` + `GetFinalPathNameByHandle` and resolves links (https://doc.rust-lang.org/std/fs/fn.canonicalize.html);
  - the hub sends lowercase hook ids, so an id cased unlike the file fails closed.
- **Read limits and memory:**
  - bounded per purpose: one title and one calls read per session (`reading`/`indexing`), 2 body reads, 1 command (sequential worker), ask queue 16;
  - the agent queue of 8 is enough for that load and drops beyond it (the hub's wait then fails the read).
- **TASK-040 swap mid-read:** `fail_reads_of` runs on leave and on disconnect. The e2e and slots tests cover it.
- **TASK-015 split:**
  - offsets merge in the hub's `AgentIndex`, and `calls` batches come in order with `more`;
  - a failed or missing read counts as a miss, so backoff is unchanged;
  - a stop re-creates a dropped candidate, so long subagents still get blocks without a reader;
  - a parent that ended before its match gives the accepted OPEN_DECISIONS 11 behaviour.
- **ai-title cost:** asked only while unknown, on prompt/stop hooks, incrementally from the last scanned offset. It is never asked of an old agent.
- **Old agents and old hubs:** `#[serde(default)]` and `#[serde(other)]`, no `deny_unknown_fields` on `Register`, VERSION 1.
- **Logs:** new lines carry short ids, `?why` and fixed text. `reads.rs` logs nothing. `command_logs`/`slots_logs` assert no marker or title.
- **Size:** the 34 `Register` literal edits are forced by exhaustive struct literals. The 19 `Slots::new` edits follow from removing the now-unused view (surgical rule: remove what your change made dead). Nothing else was worth cutting:
  - `TranscriptSource` is ~25 lines and keeps `hub/mod.rs`'s slow-command test simple;
  - the `Unavailable` variants are distinct user answers.

Sources: Rust `std::fs::canonicalize` docs (above); the TASK-010/014 wire rules in `wire.rs:13-23`; `agent.rs:9-12` (outbox kept across reconnects); `slots.rs` `random_u64` uses for `update_id`, `key_id` and `command_id`.
