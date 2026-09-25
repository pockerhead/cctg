# PREMISE_CHALLENGE — TASK-034

## 1. Counter-example tested

A file read in `crates/cctg/src/hub/` that concerns a session's files and that the agent link cannot serve, because no agent link exists for that session. Example: a nested `claude -p` run or a subagent of such a run. Its agent does not connect to the hub when `CLAUDE_CODE_ENTRYPOINT=sdk-cli`. If the hub reads that session's transcript today (for example for a `⇣ nested` block or a nested/headless session's content), then "move every read to the agent over the existing link" cannot keep the feature working. In that case acceptance criterion 1 ("blocks of subagents and nested runs work") is not met by the premise's mechanism alone. The same applies if the inventory in the premise misses a read of this kind.

## 2. Primary-source investigation

1. The inventory of file reads in `hub/`. Grep `std::fs|tokio::fs|spawn_blocking|File::open|read_to_string|fs::metadata|OpenOptions` over `crates/cctg/src/hub`. The non-test reads of session files are:
   - `commands.rs:172,182,202` and `sessions.rs:69,81` (`/brief` and `/full` locate and read the file),
   - `slots.rs:545` + `slots.rs:1570` (`read_title`, ai-title),
   - `subagents.rs:92` + `slots.rs:1403` (`scan` of the parent transcript),
   - `subagents.rs:424` + `slots.rs:1520` (`read_body`, the subagent jsonl and `.meta.json`).
   
   The other reads are the hub's own state: `offset.rs`, `registry.rs:1725`, `config.rs:126` (.env), `mod.rs:214` (`own_build`). The inventory in the premise matches (brief/full, ai-title, subagent scan and body). I found no hidden extra read.
2. Nested blocks. Per the domain contract, the `⇣ nested` block body is the Stop answer from the hook, not a file. `slots.rs:1538-1541` uses `BlockKey::Nested` only for rendering. The nested part of the counter-example did not break the premise.
3. Sessions that have no agent link at all:
   - `agent.rs:526-527`: `if entrypoint == Some("sdk-cli") { return Err(NoHub::Headless); }`. Every headless `claude -p` agent never connects to the hub.
   - Grep `(?i)headless|attended|entrypoint` over `hook.rs`, `wire.rs`, `hub/slots.rs`, `hub/registry.rs`: no match. The hook does not tell the hub that a run is headless.
   - `registry.rs:809-821`: with `parent_pid` None, the kind is `self.nesting(..)` → `SessionKind::TopLevel` → `self.allocate(...)` gets a slot and a topic.
   
   So a top-level `claude -p`, or a nested run whose parent chain broke (the documented Git Bash exec hole), is a live top-level session with a topic and no agent link, ever.
4. Today these sessions get file-backed features. `slots.rs:1274-1275` calls `read_title` from the hook followup with no agent condition. `slots.rs:1380-1403` (`check_candidates`) scans `entry.transcript_path` for any session with a candidate, again with no agent condition. `/brief`/`/full` read the file directly (`commands.rs:172`).
5. For contrast, the path gate of the existing agent side, `tail.rs:166-175` (`open_transcript(root, session_id, path)`), takes the session id from the request, not from env. The `/clear` case (the agent keeps the old env id) does not break the premise.

## 3. Did it hold

Partly. The inventory of reads is complete, and the nested-block case needs no file. But the premise's mechanism ("the agent reads on its side over the existing agent link") and its only degradation clauses ("a dead session without an agent", "an old agent without the capability") leave out a real class. That class is a live top-level session that has a topic but by design never has an agent link: headless `claude -p` (`agent.rs:526`), and a nested run that looks top-level because of the process-tree hole (`registry.rs:809-821`). Today the hub serves ai-title, subagent blocks and `/brief` for these sessions from the file (`slots.rs:1274,1403`, `commands.rs:172`). After the change as framed, they can only regress. Acceptance criterion 1 has no exception for them, and the task does not define their degradation.

## 4. Verdict

PREMISE SUSPECT — `agent.rs:526-527` (a headless agent never links) together with `registry.rs:809-821` (a SessionStart with no detected parent always gets a TopLevel slot/topic; the hook sends no headless marker) and `slots.rs:1274-1275,1380-1403` (ai-title and the subagent scan read `transcript_path` for such sessions without any agent) show live topic-owning sessions that the existing agent link cannot serve. The premise covers only dead sessions and old agents ; smallest implied reframing: the task must also decide what happens to live top-level sessions that never have an agent link (headless `claude -p` and nested runs whose parent was not detected): either give them a link just for serving files, or accept and document the regression. Criterion 1 must then say which of the two applies.
