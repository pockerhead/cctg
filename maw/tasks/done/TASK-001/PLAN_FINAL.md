# TASK-001 — финальный research report: декомпозиция cctg MVP

Дата проверки: 2026-09-22. Ревизия PLAN_V2 после решений пользователя в `TASK_FINAL.md` («Resolved questions») и повторной сверки внешних утверждений с первичными источниками.

## 0. Summary

Отчёт разбивает шаги 1–4 из `CLAUDE.md` («Порядок разработки») на 18 задач для MAW-пайплайна плюс одну явно пост-MVP задачу. Архитектура: один cargo workspace из двух package-members — binary `crates/cctg` с подкомандами `hub`/`agent`/`hook` и чистая библиотека `crates/transcript`. Главное изменение относительно PLAN_V2 — **модель слотов**: тема Telegram это слот `(device, folder, ordinal)`, а не сессия; реестр слотовый, сессии сменяют друг друга внутри слота, мёртвый слот не закрывается и буферит входящие. Второе изменение по существу — **выбор Telegram-клиента**: рекомендуется голый `reqwest` поверх десяти нужных Bot API методов, `frankenstein` как названный fallback; `teloxide` отклонён с доказательствами (released 0.17.0 от 2025-07-11 покрывает Bot API 9.1, master не выше 9.2, живой Bot API на момент проверки — 10.3 от 2026-08-24). Третье — закрыт реальный пробел PLAN_V2: там не было ни одной задачи, которая фактически доставляет турны сессии в Telegram, хотя решение пользователя требует «push every turn через outbound queue». Все пять «Открытых вопросов» из `CLAUDE.md` либо закрыты первичным источником, либо превращены в spike с описанным экспериментом.

---

## 0.1 Orchestrator correction (2026-09-22, verified by direct fetch of https://code.claude.com/docs/en/hooks.md)

The review-notes claim below that PLAN_V2 "fabricated" documentation for the subagent block is **wrong**; the reviewer relied on a delegated, truncated fetch. Direct grep of the official page confirms all four statements verbatim:

- `SubagentStop` input includes `agent_transcript_path` ("`transcript_path` is the main session's transcript, while `agent_transcript_path` is the subagent's"), example value `~/.claude/projects/.../abc123/subagents/agent-def456.jsonl`.
- "The transcript file is written asynchronously and may lag the in-memory conversation ... Hooks that need the final assistant text of the current turn should use `last_assistant_message` on Stop and SubagentStop instead of reading the transcript".
- "On Claude Code v2.1.271 or later, a subagent that runs with the `SubagentHandback` tool delivers its report through that tool before it stops. The `last_assistant_message` field then holds the subagent's closing text, if any, which is not the delivered report. The report is that call's `message` input, which a `PreToolUse` or `PostToolUse` hook matched on `SubagentHandback` receives as `tool_input.message`."
- "Not every SubagentStop event comes from a subagent Claude spawned ... For those events, `agent_type` is the agent name the session itself runs as, such as one set with `--agent` ..., and an empty string when the session runs without one."
- Also confirmed: "`SessionEnd` hooks share a 1.5-second budget; if your settings set a longer per-hook `timeout`, Claude Code raises the budget to match, up to 60 seconds".

Consequences: R9, the rows "Достаточен ли `SubagentStop.last_assistant_message`" and "path to subagent transcript not documented", and the risk items on the subagent report contract are superseded by this note. TASK-003 still captures the live payload (cheap, and confirms the doc on this version), but TASK-012 implements the documented mechanism (`PreToolUse`/`PostToolUse` matcher on `SubagentHandback` capturing `tool_input.message`, `agent_transcript_path` from `SubagentStop`) rather than gating it on the spike. What remains genuinely undocumented: `CLAUDE_CODE_SESSION_ID`, `CLAUDE_PID`, `CLAUDE_CODE_CHILD_SESSION` as hook env, and development-channel behaviour on `--resume`.

## 1. Review notes

### Обязательная disconfirmation-проверка

**Контрпример, который я выбрал и искал:** «хотя бы одно новое внешнее утверждение PLAN_V2 (то, которого нет в верифицированном `CLAUDE.md`) не подтверждается первичным источником или подтверждается наоборот».

Проверял по первичным источникам, а не по пересказу:

| Утверждение PLAN_V2 | Результат |
|---|---|
| `createForumTopic.name` 1–128 | **Подтверждено** (локальный снимок `core.telegram.org/bots/api` в `scratch/botapi.html`: «Topic name, 1-128 characters») |
| `editForumTopic.name` 0–128, `icon_custom_emoji_id` изменяем, `icon_color` не меняется | **Подтверждено**: у `editForumTopic` вообще нет параметра `icon_color`, есть только `name` (0-128) и `icon_custom_emoji_id` |
| `sendMessage.text` 1–4096 после entity parsing | **Подтверждено** дословно |
| `callback_data` 1–64 bytes | **Подтверждено** дословно |
| `ResponseParameters.retry_after` | **Подтверждено** дословно |
| Bot FAQ: ~1 msg/s на chat, 20 msg/min в группе, ~30 broadcast/s; лимиты на edit и создание тем **не** опубликованы | **Подтверждено** (fetch `core.telegram.org/bots/faq`) |
| `frankenstein 0.52` = Bot API 10.3 | **Подтверждено** (crates.io: 0.52.0 от 2026-08-28; Bot API 10.3 датирован 2026-08-24) |
| «`teloxide` master активен и добавил 9.2», поэтому отставание не проблема | **Контрпример сработал.** Источник, на который ссылается PLAN_V2, говорит противоположное выводу: в CHANGELOG master потолок это TBA **9.2**, релиз 0.17.0 (2025-07-11) — TBA **9.1**, а живой Bot API на 2026-09-22 это **10.3**. То есть «активный master» сам отстаёт примерно на год. PLAN_V2 процитировал источник верно, но сделал из него обратный вывод. |
| «Официальная документация подтверждает `SubagentStop.agent_transcript_path`, асинхронное запаздывание transcript-файла и что отчёт нужно брать из `tool_input.message` через matcher, ссылка — Hooks reference» | **Контрпример сработал, крупно.** Проверка официальной документации (делегированный fetch `code.claude.com/docs`) даёт: поля `agent_transcript_path` в схеме хуков **нет**; рекомендации ловить `SubagentHandback` через `PreToolUse`/`PostToolUse` в документации **нет**; утверждения об асинхронном запаздывании файла транскрипта **нет**; поведения `agent_type` при запуске основной сессии с `--agent` **нет**. Подтверждается только два факта: `SubagentHandback` действительно существует как инструмент (требует Claude Code ≥ 2.1.271) и `SubagentStop` действительно несёт `last_assistant_message`. PLAN_V2 выдал четыре недокументированных утверждения за процитированную документацию. |

Итог disconfirmation: контрпример **подтвердился дважды** — в обосновании выбора `teloxide` и, серьёзнее, в блоке про субагентов, где PLAN_V2 сослался на официальную документацию за утверждения, которых в ней нет. Остальные внешние числа PLAN_V2 (весь Telegram-слой) держатся. Поэтому ниже переоткрыты вопрос клиента и источник тела блока субагента; остальной фактический слой оставлен как есть.

Попутно проверено и **подтверждено** документацией: общие поля хуков `session_id`, `cwd`, `transcript_path` есть у всех четырёх lifecycle-событий; `source` документирован **только** для `SessionStart` (в `CLAUDE.md` он приписан всем четырём — расхождение вынесено в `PCTX_PROPOSALS.md`, молча я его не правлю); у `SessionEnd`-хуков общий бюджет **1.5 с**, у `UserPromptSubmit` — 30 с, у `Stop` и обычных command-хуков — 600 с; project-scope `.mcp.json` требует approval в интерактивной сессии, но в non-interactive режиме (`-p`, SDK) грузится **без** запроса, а user-scope в `~/.claude.json` approval не требует. Переменных `CLAUDE_CODE_SESSION_ID`, `CLAUDE_PID`, `CLAUDE_CODE_CHILD_SESSION` в документации нет вообще — из документированных хукам передаются `CLAUDE_PROJECT_DIR`, `CLAUDE_ENV_FILE`, `CLAUDE_EFFORT`.

Оговорка о методе: проверку документации я делегировал, а не фетчил страницы сам. Поэтому «не найдено в документации» я трактую как «не документировано», а не как «неверно»: факты из `CLAUDE.md` наблюдены на этой машине и остаются нормативными, но опираться на них как на *документированный контракт* нельзя.

Отдельно проверено на диске: `Cargo.toml` в репозитории нет ни одного (`find . -name Cargo.toml` пусто), `.claude/` содержит только `agents/` и `skills/` — файла `settings.json` нет, значит хуки spike-задач регистрируются либо в новом project `.claude/settings.json`, либо в user scope. Раскладка субагентских транскриптов из `CLAUDE.md` подтверждена листингом реального каталога `~/.claude/projects/C--Users-user-dev-cctg/<session-id>/subagents/agent-*.meta.json`.

### Что из критики PLAN_V2 остаётся в силе

Подтверждаю и сохраняю эти пункты PLAN_V2 (я их перепроверил, они верны):

1. Два workspace member вместо шести крейтов. Отдельный `proto` крейт не нужен — это модуль binary package.
2. Hook ходит одноразовым HTTP POST, а не через reconnecting TCP. Нормативный модуль `hooks` требует fire-and-forget и `exit 0` при отсутствии hub.
3. Тему мёртвой сессии **нельзя** закрывать. Теперь это ещё и верифицированный факт в `CLAUDE.md`: бот-админ в закрытую тему писать может, а пользователь нет, поэтому закрытие ломает приём сообщений в буфер.
4. State через `icon_custom_emoji_id`, а не через title.
5. Flood-control вывод: документированные числа относятся к send-операциям; численного лимита на edit и на создание тем Telegram не публикует. Выдавать «30 req/s» за документированный предел нельзя.
6. Граница чистой библиотеки: `transcript` не делает IO, путь и fallback выбирает hub.
7. Корреляция субагентов с явным parent `Agent` tool call вместо фильтра «пустой `agent_type`».
8. Soak должен явно считать темы, а не оставлять «четыре темы» двусмысленными.

### Что из PLAN_V2 отменяется или исправляется

**R1. Пункт 3 review notes PLAN_V2 («нарушено правило одна тема на session id») отозван.** Он был верен относительно старого закона и неверен относительно нового. Решение пользователя от 2026-09-22 прямо меняет закон: тема это слот `(device, folder, ordinal)`. Новый `session_id` после `/clear` **не** создаёт новую тему — он занимает тот же свободный слот и рисуется разделителем `── session <short-id> · new | resumed ──`. Новая тема создаётся только когда в папке нет слота без живой сессии. Весь TASK-011 PLAN_V2 («resume того же id переиспользует; новый top-level id получает новый topic») переписан.

**R2. Выбор Telegram-клиента переоткрыт по существу.** PLAN_V2 выбрал `teloxide` в основном потому, что так было написано в старом `CLAUDE.md`, и назвал отставание неблокирующим. Пользователь снял это ограничение (решение 5 в `TASK_FINAL.md`). Переоценка — в разделе 3.

**R3. Пробел: в PLAN_V2 нет задачи, которая доставляет турны в Telegram.** Есть `/brief` и `/full` (pull по команде), есть permission relay, есть блоки субагентов. Но решение пользователя (3) требует «push every turn через outbound queue». Без этого мост не выполняет своё назначение: в теме не появится ничего, пока человек не наберёт команду. Добавлена TASK-016.

**R4. Пробел: буферизация мёртвого слота нигде не реализуется.** Нормативный `hub`-домен требует «буфер до 50 сообщений, дропать старые, одно предупреждение». В PLAN_V2 это упомянуто только в разделе рисков как «concern». Добавлена TASK-017.

**R5. Пробел: уборка служебных сообщений тем.** Верифицированный факт в `CLAUDE.md`: после каждого `editForumTopic` в теме появляется служебное `forum_topic_edited`, и hub должен удалять его через `deleteMessage` (нужно право `can_delete_messages`), а `forum_topic_created` удалить нельзя. Поскольку hub меняет иконку состояния часто, без этой уборки тема забьётся служебными сообщениями. В PLAN_V2 нет ни слова. Свёрнуто в TASK-008 и TASK-011.

**R6. OQ-5 (лимиты создания тем) закрыт наполовину, а PLAN_V2 оставил его открытым.** Численный лимит Telegram действительно не публикует — это верно. Но эмпирика уже есть в `CLAUDE.md`: `createForumTopic` ~0.36 с, 5 тем подряд за 5 с без 429. Плюс модель слотов резко снижает частоту создания тем (темы создаются раз на слот, а не раз на сессию), так что риск массового старта почти исчезает. Вопрос переводится из «открыт» в «закрыт для MVP с известной границей».

**R7. Пропущен ряд верифицированных Telegram-фактов**, которые прямо влияют на задачи: chat id это `-100`+web id; право `can_manage_topics` проверяется через `getChatMember` на старте hub; собственные сообщения бота не приходят в `getUpdates` (значит нельзя ждать их эхо); служебные сообщения тем приходят как `message` с `is_topic_message: true` и должны игнорироваться роутингом. Всё это разложено по acceptance criteria ниже.

**R9. Блок про субагентов в PLAN_V2 переписан как наблюдаемый, а не как документированный.** PLAN_V2 построил TASK-007/012/015 на четырёх недокументированных утверждениях (см. таблицу выше) и ссылался на Hooks reference. Что остаётся твёрдым: `SubagentStop` несёт `last_assistant_message` (документировано); субагентский транскрипт лежит в `<session-id>/subagents/agent-<id>.jsonl` (наблюдено в `CLAUDE.md` и подтверждено мной листингом реального каталога); `SubagentHandback` существует как инструмент с 2.1.271 (документировано). Что становится предметом спайка, а не предположением: чем именно наполняется `last_assistant_message` при передаче отчёта, есть ли в payload путь к транскрипту субагента, и отстаёт ли файл. TASK-003 расширена этим измерением, TASK-012 больше не хардкодит `PostToolUse`-matcher и `tool_input.message`, а реализует механизм, подтверждённый спайком. Порядок fallback в TASK-007/015 сохранён: он корректен именно потому, что не зависит от того, какой источник окажется доступен.

**R8. Решения пользователя по секции 8 PLAN.md не отражены в PLAN_V2.** Нет обёртки `cctg run` — вместо неё документированный флаг и shell alias, а hub показывает состояние «нет канала». Outbound queue: token bucket 20/min на группу, FIFO внутри темы, коалесинг правок, приоритет permission; рост задержки при конкуренции принят. Всё это стало явными критериями TASK-008/011/016.

### Чего я проверить не смог (честные ограничения)

- Поведение `--dangerously-load-development-channels` при `--resume`/`--continue` официальная документация не описывает. Остаётся spike (TASK-004), выдавать догадку за факт нельзя.
- `CLAUDE_CODE_SESSION_ID`, `CLAUDE_PID` и `CLAUDE_CODE_CHILD_SESSION` в официальной документации не упоминаются вовсе (документированы только `CLAUDE_PROJECT_DIR`, `CLAUDE_ENV_FILE`, `CLAUDE_EFFORT`). `CLAUDE.md` фиксирует их как наблюдённые на этой машине, и это остаётся нормативным, но это недокументированное поведение: оно может измениться без предупреждения. Отсюда обязательность ppid-fallback, а не «приятно бы иметь». Остаётся spike (TASK-003).
- Есть ли в payload `SubagentStop` путь к транскрипту субагента — не документировано. Hub обязан уметь собрать путь сам из `cwd` + `session_id` + `agent_id` по правилу из `CLAUDE.md`, а не рассчитывать на поле.
- Измерений binary size / RSS / build time ни для одного из трёх клиентов у меня нет — я не собирал код. Рекомендация по клиенту построена на свежести схемы, объёме нужной поверхности API и нормативном инварианте, а не на весе бинаря. Это отмечено как таковое.
- Разрешён ли боту `deleteMessage` для служебных сообщений тем без `can_delete_messages` — в `CLAUDE.md` записано, что право нужно; отдельно я это не перепроверял.

---

## 2. Updated understanding

### Текущее состояние

Greenfield. В репозитории нет `Cargo.toml`, исходников и тестов; есть `CLAUDE.md`, `.gitignore` (`target/`, `.env`, `*.log`, `.cctg/`, `registry.json` — `.env` действительно игнорируется, проверено `git check-ignore`), каталог `maw/` с настройками, project-context и артефактами задач. Критерий «existing tests pass» выполняется вакуозно по решению в `TASK_FINAL.md`.

### Целевая структура

```text
Cargo.toml                  # workspace, resolver = "2"
crates/cctg/                # единственный binary package
  src/main.rs               # clap: hub | agent | hook <event>
  src/hub/                  # Telegram client, slot registry, routing, TCP + hook HTTP ingress
  src/agent/                # Channel MCP stdio + persistent TCP client
  src/hook/                 # stdin payload -> один короткий HTTP POST -> exit 0
  src/wire.rs               # общие agent<->hub wire-типы, без отдельного crate
crates/transcript/          # чистый парсер и рендереры, без IO/network/runtime
```

Два package-member: один executable и одна отдельно тестируемая чистая библиотека. Пользователь не имеет предпочтений по числу крейтов кроме «каждый крейт помещается в голове» — два помещаются, шесть создают граф зависимостей без выигрыша.

### Модель слотов (нормативная, заменяет «тема на сессию»)

- Слот = `(device, folder, ordinal)`. Тема Telegram принадлежит слоту навсегда.
- Новая сессия в папке занимает **первый слот этой папки, к которому не привязана живая сессия**. Обычно это вчерашняя тема.
- Если все слоты папки заняты живыми сессиями, создаётся слот со следующим `ordinal` и новая тема. Число тем = максимальная параллельность по папке, а не число сессий за всё время.
- Заголовок: `[host] folder · ai-title`, для `ordinal > 1` добавляется `#N`. Пока `ai-title` не появился в jsonl — короткий session id. Лимит 128 символов обязателен к проверке (усекается `folder`/`ai-title`, `[host]` и `#N` сохраняются).
- Состояние слота — иконка `icon_custom_emoji_id`: alive / dead / waiting permission / no channel. `icon_color` после создания не меняется, состояние в заголовок не выносится.
- Смена сессии внутри слота рисуется одной строкой-разделителем `── session <short-id> · new | resumed ──`, а не префиксом у каждого сообщения.
- Мёртвая сессия: тема **никогда** не закрывается. Меняется иконка, входящие буферятся (до 50, при переполнении дропается старейшее с одним предупреждением в теме), появляется кнопка Resume.
- Раздутый контекст лечится handoff-ом, а не inline `/compact` (в headless его нет): старой сессии заказывается summary, новая сессия стартует в том же слоте с этим summary как первым промптом. Для headless-resumed сессий ставится `CLAUDE_AUTOCOMPACT_PCT_OVERRIDE`.
- Субагенты и вложенные `claude -p` слота и темы не получают никогда. Они рендерятся внутри темы родительского слота: `↳ <type> <id>` и `⇣ nested <id>`.

Реестр: `slot -> (device, folder, ordinal, topic_id, current_session_id?, state)` плюс `session_id -> (slot, transcript_path, parent_session_id?)` для permission-коллбеков и субагентов. Персист в `registry.json`, сверка с живыми соединениями агентов при старте.

### Открытые вопросы `CLAUDE.md`

| Вопрос | Статус | Действие |
|---|---|---|
| Затирает ли вложенный `claude -p` `CLAUDE_CODE_SESSION_ID` для своих hooks/MCP | **Открыт.** Переменная не документирована как часть hook environment contract, гарантии нет | TASK-003: наблюдаемый эксперимент, взрослый fallback по ppid |
| Достаточен ли `SubagentStop.last_assistant_message` | **Открыт по существу.** Документировано только то, что поле есть. Чем оно наполнено при передаче отчёта через `SubagentHandback` (инструмент существует с 2.1.271) — не документировано; поля с путём к транскрипту субагента в схеме нет; запаздывание файла не документировано | TASK-003 снимает фактический payload; TASK-007/TASK-015 реализуют порядок fallback, который корректен при любом исходе |
| Поведение `--dangerously-load-development-channels` при `--resume` | **Открыт.** Документация взаимодействие флага с `--resume`/`--continue` не описывает (проверено) | TASK-004: fresh / `--resume` / `--continue` / без флага, таблица наблюдений |
| Нужен ли consent для `.mcp.json` в новой папке | **Закрыт документацией.** Project-scope сервер требует approval в интерактивной сессии (но в non-interactive `-p`/SDK грузится без запроса); user-scope в `~/.claude.json` approval не требует и доступен всем проектам. Значит регистрировать глобально | TASK-004 подтверждает наблюдением, но решение уже принято: user scope |
| Лимиты Telegram на создание тем | **Закрыт для MVP.** Численный лимит официально не публикуется; эмпирически 5 тем за 5 с без 429, `createForumTopic` ~0.36 с. Модель слотов дополнительно снижает частоту | Отдельная задача не нужна; мутации сериализуются, на 429 ждём `retry_after` |

### Telegram-факты, на которые опирается дизайн

Все проверены по `core.telegram.org` (локальный снимок в `scratch/botapi.html`) или по верифицированному блоку в `CLAUDE.md`:

- `createForumTopic`: `name` 1–128 символов, требует админ-право `can_manage_topics`. Эмпирически ~0.36 с, 5 подряд за 5 с без 429.
- `editForumTopic`: `name` 0–128, `icon_custom_emoji_id` меняется; параметра `icon_color` у метода нет вовсе. Допустимые эмодзи — `getForumTopicIconStickers` (112 штук).
- `sendMessage.text`: 1–4096 символов после парсинга entities; эмпирически 4097 даёт `message is too long`. Для MVP plain text, чтобы не ломать entity-разметку.
- `InlineKeyboardButton.callback_data`: 1–64 байта.
- `ResponseParameters.retry_after`: число секунд ожидания при flood control.
- Bot FAQ: ~1 сообщение/с на чат, **20 сообщений/минуту в группе**, ~30 broadcast/с. Лимитов на `editMessageText` и `createForumTopic` FAQ не публикует.
- Chat id супергруппы в Bot API = `-100` + id из веб-клиента.
- Бот-админ пишет в закрытую тему, пользователь — нет.
- Служебные сообщения тем приходят в `getUpdates` как `message` с `is_topic_message: true`; роутинг их игнорирует. `forum_topic_edited`/`closed`/`reopened` удаляются через `deleteMessage`, `forum_topic_created` удалить нельзя (его `message_id` = `message_thread_id`).
- Собственные сообщения бота в `getUpdates` не приходят.

---

## 3. Revised approach

### 3.1 Выбор Telegram-клиента (переоткрыт)

Сравнение трёх реальных альтернатив под фактическую потребность MVP. Нужны ровно эти методы: `getMe`, `getChatMember`, `getUpdates`, `sendMessage`, `editMessageText`, `sendDocument`, `deleteMessage`, `answerCallbackQuery`, `createForumTopic`, `editForumTopic`, `getForumTopicIconStickers`. Все существуют с Bot API 6.4 (2022) или раньше.

| Вариант | Состояние на 2026-09-22 | Что даёт | Что стоит |
|---|---|---|---|
| голый `reqwest` + узкие serde-структуры | не зависит от схемы вообще | полный контроль; десериализуются только нужные поля с `#[serde(default)]`; невозможно сломаться о новое поле Bot API; поверхность кода ~300–400 строк на 11 методов | руками писать envelope `{ok, result, description, parameters}`, multipart для `sendDocument`, error-модель; нет чужих тестов |
| `frankenstein 0.52` | релиз 2026-08-28, заявляет Bot API 10.3 (API 10.3 вышел 2026-08-24) | typed surface, актуальная схема, опциональный reqwest-клиент, активный мейнтейнер | typed-модель всего Bot API против нашего инварианта «не моделировать весь Bot API»; scheduler/routing всё равно свои; малая экосистема |
| `teloxide 0.17.0` | последний релиз **2025-07-11**, покрывает Bot API 9.1; master не выше 9.2 | dispatcher, dialogues, `Throttle`, typed updates, forum-методы на месте | ~14 месяцев отставания схемы при живом Bot API 10.3; framework-слой (dispatcher/DPtree/dialogue) нам не нужен; `Throttle` заменяется своим планировщиком |

**Рекомендация: голый `reqwest`.** Обоснование, а не вкус:

1. **Нормативный инвариант проекта прямо это предписывает:** «Deserialize only the fields you need with `#[serde(default)]`; never model the whole jsonl or **Bot API**». Оба typed-клиента делают ровно обратное.
2. **Нужная поверхность крошечная** — 11 методов, все старые и стабильные. Framework не окупается.
3. **Свой планировщик нужен в любом случае.** Приоритет permission-запросов, FIFO внутри темы, коалесинг правок, общий `retry_after`, отдельная полоса для мутаций тем — ничего из этого `teloxide::Throttle` не делает. Клиент в итоге оборачивается, а не используется.
4. **Риск дрейфа схемы реален и измерим.** У typed-клиента новое или изменившееся поле в уже известном типе апдейта ломает парсинг всего апдейта; `UpdateKind::Error` спасает только от неизвестного *вида* апдейта. В трекере teloxide это повторяющийся класс багов (issues #427, #481, PR #1421 «сделать поле optional, чтобы не падал парсинг»). При отставании схемы на год это не гипотеза.
5. **Мы уже читаем сырой Bot API.** Верифицированный блок в `CLAUDE.md` описывает поведение на уровне JSON (`is_topic_message`, `message_id == message_thread_id`, эхо своих сообщений), то есть модель мышления команды и так сырая.

**Честный контраргумент, который я не прячу:** `teloxide` заработал бы. Все нужные методы в нём есть, дрейф схемы — вероятностный риск, а не блокер, и чужие протестированные обвязки экономят время. Это решение с запасом прочности, а не исправление дефекта. Поэтому:

- `frankenstein 0.52` остаётся **названным fallback**, если написанный вручную слой окажется дороже ожидаемого: он актуален по схеме и приносит тот же `reqwest`.
- `teloxide` не рекомендуется, но и не запрещается; возврат к нему требует отдельного решения с измерениями.
- TASK-008 обязана зафиксировать измерения (binary size, clean build, idle RSS) — чтобы будущая смена решения опиралась на цифры, которых у меня сейчас нет.

Это отклонение от строки `hub`-домена «Telegram: `teloxide` ... fallback bare `reqwest`». Отклонение санкционировано решением пользователя (5) в `TASK_FINAL.md`; предложение на правку project-context записано в `PCTX_PROPOSALS.md`.

### 3.2 Архитектурный поток

1. `transcript` чисто разбирает строки и рендерит plain-text куски плюс признак «лучше файлом». Никакого IO.
2. `hub` принимает Telegram updates длинным поллингом через свой reqwest-слой, соединения агентов по newline-JSON/TCP, hook-события по HTTP POST.
3. `hook` — короткоживущий HTTP-клиент без reconnect-цикла; недоступность hub никогда не ломает Claude Code (`exit 0`).
4. `agent` — единственный persistent-клиент: MCP stdio к Claude Code и TCP к hub. На stdout только JSON-RPC.
5. **Слотовый роутинг.** `SessionStart` даёт `(device, folder, session_id)`. Hub берёт первый слот папки без живой сессии или заводит следующий ordinal. Внутри слота пишется разделитель смены сессии. Вложенный запуск и субагент получают только ссылку на слот родителя.
6. Состояние слота — custom emoji; заголовок остаётся `[host] folder · ai-title` (+`#N`). После каждого `editForumTopic` hub удаляет пришедшее служебное `forum_topic_edited`.
7. Турны сессии пушатся в тему слота по мере появления (tail локального jsonl), а не только по команде.
8. Permission-трафик обгоняет транскрипт-трафик. Send-лимиты (20/мин на группу) и неизвестные лимиты мутаций моделируются раздельно; любой 429 уважает `retry_after`.

### 3.3 Порядок источников для тела блока субагента

На `SubagentStop` тело блока выбирается по порядку: (1) перехваченный отчёт `SubagentHandback`, если он был перехвачен, (2) brief по транскрипту субагента `<session-id>/subagents/agent-<id>.jsonl`, (3) `last_assistant_message` из payload хука.

Важное уточнение к PLAN_V2, который выдал этот порядок за требование документации. Документировано ровно три вещи: `SubagentStop` несёт `last_assistant_message`; инструмент `SubagentHandback` существует начиная с Claude Code 2.1.271 и доставляет финальный отчёт субагента; поля с путём к транскрипту субагента в схеме хуков **нет**. Не документировано: чем наполняется `last_assistant_message` при передаче отчёта, отстаёт ли файл транскрипта на момент хука, и рекомендуется ли ловить отчёт tool-matcher-ом. Порядок выше выбран не потому, что так написано в документации, а потому, что он **корректен при любом из исходов**: если `last_assistant_message` содержателен, до него просто не дойдёт очередь; если пуст — сработает шаг 2 или 3.

Следствия для реализации, которые нельзя обходить:
- путь к транскрипту субагента hub **строит сам** из `cwd` + `session_id` + `agent_id` по правилу из `CLAUDE.md`, а не читает из payload;
- шаг 2 обязан переживать отсутствующий или частично дописанный файл (тогда переход к шагу 3), потому что гарантии полноты нет;
- конкретный механизм перехвата отчёта фиксируется наблюдением в TASK-003, а не предполагается заранее; если спайк покажет, что `last_assistant_message` и так несёт отчёт, шаг 1 просто не реализуется и это экономия, а не потеря.

### 3.4 Параллельность и критический путь

Параллельные полосы после bootstrap (TASK-002):

- **transcript**: 005 → 006 → 007
- **telegram**: 008 → 009
- **transports**: 010
- **spikes**: 003 и 004 — независимы, идут в любой момент

Слияния: 011 требует 008 и 010. 012 требует 010 и 011. 013 требует 010. 014 требует 011 и 013. 015 требует 007, 011, 012, 013. 016 требует 006, 011, 012. 017 требует 011. 018 требует 014, 015, 016, 017.

**Критический путь: 002 → 010 → 011 → 013 → 015 → 018.** Почти-критические: 008 (блокирует 011) и цепочка 005 → 006 → 007 (блокирует 015). Задержка любой из них немедленно становится критической. TASK-019 лежит вне MVP-пути.

---

## 4. Revised steps

Каждый блок ниже в форме `/maw-tasks` batch и пригоден к передаче в создание задач дословно.

---

### TASK-002: Bootstrap Cargo workspace and cctg CLI skeleton

Type: chore
Mode: small-fix
Priority: high
Branch: chore/bootstrap-workspace
Domains: transcript, hub, channel, hooks

**Description.** Создать workspace ровно из двух members: `crates/cctg` (единственный binary с подкомандами `hub`, `agent`, `hook <event>`) и `crates/transcript` (чистая библиотека). Подключить только базовые общие зависимости (`tokio`, `serde`, `serde_json`, `anyhow`, `tracing`, `tracing-subscriber`, `clap`), настроить логирование в stderr и дополнить `.gitignore`. Никакой бизнес-логики, никакого отдельного `proto` крейта.

Dependencies: нет.

Acceptance criteria:
- [ ] `cargo build --workspace`, `cargo test --workspace` и `cargo clippy --workspace --all-targets -- -D warnings` проходят на Windows
- [ ] workspace содержит ровно два package members и собирает ровно один executable `cctg`
- [ ] `cctg --help` показывает `hub`, `agent`, `hook`; никакой путь выполнения, кроме собственно вывода CLI, ничего не пишет в stdout
- [ ] `crates/transcript` не зависит от `tokio`, HTTP-клиента и filesystem-крейтов (проверяется `cargo tree -p transcript`)
- [ ] `.gitignore` покрывает `target/`, `.env`, `*.log`, `.cctg/`, `registry.json`; сборка не оставляет неигнорируемых файлов

---

### TASK-003: Spike — nested claude session identity

Type: chore
Mode: small-fix
Priority: high
Branch: chore/spike-nested-session-detection
Domains: hooks

**Description.** Временным локальным probe-хуком снять для двух сценариев (интерактивный старт и вложенный `claude -p` из Bash-тула) содержимое stdin хука и окружение: `CLAUDECODE`, `CLAUDE_CODE_SESSION_ID`, `CLAUDE_PID`, `CLAUDE_CODE_CHILD_SESSION`, плюс цепочку ppid. Сравнить env-id со stdin `session_id`, определить, работает ли основной признак вложенности и нужен ли fallback через `.cctg/<CLAUDE_PID>` и ppid. Заодно снять полный набор полей `SubagentStart`/`SubagentStop` и проверить, чем именно наполнено тело отчёта субагента. В репозитории нет `.claude/settings.json`, так что probe регистрируется во временном project-scope файле и удаляется после спайка. Production-код не писать.

Dependencies: нет. Разблокирует TASK-012, TASK-015.

Acceptance criteria:
- [ ] в `scratch/` лежат redacted-захваты для top-level и nested старта; ни токенов, ни Telegram id, ни приватных абсолютных путей
- [ ] findings прямо отвечают: чей `session_id` видит вложенный hook в env, и какие из `CLAUDECODE`/`CLAUDE_CODE_CHILD_SESSION`/`CLAUDE_PID` сохраняются
- [ ] отдельно проверен и записан случай отсутствующей или перезаписанной env-переменной, и подтверждено/опровергнуто, что ppid-fallback его закрывает
- [ ] описан один контракт `detect_parent(hook_input, env, process_tree) -> Option<SessionId>` с явным порядком fallback
- [ ] зафиксирован фактический набор полей `SubagentStart`/`SubagentStop` и то, какое поле несёт итоговый отчёт субагента (а не предположение о нём)
- [ ] временный hook и временные settings удалены, что подтверждено read-only проверкой

---

### TASK-004: Spike — development channel lifecycle and launch ergonomics

Type: chore
Mode: small-fix
Priority: high
Branch: chore/spike-channel-lifecycle
Domains: channel

**Description.** На минимальном временном stdio JSON-RPC probe (Rust или любой не-Node процесс) проверить четыре режима запуска: fresh с `--dangerously-load-development-channels server:probe`, `--resume`, `--continue`, и запуск без флага. Отдельно проверить user-scope запись сервера в `~/.claude.json` из новой папки — требуется ли consent. Зафиксировать наблюдением: баннер, вывод `/mcp`, факт спавна сервера, доставку inbound `notifications/claude/channel`, приход `permission_request`. Решение пользователя: обёртки `cctg run` не будет — фиксируется точная команда запуска и пример shell alias. Probe и временный конфиг удалить.

Dependencies: нет. Разблокирует TASK-011, TASK-013.

Acceptance criteria:
- [ ] findings содержат таблицу 4 режима запуска × (баннер / `/mcp` / спавн сервера / inbound доставлен / permission_request получен)
- [ ] поведение при `--resume` и `--continue` **наблюдалось**, а не выведено из документации; если канал при resume не поднимается, это записано как факт с последствием для hub
- [ ] подтверждено или опровергнуто, что user-scope сервер в `~/.claude.json` не требует per-project consent
- [ ] проверено, что происходит при вложенном `claude -p`: документация говорит, что в non-interactive режиме project-scope сервер грузится **без** запроса, значит вложенный запуск может тихо поднять наш сервер — надо убедиться, что он не превращается в самостоятельную маршрутизируемую регистрацию
- [ ] записана точная MVP-команда запуска и пример alias; отсутствие обёртки зафиксировано как принятое решение
- [ ] записано наблюдаемое поведение при отсутствии флага (тихий drop уведомлений), потому что от него зависит состояние «нет канала» в hub
- [ ] probe-процесс и временная конфигурация удалены, секретов не оставлено

---

### TASK-005: transcript — tolerant JSONL parser

Type: feature
Mode: full
Priority: high
Branch: feature/transcript-parser
Domains: transcript

**Description.** Реализовать `parse(&str) -> Vec<Turn>`: пропускать только записи `type: "user"` и `type: "assistant"`, читать лишь нужные поля и блоки с `#[serde(default)]`, молча пропускать неизвестные записи и блоки. Отдельной чистой функцией извлекать первый `ai-title` (в turn он не превращается). Никакого IO, никакого `unwrap()` на входных данных. Добавить обезличенные срезы реальных jsonl в `crates/transcript/tests/fixtures/`.

Dependencies: blocked by TASK-002.

Acceptance criteria:
- [ ] неизвестная запись, неизвестный блок и оборванная последняя строка не теряют ранее разобранные turns и не паникуют
- [ ] пустой вход и вход только из игнорируемых типов дают пустой вектор
- [ ] есть фикстуры: plain text, tool_use + tool_result, thinking + ai-title, sidechain-запись; ни одна не содержит приватного пути, токена или Telegram id
- [ ] `thinking` разбирается ровно настолько, чтобы его можно было гарантированно не отдать наружу, и ни один публичный API его не возвращает
- [ ] крейт не выполняет IO и не использует `unwrap()`/`expect()` на входных данных (проверяется clippy-lint или grep в тесте)
- [ ] `parse` на входе из произвольных байт (fuzz-подобный набор из десятка мусорных строк) не паникует

---

### TASK-006: transcript — brief/full rendering and Telegram sizing

Type: feature
Mode: full
Priority: high
Branch: feature/transcript-renderers
Domains: transcript

**Description.** Реализовать `render_brief` и `render_full` по нормативным правилам домена. Вывод MVP — plain text, чтобы не порождать невалидные Markdown-entity. Разбиение: сначала по turns и строкам, затем Unicode-safe жёсткий split; вернуть куски и явный признак «предпочесть отправку файлом» выше настраиваемого порога. Лимит считать в том же определении длины, которое использует Telegram-слой (символы после парсинга entities, для plain text — символы Unicode).

Dependencies: blocked by TASK-005.

Acceptance criteria:
- [ ] каждый текстовый кусок не превышает 4096 символов и никогда не режет UTF-8 последовательность или суррогатную пару эмодзи
- [ ] brief даёт ровно по одной строке на tool call без входов и результатов; full добавляет входы и усечённые результаты
- [ ] ни один рендерер не выдаёт `thinking` ни в каком режиме
- [ ] один блок в 50 KB и вход с эмодзи на границе куска дают детерминированные куски либо рекомендацию «файлом»
- [ ] рендер синтетического транскрипта на 5000 turns не растёт квадратично и укладывается в зафиксированный в тесте бюджет времени
- [ ] есть публичная функция рендера для набора turns (не только для всего файла), пригодная для инкрементального пуша в TASK-016

---

### TASK-007: transcript — subagent data and collapsed rendering

Type: feature
Mode: full
Priority: medium
Branch: feature/transcript-subagents
Domains: transcript

**Description.** Добавить чистые парсеры содержимого субагентского jsonl и опционального `.meta.json`, плюс модель свёрнутого блока `↳ <type> <id>`. Библиотека принимает строки и опциональные метаданные, файлы не открывает — выбор пути и чтение остаются в hub. Перехваченный отчёт субагента принимается отдельным опциональным входом и имеет приоритет над финальным текстом транскрипта.

Dependencies: blocked by TASK-005. Prefer after TASK-006.

Acceptance criteria:
- [ ] sidechain-фикстура рендерится одним блоком `↳ <type> <id>` и не попадает в top-level turns родителя
- [ ] описание из `.meta.json` используется при наличии; отсутствующий или битый meta даёт безопасный fallback без ошибки
- [ ] переданный отчёт субагента становится телом блока вместо финального текста транскрипта
- [ ] тело блока субагента всегда в brief-форме, даже когда родитель рендерится в full
- [ ] API принимает `&str` и типизированные значения, filesystem-вызовов в крейте нет
- [ ] порядок fallback (отчёт → brief транскрипта → `last_assistant_message`) выражен в типах так, что hub не может его перепутать

---

### TASK-008: hub — Telegram Bot API client and outbound scheduler

Type: feature
Mode: full
Priority: high
Branch: feature/hub-telegram-foundation
Domains: hub

**Description.** Реализовать тонкий Bot API клиент на `reqwest`: envelope `{ok, result, description, parameters}`, узкие serde-структуры только с нужными полями и `#[serde(default)]`, методы `getMe`, `getChatMember`, `getUpdates` (long polling), `sendMessage`, `editMessageText`, `sendDocument`, `deleteMessage`, `answerCallbackQuery`, `createForumTopic`, `editForumTopic`, `getForumTopicIconStickers`. Конфиг из `.env`, chat id в форме `-100...`. На старте проверять право `can_manage_topics` через `getChatMember`. Gate по allowlist `from.id`. Все исходящие операции идут через один планировщик: token bucket 20 сообщений/минуту на группу, FIFO внутри темы, коалесинг повторных правок, отдельная полоса для мутаций тем, приоритет permission-трафика. Любой 429 уважает `retry_after`. Служебные сообщения тем распознаются по `is_topic_message`/`forum_topic_*` и игнорируются роутингом. Зафиксировать измерения (release binary size, clean build time, idle RSS) как основание для возможного будущего перехода на `frankenstein`.

Dependencies: blocked by TASK-002.

Acceptance criteria:
- [ ] отправитель вне allowlist не доходит до обработчиков; ни `from.id`, ни токен не попадают в логи ни на одном пути ошибки (включая URL в тексте ошибки reqwest)
- [ ] планировщик соблюдает 20 сообщений/минуту на группу в тесте с подменённым временем и сохраняет порядок внутри темы
- [ ] повторные правки одного сообщения коалесятся; смоделированный 429 повторяется не раньше `retry_after` и не порождает retry storm
- [ ] создание и правка тем идут отдельной полосой и не списываются из message-bucket; численный лимит мутаций нигде не захардкожен
- [ ] апдейты со служебными сообщениями тем (`forum_topic_created/edited/closed/reopened`) распознаются и не роутятся как пользовательский ввод
- [ ] десериализация апдейта с неизвестными полями и неизвестным типом апдейта не роняет поллинг
- [ ] отсутствие права `can_manage_topics` обнаруживается на старте с внятной ошибкой, а не при первом `createForumTopic`
- [ ] в заметках задачи записаны измеренные binary size / build time / idle RSS

---

### TASK-009: hub — local transcript commands

Type: feature
Mode: full
Priority: high
Branch: feature/hub-transcript-commands
Domains: hub, transcript

**Description.** Первый вертикальный срез: команды `/brief [n]` и `/full [n]` читают локальный транскрипт по переданному пути, прогоняют через `transcript` и отвечают кусками через планировщик либо документом выше порога. Смещение `getUpdates` сохраняется атомарно, чтобы рестарт не обрабатывал команды повторно. Реестр слотов ещё не требуется.

Dependencies: blocked by TASK-006, TASK-008.

Acceptance criteria:
- [ ] `/brief` и `/full` на фикстурах совпадают с выводом библиотеки и сохраняют порядок кусков
- [ ] крупный вывод уходит документом; ошибка Telegram 400 по размеру один раз переключает доставку на документ, а не зацикливается
- [ ] сохранённое смещение исключает повторную обработку апдейта после смоделированного рестарта
- [ ] нечитаемый или отсутствующий путь даёт понятное пользователю сообщение, а поллинг продолжает работать
- [ ] ни логи, ни тесты, ни фикстуры не содержат токена, реальных user id и приватных путей

---

### TASK-010: cctg transport contracts — agent TCP and hook HTTP ingress

Type: feature
Mode: full
Priority: high
Branch: feature/transport-contracts
Domains: hub, channel, hooks

**Description.** Во внутренних модулях `cctg` (`src/wire.rs`, ingress в `src/hub/`) определить версионированный newline-JSON протокол persistent-соединения agent↔hub по TCP (первым сообщением shared secret, ограничение длины строки, reconnect только на стороне агента) и отдельные serde-пейлоады плюс HTTP endpoint для одноразового POST от хука. Listener по умолчанию слушает loopback; не-loopback адрес требует явной настройки. Никакого отдельного `proto` крейта и никакого reconnect-цикла в хуке.

Dependencies: blocked by TASK-002.

Acceptance criteria:
- [ ] все варианты TCP-сообщений round-trip через serde; неизвестная версия или неизвестный вид сообщения дают контролируемую ошибку без паники
- [ ] неверный secret отвергается до обработки Register и не попадает в логи; строка сверх лимита закрывает соединение с ограниченной аллокацией
- [ ] reconnect с backoff и повторный Register на стороне агента доказаны тестом с рестартом hub
- [ ] hook endpoint принимает один аутентифицированный POST и отвечает быстро; повторная доставка того же события идемпотентна по ключу события
- [ ] loopback по умолчанию и явная не-loopback конфигурация покрыты тестами
- [ ] shared secret не логируется ни на одном пути, включая ошибки парсинга

---

### TASK-011: hub — slot registry and topic lifecycle

Type: feature
Mode: full
Priority: high
Branch: feature/hub-slot-registry
Domains: hub

**Description.** Реализовать **слотовый** реестр и его атомарное сохранение и сверку. Слот это `(device, folder, ordinal)` и владеет темой навсегда. Новая сессия занимает первый слот своей папки без живой сессии; если таких нет — создаётся следующий ordinal и новая тема. Смена сессии внутри слота пишет строку-разделитель `── session <short-id> · new | resumed ──`. Заголовок `[host] folder · ai-title` (+ `#N` при ordinal > 1), короткий id пока нет `ai-title`, жёсткое усечение под 128 символов. Состояние слота — `icon_custom_emoji_id` из набора `getForumTopicIconStickers`: alive / dead / waiting permission / no channel. `SessionEnd` **не закрывает** тему, только меняет состояние. Вложенный запуск и субагент слота не получают: у них пишется `parent_session_id` и ссылка на слот родителя. Hook может создать слот и тему до подключения агента — такой слот показывает состояние «нет канала». После каждого `editForumTopic` hub удаляет пришедшее служебное `forum_topic_edited` через `deleteMessage`.

Dependencies: blocked by TASK-008, TASK-010. Prefer after TASK-004.

Acceptance criteria:
- [ ] вторая сессия в той же папке, стартовавшая после смерти первой, создаёт **ноль** новых тем и переиспользует слот; в теме появляется ровно один разделитель смены сессии
- [ ] две одновременные живые сессии в одной папке дают ровно две темы, вторая с `#2` в заголовке; третья одновременная даёт `#3`
- [ ] вложенная регистрация (nested / subagent) создаёт ноль тем и ссылается на слот родителя
- [ ] `SessionEnd` не вызывает `closeForumTopic`; состояния alive / dead / waiting / no-channel меняют `icon_custom_emoji_id` на значение из допустимого списка
- [ ] заголовок всегда ≤128 символов, при усечении сохраняет `[host]` и `#N`, а короткий id заменяется на `ai-title` при его появлении
- [ ] прерванное сохранение оставляет предыдущий валидный `registry.json`; ошибка `TOPIC_ID_INVALID` очищает только конкретную устаревшую привязку и создаёт ровно одну замену
- [ ] служебное `forum_topic_edited` после смены иконки удаляется; отсутствие права `can_delete_messages` логируется один раз и не ломает работу
- [ ] сессия, зарегистрированная только хуком, видна как «нет канала», а подключившийся позже агент привязывается к тому же слоту без создания второй темы

---

### TASK-012: hook — lifecycle events and subagent report capture

Type: feature
Mode: full
Priority: high
Branch: feature/hook-subcommand
Domains: hooks

**Description.** `cctg hook <event>` читает stdin, десериализует только нужные поля и делает один аутентифицированный HTTP POST с коротким таймаутом, всегда завершаясь `exit 0`. Поддержать шесть событий: `SessionStart`, `SessionEnd`, `Stop`, `UserPromptSubmit`, `SubagentStart`, `SubagentStop`. Дополнительно — узкий `PreToolUse`/`PostToolUse` matcher на `SubagentHandback`, который передаёт `tool_input.message` как отчёт субагента (документировано для 2.1.271+, см. раздел 0.1), плюс `agent_transcript_path` и `last_assistant_message` из `SubagentStop`. TASK-003 подтверждает payload на текущей версии, но механизм не гейтится на спайк. Признак вложенности вычисляется по зафиксированному в TASK-003 правилу. Критично по таймингу: у `SessionEnd`-хуков общий бюджет **1.5 секунды** (документировано), у `UserPromptSubmit` — 30 с, у `Stop` и обычных command-хуков — 600 с. Значит POST для `SessionEnd` должен иметь таймаут заведомо меньше 1.5 с, иначе hook будет убит и событие смерти сессии потеряется. Snippet регистрации пишется в user scope, без машинно-специфичных секретов.

Dependencies: blocked by TASK-010, TASK-011. Prefer after TASK-003.

Acceptance criteria:
- [ ] каждое из шести событий даёт ожидаемый HTTP-пейлоад с нужными полями и ничем лишним
- [ ] недоступный hub: `exit 0` в пределах настроенного короткого таймаута, stdout пуст, в stderr нет ни входного пейлоада, ни секретов
- [ ] признак вложенности и parent id соответствуют правилу, зафиксированному в TASK-003, для top-level и nested случаев
- [ ] битый, пустой или обрезанный stdin даёт `exit 0` без паники
- [ ] `SessionEnd` измерен и укладывается заведомо в 1.5 с даже при недоступном hub (таймаут POST выставлен с запасом под этот бюджет)
- [ ] snippet настроек регистрирует все события и не содержит секретов и машинно-специфичных путей
- [ ] отчёт субагента захватывается matcher-ом на `SubagentHandback` (`tool_input.message`), а `SubagentStop` передаёт `agent_transcript_path`, `agent_type`, `agent_id`, `last_assistant_message`; события с пустым `agent_type` или с `agent_type`, равным имени `--agent` сессии без соответствующего SubagentStart, отбрасываются как внутренние
- [ ] `source` читается только из `SessionStart` (для остальных событий он не документирован) и его отсутствие не считается ошибкой

---

### TASK-013: agent — Channel MCP server over stdio

Type: feature
Mode: full
Priority: high
Branch: feature/agent-channel-server
Domains: channel

**Description.** Реализовать вручную написанную JSON-RPC поверхность канала: `initialize` (с `capabilities.experimental["claude/channel"]`, опционально `claude/channel/permission` и `tools`), `notifications/initialized`, `tools/list`, `tools/call` (`reply`), исходящие `notifications/claude/channel` и `.../permission`, входящий `.../permission_request`. Всё остальное — method-not-found. Persistent TCP к hub, сопоставление сессии через env `CLAUDE_CODE_SESSION_ID` и реестр хуков. Ключи meta только `[A-Za-z0-9_]+`: невалидные отбрасываются, а не переименовываются молча. Команда установки в user scope использует абсолютный путь к исполняемому файлу. stdout — исключительно JSON-RPC.

Dependencies: blocked by TASK-010. Prefer after TASK-004, TASK-011.

Acceptance criteria:
- [ ] сценарий initialize → initialized → tools/list → tools/call выдаёт ровно по одному валидному JSON-объекту на строку stdout
- [ ] неизвестный метод возвращает `-32601`; битый внешний ввод не паникует и не убивает сервер без контролируемого ответа
- [ ] stdout никогда не содержит логов и текста паники, в том числе при недоступном hub; логи идут только в stderr или файл
- [ ] невалидный ключ meta отбрасывается, валидные ключи и значения сохраняются байт в байт
- [ ] разрыв и восстановление соединения с hub не завершают MCP-сервер и приводят к повторной регистрации той же сессии
- [ ] ручной запуск подтверждает баннер и доставку inbound в сессию; вложенный запуск не регистрируется как самостоятельный маршрутизируемый канал

---

### TASK-014: permission relay end to end

Type: feature
Mode: full
Priority: high
Branch: feature/permission-relay
Domains: channel, hub

**Description.** Транслировать `permission_request` в тему слота с кнопками Allow/Deny, приоритетом в планировщике и gate по allowlist. В `callback_data` только действие и пятибуквенный request id — гарантированно ≤64 байт. Побеждает первый ответ; поздний вердикт становится безвредным «уже решено». Итоговое сообщение hub ограничивает по 4096 символов независимо от того, что прислал Claude.

Dependencies: blocked by TASK-011, TASK-013.

Acceptance criteria:
- [ ] `callback_data` Allow/Deny укладывается в 64 байта и порождает ровно один соответствующий вердикт
- [ ] callback от отправителя вне allowlist не отправляет вердикт и не раскрывает деталей запроса
- [ ] второй или поздний callback идемпотентен: помечает запрос решённым и не шлёт второй вердикт
- [ ] длинный `input_preview` даёт сообщение ≤4096 символов, а permission-трафик обгоняет очередь транскрипта (проверяется на заполненной очереди)
- [ ] permission-запрос попадает в тему того слота, которому принадлежит сессия, даже если сессия сменилась в слоте после старта запроса
- [ ] живая проверка подтверждает: подтверждение из Telegram закрывает параллельный терминальный диалог

---

### TASK-015: subagents and nested runs inside the parent slot

Type: feature
Mode: full
Priority: high
Branch: feature/subagents-nested-routing
Domains: hub, hooks, transcript

**Description.** Связать явный родительский вызов `Agent` и его результат с `agent_id`; показывать только скоррелированные субагенты, а несопоставленные внутренние события не превращать в блоки-призраки. Тело блока на stop выбирается по порядку: перехваченный отчёт → brief по `subagents/agent-<id>.jsonl` → `last_assistant_message`. Вложенный `claude -p` прикрепляется к теме слота родителя как `⇣ nested <id>`, не создаёт ни слота, ни темы, ни маршрута канала. Ответ пользователя на блок субагента уходит в родительскую сессию с meta `target_agent=<agent_id>`.

Dependencies: blocked by TASK-007, TASK-011, TASK-012, TASK-013. Prefer after TASK-014.

Acceptance criteria:
- [ ] три явных субагента в одной сессии дают одну тему и ровно три скоррелированных блока
- [ ] несопоставленные внутренние события `SubagentStop`, включая случай сессии, запущенной с `--agent`, не создают блоков-призраков
- [ ] фикстура с перехваченным отчётом показывает доставленный отчёт; весь порядок fallback покрыт тестами, включая отстающий файл транскрипта
- [ ] вложенный `claude -p` даёт ноль новых тем и ровно один блок `⇣ nested <id>` в теме родителя
- [ ] ответ на блок субагента приходит только в канал родителя и несёт валидный ключ meta `target_agent`
- [ ] рестарт hub не создаёт дублей тем и блоков; незавершённый блок помечается детерминированно

---

### TASK-016: hub — live turn streaming into the slot topic

Type: feature
Mode: full
Priority: high
Branch: feature/hub-turn-streaming
Domains: hub, transcript

**Description.** Закрыть решение пользователя «push every turn через outbound queue»: hub следит за локальным транскриптом сессии по `transcript_path` (tail по смещению, только целые строки), скармливает новые записи в `transcript` и отправляет каждый завершённый turn в brief-виде в тему слота через планировщик. Финальный текст ассистента на `Stop` берётся из полей хука, а jsonl используется как история и может отставать. Смещение хранится в реестре, чтобы рестарт hub не переотправлял уже отправленное. Удалённые устройства (агент отдаёт файл) — вне этой задачи, это шаг 5 плана.

Dependencies: blocked by TASK-006, TASK-011, TASK-012.

Acceptance criteria:
- [ ] дописывание строк в тестовый jsonl приводит к отправке соответствующих turns в тему нужного слота в правильном порядке
- [ ] частично записанная последняя строка никогда не отправляется и не теряется: она уходит следующей итерацией целиком
- [ ] рестарт hub не переотправляет уже отправленные turns (смещение персистится) и не теряет дописанное во время простоя
- [ ] поток turns подчиняется планировщику (20/мин на группу, FIFO в теме) и уступает permission-трафику
- [ ] ротация сессии в слоте начинает новый поток с новым смещением и пишет разделитель ровно один раз
- [ ] отсутствующий или удалённый файл транскрипта не ломает hub: одно предупреждение, поллинг продолжается

---

### TASK-017: hub — dead slot buffering and resume affordance

Type: feature
Mode: full
Priority: medium
Branch: feature/dead-slot-buffer
Domains: hub

**Description.** Реализовать поведение мёртвого слота по решению пользователя: тема остаётся открытой, состояние меняется на dead, входящие сообщения буферятся (до 50 на слот; при переполнении дропается старейшее и один раз в теме печатается предупреждение), в теме появляется кнопка Resume. Буфер переживает рестарт hub вместе с реестром. Сама кнопка в этой задаче только фиксирует намерение и отвечает понятным сообщением, если исполнение резюма ещё не реализовано (TASK-019); когда сессия в слоте оживает, буфер доставляется в неё в исходном порядке и очищается.

Dependencies: blocked by TASK-011.

Acceptance criteria:
- [ ] сообщение в тему мёртвого слота попадает в буфер, тема не закрывается, состояние отображается иконкой dead
- [ ] 51-е сообщение вытесняет самое старое, предупреждение печатается ровно один раз на период мёртвости, а не на каждое сообщение
- [ ] оживление сессии в слоте доставляет буфер в исходном порядке ровно один раз и очищает его
- [ ] буфер переживает рестарт hub и не дублируется после восстановления
- [ ] кнопка Resume несёт в `callback_data` идентификатор в пределах 64 байт и отвечает понятным сообщением, если исполнение ещё не подключено
- [ ] ни буфер, ни его персист не содержат Telegram user id в логах

---

### TASK-018: multi-slot routing soak

Type: chore
Mode: full
Priority: medium
Branch: chore/multi-slot-soak
Domains: hub, channel, hooks

**Description.** QA-гейт для шага 4. Сценарий: две одновременные top-level сессии в папке A, одна top-level в папке B, один вложенный `claude -p` внутри одной из сессий A, плюс пятый top-level в папке A, стартующий при выключенном hub и после того, как первая сессия A завершилась. Ожидаемые темы: **три** — `[host] A`, `[host] A #2`, `[host] B`. Вложенный запуск темы не получает. Пятая сессия переиспользует освободившийся слот `[host] A` и добавляет разделитель, новой темы не создаёт. Проверить роутинг, восстановление, приоритеты, счётчики сообщений и итоговый реестр.

Dependencies: blocked by TASK-014, TASK-015, TASK-016, TASK-017.

Acceptance criteria:
- [ ] пять запусков дают ровно три темы и ни одного перепутанного маршрута
- [ ] пятая сессия, стартовавшая при выключенном hub, после его возврата занимает освободившийся слот, пишет один разделитель и не создаёт темы
- [ ] всплеск сообщений соблюдает политику планировщика; каждый 429 обработан по `retry_after`, retry storm не возникает
- [ ] permission-запрос проходит впереди очереди транскрипта, задержка измерена и записана
- [ ] итоговый `registry.json` точно описывает три слота, текущие сессии в них и связь вложенного запуска с родителем
- [ ] отчёт отдельно считает send / edit / topic-create операции и не сравнивает правки с неподтверждённым лимитом 20/мин
- [ ] в темах нет накопившихся служебных сообщений `forum_topic_edited`

---

### TASK-019 (пост-MVP, шаг 6): headless resume and context handoff

Type: feature
Mode: full
Priority: low
Branch: feature/headless-resume
Domains: hub, hooks

**Description.** Вне MVP (шаги 1–4), но требуется решением пользователя о модели слотов, поэтому зафиксировано здесь. Кнопка Resume запускает через агента устройства `claude -p --resume <id> --output-format stream-json`; оживший процесс занимает тот же слот. Раздутый контекст лечится handoff-ом: старой сессии заказывается summary (`claude -p --resume <old>`), новая сессия стартует в том же слоте с этим summary первым промптом, разделитель помечается как `handoff`. Для headless-resumed сессий выставляется `CLAUDE_AUTOCOMPACT_PCT_OVERRIDE`. Inline `/compact` в headless недоступен — это проверено пользователем и не подлежит перепроверке.

Dependencies: blocked by TASK-017, TASK-018.

Acceptance criteria:
- [ ] Resume поднимает сессию через агента нужного устройства и привязывает её к тому же слоту без новой темы
- [ ] буфер мёртвого слота доставляется в поднятую сессию в исходном порядке
- [ ] handoff создаёт новую сессию в том же слоте с summary первым промптом и разделителем `handoff`
- [ ] `CLAUDE_AUTOCOMPACT_PCT_OVERRIDE` выставляется только для headless-resumed процессов и записан в заметках задачи с фактическим значением
- [ ] сбой запуска даёт понятное сообщение в теме, слот не теряет состояние, повторное нажатие не плодит процессы

---

## 5. Risk areas

- **Channel API остаётся research preview.** Пользовательский сервер требует development-флага при каждом запуске; без флага уведомления молча теряются и подтверждения доставки нет. TASK-004 обязана зафиксировать реальную семантику resume, а hub — явно показывать состояние «нет канала», иначе пользователь не отличит «Claude молчит» от «канал не поднят».
- **Детект вложенности опирается на переменную, которой нет в документации.** `CLAUDE_CODE_SESSION_ID`, `CLAUDE_PID` и `CLAUDE_CODE_CHILD_SESSION` в официальной документации не упоминаются ни разу; документированы только `CLAUDE_PROJECT_DIR`, `CLAUDE_ENV_FILE`, `CLAUDE_EFFORT`. Наблюдения в `CLAUDE.md` верны для этой машины и этой версии, но это недокументированное поведение и оно может исчезнуть при апгрейде. Поэтому ppid-fallback обязателен, а детект живёт в одной изолированной функции, чтобы его можно было починить в одном месте.
- **Неоднозначность субагентских событий.** Пустой `agent_type` не единственный внутренний случай. Поведение `agent_type` при запуске основной сессии с `--agent` в документации не описано, так что «он будет равен имени main agent» — гипотеза, а не факт; проверять фикстурой. Корреляция с явным родительским вызовом `Agent` обязательна: лучше не показать блок, чем показать ложный.
- **Источник тела блока субагента не имеет документированного контракта.** Поля с путём к субагентскому транскрипту в схеме хуков нет, запаздывание файла не документировано, рецепта перехвата отчёта в документации нет. Отсюда цепочка fallback и самостоятельное построение пути в hub, а не опора на payload.
- **Telegram flood control.** 20 сообщений/минуту относится к отправке в группу; численных лимитов на правки и создание тем Telegram не публикует. Приоритеты, коалесинг и `retry_after` надёжнее выдуманного общего bucket. Модель слотов дополнительно снижает частоту создания тем.
- **Служебный шум в темах.** Hub меняет иконку состояния часто, каждая смена порождает `forum_topic_edited`. Без уборки через `deleteMessage` тема замусоривается; при отсутствии права `can_delete_messages` нужно деградировать (реже менять иконку), а не спамить.
- **Выбор Telegram-клиента сделан без измерений веса.** Аргументы за `reqwest` — свежесть схемы, размер нужной поверхности и нормативный инвариант «не моделировать весь Bot API». Цифр binary size / RSS / build time у меня нет; TASK-008 обязана их получить, иначе будущий пересмотр снова будет на вкус.
- **Windows-специфика.** Атомарная замена `registry.json`, кодировка путей и обход ppid должны тестироваться на Windows, а не переноситься из Linux-референсов. Отдельно: curl на Windows ломает UTF-8 в аргументах — это особенность инструмента, а не API, и не должна попасть в тесты как «ограничение Telegram».
- **Утечка секретов и идентичности.** Токен бота легко попадает в URL внутри цепочки ошибок reqwest; shared secret и Telegram id нельзя логировать и нельзя класть литералами в тесты и фикстуры. Тесты должны генерировать эфемерные значения в рантайме и проверять редактирование логов.
- **Буфер мёртвого слота.** 50 сообщений — решение пользователя, а не выдуманный дефолт. Персист буфера вместе с реестром обязателен, иначе рестарт hub тихо теряет пользовательский ввод.
