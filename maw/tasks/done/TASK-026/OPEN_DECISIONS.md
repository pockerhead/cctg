# Open decisions — TASK-026

- 2026-09-24: no separate QA stage: the switch to the supervisor and the first deploy are done live by the orchestrator right after merge and are the acceptance check; results recorded below.
- 2026-09-24 live: old hub stopped (-Force, old build had no graceful stop), new binary swapped in by rename, `cctg supervise --env-file <repo>/.env` started hidden from the repo root with logs in ~/.cctg/supervise.log; hub up, both live agents re-registered within 1 s; `cctg deploy` of the same bytes answered `unchanged`, exit 0.
