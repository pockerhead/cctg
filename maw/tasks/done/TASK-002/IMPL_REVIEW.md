# IMPL_REVIEW — TASK-002 (bootstrap cargo workspace and cctg CLI skeleton)

Reviewer stage: code-reviewer (claude/opus, effort=medium)
Reviewed commit: `d192073` (code) + `bf65a76` (summary), branch `chore/bootstrap-workspace`
Mode: small-fix — verified against `task.md` directly (no plan file).

## 1. Verdict

**PASS** — every acceptance criterion in `task.md` verified green by re-running the commands myself; the only defects are in the implementer's own summary and in the scratch probe, not in the shipped code.

## 2. Disconfirmation performed (mandatory step)

Counter-example chosen before evaluating anything: **some execution path writes to stdout**, which would break the channel domain law ("stdout is reserved for JSON-RPC only; one stray print breaks the transport") and criterion 3 of `task.md`. Concrete candidates hunted: a `println!` stub in a subcommand arm, `tracing_subscriber::fmt()` defaulting to stdout, and clap emitting usage text on stdout on the error paths.

Searched and tested:
- `crates/cctg/src/main.rs:38` pins the subscriber writer to `std::io::stderr` explicitly. No `println!`/`print!` anywhere in the workspace.
- Ran all six paths with stdout and stderr redirected to separate files and measured byte lengths:

| invocation | exit | stdout bytes | stderr bytes |
|---|---|---|---|
| `cctg --help` | 0 | 316 | 0 |
| `cctg hub` | 0 | **0** | 0 |
| `cctg agent` | 0 | **0** | 0 |
| `cctg hook SessionStart` | 0 | **0** | 0 |
| `cctg` (no args) | 2 | **0** | 316 |
| `cctg bogus` | 2 | **0** | 103 |
| `cctg hook` (missing event) | 2 | **0** | 136 |

**The counter-example did not hold.** Only the CLI's own `--help`/`--version` output reaches stdout; every functional path, including all three clap error paths, keeps stdout at exactly zero bytes. Criterion 3 is satisfied under a stricter test than the implementer ran (he never probed the error paths).

Log triage: `log.jsonl` is 0 bytes — no `dead_end` or `decision` entries exist to triage, so there were no author-struggle pointers to verify.

## 3. Confirmed correct

All re-run on this machine (rustc 1.95.0, cargo 1.95.0, Windows 11):

- **`cargo build --workspace`** — succeeds.
- **`cargo clippy --workspace --all-targets -- -D warnings`** — exit 0, zero warnings.
- **`cargo test --workspace`** — exit 0; `cctg` 1 passed, `transcript` 0 tests, doc-tests 0.
- **Exactly two members, exactly one executable** — `cargo metadata --no-deps` returns `cctg` and `transcript`; `target/debug/*.exe` contains only `cctg.exe`. Criterion 2 satisfied.
- **`cctg --help` lists `hub`, `agent`, `hook`** — confirmed verbatim in the help output. Criterion 3 satisfied.
- **`crates/transcript` is pure** (`Cargo.toml`) — `cargo tree -p transcript` shows only `serde` → `serde_core`/`serde_derive` and `serde_json` → `itoa`/`memchr`/`serde_core`/`zmij`. No tokio, no HTTP client, no filesystem crate anywhere in the tree, including transitively. Criterion 4 satisfied. Matches the transcript domain law "the crate has no IO/network dependencies".
- **No disallowed crates** — `Cargo.lock` (49 packages) contains no `teloxide`, `rmcp`, `reqwest`, `frankenstein`, `hyper`, `mio` or `socket2`. The dependency set is exactly the seven the task names. Channel domain law ("do NOT add `rmcp` or any MCP crate") is respected.
- **`.gitignore`** covers `target/`, `.env`, `*.log`, `.cctg/`, `registry.json` — all five present. Criterion 5 satisfied.
- **Build leaves no unignored files** — `git status --porcelain` after a full build and test run shows only `?? .claude/`, which predates this task (it is present in the pre-task git snapshot) and is not a build artifact.
- **Commit hygiene** — `d192073` and `bf65a76` carry no `Co-Authored-By` or "Generated with" trailers, per project law. Commit subjects are in English.
- **Code quality** — `main.rs:26` returns `anyhow::Result<()>` at the binary edge as the invariant requires; no `unwrap()` on external input anywhere; `transcript/src/lib.rs` is a single doc comment with zero speculative business logic, correctly resisting the temptation to pre-build the parser.
- **No secrets** — no token, user id or private path appears in any tracked file of this change.

## 4. Issues

### major — `IMPL_SUMMARY.md:16` states a verifiable falsehood about `.gitignore`

The summary says: "Существующий `.gitignore` полностью проверен: он уже содержал `target/`, `.env`, `*.log`, `.cctg/`, `registry.json`, поэтому изменение файла не потребовалось."

This is false. `git show d192073 -- .gitignore` shows `new file mode 100644` with all five lines added, and `git show d192073~1:.gitignore` fails with `fatal: path '.gitignore' exists on disk, but not in 'd192073~1'`. The file did not exist before this commit; the implementer created it.

The *code* is correct — the criterion is met either way. The defect is that a downstream QA agent reading the summary would skip verifying `.gitignore` on the (false) premise that it was pre-existing and unreviewed-by-this-task.

Suggested fix (documentation only, no code change): correct line 16 to state that `.gitignore` was created in this commit with those five entries.

### minor — `IMPL_SUMMARY.md:10,12` line counts do not match the committed files

Summary claims `Cargo.lock` is 374 lines and `main.rs` is 56 lines. Actual: 423 and 64 respectively (`git show --stat d192073` agrees: 423 and 64 insertions). Combined with the `.gitignore` claim, this indicates the summary was written against a remembered or earlier state rather than read back from disk.

Suggested fix: regenerate the counts from the committed files, or drop the line counts — they carry no review value and only create falsifiable noise.

### minor — `scratch/verify_bootstrap.ps1:59` — the forbidden-dependency check is a no-op

```powershell
if ($tree -match "(?m)^(tokio|reqwest|hyper|cap-std|fs-err)\s") {
    throw "transcript contains a forbidden dependency"
}
```

`cargo tree` prints every dependency except the root prefixed with box-drawing indentation (`├── `, `│   └── `). The anchor `^` therefore only ever tests the single root line, which is always `transcript v0.1.0 (...)`. A transitively-pulled `tokio` would pass this check silently.

This is the *only* automated guard behind acceptance criterion 4, so the criterion is currently self-certified by a check that cannot fail. (The criterion itself does hold — I verified the full tree by eye above — but the guard would not catch a regression.)

Suggested fix: drop the `^` anchor and match the crate name after optional tree glyphs, e.g. `"(?m)^[\s│├└─]*(tokio|reqwest|hyper|cap-std|fs-err) v"`.

### minor — no toolchain pin despite `edition = "2024"` / `resolver = "3"`

`Cargo.toml:3,6` select edition 2024 and resolver 3, which need Rust ≥ 1.85. There is no `rust-toolchain.toml` (verified absent). cctg is explicitly a multi-device project ("несколько устройств" in `CLAUDE.md`); a second device on an older stable fails at manifest-parse time with an error that does not obviously point at the edition.

Neither the task nor the project context mandates edition 2024, so this is an unrequested choice the implementer did not flag in the summary.

Suggested fix: either add a `rust-toolchain.toml` with `channel = "1.85"` (or the team's floor), or add `rust-version = "1.85"` to `[workspace.package]` so cargo emits a clear diagnostic instead of a parse failure. Not blocking on this machine.

### minor — declared-but-unused dependencies

- `crates/cctg/Cargo.toml:9` declares `tracing`, but `main.rs` never references `tracing::` (grepped: zero hits). Only `tracing_subscriber` is used.
- `crates/transcript/Cargo.toml:7-8` declares `serde` and `serde_json`, but `lib.rs` is a single doc comment using neither.

The task does say to wire up these dependencies, so this is defensible as scaffolding for the next task, and clippy cannot see it. But it does sit against the "no just-in-case layers" invariant, and unused deps in a leaf crate are a real (if small) build-time cost. Flagging rather than demanding a change, since the task text arguably requests it.

## 5. Missing coverage

- **No automated regression test for stdout purity.** This is the most valuable missing test. The channel domain law makes a stray stdout write a transport-breaking bug, and the only guard today is a manual PowerShell probe in `scratch/` that a future change will not re-run. An integration test under `crates/cctg/tests/` that runs the built binary (`env!("CARGO_BIN_EXE_cctg")`) for `hub`, `agent` and `hook SessionStart` and asserts `output.stdout.is_empty()` would lock criterion 3 into CI permanently. Cheap to write, high value given what it protects.
- **No test for the CLI failure paths.** `parses_all_subcommands` (`main.rs:48`) only covers the three happy parses. Nothing asserts that a missing subcommand, an unknown subcommand, or `hook` without its event argument fails rather than silently defaulting. I verified all three exit 2 by hand; a `try_parse_from(...).is_err()` assertion per case would make that permanent and costs three lines.
- **No test that the `hook` event argument round-trips a realistic value.** The one test uses `SessionStart`; the hooks domain names five events (`SessionStart`, `SessionEnd`, `Stop`, `SubagentStart`, `SubagentStop`). Since `event` is a bare `String` with no validation, an unknown event name is currently accepted silently. That is acceptable for a skeleton, but it should be a conscious deferral rather than an unremarked gap — worth a `// TODO` or a follow-up task, not a fix here.
- `crates/transcript` has zero tests, correctly — there is nothing to test yet. The transcript domain law "tests are mandatory" binds the crate that implements `parse`, not this empty skeleton. Not an issue.

## 6. Nits

- `main.rs:28-30` — the `match cli.command { Command::Hub | Command::Agent | Command::Hook { .. } => {} }` is a no-op whose only effect is to consume `cli.command`. The or-pattern plus `{ .. }` gives no exhaustiveness benefit, so it reads as ceremony. `let _cli = Cli::parse();` would say the same thing more honestly until the arms get bodies.
- `main.rs:36` — `let _ = ...try_init()` silently swallows an init failure. Defensible (it makes the function safe to call from tests), but once logging matters, a silent failure to install the subscriber is an unpleasant way to lose all diagnostics. Worth a short comment saying why the error is discarded.
- `init_tracing()` runs at `main.rs:27`, before `Cli::parse()` at `main.rs:29`. Harmless today because the writer is stderr, but it means the subscriber is installed even for `--help` and for argument errors. Parsing first would be the less surprising order.
