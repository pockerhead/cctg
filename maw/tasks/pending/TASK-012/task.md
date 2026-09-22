# TASK-012: hook — lifecycle events and subagent report capture

Type: feature
Mode: full
Priority: high
Branch: feature/hook-subcommand
Domains: hooks

## Description
`cctg hook <event>` читает stdin, десериализует только нужные поля и делает один аутентифицированный HTTP POST с коротким таймаутом, всегда завершаясь `exit 0`. Поддержать шесть событий: `SessionStart`, `SessionEnd`, `Stop`, `UserPromptSubmit`, `SubagentStart`, `SubagentStop`. Дополнительно — узкий `PreToolUse`/`PostToolUse` matcher на `SubagentHandback`, который передаёт `tool_input.message` как отчёт субагента (документировано для 2.1.271+, см. раздел 0.1), плюс `agent_transcript_path` и `last_assistant_message` из `SubagentStop`. TASK-003 подтверждает payload на текущей версии, но механизм не гейтится на спайк. Признак вложенности вычисляется по зафиксированному в TASK-003 правилу. Критично по таймингу: у `SessionEnd`-хуков общий бюджет **1.5 секунды** (документировано), у `UserPromptSubmit` — 30 с, у `Stop` и обычных command-хуков — 600 с. Значит POST для `SessionEnd` должен иметь таймаут заведомо меньше 1.5 с, иначе hook будет убит и событие смерти сессии потеряется. Snippet регистрации пишется в user scope, без машинно-специфичных секретов.

## Dependencies
- blocked by TASK-010 — hard prerequisite
- blocked by TASK-011 — hard prerequisite
- prefer after TASK-003 — soft ordering

## Acceptance criteria
- [ ] каждое из шести событий даёт ожидаемый HTTP-пейлоад с нужными полями и ничем лишним
- [ ] недоступный hub: `exit 0` в пределах настроенного короткого таймаута, stdout пуст, в stderr нет ни входного пейлоада, ни секретов
- [ ] признак вложенности и parent id соответствуют правилу, зафиксированному в TASK-003, для top-level и nested случаев
- [ ] битый, пустой или обрезанный stdin даёт `exit 0` без паники
- [ ] `SessionEnd` измерен и укладывается заведомо в 1.5 с даже при недоступном hub (таймаут POST выставлен с запасом под этот бюджет)
- [ ] snippet настроек регистрирует все события и не содержит секретов и машинно-специфичных путей
- [ ] отчёт субагента захватывается matcher-ом на `SubagentHandback` (`tool_input.message`), а `SubagentStop` передаёт `agent_transcript_path`, `agent_type`, `agent_id`, `last_assistant_message`; события с пустым `agent_type` или с `agent_type`, равным имени `--agent` сессии без соответствующего SubagentStart, отбрасываются как внутренние
- [ ] `source` читается только из `SessionStart` (для остальных событий он не документирован) и его отсутствие не считается ошибкой
- [ ] Existing tests pass
