# TASK-060: flaky tests on Windows CI

Type: bug
Mode: small-fix
Priority: medium
Branch: fix/flaky-tests
Domains: hub

## Description
Windows CI 2026-09-26 падал на тестах, которых изменения не трогали; повтор job зелёный:
- `question_hook_e2e::the_terminal_button_gives_no_decision_at_once` (run 36246604360): ждал 60 с, нажатие «В терминале» сразу после появления вопроса потерялось (гонка регистрации ожидающего вопроса и нажатия?).
- `hub::slots::tests::three_explicit_subagents_make_three_blocks_and_internal_agents_none` (run 36248773154): лишний `Send "✓ Explore … закончил"` (8 операций вместо 7), зависит от времени.
Локально под нагрузкой также падают по порогам времени `hook_cli::every_event_reaches_the_hub`, `spool_e2e` (took < 1500ms), `statusline_cli`.

Найти настоящую причину каждого (гонка в коде или в тесте), исправить без увеличения таймаутов вслепую; где порог времени неизбежен, опираться на paused time или события, а не на часы.

## Acceptance criteria
- [ ] Для каждого теста названа причина и исправлена (код или тест)
- [ ] 20 прогонов подряд каждого теста зелёные локально под нагрузкой (`-j 1` параллельно с другой сборкой)
- [ ] Existing tests pass
