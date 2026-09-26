# TASK-050 fix summary

Code commit: `3daf97f fix: a failed release download still takes a newer file on disk (TASK-050)` on `feature/update-download`.

Preflight: the review claim most likely to break correct code if applied verbatim was finding 4, "enable reqwest `system-proxy`". I checked it. The feature exists in reqwest 0.13.5 (`system-proxy = ["hyper-util/client-proxy-system"]`), but it pulls `windows-registry` (Windows) and `system-configuration` (macOS). Neither is in `Cargo.lock`, and `system-configuration` is not in the local cargo cache, so this is new downloads and new lock entries. It would also change the proxy source of the hub's Bot API client, not only the download. Not applied; documented instead (see Skipped).

## Fixed

1. **major, a tagged hub never falls back to the disk** (agent.rs). Checked: real. The `(Ok(Err(failure)), _)` arm answered every failure and never called `Worker::plan`. Now `Failure::Missing | Failure::Download` go to `follow_plan(.., failed: Some(outcome))`. If `plan()` is `Reload` (the file on disk differs from the running build: install.sh, by hand, `cctg deploy`), the agent hands over to it. For any other plan it answers the failure as before. `Checksum` stays final, per the scope. e2e: `a_failed_download_leaves_the_old_binary_and_the_agent` now ends with the running file renamed away, a newer one written in its place and a `cut` release. The answer is `reloading`, and the new worker registers with the newer build and answers a ping. Before the fix that step answered `download_failed`.
2. **late install after "old build stays"** (download.rs). Checked: real. `timeout` dropped the `spawn_blocking(install)` handle, and the install could still swap afterwards. Now the network part runs under `timeout_at(deadline)`. `install` gets the same `deadline`: the lock wait is `min(deadline, now + 30 s)`, and the deadline is checked again under the lock before the swap starts. So `Installed` is answered only for a swap that really happened, and a timeout never leaves a swap behind. Unit test `no_swap_begins_after_the_deadline_or_the_exit` covers a past deadline: `Download`, file untouched, no `.part`/`.old`.
3. **Windows swap window at exit** (download.rs, agent.rs). There is a new `download::Gate` (open / closed / swapping). At stdin EOF the agent closes it. If no swap has started, nothing is written: the lock wait stops too, and the process exits without waiting. If a swap has started, `close()` returns false and the agent waits for it (a write and two renames, capped at 30 s). The old fixed 5 s wait is gone. For a process killed outright between the two renames, `download::recover_in(exe)` puts `cctg.old` back as `cctg` when `cctg` is missing. It runs only under `try_lock` of the deploy lock, so it never races a running `cctg deploy` or download. It runs at every ⬆️ Обновить press (start of `fetch`, and before `plan()` in `follow_plan`) and inside `install` under the lock. Tests: `a_cut_swap_gets_its_previous_binary_back`, plus the gate cases in `no_swap_begins_after_the_deadline_or_the_exit` (a closed gate, and a lock held elsewhere where the wait ends with the gate) and in `the_swap_replaces_the_file_and_leaves_nothing_behind` (after a swap, `close()` is false).
5. **loopback check with user info** (download.rs). Checked: real (`http://localhost:80@evil.example/` passed). `local_http` now parses with `reqwest::Url`: scheme `http`, empty username, no password, `host_str` is `127.0.0.1` or `localhost` (the same set as `install.sh`). The test covers `localhost:80@evil.example`, `127.0.0.1@evil.example`, `user@127.0.0.1` and a non-URL.
6. **404 may be transient** (hub/status.rs). `NO_RELEASE_BUILD_NOTICE` now says the build may not be uploaded yet and to press again in a few minutes. It suggests `install.sh --from-source` only if that does not help. Thanks to fix 1, a manual install is now picked up.
7. **docs** (docs/remote-hub.md, the «Обновить» paragraph). Added three things: the fallback to a file already on disk, that the proxy comes only from claude's environment (`HTTPS_PROXY`/`ALL_PROXY`/`NO_PROXY`, no Windows/macOS system proxy), and that a tagged hub moves a newer client (a dev build from main, `--from-source`) back to the hub's build.

Missing tests from the review:
- Body cut mid-binary, over the cap without Content-Length (chunked, with a positive control that the same chunked answer fits a 32-byte cap), over the cap by Content-Length, and 404 as `Missing`. All in `a_body_cut_short_or_over_the_cap_is_a_failed_download`, which calls `get()` against a one-shot loopback answer.
- https to http redirect refused: the policy decision is now `follows(https, hops, scheme)`, and `redirects_from_https_never_go_down_to_http` tests it (https to http stops, loopback http may redirect, at most 10 hops).
- The finding 1 scenario: e2e step above.

## Skipped

- **4, system proxy.** Not enabled: it needs crates that are not in `Cargo.lock` (`windows-registry`, macOS `system-configuration`, the latter not cached, so new downloads), and it changes every reqwest client including the hub's Bot API. The prompt allowed documenting instead, so docs/remote-hub.md now says env-only. The existing PCTX proposal from the reviewer covers the risk lesson.
- **3, recovery "at shim start / next start of anything".** Not possible as asked. Every start (shim `cctg agent`, `cctg hook`, `cctg run`, the claude-cctg wrapper) runs the very file that is missing, so nothing can start and repair it. Recovery is in the running workers of other sessions on their next ⬆️ Обновить press, and a new `install.sh` also puts a file back. The exit window is closed by the gate. The only remaining case is a hard kill between the two renames, which install.sh and `cctg deploy` share.
- **Missing coverage, not added:** two `install()` threads racing (the lock plus the re-check make the second `Present`; the reviewer confirmed this by reading), a Windows swap with a held `cctg.old.exe` through `download::install` (covered through `deploy`'s `set_aside` tests; `swap_in` is shared), and `[::1]`/uppercase scheme (`[::1]` is not allowed, to match `install.sh`).
- **Nits** (log wording "is no tag", IMPL_SUMMARY wording): not code defects, left as is.

## Test results

Shared target `CARGO_TARGET_DIR=C:/Users/user/dev/cctg/target`, `CARGO_PROFILE_DEV_DEBUG=0`, `-j 1`.

- `cargo fmt --all --check`: clean.
- `cargo clippy -j 1 --workspace --all-targets -- -D warnings`: clean.
- `cargo test -j 1 -p cctg --lib -- download::`: 9 passed.
- `cargo test -j 1 -p cctg --test update_e2e`: 3 passed (including the extended `a_failed_download_leaves_the_old_binary_and_the_agent`).
- `cargo test -j 1 --workspace` (output in `scratch/fix_test_run.txt`): lib 629 passed / 1 ignored, 34 binaries ok. One failure, `transcript --test purity every_source_file_is_scanned`: `read_dir(CARGO_MANIFEST_DIR/src)` returned NotFound (os error 3). The directory exists, the crate is untouched, and a rerun (`cargo test -j 1 -p transcript --test purity`) gives 3 passed. The binary in the shared target was most likely built from another tree by the planner working in parallel. Not related to this fix.
