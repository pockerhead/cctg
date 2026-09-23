# IMPL_SUMMARY — TASK-008

## 1. Что реализовано

Добавлена библиотечная часть `cctg::hub` и подключена команда `cctg hub`:

- тонкий Telegram Bot API клиент на `reqwest` 0.13 + rustls с 11 методами, узкими serde-моделями и безопасными ошибками без URL/токена;
- конфигурация из явно указанного env-файла или `./.env`, с приоритетом process env, проверкой формы `-100...`, обязательным allowlist и редактированием секретов/Telegram user id;
- startup-проверка `getMe` → `getChatMember` → `can_manage_topics` до запуска polling;
- толерантный long polling, allowlist gate по `from.id`, обработка неизвестных/битых апдейтов без остановки polling и распознавание четырёх видов `forum_topic_*` service messages;
- единый outbound scheduler: общий message token bucket, FIFO внутри темы, приоритет permission-трафика без обгона собственной темы, коалесинг правок, отдельные неметрированные очереди edit/topic и общая пауза по `retry_after` без retry storm;
- интеграционные проверки stdout/stderr и отдельный стабильный test binary для проверки отсутствия user id в routing-логах.

Изменённые продуктовые файлы (добавлено/удалено; итоговое число строк):

| Файл | Изменение | Итог |
|---|---:|---:|
| `Cargo.toml` | +3 / -0 | 19 |
| `Cargo.lock` | +1360 / -3 | 1787 |
| `crates/cctg/Cargo.toml` | +9 / -1 | 19 |
| `crates/cctg/src/main.rs` | +16 / -3 | 77 |
| `crates/cctg/src/lib.rs` | +4 / -0 | 4 |
| `crates/cctg/src/hub/mod.rs` | +108 / -0 | 108 |
| `crates/cctg/src/hub/api.rs` | +452 / -0 | 452 |
| `crates/cctg/src/hub/config.rs` | +244 / -0 | 244 |
| `crates/cctg/src/hub/scheduler.rs` | +785 / -0 | 785 |
| `crates/cctg/src/hub/updates.rs` | +350 / -0 | 350 |
| `crates/cctg/tests/stdout.rs` | +63 / -1 | 84 |
| `crates/cctg/tests/routing_logs.rs` | +75 / -0 | 75 |

Все 12 файлов совпадают с проверенным reference по SHA-256. Доказательства implementer-прогона оставлены в `scratch/change_line_counts.out.txt`, `scratch/static_checks.out.txt`, `scratch/flake_checks.out.txt` и `scratch/release_measure_implementer.out.txt`.

## 2. Что не реализовано и почему

Отклонений от продуктовой части плана нет.

Реальный Telegram API не вызывался и локальный `.env` не читался согласно ограничениям задачи. Idle RSS повторно не измерялся: эта проба требует стартовых TLS-вызовов с реальными Telegram credentials. В таблицу ниже перенесён уже зафиксированный planner-замер. Живой long-poll RSS оставлен для TASK-009 согласно плану.

## 3. Результаты проверок

- `cargo fmt --all -- --check` — успешно, изменений форматтера нет.
- `cargo clippy --workspace --all-targets --offline -- -D warnings` — успешно, предупреждений нет.
- `cargo test --workspace --offline` — успешно: **103 passed, 0 failed** (cctg lib 27, main 1, routing logs 1, stdout 3, transcript 70, transcript doc-test 1).
- `cargo tree -p cctg --edges normal --depth 1 --offline` — ровно 10 прямых зависимостей: `anyhow`, `clap`, `dotenvy`, `reqwest`, `serde`, `serde_json`, `thiserror`, `tokio`, `tracing`, `tracing-subscriber`.
- `scratch/run_flake_checks.ps1` — cctg lib **50/50**, `routing_logs` **50/50**, падений нет.
- `git diff --check` — успешно.
- Повторная сверка SHA-256 после сборок — **12/12**.
- Статическая проверка scheduler — численного лимита edit/topic mutation нет; message bucket применяется только к `Send`/`SendDocument`.

### Измерения

Хост: 16 логических процессоров, rustc 1.95.0, Windows 11. Основной вклад в время сборки даёт `aws-lc-sys` и его C toolchain.

| Метрика | HEAD без hub | TASK-008 | Evidence / условия |
|---|---:|---:|---|
| Release `cctg.exe` | 994,304 B | **5,490,688 B** (повторный implementer-замер; совпал с reviewer-2), planner reference 5,485,056 B | `scratch/release_measure_implementer.out.txt`, `scratch/reviewer2/release_measure.out.txt`, planner `ls -l` |
| Clean release build, пустой target, offline | 8.44 s, 40 crates | 45.56 s, 127 crates на простаивающем planner-хосте; 91 s на нагруженном reviewer-2-хосте | `scratch/planner/build_base.log`, `scratch/planner/build_ref.log`, `scratch/reviewer2/release_build.log` |
| Текущий implementer release build | n/a | 48.35 s (repo `target/`; для release-профиля это была первая сборка в данном target) | `scratch/release_measure_implementer.out.txt` |
| Idle после стартовых TLS-вызовов, scheduler запущен, без `getUpdates` | n/a | working set ≈18.7 MB, private ≈5.3 MB, стабильно 5–40 s | `scratch/planner/rss_probe.out.txt`; не перемерялось, чтобы не читать `.env` и не обращаться к Telegram |

## 4. Как проверить вручную

1. Без секретов запустить `cargo test --workspace --offline` и убедиться, что проходят 103 теста, включая scheduler, allowlist, service-message routing, безопасные ошибки и 429.
2. Запустить `cargo clippy --workspace --all-targets --offline -- -D warnings` и `cargo fmt --all -- --check`.
3. Запустить `powershell -File maw/tasks/in_progress/TASK-008/scratch/run_flake_checks.ps1`; ожидается `50/50 passed` для обоих harness.
4. Проверить безопасный startup failure: из пустого каталога без `CCTG_*` выполнить `cctg hub`; процесс должен завершиться ненулевым кодом, stdout должен быть пуст, stderr должен назвать только отсутствующую переменную.
5. Для живой проверки на отдельном разрешённом этапе создать некоммитящийся `.env` с `CCTG_BOT_TOKEN`, `CCTG_CHAT_ID=-100...`, `CCTG_ALLOWED_USER_IDS`, затем запустить `cctg hub`. До сообщения `hub started, polling` должны успешно пройти `getMe` и проверка `can_manage_topics`; без права запуск должен завершиться понятной startup-ошибкой.
