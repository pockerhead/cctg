# TASK-015 FIX SUMMARY

Preflight. Самое конкретное утверждение review, которое могло сломать код при буквальном исполнении: Issue 1, "удалять `indexes` сессии в `end_blocks`". Проверил по коду: утверждение верное (`grep indexes`: удаление только в `Done::Index`, `slots.rs`), но само по себе удаление ухудшает Issue 4: `read_body` берёт описание из индекса, и SubagentStop после SessionEnd давал бы заголовок без `: <description>`. Поэтому 1 и 4 чинились вместе, одним тестом на актор. Второе место с риском: в Issue 2 review предлагал показывать ответ "при условии, что `message_id` есть". Это условие лишнее: `block_work` сам выбирает send или edit, а с этим условием блок, у которого первый send ещё в пути, потерял бы ответ. Я его не добавлял.

## 1. Fixed

- **Issue 1 (major), `Slots.indexes` без предела.** Подтвердилось. В `end_blocks` (`slots.rs`) после `lose_blocks`: индекс каждой завершённой сессии удаляется, если у неё нет кандидатов и нет идущего скана (`!indexing.contains`, `candidates.of_session(..).is_empty()`). Скан, который закончится позже, удалит индекс через уже существующую проверку в `Done::Index`. Resume той же сессии просто сканирует транскрипт заново с 0. Тест `the_agent_calls_of_an_ended_session_are_forgotten`: у живой сессии индекс остаётся, после SessionEnd `indexes` пуст и остаётся пустым после позднего stop.
- **Issue 2 (minor), поздний ответ nested run после конца родителя.** Выбрал код, а не ограничение (решение в `log.jsonl`). В `end_blocks` фильтр `block.running` стал `block.running || answer.is_some()`: если родитель закончился первым (блок уже `итог не получен`), а nested run потом прислал Stop и SessionEnd, блок редактируется на его ответ. Без ответа блок остаётся "итог не получен". Комментарий `lose_blocks` ("A later result still wins") теперь верен и для nested, doc `end_blocks` обновлён. Тест `a_nested_answer_after_its_parent_ended_still_shows`: один send, блок сначала lost, потом `⇣ nested bbbbbbbb\nlate answer`.
- **Issue 3 (minor), субагенты второго уровня.** Только строка в "Принятых ограничениях" `IMPL_SUMMARY.md`, код не менялся (по scope).
- **Issue 4 (minor), заголовок финального блока без описания.** `BodyInput` получил `header: Option<String>` (сохранённый `block.header`, пустой не передаётся). `body_text` берёт его первой строкой, когда в `.meta.json` нет описания; если описание в meta есть, решает meta, как раньше. Обратный разбор описания из заголовка я отверг: `one_line` обрезает до 120 символов с суффиксом `… [+N chars]`, повторная обрезка меняет текст (решение в `log.jsonl`). Тесты: `without_a_meta_description_the_saved_header_stays` (subagents.rs) и последняя проверка в `the_agent_calls_of_an_ended_session_are_forgotten` (`↳ Explore <id>: one\nLate.` после конца сессии, когда индекса уже нет).
- **Issue 5 (nit), формулировка канала.** `channel.rs` INSTRUCTIONS: "for that running subagent" стало "for that subagent, running or finished". Тест `initialize` проверяет новую фразу.
- **Missing coverage.** Добавлен `a_nested_resume_of_a_top_level_id_makes_no_block_and_no_topic`: две живые top-level сессии, из A запускается `claude -p --resume` с id B (ветка "nested of a known top-level") и с id самой A (ветка "ancestor is this very session"), оба со своим SessionEnd. Итог: ровно 2 createForumTopic, ноль send и edit сообщений. Тест на удаление индекса описан в Issue 1. Тест на Issue 2 выше.

Мутационная проверка (`scratch/fixer/mutations.sh`, вывод `mutations.out.txt`): откат каждой из трёх правок (удаление индекса, фильтр nested, сохранённый заголовок) валит свой новый тест.

## 2. Skipped

- Missing coverage "edit, который Telegram отвергает навсегда, через актор с фейком": по scope оркестратора не входит (низкий приоритет, registry-уровень покрыт).
- Nits `block_work` проходит все блоки при каждом `pump`, предел 256 MiB у `scan`, tombstone при таймауте reqwest: по scope оставлены как есть.

## 3. Test results

Один `CARGO_TARGET_DIR=%TEMP%/cctg-t015-fix-target`, `CARGO_PROFILE_DEV_DEBUG=0`, `-j 1`, по одному cargo, каталог удалён после прогона. Выводы в `scratch/fixer/`.

- `cargo test -j 1 -p cctg --lib hub::`: 231 passed, 1 ignored (было 227, +4 новых).
- `cargo fmt --all --check`: exit 0 (`fmt.out.txt`).
- `cargo clippy -j 1 --workspace --all-targets -- -D warnings`: exit 0 (`clippy.out.txt`).
- `cargo test -j 1 --workspace --no-fail-fast`: exit 0, cctg lib 315 passed / 1 ignored, остальные бинарники ok (`workspace.out.txt`).

Изменённые файлы: `crates/cctg/src/hub/slots.rs`, `crates/cctg/src/hub/subagents.rs`, `crates/cctg/src/channel.rs`, `IMPL_SUMMARY.md` (одна строка ограничений), `log.jsonl` (два `decision`). Не коммитил.
