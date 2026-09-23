# PoC half loop: сессия -> Telegram (2026-09-23)

Живой прогон на этой машине: hub, одна интерактивная сессия Claude Code 2.1.280 (haiku) с каналом `cctg`, ответ из сессии в тему, выход из сессии. Направление Telegram -> сессия в этот прогон не входило, это ручной шаг ниже.

Редакция: токен, id чата, user id, секрет hub, session id, email аккаунта, имя хоста и домашний путь в файлах этой папки заменены (`<redacted>`, `<sid>`, `<session-id>`, `<email>`, `<host>`, `~`). Проверка скриптом после редакции: 0 совпадений со значениями из `.env`, 0 полных session id, 0 путей с домашней папкой.

## Итог по шагам

| # | Шаг | Результат |
|---|-----|-----------|
| 1 | `cargo build --release -p cctg` | PASS, 21 с, без обходов LNK1104 |
| 2 | `~/.cctg/device.env` с `CCTG_HUB_SECRET` из `.env` | PASS, файла не было, создан скриптом без печати значения. `.env` в репо не трогался |
| 3 | hub скрытым процессом, лог в `%TEMP%\cctg-poc\hub.log` | PASS, `hub started, polling` через 1.5 с, значений из `.env` в логе 0 |
| 4 | `mcp.json` и `settings.json` по `docs/poc.md` | PASS, абсолютный путь к release `cctg.exe`, рабочая папка `%TEMP%\cctg-poc\work`, без `CLAUDE_CONFIG_DIR` |
| 5 | интерактивный `claude` в скрытой консоли, два диалога | PASS, со второй попытки для trust-диалога (см. ниже) |
| 6 | SessionStart, тема, иконка, агент, reply в тему | PASS по логу hub и `registry.json`; permission relay в Telegram не подтверждён |
| 7 | `/exit`, SessionEnd, иконка "мёртвая", остановка hub | PASS, со второй попытки для `/exit` (см. ниже) |

## Хронология (UTC, из `hub.log`)

```
16:48:26.42  hub started, polling
16:48:47     claude запущен (скрытая консоль)
             trust-диалог, затем диалог development channels, оба приняты
16:50:27.64  agent registered conn=1
16:50:27.89  hook event accepted event="session_start"
16:50:28.36  forum topic created ordinal=1          (0.47 с после SessionStart)
16:50:49.03  user_prompt_submit                       (промпт про mcp__cctg__reply)
             локальный диалог разрешения на reply, принят "1. Yes" в терминале
16:51:22.28  agent reply queued ordinal=1 parts=1     (в ту же секунду, что разрешение)
16:51:23.39  stop
16:52:14.92  user_prompt_submit / 16:52:19.47 stop   (испорченный /exit, см. ниже)
16:52:39.15  agent disconnected conn=1
16:52:40.72  hook event accepted event="session_end"  (1.6 с после отключения агента)
```

Время от принятия диалога каналов до созданной темы меньше секунды. Почти две минуты до этого ушли на мои попытки ответить на trust-диалог.

## Что подтверждено и чем

- Хуки из `--settings` работают на 2.1.280: `session_start`, `user_prompt_submit`, `stop`, `session_end` пришли в hub.
- MCP-сервер из `--mcp-config` поднялся и зарегистрировался в hub (`agent registered`) раньше хука SessionStart, hub связал их по сессии.
- Тема создана (`forum topic created ordinal=1`). В `registry.json` слот `ordinal 1` для `work`, `applied_title` вида `[<host>] work · MCP tool cctg reply` (заголовок уже подхватил ai-title), `applied_icon` после старта был выставлен.
- Reply: Claude загрузил `mcp__cctg__reply` через ToolSearch и вызвал его с текстом `cctg PoC reply ok`. Hub: `agent reply queued ... parts=1`. Успешная отправка у hub в лог не пишется (только ошибки: `message to a topic not delivered` / `got no answer`), таких строк нет, WARN/ERROR в логе 0. Отдельной проверки Telegram я не делал, в Telegram ничего сам не отправлял.
- После SessionEnd `applied_icon` в `registry.json` равен `ICON_DEAD` (🏁), `ended: true`, реестр pid пустой. Hub в коде не вызывает `closeForumTopic` вообще, тема не закрыта.
- Процессы: мой `claude.exe` завершился сам после `/exit`, `cctg.exe agent` ушёл вместе с ним, hub остановлен `taskkill` по своему pid. После уборки ни одного `cctg.exe` и моего `claude.exe` не осталось. Чужие процессы не трогались.

## Что пошло не так (прогон, не код)

1. Trust-диалог по умолчанию стоит на "No, exit". Ввод `2` не выбирает второй пункт. Хелпер TASK-004 умеет только Enter и Esc, поэтому сделал его копию в scratchpad с клавишей Down (VK 0x28): Down, затем Enter. Диалог каналов уже стоит на "1. I am using this for local development", хватило Enter. Экраны: `screen_01_startup.txt`, `screen_03_down.txt`, `screen_04_channels_dialog.txt`.
2. Первый `/exit` из Git Bash превратился в `C:/Program Files/Git/exit` (MSYS path conversion) и ушёл модели как обычный промпт (`screen_08_after_exit.txt`, лишние `user_prompt_submit`/`stop` в логе в 16:52). Повтор с `MSYS_NO_PATHCONV=1` сработал.
3. Вызов `reply` спросил разрешение в терминале (`screen_06_after_prompt.txt`), ответил "1. Yes" (не "don't ask again", чтобы не писать настройки в рабочую папку). В логе hub про `permission_request` ничего нет: на уровне INFO hub это не логирует, а в Telegram я не смотрел. Дошли ли кнопки Allow/Deny в тему, этим прогоном НЕ проверено.

## Наблюдения

- В баннере сессии строка `server:cctg · no MCP server configured with that name` (`screen_05_session.txt`), хотя агент в ту же секунду зарегистрировался в hub, а tool `mcp__cctg__reply` в сессии был и сработал. Похоже на гонку проверки канала при старте с `--strict-mcp-config`. Доходят ли при этом inbound `notifications/claude/channel` в сессию, покажет только ручной шаг ниже. Если не дойдут, это первый подозреваемый.
- Без `CLAUDE_CONFIG_DIR` Claude Code записал в `~/.claude.json` ключ `projects[.../cctg-poc/work]` с доверием папке, транскрипт лёг в `~/.claude/projects/<encoded work folder>/`. Рабочая папка фиксированная, следующий прогон переиспользует тот же ключ.
- `~/.cctg/device.env` оставлен на месте, он нужен для ручного шага.
- В `hub.log` есть username бота. Это не секрет, оставил.

## Ручной шаг для человека

1. Запустить hub: из корня репо `target/release/cctg hub`.
2. В `%TEMP%\cctg-poc\work` запустить ту же команду, что в прогоне:
   `claude --model haiku --mcp-config <tmp>/cctg-poc/mcp.json --strict-mcp-config --settings <tmp>/cctg-poc/settings.json --dangerously-load-development-channels server:cctg`
   Trust-диалог уже принят для этой папки, остаётся диалог каналов.
3. В группе найти тему `[<host>] work · ...` (ordinal 1, иконка должна смениться с 🏁 на ⚡️), написать туда сообщение.
4. Проверить: в логе hub `message forwarded to the session agent`, в терминале сессии появился `<channel source="cctg" ...>`, Claude ответил через `reply`, ответ пришёл в ту же тему.
5. Попросить в теме что-то, что требует разрешения (например создать файл): проверить кнопки Allow/Deny в теме.
6. Выйти из сессии, написать в тему ещё раз: ответ "Сессия этой темы не на связи, сообщение не доставлено."

## Файлы

- `hub.log` — лог hub за прогон (редактирован).
- `screen_01_startup.txt` ... `screen_09_exit_typed.txt` — экраны консоли сессии (редактированы).
- Во временной папке `%TEMP%\cctg-poc\` остались `mcp.json`, `settings.json`, исходные лог и экраны, pid-файлы и пустая `work/`.
