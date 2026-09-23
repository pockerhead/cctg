# PCTX proposals — TASK-008

## 2026-09-23 (planner): hub risk lesson, reqwest error text carries the token

`reqwest::Error` Display/Debug include the request URL, and the Bot API URL contains the bot token (`/bot<token>/method`). Verified by mutation: removing `.without_url()` in `hub/api.rs` makes `transport_errors_never_contain_the_token` fail (`scratch/planner/mutations.out.txt`). Proposed line for `domains/hub.md` Risk lessons: "Every reqwest error leaves the BotApi module through `without_url()`; responses are decoded from bytes by hand, never via `Response::json` or `error_for_status`, which keep the URL."

## 2026-09-23 (planner): hub config variable for the allowlist

The hub reads `CCTG_ALLOWED_USER_IDS` (comma-separated numeric ids) and refuses to start when it is empty. The repo `.env` currently has only `CCTG_BOT_TOKEN` and `CCTG_CHAT_ID`. Proposed addition to `domains/hub.md` Invariants: "Config: `CCTG_BOT_TOKEN`, `CCTG_CHAT_ID` (`-100…`), `CCTG_ALLOWED_USER_IDS`; `./.env` or `--env-file`, process env wins."

## 2026-09-23 (orchestrator, from plan-reviewer-2) — tests capturing `tracing` output

A test that installs a tracing subscriber to assert on log content must live in its own integration test binary (one test per binary): parallel tests in the same binary race on tracing callsite registration and the assertion flakes (56/100 failures observed in TASK-008 before isolation, 0/200 after).

## 2026-09-23 (qa) — negative log assertions must not match timestamps

A test that asserts an id is absent from formatted tracing output must disable timestamps (`.without_time()`) or use ids that cannot occur in an RFC 3339 microsecond timestamp. TASK-008 `tests/routing_logs.rs` uses ids 1001/2002/3003 and fails ~0.5% of runs (9 of ~1640) when the timestamp fraction contains one of them (e.g. `.331001Z`). Proposed line for the hub Risk lessons next to the tracing-callsite lesson.

> RESOLVED: all four folded into domains/hub.md on 2026-09-23.
