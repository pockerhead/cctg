# TASK-026 code review

Reviewed: `git diff main...HEAD` (merge base 87fbaed; `git merge-tree` against current main is clean, main only touched docs/poc.md in an unrelated section) on `feature/hub-supervisor`, commit 26ebca4. Mode small-fix, checked against TASK_FINAL.md.

## 1. Verdict

**NEEDS_WORK**: the design is sound and the real-process test covers every acceptance criterion, but a candidate whose file swap fails is never withdrawn, so the supervisor restarts the hub about every 2 s forever and can later install a candidate that `deploy` already reported as failed (I1). The drain on stop can also lose a hook that already got a 204 (I2). Both fixes are small.

## Disconfirmation

The counter-example I set out to find first: "a deploy whose swap fails (a Windows rename refused) leaves the system stable, with the old hub running and the candidate gone". I traced `install` -> `swap_in` -> the `supervise` loop (supervise.rs:296-301, 205-213, 250-252). **It does not hold (bug confirmed, see I1).** `cctg.next.exe` stays after `Outcome::Failed`, and the loop picks it up again on every start and every 1 s tick.

## 2. Confirmed correct

- Graceful stop, offset vs batch: `updates::poll_until` (updates.rs:380-433) checks `stop` only before `get_updates` and during the back-off sleep, both `biased`. A batch whose offset was saved is always handed to `handle` whole. A cancelled `getUpdates` confirms nothing. `poll` delegates with `pending()`, so the old behaviour is unchanged.
- Registry saved before exit: `Control::Stop` is sent after the poll returns, so it sits behind every routed message. `Slots::run` (slots.rs:467-497) drains queued hooks and agent frames, calls `pump()` (which `send_replace`s the dirty snapshot), drops `self` (the watch sender), then awaits the save task. tokio `watch::changed` reports a pending version before it reports closed, so the last snapshot is written. The hub bounds the wait at 10 s (mod.rs:243-247). The supervisor bounds the hub at 30 s and then kills it (supervise.rs:476-487).
- The stdin EOF path: a plain thread copies stdin to a sink (mod.rs:182-204). The supervisor keeps `ChildStdin` out of `Child` because `Child::wait` closes stdin; that was a real trap and it is handled (log dead_end verified against the code). A hard-killed supervisor closes the pipe, so the hub stops gracefully.
- Process groups: the hub gets `CREATE_NEW_PROCESS_GROUP` / `process_group(0)` (supervise.rs:459-465), so a console Ctrl+C reaches only the supervisor. The supervisor handles Ctrl+C, Ctrl+Break and SIGTERM (supervise.rs:512-543).
- Backoff: 1, 2, 4 ... 60 s. It resets after 60 s of uptime (checked when the hub exits) and after a successful deploy. The e2e test measures the 1 s and 2 s gaps.
- Candidate checks: `--version` with a 10 s timeout and `kill_on_drop`, `cctg ` prefix, 80-char cap, byte comparison against `current` in `spawn_blocking` (supervise.rs:321-353). The `.part` -> `next` rename means the supervisor never sees half a file. A candidate is also checked before every start, so a crash-looping hub can receive its fix (supervise.rs:205-213).
- Rename dance and rollback: `current -> old`, `next -> current`, and `current` comes back if the second rename fails. Rollback is `current -> bad`, `old -> current`. A running old/bad image that cannot be deleted is renamed to `<name>.<nanos>` and swept later; the sweep matches digits only (unit tests `swap_and_roll_back_move_the_right_files`, `set_aside_files_are_swept_and_others_are_kept`). This holds when `cctg.old.exe` is the supervisor's own image after the first deploy.
- Result file: written as temp + rename, removed by `deploy` before it places the candidate, and removed again once read. On timeout, a candidate nobody took is withdrawn.
- Secrets: the supervisor never reads the env file. It logs file names, pids, exit codes and the version line. The e2e test asserts that neither the token nor the secret appears in the combined supervisor and hub stderr. `CCTG_BOT_API_URL` goes only into `BotApi.base`, which already leaves the API only through `without_url()`.
- Dependencies: no new direct crates. tokio gains `process` and `signal`. `signal-hook-registry` and `errno` are Unix-only transitive dependencies. The dev-dependency `windows-sys` gains `Win32_System_Console`. All within the agreed set.
- Tests: `supervise_e2e` (real processes, fake Bot API, temp HOME/state, no window) covers restart backoff, the registry surviving a deploy (no second `createForumTopic`), agent reconnect, rollback with exit 1, rejected, unchanged, and a graceful Ctrl+Break stop. docs/poc.md has the supervisor and deploy section. AC 1-5 are covered.
- My own run (separate `CARGO_TARGET_DIR`, `-j 1`, `DEBUG=0`): `cargo fmt --check` clean, `cargo clippy --workspace --all-targets -D warnings` clean, `cargo test --workspace` green (section 6).

## 3. Issues

### I1 (major): supervise.rs:296-301 (and 286-289): a failed swap keeps the candidate, restart storm, and a later silent install

`install` returns `Outcome::Failed` when `swap_in` fails, but it leaves `cctg.next.exe` in place. In `swap_in` all three failure points (`set_aside(old)`, `current -> old`, `next -> current`) keep `next`. What happens next:
1. `hub = None`. The loop sees `next.is_file()` and runs `take_candidate(None)`. The swap fails again (or succeeds now), then `start_hub`.
2. After 1 s, `Event::Tick` sees `next.is_file()` and runs `take_candidate(Some(hub))`. The hub is stopped gracefully, the swap fails, back to step 1.

The hub restarts every ~2-3 s without backoff: each start costs `getMe`, `getChatMember` and the icon lookup against Telegram, and the hub is effectively down. Each round also rewrites `cctg.deploy-result`, which nobody reads. The realistic trigger on Windows is Defender scanning the fresh exe (it was just written and just executed by `--version`), which can hold it longer than the 1.4 s of rename retries. When the lock clears, the next round installs the candidate after `deploy` already printed `failed` and exited 1. The binary changes behind the orchestrator's back, and that later result is never reported. The `Rejected` path has the same pattern in a milder form when `remove_file(next)` fails: `--version` runs every second.

Fix: after any non-`Deployed` outcome that did not consume `next`, move the candidate out of the `next` name. Rename it to `bad` (or `cctg.next.exe.<nanos>`, which the sweep then deletes), and fall back to `remove_file`. If `next` still exists afterwards, back off (for example, do not re-check a `next` whose mtime and size have not changed since the last refusal). Add a unit test for `install` when `set_aside(old)` or `next -> current` fails. On Windows, a `next` held open with a share mode that forbids delete reproduces it.

### I2 (minor, arguably major): slots.rs:327-339: a hook acknowledged 204 during the final save is lost

The drain `try_recv`s the hook and agent channels, then awaits the save task while `hooks: mpsc::Receiver` is still alive, until `run` returns. `serve_hooks` keeps running. A hook whose `try_send` lands in that window (the save is fsync + rename) gets `204`, and its `dedup` entry is recorded, but the event is dropped with the receiver. A lost `SessionStart` means the session is never adopted (TASK-011: no adoption without SessionStart), and the spool will not resend it because the hub said 204. The window is milliseconds per deploy, but it is exactly the contract that the doc comment claims ("hook posts ... already queued are handled").

Fix: call `hooks.close()` and `agents.close()` before the `try_recv` loops. `close()` makes later `try_send` fail, so ingress answers 503 and the hook spools, while buffered items still drain. One line each.

### I3 (minor): slots.rs:469-497 + mod.rs:243: telegram jobs in flight are cut at exit

`pump()` in the drain can hand new jobs to the dispatch task, and jobs already in flight never report back through `done`, because the actor is gone and the runtime drops when `main` returns. For first sends of subagent blocks this becomes the TASK-015 tombstone (the block is never re-sent). A `createForumTopic` in flight can leave an orphan topic (known TASK-011 limitation). Stream lines are resent (at-least-once). This is no worse than the old hard kill, but a "graceful" stop could avoid most of it: stop handing new work after `Stop` and give in-flight `done` results a bounded wait (for example 3 s within the 10 s budget). At minimum, list it in the known limits of the deploy docs.

### I4 (minor, unverified by a live test): mod.rs:195-203: keyboard Ctrl+Break in the supervisor's console kills the hub hard

`CREATE_NEW_PROCESS_GROUP` disables Ctrl+C for the hub, but a Ctrl+Break typed on the keyboard is delivered to every process attached to the console, whatever its group (process groups only scope `GenerateConsoleCtrlEvent`). The hub registers only `ctrl_c`. The tokio Windows handler returns FALSE for an event with no listener, so the default handler runs `ExitProcess`, and the hub dies without the final save while docs/poc.md promises that Ctrl+Break stops it gracefully. The e2e test sends Ctrl+Break with `GenerateConsoleCtrlEvent` to the supervisor's group only, so it cannot see this. Fix: in `stop_requested`, also listen to `tokio::signal::windows::ctrl_break()` (and SIGTERM on Unix, for symmetry with the supervisor).

### I5 (minor): supervise.rs:547-585: stale or foreign result after a deploy timeout; racing deploys

`deploy` is guarded only by `next.exists()`, with no lock or nonce. Case A: deploy #1 hits its timeout after the supervisor took the candidate (`--timeout-secs` below the ~52 s worst case of 10 s check + 30 s stop + trial + renames). Deploy #2 then removes no result yet, places its candidate, and reads #1's result as its own. Case B: two deploys that pass the `exists` check together share the `.part`, and the first to time out deletes the other's `next` ("no supervisor took the candidate"). The orchestrator is single, so the risk is low. Fix: create `next` with `create_new` semantics (copy to a unique `.part.<pid>`, then use a rename that fails if `next` exists, or take a `cctg.deploy.lock` created with `create_new`), and put a nonce in the result (for example the candidate's length plus mtime, or a random id written next to the candidate) that `deploy` checks.

### I6 (minor): supervise.rs:345-352: a missing `cctg.exe` makes every fix a rejection

If a rollback fails halfway (`current -> bad` done, `old -> current` refused), `cctg.exe` is missing. `start_hub` then fails forever with backoff, and `check_candidate` fails on `fs::read(current)` ("cannot compare"), so every fixing candidate is `Rejected` and deleted. Only a manual copy recovers. Fix: treat a missing `current` as "different" (`NotFound` -> `Some(version)`); `swap_in` then just moves `next` in (`rename(current, old)` would fail on NotFound, so skip it when `current` is absent).

### I7 (minor): config.rs:88-90, 193-199: `CCTG_BOT_API_URL` is undocumented and allows plain http to any host

Any `http://` host is accepted, and the token then travels in cleartext in the URL path. Nothing is logged when the default is overridden, and neither docs/poc.md nor CLAUDE.md mentions the variable, although the implementer summary treats it as test-only. Fix: accept `http://` only for a loopback host (`https://` anywhere), log one `warn!` with the base URL when it is not the default (the URL itself holds no secret), and state in the doc comment and docs that it is test-only.

## 4. Missing coverage

- `install` when the swap fails (`set_aside(old)` or `next -> current` refused): the candidate must be withdrawn and the hub must not be restarted in a loop (I1).
- The `Slots::run` stop drain: a hook posted during the final save must get 503, not 204 (I2). No unit test drives `Control::Stop` at all; the path is covered only by the log line in the e2e test.
- `check_candidate` with a `--version` that hangs (the 10 s timeout and kill) and one that prints something other than `cctg ...` while exiting 0. The e2e test covers only "not a program".
- A deploy while the hub crash-loops (hub `None` when the candidate arrives). The code handles it (supervise.rs:208) but no test exercises it.
- `deploy` reading a stale or foreign result (I5).
- The Unix paths (`process_group(0)`, SIGTERM, `kill -TERM` in the test) were neither compiled nor run: no Linux target is installed on this host (`rustup target list --installed`: msvc, wasm32 only). Say so in the handoff; a `cargo check --target x86_64-unknown-linux-gnu` in CI or on a Linux box is needed before step 5 (second device).
- The e2e test needs a console for `GenerateConsoleCtrlEvent` (it asserts `sent != 0`). Run without a console (a detached service or runner), it fails instead of skipping.

## 5. Nits

- supervise.rs:143 / 147: `encode` flattens `\n` in the detail, but `decode` trims only the detail; fine, just note that a `\r` survives.
- supervise.rs:265-267: `Unchanged` also resets the backoff; harmless, but "a successful deploy resets the backoff" is broader than it reads.
- mod.rs:182: the stdin reader thread is spawned lazily, on the first poll of `stop_requested`, which is after startup (`getMe`, icon checks). A stdin EOF during startup is noticed only then. This is fine, because the supervisor kills after 30 s anyway, but a hub stuck in startup always costs the full 30 s on deploy.
- docs/poc.md: "Ctrl+C (или Ctrl+Break)" should mention that Ctrl+C during a deploy trial is honoured only after the trial (it is in IMPL_SUMMARY but not in the docs).

## 6. Test run (reviewer)

Separate `CARGO_TARGET_DIR` under %TEMP%, `CARGO_PROFILE_DEV_DEBUG=0`, `-j 1` (deleted afterwards):

- `cargo fmt --all --check`: clean.
- `cargo clippy -j 1 --workspace --all-targets -- -D warnings`: clean.
- `cargo test -j 1 --workspace`: all green. cctg lib 414 passed / 1 ignored, main 1, every integration binary ok, `supervise_e2e: ok` (the garbage candidate was rejected with os error 216), transcript suites ok, soak skipped as before.

The implementer's test claims reproduce. The issues above are in paths the tests do not reach.
