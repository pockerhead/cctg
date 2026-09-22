# TASK-019: headless resume and context handoff

Type: feature
Mode: full
Priority: low
Branch: feature/headless-resume
Domains: hub, hooks

## Description
Post-MVP (step 6 of the development order). Вне MVP (шаги 1–4), но требуется решением пользователя о модели слотов, поэтому зафиксировано здесь. Кнопка Resume запускает через агента устройства `claude -p --resume <id> --output-format stream-json`; оживший процесс занимает тот же слот. Раздутый контекст лечится handoff-ом: старой сессии заказывается summary (`claude -p --resume <old>`), новая сессия стартует в том же слоте с этим summary первым промптом, разделитель помечается как `handoff`. Для headless-resumed сессий выставляется `CLAUDE_AUTOCOMPACT_PCT_OVERRIDE`. Inline `/compact` в headless недоступен — это проверено пользователем и не подлежит перепроверке.

## Dependencies
- blocked by TASK-017 — hard prerequisite
- blocked by TASK-018 — hard prerequisite

## Acceptance criteria
- [ ] Resume поднимает сессию через агента нужного устройства и привязывает её к тому же слоту без новой темы
- [ ] буфер мёртвого слота доставляется в поднятую сессию в исходном порядке
- [ ] handoff создаёт новую сессию в том же слоте с summary первым промптом и разделителем `handoff`
- [ ] `CLAUDE_AUTOCOMPACT_PCT_OVERRIDE` выставляется только для headless-resumed процессов и записан в заметках задачи с фактическим значением
- [ ] сбой запуска даёт понятное сообщение в теме, слот не теряет состояние, повторное нажатие не плодит процессы
- [ ] Existing tests pass
