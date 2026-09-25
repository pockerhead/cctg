# TASK-031 IMPL_REVIEW (code-reviewer)

Reviewed: `git diff 72be4a1 32af056 -- . ':!maw'` (16 files, +2047/-17) on `feature/cctg-install`, against TASK_FINAL.md, PLAN_FINAL.md, OPEN_DECISIONS.md and IMPL_SUMMARY.md. Evidence: `scratch/code-reviewer/`.

## 1. Verdict

**NEEDS_WORK.** The core works and is well tested: the Windows gates are green, ShellCheck is clean and the secret handling holds. Two things still break the spec. `--uninstall` deletes `device.env` lines the script never wrote (spec: "убирает только своё"). An empty secret gives a successful install with no secret, and the plan requires a clear error for that. Both are proven with a repro. The rest is minor and cheap to fix.

## Disconfirmation (done first)

The counter-example I tested first: a hub secret containing `'`, `$`, `#`, `"`, `\` and `` ` ``, read from the environment under dash (Ubuntu `/bin/sh`), ends up in `device.env` as bytes that dotenvy reads differently, or leaks into a child's argv.
- `squote` under `dash`, `bash` and `bash --posix`: `a'b$c"d\e`f#g''h` becomes `'a'\''b$c"d\e`f#g'\'''\''h'`, and `eval` gives back the same bytes in all three shells.
- `dotenvy-0.15.7/src/parse.rs` `parse_value`: `'` turns on strong quoting (no `$` substitution, no escapes). Outside quotes, `\'` is an escaped quote. So the value is read back byte for byte.
- argv: the secret only passes through builtins (`printf`, `case`, `[`) and through stdin (`tr`, `grep | tail` on the file). `unset CCTG_HUB_SECRET` runs before `offer_claude` and before doctor (install.sh:304-307).
- Result: **the counter-example did not hold.** The e2e test also covers this with the same secret, and doctor reports "took the secret".

A second counter-example came from the task prompt: `curl | sh` prompts in a real Windows Git Bash never reach the user, or the Windows test waits on the developer's console. I probed it with a hidden console (`scratch/code-reviewer/ttyprobe3.py.txt`, `repro_output.txt`):
- A piped `sh` under a console-owning Git Bash gets `/dev/cons0`, and `stty -g` works, so the prompts work.
- `bash -c` with a piped stdin, which is the test harness case, has no `/dev/tty`, so no question appears and nothing hangs.
- **This did not hold either.**

## 2. Confirmed correct

- **Checks run by the reviewer (Windows, shared target, `-j 1`):**
  - `cargo fmt --check` clean, `cargo clippy --workspace --all-targets --locked -D warnings` clean.
  - `cargo test --workspace --locked --no-fail-fast`: 775 passed, 0 failed, 3 ignored.
  - `install_e2e`: 6/6 (`scratch/code-reviewer/{fmt,clippy,workspace_test}.txt`).
  - ShellCheck 0.10.0 (a temporary download, since deleted) with `--shell=sh|dash|bash` on install.sh: exit 0. `dash -n` ok. `install.sh` has index mode `100755` and `i/lf`, and its hash equals the reviewer2 reference.
- **curl | sh safety (install.sh:26-144, 783):** the body is `main` and the call is on the last line, so a cut-off download runs nothing. By the time `main` runs, stdin is fully consumed, and children that could read it get `</dev/null`. `set -eu`. `die` is never called inside `$(...)` where it would only leave a subshell. Subshells do not inherit the `EXIT` trap, so `$tmp` is not removed early.
- **Prompts:**
  - Every question goes through `/dev/tty` only (`interactive`, `ask`, `read_hidden`, install.sh:206-273).
  - `stty -echo` is restored on normal return, on `EXIT`, and on INT/TERM through `trap 'exit 130'`.
  - Without a terminal, a missing secret dies with "no hub secret: set CCTG_HUB_SECRET or use --secret-file". A missing pin for a remote hub dies with "needs --pin".
  - The Claude Code installer runs only after a yes or with `--yes`. It uses Anthropic's own commands (`curl ... | bash`, `irm ... | iex`).
- **Secrets:**
  - The secret is never printed on the device.
  - `device.env` and `hub.env` are written under `umask 077` and then `chmod 600`, via tmp + `mv`.
  - The hub token is unset right after it is read and never printed. The hub secret appears once, only in the client line (a user decision).
  - Doctor output contains no secret, and `ConfigProblem`/`PostError` Display carry no values (device.rs:44-73).
- **SHA256SUMS fails closed (install.sh:402-411):** it fails on a download error, a missing line, several lines, a mismatch, or no `sha256sum`/`shasum` (empty output does not match). `--proto =https --proto-redir =https --tlsv1.2`. Plain http is allowed only to 127.0.0.1/localhost. The test `a_bad_checksum_installs_nothing` covers it.
- **Tag pinning:**
  - `RELEASE=v0.1.0` selects the asset `cctg-<tag>-<triple>`, which matches the names release.yml produces.
  - The guard in release.yml's `image` job fails before the push, and the `release` job `needs: [image, client]`, so nothing gets published.
- **Update in place:**
  - `put` rewrites a file only when its bytes change, and test 2 checks the mtime.
  - On Windows the running `cctg.exe` is renamed to `cctg.old.exe`. Test 3 checks this with a live `cctg run`.
  - Worker hard links in `cctg-workers/` keep the old image.
  - I verified that MSYS `rm -f`/`mv -f` on a running exe succeed.
- **User files are left alone:** install and uninstall never write `~/.claude/settings.json` or `~/.claude.json` (checked byte for byte in test 1). The wrappers carry the mark. A foreign wrapper is moved aside once.
- **Wrapper argument order (install.sh:547-562):** `--settings S` comes last, so the prompt stays a prompt; test 1 checks the real argv on Unix. The `.cmd` uses CRLF and `%*`. `check_paths` refuses `" $ \` % \`, so nothing reaches JSON, sh or cmd unescaped.
- **`POST /v1/ping` (ingress.rs:755-760, 1000-1003):**
  - The route comes after `read_request`, so the secret check is shared.
  - The 401 path (`pre_auth!` + `AUTH_FAIL_DELAY`), `REQUEST_TIMEOUT`, `MAX_HEAD`, the per-peer caps and the semaphore are the same as for `/v1/hook`, so ping gives no new oracle.
  - It produces no event; `a_ping_checks_the_secret_and_is_no_event` covers it.
  - The `hook::exchange` refactor keeps `post` semantics (204 means Ok, anything else is `Status`, a malformed answer is `BadResponse`).
- **`cctg doctor` (doctor.rs):**
  - It reads the same `DeviceConfig::load`, and the installer unsets the env secret first, so doctor tests the file.
  - A remote hub without a pin is never contacted. An older hub's 404 counts as "reachable, not checked".
  - 4 unit tests.
- **`--hub`:**
  - The `openssl` commands run in the hub dir with `MSYS_NO_PATHCONV=1` per command. The certificate is loaded by `tls::Acceptor::from_files` in the test.
  - The pin is normalised from openssl output.
  - A hand-changed `compose.yml` is kept.
  - `wait_hub` waits for `hub started, polling` or `Error: ` up to 120 s and prints the last 20 lines on failure.
  - Cleanup is `compose down` + the compose files; `hub.env`, `tls/` and the volume stay.
- `libc` is a Unix-only dev-dependency that was already in the lock through tokio, used for `setsid` in the test. No new runtime crate.

## 3. Issues

### 1. major: `--uninstall` deletes `device.env` lines the script never wrote

**Where:** install.sh:581 (`remove "$env_file"`).

**Failure scenario:**
1. A machine has a `device.env` from the manual setup with its own keys, for example `CCTG_HOST=my-laptop` or `CCTG_STATE_DIR=...`. docs/remote-hub.md documents this manual setup, and this machine has one.
2. The installer keeps those lines on install (grep -v of the managed keys, install.sh:481-492).
3. `--uninstall` then removes the whole file.

A custom `CCTG_HOST` disappears with it. After a reinstall the device reports a different host name, and the hub gives every folder a new slot and a new topic. The spec says "`--uninstall` убирает только своё".

**Proof:** `scratch/code-reviewer/repro_empty_secret_and_uninstall.sh.txt` and `repro_output.txt`. In a temp home, a pre-existing `CCTG_HOST=my-laptop` / `CCTG_STATE_DIR=...` survives the install, then after `--uninstall` all of `~/.cctg` is gone.

**Fix:** on uninstall, drop only the `$MANAGED` lines (plus the header comment and the `CCTG_HOST` line the script added on macOS). Delete the file only when nothing else is left. Add that case to test 1: a foreign line in `device.env` survives uninstall.

### 2. minor: an empty secret gives a "successful" install with no secret

**Where:** install.sh:310-322.

**Failure scenario:** any one of these:
- `--secret-file` names an empty file.
- The file's first line is empty (for example a leading blank line).
- The user just presses Enter at the hidden prompt.

In each case `secret` stays empty. `[ -z "$secret" ] || check_secret` skips the check, `device.env` gets no secret, and the script prints "done" with exit 0. Doctor's lines do show `secret: CCTG_HUB_SECRET is not set`, but the plan says a missing required secret is a clear error. The pin prompt already does this right (`[ -n "$answer" ] || die`, install.sh:299).

**Proof:** repro above: exit 0, "done", and `device.env` without `CCTG_HUB_SECRET`.

**Fix:** after the branch chain, `[ -n "$secret" ] || [ -n "$(old_line CCTG_HUB_SECRET)" ] || die "no hub secret ..."`. For a `--secret-file` whose first line is empty, die with "is empty". Add a test with an empty secret file.

### 3. minor: the Windows binary swap has no rollback and no retry

**Where:** install.sh:424-433.

**Failure scenario:** `mv -f "$exe" "$old"` succeeds, then `mv -f "$new" "$exe"` fails. `deploy.rs:40-45` (`RENAME_WAITS`) documents the cause: a scanner holding a freshly written file for a moment. `set -e` then exits with a raw `mv` error. `~/.cctg/bin/cctg.exe` no longer exists (the binary is in `cctg.old.exe` and `cctg.install.exe`), so every hook, the wrapper and `mcp.json` point at a missing file until the user notices.

The window is small: `--version` already ran the new file, so a scan is likely done. But the failure is unrecoverable for the user.

**Proof:** code reading. `cctg deploy` retries this exact rename and rolls back (deploy.rs:11, 40-45).

**Fix:** `mv -f "$new" "$exe" || { sleep 1; mv -f "$new" "$exe"; } || { mv -f "$old" "$exe"; die "could not put the new binary in place; the old one is back"; }`.

### 4. minor: the `compose.yml.new` rule cannot tell an upstream change from a user edit

**Where:** install.sh:637-649.

**Failure scenario:** a later tag changes `deploy/compose.yml` (a new env var, a healthcheck, image settings) on a hub whose `compose.yml` was never edited. `--hub` of the new tag sees "differs", keeps the old file and prints "kept your changed compose.yml". That message is false, and the compose update never applies unless the user moves the file by hand. It is the "write-once" behaviour that reviewer2's decision (log.jsonl line 13) rejected, only with a notice.

**Proof:** code reading. The only test (`a_hub_is_set_up_with_docker_compose`) covers a real user edit, not an upstream change.

**Fix:** keep the release copy that was last installed (for example `.compose.yml.release`). Overwrite when the current file equals that copy. Write `.new` and the message only when the user changed it.

### 5. minor: the printed client line can be wrong or unusable

**Where:** install.sh:743-748.

**Failure scenario:**
- **(a) Changed ports.** The script deliberately keeps a `compose.yml` with changed host ports (`52191:47291`, docs "Порты"), but still prints `--hub-host <host>`, which becomes `<host>:47291/47292`. A device installed from the line cannot reach the hub. Doctor fails on it, and the installer itself printed the line.
- **(b) No address.** With `--yes` or no terminal and no `--public-host`, the line carries `--hub-host <this-server>`. Pasted as is, the shell reads `<this-server>` as redirections: `<this-server` fails, or a file named `this-server` becomes `sh`'s stdin instead of the script. `--public-host` is also printed unquoted and unchecked.

**Proof:** code reading. In test 5 the port change is made and the line is not checked against it.

**Fix:**
- In non-interactive mode, require `--public-host` before any work (die early like the token), or print a plain `HOST` word with a warning.
- When `compose.yml` differs from the release copy, print `--agent-addr`/`--hook-addr` or a warning that the ports differ.
- Run `--public-host` through `check_addr` (host part).

### 6. minor: `--hub` of a tag runs the `:latest` image

**Where:** deploy/compose.yml:19 (035 file, used by install.sh:633-649).

**Failure scenario:** the script and the client line are pinned to `$RELEASE`, but the hub image is `ghcr.io/pockerhead/cctg:latest`. Running an older tag's `--hub` (a saved command, an old README link) starts the newest hub and prints a client line for the old binary. Every client then gets the outdated warning, and "Обновить" has no newer file to take. This goes against the stated D2 goal that script, configs and binary belong together.

**Proof:** code reading. release.yml tags the image `latest` + `X.Y.Z`, and the compose file pins `latest`.

**Fix:** write `CCTG_IMAGE_TAG=${RELEASE#v}` into the hub `.env` and use `image: ghcr.io/pockerhead/cctg:${CCTG_IMAGE_TAG:-latest}` in compose.yml. At least, document the mismatch in README "Обновление".

## 4. Missing coverage

- **Remote hub without a pin, no terminal:** the script must die with "needs --pin" and write nothing. Neither of the two "clear failure without a tty" paths is tested at the script level.
- **No secret, no terminal:** the script must die with "no hub secret" and write nothing.
- An empty `--secret-file`, or a first line that is empty (issue 2).
- `--uninstall` with foreign lines in `device.env` (issue 1). It should also cover `cctg-workers/*`, `cctg.old.exe.<n>` and a left-over `cctg.install[.exe]`, since the code handles them but no test does.
- An upstream `compose.yml` change without a user edit (issue 4).
- `wait_hub` failure path: a docker stub whose logs say `Error: ...` should give exit 1 and print the log tail.
- `--pin` given as a whole openssl line (`sha256 Fingerprint=AB:..`) through the installer, and a bad pin being refused.
- `check_paths` refusing a home with `$` or `%`.
- `--hub` without `--public-host` in non-interactive mode (issue 5b).

## 5. Nits

- **release.yml:40** `grep -qx "RELEASE=${GITHUB_REF_NAME}"` treats `.` as a regex dot, so `RELEASE=v0x1x0` matches tag `v0.1.0`. Use `grep -qxF`.
- **install.sh:588** `rmdir "$wrap_dir"` removes an empty `~/.local/bin`, a folder Claude Code's installer shares.
- **Uninstall and the moved-aside wrapper:** uninstall neither restores nor mentions `claude-cctg.before-cctg-install`.
- **`*.tmp` leftovers:** stale `device.env.tmp` / `hub.env.tmp` from an interrupted run (secret inside, mode 600) are never cleaned up.
- **install.sh:568-569** The "left ... (in use?)" branch is dead in Git Bash: MSYS `rm -f` of a running exe succeeds (verified, `repro_output.txt`).
- **install.sh:727-732** As non-root, `tls/key.pem` is world-readable (644). It is disclosed, but a root-free fix exists: `docker run --rm -v "$hub_dir/tls:/tls" --user 0 <image> chown 10001 /tls/key.pem`, or `setfacl`.
- **install.sh:680** The `HTTPS_PROXY` question comes back on every `--hub` re-run when no proxy is set, because an empty answer writes no line.
- **install.sh:339-340** `[Y/n]` takes `yes` but not `Yes`.
- **install.sh:226-231** `is_loopback` treats `127.0.0.1.nip.io:5` as loopback, but Rust `tls::is_loopback_addr` does not. The script then asks no pin and doctor fails with `PlainRemote`. Harmless, the Rust side is the gate. Also, re-running with new `--hub-host` keeps an old pin, and there is no flag to clear a managed key.
- **Wrapper without `--strict-mcp-config`:** unlike docs/poc.md, the wrapper drops `--strict-mcp-config`. A machine that still has the older user-scope `cctg` registration (`claude mcp add --scope user`, CLAUDE.md "Channels") could get two servers named `cctg`. I have not verified which one Claude Code picks. README could tell those users to run `claude mcp remove -s user cctg`.
- **`install.ps1`:** TASK_FINAL's acceptance criterion names `install.ps1`, but PLAN.md:12 records "без `install.ps1` (решение пользователя)". That decision is not in OPEN_DECISIONS.md. QA/orchestrator should adjust the checkbox wording so the gate does not read as unmet.
- **docs/remote-hub.md:42:** the manual path still fetches `hub.env.example` from `main`, not from the tag.
