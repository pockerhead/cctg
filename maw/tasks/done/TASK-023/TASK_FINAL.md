# TASK-023: hub — Stop answer waits behind a broken stream topic

Type: fix
Mode: small-fix
Priority: medium
Branch: fix/answer-after-refused-lines
Domains: hub

## Description
Найдено QA TASK-016 (раунд 2, `maw/tasks/done/TASK-016/QA_REPORT_2.md`, BUG-1, тест `qa2_answer_after_a_refused_line_still_follows_its_lines` в `scratch/qa/round2/e2e`): когда Telegram отказывает stream-сообщению с 5xx или сетевой ошибкой, тема помечается `broken`, и строки вызовов ждут rewind. Удержанный ответ `Stop` уходит обычным `Op::Send`, этот гейт его не держит, поэтому в теме ответ хода появляется раньше строк вызовов своего хода. Потерь нет. Ответ должен уходить только после строк своего хода, в пределах существующего `hold_answer`, либо через тот же `broken`-гейт темы. Заодно проверить вывод QA, полученный только чтением кода: отказ Telegram в момент ротации слота может оставить строки старой сессии недоставленными (если подтвердится, чинить здесь же или завести отдельную задачу). И сделать детерминированным тест `a_new_agent_process_goes_on_from_the_stream_position` (мутация R11 выживала в 1 прогоне из 3).

## Dependencies
- blocked by TASK-016 — hard prerequisite

## Acceptance criteria
- [ ] после отказа stream-сообщения ответ Stop хода приходит в тему после всех строк вызовов этого хода (e2e через настоящий `serve_agents`)
- [ ] ответ не задерживается больше, чем `hold_answer` плюс один цикл rewind, и не теряется
- [ ] проверен сценарий отказа во время ротации слота; результат записан (тест или обоснование)
- [ ] `a_new_agent_process_goes_on_from_the_stream_position` не зависит от тайминга
- [ ] Existing tests pass
