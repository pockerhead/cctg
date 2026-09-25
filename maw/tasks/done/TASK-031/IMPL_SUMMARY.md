# TASK-031 IMPL_SUMMARY

Branch `feature/cctg-install` (from `feature/remote-hub`, TASK-035 final code incl. fixer 72f2e50). Code commit `32af056`.

## 1. What was implemented

Applied `scratch/reviewer2/task031.patch` (PLAN_FINAL step 0) with `git apply --ignore-whitespace --reject` (`--3way` could not run: the repository lacks the base blobs of the 035 reviewer2 state). 16 files, +2047/-17:

| file | lines |
|---|---|
| `install.sh` (new, LF, mode 100755) | +783 |
| `crates/cctg/tests/install_e2e.rs` (new) | +819 |
| `crates/cctg/src/doctor.rs` (new) | +245 |
| `README.md` (new) | +89 |
| `.gitattributes` (new) | +2 |
| `crates/cctg/src/hook.rs` | +30/-9 |
| `crates/cctg/src/hub/ingress.rs` | +31/-2 |
| `.github/workflows/release.yml` | +12/-2 |
| `docs/remote-hub.md` | +11/-3 |
| `.github/workflows/ci.yml` | +5/-1 |
| `crates/cctg/src/main.rs` | +8 |
| `crates/cctg/Cargo.toml` | +5 |
| `crates/cctg/src/wire.rs` | +3 |
| `CLAUDE.md` | +2 |
| `Cargo.lock`, `crates/cctg/src/lib.rs` | +1 each |

Hash check (`scratch/reviewer2/verify_hashes.sh`): 14 x OK; MISMATCH only for the two hand-ported files below, both changed by the TASK-035 fixer after its reviewer2 patch.

### Hand-ported hunks

1. `crates/cctg/src/hub/ingress.rs`, hunk 2 (`Route::Ping` arm in `hook_request`): the fixer restructured the function (`let read = timeout_at(..)`, `drop(pre_auth.peer_place)`, `gate`). I put the same arm (`Ok(Ok((Route::Ping, _))) => { debug!(%peer, "ping answered"); Status::NoContent }`) right after the `Route::Hook` arm of the new `match read`. The 401 path (`pre_auth!` + `AUTH_FAIL_DELAY`) stays shared. The other 4 hunks (import, `Route::Ping`, `PING_PATH => Route::Ping`, test) applied with an offset.
2. `docs/remote-hub.md`, all 3 hunks:
   - "Один раз на сервере": the `install.sh --hub` paragraph added verbatim.
   - "Устройство" bullet: the reviewer text (installer, `cctg doctor`) plus the fixer's manual path kept as "Руками: ... `chmod +x` после каждого скачивания ...; регистрация ... `docs/poc.md`".
   - "Обновление": the `RELEASE=vX.Y.Z` condition added verbatim.
   - "Версии клиента и hub": the reviewer's installer sentence, then the fixer's manual steps (with `chmod +x`) as "Руками: ...".
   Logged as a `decision` in `log.jsonl`.

Plus: `git apply` wrote `install.sh` with CRLF (core.autocrlf=true), and `the_script_has_unix_line_endings` failed on it. I converted it to LF, staged it with `.gitattributes` (`i/lf w/lf attr/text eol=lf`) and set mode 100755 with `git update-index --chmod=+x`.

## 2. Deviations

No functional deviation from the plan. I found no defect in the patched code that I could prove with a test. Read critically: `install.sh` in full, `doctor.rs`, the hook/ingress/wire/main diffs, README. Minor observation, not changed: when `--hub` gets no public host and no terminal, the client line prints `--hub-host <this-server>`, and pasting it unedited gives a shell redirection error. It is a visible placeholder and the user has to edit it anyway.

## 3. Test results

All with `CARGO_TARGET_DIR=C:/Users/user/dev/cctg/target CARGO_PROFILE_DEV_DEBUG=0`, `-j 1`, one cargo at a time:

- `cargo fmt --all -- --check`: clean.
- `cargo clippy -j 1 --workspace --all-targets --locked -- -D warnings`: clean.
- `cargo test -j 1 -p cctg --locked --lib -- ingress::tests::a_ping doctor::`: 5 passed.
- `cargo test -j 1 -p cctg --locked --test install_e2e`: 6 passed (after the LF fix; before it, only `the_script_has_unix_line_endings` failed).
- Mutations: 14/14 KILLED (`scratch/implementer/mutations.log`). The reviewer's `run_mutations.py` asserts on CRLF `.rs` working copies for the two multi-line Rust mutations. I ran those 6 with a CRLF-tolerant copy, `scratch/implementer/run_mutations_crlf.py.txt`, which only adds the CRLF handling.
- `cargo test -j 1 --workspace --locked --no-fail-fast`: exit 0, 41 result lines all ok, 775 passed, 3 ignored, 0 failed (`scratch/implementer/workspace_test.txt`). The known `message_logs` flake did not fire.
- ShellCheck is not installed here and was not run locally. CI runs it (`ci.yml` job `test`, Linux). Linux/macOS `install_e2e` runs only in CI, per the plan.

## 4. Manual verification

- `bash maw/tasks/in_progress/TASK-031/scratch/reviewer2/verify_hashes.sh`: expect OK except `ingress.rs` and `docs/remote-hub.md` (hand-ported).
- `git ls-files -s --eol install.sh`: `100755`, `i/lf`.
- `cargo run -p cctg -- doctor` with a temp `USERPROFILE` pointing at a test `device.env` shows the version, secret, pin and both link lines. Do not run it against the real `~/.cctg` / live hub. Do not run `install.sh` on this machine (orchestrator decision 5).
- The first CI run on the pushed branch covers ubuntu (dash + shellcheck), macos-latest (codesign, shasum) and windows-latest.
