# FIX SUMMARY — TASK-011

Stage: fixer (claude/opus, effort=medium), continuation after the codex fixer died (host out of memory). Base: HEAD `8b96505` plus the codex partial diff in `registry.rs`. I checked that partial diff against the code, kept it, fixed it in a few places and finished the rest. Nothing committed.

## Preflight: the claim that would break correct code if applied as written

I1, the review's suggested fix: "prefer a free slot whose last session had the same `host/claude_pid`; pid reuse on Windows is harmless because the slot is free anyway". `SessionEntry.claude_pid` survives SessionEnd (`registry.rs`, `session_started` sets it and SessionEnd only clears `pids`). So a lookup by `entry.claude_pid` with no time bound and no `reason`/`source` check would match any old session whose pid Windows gave to a new, unrelated claude. A fresh `startup` session would then land in some older free slot instead of the first free ordinal, which breaks the "first free slot" rule. The same lookup would also match nested entries, whose `slot` is the parent's slot. So the claim is not safe as written. I used the orchestrator's variant instead: keyed by host+pid, a 60 s TTL, set only by `SessionEnd(reason=clear)` of a top-level session, used only by `SessionStart(source=clear)`.

## 1. Fixed

- **I1 (major, confirmed): `/clear` in the real hook order moved the conversation to another topic.** `crates/cctg/src/hub/registry.rs`:
  - The codex partial had a transient `recent_clears: host/pid -> (slot, expiry)` with `#[serde(skip)]` and `CLEAR_HANDOFF_TTL = 60 s`. I kept it.
  - SessionEnd with reason `clear` records the slot only if the pid is not already taken by the next session, which covers the reverse order. I added the missing check that only a `TopLevel` entry may hand its slot on, because a nested entry's `slot` is the parent's.
  - In `session_started`, `source=clear` takes the record through `take_clear_slot`, which drops expired records and checks host and folder. Any other start from that pid clears the record. `allocate` prefers that slot when it is free and in the same folder. The old `pids` rule for the reverse order is unchanged.
  - Tests:
    - `clear_end_before_start_stays_in_its_slot`: the reviewer's probe, ported with a realistic `SessionEnd{reason:"clear"}`.
    - `clear_in_one_process_stays_in_its_slot`: the original order. It now also applies B's late clear SessionEnd and checks that nothing gets recorded.
    - New `a_clear_hand_off_is_only_for_a_clear_start_in_time`: a plain SessionEnd, a non-clear start with a reused pid, and an expired record all go to the first free slot; only an in-time clear start keeps `#2`.
- **I2 (major, confirmed): a nested start rewrote a known top-level session's kind for good.** In `session_started`, if `parent_pid` is set and the session is known as `TopLevel`, the code returns `Parent(slot of the resolved parent)` without touching `kind`, `slot`, `pids` or `ended`. The codex version used `unreachable!` on an `else` branch. I replaced it with a plain lookup chain, so there is no panic path. Test `nested_start_of_a_known_top_level_does_not_take_its_slot` follows the probe step by step: nested resume from A's Bash, then the nested run's SessionEnd, then a plain resume. It also checks that the nested start creates no topic work. B stays `TopLevel` in its own slot.
- **I4 (minor, confirmed): hot loop when the scheduler is gone.** In `slots.rs::on_topic_done`, when `delivery == None` the code now logs one `warn` without private data and calls `topic_failed`, so the job waits for the retry tick. Pending separators stay pending. `Registry::release` had no other caller and is removed, along with the part of `a_separator_stays_pending_until_it_is_delivered` that used it. The failure path above it in the same test covers the same case. New actor-level test `a_stopped_scheduler_is_retried_on_the_tick_not_in_a_loop`: the scheduler is dropped, one `Done{delivery: None}` comes back, nothing is handed out again within 300 ms, and a retry tick hands it out once more.
- **I5 (minor, confirmed): full transcript rescan on every Stop/UserPromptSubmit.**
  - `read_title(path, from) -> (Option<title>, offset)` seeks to `from` and returns the offset reached through complete lines. A last line without its newline is scanned but not counted, so it is read again next time. The 256 MiB cap is still counted from the start of the file.
  - The actor keeps `scanned: session -> (path, offset)`. The offset resets if the path changes. The entry is dropped when a title is found or on SessionEnd, so the map stays bounded.
  - Tests:
    - `a_title_scan_goes_on_from_where_the_last_one_stopped` (unit): the head is skipped, a partial line is not counted, and the finished line is found.
    - `a_title_less_transcript_is_scanned_only_past_the_last_scan` (actor): a title written into the region already scanned is never read, and a title appended later is.
    - The existing `the_ai_title_is_found_past_the_head_of_a_long_transcript` is updated to the new signature.
- **Missing coverage (orchestrator decision 6):**
  - A->B->C while B's separator is still pending: `rapid_session_changes_keep_only_the_latest_separator` (from the codex partial, kept). Only C's separator is sent. I don't count that as a defect: B never reached the topic, and TASK-011 has no messages of its own to separate. The behaviour is now pinned by the test.
  - Duplicate `topic_id` or duplicate `(host, folder_key, ordinal)` in `registry.json`: `load` now returns `LoadError::Invalid` (codex partial, kept, checked). `allocate` never produces duplicates and Telegram does not reuse thread ids, so a valid file is never rejected. Test `duplicate_topic_ids_and_slot_identities_refuse_to_load`.

## 2. Skipped (no behaviour change, per orchestrator decision 5)

- **I3 (orphan topic when a `createForumTopic` answer is lost): real but not closable.** `createForumTopic` has no idempotency key. I added a known-limitation comment at the Create error branch in `slots.rs::on_topic_done`. The review's mitigation (don't retry non-400 errors when the name changes) is not done. It would only reduce orphans, not remove them, and TASK-017 can match `forum_topic_created` by name.
- **I6 (a session resumed from another folder keeps its old slot busy until SessionEnd): real, rare.** Documented as a known limitation in `allocate`, where the decision is made. The review's fix, clearing `current_session` of the old slot, is not applied. It would also drop that topic's title label and make the next session in that slot get no separator (`occupy` with `current_session = None`).
- **Nits** (trailing space in the title for an empty `cwd`, duplicated `short()`, `IconError::TooFew` count, the dead `creator` branch): not in scope of the decisions and they change no behaviour. Left as they are.

Known consequence of the I2 fix: the nested run's own claude pid is not put into `pids`. A run nested one level deeper under it is therefore NestedUnknownParent: no topic, same as today.

## 3. Test results

One cargo command at a time, default repo target dir, `--offline`.

```
cargo fmt --all -- --check                                   -> clean
cargo clippy --workspace --all-targets --offline -- -D warnings -> Finished, no warnings
cargo test --workspace --offline
  cctg lib        155 passed, 0 failed, 1 ignored
  cctg main         1 passed
  command_logs      1 passed
  ingress_logs      1 passed
  routing_logs      1 passed
  slots_logs        1 passed
  stdout            3 passed
  transcript: parse_fixtures 10, parse_tolerance 15, purity 3, render 19, split 14, subagent 14, doctests 1
  total 239 passed, 0 failed, 1 ignored
```

That is 231 + 8 new tests (registry +5, slots +3). The one ignored test is the old isolated config test.

---

# Round 2 (after QA NEEDS_FIXES: B1, B2)

Stage: fixer (claude/opus, effort=medium). Base: HEAD `e6a808a`. Nothing committed.

## Preflight: the claim that would break correct code if applied as written

QA B1 option (a): "hub ignores SessionEnd if the pid is present and not equal to `entry.claude_pid`". `SessionEntry.claude_pid` is `None` whenever the SessionStart carried no pid (`registry.rs`, `or_insert_with` sets `claude_pid: None`, and it is only overwritten when the start has one). Taken literally, `Some(x) != None` is a mismatch, so once TASK-012 hooks send the pid on SessionEnd, any session whose start came without a pid (older hook, missing env) would never end: its topic stays alive forever and its slot is never free. The fix therefore ignores a SessionEnd only when both pids are present and differ; test `a_session_end_ends_a_session_whose_start_had_no_pid`.

## 1. Fixed

- **B1 (medium, confirmed by s5b and s5c failing on the old code): the SessionEnd of a nested `claude -p --resume <id>` ended the live session `<id>` and gave its topic to the next session.**
  - `crates/cctg/src/wire.rs`: `HookEvent::SessionEnd` gains `claude_pid: Option<u32>` with `#[serde(default)]`. A body without it still decodes (`bad_hook_bodies_are_typed_errors` checks `{"type":"session_end"}`), the round-trip sample now carries `Some(10)`. `VERSION` unchanged (optional field, per the wire module rule).
  - `crates/cctg/src/hub/registry.rs` `apply_hook(SessionEnd)`: returns early, touching nothing, when the event pid and the recorded pid are both present and differ. The nested start paths (`Parent(None)` for self-resume and `Parent(slot)` for a known top-level) already leave `entry.claude_pid` at the live run's pid, so the comparison is against the right process. Without a pid, behaviour is as before.
  - **TASK-012 must make the hook send `claude_pid` (its `CLAUDE_PID`) on SessionEnd**; until then the check never fires and B1 stays latent, as it was (no hook sends `parent_claude_pid` yet either).
  - Regression tests in the crate: `the_end_of_a_nested_resume_of_a_live_session_does_not_end_it` (port of s5b, plus B's own end still ends it) and `the_end_of_a_nested_resume_of_the_parent_itself_does_not_end_it` (port of s5c: no icon change, next session does not take A's topic).
- **B2 (low, confirmed by s4b): an unrelated startup with a reused pid inherited the slot of a session whose SessionEnd was lost.**
  - `allocate` now takes `clear_pid` (the pid only when `source == "clear"`), so the pids hand-off works only for `/clear` (the reverse hook order, s3b, still passes).
  - In `session_started`, for any other source, a `pids` entry of the same host/pid pointing at another session marks that session `ended` (the pid reuse proves its process is gone); the new session then goes through the normal first-free rule.
  - Regression test `a_reused_pid_after_a_lost_end_takes_the_first_free_slot` (port of s4b): C gets `#1`, B is ended and its slot shows Dead.
- **O3 (fixed, a few lines): a title scan finishing after SessionEnd re-added the session to `scanned`.** `slots.rs` `on_done(Done::Title)` stores the offset only if the session is known and not ended. Test `a_title_scan_that_ends_after_session_end_keeps_nothing`.
- QA harness `scratch/qa/tests/{e2e,logs}.rs` updated for the new field; s5b/s5c now send the nested run's pid 30 on its SessionEnd. Result: e2e 18/18, unit 4/4, logs 1/1.

## 2. Skipped / documented

- **O1 (a separator that always fails blocks the slot's name and icon edits): documented, not fixed.** A bounded retry needs a new per-slot attempt counter and a policy for dropping an undelivered separator (the registry currently guarantees a separator is never lost). Not a few lines. Known-limitation comment added at the separator failure branch in `slots.rs::on_topic_done`.
- **O2 (agent registered with the pre-`/clear` session id): goes to TASK-013** per the orchestrator (already noted there by commit `e6a808a`). No code change.
- Note for whoever reads the timings: `slots::a_stalled_telegram_never_stalls_ingress` takes ~7.5 s of its 10 s budget on this host both before and after this round (measured with the round-2 changes stashed). It failed once in a parallel `cargo test --lib hub::` run and passed alone 3 of 3 and in the full suite. Pre-existing and not touched here, but it is close to flaky under host load.

## 3. Test results

One cargo command at a time, default repo target dir, `--offline`; the QA crate with `CARGO_TARGET_DIR=$TEMP/cctg-task011-qa-target`.

```
cargo fmt --all -- --check                                     -> clean
cargo clippy --workspace --all-targets --offline -- -D warnings -> Finished, no warnings
cargo test --workspace --offline
  cctg lib        160 passed, 0 failed, 1 ignored
  cctg main         1 passed
  command_logs      1, ingress_logs 1, routing_logs 1, slots_logs 1, stdout 3
  transcript: parse_fixtures 10, parse_tolerance 15, purity 3, render 19, split 14, subagent 14, doctests 1
  total 244 passed, 0 failed, 1 ignored
scratch/qa: cargo test --offline --test e2e   -> 18 passed (s4b, s5b, s5c now pass)
            cargo test --offline --test unit --test logs -> 4 + 1 passed
```

239 + 5 new (registry +4, slots +1).
