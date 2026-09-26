# TASK-057 IMPL_SUMMARY

Коммит `c10bff9` на ветке `fix/console-ghost-text` (worktree `C:/Users/user/dev/cctg-057`).

## 1. Что сделано

- `crates/cctg/src/term.rs` (+117/-4, из них тесты ~75):
  - новый ask `"rows"` (`Ask::Rows`) и тип `Rows { lines, solid: Option<Vec<String>> }`; `solid` это те же строки, где faint-ячейки (SGR 2) заменены пробелами;
  - `Screen::rows()` строит обе версии из ячеек vt100 (`cell.dim()`, широкие символы учтены) под одной блокировкой;
  - `answer` отвечает на `"rows"`; ответ на `"screen"` не изменился;
  - `term::rows(socket)`: спрашивает `"rows"`, при отсутствии ответа (старый `cctg run` молча закрывает соединение на незнакомый ask) откатывается на `"screen"` с `solid: None`;
  - doc модуля: формат asks только расширяется.
- `crates/cctg/src/keys.rs` (+184/-23, из них тесты ~120):
  - `Terminal::rows()` вместо `lines()` (`lines()` стал provided-методом); Run берёт `term::rows`, Windows `Attached` отдаёт `solid: None` (атрибуты консоли faint не несут, проба);
  - `typed_box(&Rows)`: поле ввода находится по полным строкам, текст берётся из `solid`, если оно есть и той же длины; `input_box` и новый `typed_box` делят `box_rows`;
  - `watch` после набора сверяет `typed_box`, а не `input_box`; `agents_block`, `panel`, `exit_dialog` по-прежнему читают полный текст (плейсхолдер `Message @agent…` сам faint, его нельзя выкидывать);
  - `box_shows` склеивает перенесённые строки: первая строка с глифом (или `!` bash-режима) и непустым текстом, продолжения сравниваются по порядку, на стыке допускается пропавший пробел (word wrap) или его отсутствие (перенос внутри слова).

## 2. Отклонения и что не сделано

- Проверки черновика «до набора» в коде нет и не было: единственная проверка это `box_shows` после набора. Плейсхолдер/подсказка исчезают при первом набранном символе (бандл), так что отдельная проверка до набора не нужна; не добавлял.
- Windows: признака faint нет (плейсхолдер и набранный текст оба 0x0007 в `ReadConsoleOutputW`). Windows остаётся на текстовой проверке, склейка переносов работает и там. Inline-дополнение после набранного текста на Windows по-прежнему даёт отказ Draft (безопасный исход).
- Серый argument hint слэш-команды (цвет темы `inactive`, не SGR 2) не считается призраком: команда с хвостовым пробелом (`/compact `) всё ещё отказывается. Решение и отвергнутая альтернатива в log.jsonl.
- Живой prompt suggestion в пробе не появился (пользовательские настройки не грузились); стиль подсказки подтверждён бандлом (`D3=uj&&ev?ev:s3` → плейсхолдер → `pe.dim`) и живыми байтами плейсхолдера того же рендерера. Линукс-отказ в TASK объяснён выводом, не воспроизведён вживую (FINDINGS раздел 4).
- Тест `an_old_run_still_gives_its_rows` под `#[cfg(unix)]`: на этой машине не компилировался (нет linux target), проверится в CI.

## 3. Тесты

`CARGO_TARGET_DIR=C:/Users/user/dev/cctg/target CARGO_PROFILE_DEV_DEBUG=0`, `-j 1`, lib.rs/main.rs touched:
- `cargo fmt --all -- --check`: чисто.
- `cargo clippy -j 1 --workspace --all-targets -- -D warnings`: чисто.
- `cargo test -j 1 --workspace --no-fail-fast`: 931 passed, 0 failed.
- Флейки при прогонах без `--no-fail-fast` (не связаны с изменением, при повторе поодиночке зелёные): `update_e2e` (3 теста, прогон шёл параллельно со сборкой в общем target), `hook_cli::every_event_reaches_the_hub` и `no_hub_listening_is_quiet_and_fast` (`SessionStart: 1.2s`, тайминг).
- Новые тесты: `keys::faint_text_in_the_box_is_not_a_draft`, `keys::the_typed_box_leaves_out_faint_text_only_where_known`, `keys::a_line_wrapped_in_the_box_is_joined_back`, `term::faint_text_is_left_out_of_the_solid_rows` (байты из живой пробы через vt100 до `box_shows`), `term::an_old_run_still_gives_its_rows` (unix), расширены `the_ask_format_is_frozen`, `asks_are_answered_one_line_each`, `without_unix_sockets_nothing_answers`.

## 4. Как проверить руками

- Linux/macOS: `claude-cctg` (новый `cctg run`) в терминале уже недостаточной ширины, дождаться серой подсказки в пустом поле, прислать из темы `! echo <длинная строка шире терминала>` → команда набрана и отправлена; набрать в терминале настоящий черновик и повторить → отказ «неотправленный текст», черновик на месте.
- Совместимость: агент новой сборки со старым запущенным `cctg run` набирает как раньше (ask `rows` без ответа → `screen`).
- Факты пробы: `scratch/FINDINGS.md`, скрипты и сырые дампы: `scratch/probe/` (`conpty.bin`, `conhost_*.json`, `show_box.py`).
