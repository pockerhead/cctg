# Fix summary

## Fixed

- **Review: `IMPL_SUMMARY.md` неверно описывает `.gitignore`.** Исправлено: summary теперь указывает, что `.gitignore` был создан в этой задаче с правилами для `target/`, `.env`, `*.log`, `.cctg/` и `registry.json`.
- **Review: счётчики строк в `IMPL_SUMMARY.md` устарели.** Удалены все счётчики строк, а не только два отмеченных reviewer: они не несут полезной информации и быстро устаревают. Число прошедших тестов обновлено после добавления integration test.
- **Review: dependency regex в `scratch/verify_bootstrap.ps1` был no-op.** `cargo tree` теперь запускается с `--prefix none`, поэтому якорь `^` действительно сопоставляется с именами зависимостей. Вывод проверяется в памяти и сохраняется в `transcript-tree.txt` как UTF-8 без BOM; это исключает дополнительную Windows-проблему с кодировкой shell redirection. Отдельный отрицательный probe подтвердил, что синтетическая строка `tokio v1.0.0` теперь обнаруживается.
- **Review: нет автоматической защиты чистоты stdout.** Добавлен `crates/cctg/tests/stdout.rs`, который запускает собранный binary через `CARGO_BIN_EXE_cctg` для `hub`, `agent` и `hook SessionStart`, требует успешный exit status и пустой stdout.

## Skipped

- **Toolchain pin / `rust-version = "1.85"`.** Не добавлен. Спецификация не задаёт toolchain floor; review проверял проект на Rust 1.95 и не доказал совместимость всего lockfile с 1.85. Буквальный pin мог бы внести новую поломку, тогда как текущие обязательные build/test/clippy проходят.
- **Удаление объявленных `tracing`, `serde`, `serde_json`.** Не выполнялось: task требует подключить базовые общие зависимости, а prompt отдельно запрещает удалять их без доказанного нарушения критерия. Нарушения нет.
- **Дополнительные тесты clap failure paths.** Поведение не является дефектом: review уже подтвердил exit code 2 и пустой stdout. Для этой skeleton-задачи добавлен только запрошенный regression test трёх рабочих подкоманд.
- **Валидация всех hook event names / TODO.** Не добавлялась: `hook <event>` по спецификации является skeleton без бизнес-логики, а ограничение строки событий расширило бы scope.
- **Тесты `transcript`.** Не добавлялись: библиотека пока содержит только doc comment и не имеет поведения для тестирования.
- **Nits про no-op `match`, порядок `init_tracing()` и игнорирование `try_init()` error.** Оставлены без изменений: это сознательная минимальная CLI-заглушка, stdout явно защищён integration test, а функционального дефекта review не показал.

## Test results

- `cargo fmt --all --check` — exit 0, без вывода.
- `cargo build --workspace` — exit 0; `Finished dev profile`.
- `cargo test --workspace` — exit 0; 1 unit test и 1 integration test passed, 0 failed; transcript unit/doc tests также завершились без ошибок.
- `cargo clippy --workspace --all-targets -- -D warnings` — exit 0, предупреждений нет.
- `maw/tasks/in_progress/TASK-002/scratch/verify_bootstrap.ps1` — exit 0; `bootstrap acceptance probe passed`.
- `cargo metadata --format-version 1 --no-deps` — workspace packages: `cctg`, `transcript`; единственный binary target: `cctg`.
- `git check-ignore --no-index` для `target/probe`, `.env`, `probe.log`, `.cctg/probe`, `registry.json` — все пять путей ignored.
- Независимый process probe: `hub`, `agent`, `hook SessionStart` — каждый exit 0, stdout 0 символов, stderr 0 символов.
