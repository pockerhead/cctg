# TASK-058 code review (be4a18e)

## Verdict

**SHIP.** The flow statusline -> file -> agent -> `status_line` -> hub is correct, additive on the wire, and guarded on the hub by the bound session. I found no blocking defect. Four minor findings below can go to the fixer or be taken as known limits.

## Disconfirmation

Counter-example tested: after `/clear` the agent keeps its stale `CLAUDE_CODE_SESSION_ID`, so it could go on marking and sending the old session while the new session's numbers (remote hub) are lost. Checked in code: `follow_pid` (slots.rs:1557-1600) ends with `tell_bound(conn)`; the agent's `StatusWatch::bind` switches the session, removes the old session's files, and starts a fresh `StatusState` (`sent: None`), so the first tick sends the new session's file. `status_e2e::numbers_over_the_agent_link_show_like_the_hooks_numbers` shows `Bound(B)` after SessionEnd(clear)+SessionStart(clear). **The counter-example did not hold.** The one leftover hole is when `try_send` drops the `Bound` (finding 4).

## Confirmed correct

- Wire compatibility: `Register.status_lines` is `#[serde(default)]`, there is no `deny_unknown_fields` in wire.rs, and no `VERSION` bump. The hub sends `Bound` only to `status_lines` agents (`tell_bound` filter, slots.rs:1465-1478), and the agent sends `StatusLine` only after `Bound` (agent.rs `numbers: None` until the `Bound` arm). Both mixes are safe: an old hub with a new agent sees no new frame, and a new hub with an old agent gets no `bound`.
- `AgentMsg::StatusLine` is in the ingress forward list (ingress.rs:494), per the TASK-016 lesson, and it is covered over a real TCP link to `serve_agents` (status_e2e).
- Hub guard: the frame is taken only when `session_id == session_at(conn, received_at)`. Otherwise it falls to the `_ => debug!` arm. `apply_event` still requires live + TopLevel, and the empty `cwd`/`transcript_path` in the synthesized `HookPost` are ignored (registry.rs:871, 1074 only copy non-empty values).
- Handover: `run_stdio` skips `clean()` on `Ended::Handover`. The new worker registers, `agent_session` maps the pid to the current session, `tell_bound` fires, and the mark gets renewed. Prune only touches files older than 24 h, so the handover files survive.
- statusline: `hand_over` never contacts a non-loopback hub (`tls::is_loopback_addr`). Nested runs skip it entirely. Session ids are limited to `[A-Za-z0-9_-]{1,128}`, so there is no path traversal. The file holds `session_id` + numbers only, and the e2e test checks for no secret and no `@`.
- Windows rename over a file the agent is reading: `std::fs::File::open` shares DELETE, and rustc 1.95 `rename` falls back from `MoveFileExW` ACCESS_DENIED to `FileRenameInfoEx` with `POSIX_SEMANTICS` (std/src/sys/fs/windows.rs:1311-1346). The replace succeeds. A failed write/rename removes its temp file, and stray `.tmp` files older than 60 s are pruned.
- Unix: the file is created `0600` via `OpenOptionsExt::mode` and the dir is set to `0700` when created.
- Latency: measured in %TEMP% (20 runs each, remote hook addr, no hub): 131 ms with the status file vs 132 ms without a state dir. The difference is noise.
- Build/tests (shared target, -j 1, touched lib.rs/main.rs/tests): clippy `-D warnings` clean; `statusline_agent_e2e` 2/2, `status_e2e` 9/9, `statusline_cli` 6/6, lib `status` 31/31, `agent::` 26/26, `wire` 24/24.

## Issues

1. **minor — agent.rs `StatusState::due` (`if self.sent == Some(changed)`), statusfile.rs `changed`/`read`.** Change detection uses mtime only. On a file system with coarse timestamps (HFS+ 1 s, FAT/exFAT 2 s, some SMB/NFS mounts), two writes 300 ms apart can get the same mtime. Sequence: the agent reads the first write and records mtime M, then the second write lands with mtime M too. The agent never sends it, and the topic shows stale numbers (often the last update of a turn) until the next change. NTFS/ext4/APFS are fine. Fix: keep the last sent `HookEvent` (or the file bytes, < 4 KiB) and compare content. Keep mtime only as the "nothing to read" fast path, or read on every tick, since it is one small file per second.

2. **minor — tests/status_e2e.rs, new test, "Another session's numbers first: dropped".** B is not started at that point, so `numbers(B, 77)` would be dropped by `apply_event` (unknown session) even without the `session_id == session` guard. The test does not prove the anti-spoofing guard. Fix: `hub.start(B, 11)` first (a live top-level session with its own topic), then send `numbers(B, 77)` over A's link and assert B's status never shows `ctx 77%`.

3. **minor (rollout/doc) — statusline.rs `hand_over`.** The summary lists the regression for old agent + new statusline + remote hub. There is a second one it does not mention: **new agent + old (pre-058) remote hub.** No `bound` means no mark, and a remote addr means no POST, so every number is dropped. Before, a Windows client's POST (37-45 ms TLS) sometimes fit into 80 ms. This follows the task's "иначе не слать", but the update order matters: update the hub before the clients. Fix: one line in docs/remote-hub.md (or the release notes) that the hub must be updated first.

4. **minor — slots.rs `tell_bound` (try_send).** If the agent queue is full when `follow_pid` rebinds after `/clear`, the `Bound` is dropped and nothing re-sends it until a reconnect. Meanwhile the agent keeps marking and sending the old session (the hub drops those frames by the guard), and the new session has no mark. With a remote hub its numbers are lost for the rest of the connection. The probability is low (a 256-deep queue). Fix: when a `StatusLine` arrives for a session other than the conn's bound one, call `tell_bound(conn)` again. That is cheap and self-healing.

## Missing coverage

- Agent side of `/clear`: `bind()` removes the previous session's `.json`/`.agent`. This is only covered through the hub e2e with a fake agent, not by the real `cctg agent`.
- Handover (`Ended::Handover`) keeps the session files, and the new worker is marked after its `bound`.
- Reconnect resend: after a link drop the same numbers are sent once more (`sent: None` per connection).
- The spoofing guard with a known second session (finding 2).

## Nits

- `tell_bound` also fires for an agent whose session is still pending (unknown) or nested. The agent then marks and sends numbers the hub drops. This is harmless, but "sent" is recorded, so the first numbers after a late SessionStart wait for the next change.
- A crashed agent leaves a fresh mark that suppresses the loopback fallback for up to 120 s. The summary accepts this and I agree.
- `hand_over` does its file I/O serially before the user's command starts. Measured cost is within noise, so this is noted only.
