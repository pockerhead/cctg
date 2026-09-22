# Disconfirmation target

Concrete counterexample: a finished subagent transcript renders a brief ending in
`not done`, while `last_assistant_message` is `done`. A raw
`brief.ends_with(last_assistant_message)` check returns true even though the hook
message is not the final rendered message, so the implementation incorrectly
chooses the stale transcript instead of `LastMessage`.

Status: confirmed. `src/subagent.rs:148` uses `brief.ends_with(last)`. The
adversarial integration test `last_message_must_match_a_complete_rendered_line`
builds a finished transcript whose brief is `not done` and whose hook value is
`done`; the prototype returns `Transcript("not done")` instead of
`LastMessage("done")`.

The same adversarial test run found two further failures:

- mixed parent + sidechain turns render the sidechain prompt and answer again at
  parent top level;
- an embedded block uses the parent `Agent` type/description rather than the
  conflicting `.meta.json` values held by `Subagent`.

Command: `cargo test -p transcript --test reviewer1_adversarial -- --nocapture`
with `CARGO_TARGET_DIR` under `%TEMP%`; result: 0 passed, 3 failed.
