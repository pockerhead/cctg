# FIX SUMMARY — TASK-006

## Fixed

- **Minor 1 — compact summary отображался в brief как пользовательский prompt.** Подтверждено в `crates/transcript/src/render.rs`: строка `This session is being continued from a previous conversation...` не входила в `SERVICE_PREFIXES`, поэтому классифицировалась как `Prompt` и становилась границей для `tool_after`. Префикс добавлен к service-записям: summary теперь скрыт в brief, виден в full и не меняет in-progress state. Добавлена обезличенная real-shape фикстура `compact_summary.jsonl` и тест, одновременно проверяющий все три свойства.
- **Missing coverage — `>` внутри значения channel-атрибута.** Замечание подтвердилось: `split_once('>')` принимал `>` внутри кавычек за конец открывающего тега. Добавлен небольшой quote-aware разбор открывающего `<channel ...>`; тест с `user="a>b"` проверяет правильное извлечение тела в brief и full.
- **Missing coverage — многострочный prompt.** Добавлен точный тест текущей согласованной с финальным планом формы `> first line\nsecond line`. Дефекта код не выявил, поэтому поведение не менялось.
- **Missing coverage — CRLF около границы 4096.** Добавлен тест с `\r\n`, начинающимся после 4095 UTF-16 units. Он подтверждает, что splitter не режет CRLF, сохраняет вход побайтно и соблюдает лимит.
- Новая фикстура включена в общие проверки приватности, JSON-валидности и отсутствия утечки thinking.

## Skipped

- **Minor 2 — использование стандартного `repo/target`.** Не является дефектом: оркестратор явно разрешил его для реализации и потребовал использовать default target dir при этой проверке. Код и артефакты не менялись.
- **Nit: `← error` заменить на `← Bash (error)`.** Технически имя можно надёжно получить из существующей карты `tool_use_id -> name` (исходное опасение о неверной привязке при проверке не подтвердилось), но текущая метка прямо соответствует финальному плану и существующему контрактному тесту. Функционального дефекта нет; вне хирургического объёма фикса.
- **Nit: префиксовать `> ` каждую строку многострочного prompt.** Финальный план задаёт форму `> text`; добавленный тест фиксирует именно её. Это визуальное предпочтение, не ошибка.
- **Nit: второй вызов `user_text` в pre-pass.** Он остаётся O(n), покрыт тестом линейного времени и не создаёт неверного результата. Рефакторинг без исправления дефекта пропущен.
- **Nit: `scratch/crev/probe/`.** Это свидетельство ревью, а не production-код; запуск авторского probe не использовался как независимая проверка и удаление не требовалось.

## Test results

- Baseline до правок: `cargo test --workspace` — exit 0; 54 теста пройдены (2 cctg, 10 parse fixtures, 15 parse tolerance, 3 purity, 11 render, 13 split).
- Целевые проверки после правок:
  - `cargo test -p transcript --test render` — exit 0; 14 passed, 0 failed.
  - `cargo test -p transcript --test split` — exit 0; 14 passed, 0 failed.
  - `cargo test -p transcript --test parse_fixtures` — exit 0; 10 passed, 0 failed.
- Финальная обязательная проверка:
  - `cargo fmt --all --check` — exit 0, без изменений форматирования.
  - `cargo clippy --workspace --all-targets -- -D warnings` — exit 0, warnings отсутствуют.
  - `cargo test --workspace` — exit 0; 58 passed, 0 failed (2 cctg, 10 parse fixtures, 15 parse tolerance, 3 purity, 14 render, 14 split; doc-tests: 0).
- `git diff --check` — exit 0; только предупреждения Git о будущей LF→CRLF нормализации при `core.autocrlf`, whitespace errors отсутствуют.
