# OPEN_DECISIONS — TASK-035

1. 2026-09-25, orchestrator: PREMISE SUSPECT resolved by amending TASK_FINAL.md: build identity = git commit (+dirty) embedded at build time, exe sha256 only as a fallback without git. Rejected: keep the exe hash and silence warnings for remote hubs (hides real version drift); download clients from Releases here (separate task, keeps this one bounded).
