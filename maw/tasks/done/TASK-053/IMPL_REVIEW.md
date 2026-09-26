# TASK-053 — IMPL_REVIEW (code-reviewer)

Reviewed: `git diff 7748a43 54d25b7 -- crates docs install.sh` in the main tree (branch `feature/compact-status`), against `TASK_FINAL.md` (small-fix, no plan), `IMPL_SUMMARY.md` and `scratch/hooks_doc.md` (PreCompact/PostCompact/SessionStart docs).

## Disconfirmation

Counter-example tested: "a compaction that starts but never produces `SessionStart source=compact` while the session keeps working". Only three things end a running compaction record: `SessionStart(compact)` (`compact_ended`, slots.rs:1702), the session leaving `is_live_top_level` (retain, slots.rs:1639) and `COMPACT_MAX` = 15 min (`check_compactions`, slots.rs:1768). `status_view` puts `Phase::Compacting` above any activity (slots.rs:1850-1860). The installed Claude Code binary (`~/.local/bin/claude`, grep of its strings) has `Compaction canceled.`, `Compaction interrupted · This may be due to network issues — please try again.`, `Error during compaction: …`, `automatic compaction failed:`, `reactive compaction failed`. So compactions that start and never finish are a normal case, not a theoretical one. The counter-example **held**: see finding 1.

## 1. Verdict

**NEEDS_WORK**: the happy path, the hook side and the wire/install parts are correct and tested. But a compaction that is cancelled (Esc, or the status's own ⏹ button) or fails leaves the pinned status on "🗜 Сжимаю контекст…" for up to 15 minutes while the session works and waits. That hides the real TASK-029 status and the user can't tell that anything is wrong.

## 2. Confirmed correct

- Hook never blocks compaction: `build` for `PreCompact` (hook.rs:886-891) only reads `trigger`, whitelisted to `manual|auto`. The hook exits 0 with empty stdout (asserted in `status_e2e::pre_compact` and `hook_cli::a_hub_without_compactions_leaves_the_hook_quiet`). The POST uses the short budget (300 ms, TLS 600 ms, `post_timeout` hook.rs:651-665) and is not spooled (`spool::keeps` covers only start/end). The settings `timeout: 5` is well above that budget. Per the docs (hooks_doc.md:3072) only exit 2 or `decision: block` blocks, so a timeout is not a block.
- `custom_instructions` is not in `Input` and is never deserialized, so it can't reach the POST, hub or logs. The build test checks the POST body, the e2e runs the real binary with `RUST_LOG=trace` and checks stderr and every fake Bot API op, and hook_cli checks stderr on a 400.
- Old hub: `decode_hook` gives `UnknownKind` → 400 (ingress test moved to `post_compact`). The hook logs one fixed `HTTP 400` line and exits 0. The change is additive, `VERSION` is untouched, the `trigger` field is `#[serde(default)]` and a bare `pre_compact` decodes. Old agents never see hook events.
- Registry: `PreCompact` is a no-op (registry.rs:1094-1096). `SessionStart(compact)` of a known top-level session keeps its slot (`occupy` with the same session makes no separator, registry.rs:739) and its `metrics`.
- Status/flood: `pump_status` edits only when the rendered text changes and at most once per `status_every` (slots.rs:5923-5937). The minute wake-up (`compaction_deadlines`) causes one edit per whole minute, and wakes with no text change cost nothing. Each compaction costs 2 metered silent sends (`message_op`, `notify: false`).
- The state machine handles a PreCompact repeat while one is running (ignored), a nested run or a session that is not current (not shown, `current_slot`), no topic (not shown), SessionEnd/reap (dropped by the retain), the 15-min limit, a hub restart (in-memory only, and the status recomputes on the next pump), and SessionStart(compact) without a record (ignored). The 10 s wait for numbers has a deadline and falls back to a line without percentages.
- install.sh / docs/hook-settings.json / docs/poc.md groups match. install_e2e checks that the event sets are equal and that the PreCompact group is equal apart from the command path. The hook_cli snippet test checks timeout 5.
- Test run (reviewer, HEAD 60b7335, shared target, `-j 1`, after touching lib.rs/main.rs): `cargo clippy --workspace --all-targets -- -D warnings` is clean. `cargo test --workspace` gives 881 passed, 0 failed, 3 ignored, which matches IMPL_SUMMARY. No new crates were added.

## 3. Issues

### 1. major: a cancelled or failed compaction keeps "🗜 Сжимаю контекст…" as the status for up to 15 min
- Where: `crates/cctg/src/hub/slots.rs:1850-1860` (`status_view`: `Compacting` overrides `status::phase(activity, …)`), `slots.rs:1657-1697` / `1768-1788` (only `SessionStart(compact)`, the session's end or `COMPACT_MAX` end a running record).
- Proof: Claude Code has these paths (strings in the installed binary): `Compaction canceled.`, `Compaction interrupted …`, `Error during compaction: …`, `automatic compaction failed:`. Per the hook docs, `SessionStart source=compact` and `PostCompact` come only "after compaction". After a cancelled `/compact` the user types a new prompt and Claude works: `UserPromptSubmit`, `ToolStart`, `Stop` all update `activity`, but the status still says "🗜 Сжимаю контекст (вручную)… 3 мин". It stays until 15 min pass or the session ends. The implementer noted only the "blocked by another user hook" case (IMPL_SUMMARY §2), not Esc or errors, which are more common.
- Our own button makes it worse: `interrupt: keys && !waiting && self.busy(session)` (slots.rs:5876) stays on during an auto compaction mid-turn (activity is busy). The ⏹ press writes Esc, which cancels the compaction, and then the status gets stuck by exactly this path.
- Fix: treat a later turn event of the same session as proof that the compaction is over without success. The simplest safe variant: in `on_hook`, on `UserPromptSubmit` / `ToolStart` / `Stop` of a session with a running compaction (`done.is_none()`), drop the record with no line (log at info). If the order of `SessionStart(compact)` relative to the async `ToolStatus` POSTs of an auto compaction mid-turn is not guaranteed, keep the record but stop showing `Compacting` in `status_view` after such an event, and still accept a `SessionStart(compact)` for the done line for a short window. Add a unit test: PreCompact → UserPromptSubmit/ToolStart → the status head is no longer "🗜".

### 2. minor: a stale status-line value that arrives after the end can be reported as "after" and show growth
- Where: `slots.rs:1736-1745` (`compact_numbers` accepts any `context != before`).
- Case: a status line with 83% is sent while the compaction runs, but its POST reaches the hub after `SessionStart(compact)` (both are separate short POSTs from separate processes, so they can arrive in either order). `compact_ended` has set `before` = the last value the hub saw (80). The late 83 then gives "🗜 Контекст сжат за N с: 80% → 83%". This case is not covered by the test, which only covers the value repeating.
- Fix: accept only `context < before` when `before` is known (a compaction always shrinks the context), and keep waiting otherwise.

### 3. minor: a second PreCompact within the 10 s wait for numbers loses the first done line
- Where: `slots.rs:1658-1664`. The guard returns only for `done.is_none()`. For an ended record that is still waiting for numbers, `insert` overwrites it silently, so the "сжат за" line of the first compaction is never sent. This is rare (back-to-back compactions, e.g. an auto compaction right after a manual one), but it breaks the "start line + end line" pairing.
- Fix: call `compact_told(session, None)` before inserting the new record when the old one has `done.is_some()`.

### 4. minor: a session whose metrics have no `context` still waits 10 s for nothing
- Where: `slots.rs:1708` (`has_numbers = metrics.is_some()`). If `metrics` is `Some` but `context` is `None` (model/effort only), `before` is `None`, so the line can never show percentages, yet it still waits the full `COMPACT_NUMBERS_WAIT`.
- Fix: `has_numbers = before.is_some()` after `before = last.or(..)`; otherwise tell the line at once.

## 4. Missing coverage

- A cancelled or failed compaction: PreCompact followed by UserPromptSubmit/ToolStart/Stop with no SessionStart(compact), and the status must leave "🗜" (finding 1).
- The ⏹ button during an auto compaction mid-turn (is it offered, and what happens to the status after the press).
- A status line whose value is higher than `before` arriving after the end (finding 2).
- A PreCompact while the previous compaction waits for its numbers (finding 3).
- `status_e2e` feeds `SessionStart(compact)` straight into the hub, not through the real `cctg hook SessionStart` with `source: compact`. That hook path is not new, so this is acceptable but worth noting.

## 5. Nits

- `docs/poc.md` and install.sh add `timeout: 5` while other non-waiting hooks use the default. That is justified (a sync PreCompact delays the compaction), but `"async": true` would remove even the ~0.3-0.6 s + process start delay. Compaction start ordering does not need the synchronous path, since the hub timestamps on receipt. This is a design choice; worth a sentence in the module doc if it stays sync.
- The duration "сжат за N с" is measured between hub receipts of two separate POSTs, so hook process start and lineage walk on `SessionStart` skew it by a few hundred ms. That's fine at a seconds granularity.
