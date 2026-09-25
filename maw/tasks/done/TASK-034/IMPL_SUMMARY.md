# IMPL_SUMMARY — TASK-034

Verdict: implemented. Code commit `84b0ab8` on `feature/agent-serves-transcripts`.

## 1. What was implemented

### Step 0: the reference

`git apply --ignore-whitespace scratch/reviewer2/reference.patch` on HEAD `8252a80` (same code as `6024465`; patch sha256 `823565e0…` matched). `crates/cctg/src/hub/sessions.rs` was deleted by the patch itself. Then `verify_hashes.sh` printed `same` for all 32 files, and `sessions.rs` was absent. Pre-flight passed: every file, function and field the plan names exists with the shape it assumes. That is expected, since the plan is the patch.

### OPEN_DECISIONS 12 on top of the reference: the agent serves only its own project folder

What changed, measured over the reference (`scratch/implementer/decision12_over_reference.diff`, 446+/103-):

| File | +/- | What |
|---|---|---|
| `crates/cctg/src/tail.rs` | 192 (+/-) | `own_project(root, session_id, cwd)`: the folder that already holds `<env session id>.jsonl` (a resumed session), else `root/project_folder_name(cwd)`. `None` without a plain session id or without a cwd. `project_folder_name(cwd)`: every UTF-16 unit that is not an ASCII letter or digit becomes `-`. `open_transcript(project, id, path) -> Result<File, Closed>`: the canonical parent must equal the canonical own folder. `Closed::OtherProject` covers a file named `<id>.jsonl` whose folder sits beside the own folder in the same projects root, also when the own folder does not exist yet. Everything else is `Closed::Missing`. `read_chunk` takes the project folder. Module doc updated. 3 new tests, junction test fixed (it now creates the own folder first, so the refusal is not just "folder missing"). |
| `crates/cctg/src/reads.rs` | 144 | `answer(project, ..)`: with no own folder, every ask is `[Refused]`. A transcript of another folder gives `Refused` for render/title/calls. Subagent files open only from `<project>/<session_id>/subagents/`. Module doc updated. New test `another_project_folder_is_refused_and_any_session_of_the_own_one_is_served`. |
| `crates/cctg/src/wire.rs` | 12 | `SessionAnswer::Refused` (additive; a peer that does not know it decodes `Other`), plus a sample in `agent_samples`. VERSION stays 1. |
| `crates/cctg/src/agent.rs` | 62 | `Dirs.projects` renamed to `Dirs.project` (the own folder). `run_stdio` derives it once, on `spawn_blocking`, from `CLAUDE_CODE_SESSION_ID` + `current_dir` under `tail::projects_root()`. The stream reader and the session reader get the folder. Test call sites updated. |
| `crates/cctg/src/hub/commands.rs` | 7 | `Unavailable::Refused` with its notice: "Агент сессии {short} отдаёт только транскрипты папки проекта своей сессии, а этот лежит не там (или папку проекта агент определить не смог)." Added to `notices_name_the_short_id_only`. |
| `crates/cctg/src/hub/slots.rs` | 23 | `failure()` maps `Refused` to `Unavailable::Refused`. The test agents read from `projects/C--w` (the own folder). Flaky-test fix (see §2). |
| `crates/cctg/tests/reads_e2e.rs` | 99 | 2 new real-binary tests: `a_new_sessions_folder_comes_from_the_cwd_and_is_served` and `a_transcript_of_another_project_folder_is_refused`. |
| `crates/cctg/tests/{command_logs,stream_logs,files_e2e}.rs` | 2 each | Pass the own project folder / `project: None`. |
| `docs/poc.md` | 4 | `/brief` note: the agent serves only its own session's project folder. |

The whole task over `8252a80`: 32 files, 4109+/1553- (`git show --stat 84b0ab8`). Files that differ from the reference after this step, as `verify_hashes.sh` reports them: agent.rs, commands.rs, slots.rs, reads.rs, tail.rs, wire.rs, command_logs.rs, files_e2e.rs, reads_e2e.rs, stream_logs.rs, docs/poc.md. All other files are `same`.

### Security note (replaces PLAN_FINAL §5 "Decision: acceptable, kept")

The reference let any agent serve any session file under `<config>/projects` that the hub named. With decision 12, the agent serves only its own project folder `<CLAUDE_CONFIG_DIR|~/.claude>/projects/<p>`. It works that folder out once at start:

1. It takes the folder that already holds its env session's transcript. This covers `--resume`, including a session first stored under another folder.
2. Otherwise it takes Claude Code's name for its cwd, which covers a new session that has written nothing yet.

The rule is checked against this machine's folders. `scratch/implementer/check_project_encoding.py` looked at 13 projects: 12 matched exactly and 1 differed only in the case of the drive letter. On NTFS that is the same folder, and the gate compares canonical paths.

Any session id inside that folder is served. After `/clear` the id changes but the folder does not, and the hub rebinds the agent. A transcript of another project folder answers `refused` even when the hub names it for the agent's bound session. So a wrong or compromised hub (TASK-035 makes it remote) cannot read another project's transcripts or subagent files. When no folder can be derived (no session id, no cwd, no config root), every session read answers `refused` and every stream read is `missing`.

Residual risks:

- The hub can still read every session of the agent's own project folder, including older sessions there. That is accepted by decision 12 ("any session id inside it").
- Claude Code shortens very long folder names in its own way, and that is not modelled. A new session in a cwd whose name Claude Code shortens is served only once the agent restarts after the transcript exists. Until then the gate fails closed and answers "не найден". A resumed session is not affected.
- `Refused` for a foreign folder tells the hub that `<id>.jsonl` exists in a sibling project folder. That is an existence oracle limited to session ids, which the hub learns from hooks anyway.
- Hard links and TOCTOU swaps between canonicalize and open are still not detected (as in PLAN_FINAL §4).

## 2. Deviations and defects found

- **New wire variant `SessionAnswer::Refused`** (and `Unavailable::Refused`). The orchestrator asked for "refuse all reads with a clear answer". `Missing` would have told the user the file is not written yet. The variant is additive; an older peer decodes it as `Other`, and the hub maps that to `Failed`.
- **Defect in my first gate, found by a test:** it canonicalized the own folder first, so before a new session's first write a foreign transcript came back `Missing` instead of `Refused`. `reads_e2e::a_transcript_of_another_project_folder_is_refused` failed on it. Fixed: when the own folder does not exist, the check compares against the canonical projects root instead. The unit test `only_transcripts_of_the_agents_own_project_folder_are_served` now also covers an own folder that does not exist yet.
- **Defect in the reference (a flaky test), proven:** `hub::slots::tests::without_an_agent_that_reads_a_block_opens_on_its_stop_from_the_hooks` failed 1 in 3 runs of the parallel `hub::slots` suite. The captured ops were `[CreateTopic{icon 👀}, EditTopic{icon ⚡}]`: the agent's bind landed after the topic was created, so an icon edit followed and `ops().len() == 1` broke. The test now waits for `bound(&rig)` and compares with the op count after the bind. `hub::slots` then passed 5 runs in a row (157 each). No production code changed for this.
- **Not tested end to end:** a `/clear` rebind with the real `cctg agent` binary. That agent registers `claude_pid: None` in `reads_e2e`, because there is no claude ancestor, so the hub cannot move it to a new session. "A new session id in the same folder is served" is covered by the unit tests `tail::…only_transcripts_of_the_agents_own_project_folder_are_served` and `reads::…another_project_folder_is_refused_and_any_session_of_the_own_one_is_served`. The hub-side rebind by pid is covered by the existing slots tests.
- **Environment:** the first full run had one failure, `transcript` `purity::every_source_file_is_scanned` (NotFound on `read_dir`). That test binary in the shared target had been built from another checkout's manifest dir. Touching `crates/transcript/tests/purity.rs` rebuilt it; it passed, and the full rerun was green.

## 3. Test results

All commands ran from the repo root with `CARGO_TARGET_DIR=C:/Users/user/dev/cctg/target CARGO_PROFILE_DEV_DEBUG=0`, `-j 1`, one at a time.

| Command | Result |
|---|---|
| `bash …/scratch/reviewer2/verify_hashes.sh` right after the apply | all 32 `same`, `sessions.rs` absent |
| `cargo fmt --all -- --check` | clean |
| `cargo clippy -j 1 --workspace --all-targets -- -D warnings` | no warnings |
| `cargo test -j 1 --workspace` (log: `scratch/implementer/workspace_test.log`) | exit 0: 38 binaries, 727 passed, 0 failed, 3 ignored; cctg lib 557 passed / 1 ignored |
| `cargo test -j 1 -p cctg --lib hub::slots` ×5 | 157 passed each time |
| `cargo test -j 1 -p cctg --test reads_e2e` | 6 passed (4 reference + 2 new) |
| `cargo test -j 1 -p cctg --lib -- tail:: reads::` | 24 passed |
| `cargo test -j 1 -p cctg --test soak -- --ignored --nocapture` (log: `scratch/implementer/soak.log`) | `soak: ok`, exit 0 |
| `grep -n "pub const VERSION" crates/cctg/src/wire.rs` | `39: pub const VERSION: u32 = 1;` |

## 4. How to verify manually

- Gate for decision 12: `cargo test -p cctg --lib -- own_project own_one` runs the three new gate tests. `cargo test -p cctg --test reads_e2e` runs the real agent binary: a new session's folder is derived from its cwd and served, and another project's transcript gets the "отдаёт только транскрипты папки проекта" notice.
- Live, with a real session (hub updated first, then the session's agent via ⬆️): `/brief` in the session's topic returns the brief. After `/clear` in that session, `/brief` returns the new session's brief, because the folder is the same.
- Encoding rule on another machine: `python maw/tasks/in_progress/TASK-034/scratch/implementer/check_project_encoding.py`. It prints only counts and lengths.
- The rest of the task (hub reads no files, old agents, notices) is checked as in PLAN_FINAL §3.

Scratch files: `scratch/implementer/check_project_encoding.py`, `decision12_over_reference.diff`, `workspace_test.log`, `soak.log`. PCTX proposal added for the narrower channel-domain gate (`PCTX_PROPOSALS.md`, last entry).
