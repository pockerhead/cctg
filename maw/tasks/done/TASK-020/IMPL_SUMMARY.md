# Implementation summary

## 1. Что реализовано

- `crates/transcript/src/render.rs` (+24/-4): записи slash-команд распознаются до общей фильтрации service-records, имя и аргументы сворачиваются в одну строку, пустые аргументы не добавляют пробел; команда классифицируется как обычный prompt и участвует в расчёте маркера `в работе…`.
- `crates/transcript/tests/render.rs` (+16/-1): добавлена реальная фикстура в thinking-leak matrix и тест brief/full для команды с аргументом, команды без аргументов, многострочных аргументов с `<`/`>`, скрытого `local-command-stdout` и незавершённого хвоста.
- `crates/transcript/tests/parse_fixtures.rs` (+3/-1): новая фикстура включена в общие проверки приватности и JSON-validity, а также в parser thinking-leak matrix.
- `crates/transcript/tests/fixtures/slash_command.jsonl` (+6/-0): обезличенная фикстура скопирована из `scratch/slash_command.jsonl` байт-в-байт; Git blob hash обеих копий — `4e07b318c04d13ee7ac0286c5641ea97192484f8`.

## 2. Что не реализовано

Отклонений от spec нет. Обработка остальных service-префиксов не менялась; зависимости не добавлялись.

## 3. Результаты тестов

- `cargo build --workspace` — успешно.
- `cargo fmt --all --check` — успешно, изменений форматирования не требуется.
- `cargo clippy --workspace --all-targets -- -D warnings` — успешно, предупреждений нет.
- `cargo test --workspace` — успешно: 108 passed, 0 failed, 1 ignored (изолированный helper-test, как предусмотрено существующим suite).
- `cargo test -p transcript --test render slash_commands_are_one_line_prompts` — успешно: 1 passed.
- `crates/transcript/tests/purity.rs` входит в workspace suite и проходит: 3 passed, новых зависимостей нет.

## 4. Ручная проверка

1. Запустить `cargo test -p transcript --test render slash_commands_are_one_line_prompts -- --exact`.
2. Проверить ожидаемый brief в тесте: `/model opus` и `/compact` показаны как prompt-строки; многострочный `/review` показан одной строкой и заканчивается `в работе…`; `local-command-stdout` отсутствует.
3. Проверить ожидаемый full: slash-команды также нормализованы в одну строку, а `local-command-stdout` по-прежнему виден как service-record.
