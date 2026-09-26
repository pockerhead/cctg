# TASK-050 (+ TASK-051) code review

Reviewed: `git diff main a0670ea -- . ':!maw'` on `feature/update-download`, all changed files read in full (download.rs whole file; agent.rs, wire.rs, slots.rs, status.rs, client.rs, deploy.rs, build.rs, channel.rs, release.yml, Dockerfile, README, remote-hub.md, update_e2e.rs, files_e2e.rs diffs plus the surrounding code: `update::Worker::plan`, `shim` header, `deploy::swap_in/set_aside/sweep/rename`, `slots::press_update/pump_updates/on_update_answer/outdated`, `install.sh install_binary`).

## 1. Verdict

**NEEDS_WORK**: the download path works and is safe, but when a tagged hub's download can't finish, the agent never falls back to a build already on disk. So the manual workaround that the new notices themselves recommend ends in the same notice again (finding 1). Everything else is minor.

Commands run (shared target, `CARGO_PROFILE_DEV_DEBUG=0`, `-j 1`), output in `scratch/review/cargo.txt`:
- `cargo clippy -j 1 --workspace --all-targets -- -D warnings`: clean.
- `cargo test -j 1 --workspace`: every binary `ok`, 0 failed (lib 625 passed / 1 ignored; `update_e2e` 3 passed, including both new tests; `files_e2e` passed).
- No dependency changes (`Cargo.toml`/`Cargo.lock` untouched); `aws-lc-rs` digest was already a dependency.

## Disconfirmation

What I expected would break it: **Windows, two sessions on one machine, second release in a row.** After the first update, the shims of every running session execute `cctg.old.exe` (the renamed image). The next swap has to get rid of `cctg.old.exe` before `cctg.exe -> cctg.old.exe`. If that failed, every later download would end in `download_failed`. A second case: two agents pressing at once, both writing `cctg.next.exe.part`.

Result: **the failure did not happen.** `deploy::set_aside` (deploy.rs:310-320) deletes the old file, or renames it to `cctg.old.exe.<nanos>` when it can't be deleted (a running image may be renamed), and `sweep` removes those later. Workers run from shim hard links (`shim.rs` header), so they never pin `cctg.exe`. Concurrent presses are serialised by the OS lock on `cctg.deploy-lock` (download.rs:226-238, same std `try_lock` as `deploy::deploy` deploy.rs:144-158). The re-check under the lock (download.rs:240) turns the second agent into `Present` -> `Reload`, so it never writes a second `.part`. The deploy tests even print the os-error-32 path (`cannot delete or rename the previous cctg.old.exe`), which maps to `Failure::Download` with the old file restored by `swap_in`.

Log triage: `log.jsonl` has no `dead_end` entries, only five implementer `decision`s. I checked each against the code. The "no fallback to disk on failure" decision (refs `agent.rs:download_outcome`) is the root of finding 1.

## 2. Confirmed correct

- **Only on a press.** `download::fetch` is spawned only from the `HubMsg::Update{release: Some}` arm (agent.rs:880-887). The hub sends `update` only from `pump_updates` for a pressed session (slots.rs `press_update` inserts into `updates`). Nothing else calls it.
- **Tag only to outdated agents.** `self.options.release.clone().filter(|_| self.outdated(conn))` (slots.rs, `pump_updates`). The second-round agent (hub's build) gets `None`, asserted in `a_release_hub_sends_its_tag_to_outdated_agents_and_tells_download_failures`.
- **Tag validation, three layers.** build.rs `checked()` (charset + length, build fails otherwise), `client.rs` test asserts `release().is_none_or(is_tag)`, and the agent re-checks `is_tag` (download.rs:61-67, 111-114) before any URL is built. `../x` gives `no_release_build` without a request (e2e asserts no `..` path was asked).
- **https only, no downgrade.** Non-`https://` bases other than loopback are refused before building a client (download.rs:134-136, unit test with `http://example.invalid`). The redirect policy stops any non-https hop for an https base, and the stopped 30x becomes `Failure::Download` (download.rs:142-148, 183-187).
- **Limits while streaming.** Content-Length pre-check plus a running cap per chunk (download.rs:188-200). 64 KiB for SUMS, 64 MiB for the binary, 90 s overall plus the reqwest per-request timeout. No decompression features, so the caps are real byte counts.
- **Checksum before touching anything.** The sha256 of the in-memory body is compared (download.rs:161) before `install()` opens the lock or writes the `.part`.
- **.part never used half-written.** `write_part` writes, `sync_all`s and chmods 755 (unix) before any rename. A failed swap removes the `.part` (download.rs:244-249). The `.part` name is shared with `cctg deploy`, but only under the same lock. `install.sh` uses a different name (`cctg.install[.exe]`).
- **Swap semantics match deploy/install.sh.** Windows: `swap_in` (current -> old with retries, part -> current, rollback of current on failure). Unix: a single rename over the file, so running processes keep the old inode.
- **Reload after success.** `Installed`/`Present` -> `follow_plan` -> `Worker::plan` sees `build_of(exe) != build` -> `Reload` -> `shim::SWITCH`. The shim re-links from `SOURCE_VAR`. The e2e test proves that the new worker registers with the asset's build and answers a ping, and that a second tagged press is `up_to_date` with no second binary GET.
- **Claude Code lines keep flowing during a download.** The fetch runs as a spawned task polled in its own `select!` branch (agent.rs:832-853). A second `update` meanwhile is ignored (agent.rs:880 filter). `follow_plan` is the former inline match moved unchanged (diff confirms).
- **No URL, no secret in logs.** The only `info!` lines carry tag/target/outcome, and every reqwest error is dropped at `get()` (download.rs:182, 195).
- **Proxy.** `HTTPS_PROXY`/`ALL_PROXY`/`NO_PROXY` from the environment are honoured (reqwest `auto_sys_proxy`). A loopback base uses `no_proxy()`. See finding 4 for system proxy settings.
- **Compat.** A tagless `update` is byte-identical to before (wire test). `HubMsg` has no `deny_unknown_fields`, so TASK-040 agents ignore `release`. The three new outcomes decode as `Other` on older hubs, giving `UPDATE_FAILED_NOTICE`. A tagless hub keeps the old flow (`a_failed_download_leaves_the_old_binary_and_the_agent`, last step). An old agent without `self_update` still gets `ANSWER_OLD_CLIENT`. `wire::VERSION` is not bumped.
- **CI wiring.** `RELEASE_TAG` is `github.ref_name` only for `refs/tags/v*`, else `''`. It goes to both Docker builds (image + static Linux binary) and the matrix `cargo build`. The Dockerfile `ARG CCTG_RELEASE=` is placed after the `COPY`. build.rs has `rerun-if-env-changed=CCTG_RELEASE`, so the cached `/src/target` does not keep a stale tag.
- **TASK-051.** Blank captions are already `None` at parse (channel.rs:408). `upload()` substitutes the cleaned basename (agent.rs:1423). The tool description and instructions ask for a caption. `files_e2e` checks a photo (blank) and a document (absent), checks that given captions are unchanged, and checks that neither name reaches the logs.

## 3. Issues

### 1. major: a tagged hub makes the disk-only path unreachable, and the notices send the user into a loop
`crates/cctg/src/agent.rs:843-846` (`(Ok(Err(failure)), _) => answer(download_outcome(failure))`), `crates/cctg/src/download.rs:154-159`, `crates/cctg/src/hub/status.rs:76,78`.

When `release` is `Some`, every failure of `fetch` is answered as final and `follow_plan` is never run. The check "is the disk file already the release" (download.rs:157) only happens *after* `SHA256SUMS` was fetched successfully. So:
- **No release build for the platform** (aarch64 Linux, Intel macOS, or an asset not uploaded yet): `NO_RELEASE_BUILD_NOTICE` says "поставьте клиент вручную (install.sh --from-source) и нажмите «Обновить»". The user does that, the running worker is still outdated, the hub sends the tag again, `fetch` returns `Missing` again (download.rs:115-118 or 156), and the same notice comes back. Before TASK-050, `plan()` would have seen the new file and answered `Reload`.
- **Agent can't reach GitHub** (no network, or a proxy the agent does not see, see finding 4): `DOWNLOAD_FAILED_NOTICE` says "нажмите ещё раз позже". A user who puts the release binary in place by hand (the manual route still described in docs/remote-hub.md:134) presses again, the SUMS GET fails first, and the answer is `download_failed` again. The correct file on disk is never picked up.

Proof path: agent.rs:880 -> spawn `fetch` -> download.rs:154 `get(SUMS)?` returns `Err(Download|Missing)` -> agent.rs:843 answers the failure. `Worker::plan` (update.rs:135-146), which would return `Reload` for a disk file that differs from the running build, is not called on this branch.

Suggested fix (small): on `Err(Failure::Missing | Failure::Download)`, run the disk plan first. If `plan()` is `Reload` (the file on disk differs from the running build, placed by install.sh, `cctg deploy` or another session), follow it. Otherwise answer the failure outcome. Keep `Checksum` final (nothing was written, and the disk is unchanged anyway, so the same rule gives the same result). Add an e2e step: a `nosum` release plus a newer file written in place of `exe` -> `reloading`.

### 2. minor: the 90 s limit does not bound the swap, and a timed-out download can still install after "old build stays" was reported
`crates/cctg/src/download.rs:119-121, 165-167, 232-238`.

`tokio::time::timeout` drops the `spawn_blocking(install)` JoinHandle, but a blocking task can't be cancelled. If the timeout fires while `install` waits for the lock (up to 30 s, for example behind a `cctg deploy` trial) or retries renames, the agent answers `download_failed`. The hub shows "Старая сборка на месте", and `install` then swaps the file in anyway. Nothing breaks: the next press finds `Present` -> `Reload`. But the notice is false, and the stated overall limit isn't a hard one. Fix: take the lock deadline from the remaining budget (pass `deadline` into `install`), or answer from the result of the blocking task and not from the timeout once `install` has started.

### 3. minor: a process exit during the Windows swap can leave no `cctg.exe`
`crates/cctg/src/agent.rs:1041-1044`, `crates/cctg/src/download.rs:270-273`.

The worker exits through `std::process::exit` (main.rs:171), which kills a running `spawn_blocking` install. The 5 s grace after stdin EOF covers the common case. Two cases remain: an install that starts near the end of that window, and Claude Code killing the process tree outright. If either lands between `current -> old` and `part -> current`, `cctg.exe` is gone. `rename` retries for up to 1.4 s on a scanner-held `.part`, which is exactly when that window is widest. The next shim then can't start, and neither can `claude-cctg` or new sessions. install.sh and `cctg deploy` have the same two-step window, so this is not new, but the download adds an unattended writer. Suggested fix: at `fetch`/`install` start (and ideally in `doctor`), if `current` is missing and `old` exists, rename `old` back. Or wait for the install itself (the blocking part) rather than the outer task.

### 4. minor: only env proxies are used, not the Windows/macOS system proxy
`Cargo.toml:12` (`reqwest` with `default-features = false`, no `system-proxy`). `cargo tree -e features -i hyper-util` shows `client-proxy` but not `client-proxy-system`.

The spec says "через системный/`HTTPS_PROXY` прокси". A proxy set only in Windows Internet Options or macOS network settings (common with desktop proxy clients) is ignored, and the download fails as `download_failed`. Together with finding 1, the user then has no way out through the button. The docs mention only `HTTPS_PROXY`, so the behaviour is at least described. Fix: enable reqwest's `system-proxy` feature (this affects the hub's Bot API client too; check that `NO_PROXY`/loopback still bypass it), or say explicitly in the notice/docs that only `HTTPS_PROXY` in claude's environment counts.

### 5. minor: `local_http` accepts `http://localhost:80@evil.example/`
`crates/cctg/src/download.rs:171-177`.

The host is taken as the text before the first `/` or `:`. For `http://localhost:80@evil.example/r` that is `localhost`, but the real host is `evil.example` (userinfo `localhost:80`). Checked with a URL parser: `hostname == evil.example`. Such a base passes the "http only on this machine" rule and also switches the proxy off. The base comes only from `CCTG_RELEASE_BASE_URL` in the user's own environment, so it can't be exploited remotely, but the rule the code promises is not the rule it enforces. Fix: parse with `reqwest::Url` and check `scheme() == "http"`, `host_str()` in {`127.0.0.1`, `localhost`, `[::1]`} and empty `username()`/`password()`. Add the case to `only_https_or_this_machine_is_asked`.

### 6. minor: `NO_RELEASE_BUILD_NOTICE` covers a transient cause with permanent advice
`crates/cctg/src/download.rs:185` (404 -> `Missing`), `hub/status.rs:78`.

A 404 on `SHA256SUMS` or on the asset also happens while the release job is still uploading (the tag's image can be pulled before the client assets exist). The notice then says the platform has no build and advises `install.sh --from-source`. That is wrong advice for a supported platform, and with finding 1 it is a dead end. Fix: reword to "в релизе hub нет сборки для этой платформы (или она ещё не выложена)". Advise pressing again later, and advise the manual install only once finding 1 makes a manual install effective.

### 7. minor (spec-conforming, worth a line in docs): a tagged hub can downgrade a newer client
`hub/slots.rs outdated()` means "a different build", not "an older build". A client running a newer build (a dev build from `main`, or install `--from-source`) against a tagged hub is shown as outdated. "Обновить" then replaces it with the older release binary. The spec asks for exactly "the hub's version", so this is intended, but docs/remote-hub.md should say it.

## 4. Missing coverage

- A download cut **mid-binary** and a binary **over the size cap** (chunked, no Content-Length). Only the `SHA256SUMS` request is cut in `a_failed_download_leaves_the_old_binary_and_the_agent` (`/cut/` is the tag directory, so the SUMS GET is the one cut).
- The https -> http redirect stop. There is no test for the custom redirect policy; a loopback https server isn't needed, and a unit test on the policy closure or an http-base test with `https=true` forced would do.
- Two `install()` calls racing on one bin dir (two threads): one `Installed`, one `Present`, one `.part` at most, the old file set aside once.
- Lock held by another process (a `cctg deploy` stand-in holding `cctg.deploy-lock`): `install` waits, then fails as `Download` with the file untouched.
- Windows: a download swap when `cctg.old.exe` already exists and is held open (the `set_aside` rename path) through `download::install`, not only through `deploy`.
- The finding 1 scenario: tag present, fetch fails, newer file already on disk -> `reloading`.
- `local_http` with userinfo (`http://localhost:1@x/`), `[::1]`, uppercase scheme.

## 5. Nits

- download.rs:112: `info!("the hub's release tag is no tag; …")` could say "not a valid tag".
- `Failure::Missing` is also returned for "platform without release builds" and for a bad tag. The outcome name `NoReleaseBuild` fits; the notice (finding 6) is the only thing that reads oddly.
- IMPL_SUMMARY §3 lists `a_failed_download...` as covering a "cut connection", but it is the SUMS request that is cut, not the binary (see Missing coverage).
