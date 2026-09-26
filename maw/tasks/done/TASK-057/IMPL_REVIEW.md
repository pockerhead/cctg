# TASK-057 IMPL_REVIEW (code-reviewer, commit c10bff9)

## Verdict

**NEEDS_WORK**: the wrap joining fixes the live Linux refusal and every draft path still refuses. But the faint filter models Claude Code's inline completion wrongly: the renderer draws the completion's first character solid, so a completion still refuses in most cases. The summary, FINDINGS §4 and the tests all say this case is covered.

## Disconfirmation

Test case: a bash-mode command with a history inline completion after it, drawn the way Claude Code 2.1.283 actually draws it. If the completion's first grapheme is not faint, `typed_box` keeps that grapheme and `box_shows` refuses, so the "inline completion is covered" claim is false.

- Bundle proof: `~/.local/bin/claude.exe` @217462400, `Cursor.render(e,r,n,s,…)`: `let H=q8e(s.text)||s.text[0]; te=e?n(H):H; let Y=s.text.slice(H.length); if(Y.length>0)J=s.dim(Y)`. The first grapheme `H` is either plain (hardware cursor, `cursorChar:""`) or inverted (painted cursor). Only the rest goes through `dim`.
- Repro: a %TEMP% crate against the worktree `cctg`, shared target, deleted afterwards. It fed real-shaped bytes through `Screen::rows` → `typed_box` → `box_shows`:

```
ghost first char plain:   solid=["!\u{a0}curl -s localhost/api/v"] shows=false
ghost first char inverse: solid=["!\u{a0}curl -s localhost/api/v"] shows=false
ghost first char space:   solid=["!\u{a0}curl -s localhost/api"]   shows=true
ghost all faint (impl model): solid=["!\u{a0}curl -s localhost/api/"] shows=true
```

**The counter-example held.** The fix works only when the completion suffix starts with whitespace, because the trailing space gets right-trimmed. The failure is the safe kind (`Typed::Draft`, nothing sent), but the claimed fix is incomplete.

## Confirmed correct

- **The live failure is fixed by the wrap joining.** The prompt suggestion disappears on the first typed character (`showPlaceholder: p.length===0`, FINDINGS §1). So the screenshot's `Да, давай T2I` could not be on screen after typing. A 100-190 char command in a pane about 110 columns wide wraps, and `box_shows` used to require exactly one row. `keys.rs:290-324` now joins continuation rows for both the glyph form and the bash `!` form. The probe shapes (break after a word with the space dropped, break inside a word, 2-space indent) are tested in `keys.rs` `a_line_wrapped_in_the_box_is_joined_back`.
- **Drafts still refuse:**
  - A draft row before or after our text fails `joins`.
  - An empty first piece is refused (`!first.trim().is_empty()`).
  - A missing or extra piece is refused.
  - `type_exit` goes through the same `watch` → `typed_box` → `box_shows` path (`keys.rs:363`, `:440-452`), so the TASK-040/047 `/exit` safety holds.
  - `agents_block`, `panel` and `exit_dialog` still read the full text (`lines()`), so the faint `Message @agent…` placeholder keeps blocking.
- **The old `cctg run` fallback works.** An old `answer` fails to parse `"rows"` as its `Ask`, returns without a reply and drops the stream. The client gets EOF and empty bytes, the parse fails, and `term.rs:176-181` falls back to `"screen"` with `solid: None`. The `"screen"` answer is unchanged (`asks_are_answered_one_line_each`), and the ask format only grows.
- **vt100 faint detection:**
  - `cell.dim()` reads the per-cell mode. An SGR 38;2 between `2m` and `22m` keeps dim (`faint_text_is_left_out_of_the_solid_rows`, and my repro with a colon sub-param).
  - Wide characters stay in place (`is_wide_continuation`).
  - `lines` and `solid` are built under one lock (`answer`, `term.rs:151-157`).
  - `typed_box` uses `solid` only when its length matches (`keys.rs:252`).
- **Windows only gains the wrap joining.** `Attached::rows` returns `solid: None` (`keys.rs:582-587`), and the `lines()` default method keeps the other readers unchanged.
- **Build and tests:**
  - `cargo fmt --check` is clean.
  - `cargo clippy --workspace --all-targets -D warnings` is clean.
  - `cargo test -p cctg --lib`: 731 passed.
  - The workspace run showed no failures in the part I saw (output truncated).
  - The unix-only `an_old_run_still_gives_its_rows` reads correctly and runs in the CI matrix (ubuntu and macos).
  - No new crates were added.

## Issues

1. **major**, `crates/cctg/src/term.rs:102-125` (`Screen::rows`) together with `keys.rs:249-255`. **The inline completion is not covered.**
   - Claude Code draws the completion's first grapheme solid (plain or inverse, never dim); proof and repro are above. So after typing, `solid` holds our text plus one foreign character, and the command refuses as `Draft` unless the suffix starts with whitespace.
   - `IMPL_SUMMARY` §1/§3, FINDINGS §1 ("The inline completion … also SGR 2") and §4 ("Both are covered now") are wrong for this case.
   - Ghost text only exists while the cursor is at the end (`this.isAtEnd()`), and accepting it is bound to Tab only (bundle @207596208: `Autocomplete: {tab: "autocomplete:accept", …}`). So Enter submits only the typed value.
   - Suggested fix (keys side, attribute-agnostic):
     - Accept when the solid box equals `text` plus exactly one grapheme on the last row, and the full `lines` row continues past `solid` (faint cells follow).
     - A one-grapheme suffix has no faint tail and keeps refusing, which is safe.
     - Add a test built from the real renderer bytes: typed + plain `H` + `\x1b[2m` rest, and typed + `\x1b[7mH\x1b[27m` + dim rest.
   - Or, if this is out of scope: record it as a known limit in FINDINGS and the summary, and fix the tests (issue 3).

2. **minor**, `crates/cctg/src/term.rs:109-113`. **Faint cells are removed anywhere in the box, not only in a trailing ghost run.** The draft-safety rule assumes the user's own text is never faint, but the bundle has two exceptions:
   - The input highlights carry a `dimColor` range for the voice-dictation `interimRange` (@228924798: `if(zo)Fi.push({start:zo.start,end:zo.end,color:void 0,dimColor:!0,priority:1})`, where `zo = xe(D, IZ)` and `IZ = h => h.interimRange`). That range is part of the value.
   - The text input has a whole-value `dimColor` prop (`q = o.dimColor`); I did not trace when it is set.

   If interim dictation text sits in the box when the agent types, it is dropped from `solid`, `box_shows` passes, and Enter submits the user's words together with our command. That breaks the "never type into a draft" rule. The window is narrow (push-to-talk, user at the terminal).

   Suggested fix: drop faint text only as the box's trailing faint run (the rest of the last non-blank row, plus later rows only when they are entirely faint). Keep any faint cell that has solid text after it. The placeholder and the completion are both trailing, so no current case is lost.

3. **minor**, `crates/cctg/src/term.rs:659-666` and `keys.rs` tests (`Fake.ghost` together with `faint`). **The tests encode the wrong renderer model.**
   - The term test feeds `!ls 日本\x1b[2m -la`, a fully faint completion that also starts with a space. That is the one shape that passes both before and after this change.
   - `faint_text_in_the_box_is_not_a_draft` makes the whole ghost faint too.
   - Result: the headline test proves nothing about a real completion. Rebuild these tests from the renderer shape in issue 1.

4. **minor**, `crates/cctg/src/term.rs:43-47` (the `Rows` doc) and the `keys.rs:11-14` module doc. Both say the inline completion is drawn faint. Correct them to match the renderer: the first grapheme is solid.

## Missing coverage

- An inline completion drawn as the renderer does: first grapheme plain, and first grapheme inverse, with a non-space suffix. Expected result: whatever issue 1 decides, pinned by a test.
- Faint text in the middle of the box with solid text after it (the interim-dictation shape). Must refuse.
- A wrapped command whose continuation row ends in a faint completion: wrap joining and the faint filter together.
- `rows()` against a new `cctg run` that answers `null` (no screen copy). The fallback makes a second round-trip; harmless, but untested.

## Nits

- vt100 0.16 treats bold and dim as one intensity. `\x1b[2;1m` leaves the cell bold and not dim (repro: `shows=false`). This fails the safe way; worth one line in the `Rows` doc.
- `joins` accepts zero whitespace at every row break. That is needed for a hard break inside a word, but it also accepts a user draft that is a newline followed by exactly the tail of our text. Contrived; noting it only.
