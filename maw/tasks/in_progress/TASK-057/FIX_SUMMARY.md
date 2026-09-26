# TASK-057 FIX_SUMMARY

Коммит `d999f40` на `fix/console-ghost-text` (worktree `C:/Users/user/dev/cctg-057`), поверх `c10bff9`.

## Preflight: самое опасное утверждение ревью

Предложенный в issue 1 фикс дословно: «принять, если solid = text плюс ровно одна графема на последней строке и полная строка `lines` продолжается за `solid`». Если сделать так поверх старого `solid` (faint-ячейки вычищены везде), принимается и настоящий черновик: наш текст, потом символ пользователя, потом любая faint-ячейка где-то дальше (например interim-диктовка через пробел). Проверил по коду: `Screen::rows` (term.rs) гасит faint в любом месте строки, а `typed_box` брал `solid` целиком. Значит утверждение реальное, и дословно его делать нельзя. Сделал по решению оркестратора: faint-хвост должен начинаться сразу после этого символа, и отбрасывается только хвостовой faint-отрезок.

Проверка бандла (`~/.local/bin/claude.exe`, 2.1.283), все утверждения ревью подтвердились:
- `Cursor.render` @217463838: `let H=q8e(s.text)||s.text[0];te=e?n(H):H;let Y=s.text.slice(H.length);if(Y.length>0)J=s.dim(Y)`, `q8e` @201504274 это первая графема через `Intl.Segmenter`. Первая графема дополнения стоит под курсором (plain или invert), dim только остаток.
- Enter дополнение не принимает: keybindings @207596208 (`Chat`: `enter:"chat:submit"`; `Autocomplete`: только `tab` accept, `escape`, `up`, `down`); key handler автокомплита @228709541 без списка подсказок пропускает `return`; submit @228934346 шлёт `D.value`, не ghost.
- Interim-диктовка dim: @228924798 (`dimColor:!0` на `zo`), `IZ=(h)=>h.interimRange` @228609497.

## 1. Fixed

- **Issue 1 (major, inline completion).** `keys::typed_box` теперь возвращает `TypedBox { lines, ghost_follows }`. Новая `keys::typed_shows`: `box_shows` или, если faint-хвост начинается сразу за последним символом строки, тот же `box_shows` без этого символа, при условии что перед ним на той же строке стоит не пробел. `watch` проверяет через `typed_shows`. Enter шлёт только набранное (бандл выше, записано в FINDINGS §1).
- **Issue 2 (minor, faint в середине).** Отбрасывается только хвостовой faint-отрезок: остаток последней строки с solid-текстом, начиная с первой faint-ячейки, и строки после неё (они целиком faint). Если хоть одна faint-ячейка стоит перед solid-текстом (строка до последней solid отличается от `lines`, или последняя не префикс `lines`), фильтр выключается и поле читается целиком, то есть это отказ. Логика живёт в keys (судит агент), формат ответа `rows` не менялся.
- **Issue 3 (minor, тесты).** `Fake` рисует байты так, как рисует рендерер, и читает их через настоящий vt100 `term::Screen`: плейсхолдер целиком faint, дополнение = первый символ plain или `\x1b[7m…\x1b[27m` плюс `\x1b[2m`-остаток, interim-черновик faint. Новые тесты: `an_inline_completion_is_not_a_draft` (plain и inverse курсор, дополнение с пробелом, односимвольное дополнение → Draft, без faint → Draft), `a_draft_still_blocks_with_faint_text_around`, `only_the_trailing_faint_run_is_left_out` (лишние 2 символа, пробел перед символом, faint в середине → отказ), `a_wrapped_line_with_a_completion_is_joined_back` (перенос плюс дополнение, faint-строка между solid → отказ). `term::faint_text_is_left_out_of_the_solid_rows` переделан на форму inverse-символ + dim-остаток. Все модели «дополнение целиком faint» удалены.
- **Issue 4 (minor, docs).** Исправлены doc `Rows` (term.rs), doc модуля keys, doc `typed_box`/`typed_shows`; в doc `Rows` добавлена строка ревью про bold+faint в vt100. FINDINGS: §1 исправлен (дополнение не целиком faint, Enter со ссылками на offsets), §4 обновлён, в «Limits left» добавлены отказные случаи и остаточное допущение.

## 2. Skipped

- Nit «`joins` допускает ноль пробелов на каждом переносе»: так задумано для переноса внутри слова, ревьюер сам называет случай надуманным. Не трогал.
- Missing coverage «`rows()` против нового run, который отвечает `null`»: по коду (`term.rs` `rows`) `Ok(None)` → `flatten` → fallback на `screen`, который тоже вернёт `null` → `None`. Лишний round-trip безвреден; тест unix-only через сокет, на этой машине его не прогнать, не добавлял.
- Известные пределы (записаны в FINDINGS, безопасные отказы): односимвольное дополнение без faint-остатка; первая графема дополнения из нескольких code point; первая графема на начале перенесённой строки; Windows без признака faint. Остаточное допущение правила: символ, который пользователь набрал сразу после нашего текста в окне 400 мс, и сразу за ним faint-текст из значения (interim-диктовка, начатая в том же окне). Руками это практически недостижимо.
- IMPL_SUMMARY §1/§3 (утверждение, что дополнение целиком faint) не правил: это артефакт имплементера, исправление записано здесь и в FINDINGS.

## 3. Test results

Везде `CARGO_TARGET_DIR=C:/Users/user/dev/cctg/target CARGO_PROFILE_DEV_DEBUG=0`, `-j 1`, перед каждым прогоном touch `crates/cctg/src/lib.rs`, `main.rs`. Тестовый exe в общем target дважды был занят прогоном из другого дерева (LNK1104), поэтому запускал через ожидание плюс повтор (scratchpad `retry.sh`); чужие процессы не трогал.

- `cargo fmt --all -- --check`: чисто.
- `cargo clippy -j 1 --workspace --all-targets -- -D warnings`: чисто.
- `cargo test -j 1 -p cctg --lib -- keys:: term::`: 25 passed.
- `cargo test -j 1 --workspace --no-fail-fast`: 933 passed, 1 failed. Упал `question_hook_e2e::own_text_answers_after_other_or_as_a_reply` (`decision JSON: EOF`, бинарь шёл 60 с под нагрузкой). Повтор отдельно `cargo test -j 1 -p cctg --test question_hook_e2e`: 6 passed за 2.06 с. Это флейк по таймингу, к изменению не относится (keys/term его не касаются).
