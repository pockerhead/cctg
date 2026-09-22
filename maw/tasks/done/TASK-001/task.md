# TASK-001: Decompose cctg MVP into pipeline tasks

Type: chore
Mode: deep-research
Priority: high
Branch: chore/decompose-mvp
Domains: transcript, channel, hub, hooks

## Description
Produce a research report that breaks the cctg MVP (steps 1-4 of the development order in the repo root `CLAUDE.md`: `transcript` crate, `hub`, `agent` + `hook`, multi-session routing with subagents and nested runs) into concrete tasks for the MAW pipeline. Each proposed task must be small enough for one `full` or `small-fix` run, name its mode, its dependencies on other proposed tasks, and testable acceptance criteria. The report must treat the "Открытые вопросы" section of `CLAUDE.md` as first-class: for every open question, either answer it from primary sources (official Claude Code docs, channels reference, fakechat example, hdcd-telegram source, teloxide docs) with a link, or turn it into an explicit spike task with a described experiment. Verified facts in `CLAUDE.md` are not to be re-researched; contradicting them requires a cited source.

## Acceptance criteria
- [ ] Report contains an ordered list of proposed tasks covering steps 1-4 of the plan, each with: title, suggested mode (`full`/`small-fix`/`brainstorm`), one-paragraph scope, dependencies (`blocked by` / `prefer after`), and 3-6 testable acceptance criteria
- [ ] Every open question from `CLAUDE.md` "Открытые вопросы" is either answered with a cited primary source or mapped to a spike task with a concrete experiment and expected observable outcome
- [ ] Cargo workspace layout (crates, binary, subcommands) and the crate set are confirmed or amended with reasons; `teloxide` vs bare `reqwest` gets a recommendation backed by current crate state (version, maintenance, forum-topic API coverage)
- [ ] Telegram Bot API constraints relevant to the design are verified with links: forum topic creation limits, message length, callback data size, edit rate limits
- [ ] Report names which proposed tasks can run in parallel and which form the critical path
- [ ] Report is written to PLAN_FINAL.md in a form that `/maw-tasks` batch mode can consume directly (one block per task)
- [ ] Existing tests pass (not applicable until a Cargo workspace exists in the repo; while there is no `Cargo.toml`, this criterion is satisfied vacuously and `cargo test` is not required to run)
