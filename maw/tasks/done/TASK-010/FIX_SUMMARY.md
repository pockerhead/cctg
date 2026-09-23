# FIX SUMMARY — TASK-010

## Fixed

- **I1 (major) — потеря частично прочитанной строки в `select!`.** На hub и agent чтение вынесено в отдельную задачу, владеющую read half: она завершает bounded `wire::read_line`, декодирует кадр и передаёт результат основному циклу через bounded `mpsc`. Основной цикл теперь выбирает только между cancel-safe `mpsc::recv()` и outbound. `wire::read_line` сохраняет partial buffer после отмены, ограничивает следующий read остатком `MAX_LINE - buf.len()` и требует очистки только после обработки полной строки. Все call sites переведены на новый явный контракт. Добавлены hub-side и agent-side split-line regression tests, а также прямой тест отмены `read_line`.
- **I2 — неточная граница HTTP head.** Чтение заголовка ограничено `MAX_HEAD + 4` байтами вместе с терминатором, а `head_end > MAX_HEAD` даёт 431. Boundary test принимает head ровно `MAX_HEAD` и отвергает `MAX_HEAD + 1`.
- **I3 — повторный `Authorization`.** Parser хранит факт первого заголовка и возвращает 400 на любой второй `Authorization`, независимо от порядка верного и неверного bearer. Тест покрывает исходный bypass-порядок «неверный, затем верный» и обратный порядок.
- **I4 — возможная взаимная блокировка на одновременной записи.** Reader tasks продолжают дренировать вход независимо от outbound write. Все production-записи agent↔hub (hello/register, registered/rejected и обычные кадры) дополнительно ограничены пятисекундным write timeout; timeout разрывает link/reconnect path вместо вечного ожидания.
- **I5 — reconnect после закрытия outbox.** Reconnect loop проверяет закрытие outbox до новой попытки и после неудачной попытки. Regression test закрывает sender при недоступном hub и подтверждает завершение task без следующего подключения.

Красный baseline был зафиксирован до функциональных правок собственными crate-тестами: оба split-line теста завершались timeout, тест cancel-safe `read_line` терял первую половину строки, exact-header boundary отвергался, а task с закрытым outbox продолжал жить.

## Skipped

- Ни один из findings I1–I5 не пропущен: все подтверждены по текущему коду и исправлены.
- Буквальный вариант I1 «только убрать `buf.clear()`» не применялся изолированно: он ломал существующий контракт последовательных call sites и всё равно оставлял race завершённого, но не выбранного future внутри `select!`. Вместо этого обновлены все call sites и добавлены требуемые reader tasks.
- Три пункта из раздела review **Nits** не менялись: они прямо обозначены как ожидаемое/безвредное поведение и не относятся к I1–I5; такие правки вышли бы за хирургический scope fixer-а.

## Test results

- `cargo test -p cctg --offline --lib split_ -- --nocapture` до исправления: **FAILED**, 2 regression tests failed; после исправления: **ok**, 3 passed, 0 failed.
- `cargo test -p cctg --offline --lib a_cancelled_read_keeps_its_partial_line -- --nocapture` до исправления: **FAILED** (первая половина потеряна); после: **ok**, 1 passed.
- `cargo test -p cctg --offline --lib the_header_limit_is_exact -- --nocapture` до исправления: **FAILED**; после: **ok**, 1 passed.
- `cargo test -p cctg --offline --lib dropping_the_outbox_stops_reconnects_while_the_hub_is_unreachable -- --nocapture` до исправления: **FAILED** (timeout ожидания завершения task); после: **ok**, 1 passed.
- `cargo test -p cctg --offline --lib bad_requests_get_errors_and_no_event -- --nocapture`: **ok**, 1 passed.
- `cargo test --workspace --offline`: **ok**, 189 passed, 0 failed, 1 existing ignored.
- `cargo clippy --workspace --all-targets --offline -- -D warnings`: **ok**, 0 warnings.
- `cargo fmt --all -- --check`: **ok**.
- `cargo build --workspace --offline`: **ok**.
- `git diff --check`: **ok** (только предупреждения Git о будущей CRLF-конвертации рабочей копии).

Секреты не добавлялись в логи или тексты ошибок; отдельный `ingress_logs` test binary прошёл в составе workspace suite.
