# PoC: одна сессия Claude Code в теме Telegram

Рецепт живой проверки на одной машине: hub, одна интерактивная сессия Claude Code с каналом `cctg`, сообщения из темы в сессию и ответы обратно. Глобальный конфиг Claude Code (`~/.claude.json`, `~/.claude/settings.json`) не трогается: MCP-сервер и хуки живут во временных файлах и подключаются флагами только на этот запуск.

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

## 2. Временные файлы для Claude Code

Во временной папке, например `<tmp>/cctg-poc/`, два файла. `<cctg>` это абсолютный путь к собранному `cctg` (на Windows `cctg.exe`, прямые слэши).

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
    "PostToolUse": [{ "matcher": "SubagentHandback", "hooks": [{ "type": "command", "command": "\"<cctg>\" hook PostToolUse" }] }]
  }
}
```

Хуки из `--settings` Claude Code применяет (проверено на 2.1.280: `SessionStart` из файла, переданного `--settings`, сработал). В справке CLI есть фраза, что хуки не грузятся из `--settings`; если в вашей версии тема не появляется при старте, положите тот же блок `hooks` в `.claude/settings.local.json` рабочей папки из шага 3 (это проектный файл, не глобальный).

## 3. Запустить сессию

Рабочая папка одна и та же на все прогоны (Claude Code записывает ключ `projects[<папка>]` в `~/.claude.json` для каждой новой папки):

```
cd <рабочая папка>
claude --mcp-config <tmp>/cctg-poc/mcp.json --strict-mcp-config --settings <tmp>/cctg-poc/settings.json --dangerously-load-development-channels server:cctg
```

`--strict-mcp-config` берёт MCP-серверы только из `mcp.json`: если `cctg` уже зарегистрирован глобально, второго экземпляра не будет. Claude Code спросит про доверие к папке и про development channels, оба вопроса подтвердить.

## 4. Что проверить

1. В группе появилась тема `[<host>] <папка> · <начало id сессии>`, иконка "живая".
2. Текст в этой теме доходит в сессию как `<channel source="cctg" ...>`, Claude отвечает инструментом `reply`, ответ приходит в ту же тему. Длинный ответ приходит несколькими сообщениями по порядку или одним файлом.
3. `/brief` в теме отвечает транскриптом и не попадает в сессию.
4. `/clear` в терминале: в теме разделитель `── session <id> · new ──`, следующее сообщение из темы доходит в новую сессию.
5. Выход из Claude Code: иконка "мёртвая"; сообщение в тему даёт ответ "Сессия этой темы не на связи, сообщение не доставлено."
6. Сообщение в General никуда не уходит.

## Уборка

Остановить hub (Ctrl+C), удалить `<tmp>/cctg-poc/`. Глобальный конфиг Claude Code не менялся, кроме ключа `projects[<рабочая папка>]`. Тему в группе можно удалить руками; при следующем старте сессии в этой папке hub создаст новую.
