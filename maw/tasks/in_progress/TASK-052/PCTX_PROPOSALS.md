# PCTX proposals (TASK-052)

## 2026-09-26, implementer: tooling
- CI runs `shellcheck --shell=sh install.sh`, but there is no shellcheck on this Windows host and Docker Desktop is usually not running. `pip install --target <session scratchpad>/sc shellcheck-py` gives `sc/bin/shellcheck.exe` (0.11.0) without touching the user's Python environment; run it with `--shell=sh`, `dash` and `bash`. Worth a line in the implementer tooling section.
- The Rust sources are CRLF in the working copy (`core.autocrlf=true`), `install.sh` stays LF (`.gitattributes`); scripted edits of `.rs` files must match `\r\n` or go through the Edit tool.
