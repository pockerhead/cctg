# FIX_SUMMARY — TASK-034 (fixer, after IMPL_REVIEW of `84b0ab8`)

Code commit: `5cd323e` on `feature/agent-serves-transcripts`.

## 0. Preflight

- I read `scratch/` (planner, reviewer2 and implementer patches and logs, code-reviewer repros) as a coverage map only. I did not rerun any author script as proof.
- The review claim that would break correct code if applied verbatim is 3.5's allowlist: "forbid the word `files` unless the next tokens are a known pure item (`chunks`, `clean_name`, ...)", plus `reads` only before `::MAX_TEXT`. I checked it against the code. The hub's production code also uses `use crate::files;`, `files::room`, `files::Assembly`, `files::is_photo`, `files::default_name` and `files::MAX_*`/`CHUNK`/`AHEAD` in `hub/fetch.rs` and `hub/slots.rs`. Taken literally, the guard would fail on correct code. The new guard allows the pure `files` items and any `UPPER_CASE` constant. A plain `use crate::files;` is allowed because every later `files::X` is still checked. Logged as `dead_end`.
- The other claims I verified before acting:
  - I read Claude Code's `dx` in the installed 2.1.28x binary: `k(e)=e.replace(/[^a-zA-Z0-9]/g,"-")`, cut to 200, then `-${Math.abs(wX(e)).toString(36)}` where `wX` is `(e<<5)-e+c|0`. I ran it under node and saved the result in `scratch/fixer/claude_folder_name.js`.
  - The pipe repro (3.2) is real. My agent-level mutation, which adds `canonicalize` on the hub path, makes the named-pipe server see a client.

## 1. Fixed

**3.1 + 3.2 (major): the agent guessed its folder, and opened the hub's path.**
- The agent now builds every path itself (`tail.rs`):
  - `OwnProject::open` joins plain names (a session id checked by `is_plain_session_id`, an agent id checked by `is_agent_id`; neither allows dots, separators or more than 64 bytes) under the own folder.
  - The folder must sit directly in the canonical projects root.
  - The canonical path must equal `<canonical folder>/<parts>` exactly, so no link or other spelling gets through.
- `reads::answer(project, session_id, ask)` and `tail::read_chunk(project, session_id, from)` take no path.
- Wire (`wire.rs`):
  - `HubMsg::SessionRead` has no `path` any more. It is not on `main` (unreleased), and serde drops the `path` an earlier hub sends. A test decodes such a line.
  - `TranscriptRead` still carries the hook path, because agents built from `main` require it. The new agent binds it to `..` and never opens it.
  - The hub no longer passes a path to `ask_read`.
- The own folder is found, not computed. `OwnProject::folder`:
  - It takes the folder that holds `<env session id>.jsonl` (the cwd-named folders first, then a `read_dir` of the root). It looks again on each read until it finds one, then keeps it.
  - An id found in two folders gives no answer, as in Claude Code's own lookup (nit 5.2).
  - Until the folder is found, reads answer **Missing**.
- The cwd fallback is kept, because one case needs it: a session that runs `/clear` before its first record. Its env id never gets a transcript, so only the cwd name can find the new session's file.
  - It matches Claude Code: the resolved cwd first, then the cwd as given, both through `project_folder_name` = `dx` with the 200-character cut and hash.
  - Test vectors come from node. They include a negative hash and a name made of non-ASCII and surrogate-pair characters.
  - A cwd-named folder is only a stand-in and is never kept.
- `Refused` now means only "no session id or config folder". Another project's session answers `Missing`: no path leads there, and there is no existence oracle.
- `agent.rs`: `Dirs.project: Option<Arc<OwnProject>>`. The `info!` line fires only when no folder can ever be found (nit 5.3).
- Tests:
  - `agent::tests::a_transcript_read_is_answered…` and `…session_reads_are_answered…`: the hub's path, or an earlier hub's `path` field, names a `\\.\pipe\…` server. The own file is served, and the pipe sees no client (on non-Windows the test uses a missing file).
  - `tail::tests`:
    - `a_new_session_under_a_junction_is_served_from_its_resolved_folder`
    - `a_new_session_with_a_long_cwd_is_served_from_claude_codes_folder`, which also covers a session that ran `/clear` before its first record
    - `the_own_folder_is_found_by_the_sessions_transcript_and_then_kept`
    - `claude_codes_folder_names_are_matched_long_ones_cut_with_its_hash`
    - `the_own_folder_named_in_another_case_than_on_disk_is_served`
    - `only_transcripts_of_the_agents_own_project_folder_are_served` (a new id in the own folder is served)
    - `a_junction_out_of_the_projects_root_is_not_followed`
  - `reads::tests`: `another_projects_session_is_never_served_and_any_session_of_the_own_one_is`, `before_the_own_folder_is_found_reads_are_missing_and_then_served`, plus the junction test and an id cased unlike the file.
  - `reads_e2e::a_path_the_hub_names_is_never_opened` (real binary). The hook names another project's file with private content. The agent serves its own transcript, and the private text never appears.
  - `a_new_sessions_folder_comes_from_the_cwd_and_is_served` is kept. The first e2e test now finds its folder (`C--qa-w`) only through the env-id transcript.

**3.3 (major): a Refused or Missing calls read lost the blocks.** In `slots.rs` `read_failed(Calls)`, any reason except `LinkLost` and `NoAnswer` now calls `open_from_stops(&session)` before `on_scan`, exactly like the path with no reading agent. Test: `an_agent_that_cannot_give_the_calls_still_gets_a_block_on_the_stop`, with a `missing` agent (folder `C--not-mine`) and a `refused` agent (no folder). It is the reviewer's repro, adapted.

**3.4 (minor): a body read cut by a swap.**
- A body read that fails with `LinkLost`/`NoAnswer` is parked in `bodies_parked`, at most once per stop (`bodies_retried`), for up to `BODY_RETRY_WAIT` = 60 s.
- On each tick, `retry_bodies` hands it to `reader(parent)` once there is one (the next worker). While something is parked, the actor wakes every second.
- It falls back to the hook text when the parent ended or the wait ran out. A newer stop replaces a parked read.
- Tests:
  - `a_block_read_cut_by_a_worker_swap_is_read_by_the_next_agent`: agent 1 leaves with `Reloading`, agent 2 renders the files, and a second cut gives the stop text.
  - `a_block_waiting_for_the_next_agent_of_an_ended_session_gets_the_stop_text`.
- The existing `a_session_read_fails_on_timeout_and_on_a_lost_or_leaving_link` now expects the one retry.

**3.5 (minor): guard bypasses** (`tests/hub_reads_no_files.rs`):
- It scans `src/hub` recursively.
- It strips comments, strings (including raw strings and strings that span lines) and char literals before matching.
- `reads`/`files` followed by `::` must name an allowed pure item or a constant. An `as` alias or a brace import is flagged.
- `registry.rs` is scanned, except for the `impl RegistryStore { … }` block.
- New cases in `the_guard_sees_a_file_read` cover every bypass the review lists, plus a new test `every_hub_source_is_found_in_subfolders_too`.
- Mutation: inserting `use crate::reads::{answer as a};` into `registry.rs` fails the guard.

**3.6 (minor): the Refused notice.** The new text says the agent does not know where Claude Code keeps the session's transcripts (no session id, or no `CLAUDE_CONFIG_DIR` / `~/.claude` on the session's machine). Test: `the_refused_notice_names_the_missing_config_folder`.

**3.7 (minor): the title was asked on every turn forever.** `title_failures` keeps (session → agent conn, count). After `MAX_TITLE_FAILURES` = 4 failures in a row on one agent, that agent is not asked again. A new agent is asked again. A found title or the session's end clears the entry. Test: `a_title_that_cannot_be_read_is_asked_of_one_agent_only_a_few_times`.

**Docs:** `docs/poc.md` now says the agent finds the transcript by session id in its own folder and does not open the hook path. `PCTX_PROPOSALS.md` has two new entries: the corrected gate, and a lesson that `canonicalize`/`metadata` on a path from a peer is already an open.

## 2. Skipped

- Nit 5.1 (the agent imports `hub::registry::cut` and the `hub::subagents` helpers): it has no functional effect, it is outside this task's scope, and the surgical-change rule applies.
- 4 "no end-to-end `/clear` rebind with the real binary": the real agent in `reads_e2e` has no claude ancestor (`claude_pid: None`), so the hub cannot rebind it. The unit tests cover a new id in the own folder, and the slots tests cover the pid rebind.
- UNC-path test: there is no SMB server to watch in a test. The named pipe stands in for it. Neither wire field is ever passed to a filesystem call any more: `SessionRead` has no path, and `TranscriptRead`'s path is bound to `..`.
- Residual risk, noted and not fixed: a session resumed from another folder (env-id transcript in folder A) that then runs `/clear` writes the new session into the cwd's folder B. The kept folder is A, so reads of the new id are `Missing`. The implementation before the fix had the same limit.

## 3. Test results

Every command ran from the repo root with `CARGO_TARGET_DIR=C:/Users/user/dev/cctg/target CARGO_PROFILE_DEV_DEBUG=0 -j 1`, one cargo at a time.

| Command | Result |
|---|---|
| `cargo fmt --all -- --check` | exit 0 |
| `cargo clippy -j 1 --workspace --all-targets -- -D warnings` (`scratch/fixer/clippy.log`) | exit 0 |
| `cargo test -j 1 --workspace` (`scratch/fixer/workspace_test.log`) | exit 0: 38 binaries, 737 passed, 0 failed, 3 ignored |
| `cargo test -j 1 -p cctg --lib -- tail::` (after I added the case test and ran rustfmt on it) | 16 passed |
| `cargo test -j 1 -p cctg --test soak -- --ignored --nocapture` (`scratch/fixer/soak.log`) | `soak: ok`, exit 0 |
| `python scratch/fixer/mutations.py` (`scratch/fixer/mutations.log`) | 5 of 5 mutations killed |

The 5 mutations:
- the agent probes the hub's transcript path;
- the cwd guess is frozen;
- there is no 200-character cut;
- a link inside the own folder is followed;
- a failed calls read is only a miss.

The guard mutation, done by hand, was also killed.

The only change after the full workspace run is one Windows test in `tail::tests` and its rustfmt reflow. I ran that module again, and the soak ran after it.
