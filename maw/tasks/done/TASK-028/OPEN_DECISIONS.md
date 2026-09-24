# Open decisions — TASK-028

- 2026-09-24: review finding 1 (does a waiting hook block the terminal dialog?) is settled by a live check right after deploy: the hook is enabled in ~/.cctg/poc/settings.json and the user triggers an ordinary prompt and a safety-check prompt in a restarted session. If the hook blocks the dialog, the twin wait is revisited.
- 2026-09-24: minors 4 (id collision ~1e-7) and 5 (pairing by session+tool) accepted.
