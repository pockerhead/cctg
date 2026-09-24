# PREMISE_CHALLENGE — TASK-034

## 1. Counter-example tested

Written before any investigation:

A hub feature reads a session file for a session that has NO agent link able to serve it. The concrete case: a nested `claude -p` run (or any `CLAUDE_CODE_ENTRYPOINT=sdk-cli` run), whose agent starts without a hub link. If the hub today reads the transcript of such a run (for example for the `⇣ nested <id>` block, for ai-title, or for `/brief` on a slot whose current session has no agent), then "move every read to the agent over the existing link, with the path gate limited to the agent's own session" cannot cover it. The acceptance criterion "blocks of nested runs work when the hub cannot see the path" could then be met only by degrading them, or the path gate would have to serve files of another session. That would make the premise incomplete.

(sections 2-4 follow after investigation)
