# PCTX proposals from TASK-043 (implementer)

## 2026-09-24: hub domain, TASK-021 line is outdated
The hub domain says "every other allowlisted text in a topic, including `/compact` or `/path`, is routed via `Control::Message` ... to the agent". Since TASK-043 the slots actor turns a non-forwarded text that starts with `!` + text, or with `/name` (name `[A-Za-z0-9_:.-]+`, optional `@bot`, then a space or the end), into a console command (`hub/console.rs`). It goes to the live agent that announced `Register.console_commands` as `console_command`, answered by `console_command_typed`. Such a text is never parked and never reaches the model. It is refused with a reply while `busy` or `waiting`, without a capable live agent, or when it is not `keys::typable`. `/tmp/x` stays a message. Proposed: replace the `/compact` example in the TASK-021 line and add a TASK-043 line.

## 2026-09-24: transcript domain, bash and local-command records now stream
The transcript domain says brief hides `<bash-input>`, `<bash-stdout>` and `<local-command-stdout>` as service records. Brief still does. But `stream_events` now turns `<bash-input>` into `Prompt("! cmd")`. Bash stdout+stderr and `<local-command-stdout>` become a `Note` carrying a fenced code block, ANSI stripped, cut to 20 lines / 1500 graphemes. Shape (surveyed 2026-09-24 on 2.1.28x): a bash-input record is `<bash-input>CMD</bash-input>`; the next record is `<bash-stdout>OUT</bash-stdout><bash-stderr>ERR</bash-stderr>` in one string. Both are non-meta `user` string records.

> RESOLVED: deferred to the domain condense (2026-09-24).
