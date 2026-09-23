# TASK-009 Implementation Summary

## 1. Что реализовано

Реализованы локальные команды hub `/brief [n] [session-id-prefix]` и `/full [n] [session-id-prefix]` по утверждённому плану. Команды последовательно обрабатываются отдельным worker, находят top-level Claude Code transcript через узкий `TranscriptLocator`, читают его с лимитом 256 MiB, используют библиотечные `parse` / `last_prompts` / `render_brief` / `render_full` и доставляют результат через общий scheduler. Короткий ответ сохраняет порядок Telegram-кусков, большой отправляется документом; Telegram 400 `too long` один раз переключает недоставленный хвост на документ.

Добавлено атомарное хранение offset для `getUpdates`: temp-файл, `sync_all`, rename, загрузка при старте, игнорирование состояния старше 24 часов, два коротких retry и продолжение polling при устойчивой ошибке записи. Offset сохраняется до передачи batch обработчику; следующий offset вычисляется по batch, включая случай перезапуска Telegram update ids ниже прежнего значения.

Изменены 13 продуктовых файлов (итого +2005/-29 строк относительно HEAD):

- `Cargo.lock` — +1/-0
- `crates/cctg/Cargo.toml` — +1/-0
- `crates/cctg/src/hub/commands.rs` — новый, 885 строк
- `crates/cctg/src/hub/config.rs` — +53/-3
- `crates/cctg/src/hub/mod.rs` — +183/-11
- `crates/cctg/src/hub/offset.rs` — новый, 124 строки
- `crates/cctg/src/hub/sessions.rs` — новый, 252 строки
- `crates/cctg/src/hub/testdir.rs` — новый, 30 строк
- `crates/cctg/src/hub/updates.rs` — +255/-7
- `crates/cctg/tests/command_logs.rs` — новый, 136 строк
- `crates/transcript/src/lib.rs` — +2/-1
- `crates/transcript/src/render.rs` — +28/-6
- `crates/transcript/tests/render.rs` — +55/-1

Все 13 файлов совпадают с SHA-256 из `scratch/reviewer2/hashes.txt`.

## 2. Что не реализовано

Отклонений от продуктового плана нет. По решению оркестратора не выполнялись живой Telegram `/brief` round-trip и измерение RSS под long poll; их следует сделать smoke-проверкой после merge. Реальный Telegram API, `.env` и пользовательский Claude Code projects directory не читались. Коммит не создавался: это оставлено оркестратору.

At-most-once имеет ожидаемое окно потери: crash после сохранения offset и до ответа теряет команды уже подтверждённого batch, поэтому пользователь повторяет команду. Если запись offset не удалась после двух retry, рестарт до следующего успешного сохранения может повторить batch; polling при этом остаётся живым.

## 3. Результаты тестов

- Baseline до изменений: `cargo fmt --all -- --check` — успешно; `cargo clippy --workspace --all-targets --offline -- -D warnings` — успешно; `cargo test --workspace --offline` — 110 passed, 1 ignored.
- После изменений: `cargo fmt --all -- --check` — успешно.
- После разрешённого `cargo clean -p cctg -p transcript` для устранения stale Cargo artifact: `cargo clippy --workspace --all-targets --offline -- -D warnings` — успешно, warnings отсутствуют.
- `cargo test --workspace --offline` — 140 passed, 0 failed, 1 ignored.
- `cargo test -p cctg --offline --no-run`, затем прямой запуск cctg lib test binary 30 раз и `command_logs` test binary 30 раз — 0 падений. Свидетельство: `scratch/impl_flake_results.txt`.
- Проверка diff на `Users/.../user` и `AppData` — совпадений нет.
- Финальная SHA-256 проверка всех 13 файлов — все `OK`.

## 4. Ручная проверка

1. После merge настроить тестового приватного бота через обычный `.env`, при необходимости задать `CCTG_PROJECTS_DIR` и `CCTG_STATE_DIR`.
2. Создать несколько синтетических top-level `<uuid>.jsonl` в разных project-каталогах и более свежий файл в `<uuid>/subagents/`; проверить, что без префикса выбирается самый свежий top-level файл, а subagent не выбирается.
3. Отправить `/brief`, `/full 1 <unique-prefix>` и неоднозначный prefix. Сверить краткий/полный текст с прямым выводом библиотеки и убедиться, что неоднозначность выдаёт кандидатов без угадывания.
4. Проверить transcript с ответом длиннее четырёх Telegram-кусков: должен прийти один `.txt` document. Смоделировать Telegram 400 `message is too long` на текстовом куске: оставшийся хвост должен уйти одним document без повторного fallback.
5. Перезапустить hub после обработки команды и убедиться, что update не обработан повторно. Затем удалить/испортить transcript: пользователь получает понятный notice, а следующая команда продолжает обрабатываться.
