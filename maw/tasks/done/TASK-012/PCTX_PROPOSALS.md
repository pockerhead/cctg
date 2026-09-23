# PCTX proposals — TASK-012 (planner)

## 2026-09-23 — hooks: внутренние агенты не оставляют файлов субагента

Что: в домен hooks добавить факт. У всех 14 шумовых `SubagentStop` из TASK-003 (`agent_type: ""`) нет ни `subagents/agent-<id>.jsonl`, ни `.meta.json` рядом с `agent_transcript_path`; у настоящего Explore есть оба. Проверено 2026-09-23 по `capture_unknown.jsonl` и диску (`scratch/planner/probe_internal_agents.out`).
Зачем: это локальный признак внутреннего агента, который работает и для случая `agent_type` = имя `--agent` сессии, без состояния SubagentStart. TASK-012 использует его как второй фильтр после `agent_type != ""`.

## 2026-09-23 — hooks/hub: connect на закрытый локальный порт в Windows длится до таймаута

Что: риск-урок. На Windows `connect` к 127.0.0.1 на порт без слушателя не падает сразу: стек повторяет SYN после RST, и вызов длится до таймаута клиента (замер: хук с таймаутом 1 с тратил 1027 мс, с 500 мс тратит 529 мс). Значит остановленный hub стоит каждому хуку ровно его таймаут.
Зачем: любой будущий клиент hub (agent TASK-013, CLI) не должен рассчитывать на мгновенный ECONNREFUSED на Windows.

## 2026-09-23 — hooks: формат регистрации

Что: доки Claude Code (code.claude.com/docs/en/hooks, 2026-09-23) описывают exec form (`command` + `args`, без шелла, `command` ищется в PATH) и `shell: "powershell"`. Stdout хука `SessionStart` и `UserPromptSubmit` при exit 0 становится контекстом Claude. Установка cctg пока использует shell form `cctg hook <Event>` (через Git Bash, как в TASK-003); exec form не проверена на Windows.
Зачем: чтобы следующая задача не переоткрывала варианты регистрации и помнила про stdout.

## 2026-09-23 — hooks (plan-reviewer-2): CLAUDE_PID ниже первого claude.exe

Что: уточнить контракт вложенности. Если предок с pid == `CLAUDE_PID` называется `node(.exe)` и стоит ближе первого `claude(.exe)`, это собственная сессия (npm-claude, запущенный из нативной сессии), а тот `claude.exe` выше это родитель. Без этого SessionEnd несёт pid родителя. Родитель по-прежнему ищется только по имени `claude`, npm-only родитель не распознаётся (Node вне поддерживаемой установки).
Зачем: правило "пропустить первый claude.exe с pid == CLAUDE_PID" молчит про случай, когда own не называется claude. Доказательство: `scratch/reviewer2/repro_before_fix.out`, `mutations.out.txt`.

## 2026-09-23 — hooks (qa): цепочку рвёт exec в MSYS, не только короткоживущий wrapper

Что: уточнить известную дыру детекта вложенности. Если Git Bash процесс, у которого родитель тоже MSYS/Cygwin процесс, выполняет одну нативную команду последней (`bash -c 'claude -p ...'`, `env X=1 claude -p ...`), MSYS делает exec: у нативного ребёнка остаётся ppid промежуточного процесса, а тот уже завершился. Обход дерева останавливается, вложенный запуск выглядит top-level и получит свою тему. Если родитель bash нативный (claude.exe запускает Bash-тул), stub остаётся живым, и цепочки TASK-003 целые. Проверено QA TASK-012 на живом дереве (`scratch/qa/chain.ps1`).
Зачем: runner-ы, которые оборачивают `claude -p` в `bash -c` или `env`, нужно проверять отдельно (maw runner). Для надёжности нужен сигнал, который не зависит от живых промежуточных процессов.

## 2026-09-23 — hooks (qa): на Windows имя образа `claude.exe` носит и Claude Desktop

Что: факт. Claude Desktop из Store (`\WindowsApps\Claude_*\app\claude.exe`, Electron, main плюс около 12 дочерних) и classic install (`\AnthropicClaude\`) тоже называются `claude.exe`. CLI, который поставляет Desktop, лежит в `%APPDATA%\Claude\claude-code\<version>\claude.exe`. TASK-012 отличает CLI от Desktop по полному пути образа и принимает звено цепочки, только если родитель создан строго раньше ребёнка.
Зачем: любой будущий код, который ищет Claude Code по имени процесса, должен проверять путь образа.

> RESOLVED: all six folded into domains/hooks.md on 2026-09-23 (implementation line, lineage rule, internal-agent filter, three risk lessons).
