# cctg — Claude Code in Telegram

Мост между локальными сессиями Claude Code (на нескольких устройствах, в разных папках) и одним закрытым Telegram-форумом. Бот с админ-правами сам создаёт темы, роутит сообщения в нужную сессию, прокидывает permission prompts и отдаёт транскрипт сессии в кратком или полном виде.

Язык общения в репозитории: русский. Код, идентификаторы, коммиты: английский.

## Зачем своё, а не готовое

Проверено 2026-09-22:
- Официальный плагин `telegram@claude-plugins-official`: один бот = одна сессия. Для N папок нужно N ботов. Не подходит.
- Официальный Remote Control: только через приложение Claude, не Telegram.
- ccgram/ccbot: tmux, под Windows только через WSL.
- ClaudePlus: Windows, но роутинг через Win32 key-sending, 0 звёзд.

## Проверенные факты о платформе (не перепроверять без причины)

**Channels (research preview, Claude Code 2.1.278):**
- Channel это MCP-сервер по stdio, который Claude Code сам спавнит. Протокол это JSON-RPC 2.0 построчно, язык любой. Node/Bun в доках только как пример. Доказательство: hdcd-telegram (Rust, hand-rolled JSON-RPC, 3.5 MB, ~5 MB RAM) работает как drop-in замена официального плагина, включая permission relay.
- Минимальная поверхность MCP, которую надо реализовать руками: `initialize` (вернуть capabilities и instructions), `notifications/initialized`, `tools/list`, `tools/call`, исходящие `notifications/claude/channel` и `notifications/claude/channel/permission`, входящая `notifications/claude/channel/permission_request`. Всё остальное отвечать method-not-found.
- Регистрация: `capabilities.experimental['claude/channel'] = {}`. Опционально `'claude/channel/permission' = {}` для relay разрешений и `tools: {}` для reply-инструмента.
- Inbound: сервер шлёт `notifications/claude/channel` с `{ content: string, meta: Record<string,string> }`. Ключи meta только `[A-Za-z0-9_]`, иначе молча выкидываются. Meta становятся атрибутами тега `<channel source=... key=...>`.
- Outbound: Claude вызывает наш MCP tool (например `reply(chat_id, text)`). Текст ответа в терминале не показывается, только факт вызова.
- Permission relay: Claude Code шлёт `notifications/claude/channel/permission_request` с `{ request_id, tool_name, description, input_preview }`. `request_id` это 5 строчных букв без `l`. Ответ: `notifications/claude/channel/permission` с `{ request_id, behavior: 'allow' | 'deny' }`. Терминальный диалог остаётся открытым параллельно, побеждает первый ответ. Trust-диалоги и MCP consent не релеятся.
- Запуск нашего канала: `claude --dangerously-load-development-channels server:cctg` при записи `cctg` в `.mcp.json` или `~/.claude.json`. Флаг `--channels` наш сервер не примет (allowlist Anthropic).
- Сообщения доходят только пока сессия жива. Всё, что пришло в мёртвую сессию, надо буферить на нашей стороне.
- Проверено 2026-09-22 (TASK-004, Claude Code 2.1.280): подсистема каналов поднимается только в интерактивной сессии. В `claude -p` (и `-p --resume`) сервер спавнится как обычный MCP, но inbound выкидывается и `permission_request` не приходит, поэтому headless resume бывает только одноразовым. Интерактивные `--resume <id>` и `--continue` сохраняют session id, канал поднимается полностью. Без флага сервер тоже спавнится, inbound молча дропается, `/mcp` выглядит так же, как с флагом: состояние "нет канала" hub узнаёт только от агента. User-scope сервер не требует consent в новой папке и спавнится в каждой сессии машины, включая сессии без флага; `claude mcp remove` уже запущенные экземпляры не гасит. Вложенный `claude -p` поднимает второй сервер со своим session id (`CLAUDE_CODE_ENTRYPOINT=sdk-cli`, но это признак headless, не вложенности). `CLAUDE_PID` в env сервера наследуется (у вложенного это pid родителя, у чистого top-level его нет). Команда запуска и alias: `maw/tasks/done/TASK-004/scratch/FINDINGS.md` раздел 9, обёртки `cctg run` нет (решение).
- Референс: https://code.claude.com/docs/en/channels-reference, пример fakechat в `anthropics/claude-plugins-official/external_plugins/fakechat`.

**Telegram Bot API (проверено 2026-09-22 на реальной группе):**
- Id supergroup в Bot API это `-100<id из веб-клиента>`. Веб-клиент показывает `<group-id>`, Bot API принимает только `-100<group-id>`.
- Боту нужно админ-право `can_manage_topics`, без него `createForumTopic` не работает. `getChatMember` показывает право, проверять на старте hub.
- `createForumTopic` ~0.36 с, 5 тем подряд за 5 с без 429. `editForumTopic` меняет `name` и `icon_custom_emoji_id` по отдельности, `icon_color` после создания не меняется. Список иконок: `getForumTopicIconStickers`, 112 штук.
- Бот-админ может писать в закрытую тему (`closeForumTopic` не блокирует бота). Пользователя закрытие блокирует, поэтому темы мёртвых сессий не закрывать: в них должны приниматься сообщения для буфера.
- Лимит текста ровно 4096 символов, 4097 даёт `message is too long`. `editMessageText` и inline-кнопки с `callback_data` в теме работают.
- Служебные сообщения тем (`forum_topic_created`, `forum_topic_edited`, `forum_topic_closed`, `forum_topic_reopened`) приходят в `getUpdates` как `message` с `is_topic_message: true`, hub их игнорирует. Собственные сообщения бота в `getUpdates` не приходят.
- Служебные сообщения о rename и смене иконки (`forum_topic_edited`, а также `forum_topic_closed`/`reopened`) бот с `can_delete_messages` удаляет через `deleteMessage` по `message_id` из `getUpdates`. Сообщение о создании темы (`forum_topic_created`) удалить нельзя: его `message_id` равен `message_thread_id`, это корень темы. Hub после каждого `editForumTopic` ждёт служебное сообщение в апдейтах и удаляет его.
- curl на Windows ломает UTF-8 в аргументах (`·` в имени темы даёт `strings must be encoded in UTF-8`), это не ограничение API.

**Транскрипты сессий:**
- Путь: `~/.claude/projects/<encoded-cwd>/<session-id>.jsonl`. Encoded cwd: путь с заменой `:`, `\`, `/`, пробелов и `_` на `-` (пример: `C:\Users\user\dev` → `C--Users-user-dev`). Файла может не быть вовсе: при унаследованной `CLAUDE_CODE_CHILD_SESSION=1` сохранение транскрипта выключено.
- Записи `type: "user"` и `type: "assistant"`, поле `message.content` это строка (набранные промпты, многие `isMeta`) или массив блоков `text | tool_use | tool_result | thinking | image` (TASK-005). Один ответ API разбит на несколько записей `assistant` с общим `message.id`, по одному блоку в каждой. Id субагента от вызова `Agent` лежит в `toolUseResult.agentId` записи с результатом. Входящие из канала сообщения лежат как `user` с `isMeta: true`. У каждой записи `uuid`, `parentUuid`, `timestamp`, `cwd`, `sessionId`, `gitBranch`, `isSidechain` (субагенты), `isMeta` (служебные).
- Прочие типы, которые надо игнорировать при рендере: `mode`, `permission-mode`, `file-history-snapshot`, `ai-title`, `last-prompt`, `system`, `summary`.
- `ai-title` даёт автоназвание сессии, пригодится для заголовка в теме.

**Hooks:** `SessionStart`, `SessionEnd`, `Stop`, `UserPromptSubmit` получают в stdin JSON с `session_id`, `cwd`, `transcript_path`, `source` (`startup|resume|clear|compact|fork`). Это единственный надёжный способ узнать session id и путь транскрипта. Channel MCP-сервер сам этого не знает.

**Субагенты:**
- `SessionStart` для субагентов НЕ стреляет. Есть отдельные `SubagentStart` / `SubagentStop` с `agent_id`, `agent_type`, `session_id` родителя, `transcript_path` родителя и `agent_transcript_path` субагента; `SubagentStop` несёт `last_assistant_message`. Проверено по докам 2026-09-22: с 2.1.271 субагент с `SubagentHandback` отдаёт отчёт через этот tool, `last_assistant_message` тогда только прощальный текст, отчёт лежит в `tool_input.message` хука `PreToolUse`/`PostToolUse` с matcher `SubagentHandback`. `SubagentStop` стреляет и для внутренних агентов Claude Code (prompt suggestions, `/btw`) с пустым `agent_type` или с именем `--agent` сессии, их надо отбрасывать. `transcript_path` пишется асинхронно и может отставать от текущего хода. Хуки `SessionEnd` делят бюджет 1.5 с.
- Транскрипт субагента лежит отдельно: `~/.claude/projects/<encoded-cwd>/<session-id>/subagents/agent-<agent_id>.jsonl` плюс `.meta.json`. В записях `isSidechain: true`, `agentId`. В основной jsonl сессии записи субагента не дублируются (проверено grep).
- Дочерние процессы сессии (tool Bash, hooks, MCP-серверы) наследуют env: `CLAUDECODE=1`, `CLAUDE_CODE_SESSION_ID=<id>`, `CLAUDE_PID`, `CLAUDE_CODE_CHILD_SESSION=1`. Значит вложенный `claude -p`, запущенный из Bash-тула (maw runner, любой внешний шелл), тоже их видит.

**Вложенные запуски (`claude -p` из сессии), проверено кодом 2026-09-22 (TASK-003):** `SessionStart` для них стреляет как для обычной сессии. Признак через env НЕ работает: любой claude (вложенный, headless, интерактивный) перезаписывает `CLAUDE_CODE_SESSION_ID` и `CLAUDE_PID` своими значениями, а `CLAUDECODE=1` и `CLAUDE_CODE_CHILD_SESSION=1` стоят и у настоящего top-level. В хуке env id всегда равен stdin id. Работает только обход дерева процессов: пропустить claude.exe с pid == `CLAUDE_PID` (это мы), следующий claude.exe-предок это родитель, его session id берётся из реестра hub `claude_pid -> session_id` (заполняется на SessionStart, чистится на SessionEnd). Три исхода: TopLevel / Nested(parent) / NestedUnknownParent, последний тоже не создаёт тему. Дыра: если промежуточный процесс-обёртка умер до хука, цепочка обрывается и вложенный запуск выглядит как top-level. Детали и фикстуры: `maw/tasks/done/TASK-003/scratch/FINDINGS.md`.

## Архитектура

Rust, один cargo workspace, один бинарник `cctg` с подкомандами `hub`, `agent`, `hook`. Без Node на машине. Решение принято 2026-09-22 после сравнения: Node-стек прожорливый (официальный плагин ~100 MB на сессию), Rust даёт один статический exe на все устройства.

Крейты:
- `tokio`, `serde`, `serde_json`, `anyhow`, `tracing`.
- Telegram: `teloxide` (long polling, `create_forum_topic`/`edit_forum_topic`, inline keyboards, `message_thread_id`). Если окажется тяжёлым, запасной путь как у hdcd-telegram: голый `reqwest` к Bot API.
- MCP stdio: без крейта, свой JSON-RPC на `serde_json` поверх stdin/stdout. `rmcp` не брать: experimental capabilities и кастомные нотификации там проходят через generic-слой, проще и прозрачнее написать 200 строк самим.
- hub <-> agent: `tokio-tungstenite` (ws) или простой newline-JSON по TCP. Начать с TCP newline-JSON, ws добавить только если понадобится браузер.
- Транскрипт: `serde_json` построчно, без своих типов на весь jsonl, только нужные поля через `#[serde(default)]`; `unicode-segmentation` для безопасной разбивки (TASK-006).

```
Telegram forum  <-- teloxide -->  hub  <-- tcp/json (localhost / tailscale) -->  agent (per session)
                                 |                                          |
                          registry.json                              Claude Code process
                          transcript reader                          (--dangerously-load-development-channels)
```

### 1. `cctg hub` — один процесс на "главном" устройстве

- Владеет токеном бота. Бот админ в закрытом супергруппе-форуме.
- TCP-сервер для агентов (newline-JSON). Аутентификация shared secret первым сообщением. Другие устройства ходят через Tailscale/LAN.
- Реестр слотов: `slot -> (device, folder, ordinal, topic_id, current_session_id?, state)` плюс `session_id -> (slot, transcript_path, parent_session_id?)` для роутинга permission-кнопок и субагентов.
- Правило тем (решение 2026-09-22): **тема это слот `(device, folder, ordinal)`, не сессия.** Новая сессия в папке занимает первый слот папки без живой сессии, то есть обычно вчерашнюю тему. Второй параллельный claude в той же папке получает слот `#2`. Число тем равно максимальной параллельности по папке, а не числу сессий за всю жизнь. Заголовок `[host] folder · ai-title` (`#N` для ordinal > 1), иконка через `icon_custom_emoji_id` по состоянию: живая / мёртвая / ждёт разрешения / без канала. Смена сессии внутри слота рисуется разделителем `── session <id> · new | resumed ──`. Тема мёртвой сессии не закрывается (пользователь не сможет писать), сообщения буферятся (до 50, старые дропаются с одним предупреждением), кнопка Resume запускает `claude -p --resume <id>` через агента устройства (одноразовый прогон: в `-p` канала нет, TASK-004). Раздутый контекст лечится handoff-ом: старой сессии заказывается summary, новая сессия стартует в том же слоте с этим summary; inline `/compact` в headless нет. Топик `General` это дашборд и команды.
- **Субагенты и вложенные запуски не получают свою тему.** Они живут внутри темы родителя:
  - Субагент отображается как свёрнутый блок в теме родителя: `↳ Explore a13bc9…` с кратким транскриптом из `subagents/agent-<id>.jsonl` по `SubagentStop`, промежуточный прогресс по запросу.
  - Вложенный `claude -p` (maw runner и т.п.) регистрируется через свой `SessionStart`, но hub по признаку вложенности привязывает его к теме родителя как `⇣ nested <id>` и не создаёт новую. Свой channel у него не поднимается.
  - Общение с субагентом из Telegram: reply на его блок → inbound в родительскую сессию с meta `target_agent=<agent_id>`, а instructions канала говорят Claude переслать это через SendMessage нужному агенту. Тот же путь, что из окна claude. Прямого канала в субагент у Claude Code нет, поэтому только через родителя.
- Роутинг inbound: сообщение в теме → сессия этой темы. Если сессия мёртвая, буферим и предлагаем поднять headless (`claude -p --resume <id>`, фаза 2).
- Команды: `/sessions`, `/brief [n]`, `/full [n]`, `/agents` (субагенты текущей сессии), `/kill`, `/resume`, `/allow`, `/deny` через кнопки.
- Реестр переживает рестарт hub: `registry.json` на диске, при старте сверка с живыми подключениями агентов.

Референсы, которые стоит прочитать перед кодом (не копировать, смотреть решения):
- https://github.com/gohyperdev/hdcd-telegram (Apache-2.0): channel-протокол на Rust руками, permission relay через inline keyboard, router-режим с темой на сессию и mailbox-файлами.
- https://github.com/robertelee78/claude-telegram-mirror: один бинарник в трёх режимах (daemon / hook / cli), тема на сессию, субагенты в тему родителя, один daemon на хост и общий супергруп. Linux-only и tmux, поэтому только как образец организации.
- https://github.com/johnkozaris/tebis: Rust, Windows через psmux. Не наш путь (эмуляция терминала), но показывает грабли Windows.
- Транскрипт читает jsonl напрямую (по `transcript_path` из hook). Не через агента, чтобы работало и для мёртвых сессий на том же устройстве. Для удалённых устройств агент отдаёт файл по запросу.

### 2. `cctg agent` и `cctg hook` — стартует Claude Code на каждую сессию

Два входа в один процесс-компаньон:
- **Channel MCP-сервер** (stdio, спавнит Claude Code). Держит TCP-соединение с hub. Inbound из hub → `notifications/claude/channel`. Tool `reply` → hub → Telegram. Permission request → hub → inline-кнопки Allow/Deny → verdict обратно.
- **Hooks** (`SessionStart`/`SessionEnd`/`Stop`/`SubagentStart`/`SubagentStop`): короткие скрипты, которые POST-ят в hub `session_id`, `cwd`, `transcript_path`, hostname, `agent_id`/`agent_type` и признак вложенности. Так hub связывает MCP-соединение с сессией. Связка MCP-сервера и hook: оба наследуют `CLAUDE_CODE_SESSION_ID` от процесса claude, сверяем по нему. Если env окажется ненадёжным, запасной вариант: сверка по `CLAUDE_PID`/ppid.

### 3. `crates/transcript` — чистая библиотека

- `parse(jsonl) -> Turn[]`, без IO-зависимостей, покрыта тестами на реальных jsonl из `~/.claude/projects`.
- `renderBrief(turns)`: промпты пользователя, финальный текст ассистента, вызовы инструментов в одну строку (`Bash: описание`, `Edit: file`). Вызов `Agent` показывается как `↳ <type> <agent_id>` с кратким итогом субагента.
- `renderFull(turns)`: плюс входы инструментов и результаты с обрезкой, без thinking. Субагенты разворачиваются в своём кратком виде, не в полном.
- Тот же парсер читает `subagents/agent-*.jsonl`, формат записей одинаковый.
- Разбивка под лимит Telegram 4096, считается в UTF-16 (`transcript::telegram_len`, консервативно при любом правиле сервера), без разреза графем-кластеров; больше 4 кусков уходит файлом. Brief показывает ответом текст с `stop_reason: end_turn` (у субагентов `null` на финальном тексте, там структурный fallback), незавершённый ход помечается «в работе…» (TASK-006).
- Тесты: фикстуры это обезличенные куски реальных jsonl из `~/.claude/projects`, лежат в `crates/transcript/tests/fixtures/`.

## Безопасность

- Gate по `from.id` пользователя, не по чату. Allowlist в hub.
- Permission relay только от allowlisted. Кнопки Allow/Deny с `request_id` в callback data.
- Секрет hub в `.env`, не в репо. `.cctg/` в gitignore.

## Порядок разработки

1. `transcript`: parse + brief/full на реальных jsonl. Проверка: unit-тесты, вывод глазами.
2. `hub`: бот в форуме, создание темы, реестр, команда `/brief` по локальному jsonl. Проверка: тема появилась, транскрипт пришёл.
3. `agent`: channel-сервер + hooks, один локальный Claude Code. Проверка: сообщение из темы дошло в сессию, ответ вернулся, permission кнопка работает.
4. Несколько сессий в одной папке и в разных папках, субагенты и вложенный `claude -p` внутри темы родителя. Проверка: роутинг не путает, лишних тем не появляется.
5. Второе устройство через Tailscale.
6. Headless resume мёртвых сессий (`claude -p --resume --output-format stream-json`).

## Открытые вопросы (проверить кодом, не гадать)

- ~~Затирает ли вложенный `claude -p` переменную `CLAUDE_CODE_SESSION_ID`~~ Да, всегда (TASK-003). Детект только через ppid и реестр pid.
- ~~Хватает ли `SubagentStop.last_assistant_message`~~ Зависит от того, вызвал ли субагент `SubagentHandback`: если был `PreToolUse SubagentHandback` для этого `agent_id`, отчёт в `tool_input.message`, иначе в `last_assistant_message` (TASK-003).
- ~~Как ведёт себя `--dangerously-load-development-channels` при `claude --resume`~~ Канал поднимается, session id тот же; в `-p` канала нет вообще (TASK-004).
- ~~Нужен ли consent-диалог для `.mcp.json` каждый раз в новой папке~~ User-scope регистрация в `~/.claude.json` consent не требует (TASK-004).
- Лимиты Telegram на создание тем (rate limit при массовом старте).

## MAW (установлен, https://github.com/pockerhead/maw)

Многоагентный pipeline с adversarial-ревью: каждый следующий агент считает, что предыдущий ошибся, и проверяет по коду. Установлен `install.sh --claude`: `.claude/skills/{maw-tasks,maw-execute-task,maw-context}` и 45 субагентов `.claude/agents/maw-*.md` (9 ролей × 5 уровней effort). Задачи живут в `maw/tasks/{pending,in_progress,done,blocked}/TASK-NNN/`, настройки в `maw/settings.json`, проектные знания для агентов в `maw/project-context/`.

Как использовать здесь:
- `/maw-tasks "описание"` создаёт задачу, предлагает режим: `full` (9 стадий), `small-fix` (implementer → review → fixer → QA), `brainstorm` (до плана, без кода), `deep-research`.
- `/maw-execute-task [N] [--worktree]` гоняет pipeline по задаче.
- `/maw-context` заводит и правит `maw/project-context/`. Сделать первым делом: положить туда факты о channels и формате jsonl из этого файла, чтобы агенты pipeline не переоткрывали их.
- Шаги 1-3 из плана выше это кандидаты в `full`, мелочи после этого в `small-fix`. Архитектурные развилки (детект вложенности, связь субагент ↔ тема) в `brainstorm`.
- Runner maw спавнит `claude -p`, то есть это как раз вложенные запуски, которые hub должен клеить к теме родителя, а не плодить темы.

## Стиль

- Хирургические изменения, без лишних абстракций. Каждый пакет должен помещаться в голове.
- Секреты и user id никогда в логи.
- Тесты на transcript обязательны, на hub/agent по мере появления протокола.
