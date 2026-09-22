# FIX SUMMARY — TASK-007

## Fixed

- Review: `agent_id` попадал в однострочный заголовок без нормализации. `agent_header` теперь пропускает отображаемый ID через существующий `one_line`. Исходный ID в `Subagent` не меняется, поэтому lookup по ID из родительского `tool_result` продолжает работать. Добавлен fixture-based тест с ID, содержащим перевод строки, который проверяет и заголовок, и сохранение привязки к родителю.
- Missing coverage: background `Agent` с ещё работающим субагентом. Добавлен тест на основе async-вызова из `tool_use_result.jsonl` и незавершённого среза `sidechain.jsonl`; тело блока проверяется как `SubagentBody::InProgress` и встраивается под родительский заголовок.
- Missing coverage: повторяющиеся `agent_id` в `&[Subagent]`. Текущее поведение проверено как детерминированное: последний элемент с данным ID побеждает при сборке `HashMap`. Добавлен тест обоих порядков входного slice, закрепляющий правило `last wins`; изменение реализации не потребовалось.

## Skipped

- Full-mode строка `← Agent: ...` не подавлялась. Это намеренное решение плана: родительский `tool_result` остаётся частью полного transcript. Буквальное применение предложения review изменило бы совместимость `render_full` и скрыло бы исходные родительские данные.
- Trade-off без `last_assistant_message` не менялся. План и domain-контекст прямо допускают, что отстающий transcript без hook-данных может выглядеть завершённым; устранение требует данных hub/hook, которых этот чистый crate не имеет.
- Пустой `agent_id` отдельно не тестировался. Review помечает случай как harmless, а реальные ID приходят из hooks/jsonl в формате `a` + hex. Изменение поведения пустого ID не требуется спецификацией.
- `indent` по-прежнему превращает пустую строку тела в два пробела. Это косметический trailing whitespace без изменения содержимого; `split_for_telegram` отбрасывает whitespace-only chunks, и review не связывает это с нарушением acceptance criteria.
- Размеры реальных блоков свыше Telegram 4096 не менялись. По плану это ответственность hub: применить `split_for_telegram` либо отправить файл; обрезка тела в transcript crate была бы изменением согласованного поведения.

## Test results

- `cargo test -p transcript --test subagent` — exit 0; `14 passed; 0 failed`.
- `cargo fmt --all -- --check` — exit 0; output empty (formatting clean).
- `cargo clippy --workspace --all-targets -- -D warnings` — exit 0; `Finished dev profile`, warnings отсутствуют.
- `cargo test --workspace` — exit 0; 73 passed, 0 failed: cctg 2, parse fixtures 10, tolerance 15, purity 3, render 14, split 14, subagent 14, doc-tests 1.
