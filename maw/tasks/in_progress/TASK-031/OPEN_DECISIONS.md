
Orchestrator answers 2026-09-25 (after the plan):
- Split accepted: `/join` in General and `--hub --local` go to a follow-up task (TASK-046); this task keeps `install.sh --hub` with Docker and the printed client line.
- README language: Russian (the repository's working language; docs/remote-hub.md is Russian too), short.
- First tag: `v0.1.0` right after TASK-035 and TASK-031 are merged and CI is green; the printed client line and README pin the installer URL to the tag (`https://raw.githubusercontent.com/pockerhead/cctg/<tag>/install.sh`), not `main`.
- GHCR package visibility: public (TASK-035 decision); the orchestrator switches it after the first image push.
- Do not run the installer on the orchestrator's machine before the hub moves to the server (noted).

- 2026-09-25 orchestrator: no install.ps1 (user decision: one install.sh, Windows under Git Bash); the acceptance line naming install.ps1 is read as install.sh on Windows.
- 2026-09-25 orchestrator: no --strict-mcp-config in the wrapper: it would drop the user's other MCP servers; a stale user-scope cctg registration is documented in the README troubleshooting instead.
