# Writes crates/transcript/tests/fixtures/console_commands.jsonl: the record
# shapes Claude Code writes for a `!` command typed in the terminal
# (`<bash-input>`, then `<bash-stdout>...</bash-stdout><bash-stderr>...</bash-stderr>`)
# and for a local slash command (`<command-name>` then `<local-command-stdout>`),
# as surveyed in real transcripts (TASK-043, shapes only). Content is made up.
import json

BASE = {
    "cwd": "C:\\work\\demo",
    "entrypoint": "cli",
    "gitBranch": "main",
    "isSidechain": False,
    "sessionId": "00000000-0000-4000-8000-000000000001",
    "userType": "external",
    "version": "2.1.281",
    "type": "user",
}

texts = [
    "<bash-input>echo hi</bash-input>",
    "<bash-stdout>hi</bash-stdout><bash-stderr></bash-stderr>",
    "<bash-input>git status</bash-input>",
    "<bash-stdout></bash-stdout><bash-stderr>fatal: not a git repository\n</bash-stderr>",
    "<bash-input>true</bash-input>",
    "<bash-stdout></bash-stdout><bash-stderr></bash-stderr>",
    "<command-name>/cost</command-name>\n            <command-message>cost</command-message>\n            <command-args></command-args>",
    "<local-command-stdout>\u001b[1mTotal cost:\u001b[22m $0.12\nTotal duration: 3m</local-command-stdout>",
    "<bash-input>cat notes.md</bash-input>",
    "<bash-stdout>" + "\n".join(["```rust"] + [f"line {n}" for n in range(1, 30)]) + "</bash-stdout><bash-stderr></bash-stderr>",
]

lines = []
parent = None
for n, text in enumerate(texts):
    uuid = f"00000000-0000-4000-8000-{n + 300:012d}"
    record = dict(BASE)
    record.update(
        {
            "uuid": uuid,
            "parentUuid": parent,
            "promptId": f"00000000-0000-4000-8000-{n + 400:012d}",
            "timestamp": f"2026-01-01T00:00:{n:02d}.000Z",
            "message": {"role": "user", "content": text},
        }
    )
    parent = uuid
    lines.append(json.dumps(record, ensure_ascii=False))

with open("crates/transcript/tests/fixtures/console_commands.jsonl", "w", encoding="utf-8", newline="\n") as out:
    out.write("\n".join(lines) + "\n")
