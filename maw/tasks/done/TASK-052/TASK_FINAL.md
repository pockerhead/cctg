# TASK-052: installer PATH and host name; tell when the channel is off

Type: bugfix
Mode: small-fix
Priority: high
Branch: fix/install-path-channel-off
Domains: hub, channel

## Description
Найдено вживую 2026-09-25/26 на Mac и в Linux-контейнере.

1. `install.sh`: если `~/.local/bin` нет в `PATH`, предложить (с `--yes` без вопроса) добавить `export PATH="$HOME/.local/bin:$PATH"` в rc-файл shell пользователя: `~/.zshrc` для zsh (по умолчанию на macOS), `~/.bashrc` для bash, иначе `~/.profile`. Строка с меткой `# cctg`, не дублируется при повторном запуске, `--uninstall` её убирает. Windows (Git Bash) — `~/.bashrc`.
2. `install.sh`: в Docker-контейнере (есть `/.dockerenv` или cgroup/`container` признак) hostname — это id контейнера; установщик пишет `CCTG_HOST` (флаг `--host NAME`, иначе спрашивает с разумным предложением, с `--yes` берёт имя хоста контейнера из `CCTG_HOST` окружения или оставляет как есть с предупреждением). macOS-ветка с `CCTG_HOST` остаётся.
3. hub: сообщение отдано агенту живой сессии, но за N секунд (например 20) от сессии не пришло ни `UserPromptSubmit`, ни записи канала в транскрипте, ни хода — в тему один раз уходит понятное уведомление: канал в этой сессии, похоже, не включён (claude запущен без флага или диалог development channels не подтверждён); что сделать (`/exit`, `claude-cctg --continue`, подтвердить диалог). Одно уведомление на сессию до следующего успешного приёма; без ложных срабатываний во время идущего хода (сообщение ждёт в очереди Claude Code до конца хода).

## Acceptance criteria
- [ ] PATH-строка добавляется один раз в правильный rc, `--uninstall` её убирает (install_e2e)
- [ ] в контейнере установщик задаёт CCTG_HOST (тест с фейковым признаком контейнера)
- [ ] hub шлёт одно уведомление «канал не включён», когда сообщение не принято; не шлёт во время хода и после успешного приёма (тест фейковым линком)
- [ ] Existing tests pass
