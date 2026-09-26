# TASK-056: rich built-in status line out of the box

Type: feature
Mode: small-fix
Priority: high
Branch: feature/builtin-statusline
Domains: hooks

## Description
Запрос пользователя 2026-09-26: на свежей машине в терминале не видно контекста и прочего. У пользователя на Windows своя строка `~/.claude/statusline.py` (Python), которую `cctg statusline` (TASK-029) вызывает цепочкой, если она есть в `~/.claude/settings.json`; на новой машине её нет, и остаётся короткая `own_line` (`Opus · ctx 50% · 5h 3% · 7d 92%`). Хочется, чтобы cctg ставил «нашу» строку сам и обновлял её вместе с бинарником.

Решение оркестратора: не копировать Python-скрипт (на машине может не быть Python), а сделать `own_line` в Rust такой же, как `statusline.py` пользователя, тогда она ставится и обновляется вместе с `cctg`:
- строка 1: `**model** [effort] br:<branch>[*] dir:<basename cwd> ctx:NN%` (жирная модель, ветка голубым; `*` если `git status --porcelain` не пуст; git с `--no-optional-locks`, короткий таймаут, без git просто без ветки);
- строка 2: `acc:<email> 5h:NN% 7d:NN%`, проценты жёлтые от 80, красные от 95; email из `oauthAccount.emailAddress` в `~/.claude.json` (или `$CLAUDE_CONFIG_DIR/.claude.json`) только для вывода в терминал, никогда не в hub и не в логи;
- `sh:<shell>` из statusline.py не брать.
Цепочка на пользовательский `statusLine.command` остаётся как есть (у кого своя строка, тот видит свою).

Также выяснить, почему на свежей машине не было даже `own_line` (проверить, что `install.sh` пишет `statusLine` в `~/.cctg/claude/settings.json` на Linux/macOS/Windows и что обёртка передаёт `--settings`); если найдётся дыра, закрыть её здесь.

## Acceptance criteria
- [ ] `own_line` выдаёт две строки в формате выше (unit-тесты на входной JSON, с веткой и без git, пороги цветов)
- [ ] email не уходит в hub (тест на тело POST в statusline_cli) и не пишется в логи
- [ ] время работы `cctg statusline` без пользовательской команды остаётся коротким (git с таймаутом)
- [ ] install_e2e подтверждает `statusLine` в настройках клиента
- [ ] Existing tests pass
