# PoC: одна сессия Claude Code в теме Telegram

Рецепт живой проверки на одной машине: hub, одна интерактивная сессия Claude Code с каналом `cctg`, сообщения из темы в сессию и ответы обратно. MCP-сервер и хуки живут во временных файлах и подключаются флагами только на этот запуск. Сам Claude Code при запуске пишет в свой конфиг (ключ `projects[<папка>]` с решением о доверии папке, история, транскрипт), поэтому сессия запускается с `CLAUDE_CONFIG_DIR` во временной папке: тогда эти записи попадают туда, а не в `~/.claude.json` и `~/.claude/`.

## Что нужно заранее

- Закрытая супергруппа с темами, бот в ней админ с правами "Manage Topics" и "Delete Messages".
- Файл `.env` в корне репозитория (в git не попадает):

  ```
  CCTG_BOT_TOKEN=<токен бота>
  CCTG_CHAT_ID=-100<id группы>
  CCTG_ALLOWED_USER_IDS=<ваш user id>
  CCTG_HUB_SECRET=<16+ видимых ASCII символов>
  ```

- Файл устройства `~/.cctg/device.env` с тем же секретом. Это конфиг cctg, не Claude Code; хук и агент читают его сами, в окружение процессов секрет не попадает:

  ```
  CCTG_HUB_SECRET=<тот же секрет>
  ```

  Адреса по умолчанию `127.0.0.1:47291` (агенты) и `127.0.0.1:47292` (хуки) совпадают у hub и устройства, их можно не задавать.

## 1. Собрать и запустить hub

```
cargo build --release
target/release/cctg hub
```

Запускать из корня репозитория: hub читает `./.env` (или `--env-file <путь>`). В логе должна появиться строка `hub started, polling`. Состояние (`offset`, `registry.json`) пишется в `.cctg/` рядом.

### Через supervisor (перезапуск и деплой без ручной остановки)

`cctg supervise` держит hub запущенным и сам ставит новые сборки. Бинарник лежит в своей папке (дальше `<bin>`, например `~/.cctg/bin/cctg.exe`), supervisor запускается из неё и из той же рабочей папки, где hub находит `.env` и `.cctg/`:

```
cd <папка с .env>
<bin>/cctg supervise --env-file <папка с .env>/.env
```

- Hub работает дочерним процессом (`cctg hub --stop-on-stdin`) в своей группе процессов. Упавший hub перезапускается через 1, 2, 4 ... 60 с; после минуты нормальной работы пауза снова 1 с.
- Ctrl+C (или Ctrl+Break) в окне supervisor: он закрывает hub его stdin, hub перестаёт опрашивать Telegram между пачками, записывает `registry.json` и выходит; supervisor выходит за ним (hub, не успевший за 30 с, убивается). Сам `cctg hub` без supervisor по Ctrl+C, Ctrl+Break (SIGTERM на Unix) тоже останавливается так же. Во время пробного срока нового hub (см. ниже) Ctrl+C supervisor выполняет только после этого срока; Ctrl+Break с клавиатуры в этот момент может дойти и до hub (он в той же консоли), тот выйдет, и supervisor сочтёт это падением и откатит сборку.
- Логи supervisor: имена файлов, pid, коды выхода, версия. Секретов и содержимого `.env` в них нет, supervisor `.env` не читает.

Деплой новой сборки, пока supervisor работает:

```
cargo build --release -p cctg
<bin>/cctg deploy target/release/cctg.exe
```

`deploy` кладёт копию в `<bin>/cctg.next.exe` и ждёт ответа (по умолчанию 120 с, `--timeout-secs`; другая папка: `--bin-dir`). Supervisor проверяет кандидата: `--version` отвечает `cctg ...`, байты отличаются от работающего. Потом мягко останавливает hub, переименовывает `cctg.exe` в `cctg.old.exe` (Windows не даёт перезаписать запущенный exe, но даёт переименовать; агенты и хуки, запущенные раньше, работают дальше из переименованного файла), ставит новый на место и запускает hub. Если новый hub падает за пробный срок (10 с, `supervise --trial-secs`), новый файл уходит в `cctg.bad.exe`, `cctg.old.exe` возвращается на место, hub стартует из него. Итог печатает `deploy`:

- `deployed: cctg <версия>`, код 0;
- `unchanged: ...`, код 0: кандидат совпадает с работающим бинарником;
- `rejected: ...`, код 1: кандидат не запускается или не отвечает как cctg, hub не трогался;
- `rolled back: ...`, код 1: новый hub упал, работает прежний;
- `failed: ...`, код 1: не удалось переставить файлы, в тексте сказано, что работает.

Кандидат, который не поставлен (`unchanged`, `rejected`, `failed`), сразу убирается из `cctg.next.exe` (удаляется или переименовывается в `cctg.bad.exe.<число>`), поэтому supervisor не берёт его повторно и не перезапускает hub по кругу. Если убрать файл не вышло, supervisor его больше не трогает, пока файл не изменится, и пишет в лог, что его надо удалить руками. Одновременно идёт только один `deploy` (блокировка `<bin>/cctg.deploy-lock`, снимается с выходом процесса), а ответ supervisor несёт id деплоя из `<bin>/cctg.deploy-id`, так что поздний ответ предыдущего деплоя за свой не примется.

Реестр переживает деплой: hub дописывает его перед выходом, новый hub читает тот же `.cctg/`. Агенты сессий переподключаются сами (пауза до 30 с), хуки на время подмены (пара секунд) складывают `SessionStart`/`SessionEnd` в спул, а ответ хода (`Stop`), пришедшийся на эти секунды, в тему не попадёт. Старые `cctg.old.exe`, которые ещё кто-то держит (сам supervisor, агенты), следующий деплой не удаляет, а переименовывает в `cctg.old.exe.<число>` и чистит потом. Код самого supervisor меняется только после его перезапуска.

Ограничения мягкой остановки: запросы к Telegram, уже отданные в работу (первая отправка блока субагента, `createForumTopic`), при выходе hub обрываются так же, как при прежнем жёстком Ctrl+C; блок субагента тогда не пересылается (надгробие TASK-015), тема может остаться сиротой (известное ограничение TASK-011). Если откат не смог вернуть `cctg.old.exe` на место и `cctg.exe` пропал, hub не стартует, но следующий `deploy` поставит новую сборку как обычно.

`CCTG_BOT_API_URL` только для тестов: другой адрес Bot API (фейковый сервер в `supervise_e2e`). Разрешены `https://` или `http://` на loopback (`localhost`, `127.x`, `[::1]`), потому что токен идёт в пути запроса. Hub при заданной переменной пишет одно предупреждение без адреса.

## 2. Временные файлы для Claude Code

Во временной папке, например `<tmp>/cctg-poc/`, папка `claude-config/` (пустая, это будущий `CLAUDE_CONFIG_DIR`) и два файла. `<cctg>` это абсолютный путь к собранному `cctg` (на Windows `cctg.exe`, прямые слэши).

`mcp.json`:

```json
{ "mcpServers": { "cctg": { "command": "<cctg>", "args": ["agent"] } } }
```

`settings.json` (те же хуки, что в `docs/hook-settings.json`, но с полным путём; кавычки нужны, если в пути есть пробелы):

```json
{
  "hooks": {
    "SessionStart": [{ "hooks": [{ "type": "command", "command": "\"<cctg>\" hook SessionStart" }] }],
    "SessionEnd": [{ "hooks": [{ "type": "command", "command": "\"<cctg>\" hook SessionEnd" }] }],
    "UserPromptSubmit": [{ "hooks": [{ "type": "command", "command": "\"<cctg>\" hook UserPromptSubmit" }] }],
    "Stop": [{ "hooks": [{ "type": "command", "command": "\"<cctg>\" hook Stop" }] }],
    "SubagentStart": [{ "hooks": [{ "type": "command", "command": "\"<cctg>\" hook SubagentStart" }] }],
    "SubagentStop": [{ "hooks": [{ "type": "command", "command": "\"<cctg>\" hook SubagentStop" }] }],
    "PostToolUse": [{ "matcher": "SubagentHandback", "hooks": [{ "type": "command", "command": "\"<cctg>\" hook PostToolUse" }] }],
    "PermissionRequest": [{ "hooks": [{ "type": "command", "command": "\"<cctg>\" hook PermissionRequest", "timeout": 100 }] }]
  }
}
```

`PermissionRequest` ждёт ответа из Telegram до 90 с (кнопки для диалогов, которые канал не пересылает, например проверка безопасности auto mode), поэтому ему нужен свой `"timeout": 100`. Запрос, который уже пришёл через канал, хук отпускает без решения за ~1.5 с.

Хуки из `--settings` Claude Code применяет (проверено на 2.1.280: `SessionStart` из файла, переданного `--settings`, сработал). Если в вашей версии тема не появляется при старте, положите тот же блок `hooks` в `<tmp>/cctg-poc/claude-config/settings.json`: при `CLAUDE_CONFIG_DIR` это пользовательские настройки только этого запуска.

## 3. Запустить сессию

`CLAUDE_CONFIG_DIR` задаётся только в этом окне терминала (PowerShell; в bash `export CLAUDE_CONFIG_DIR=...`):

```
cd <рабочая папка>
$env:CLAUDE_CONFIG_DIR = "<tmp>\cctg-poc\claude-config"
claude --mcp-config <tmp>/cctg-poc/mcp.json --strict-mcp-config --settings <tmp>/cctg-poc/settings.json --dangerously-load-development-channels server:cctg
```

Что даёт и чего стоит `CLAUDE_CONFIG_DIR`:

- По документации (https://code.claude.com/docs/en/claude-directory) в эту папку переезжают все пути `~/.claude`. Что туда же переезжает и `.claude.json`, в документации прямо не сказано; проверено на 2.1.280: с `CLAUDE_CONFIG_DIR` Claude Code создаёт `.claude.json` внутри этой папки и не видит серверов из `~/.claude.json`.
- В новой папке нет логина. Первый запуск попросит войти (`/login`, аккаунт claude.ai) либо возьмёт `ANTHROPIC_API_KEY` из окружения (ключ Console); каналы работают с обоими. Логин сохраняется в `claude-config/`, поэтому папку не удалять между прогонами.
- Ваши личные настройки, память `~/.claude/CLAUDE.md`, плагины и глобальные хуки в этой сессии не действуют. Для проверки канала это и нужно.
- Транскрипт сессии ляжет в `claude-config/projects/`. `/brief` в теме слота берёт путь транскрипта из хука, поэтому работает без настройки.

Без `CLAUDE_CONFIG_DIR` рецепт тоже работает, но Claude Code запишет в ваш `~/.claude.json` ключ `projects[<рабочая папка>]` (доверие папке) и сохранит транскрипт в `~/.claude/projects/`. Тогда берите одну и ту же рабочую папку на все прогоны, чтобы ключ был один.

### Из Git Bash

В Git Bash (MSYS) пути в аргументах пишите в Windows-виде (`C:\...` или `C:/...`): их MSYS не переписывает, `server:cctg` тоже доходит как есть. Пути в стиле MSYS (`/tmp`, `/c/...`) MSYS переводит при запуске `claude`, это нормально. `MSYS_NO_PATHCONV=1` не ставьте, особенно через `export` в обёртке: переменная наследуется всеми процессами сессии, и тогда команды Bash-инструмента внутри Claude Code передают нативным программам `/c/...` без перевода (живой случай 2026-09-24: Godot записал снапшот в `C:\c\Users\...`).

```
cd <рабочая папка>
$env:CLAUDE_CONFIG_DIR = "<tmp>\cctg-poc\claude-config"
claude --mcp-config <tmp>/cctg-poc/mcp.json --strict-mcp-config --settings <tmp>/cctg-poc/settings.json --dangerously-load-development-channels server:cctg
```

Что даёт и чего стоит `CLAUDE_CONFIG_DIR`:

- По документации (https://code.claude.com/docs/en/claude-directory) в эту папку переезжают все пути `~/.claude`. Что туда же переезжает и `.claude.json`, в документации прямо не сказано; проверено на 2.1.280: с `CLAUDE_CONFIG_DIR` Claude Code создаёт `.claude.json` внутри этой папки и не видит серверов из `~/.claude.json`.
- В новой папке нет логина. Первый запуск попросит войти (`/login`, аккаунт claude.ai) либо возьмёт `ANTHROPIC_API_KEY` из окружения (ключ Console); каналы работают с обоими. Логин сохраняется в `claude-config/`, поэтому папку не удалять между прогонами.
- Ваши личные настройки, память `~/.claude/CLAUDE.md`, плагины и глобальные хуки в этой сессии не действуют. Для проверки канала это и нужно.
- Транскрипт сессии ляжет в `claude-config/projects/`. `/brief` в теме слота берёт путь транскрипта из хука, поэтому работает без настройки.

Без `CLAUDE_CONFIG_DIR` рецепт тоже работает, но Claude Code запишет в ваш `~/.claude.json` ключ `projects[<рабочая папка>]` (доверие папке) и сохранит транскрипт в `~/.claude/projects/`. Тогда берите одну и ту же рабочую папку на все прогоны, чтобы ключ был один.

### Из Git Bash

В Git Bash (MSYS) перед командой нужен `MSYS_NO_PATHCONV=1`. Без него MSYS переписывает аргументы, похожие на пути: `server:cctg` и пути к файлам доходят до Claude Code искажёнными, канал не регистрируется и диалог про development channels не появляется. С `MSYS_NO_PATHCONV=1` пути пишите в виде `C:/...`: `/tmp`, `/c/...` и `$TMP` в стиле MSYS дойдут до Claude Code как есть и не откроются.

```
cd <рабочая папка>
export CLAUDE_CONFIG_DIR="<tmp>/cctg-poc/claude-config"
claude --mcp-config <tmp>/cctg-poc/mcp.json --strict-mcp-config --settings <tmp>/cctg-poc/settings.json --dangerously-load-development-channels server:cctg
```

Чтобы не набирать это каждый раз, положите в папку из `PATH` (например `~/bin`) скрипт `claude-cctg`:

```bash
#!/usr/bin/env bash
# claude-cctg: Claude Code with the cctg channel for this run only.
export CLAUDE_CONFIG_DIR="<tmp>/cctg-poc/claude-config"
exec claude \
  --mcp-config "<tmp>/cctg-poc/mcp.json" --strict-mcp-config \
  --settings "<tmp>/cctg-poc/settings.json" \
  --dangerously-load-development-channels server:cctg \
  "$@"
```

`chmod +x ~/bin/claude-cctg`, дальше `claude-cctg` в любой рабочей папке, остальные аргументы уходят в `claude` как есть (`claude-cctg --resume <id>`). Строку `CLAUDE_CONFIG_DIR` уберите, если запускаете без отдельного конфига.

`--strict-mcp-config` берёт MCP-серверы только из `mcp.json`: если `cctg` уже зарегистрирован глобально, второго экземпляра не будет. Claude Code спросит про доверие к папке и про development channels, оба вопроса подтвердить.

## 4. Что проверить

1. В группе появилась тема `[<host>] <папка> · <начало id сессии>`, иконка "живая".
2. Текст в этой теме доходит в сессию как `<channel source="cctg" ...>`, финальный ответ хода приходит в ту же тему сам (из хука `Stop`), даже если Claude не вызвал `reply`. Инструкции канала просят вызывать `reply` только для дополнительных сообщений по ходу работы; если Claude всё же отправит финальный ответ и через `reply`, он придёт дважды. В тему уходит финальный ответ каждого хода живой top-level сессии слота, в том числе ходов, начатых из терминала. Длинный ответ приходит несколькими сообщениями по порядку или одним файлом.
3. `/brief` в теме отвечает транскриптом и не попадает в сессию.
4. `/clear` в терминале: в теме разделитель `── session <id> · new ──`, следующее сообщение из темы доходит в новую сессию.
5. Выход из Claude Code: иконка "мёртвая"; сообщение в тему даёт ответ "Сессия этой темы не на связи, сообщение не доставлено." Несколько сообщений подряд дают один такой ответ в минуту.
6. Сообщение в General никуда не уходит.

## Уборка

Остановить hub (Ctrl+C), удалить `<tmp>/cctg-poc/` (вместе с логином и транскриптами этого прогона). С `CLAUDE_CONFIG_DIR` ваш `~/.claude.json` и `~/.claude/` этим прогоном не менялись; без него в `~/.claude.json` остался ключ `projects[<рабочая папка>]`, а в `~/.claude/projects/` транскрипт. Тему в группе можно удалить руками; при следующем старте сессии в этой папке hub создаст новую.
