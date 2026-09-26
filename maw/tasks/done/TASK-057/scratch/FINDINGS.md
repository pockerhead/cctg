# TASK-057 probe findings

Claude Code 2.1.283 (installed `~/.local/bin/claude.exe`), 2026-09-26. Probe folder: the fixed trusted `%TEMP%/cctg-t016-probe`, `CLAUDE*` env stripped, `--setting-sources project --strict-mcp-config --mcp-config scratch/probe/empty_mcp.json`, no channels flag. No window: conhost runs used `CREATE_NEW_CONSOLE` + `SW_HIDE`, the byte capture ran in a ConPTY (no console window at all). Only our own process tree was killed. No hub, no Telegram.

Scripts and raw evidence: `scratch/probe/`.

## 1. How the placeholder and the prompt suggestion are drawn: SGR 2 (faint)

- Bytes (`conpty_capture.py` -> `conpty.bin`, marks `idle`, `erased`, `end`): the empty box is
  `❯\u00a0\x1b[2mTry "how do I log an error?"\x1b[22m`. The placeholder is SGR 2 with the default foreground. Rules are `\x1b[38;2;136;136;136m` (theme `promptBorder`), not faint. Typed text (`draft`) is plain default foreground after `\x1b[m`.
- Bundle (`grep_bundle.py`, `near.py`): the text input renders the placeholder with `Ze()`: `o=pe.dim(t)` (chalk dim = SGR 2); with a painted cursor the first character goes through `invert` and the rest is dim. The placeholder shown is `D3=uj&&ev?ev:s3`, where `ev` is the generated prompt suggestion (`ss.status==="generated"`) and `s3` the default placeholder (`Try "..."`, and in the agent view `Message @<agent>…`). So the prompt suggestion of the Linux screenshot is the placeholder: faint.
- The inline completion after typed text (`inlineGhostText`) is NOT all faint (corrected in the fix stage after review). `Cursor.render` (bundle @217463838): `let H=q8e(s.text)||s.text[0];te=e?n(H):H;let Y=s.text.slice(H.length);if(Y.length>0)J=s.dim(Y)`. `q8e` (@201504274) is the first grapheme (`Intl.Segmenter`). So the completion's first grapheme stands under the cursor, plain (hardware cursor, `cursorChar` empty) or through `invert` (painted cursor), and only the rest is `dim` (`dim: pe.dim`, SGR 2). It is drawn only on the last wrapped row and only while the cursor is at the end (`I===j.length-1&&this.isAtEnd()`).
- Enter does not accept the completion. Keybindings (@207596208): context `Chat` binds `enter:"chat:submit"`; context `Autocomplete` binds only `tab:"autocomplete:accept"`, `escape`, `up`, `down`. The autocomplete key handler (@228709541) accepts on `right` only the empty-box prompt suggestion (`Qbe(rn)&&De===""`), on `tab` the completion; with no suggestion list it lets `return` through untouched (only a double-submit guard `vn.current` may swallow it, @228710254). The submit handler (@228934346) sends `He(bd===""?"":D.value,...)`: the typed value, never `inlineGhostText`.
- The placeholder shows only while the value is empty (`showPlaceholder: p.length===0`) and only in prompt mode (`uj` needs `jt==="prompt"`): a typed `!` (bash mode) or any typed character hides it. What can stay after typing is the inline completion.
- Not faint: the argument hint of a slash command. `/compact ` (trailing space) showed `\x1b[38;2;153;153;153m<optional custom summarization instructions>`: theme `inactive` grey (153 dark, 102 light, `ansi:blackBright` / `ansi:white` in the ansi themes). Not handled by TASK-057 (see limits).
- No live prompt suggestion appeared after a one-word turn in the probe (user settings were not loaded); the suggestion style comes from the bundle plus the live placeholder bytes, which use the same renderer.

## 2. Windows console attributes carry no faint

`console_attrs.py` (ReadConsoleOutputW on the hidden conhost, dumps `conhost_*.json`, `show_box.py` prints the box): the faint placeholder `❯\u00a0Try "refactor <filepath>"` reads as attribute `0x0007` over the whole row, exactly like typed `❯\u00a0draft`. Rules read `0x0008` (the grey maps to FOREGROUND_INTENSITY), bash-mode rules `0x000c`. No COMMON_LVB_REVERSE_VIDEO on the first placeholder character. So on Windows the attribute words cannot tell the ghost from typed text; the keys reader keeps its text-only view there (`Rows.solid = None`). The cursor position is at the start of the placeholder and after typed text, but a real draft after the cursor looks the same, so it is not a safe signal either.

## 3. A long line wraps in the box

`drive_wrap.py` (120 columns, typed then erased, nothing submitted):
- `!echo word00 … word24` (180 chars): row 1 `!\u00a0echo word00 … word15`, row 2 `  word16 … word24`. Broken after a word, the space at the break dropped, the next row indented by two spaces, no glyph.
- 150 × `x`: row 1 `❯\u00a0` + 118 `x`, row 2 `  ` + 32 `x`: broken inside the word.
- `/compact` without a trailing space showed no hint in the conhost run.

## 4. Why the Linux refusal happened (inference, not reproduced live)

The prompt suggestion vanishes as soon as anything is typed (section 1), so after typing the box held either an inline completion (faint, e.g. from shell history in bash mode) or a command longer than the terminal width (wrapped, section 3). Both made `box_shows` see more than the typed text on one row. Both are covered now on Unix: the box's trailing faint run is left out (`keys::typed_box`), one more character right before that run with no whitespace before it is taken for the completion's first grapheme under the cursor (`keys::typed_shows`), and wrapped rows are joined on both platforms. Enter with a completion on screen submits only the typed value (section 1, bundle offsets); not checked live.

Only a trailing faint run counts as ghost text: faint text with solid text after it stays in the box. The bundle dims text that is part of the value too: interim voice dictation (`dimColor` highlight of `interimRange`, @228924798, `IZ=(h)=>h.interimRange` @228609497) and a whole-value `dimColor` prop.

## Limits left

- A one-grapheme completion (no faint rest), a completion whose first grapheme is several code points (e.g. a combining sequence), or one whose first grapheme lands at the start of a wrapped row, still refuses (`Draft`, the safe side).
- Residual assumption of the completion rule: a character the user types right after our text within the echo wait (400 ms), immediately followed by faint text that is part of the value (interim dictation started in the same window), would be taken for the completion's first grapheme. Not reachable by hand in practice (push-to-talk is held Space).

- Windows: no faint signal (section 2); an inline completion after typed text still refuses there, as before.
- The grey argument hint (`inactive` colour) is not treated as ghost text: a slash command typed with a trailing space still refuses.
