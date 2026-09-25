# IMPL_REVIEW — TASK-034 (code review of `84b0ab8`)

Reviewed: `git diff 8252a80 84b0ab8 -- crates docs` (32 files), against TASK_FINAL, PLAN_FINAL, OPEN_DECISIONS 12 and IMPL_SUMMARY. The summary was read as a claim and checked against the code.

## 1. Verdict

**NEEDS_WORK.** The split is sound, and clippy, tests and the fake soak are green. The decision-12 gate has two defects, both proven by repro tests:

- The agent picks its own project folder once, with a rule that differs from Claude Code's in two known cases. When it guesses wrong, its own session is refused for the whole life of the agent: no `/brief`, no title, no TASK-016 stream, and subagent blocks are lost completely.
- The gate opens whatever path the hub sends before it checks that path. That runs against decision 12's "compromised remote hub" threat model.

### Disconfirmation (done first)

Counter-example written down before evaluating anything: *a new session whose Claude Code project folder name is not `project_folder_name(raw cwd)`. The agent derives its folder once, before the transcript exists, so every read of the session's own transcript is refused.*

Where Claude Code's rule comes from: the installed Claude Code binary (`~/.local/bin/claude`, the 2.1.28x build).

- `import{realpathSync as r}from"fs"; ... function j1r(){... n=o(r(e)) ...}`: `originalCwd` is `realpathSync(process.cwd())`.
- The transcript lives at `yd() = join(kv() ?? kc(he()), `${id}.jsonl`)`, with `he()` = `originalCwd`.
- The folder name is `dx(e)`: `e.replace(/[^a-zA-Z0-9]/g,"-")`, cut to 200 characters plus `-<hash base36>` when longer.

The agent uses the raw `std::env::current_dir()` and never cuts the name (`agent.rs:504-512`, `tail.rs:71-86`).

**Result: the counter-example held.** Repro tests in a `%TEMP%` copy (source kept in `scratch/code-reviewer/review_repro.rs.txt`; the copy was deleted afterwards):

- `a_new_session_under_a_junction_is_refused_its_own_transcript` passes, so the defect is real. The cwd is reached through a junction, and `reads::answer` returns `[Refused]` for the session's own transcript.
- `a_new_session_with_a_long_cwd_is_refused_its_own_transcript` passes, so the defect is real. The encoded name is over 200 characters, and the answer is `[Refused]`.
- `own_folder_in_another_case_is_served` passes, so this case is fine. It is the `c--`/`C--` question: the agent's folder is spelled `C--Users-a-proj` and the disk has `c--users-a-proj`. The canonical compare folds case on NTFS. This machine really has a `c--...` folder whose records carry `cwd` `C:\...`, and it is served correctly.

## 2. Confirmed correct

- Wire compatibility, `wire.rs`:
  - `Register.session_reads` is `#[serde(default)]`.
  - `SessionAsk`/`SessionAnswer` have `#[serde(other)] Other`.
  - `Refused` is additive: an older peer decodes it as `Other`, and the hub maps that to `Failed`.
  - `VERSION` is still 1 (`wire.rs:39`).
  - `session_reads_stay_compatible_with_version_one_peers` covers an old Register, and an unknown ask or answer.
- Read ids are `wire::random_u64()` (`slots.rs` `ask_read`), so a stale answer the agent's outbox replays after a hub restart matches nothing. Only the conn that was asked may answer (`on_session_answer` filters on `p.conn == conn`).
- Reads fail at the right moments:
  - on disconnect: `fail_reads_of` runs first in `Disconnected`;
  - on the TASK-040 leave: it runs right after `leaving = true`;
  - on timeout, in `on_tick`. Each `more` piece restarts the wait.
  - The hub caps a read at 16 MiB, and an overflow ends as `TooLarge` with one `warn!` that carries no content.
- Pieces and `calls` batches fit `MAX_LINE` even fully escaped: `PIECE` is 128 KiB ×6 < 1 MiB, and the batch-weight bound is ~6× of 128 KiB. Tests `long_texts_come_in_pieces_that_each_fit_a_link_line` and `many_calls_come_in_batches_that_each_fit_a_link_line`.
- Ingress forwards `SessionAnswer` (`ingress.rs:229`, per the TASK-016 QA lesson), and `reads_e2e` covers it over real TCP. The `channel.rs` match stays exhaustive.
- The hub reads no file:
  - `sessions.rs`, `CCTG_PROJECTS_DIR` and `TopicView` are gone.
  - The guard passes.
  - The remaining `spawn_blocking` calls in `hub/` handle the hub's own state only.
- Canonical gate against `..`, junctions out of the session, an id cased unlike the file, and a directory named like a transcript: `reads::tests`, `tail::tests`.
- Logs carry short ids, `?why` and fixed text, and `reads.rs` logs nothing. `command_logs`/`slots_logs` assert this.
- The flaky reference test fix (`without_an_agent_that_reads_a_block_opens_on_its_stop_from_the_hooks` waiting for `bound`) is correct and changes no production code.
- My own runs (logs in `scratch/code-reviewer/`):
  - `cargo clippy -j 1 --workspace --all-targets -- -D warnings`: exit 0.
  - `cargo test -j 1 --workspace`: exit 0, 38 binaries, 728 passed, 0 failed, 3 ignored.
  - Fake soak: `soak: ok`.

## 3. Issues

### 3.1 [major] The own project folder is guessed once with a rule that is not Claude Code's; a wrong guess refuses the session's own files for the agent's whole life

- **Where:** `crates/cctg/src/agent.rs:504-512` (derived once at start from the raw `current_dir`); `crates/cctg/src/tail.rs:71-86` (`own_project`: cwd name, else the folder that already holds `<env id>.jsonl`, else the cwd name again); `tail.rs:228-243` (another folder = `OtherProject`).
- **Failure scenario:** a new session (no transcript yet when the agent starts) in either of two cases:
  - a cwd reached through a junction, symlink or `subst` drive. Claude Code uses `realpathSync(cwd)`; the agent uses the raw cwd.
  - a cwd whose encoded name is longer than 200 characters. Claude Code writes `<first 200>-<hash>`.

  The agent keeps `root/<raw name>` for its lifetime. Then:
  - every `session_read` of the session's own transcript answers `Refused`;
  - every TASK-016 `transcript_read` answers `missing`, which breaks a feature that worked before this commit (the old gate took any `<root>/<p>/<id>.jsonl`);
  - the ai-title is never found;
  - subagent blocks are lost completely (3.3).

  This lasts until the agent process restarts. A TASK-040 swap re-derives the folder and would then find the transcript.
- **Proof:** the two junction and long-cwd repro tests above; the real-binary pattern is the same as in `reads_e2e::a_transcript_of_another_project_folder_is_refused`.
- **The summary is wrong here:** IMPL_SUMMARY §1 "Residual risks" says that such a session "answers 'не найден'" until a restart. It actually answers `Refused`: the transcript's folder is a sibling in the projects root, so the gate returns `OtherProject`. The summary also does not mention the stream or the subagent blocks. The realpath case is not mentioned at all.
- **Suggested fix:** do not freeze a folder that was never confirmed. Keep the cwd-derived folder only as a provisional value. On each read while it is unconfirmed, re-run the transcript lookup (`<root>/*/<env session id>.jsonl`, the same `read_dir` as now, which is cheap). Once Claude Code writes its first record, the true folder is found through the transcript whatever naming rule it used. Optionally, also try `project_folder_name(canonical cwd without \\?\)` as a second candidate, and the 200-character cut plus hash (`wX` is a 32-bit `(h<<5)-h+c` over UTF-16 units, printed as `Math.abs(..).toString(36)`). Add the junction and long-cwd tests to `tail::tests`.

### 3.2 [major, security under decision 12] The gate opens the hub-named path before checking it

- **Where:** `crates/cctg/src/tail.rs:228` (`std::fs::canonicalize(path)` is the first filesystem call, on the path exactly as the hub sent it), `crates/cctg/src/reads.rs:331` (the same in `open_subagent_file`). The pattern came from TASK-016, but this task sends every `render`/`title`/`calls`/`subagent` ask through it. Decision 12 is about a wrong or compromised remote hub (TASK-035).
- **Failure scenario:** a hub sends `session_read{path: "\\\\host\\share\\<id>.jsonl"}`. `canonicalize` calls `CreateFileW` on the UNC path, so the agent opens an SMB connection to a host the hub chose. Windows tries NTLM authentication to it, which leaks the user's NTLMv2 hash. Named pipes and other device paths are opened the same way. The gate then answers `Missing`, but the damage is done.
- **Proof:** repro `the_gate_opens_a_hub_named_pipe_before_checking` (`%TEMP%` copy). A tokio named-pipe server on `\\.\pipe\cctg-cr034-<pid>` sees the agent connect (`connect Ok(Ok(()))`) while `reads::answer(.., Title)` returns `[Missing]`.
- **Suggested fix:** the agent builds the path itself and never opens the hub's path:
  - the transcript: `project.join(format!("{session_id}.jsonl"))`;
  - subagent files: `project.join(session_id).join("subagents").join(name)`.

  Canonicalize only that path, keeping the junction check against the canonical project. The hub's `path` can at most be compared lexically. This also removes the existence oracle behind `Closed::OtherProject`/`Refused` (IMPL_SUMMARY residual 3). Refused would then only mean "no own folder".

### 3.3 [major together with 3.1] An agent that answers `Refused` loses every subagent block; without an agent the block would open from the stop

- **Where:** `crates/cctg/src/hub/slots.rs:1427` (`check_candidates` goes to `open_from_stops` only when `reader()` is `None`), `slots.rs:1867` (`read_failed(Calls)` → `on_scan(Scan::nothing)` counts every failure as a miss).
- **Failure scenario:** the agent reads, but its answers are refused (3.1). A persistent `Unsupported`/`Unreadable`/`Failed` gives the same path. Each calls ask fails and counts as a miss. The candidate, stop included, is dropped when its window ends, and no block is ever shown. A session with no agent at all gets a block from its stop.
- **Proof:** a repro unit test in `hub::slots::tests` (`%TEMP%` copy; source in `scratch/code-reviewer/slots_refusing_reader_test.rs.txt`). The agent answers from a folder that is not the transcript's, with call and result lines present, and a start and a stop with files. The test fails with `no block at all: {}`. The control run, with the agent answering from `C--w`, passes.
- **Suggested fix:** in `read_failed(Purpose::Calls{..})`, when `why` is `Refused`, `Unreadable`, `Failed` or `Missing` with a stopped candidate, do what the no-reader path does: `open_from_stops(&session)` before `on_scan`. Only `NoAnswer`/`LinkLost` stay plain misses, since they are transient.

### 3.4 [minor] A failed body read makes a block final with the hooks-only text; no retry with the next worker

- **Where:** `slots.rs:1868-1876` (`read_failed(Purpose::Body)` → `body_text(input, None, None)` → `finish_block`).
- **Failure scenario:** a TASK-040 worker swap (`LinkLost`), or a body read that waits more than 20 s behind a `/full` of a large transcript (`NoAnswer`; the agent's session reader is serial, and the hub's timer starts at ask time). The block then shows only the stop's last message, with no tool lines and no description from `.meta.json`, and stays that way. Before this commit the hub read the files itself, so this could not happen.
- **Suggested fix:** for `LinkLost`/`NoAnswer`, put the input back into `bodies_waiting` once (a one-retry flag) instead of finishing. Or accept this explicitly in PLAN_FINAL §4 "Known limits".

### 3.5 [minor] The AC2 source guard can be bypassed with simple forms

- **Where:** `crates/cctg/tests/hub_reads_no_files.rs:35-60`.
- **Failure scenario** (traced against `offending()`), each passing the guard:
  - `use crate::reads::{answer};` followed by `answer(..)`, and likewise `use crate::files::{save as keep};`. The substring `reads::answer` never appears, and `reads`/`files`/`answer` are not forbidden words.
  - A line with a `"//"` string literal before the read, such as `let u = ("//", std::fs::read(p));`. Here `split("//")` drops the rest of the line.
  - Any read inside `registry.rs`: the whole file is excluded, not only its store.
  - Files in a future `src/hub/<dir>/`: `read_dir` does not recurse.
- **Suggested fix:**
  - forbid the word `reads` unless the next tokens are `::MAX_TEXT`, and the word `files` unless the next tokens are a known pure item (`chunks`, `clean_name`, ...);
  - strip string literals before cutting at `//`;
  - scan `registry.rs` except the `RegistryStore` impl;
  - walk the tree recursively.
- AC2 holds today; this issue is about the guard's strength, not current code.

### 3.6 [minor] The `Refused` notice misleads in its only realistic case

- **Where:** `crates/cctg/src/hub/commands.rs:329-331`.
- **Failure scenario:** the hub only ever asks the bound agent for its own session's transcript, so in practice `Refused` means 3.1 (the folder was guessed wrong). The notice tells the user that "этот [транскрипт] лежит не там". That reads as a wrong request, not an agent that could not find its own folder, and there is no hint what to do (restart the session, or update the agent to re-derive).
- **Suggested fix:** after 3.1 and 3.2, word it as "агент не нашёл папку проекта своей сессии; перезапустите сессию" and log the reason once on the agent side (`debug!`, no path).

### 3.7 [minor] The title is asked every turn forever when the read cannot succeed

- **Where:** `slots.rs:1860-1862` (`read_failed(Title)` forgets `reading` only) together with `read_title` at `slots.rs:1686`.
- **Failure scenario:** an agent that answers `Refused` (3.1) or `Missing` (no transcript at all with `CLAUDE_CODE_CHILD_SESSION=1`) gets a `title` ask on every `UserPromptSubmit` and every `Stop` for the life of the session. Each ask is cheap: an open plus a seek.
- **Suggested fix:** after N consecutive failures for a session, stop asking until its agent re-registers.

## 4. Missing coverage

- `tail::tests`: a new session under a junction cwd, and one with a cwd over 200 characters (3.1). Today both show the defect; after the fix they should be served once the transcript exists.
- `tail::tests`: the own folder spelled in another case than on disk (the `c--`/`C--` question). It works, but nothing guards it; the reference test only upper-cases the transcript path.
- A gate test that a UNC, pipe or device path from the hub is never opened (3.2). It needs a filesystem-free check, for example a named-pipe server that must see no client.
- `hub::slots::tests`: an agent that answers `Refused` or `Unsupported` still gets a block on the stop (3.3).
- `hub::slots::tests`: a body read that fails with `LinkLost` during a TASK-040 leave, and what the block then shows (3.4).
- `hub_reads_no_files::the_guard_sees_a_file_read`: the brace-import and `"//"` forms (3.5).
- No end-to-end `/clear` rebind with the real binary (the implementer notes this too). The unit tests cover the folder rule and the pid rebind separately.

## 5. Nits

- `crates/cctg/src/reads.rs:31-32`: agent-side code imports `crate::hub::registry::cut` and `crate::hub::subagents::{BodyInput, body_text, is_agent_id}`. The client/server split this task starts would be cleaner with these pure helpers in a shared module. It is one binary today, so there is no functional effect.
- `tail.rs:79-84`: the fallback `read_dir` takes the first folder that holds `<id>.jsonl`, in directory order. Claude Code's own lookup (`Mqn`) returns `null` when the id is in more than one folder. No duplicate ids exist on this machine (95 transcripts checked), so this is cosmetic.
- `agent.rs:513`: `debug!("own project folder unknown; transcript reads refused")` is the only sign of the whole-session refusal. An `info!` once would make 3.1 diagnosable from the agent log.

## Evidence

- `scratch/code-reviewer/review_repro.rs.txt`: 4 integration repros (case folding served; junction refused; long cwd refused; hub-named pipe opened).
- `scratch/code-reviewer/slots_refusing_reader_test.rs.txt`: the 3.3 repro, which fails on `84b0ab8`. The control variant passes.
- `scratch/code-reviewer/{clippy,test,soak}.log`: my runs, with the home path redacted.
- Every build used `CARGO_TARGET_DIR=C:/Users/user/dev/cctg/target CARGO_PROFILE_DEV_DEBUG=0 -j 1`, one cargo at a time. The `%TEMP%` copy has been deleted, and no project file was edited.
