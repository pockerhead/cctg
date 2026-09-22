# QA_REPORT — TASK-002 (bootstrap cargo workspace and cctg CLI skeleton)

Stage: qa (claude/opus, effort=medium)
Tested commit: `b40bf63` (HEAD of `chore/bootstrap-workspace`), working tree clean except pre-existing `?? .claude/`
Mode: small-fix — verified against `task.md` directly, no plan file.

## 1. Environment

No docker-compose, no Makefile/justfile, no dev server. Decision tree landed on option 3: existing cargo test
infrastructure, run directly in the working directory `C:/Users/user/dev/cctg`.

- Windows 11 Pro 10.0.26200, `rustc 1.95.0 (59807616e 2026-04-14)`, `cargo 1.95.0 (f2d3ce0bd 2026-03-21)`.
- Nothing was spun up, nothing needs cleanup. No mocks. No network.
- One throwaway cargo probe crate was created under `scratch/qa_tracing_probe/` for the disconfirmation
  experiment (section 3) and **deleted afterwards**; `git status` is back to its pre-QA state.

Reproduce:

```bash
cd C:/Users/user/dev/cctg
cargo clean
cargo build --workspace
cargo test --workspace
cargo clippy --workspace --all-targets -- -D warnings
cargo fmt --all -- --check
cargo metadata --format-version 1 --no-deps
cargo tree -p transcript --prefix none
cargo tree -p transcript -e all --prefix none
ls target/debug/*.exe
git status --porcelain
```

I did **not** run `scratch/verify_bootstrap.ps1` as evidence. Everything below is from commands I issued myself.
The `.ps1` was read and its one fixed check was re-tested in isolation (section 5).

Not executed and why: nothing in this task touches Telegram, the channel transport or hooks at runtime — there is
no business logic to exercise. No live bot, no MCP handshake, no hub/agent connection was tested, because none
exists yet. `crates/transcript` has no `parse`/render functions yet, so the qa-tooling instruction to render a real
jsonl from `~/.claude/projects/` is not applicable to this task (nothing to call).

## 2. Test results

### Existing suite (from a full `cargo clean`, not cached)

| command | result |
|---|---|
| `cargo clean` | removed 1000 files, 286.4 MiB |
| `cargo build --workspace` | exit 0, `Finished dev profile ... in 6.43s` |
| `cargo test --workspace` | exit 0 — unit `cctg` 1 passed; integration `tests/stdout.rs` 1 passed; `transcript` 0 tests; doc-tests 0 |
| `cargo clippy --workspace --all-targets -- -D warnings` | exit 0, zero warnings |
| `cargo fmt --all -- --check` | exit 0, no diff |

All three mandatory toolchain gates are clean on a cold build, not just on a warm cache.

### New probes written and run by QA

1. **Full CLI surface, byte-exact stdout/stderr on nine invocations** (the reviewer covered seven; I added
   `--version` and `help hub`):

| args | exit | stdout bytes | stderr bytes |
|---|---|---|---|
| `--help` | 0 | 316 | 0 |
| `--version` | 0 | 11 | 0 |
| `help hub` | 0 | 77 | 0 |
| `hub` | 0 | **0** | 0 |
| `agent` | 0 | **0** | 0 |
| `hook SessionStart` | 0 | **0** | 0 |
| *(no args)* | 2 | **0** | 316 |
| `bogus` | 2 | **0** | 103 |
| `hook` *(missing event)* | 2 | **0** | 136 |

2. **Same three functional paths with `RUST_LOG=trace`** — stdout 0 bytes, stderr 0 bytes for all three. This is the
   observation that exposed the finding in section 3: the subscriber is installed but no event is ever emitted.

3. **`hook` with JSON on stdin** (`echo '{"session_id":"x"}' | cctg hook SessionStart`) — exit 0, stdout 0, stderr 0.
   The skeleton does not choke on, or echo, hook stdin.

4. **`hook` startup latency**, 3 runs: 40 / 37 / 38 ms. Well inside the 1.5 s `SessionEnd` hook budget from the hooks
   domain, despite `#[tokio::main]` with `rt-multi-thread`. Informational only.

5. **Disconfirmation experiment** — see section 3.

6. **Independent re-test of the fixed `cargo tree` regex** — see section 5.

## 3. Disconfirmation (mandatory step)

**Counter-example chosen before testing anything:** *the committed code writes to stdout on a non-CLI path, or the
new regression test that is supposed to prevent that cannot actually detect it.* This is the highest-stakes item in
the task, because the channel domain law says "stdout is reserved for JSON-RPC only; one stray print breaks the
transport", and `FIX_SUMMARY.md` claims the risk is now closed by `crates/cctg/tests/stdout.rs`.

The first half of the counter-example **did not hold**: nine invocations above confirm no functional path writes a
single byte to stdout; only `--help`, `--version` and `help <sub>` do, which is the CLI's own output and explicitly
allowed. `main.rs:37` pins the writer to `std::io::stderr` and there is no `print!`/`println!` in the workspace.

The second half **did hold — the guard is vacuous today.** I copied the committed `main.rs` into a throwaway crate,
removed *only* the `.with_writer(std::io::stderr)` line, copied `tests/stdout.rs` verbatim (retargeting the
`CARGO_BIN_EXE_*` env var) and ran it:

```
test subcommands_do_not_write_to_stdout ... ok
```

The test passes with the stderr writer deleted. Reason: `tracing` is declared but never used — no `tracing::` macro
exists anywhere in the workspace, so no event is ever formatted, so the writer is never exercised (confirmed
empirically by the `RUST_LOG=trace` runs producing zero bytes on both streams). The test asserts a property that is
currently true for a reason unrelated to the thing it is meant to protect.

That the risk is real and not theoretical: adding one `tracing::info!("hub starting")` to the same writer-less probe
produced **64 bytes on stdout, 0 on stderr** — `tracing_subscriber::fmt()`'s default writer is stdout.

Net effect: the code as committed is correct, and the test will start doing real work the moment a subcommand logs
at startup. But `FIX_SUMMARY.md`'s framing ("нет автоматической защиты чистоты stdout … Добавлен …") overstates what
the test currently guarantees. Low severity, no code change required for this task.

## 4. Log triage (`log.jsonl`)

Two entries, both verified against primary output rather than taken on trust:

- `dead_end` (fixer): PowerShell `.NET ProcessStartInfo.ArgumentList` unavailable, so the first stdout probe ran the
  binary with no arguments and every process exited 2. Ref `crates/cctg/src/main.rs`. **Verified as a tooling
  dead end, not a code defect** — my own probes pass the arguments correctly and all three subcommands exit 0 with
  empty stdout. Exit 2 with 0 bytes on stdout is also exactly what clap does on a missing subcommand (row 7 of my
  table), so the fixer's diagnosis of his own failure was right.
- `decision` (code-reviewer): PASS over NEEDS_WORK. The three defects it names were re-checked; all three are now
  actually fixed on disk (section 5).

No entry pointed at an unresolved code problem.

## 5. Verification of the claimed fixes (not trusted, re-checked)

| FIX_SUMMARY claim | verdict | evidence |
|---|---|---|
| `IMPL_SUMMARY.md` now says `.gitignore` was created by this task | **true** | `IMPL_SUMMARY.md` line reads "`.gitignore` создан с правилами для …"; `git ls-files` shows `.gitignore` tracked, added in `d192073` |
| Line counts removed from `IMPL_SUMMARY.md` | **true** | no line counts remain in the file |
| `verify_bootstrap.ps1` forbidden-dependency regex is no longer a no-op | **true, with a caveat** | re-ran the exact regex `^(tokio\|reqwest\|hyper\|cap-std\|fs-err)\s` (multiline) against real `cargo tree -p transcript --prefix none` output: no match; against the same output plus a synthetic `tokio v1.0.0` line: match. Caveat: the denylist is five hardcoded names — a synthetic `mio v1.0.0` line does **not** match, so crates like `mio`, `socket2`, `tempfile`, `walkdir` would still slip through. Better than before, still not a general guard. |
| stdout-purity regression test added | **added, but weaker than claimed** | see section 3 |
| Skipped items (toolchain pin, unused deps, clap failure-path tests, hook event validation, transcript tests) | reasonable for a skeleton | the toolchain-pin skip is the only one I'd push back on, see bug 2 |

Two `scratch/` evidence files are stale and were not regenerated by the fixer: `final-state.txt` still shows the
pre-fix line counts (`Cargo.lock 374`, `main.rs 56`; actual 423 and 64) and a pre-fix `git status`, and
`cargo-build.txt` / `cargo-clippy.txt` / `cargo-test.txt` are UTF-16 with PowerShell `NativeCommandError` noise
baked in. Cosmetic, evidence-hygiene only.

## 6. Acceptance criteria

| # | Criterion | Test performed | Result |
|---|---|---|---|
| 1 | `cargo build`/`test`/`clippy -D warnings` pass on Windows | Full `cargo clean` then all three, plus `cargo fmt --check`; all exit 0, zero warnings, 2 tests passed | **PASS** |
| 2 | Exactly two package members, exactly one executable `cctg` | `cargo metadata --no-deps`: members = `cctg`, `transcript`; bin targets = exactly `[('cctg','cctg',['bin'])]`; `ls target/debug/*.exe` = only `cctg.exe` | **PASS** |
| 3 | `cctg --help` shows `hub`, `agent`, `hook`; no execution path but CLI output writes to stdout | Help lists all three verbatim; nine invocations measured byte-exact (table in §2), all three functional paths and all three clap error paths at exactly 0 stdout bytes, also under `RUST_LOG=trace` and with JSON piped to stdin | **PASS** (guard weakness noted, bug 1) |
| 4 | `crates/transcript` free of tokio / HTTP / filesystem crates | `cargo tree -p transcript --prefix none` and `cargo tree -p transcript -e all --prefix none`: only `serde`, `serde_core`, `serde_derive`, `serde_json`, `itoa`, `memchr`, `zmij`, `syn`, `quote`, `proc-macro2`, `unicode-ident`. No tokio/HTTP/fs crate, including build- and dev-deps. `crates/transcript/Cargo.toml` declares only serde + serde_json | **PASS** |
| 5 | `.gitignore` covers `target/`, `.env`, `*.log`, `.cctg/`, `registry.json`; build leaves no unignored files | All five lines present; `git check-ignore --no-index` returns ignored for `target/probe`, `.env`, `probe.log`, `.cctg/probe`, `registry.json`, `target/debug/cctg.exe`. After a full clean rebuild + test run, `git status --porcelain` = `?? .claude/` only, which predates this task (present in the pre-task snapshot) and is not a build artifact | **PASS** |
| 6 | Existing tests pass | `cargo test --workspace` from cold: 2 passed, 0 failed | **PASS** |

Additional project-law checks (not acceptance criteria, checked anyway):

- No disallowed crates: `Cargo.lock` (49 packages) contains no `teloxide`, `rmcp`, `reqwest`, `frankenstein`, `hyper`,
  `mio`, `socket2`. Matches the channel law ("do NOT add rmcp or any MCP crate") and the hub law (bare reqwest later,
  not teloxide).
- No secrets, tokens, user ids or private paths in any tracked file of this change.
- `git log` — commits `d192073`, `4b10a9a` and the task-doc commits carry no `Co-Authored-By` / "Generated with"
  trailers; subjects are English.
- `anyhow::Result<()>` at the binary edge, no `unwrap()` on external input in shipped code (`main.rs`); the only
  `unwrap()`s are in `#[cfg(test)]` and in the integration test, which is fine.

## 7. Bugs found

### Bug 1 — low — stdout-purity regression test is vacuous today

- **Where:** `crates/cctg/tests/stdout.rs`, interacting with `crates/cctg/src/main.rs:35-40`.
- **Repro:** copy `main.rs` to a scratch crate, delete only the `.with_writer(std::io::stderr)` line, copy
  `tests/stdout.rs` unchanged, `cargo test`. It passes. (Done; output in §3.)
- **Expected:** the test fails, because the stderr pin is the thing it exists to protect.
- **Actual:** it passes. `tracing` is declared but never invoked (zero `tracing::` call sites in the workspace), so
  no event reaches the writer; `RUST_LOG=trace` on all three subcommands produces 0 bytes on both streams. Adding a
  single `tracing::info!` to a writer-less build puts 64 bytes on stdout, so `tracing_subscriber::fmt()`'s default
  really is stdout and the exposure is real.
- **Impact:** none on this task's deliverable — the committed code is correct. The cost is a false sense of coverage
  carried into TASK-003+, where the channel agent's stdout is the JSON-RPC transport. Cheapest real fix (for a later
  task, not this one): have the test also run with `RUST_LOG=trace` and assert stdout empty *after* the first
  startup log line exists, or add a `tracing::debug!` at the top of each subcommand arm now.

### Bug 2 — low — no toolchain floor for `edition = "2024"` / `resolver = "3"`

- **Where:** `Cargo.toml:3,6`. No `rust-toolchain.toml`, no `rust-version` (confirmed: `cargo metadata` reports
  `"rust_version": null` for both packages).
- **Repro:** not reproducible on this machine (1.95.0). On a device with stable < 1.85, `cargo build` fails at
  manifest parse with a message that does not clearly name the edition.
- **Expected/Actual:** for a project whose whole premise is "несколько устройств", the second device should get a
  clear `rustc 1.85 required` diagnostic, not a manifest parse error.
- The reviewer raised this; the fixer skipped it with a defensible reason (the 1.85 floor is unproven for the whole
  lockfile). Adding `rust-version` to `[workspace.package]` is a documentation-grade change that does not need to be
  proven against an old toolchain — it only improves the error message. Not blocking; worth a follow-up ticket.

### Bug 3 — informational — `scratch/` evidence partly stale and partly unreadable

`final-state.txt` reports the pre-fix line counts and pre-fix `git status`; `cargo-build.txt`, `cargo-clippy.txt`,
`cargo-test.txt` are UTF-16 with PowerShell `NativeCommandError` wrapping (a consequence of `2>&1` on a native exe in
PS 5.1). Not a product defect; it is why I re-ran every command myself rather than reading the evidence files.

Nothing found at major or critical severity. Nothing in the shipped code needs to change for this task.

## 8. Verdict

**SHIP.**

All six acceptance criteria verified PASS by commands I ran myself from a cold `cargo clean`, including the criteria
the earlier agents got wrong in prose. The workspace is exactly two members and one executable, the CLI exposes the
three subcommands, no functional or error path emits a byte on stdout, `transcript` is dependency-pure under
`-e all`, `.gitignore` covers all five patterns and a full build leaves the tree clean.

The disconfirmation attempt found one genuine weakness — the new stdout test cannot currently fail for the reason it
was written — but that is a coverage gap in a guard, not a defect in the deliverable, and the property it guards is
independently confirmed correct by direct measurement. The two remaining findings are a missing toolchain floor and
stale scratch evidence. None of them blocks a skeleton whose entire job is to compile, expose three subcommands and
keep stdout clean.

Recommended follow-ups (do **not** hold this task for them): give the stdout test something to actually detect
(bug 1), and add `rust-version` to `[workspace.package]` (bug 2).
