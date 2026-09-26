# PCTX proposals (TASK-052)

## 2026-09-26, implementer: tooling
- CI runs `shellcheck --shell=sh install.sh`, but there is no shellcheck on this Windows host and Docker Desktop is usually not running. `pip install --target <session scratchpad>/sc shellcheck-py` gives `sc/bin/shellcheck.exe` (0.11.0) without touching the user's Python environment; run it with `--shell=sh`, `dash` and `bash`. Worth a line in the implementer tooling section.
- The Rust sources are CRLF in the working copy (`core.autocrlf=true`), `install.sh` stays LF (`.gitattributes`); scripted edits of `.rs` files must match `\r\n` or go through the Edit tool.

## 2026-09-26, fixer (CI run 1): lessons
- hub tests: `serve_agents` answers `registered` right after handing `AgentEvent::Registered` to the actor's channel, before the actor binds the agent; a `Control` message sent right after `Agent::connect` can reach the actor first (independent channels, random `select!`) and get the offline answer. E2E tests that act on the agent right after connecting wait for a hub-visible sign of binding (the topic's alive icon, `status_e2e.rs` `Hub::agent_bound`). Proven with a 300 ms delay probe in ingress (`scratch/fixer/register_race_probe.diff.txt`).
- Shared target hazard: a %TEMP% reference workspace (TASK-038 planner's `cctg-task038-ref`) built into the shared target leaves test binaries whose `env!("CARGO_MANIFEST_DIR")` points at that copy; fingerprints of workspace members are path-relative, so cargo reuses them for the main tree and `transcript/tests/purity.rs` fails with NotFound once the copy is gone. Remedy: touch the test source (mtime) to force a rebuild; better, reference workspaces should not share the target, or should be kept until the main tree rebuilt.
