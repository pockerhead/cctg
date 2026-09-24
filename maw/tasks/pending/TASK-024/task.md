# TASK-024: hub — stream edge cases after refusals (rotation, unpaired timeout)

Type: fix
Mode: small-fix
Priority: low
Branch: fix/stream-rotation-drain
Domains: hub

## Description
Остатки TASK-016/023, найденные ревью и QA (см. `maw/tasks/done/TASK-023/IMPL_SUMMARY.md` раздел 5, `scratch/implementer/rotation_probe.*`, `QA_REPORT_2.md`). Потерь в обычной работе нет, всё это про отказы Telegram (5xx, сеть) в неудачный момент.
1. Сессия заканчивается (или слот уходит новой сессии после `/clear`), пока её стрим ждёт rewind после отказа: `pump_streams` выкидывает `Live`, отказанные строки старой сессии не доставляются никогда. Нужно дочитать стрим закончившейся сессии до разделителя новой (или доставить отказанное до него).
2. Ответ Stop, отпущенный по таймауту `hold_answer` до спаривания со своим концом хода (`end: None`), может сдвигать порядок следующих ответов ход за ходом. Учесть, что пользовательский Stop hook с `stop_hook_active` даёт несколько Stop на ход.
3. При reset транскрипта (`read_at = 0`) `answered_ends` не очищается.

## Dependencies
- blocked by TASK-023 — hard prerequisite

## Acceptance criteria
- [ ] отказ Telegram во время конца сессии или ротации слота не теряет строки старой сессии, и они приходят до разделителя новой (e2e через `serve_agents`)
- [ ] ответ, ушедший по таймауту без пары, не сдвигает порядок следующих ответов (тест), либо записано обоснованное ограничение
- [ ] reset транскрипта сбрасывает отметки отвеченных концов хода
- [ ] Existing tests pass
