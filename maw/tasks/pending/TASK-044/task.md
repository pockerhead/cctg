# TASK-044: console control on Linux and WSL clients

Type: feature
Mode: full
Priority: medium
Branch: feature/linux-console
Domains: hub, channel

## Description
Решение пользователя 2026-09-25: клиент cctg должен работать в Linux и WSL так же, как в Windows. Сейчас всё управление консолью claude сделано через Windows-консоль (`keys.rs`: `AttachConsole` + `WriteConsoleInputW` + чтение экрана), и на других ОС агент эти возможности не объявляет. Не работают: ⏹ прерывание хода (Esc), команды `!` и неизвестные `/` из темы (TASK-043), чтение и закрытие панелей `/cost` и `/usage`, перезапуск claude по «Обновить» (TASK-040, безопасный `/exit` с чтением поля ввода); `cctg run` и диалог development channels при `--resume`.

Сделать тот же набор на Linux: ввод клавиш в терминал claude и чтение его экрана. Варианты, выбрать и обосновать: (а) если claude запущен в tmux, `tmux send-keys` и `tmux capture-pane` по панели этого процесса; (б) `cctg run` сам держит pty (claude работает внутри псевдотерминала, `cctg run` прокидывает ввод и вывод пользователя и параллельно ведёт копию экрана через эмулятор терминала, например `vt100`) и принимает клавиши от агента по локальному каналу; (в) оба, с выбором по окружению. `cctg run` должен остаться тупым (принцип TASK-040): никакой логики версий, сети и протокола хаба. Проверки поля ввода (глиф `❯`/`>` и U+00A0, черновик, панель `▔`) общие с Windows.

## Dependencies
- blocked by TASK-035 — Linux CI appears there; this task needs Linux runs of its tests

## Acceptance criteria
- [ ] на Linux агент объявляет console_keys и console_commands, если управление доступно, и не объявляет, если нет (понятный ответ в теме, как сейчас)
- [ ] ⏹, `!`/`/` команды с проверкой черновика, чтение и закрытие панелей и перезапуск по «Обновить» работают на Linux (e2e на Linux-раннере с поддельным claude, как run_e2e)
- [ ] `cctg run` на Linux запускает claude в том же терминале, перезапускает по заявке и отвечает на диалог каналов; Ctrl+C, изменение размера окна и выход работают как без cctg
- [ ] WSL проверен как Linux; Windows-поведение не меняется
- [ ] Existing tests pass
