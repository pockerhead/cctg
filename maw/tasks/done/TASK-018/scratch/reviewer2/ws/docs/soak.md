# Soak: несколько слотов на одном устройстве (TASK-018)

`crates/cctg/tests/soak.rs` прогоняет пять запусков на одном устройстве и проверяет, что темы, маршруты и реестр сходятся:

1. Две одновременные top-level сессии в папке A и одна в папке B: три темы `[host] A`, `[host] A #2`, `[host] B`.
2. Сообщения из каждой темы доходят только до агента своей сессии; `reply`, ответ хода (`Stop`) и строки транскрипта приходят в свою тему.
3. Вложенный `claude -p` внутри первой сессии A: темы нет, блок `⇣ nested` в теме родителя, в реестре `nested` с родителем. Вложенность находит сам `cctg hook` по дереву процессов.
4. Всплеск строк транскрипта в двух темах, два 429 от фейка, два permission-запроса во время очереди: запрос обгоняет строки своей темы, 429 останавливает всю очередь на `retry_after`, отказанный вызов повторяется один раз.
5. Первая сессия A завершается, hub останавливается, пятая сессия A стартует при выключенном hub (её `SessionStart` уходит в спул), hub возвращается: пятая занимает слот `[host] A`, один разделитель, новой темы нет.

В конце проверяются: ровно три `createForumTopic`, ни одного текста сессии в чужой теме, все `forum_topic_edited` своих тем удалены, `registry.json` поле за полем (точные наборы ключей; три слота, текущие сессии, пути транскриптов, смещения стрима, `pids`, вложенный запуск с родителем и его блок). Упавшая проверка не оставляет ни процессов hub, ни дублёров, ни временной папки.

## Как устроено

Настоящие процессы `cctg hook` и `cctg agent`, настоящие `serve_hooks`, `serve_agents`, `Slots`, `Scheduler` и `updates::poll`. Claude Code заменён процессом-дублёром с именем `claude.exe` (копия тестового бинарника): он запускает хуки и агента своими детьми с тем окружением, что ставит Claude Code (`CLAUDE_PID`, `CLAUDE_CODE_SESSION_ID`, `CLAUDE_CODE_ENTRYPOINT`). Дублёр top-level сессии стартует через промежуточный процесс, который сразу выходит, поэтому цепочка процессов над ним обрывается, как у сессии из терминала, даже если тест запущен из Claude Code. Дублёр вложенного запуска это ребёнок дублёра A1, и хук находит родителя сам.

Home, `~/.cctg`, `CLAUDE_CONFIG_DIR` и состояние hub лежат во временной папке, настоящие `~/.cctg` и `~/.claude` не трогаются. Окон нет.

## Прогон с фейковым Telegram (по умолчанию)

```
cargo test -p cctg --test soak -- --ignored
```

Около 15-30 с. Без `--ignored` бинарник только печатает `soak: skipped`, поэтому обычный `cargo test --workspace` его не ждёт. Отчёт печатается в stdout; `CCTG_SOAK_REPORT=<абсолютный путь>` дополнительно пишет его в файл (относительный путь считается от `crates/cctg`, там cargo запускает тест).

## Живой прогон с настоящим ботом

Делает человек или оркестратор, не агент, который пишет код: тест сам читает `.env` (токен, чат, allowlist) и никуда его не выводит.

1. Остановить работающий `cctg hub`: у бота один читатель `getUpdates`, второй получает 409. Тест подтверждает прочитанные апдейты, поэтому сообщения, написанные боту во время прогона, штатный hub потом не увидит.
2. Запустить из корня репозитория:

   ```
   CCTG_SOAK_LIVE=1 CCTG_SOAK_REPORT="$PWD/soak-live.md" cargo test -p cctg --test soak -- --ignored
   ```

   Другой файл настроек: `CCTG_SOAK_ENV=<путь>`. Прогон занимает несколько минут: бакет настоящий (20 сообщений в минуту), всплеск меньше, чем в фейке.
3. Успех: exit 0 и `soak: ok`. Перед стартом тест проверяет права бота (`can_manage_topics`, `can_delete_messages`). Он создаёт три темы `[soakbox] A`, `[soakbox] A #2`, `[soakbox] B`, удаляет служебные `forum_topic_edited` только в них (чужие темы hub не трогает, их служебные сообщения тест не считает) и в конце удаляет эти три темы прямым `deleteForumTopic`, также при упавшей проверке. Если удалить не вышло, тест падает и печатает id тем: их надо удалить руками.

Сообщения пользователя и нажатия кнопок тест подаёт hub напрямую с синтетическими id. `setMessageReaction` и `answerCallbackQuery` на них в Telegram не уходят: транспорт теста отвечает сам, в отчёте это "answered locally". 429 в живом режиме не подстраиваются, только считаются, если случились. Ответы (`Stop`) ходов, прошедших при выключенном hub, и `SessionEnd` чужих завершившихся сессий не досылаются: это известные ограничения спула.

## Шаблон отчёта

```
# TASK-018 soak report

mode: <fake Telegram | live (real bot)>; duration <s>; <N> Telegram calls; 2 hub runs

| operation | Bot API method | accepted |
|---|---|---|
| send | sendMessage (replies, answers, notices, separators, blocks) | |
| permission | sendMessage (permission prompts) | |
| stream | sendMessage (transcript stream) | |
| document | sendDocument | |
| edit | editMessageText | |
| react | setMessageReaction | |
| callback | answerCallbackQuery | |
| create_topic | createForumTopic | 3 |
| edit_topic | editForumTopic | |
| delete | deleteMessage (service messages) | |

- topics: 3 (A, A #2, B); separators: 1; service messages: <shown> shown, <deleted> deleted, 0 left
- 429: <n> (retry_after <s>), each followed by a pause of the whole queue and one retry; other errors: <n>; answered locally (live: reactions and callback answers on simulated ids): <n>
- permission latency (request written to the agent -> sendMessage): A <ms> (other topic), A #2 <ms> (own topic behind its stream); A #2 burst lines written before the request and sent after the prompt: <n>
- stream: <lines> lines in <messages> messages; metered sends peak at <p>% of the bucket in any 1, 3 or 60 s window; smallest gap <ms>
- edits and topic calls are counted separately and are not compared with the 20 messages/min group limit: Telegram publishes no number for them
- registry.json: 3 slots (A: a5a5a5a5, A #2: a2a2a2a2, B: b1b1b1b1), nested ee0e0e0e -> parent a1a1a1a1
```
