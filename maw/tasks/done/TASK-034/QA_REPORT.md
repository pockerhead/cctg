# QA_REPORT — TASK-034 (hub never reads session files)

Tested: branch `feature/agent-serves-transcripts` at `44852f4` (code `5cd323e`), change `git diff 8252a80 HEAD -- crates docs` (32 files). Main tree clean before and after.

## 0. Preflight and disconfirmation

- Read `scratch/` (planner, reviewer2, implementer, code-reviewer, fixer) as a coverage map only; no author script was rerun as evidence.
- Read TASK_FINAL, PLAN_FINAL, IMPL_SUMMARY, IMPL_REVIEW, FIX_SUMMARY, OPEN_DECISIONS (decision 12) and `log.jsonl` (two `dead_end` entries: the implementer's first gate, the review's verbatim guard allowlist; both checked against the code, both are as described).
- **Counter-example written first:** "the agent still opens a path the hub names (the `path` of `transcript_read`, or a `path` an earlier hub puts into `session_read`), or serves a transcript of another project folder when the hub names its session id".
  - Search: agent-side code (`agent.rs`, `tail.rs`, `reads.rs`) has no filesystem call on any hub field. `agent.rs` non-test code has no `std::fs`/`canonicalize`/`metadata` at all; `TranscriptRead { session_id, from, .. }` drops `path`; `HubMsg::SessionRead` has no `path` field (serde ignores it). Every open goes through `OwnProject::open`, which joins only ids checked by `is_plain_session_id`/`is_agent_id` under the own folder and compares the canonical path with `<canonical own folder>/<parts>`.
  - Probe: `qa_clear_after_find_serves_new_id_and_never_foreign` (below): a foreign project's session `S3` (transcript and subagent files with private content) asked through render, title and subagent ask gives `Missing` / the stop text only; the own folder's new id after `/clear` is served.
  - **Result: the counter-example did not hold.**

## 1. Environment

- No docker-compose, no dev server. Direct `cargo` test infrastructure, fake Bot API (the soak's default), no real Telegram, no `.env` values, no interactive claude, no windows. The live `~/.cctg` hub/supervisor were not touched.
- All builds: `CARGO_TARGET_DIR=C:/Users/user/dev/cctg/target CARGO_PROFILE_DEV_DEBUG=0`, `-j 1`, one cargo at a time.
- Adversarial tests ran in a `%TEMP%` clone (`/tmp/cctg-qa034-clone`, checkout `44852f4`), deleted afterwards. Because the clone shares `target/`, the main tree's `reads_e2e`/`hub_reads_no_files` binaries were rebuilt afterwards (touch + rerun: 6 and 3 passed, no `qa_` test left in them).
- Nothing was started that needs stopping.

Reproduce:
```
cargo fmt --all -- --check
cargo clippy -j 1 --workspace --all-targets -- -D warnings
cargo test -j 1 --workspace
cargo test -j 1 -p cctg --test reads_e2e
cargo test -j 1 -p cctg --test hub_reads_no_files
cargo test -j 1 -p cctg --test update_e2e
cargo test -j 1 -p cctg --test soak -- --ignored --nocapture
cargo test -j 1 -p cctg --lib -- tail:: reads::
cargo test -j 1 -p cctg --lib hub::slots        # x3
```
Logs: `scratch/qa/{fmt,clippy,workspace_test,named}.log`. QA test sources: `scratch/qa/qa_adversarial.rs.txt` (copied as `crates/cctg/tests/qa_adversarial.rs`) and `scratch/qa/reads_e2e_qa_append.rs.txt` (appended to `tests/reads_e2e.rs`), in the clone only.

## 2. Test results

| Run | Result |
|---|---|
| `cargo fmt --all -- --check` | exit 0 |
| `cargo clippy -j 1 --workspace --all-targets -- -D warnings` | exit 0, no warnings |
| `cargo test -j 1 --workspace` | exit 0: 38 test binaries, 738 passed, 0 failed, 3 ignored |
| `--test reads_e2e` | 6 passed |
| `--test hub_reads_no_files` | 3 passed |
| `--test update_e2e` | 1 passed (`a_new_binary_is_taken_without_losing_a_line`) |
| `--test soak -- --ignored` (fake Bot API) | `soak: ok` |
| `--lib -- tail:: reads::` | 29 passed (junction/case tests ran on Windows, no "mklink unavailable") |
| `--lib hub::slots` x3 | 161 passed each run |

New QA tests (clone):

| Test | What it proves | Result |
|---|---|---|
| `qa_clear_after_find_serves_new_id_and_never_foreign` | Own folder found through the env id's transcript in a folder NOT named after the cwd (`OwnProject::new`, not the test-only `at`); a new id there (after `/clear`) renders byte-equal to `transcript::render_brief`; a foreign project's id is `Missing` for render and title; a subagent ask for the foreign session gives the stop text only (no `Bash` tool line from the foreign file) | PASS |
| `qa_reserved_device_names_are_missing` | Session ids `NUL`, `CON`, `AUX`, `PRN`, `COM1`, `LPT1`, `nul` from a hub: `Missing`, no hang; the same as agent ids give the fallback text | PASS |
| `qa_edge_offsets_and_prompt_counts` | `title{from: u64::MAX/2}` gives `Title{None, scanned: from}`; `calls{from: u64::MAX}` no panic; `prompts` 0 clamps to 1 and `u32::MAX` to 100 (equal to the library); `Other` gives `Unsupported`; no project gives `Refused` | PASS |
| `qa_config_dir_through_a_junction_is_served` (Windows) | `CLAUDE_CONFIG_DIR` reached through a junction still serves the own folder | PASS |
| `qa_subagents_junction_to_foreign_project_is_not_followed` (Windows) | `<own>/<S1>/subagents` as a junction to another project's `subagents` is not followed | PASS |
| `qa_folder_name_short` | `C:\Users\user\dev\cctg` gives `C--Users-user-dev-cctg`; UNC spelling | PASS |
| `reads_e2e::qa_every_odd_answer_gives_a_notice` (real TCP to `serve_agents`, real `Slots` and command worker) | Agent answers `refused`, `missing`, `too_large`, `unreadable`, an unknown kind, and a `title` to a render ask: each gives one notice with the short id (texts printed, readable Russian); a stray `text` for another `read_id` sent first is ignored; a normal `text` works afterwards | PASS |

Guard mutations (clone, reverted):
- `Path::new(session).exists()` added to `hub/slots.rs::reader`: the guard FAILS as it should (positive control).
- `Path::new(session).is_dir() || Path::new(session).is_symlink()` added at the same place: the guard PASSES — see bug B1.

## 3. Acceptance criteria

| Criterion | Test performed | Result |
|---|---|---|
| AC1: `/brief`, `/full`, ai-title, subagent and nested blocks work through the agent when the hub cannot see the files; sessions without an agent degrade | `reads_e2e::brief_title_and_blocks_come_from_the_agent_when_the_hub_cannot_see_the_files` (real agent binary, own `CLAUDE_CONFIG_DIR`/home; hook paths relative to the agent's cwd, `Session::write` asserts the hub cannot see them): title, block, `/brief`, `/full` byte-equal. `old_agents_missing_agents_and_ended_sessions_get_notices`: no agent / old agent / ended notices, block from the stop. Nested blocks: unchanged path (Stop hook text), covered by the existing slots suite (green). QA: `qa_clear_after_find_...`, `qa_every_odd_answer_gives_a_notice` | PASS |
| AC2: no session file reads in `hub/` | `hub_reads_no_files` 3 passed; my own grep over `src/hub` non-test code: only `offset.rs`, `registry.rs` (`RegistryStore`), `config.rs` (`.env` `is_file`), and `spawn_blocking` for registry save, offset save, `client::own_build`. Mutation: `.exists()` is caught | PASS (guard gap B1, current code is clean) |
| AC3: agent serves only its own session's files, gate covered by tests | Code: all opens via `OwnProject::open`, ids validated, canonical compare; unit `tail::`/`reads::` (29), agent pipe-bait tests, `reads_e2e::a_path_the_hub_names_is_never_opened`; QA: foreign project, junction out of `subagents`, device names, junction config dir | PASS |
| AC4: old agent without capability degrades clearly, no panic; VERSION unchanged | `wire::tests::session_reads_stay_compatible_with_version_one_peers` (in the workspace run); `reads_e2e::old_agents_...` (old register never gets `session_read`, notice "старой версии"); `wire.rs:39 pub const VERSION: u32 = 1`, not in the diff; `transcript_read` still carries `path` for agents built from `main`; QA unknown answer kind gives the `Failed` notice | PASS |
| AC5: existing tests pass, fake soak green | workspace 738/0/3, soak `soak: ok`, slots x3 green | PASS |

## 4. Review findings re-checked (IMPL_REVIEW of `84b0ab8`)

| Finding | Check | Status |
|---|---|---|
| 3.1 own folder guessed once with a non-Claude rule | `OwnProject::folder` finds the folder by `<env id>.jsonl` (cwd names first, then `read_dir` of the root), keeps it once found, cwd names (resolved + given, 200-char cut + JS hash) only stand in; tests `a_new_session_under_a_junction_...`, `a_new_session_with_a_long_cwd_...`, `the_own_folder_is_found_...`; QA `qa_clear_after_find_...` | Fixed |
| 3.2 hub-named path opened before the check | No fs call on hub fields (grep); `SessionRead` has no `path`; agent pipe-bait tests green | Fixed |
| 3.3 refusing reader loses blocks | `read_failed(Calls)`: any reason except `LinkLost`/`NoAnswer` calls `open_from_stops` (`slots.rs:1911`); test `an_agent_that_cannot_give_the_calls_still_gets_a_block_on_the_stop` green | Fixed |
| 3.4 cut body read final with hook text | `bodies_parked` once per stop, `retry_bodies` on tick, 60 s; tests `a_block_read_cut_by_a_worker_swap_...`, `a_block_waiting_..._ended_session_...` green | Fixed |
| 3.5 guard bypasses | Brace import, alias, `"//"` strings, `registry.rs`, recursion now covered; **`is_dir`/`is_symlink` still pass** (B1) | Fixed as listed; one new gap |
| 3.6 misleading Refused notice | New text seen in the QA e2e run | Fixed |
| 3.7 title asked forever | `MAX_TITLE_FAILURES = 4` per (session, conn); test green | Fixed |

## 5. Bugs found

**B1 [minor, test strength] The AC2 guard does not forbid `is_dir` / `is_symlink`.**
- Where: `crates/cctg/tests/hub_reads_no_files.rs` `FORBIDDEN_NAMES` (lists `exists`, `try_exists`, `is_file`, `metadata`, ... but not `is_dir`, `is_symlink`).
- Repro: add `let _probe = std::path::Path::new(session).is_dir() || std::path::Path::new(session).is_symlink();` to `hub/slots.rs` `fn reader`; `cargo test -p cctg --test hub_reads_no_files` passes (3/3). The same line with `.exists()` fails the guard.
- Expected: a probe of a session-machine path in `hub/` fails the guard (the guard's own doc: "open, list or probe files"). Actual: passes.
- Impact: none today (no such call in `hub/`); a future regression could slip through. Fix: add `is_dir`, `is_symlink` to `FORBIDDEN_NAMES`.

No other defects were proven.

Notes (not defects, within accepted decisions):
- A session resumed inside a running claude (`/resume`) from another project's folder is served nothing (the own folder is the env session's). This follows decision 12 ("another project's transcripts never") and the fixer's residual note.
- A buggy agent answering `calls{more: true}` without moving `offset` makes the hub re-ask at once in a loop; the real agent always moves the offset (the line is consumed before the weight check), so this needs a broken or hostile agent.

## 6. Verdict

**PASS.** All five acceptance criteria hold with evidence; fmt, clippy `-D warnings`, the full workspace (738 passed), the named e2e tests and the fake soak are green; every review finding is fixed in code and covered by tests. The one new finding (B1) is a minor gap in the source guard, not in the product; it can be closed with a two-word change and does not block shipping.
