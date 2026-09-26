# TASK-050 implementer summary (small-fix, TASK-051 folded in)

Pre-flight: every named file, function and API exists with the shape the orchestrator notes assume (`update::Worker`/`Plan::Reload`, `shim::SOURCE_VAR`, `client::build_of`/`short`, `wire::HubMsg::Update`/`UpdateOutcome`, `slots::press_update`/`pump_updates`/`outdated`, `build.rs` `CCTG_BUILD_ID`, `release.yml` asset names and `SHA256SUMS`, `install.sh` swap, `deploy::swap_in`/`rename`). Not blocked.

Code commit: `feat: the update button downloads the hub release itself (TASK-050, TASK-051)` on `feature/update-download`.

## 1. What was implemented

| File | +/- | What |
|---|---|---|
| `crates/cctg/src/download.rs` (new) | +353 | `fetch(base, tag, exe)`: tag check, compile-time target (`x86_64-pc-windows-msvc`, `x86_64-unknown-linux-musl` for x86_64 Linux, `aarch64-apple-darwin`), `GET <base>/<tag>/SHA256SUMS` (64 KiB cap), a disk file already equal to the asset's sum means `Present` and no download, else `GET` asset (64 MiB cap, 90 s total), sha256 check, then under the `cctg.deploy-lock` (polled up to 30 s) a re-check, `sweep`, write `cctg.next[.exe].part` (fsync, chmod 755 on unix), swap (Windows: `deploy::swap_in`, the running exe to `cctg.old.exe`; unix: rename over). Base `CCTG_RELEASE_BASE_URL` or `https://github.com/pockerhead/cctg/releases/download`; https only, plain http only to 127.0.0.1/localhost (then `no_proxy`); redirects never downgrade to http. No reqwest error or URL is logged: one `info!` with tag, target and outcome. Unit tests: tags, asset names, SHA256SUMS lines, base checks, swap. |
| `crates/cctg/src/agent.rs` | +123/-46 | `update` with `release` and a self-updating worker spawns `download::fetch` as a task (`downloading`), answered in its own `select!` branch, so Claude Code lines keep flowing. Success goes to the old plan path, now `follow_plan()` (the former inline match, moved unchanged); failure answers `download_failed` / `no_release_build` / `checksum_mismatch`. A second `update` during a download is ignored like one during a leave. At stdin end a running download gets up to 5 s so a swap is not cut by the exit. TASK-051: an absent or blank caption becomes the file's (cleaned) name. |
| `crates/cctg/src/wire.rs` | +57/-2 | `HubMsg::Update.release: Option<String>` (skipped when `None`), `UpdateOutcome::{DownloadFailed, ChecksumMismatch, NoReleaseBuild}`; no VERSION bump. Compat test: a tagless `update` line is byte-identical to before, a tagged one decodes, the three outcomes decode (older hubs read them as `Other`). |
| `crates/cctg/src/hub/slots.rs` | +90/-8 | `Options.release`; `pump_updates` sends the tag only when the conn is `outdated` (the next agent, already the hub's build, gets `None`); the three outcomes map to their notices. New test `a_release_hub_sends_its_tag_to_outdated_agents_and_tells_download_failures`; the existing flow test now asserts `release: None` for a tagless hub. |
| `crates/cctg/src/hub/status.rs` | +4 | `DOWNLOAD_FAILED_NOTICE`, `CHECKSUM_NOTICE`, `NO_RELEASE_BUILD_NOTICE` (Russian, each says the old build stays). |
| `crates/cctg/src/hub/mod.rs` | +2 | `release: client::release()` into `Options`; the start log line shows the tag. |
| `crates/cctg/build.rs` | +21/-5 | Bakes `CCTG_RELEASE` (checked like `CCTG_BUILD_ID`, empty when unset), rerun on its change. |
| `crates/cctg/src/client.rs` | +9 | `client::release() -> Option<&'static str>`. |
| `crates/cctg/src/deploy.rs` | +3/-3 | `swap_in`, `sweep`, `rename` made `pub(crate)` for the download swap. |
| `crates/cctg/src/channel.rs` | +13/-4 | TASK-051: `send_file` caption description ("Always give one ... Without it the file name is shown"), instructions ask for "a short caption saying what the file is"; test assertions. |
| `.github/workflows/release.yml` | +12 | `RELEASE_TAG` env (tag name on `v*` tags, empty on main), passed as `CCTG_RELEASE` to both Docker builds and the client matrix builds. |
| `Dockerfile` | +6 | `ARG CCTG_RELEASE=` passed to `cargo build`. |
| `crates/cctg/tests/update_e2e.rs` | +349 | Two e2e tests with a real shim + worker process, the real `serve_agents` and a loopback fake release server (see 3). |
| `crates/cctg/tests/files_e2e.rs` | +46 | TASK-051 through the fake Bot API: a photo with a blank caption and a document without one arrive with `bare-shot.png` / `report.txt` as caption; given captions unchanged; names not in logs. |
| `README.md`, `docs/remote-hub.md` | 1 line each | "Обновить" now downloads the hub's release; tagless hubs keep the disk-only behaviour. |

## 2. Deviations and decisions (all in `log.jsonl`)

- "Disk binary differs from the hub's build" is decided by the sha256 of the shim's source file against the asset's `SHA256SUMS` line, not by build ids (the agent cannot know the disk file's build id without running it). Costs one small GET per press on a release hub.
- Three additive outcomes instead of one `DownloadFailed`: the acceptance criteria want a clear answer for network, checksum and missing asset each; `notify` takes `&'static str`, so three fixed notices. `DownloadFailed` also covers timeout, size cap, lock wait and write/rename failures.
- Timeout 90 s (not 120 s) so the answer arrives inside the hub's `UPDATE_WAIT` of 120 s.
- No macOS `codesign`: the asset is written byte-identical, so the linker's ad-hoc signature stays valid; `install.sh` does not sign either. Not verified on a Mac (no macOS machine here); `update_e2e` signs only its rewritten test binary, as before.
- A failed download does not fall back to a plain reload of whatever is on disk; the user presses again.
- On unix the swap is a single rename over the file (no `cctg.old`), as `install.sh` does.

## 3. Test results

- `cargo fmt --all --check`: clean.
- `cargo clippy -j 1 --workspace --all-targets -- -D warnings`: clean.
- `cargo test -j 1 --workspace` (shared target, `CARGO_PROFILE_DEV_DEBUG=0`): 41 test binaries, 809 passed, 0 failed, 3 ignored. Summary lines in `scratch/test_run.txt`.
- New e2e (`cargo test -p cctg --test update_e2e`):
  - `the_hubs_release_is_downloaded_checked_and_taken`: `update{release}` -> `reloading`; the file now has the asset's bytes (Windows: the original in `cctg.old.exe`); after `released` the new worker registers with the asset's build and answers Claude Code; a second tagged `update` -> `up_to_date`, `SHA256SUMS` asked twice, the asset once, no `.part` left.
  - `a_failed_download_leaves_the_old_binary_and_the_agent`: bad sum -> `checksum_mismatch`, no line -> `no_release_build`, 404 -> `no_release_build`, cut connection -> `download_failed`, `../x` tag -> `no_release_build` without a request; after each the file is unchanged and the channel answers a ping; nothing written next to the binary; a tagless `update` -> `up_to_date` (old behaviour).
  - The existing `a_new_binary_is_taken_without_losing_a_line` still passes (tagless path).
- Old agent: covered by the wire compat test (the `release` field is ignored by an agent that does not know it; serde does not deny unknown fields).

## 4. How to verify manually

1. `cargo test -p cctg --test update_e2e --test files_e2e` and `cargo test -p cctg --lib -- download:: wire:: a_release_hub`.
2. Real run (needs a tag): tag a release with `install.sh` `RELEASE=<tag>`; the hub image of that tag logs `release=<tag>` at start. On a client of an older build press ⬆️ Обновить: the topic shows "✅ Клиент cctg обновлён." (or the restart flow), `~/.cctg/bin/cctg[.exe]` has the sha256 of the release asset (`cctg.old.exe` on Windows is the previous one). Pull the network or point `CCTG_RELEASE_BASE_URL` (in claude's environment) at a bad mirror: the topic gets the Russian failure notice and the old build keeps running.
3. TASK-051: from a session call `send_file` without a caption: the photo/document in the topic carries the file name as caption.
