# TASK-004: Spike — development channel lifecycle and launch ergonomics

Type: chore
Mode: small-fix
Priority: high
Branch: chore/spike-channel-lifecycle
Domains: channel

## Description
На минимальном временном stdio JSON-RPC probe (Rust или любой не-Node процесс) проверить четыре режима запуска: fresh с `--dangerously-load-development-channels server:probe`, `--resume`, `--continue`, и запуск без флага. Отдельно проверить user-scope запись сервера в `~/.claude.json` из новой папки — требуется ли consent. Зафиксировать наблюдением: баннер, вывод `/mcp`, факт спавна сервера, доставку inbound `notifications/claude/channel`, приход `permission_request`. Решение пользователя: обёртки `cctg run` не будет — фиксируется точная команда запуска и пример shell alias. Probe и временный конфиг удалить.

## Dependencies
- unblocks TASK-011 — waits on this task
- unblocks TASK-013 — waits on this task

## Acceptance criteria
- [ ] findings содержат таблицу 4 режима запуска × (баннер / `/mcp` / спавн сервера / inbound доставлен / permission_request получен)
- [ ] поведение при `--resume` и `--continue` **наблюдалось**, а не выведено из документации; если канал при resume не поднимается, это записано как факт с последствием для hub
- [ ] подтверждено или опровергнуто, что user-scope сервер в `~/.claude.json` не требует per-project consent
- [ ] проверено, что происходит при вложенном `claude -p`: документация говорит, что в non-interactive режиме project-scope сервер грузится **без** запроса, значит вложенный запуск может тихо поднять наш сервер — надо убедиться, что он не превращается в самостоятельную маршрутизируемую регистрацию
- [ ] записана точная MVP-команда запуска и пример alias; отсутствие обёртки зафиксировано как принятое решение
- [ ] записано наблюдаемое поведение при отсутствии флага (тихий drop уведомлений), потому что от него зависит состояние «нет канала» в hub
- [ ] probe-процесс и временная конфигурация удалены, секретов не оставлено
- [ ] Existing tests pass
