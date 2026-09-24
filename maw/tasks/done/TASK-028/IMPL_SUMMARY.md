# TASK-028 implementer summary (small-fix)

## 1. What was implemented

Design as built:
- `cctg hook PermissionRequest` (hook.rs): reads stdin, builds a `PermissionPost` (tool name, `tool_input.description` as description, the rest of `tool_input` as compact JSON preview, both capped at 4 KiB), POSTs it to `/v1/permission` with the same Bearer secret (connect+send 2 s, answer wait 97 s), prints `{"hookSpecificOutput":{"hookEventName":"PermissionRequest","decision":{"behavior":"allow"}}}` (deny adds `"message":"Denied by the user in Telegram"`) only on a 200 answer. 204, 404, any error, timeout, hub down: nothing on stdout, exit 0. Never spooled (branches before `build`/spool).
- Ingress (ingress.rs): `serve_hooks` unchanged for callers (404 on the new path); new `serve_hooks_and_permissions(listener, secret, events, asks)`. A permission request drops its general hook permit, takes one of `MAX_PERMISSION_WAITS = 16` own permits (none left: 204 at once), `try_send`s a `PermissionAsk { post, answer: oneshot }` to the actor and waits for: the answer (200 + `{"behavior":..}` or 204), the client going away (read EOF: dropped, the actor notices `is_closed`), or a 95 s cap (204). Other hooks are never blocked.
- Slot actor (slots.rs): `Slots::permission_asks()` creates the channel (called in `hub::run`). An ask of a session that is not live top-level gets no decision at once. Twin matching: channel `permission_request` and hook asks of the same session and tool within `TWIN_WINDOW = 1.5 s` either way, one-to-one: channel first -> hook answered no-decision at once; hook first -> held 1.5 s, a channel twin in that time answers it no-decision. Without a twin the ask becomes a prompt in the TASK-014 book (`Prompt.hook = true`, hub-minted 5-letter id, conn 0), same Allow/Deny buttons and waiting icon. First press -> `State::Decided`, the answer goes to the hook's oneshot on the next pump; the usual decided edit removes the buttons. Timeout (`Options::hook_answer_wait`, default 90 s), gone hook, SessionEnd (existing `close_prompts`), full book, failed send -> no decision; the prompt ends as new `State::Expired` ("Запрос устарел") or `Closed`. A press after the hook left answers "Запрос устарел". While hooks wait the actor wakes at least every 1 s. On `Control::Stop` all waiting hooks get no decision and their prompts are finished as Expired (edit best effort).
- Wire (wire.rs): `PERMISSION_PATH`, `PermissionPost`, `PermissionAnswer`, `decode_permission` (version-checked). Additive, no `VERSION` bump.
- Logs: short session id, `?behavior` and fixed text only; no tool input, description, command or secret (hub and hook side).

Files (git numstat, +/-):
- crates/cctg/src/hook.rs +371 -4 (incl. tests)
- crates/cctg/src/hub/ingress.rs +172 -18
- crates/cctg/src/hub/slots.rs +463 -8 (incl. 4 tests)
- crates/cctg/src/hub/permissions.rs +51 -1
- crates/cctg/src/hub/mod.rs +8 -2
- crates/cctg/src/wire.rs +60
- crates/cctg/tests/permission_hook_e2e.rs new, 413 lines
- crates/cctg/tests/hook_cli.rs +8 (settings snippet test knows PermissionRequest and its timeout 100)
- docs/hook-settings.json +11, docs/poc.md +4 -1

## 2. Deviations

- Twin match uses session + tool_name only (the channel `input_preview` format is not comparable with the hook `tool_input`). Two parallel requests of the same tool within 1.5 s can pair wrongly (one hook gets no decision -> the terminal dialog still works).
- A channel twin that arrives later than 1.5 s after the hook (only possible if the hook does not block the terminal dialog and Claude Code relays late) gives two sets of buttons; not handled on purpose (see log decision).
- Hub stop: the hook gets 204 if the ingress task still runs, otherwise the connection closes; both are "no decision".
- The PermissionRequest hook does not replay the session spool (no network budget to share, and it must not delay the prompt).

## 3. Tests

`CARGO_TARGET_DIR=%TEMP%/cctg-task028-target CARGO_PROFILE_DEV_DEBUG=0`, `-j 1`:
- `cargo fmt --all -- --check`: clean.
- `cargo clippy -j 1 --workspace --all-targets -- -D warnings`: clean.
- `cargo test -j 1 --workspace`: all green (lib 440 passed / 1 ignored; permission_hook_e2e 5 passed; hook_cli 8; every other binary passed). Target dir deleted afterwards.

New tests:
- permission_hook_e2e.rs (real `cctg hook PermissionRequest` binary + `serve_hooks_and_permissions` + Slots + Scheduler with a fake Telegram): press allow/deny -> decision JSON on stdout and buttons removed; channel request first -> hook exits in < 1.5 s with empty stdout and no second prompt; SessionEnd while waiting -> empty stdout, prompt closed; hub `Control::Stop` while waiting -> empty stdout; nothing listening -> empty stdout, exit 0, < 4 s. stderr checked for secret, command text and description.
- slots.rs: no twin -> prompt after >= 1.5 s, press goes to the hook only, second press "Уже решено", one edit; twin before/after -> None quickly, other tool still prompts; timeout / gone hook / SessionEnd / ended session -> None and buttons closed; hook gone before show -> no prompt.
- hook.rs: `ask` against the real ingress (allow, deny, 204, dropped, 401, old-hub 404), strict answer parsing, `build_permission` fields/caps/skips, decision JSON shape.
- permissions.rs: hook ids are valid request ids; Expired final text. wire.rs: PermissionPost round trip and version check.

## 4. Manual verification

After deploy, add to the live `settings.json` hooks:
`"PermissionRequest": [{ "hooks": [{ "type": "command", "command": "\"<cctg>\" hook PermissionRequest", "timeout": 100 }] }]`
Then in a channel session trigger a normal permission prompt: one set of buttons only (the channel's). In auto mode trigger the safety check (e.g. `rm -rf "$UNSET"/`): after ~1.5 s buttons appear in the topic; Deny/Allow there closes the terminal dialog with that decision. Not answering for 90 s: buttons become "Запрос устарел" and the terminal dialog stays. Unverified live: whether a waiting hook hides the terminal dialog; the design gives one answer path in both cases.
