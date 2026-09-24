# PREMISE_CHALLENGE — TASK-040

## 1. Counter-example tested

Step 4 of the premise assumes the worker agent (the stdio MCP server that claude spawns) can "type `/exit` into the console of its claude" by reusing the TASK-029 WriteConsoleInputW mechanism, and that `cctg run` can sit between the shell and claude without breaking anything the rest of the system relies on. Concrete case that would break it: the TASK-029 mechanism does not exist in the agent (or cannot reach claude's console from the agent process, e.g. it lives elsewhere or needs a console the MCP child does not have), OR inserting `cctg run` as claude's parent breaks the process-lineage walk that the hooks use for nesting detection (a `cctg.exe` between shell and claude being mistaken for or hiding a parent), so the success predicate "restart in the same window" could be met while the session is mis-slotted.

(Investigation follows below.)

## 2. Primary-source investigation

- `crates/cctg/src/keys.rs:30-94`: `press(claude_pid, key)` runs inside the agent process: `SetConsoleCtrlHandler(None,1)`, `FreeConsole`, `AttachConsole(claude_pid)` (line 72), `CreateFileW("CONIN$")`, `WriteConsoleInputW` (line 86), `FreeConsole`. The module doc (lines 4-10) states the agent is a child of claude and its stdio pipes stay untouched. It does not need the MCP child to own a console: it attaches to claude's by pid.
- `crates/cctg/src/agent.rs:373` builds the presser `keys::press(pid, key)` from the agent's known claude pid; `agent.rs:598` calls it on a blocking task. So the mechanism lives in the worker agent, the process the premise names.
- `crates/cctg/src/keys.rs:21-26`: only `ConsoleKey::Interrupt` (Esc) exists today; typing `/exit` + Enter is a new key sequence, not a new mechanism.
- `crates/cctg/src/proctree.rs:62-91`: the lineage chain walks upward from the hook through any image name, stopping only on a missing/unverifiable/non-older parent; `proctree.rs:207-219` picks the own process and the parent only among `claude(.exe)` CLI images. A `cctg.exe` placed ABOVE a top-level claude (as `cctg run` would be) is never considered a claude and is above the own process, so it cannot create a false parent. Inside a nested case (`cctg run` under a parent claude's blocking Bash tool) it is an extra live intermediate, which the walk passes through like a shell. The existing test `proctree.rs:677-678` already has `cctg.exe` in a chain.
- `crates/cctg/src/wire.rs:34` `VERSION = 1`; `wire.rs:116-140` `Register` has capability booleans (`verdict_ack`, `transcript_reads`, `console_keys`) and no client version or binary hash field: the gap step 2 names really exists, and the capability pattern the premise relies on is real.
- `crates/cctg/src/main.rs:104-110,203,212`: `supervise` and `deploy` subcommands exist; there is no `run` subcommand yet (the gap step 4 names is real).
- Normative context (hooks domain, TASK-003 risk lesson) already records that `.claude/settings.json` hook edits are picked up by running sessions. That makes the step-1 probe partly pre-answered (for hooks in settings.json; not for `--settings <file>` or statusLine), which narrows the probe but does not falsify the premise.

No live probe was run (forbidden here). Not verified by execution: whether a typed `/exit` via WriteConsoleInputW actually exits Claude Code's TUI and whether the channels dialog on `--resume` can be answered from `cctg run`'s own console; both are live-probe questions for implementation, not premise-breaking evidence.

## 3. Did it hold

The counter-example did not hold. The console-write mechanism exists in the worker agent and attaches to claude's console by pid, so the agent can type into its own claude's console. `cctg run` as claude's parent does not disturb nesting detection, because the lineage logic only treats `claude(.exe)` CLI images as own/parent processes. The gaps the premise claims (no version in `Register`, no `cctg run`, capability-gated wire) are real at the cited lines.

## 4. Verdict

PREMISE HOLDS — `crates/cctg/src/keys.rs:69-92` (agent does AttachConsole(claude_pid) + WriteConsoleInputW from its own process), `crates/cctg/src/agent.rs:373`, `crates/cctg/src/proctree.rs:62-91,207-219` (a non-claude `cctg.exe` above claude cannot become a parent), `crates/cctg/src/wire.rs:34,116-140` (VERSION=1, capability flags, no version field yet).
