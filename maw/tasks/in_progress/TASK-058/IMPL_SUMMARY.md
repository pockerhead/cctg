# TASK-058 implementer summary

Commit `be4a18e` on `fix/statusline-via-agent` (worktree `C:/Users/user/dev/cctg-058`).

## 1. What was implemented

Flow: `cctg statusline` -> `<state>/status/<session>.json` -> the session's agent (link task, 1 s poll) -> `status_line` over the already open agent link -> hub `on_hook` as the session's `HookEvent::StatusLine`.

| File | +/- | What |
|---|---|---|
| `crates/cctg/src/statusfile.rs` (new) | +322 | `write` (temp + `rename` over the old file, `0600` file / `0700` dir on Unix, skips the write when the content is the same so the agent does not resend), `read`/`changed`, agent mark `<session>.agent` (`mark`, `agent_present`: fresh < 120 s, renewed every 30 s), `remove`, `prune` (> 24 h, stale `.tmp` > 60 s). Session ids limited to `[A-Za-z0-9_-]`, max 128. 3 unit tests. |
| `crates/cctg/src/statusline.rs` | +64/-10 | `hand_over()`: always writes the file; POSTs only when there is no fresh agent mark AND `CCTG_HUB_HOOK_ADDR` is loopback. A remote hub is never contacted. Module and `POST_TIMEOUT` docs updated. Unit test `numbers_go_to_the_agent_and_a_post_only_to_a_local_hub_without_one`. |
| `crates/cctg/src/wire.rs` | +47 | `Register.status_lines` (`serde(default)`), `HubMsg::Bound { session_id }`, `AgentMsg::StatusLine { session_id, model, effort, context, five_hour, seven_day }`; KINDS, header doc, samples. No `VERSION` bump. |
| `crates/cctg/src/agent.rs` | +173/-30 | `LinkConfig.status: Option<StatusWatch>`; `serve()` takes `bound` like `ping` (never reaches the channel), then polls the file once a second and writes `status_line` itself (only after this connection's `bound`; last-sent mtime resets per connection, so a reconnect resends once), renews the mark. `bind()` removes the files of the previous session after `/clear`. `run_stdio`: registers `status_lines` when a state dir exists, prunes old files at start, removes its session's files on exit (not on a handover to a newer worker). The select arms now return "what to write" and one write site follows (same behaviour for ack/ping/outbox). |
| `crates/cctg/src/hub/slots.rs` | +60 | `Conn.status_lines`; `tell_bound(conn)` (try_send `Bound`) after registration and at the end of `follow_pid` (/clear); `AgentMsg::StatusLine` accepted only when `session_id` equals the session the connection was bound to when the frame was read, turned into a `HookPost` (host of the connection) and passed to `on_hook`. |
| `crates/cctg/src/hub/ingress.rs` | +2 | `AgentMsg::StatusLine` added to the forwarded list. |
| `crates/cctg/src/channel.rs` | +2 | `HubMsg::Bound` in the "not for the channel" arm. |
| `crates/cctg/tests/statusline_agent_e2e.rs` (new) | +277 | Real `cctg agent` + real `cctg statusline` against a hand-written hub that answers the handshake after 300 ms and `bound` after another 300 ms. |
| `crates/cctg/tests/status_e2e.rs` | +90 | Hub side over a real TCP link to real `serve_agents` + `Slots`. |
| 10 test files | +1..2 each | `status_lines: false` in `Register` literals, `status: None` in `LinkConfig` literals. |
| `docs/poc.md`, `docs/remote-hub.md` | 1 line each | New behaviour described (Russian). |

"Agent present" (decision): a fresh `<state>/status/<session>.agent` mark, which the agent creates only after the hub said `bound` (so a hub older than TASK-058 never gets one) and renews every 30 s while the link is up. The status line checks it with one `metadata()` call; no network, no process-tree walk.

How the agent knows its session: the hub tells it (`bound`), because `CLAUDE_CODE_SESSION_ID` is stale after `/clear`. Alternatives rejected: keying files by claude pid (a ToolHelp snapshot on the status line path, and `CLAUDE_PID` in the status line env is not verified), and inferring the session from `transcript_read`/`session_read` (implicit coupling to the stream).

## 2. Deviations / not implemented

- Nothing from the task is left out.
- Criterion 3 vs criterion 2: a new `cctg statusline` with an OLD agent (running, not yet updated) and a REMOTE hub no longer posts at all (criterion 2 forbids network to a remote hub), whereas before a POST from a Windows machine (37-45 ms TLS) sometimes fit into 80 ms. With a local hub nothing changes (no mark -> POST as before). This stays until the agent is updated (hub "Обновить" or the next session). Old hub + new agent: the hub never sends `bound`, the agent never sends `status_line` or makes a mark, the status line posts to a local hub as before (tested). New hub + old agent: no `status_lines` in `Register`, so no `bound` is sent (tested).
- A session without a state dir (no HOME and no `CCTG_STATE_DIR`) keeps the old behaviour: POST to a local hub only.
- The mark is removed at exit after the channel loop ends; the link task could renew it in the same instant before the process exits. Then the stale mark stops the loopback fallback POST for at most 120 s for a session that has ended anyway, and the next agent start's prune does not touch it (< 24 h). Accepted as harmless.
- File reads in the link task are inline (stat once a second, a < 4 KiB read only on change), not `spawn_blocking`.

## 3. Test results

- `cargo fmt --all --check`: clean.
- `cargo clippy -j 1 --workspace --all-targets -- -D warnings`: clean.
- `cargo test -j 1 --workspace --no-fail-fast` (`scratch/implementer/test_workspace2.txt`): 39 of 43 targets ok; 4 failed because of the shared target dir: another tree rebuilt `target/debug/cctg.exe` during the run (failures show an older commit's build id `de8281d...` in `update_e2e`, the pre-TASK-056 line format in `statusline_cli`, `status_lines: false` in `statusline_agent_e2e`), plus two load-timing asserts in `hook_cli` (1.24 s vs 1.2 s; `hook.rs` is untouched).
- Rerun of those 4 targets after `touch lib.rs main.rs` (`scratch/implementer/test_rerun.txt`): `statusline_agent_e2e` 2/2, `statusline_cli` 6/6, `update_e2e` 3/3 ok; `hook_cli` failed once more on the same timing asserts under load and then passed 3 times in a row alone (9/9).
- The first full run without `--no-fail-fast` (`scratch/implementer/test_workspace.txt`) had 697 lib tests ok and stopped at the same contention failure in `statusline_cli`.
- New tests: `statusfile::tests::*` (3), `statusline::tests::numbers_go_to_the_agent_and_a_post_only_to_a_local_hub_without_one`, `status_e2e::numbers_over_the_agent_link_show_like_the_hooks_numbers` (bound on registration, numbers shown, another session's numbers dropped, `bound` again after `/clear`, an old agent gets no `bound`), `statusline_agent_e2e::a_slow_hub_gets_the_numbers_through_the_agent_and_no_post_is_made` (hub answers after 300 ms + 300 ms; numbers arrive, no POST, same numbers not resent, file has no secret or `@`, files gone after the agent exits), `statusline_agent_e2e::with_a_hub_before_task_058_the_status_line_posts_as_before`.

## 4. Manual verification

1. Build and install the branch binary on a client whose hub is remote (TLS pin set), restart a `claude-cctg` session so its agent is new, and update the hub to this commit.
2. In the session send any prompt. In `~/.cctg/status/` there are `<session_id>.json` (numbers only) and `<session_id>.agent`; on Unix `ls -l` shows `-rw-------`.
3. The pinned status in the topic shows model, effort, ctx, 5h and 7d within about a second of the terminal status line changing.
4. `/clear` in the session: after the next answer the status shows the new session's numbers; the old session's two files are gone.
5. Exit claude: both files of the session are gone. With a local hub and a session started without the channel flag, the numbers still arrive (direct POST fallback).
