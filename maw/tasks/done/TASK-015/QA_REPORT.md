# TASK-015 QA REPORT

Verdict: **SHIP**

## 0. Disconfirmation

Самый конкретный контрпример, который сломал бы задачу: хуки субагента приходят раньше, чем транскрипт родителя получает вызов `Agent`, а строка результата одного вызова ещё дописывается (нет `\n`). Тогда либо блока нет совсем (кандидат не перечитывается), либо недописанная строка читается и offset уходит за неё, и результат теряется навсегда.

Проверил в e2e-тесте (шаг "The parent transcript catches up"): три `SubagentStart` приходят при пустом транскрипте, 400 мс блоков нет; потом дописываются вызовы, у A3 строка результата без перевода строки. Пришло ровно 2 блока, 300 мс спустя всё ещё 2; после дописанного `\n` появился третий. **Не подтвердился**: код правильно перечитывает транскрипт и не трогает незаконченную строку.

## 1. Environment

- Прямой прогон cargo, без docker и без живого Telegram. Один `CARGO_TARGET_DIR=%TEMP%/cctg-t015-qa-target`, `CARGO_PROFILE_DEV_DEBUG=0`, `-j 1`, cargo по одному. Каталог удалён.
- `.env`, `device.env`, `~/.claude.json` не читались и не менялись. Интерактивный claude и окна не запускались.
- Скрипты: `scratch/qa/run_suite.sh` (fmt, clippy, workspace), `scratch/qa/run_e2e.sh [none|ghost|target]` (копирует `scratch/qa/qa_e2e.rs` в `crates/cctg/tests/` на время прогона, применяет мутацию, потом всё возвращает; `git status` после запуска чистый).

## 2. Test results

Существующие проверки (`scratch/qa/*.out.txt`):
- `cargo fmt --all -- --check`: exit 0.
- `cargo clippy -j 1 --workspace --all-targets -- -D warnings`: exit 0.
- `cargo test -j 1 --workspace --no-fail-fast`: exit 0. cctg lib 315 passed / 1 ignored (ignored был до задачи), все интеграционные бинарники и transcript ok (subagent 15 passed).

Новый тест `scratch/qa/qa_e2e.rs::qa_subagents_nested_reply_and_restart_end_to_end` (`e2e_none.out.txt`, ok, 8.8 s):
- Хуки идут настоящим путём: stdin в формате Claude Code -> `hook::build` (с настоящим фильтром файлов агента) -> `hook::post` по HTTP с Bearer-секретом -> `ingress::serve_hooks` -> настоящий `Slots`.
- Агенты подключаются по TCP к `ingress::serve_agents` (hello + register).
- `Scheduler` настоящий, Telegram заменён фейком. Обезличенные jsonl родителя и субагентов лежат во временном каталоге.
- После этого актор останавливается, и новый поднимается из сохранённого `registry.json`.

Мутации, доказывающие, что тест не пустой:
- `ghost` (в `match_candidates` кандидат без найденного вызова `Agent` всё равно подтверждается): тест падает на строке 527, блок-призрак до корреляции. KILLED (`e2e_ghost.out.txt`).
- `target` (убрана вставка `target_agent`): тест падает на строке 704, `left: None, right: Some("a2000000000000002")`. KILLED (`e2e_target.out.txt`).

## 3. Acceptance criteria

| Критерий | Проверка | Результат |
|---|---|---|
| 3 явных субагента: одна тема, ровно 3 блока | e2e: A1..A3 через хуки, после окна 3.5 s ровно 3 send `↳`, все в теме 101. `createForumTopic` 2: родитель плюс посторонняя сессия Q, лишних нет | PASS |
| Нет призраков (internal, `--agent`) | e2e: typed `my-agent` с файлами проходит хук, hub его отбрасывает; `agent_type:""` пропускает хук, а сырой POST отбрасывает hub; субагент nested run тоже отброшен. Id ни одного из них нет ни в одной операции Telegram и нет в `registry.json` | PASS |
| Отчёт handback и весь fallback, включая отстающий файл | e2e: A1 показывает `REPORT-A1` (не last message); A2 показывает brief готового транскрипта (`• Bash: ...` + `FINAL-A2`); A3 с отстающим файлом показывает `LAST-A3`. Блоки редактируются на месте. Юнит-тесты fallback в наборе проходят | PASS |
| Nested `claude -p`: 0 тем, ровно 1 блок `⇣ nested` | e2e: nested SessionStart (lineage 20->10) даёт 0 новых тем и один `⇣ nested 5e551017` в теме родителя. После Stop и SessionEnd блок отредактирован на `NESTED-ANSWER`, отдельным сообщением ответ не ушёл | PASS |
| Reply на блок только в канал родителя, с валидным `target_agent` | e2e: reply на блок A2 доходит до агента P с `target_agent=a2000000000000002`. Reply на nested-блок и обычное сообщение приходят без него. Тот же `reply_to` в чужой теме Q даёт сообщение для Q без `target_agent`. Агент nested run ничего не получил | PASS |
| Рестарт: без дублей тем и блоков, детерминированная пометка | e2e: после перезапуска из `registry.json` 0 createTopic и 0 send. Reply на A2 всё ещё несёт `target_agent`. SessionEnd родителя переводит работающий A4 в `итог не получен` через edit того же message_id | PASS |
| Existing tests pass | workspace, clippy, fmt | PASS |

## 4. Bugs found

Блокирующих нет. Замечания:

1. **minor, поведение, не регрессия.** Повторный `SubagentStop` того же агента без нового handback перетирает показанный отчёт на `last_assistant_message`. Воспроизведение: e2e после рестарта, повторный stop A1 даёт `Edit 1001 -> "...\nLAST-A1"` вместо `REPORT-A1`. Без рестарта результат тот же: `Reports::take` уже забрал отчёт. В жизни второй stop бывает при resume через SendMessage, и тогда новый ответ законно новее. Хук ничего не повторяет сам (`hook::post` одноразовый, ingress дедупит по `event_id`). Reviewer-2 это видел и принял. Ожидалось бы оставить отчёт, если новый last message совпадает со старым; сейчас показывается last message.
2. **nit.** `finish_block` отправляет документ с полным текстом (текст длиннее 4096) сразу, а send самого блока может ещё ждать в registry. Файл тогда придёт раньше блока. Тестом не воспроизводил, видно по коду `slots.rs finish_block`.
3. **nit.** `Subagent::new`: meta с `description: ""` не даёт упасть обратно на описание из вызова (`.or` срабатывает только на `None`). Заголовок тогда без описания. Косметика.

Проверки фиксов из FIX_SUMMARY по коду: удаление `indexes` в `end_blocks` есть; фильтр `running || answer.is_some()` есть; `BodyInput.header` используется в `body_text`; текст `channel.rs` изменён. Все 4 новых теста фиксера входят в прошедшие 315.

## 5. Не проверялось

Живой Telegram и живой Claude Code вне scope: реальные тайминги записи транскрипта родителя относительно хуков, фактический рендер в клиенте Telegram, flood limits на реальной группе.

## 6. Verdict

**SHIP.** Все 7 критериев подтверждены моим собственным сквозным тестом через настоящий ingress хуков и агентов, плюс полный workspace, fmt и clippy. Тест убивает обе мутации. Найденное это одно minor-поведение, которое уже принято на ревью, и два nit.

Cleanup: контейнеров и сервисов не поднималось. `%TEMP%/cctg-t015-qa-target` и временные каталоги `cctg-qa-e2e-*` удалены.
