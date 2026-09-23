# TASK-010 implementation summary

## 1. Что реализовано

- `Cargo.toml` — +1 строка: workspace-зависимость `subtle = "2.6"`.
- `Cargo.lock` — +1 строка: `subtle` добавлен в прямые зависимости `cctg`; новых пакетов в lockfile нет.
- `crates/cctg/Cargo.toml` — +2/-1 строки: подключён `subtle`, для Tokio включены `io-util` и `net`.
- `crates/cctg/src/lib.rs` — +3 строки: экспортированы модули `agent`, `hook`, `wire`.
- `crates/cctg/src/wire.rs` — новый файл, 725 строк: версионированный newline-JSON контракт agent↔hub, ограниченное чтение строк, редактируемый shared secret с constant-time сравнением, hook payloads и случайный `event_id`.
- `crates/cctg/src/agent.rs` — новый файл, 393 строки: persistent TCP link агента, handshake/register, equal-jitter backoff, reconnect и повторный Register.
- `crates/cctg/src/hook.rs` — новый файл, 254 строки: один HTTP/1.1 POST с общим timeout, без reconnect-цикла, строгая проверка status line.
- `crates/cctg/src/hub/config.rs` — +102/-1 строки: обязательный hub secret на старте, loopback listener defaults и явные `ip:port` overrides.
- `crates/cctg/src/hub/ingress.rs` — новый файл, 1186 строк: TCP ingress агентов, строгий HTTP hook endpoint, bounded parsing/queues/concurrency и bounded TTL/count dedup по `event_id`.
- `crates/cctg/src/hub/mod.rs` — +41/-1 строка: оба listener-а bind-ятся до Telegram API; ingress подключён через bounded channels и временный drain до TASK-011.
- `crates/cctg/tests/ingress_logs.rs` — новый файл, 192 строки: изолированная TRACE-проверка отсутствия секретов и содержимого сообщений в логах.
- `scratch/verify_task010_hashes.ps1` — 34 строки: воспроизводимая CRLF-нормализованная проверка 11 эталонных SHA-256 (PowerShell fallback для неработающего Git Bash).

Все 11 implementation-файлов совпадают с хэшами reviewed reference из `scratch/reviewer2/hashes.txt`.

## 2. Что не реализовано

Отклонений от implementation plan нет. Коммит не создан: override оркестратора явно требует оставить коммит ему. `.env` не читался и не изменялся, Telegram API не вызывался.

Существующее изменение `maw/tasks/in_progress/TASK-010/metrics.md` не относится к реализации и не изменялось в рамках этой работы.

## 3. Результаты тестов

- `cargo fmt --all -- --check` — успешно, без вывода.
- `cargo clippy --workspace --all-targets --offline -- -D warnings` — успешно, warnings отсутствуют.
- `cargo test --workspace --offline` — успешно: 184 passed, 0 failed, 1 ранее существовавший isolated config test ignored.
- 10 × `cargo test -p cctg --offline --lib -- wire:: agent:: hook:: hub::ingress:: hub::config::` — все прогоны успешны; каждый: 47 passed, 0 failed, 1 ранее существовавший ignored.
- 5 × `cargo test -p cctg --offline --test ingress_logs` — все прогоны успешны; каждый: 1 passed, 0 failed.
- `git diff --check` — ошибок whitespace нет (только предупреждение Git о будущей CRLF-конверсии существующего `metrics.md`).
- `Cargo.lock` diff — ровно одна добавленная строка `"subtle",` в dependencies пакета `cctg`.
- `scratch/verify_task010_hashes.ps1` — 11/11 `OK`.

## 4. Ручная проверка

1. Задать валидные `CCTG_BOT_TOKEN`, `CCTG_CHAT_ID`, `CCTG_ALLOWED_USER_IDS`, `CCTG_HUB_SECRET` (16+ visible ASCII); секрет не печатать и не передавать в аргументах команд.
2. Запустить `cctg hub` и убедиться, что по умолчанию listener-ы заняли `127.0.0.1:47291` и `127.0.0.1:47292` до обращения к Telegram.
3. Подключить тестовый agent client: отправить `hello` с secret, затем `register`; ожидать `registered`. Перезапустить hub и убедиться, что агент переподключился и повторил Register.
4. Отправить валидный `POST /v1/hook` с Bearer secret и сериализованным `HookPost`; ожидать `HTTP/1.1 204`. Повторить тот же POST с тем же `event_id`: снова получить 204, но увидеть только одно событие на стороне hub.
5. Отправить другой POST с теми же естественными полями, но новым `event_id`: событие должно быть принято отдельно.
6. Проверить негативные случаи: неверный secret → 401/rejected до Register; строка agent больше 1 MiB → закрытие соединения; неизвестные `v`/`type` → контролируемая ошибка без panic.
7. Для внешнего bind явно задать `CCTG_AGENT_LISTEN=<ip>:<port>` и/или `CCTG_HOOK_LISTEN=<ip>:<port>`; в логах должно появиться только предупреждение о plain TCP, без секретов.
