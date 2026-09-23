# IMPL_REVIEW — TASK-020

## Verdict

**NEEDS_WORK**: the parser only recognises commands whose text starts with `<command-name>`. The second real format, `<command-message>…<command-name>…<command-args>` (user-typed skill commands such as `/maw-context --review` and `/maw-tasks "..."`), is still hidden in brief. That is 5 of the 43 real records the spec counts.

## Disconfirmation

Counter-example I went looking for: a real non-meta command record whose tags come in a different order, so `strip_prefix("<command-name>")` fails.

Result: **found, and it breaks the implementation.** A census of every `~/.claude/projects/**/*.jsonl` (script `scratch/census.py`, read-only):

| count | isMeta | first tag | tag order |
|---|---|---|---|
| 38 | false | `command-name` | name, message, args (built-in `/login`, `/model`, `/compact`...) |
| 5 | false | `command-message` | message, name, args (skills: `/maw-context --review`, `/maw-tasks <long text>`; versions 2.1.241 to 2.1.276) |
| 1 | true | none | agent-message that only mentions the tags (correctly stays hidden) |

38 + 5 = 43, which matches the spec's count. A harness (`scratch/harness`, built with CARGO_TARGET_DIR outside the repo) confirms it:

```
input: "<command-message>maw-context</command-message>\n<command-name>/maw-context</command-name>\n<command-args>--review</command-args>"
brief = ""                      (expected "> /maw-context --review")
full  = "> <command-message>maw-context</command-message>\n<command-name>…"   (raw, multi-line)
```

## Confirmed correct

- `crates/transcript/src/render.rs:176-178`: command detection only runs for `!turn.is_meta`, so isMeta records such as the skill body and channel messages keep their old handling. The only isMeta record in the census that contains the tags stays hidden.
- `render.rs:190-205`: for the name-first form, name and args are taken from their tags, whitespace (including newlines) collapses to single spaces, and empty args give `/compact` with no trailing space. `<`/`>` inside args survive (test and harness: `a<b and c>d`).
- `render.rs:97-108` together with `tool_after` at `:243`: a command is a `UserText::Prompt`, so it resets `finished` and `tool_after` exactly like a typed prompt. A command in the tail gets «в работе…» (test `slash_commands_are_one_line_prompts`).
- `<local-command-stdout>` and the other `SERVICE_PREFIXES` stay hidden in brief and shown in full (the test checks both).
- Fixture `crates/transcript/tests/fixtures/slash_command.jsonl` copies the real shape of the name-first record, including the 12-space indentation. It has no private paths, ids or tokens (`C:\work\demo`, zero UUIDs). It is wired into both `ALL` lists (the thinking-leak matrix and the privacy checks).
- `cargo test --workspace` is green, and so are `cargo clippy --workspace --all-targets -D warnings` and `cargo fmt --check`. No dependency changes (Cargo.toml/Cargo.lock diff is empty).

## Issues

### 1. major: `render.rs:191`, the message-first command form is not recognised
`slash_command` requires the text to start with `<command-name>`. Real skill commands start with `<command-message>`, so they fall through to `SERVICE_PREFIXES`. They stay hidden in brief, show raw in full, and do not count as a prompt boundary. Skill commands are the most meaningful user commands in this project (`/maw-tasks`, `/maw-execute-task`, `/maw-context`), and acceptance criterion 1 as the spec intends it (all 43 records) is not met.
Fix: accept either leading tag and find `<command-name>…</command-name>` and `<command-args>…</command-args>` anywhere in the text. For example, gate on `text.starts_with("<command-name>") || text.starts_with("<command-message>")` and then use `find`/`split_once` for each tag on the whole text. Add a second fixture record with the message-first order (anonymized, e.g. `/demo-skill --flag`) and assert `> /demo-skill --flag` in brief and full.

### 2. minor: `render.rs:193`, a missing `<command-args>` tag hides the command
When `<command-args>` is absent, `slash_command` returns `None` and the record goes back to being a hidden service record, so the command name is lost. None of the 43 real records lacks the tag today, but the spec says the name must not be lost, and treating a missing tag as empty args costs nothing.
Fix: `let args = extract(text, "command-args").unwrap_or("")`.

### 3. minor: `render.rs:194`, a literal `</command-args>` inside args cuts them
`split_once` stops at the first closing tag (harness: `a </command-args> b` gives `/x a`). The name is kept, so this is a small edge. `rsplit_once` on the closing tag would keep everything up to the last one.

## Missing coverage

- A message-first record (`<command-message>` first), in brief and full. This is the real skill form and is currently untested and broken.
- A command record without `<command-args>`.
- A command followed by a tool call and a final answer, to check `tool_after` resets at the command. It is covered only indirectly through a typed prompt.
- A non-meta typed prompt that merely mentions `<command-name>` later in its text should stay a normal multi-line prompt. It works now because of `strip_prefix`, but no test pins it.

## Nits

- Command args are not truncated. A `/maw-tasks` with several paragraphs of args becomes one very long line. This matches how typed prompts are handled (they are not truncated either), so it is not a defect, just worth knowing.
- Following the spec, a local command at the tail (`/model`, `/login`), where no assistant reply ever comes, shows «в работе…». That is what the spec asks for, but it will look like a false "still working" in the topic. If it bothers anyone, it belongs in a follow-up, not in this fix.
