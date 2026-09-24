# TASK-029 PLAN_FINAL: закреплённое сообщение статуса с кнопкой ⏹

Эталон: `maw/tasks/in_progress/TASK-029/scratch/reviewer2/` (дальше `R/`). Это patch planner-а (`scratch/planner/task029.patch`), из которого убран весь Ctrl+B/⏬, плюс исправления ревью (раздел 5) и новые тесты. Рабочая копия и target были вне репозитория (`%TEMP%`) и удалены; `R/task029.patch` воспроизводит всё.

## 1. Summary

У каждого слота с темой одно сообщение статуса: hub отправляет его один раз (после разделителя живой текущей сессии), закрепляет один раз (`pinChatMessage`, без уведомления) и дальше только редактирует, не чаще раза в 5 с. Служебное «бот закрепил» удаляется, только если его автор сам бот (`getMe.id`) и закреплено наше сообщение. Первая строка: `🏁 Сессия завершена` / `❓ Ждёт разрешения` / `⏹ Esc отправлен в терминал` / `⚙️ <brief-строка вызова> (+N)` / `💭 Думает` / `💤 Ждёт вас`. Вторая: цифры statusline (`Opus 5.5 · high · ctx 50% · 5h 3% · 7d 92%`), которые хранятся на сессию в `registry.json` и переживают рестарт hub. Источники: `UserPromptSubmit`, `Stop`, async-хук `cctg hook ToolStatus` (Pre/PostToolUse/PostToolUseFailure), результаты вызовов и заметка `[Request interrupted by user` в потоке транскрипта, permission prompts, конец сессии, и новая подкоманда `cctg statusline` в `statusLine` cctg-сессий: шлёт цифры в hub (таймаут 80 мс, параллельно) и печатает вывод пользовательской `statusLine.command` байт в байт с её кодом выхода, а без неё свою строку. Кнопка ⏹ (подтверждение вторым нажатием за 10 с) доходит только до агента живой текущей сессии этого слота с capability `console_keys` и не показывается, пока ждёт permission prompt. Агент (Windows) пишет Esc в консоль своего claude через `WriteConsoleInputW` и отвечает `console_key_written { written }`. `written=true` показывается как «Esc отправлен», а не как конец хода. Провод совместим: `VERSION` = 1, новые типы за capability, новые поля `serde(default)`.

## 2. Implementation steps

### Шаг 0. Применить эталон (обязательно, до любых правок)

В checkout ветки `feature/status-interrupt` на HEAD `4a003b1` (код и docs совпадают с `main`, `1252ff8`):

```
git apply --check maw/tasks/in_progress/TASK-029/scratch/reviewer2/task029.patch
git apply maw/tasks/in_progress/TASK-029/scratch/reviewer2/task029.patch
bash maw/tasks/in_progress/TASK-029/scratch/reviewer2/verify_hashes.sh
```

Ожидание: `git apply` без ошибок (проверено на свежем clone с `core.autocrlf=true`: git сам переводит CRLF, `--ignore-whitespace` не нужен), `verify_hashes.sh` печатает 34 строки `OK` и выходит с 0. Хеши считаются по LF (CR вырезается). `git apply --numstat`: 34 файла, +3594 −59. Ничего поверх не менять. Если что-то всё же надо поменять: рабочая копия вне репозитория (`git -c core.autocrlf=false archive HEAD Cargo.toml Cargo.lock crates docs | tar -x -C %TEMP%\<dir>`, `git apply R/task029.patch`), правка, затем `python R/build_patch.py <dir>` пересобирает `task029.patch` и `hashes.txt`.

### Что лежит в патче (по файлам, чтобы code review сверил diff с намерением)

1. **`crates/cctg/src/wire.rs`.** `Register.console_keys: bool` (`serde(default)`). `enum ConsoleKey { Interrupt }` (snake_case, только Esc). `HubMsg::ConsoleKey { key_id: u64, key }`, шлётся только агенту с `console_keys`. `AgentMsg::ConsoleKeyWritten { key_id: u64, written: bool }`, kind `console_key_written`: значит только «записано в буфер консоли», не «ход прерван». `HookEvent::ToolStart { tool_use_id, line }`, `ToolEnd { tool_use_id }`, `StatusLine { model, effort, context, five_hour, seven_day }` (все `Option`, `serde(default)`), `kind()`, `KINDS`, `is_frequent()`. `VERSION` не трогать. Тесты: round-trip образцы, `console_keys_and_status_events_stay_compatible_with_version_one_peers` (старый register без поля = false; неизвестная клавиша = `Malformed`).
2. **`crates/transcript/src/render.rs` + `lib.rs`.** `pub fn call_line(name, input)` = `tool_line(name, input, None, None)`, реэкспорт; тест `a_call_line_from_hook_input_matches_the_streamed_line`. Строка статуса совпадает со строкой потока.
3. **`crates/cctg/src/hook.rs`.** `TOOL_STATUS_EVENT = "ToolStatus"`, `build_tool_status` с узким `ToolStatusInput` (`serde(default)`). Без POST: нет `session_id`/`tool_use_id`/`tool_name`, непустой `agent_id` (вызов внутри субагента), `SubagentHandback`, другое `hook_event_name`, не JSON. `PreToolUse` -> `ToolStart` (строка через `transcript::call_line`, cap 512 байт, id cap как `MAX_TOOL_NAME`); `PostToolUse`/`PostToolUseFailure` -> `ToolEnd`. Обход дерева процессов не делается. `post_timeout` 300 мс для `ToolStart/ToolEnd/StatusLine`. `read_stdin` стал `pub(crate)`. Старые `PreToolUse`/`PostToolUse` (handback) не меняются. Тесты `tool_status_carries_the_call_line`, `tool_status_skips_subagent_calls_handbacks_and_other_events`, таймауты.
4. **`crates/cctg/src/statusline.rs` (новый) + `main.rs` + `lib.rs` + `device.rs`** (`home_dir` стал `pub(crate)`):
   - stdin до 1 MiB, ожидание до 500 мс; поля терпимо: null, отсутствие, чужой тип = нет поля. Проценты только в `0..=100` (документированный диапазон), округление; всё вне диапазона отбрасывается.
   - Если env `CCTG_STATUSLINE` не задан и есть `session_id`: одно событие `status_line` через `hook::post`, `POST_TIMEOUT = 80 мс`, в отдельной задаче параллельно с командой пользователя; без секрета ничего не шлётся.
   - Команда пользователя: `statusLine.command` из `$CLAUDE_CONFIG_DIR/settings.json`, иначе `<home>/.claude/settings.json`, только при `type: "command"`, читается на каждом вызове. Shell как у Claude Code: Git Bash (`CLAUDE_CODE_GIT_BASH_PATH`, затем `SHELL`, если это файл `bash.exe`, затем `EXEPATH\bash.exe`, затем `bin\bash.exe` рядом с `git.exe` из `PATH`), иначе `powershell -NoProfile -Command`; не на Windows `sh -c`. Тот же stdin, env `CCTG_STATUSLINE=1` (вложенный вызов не постит и не зовёт команду снова), stderr в null, вывод до 64 KiB, лимит 10 с.
   - Вывод команды печатается байт в байт (пустой, ANSI, несколько строк), процесс выходит с её кодом выхода (`status.code()`, без кода = 1): по документации Claude Code гасит строку при ненулевом коде или пустом выводе, так и без cctg. Если команды нет, она не запустилась или не уложилась в 10 с: своя строка `model · ctx N% · 5h N% · 7d N%`, код 0.
   - `main.rs`: `Command::Statusline`; плохие аргументы = exit 0 и одна строка в stderr; фиксированный panic-текст; tracing в stderr; `std::process::exit(statusline::run().await)`.
5. **`crates/cctg/src/keys.rs` (новый) + `crates/cctg/Cargo.toml`** (фичи `windows-sys`: `Win32_Security`, `Win32_Storage_FileSystem`, `Win32_System_Console`; крейт уже в lock-файле, скачивания нет). `SUPPORTED = cfg!(windows)`, `key_event(Interrupt) = (0x1B, 0x01, 0x1B, 0)`, `press(claude_pid, key)`: глобальный mutex, `SetConsoleCtrlHandler(None, TRUE)`, `FreeConsole`, `AttachConsole(pid)`, `CreateFileW("CONIN$")`, `WriteConsoleInputW` (down+up, `written == 2`), `CloseHandle`, `FreeConsole`. Вне Windows `false`. В тестах `press` не вызывается (отцепил бы тест-раннер от консоли).
6. **`crates/cctg/src/agent.rs` + `channel.rs`.** `type Presser`; `run_stdio` считает `claude_pid` один раз (`proctree::current_lineage`, не env `CLAUDE_PID`); `Register.console_keys = presser.is_some()` (Windows и известный pid). `serve_channel(.., presser)`, воркер `spawn_presser`: очередь 4, по одному через `spawn_blocking`, ответ `ConsoleKeyWritten`; без presser `written: false`; переполнение = ключ отброшен. `channel.rs`: `HubMsg::ConsoleKey` в ветке «не канал», до Claude Code не доходит. Тесты по реальному TCP: `a_console_key_is_pressed_answered_and_never_reaches_claude`, `without_a_presser_a_console_key_is_answered_as_failed`.
7. **Bot API и маршрутизация.** `hub/api.rs`: `Message.pinned_message: Option<MessageRef>`, `ChatMember.can_pin_messages`, `pin_chat_message` (`disable_notification: true`). `scheduler.rs`: `Op::Pin` в полосе `Topic`. `updates.rs`: `ServiceKind::Pinned(id)` и новое поле `ServiceMessage.from: Option<i64>` (id автора, в логи не попадает; debug-лог печатает только `kind`). `hub/mod.rs`: `pub mod status`; `route_inbound(commands, control, bot_id)`, `run` передаёт `me.id`; `Pinned` уходит в `Control::Pinned` только при `from == Some(bot_id)`, человеческий или безавторский pin не трогается. На старте `can_pin = creator || can_pin_messages` (нет права = один warn); `Options { can_pin, status_every: Some(STATUS_EVERY), .. }`. `ingress.rs`: `AgentMsg::ConsoleKeyWritten` в явном списке пересылки (урок TASK-016 QA); `is_frequent` события логируются `debug`.
8. **`crates/cctg/src/hub/registry.rs`.** `StatusMessage { message_id, pinned }`, `Slot.status` (`serde(default, skip_serializing_if = none)`), сброс в `topic_invalid`. `SessionEntry.metrics: Option<status::Metrics>` (skip when none). `apply_hook(StatusLine)` пишет metrics только известной, не завершённой top-level сессии и только при изменении (`dirty`); `ToolStart/ToolEnd` registry не меняют.
9. **`crates/cctg/src/hub/status.rs` (новый, чистый).** `Metrics` (Serialize/Deserialize, пропуск `None`). `Activity`: `turn`, до 16 идущих вызовов, 64 закончившихся id (поздний `ToolStart` после своего конца ничего не показывает), `noted_to` (заметка прерывания учитывается один раз на байт транскрипта), `interrupt_sent`. `prompt()` и `stop()` сбрасывают `interrupt_sent`; `interrupt_written()` ставит его и чистит вызовы; `busy() = !interrupt_sent && (turn || running)`. `phase`: Ended -> Waiting -> InterruptSent -> Tool -> Thinking -> Idle. `render` всегда отдаёт явную клавиатуру (пустую тоже). Callback data только `status:stop` и `status:confirm`. Ответы: `ANSWER_CONFIRM`, `ANSWER_INTERRUPTING`, `ANSWER_IDLE`, `ANSWER_WAITING` («Сначала ответьте на запрос разрешения»), `ANSWER_OFFLINE`, `ANSWER_NO_KEYS`, `ANSWER_STALE`, `KEY_FAILED_NOTICE`. `CONFIRM_FOR` 10 с, `KEY_WAIT` 30 с, `MAX_KEY_ASKS` 32.
10. **`crates/cctg/src/hub/slots.rs`.**
    - `Options.status_every: Option<Duration>` (по умолчанию `None`, статус выключен, старые тесты actor-а не меняются), `can_pin`, `STATUS_EVERY` 5 с. `Control::Pinned`, `StatusJob {Create, Edit, Pin}`, `Shown`, `Done::Status`/`Work::Status`, `Conn.keys`.
    - `track_activity` в `on_hook` только для живых top-level сессий; activity закончившихся удаляется.
    - `on_chunk`: для каждой строки потока `StreamItem::Result { id }` вызывает `tool_end(id)` (отклонённый permission не даёт `PostToolUse*`, а результат в транскрипте есть), заметка прерывания вызывает `interrupted_at(line.end)`.
    - `press_status`: слот ищется по `message_id` нажатого сообщения; нужна живая текущая сессия слота с привязанным агентом и `keys`. Если ждёт permission prompt: взведённое подтверждение снимается, ответ `ANSWER_WAITING`, Esc не шлётся. Иначе не занята = `ANSWER_IDLE`; первое нажатие взводит подтверждение на 10 с (правка сразу, вне темпа); второе в срок шлёт `HubMsg::ConsoleKey`.
    - `KeyAsk { slot, session, conn, until }`. `on_key_written` принимает ответ только если совпадают conn и сессия кадра и `live_agent(slot) == (session, conn)`; иначе молча отбрасывает (сессия кончилась, слот занят другой, агент переподключился). `written=true` -> `interrupt_written()`; `written=false` -> одно уведомление `KEY_FAILED_NOTICE` через `notify` (не чаще `notice_every`).
    - `status_view`: metrics из `SessionEntry.metrics`; ⏹ = есть агент с keys, нет ожидающего prompt, `busy`.
    - `pump_status`: одно задание на слот, Create только для живой сессии после её разделителя, затем Pin (если `can_pin` и не было отказа), затем Edit не чаще `status_every`. `on_status_done`: «message to edit not found» / «can't be edited» -> статус сбрасывается и отправляется заново с pin; неудачный pin -> один warn, до рестарта не повторяется.
11. **Тесты-соседи** (`tests/{buffer_e2e,ingress_logs,message_logs,permission_hook_e2e,permission_logs,slots_logs,stream_logs}.rs`: `console_keys: false` в литералах `Register`; `soak.rs`: `Op::Pin` в `describe`; `hook_cli.rs`: тест snippet-а знает `statusLine` и три async `ToolStatus`).
12. **`crates/cctg/tests/status_e2e.rs`** (новый) и **`tests/statusline_cli.rs`** (новый), см. раздел 3.
13. **`docs/hook-settings.json`**: `statusLine: cctg statusline` и `cctg hook ToolStatus` с `"async": true` на `PreToolUse`, `PostToolUse` (вторая группа без matcher), `PostToolUseFailure`. **`docs/poc.md`**: право «Pin Messages», тот же snippet, абзац про statusline (байт в байт, код выхода, 80 мс, копировать `padding`/`refreshInterval`/`hideVimModeIndicator` в `statusLine` файла `--settings`, потому что он заменяет объект целиком) и абзац про ⏹ (только Windows, «Esc отправлен» не значит «остановлен», в фазе разрешения сначала Allow/Deny, conhost проверен, Windows Terminal не проверен, mintty не работает).

После merge orchestrator правит живой `~/.cctg/poc/settings.json` (добавить `statusLine` и три `ToolStatus`); это не часть патча.

## 3. Test plan

Сборка: один `CARGO_TARGET_DIR` под `%TEMP%`, `CARGO_PROFILE_DEV_DEBUG=0`, `-j 1`, строго один cargo за раз; target удалить после. Не вызывать Telegram API, не читать `.env`/`device.env`, не запускать интерактивный claude.

1. Узко при итерации: `cargo test -j 1 -p cctg --lib status`, `cargo test -j 1 -p cctg --test status_e2e`, `cargo test -j 1 -p cctg --test statusline_cli`.
2. Финально: `cargo fmt --all -- --check` (чисто, `R/fmt.out.txt`), `cargo clippy -j 1 --workspace --all-targets -- -D warnings` (чисто, `R/clippy.out.txt`), `cargo test -j 1 --workspace`: **614 passed, 0 failed, 1 ignored** (`R/workspace_test.txt`; до задачи 581, новых 33).
3. Мутации: `python -X utf8 R/mutations.py <workspace>` (все 23 должны быть KILLED; `R/mutations.out.txt`). M1-M12 от planner-а (M5, M11 под `ConsoleKeyWritten`), R1-R10 на исправления ревью.

Что доказывают новые тесты (ожидаемые исходы):

| Тест | Сценарий | Ожидание |
|---|---|---|
| `status_e2e::the_status_message_follows_the_session_and_its_button_writes_esc` | реальные `serve_hooks`/`serve_agents`, `Slots`, `Scheduler`, фейковый Telegram | одно сообщение, один pin, удалён только pin-notice про него; цифры; ⏹ только со второго нажатия; `written=true` -> «⏹ Esc отправлен в терминал» без кнопок, повторное нажатие ничего не шлёт; `Stop` -> «💤 Ждёт вас»; `written=false` -> одно уведомление; конец сессии -> «🏁» без кнопок, нажатие = `ANSWER_OFFLINE` |
| `…a_waiting_permission_prompt_hides_stop_and_a_press_writes_nothing` | агент шлёт `PermissionRequest` при взведённом ⏹ | «❓ Ждёт разрешения» без кнопок; confirm и stop -> `ANSWER_WAITING`, агенту ничего; после Allow + ack ⏹ возвращается, свежее подтверждение шлёт Esc |
| `…a_late_key_answer_never_reaches_the_next_session_of_the_slot` | Esc спрошен у A, A кончилась, B занял тот же слот, агент A отвечает `written=false` | нет уведомления в теме B, статус B не тронут, сообщение одно |
| `…a_restarted_hub_keeps_its_status_message_and_the_numbers` | `Control::Stop`, новый hub на том же `registry.json` | правится то же сообщение, цифры на месте без нового statusline, нет второго send/pin |
| `…a_press_reaches_only_the_agent_of_its_own_slot` | два слота, два агента | Esc только своему агенту; подтверждение одного сообщения не взводит другое; чужой message_id = `ANSWER_STALE` |
| `…edits_are_paced_and_a_deleted_status_message_comes_back` | шторм ToolStart/StatusLine/ToolEnd 1.2 с | правок не больше окон темпа + 2; «not found» -> новое сообщение и новый pin |
| `slots::a_call_whose_result_is_in_the_stream_no_longer_shows_as_running` | ToolStart, затем в транскрипте вызов и отказанный результат, `PostToolUse` нет | «⚙️» сменяется на «💭 Думает» |
| `slots::an_interrupt_note_in_the_stream_ends_the_turn_on_the_status_message`, `…without_console_keys_sends_nothing`, `…off_without_status_every` | как у planner-а | без изменений |
| `hub::tests::messages_go_to_the_slot_actor_and_commands_do_not` | pin-notice от allowlisted, от другого id, без автора, от бота | в actor уходит только последний |
| `status::tests::*` | фазы, `a_written_esc_is_not_the_end_of_the_turn`, поздний старт, заметка один раз, рендер, serde `Metrics`, callback ≤64 байт, длинная строка | как в названиях |
| `statusline_cli::the_users_command_gets_the_same_stdin_and_its_output_passes_byte_for_byte` | команда `cat`, вход с не-ASCII и без финального `\n` | stdout == stdin байт в байт, код 0; hub получил `StatusLine` с округлёнными цифрами |
| `statusline_cli::colours_lines_a_failing_exit_and_empty_output_pass_unchanged` | `printf 'a\033[31mb\nc'; exit 3`, затем `true` | `a\x1b[31mb\nc` и код 3; пустой вывод и код 0 (не своя строка) |
| `statusline_cli::without_a_command_or_inside_one_cctg_prints_its_own_line` | нет settings; `CCTG_STATUSLINE=1` | своя строка, код 0; во вложенном вызове нет POST |
| `statusline_cli::a_stopped_hub_adds_well_under_150_ms` | лучший из 5 прогонов с закрытым портом против живого hub | разница < 150 мс |
| `statusline::tests::missing_null_and_odd_fields_are_left_out` | −1, 100.6, 999, f64::MAX; 0 и 100 | вне диапазона отброшены, края приняты; `POST_TIMEOUT ≤ 100 мс` |

Замер процессов (`R/hook_cost.out.txt`, `python R/hook_cost.py <cctg.exe>`, dev-сборка, медиана 20 прогонов): ToolStatus 12 мс с hub и 315 мс без (async, Claude Code не ждёт); statusline своя строка 12 мс с hub и 93 мс без; с цепочкой `echo` 40 мс с hub и 109 мс без. Лишнее время при лежащем hub около 70-80 мс, в пределах 150.

Тесты statusline_cli требуют Git Bash на Windows (как и сам Claude Code для statusline); без него они печатают «skipped» и выходят (кроме `a_stopped_hub…` и теста своей строки, которым shell не нужен).

## 4. Rollout notes

- **Провод.** `wire::VERSION` остаётся 1. Старый агент не объявляет `console_keys` и ключей не получает; старый hub не знает `console_key_written`, но новый агент шлёт его только в ответ на `console_key`, который старый hub не шлёт. Новые hook-виды (`tool_start`, `tool_end`, `status_line`) старый hub отвергает 400; хук `ToolStatus` async, statusline не ждёт ответа, так что это видно только в логе. Агенты переживают апгрейд hub: после апгрейда уже запущенный агент без `console_keys` кнопку ⏹ не получает до перезапуска сессии.
- **`registry.json`.** Новые поля `Slot.status` и `SessionEntry.metrics` с `serde(default)` и пропуском пустых: старый файл читается, старый hub игнорирует новые поля.
- **Права бота.** Нужно «Pin Messages» (`can_pin_messages`); без него статус отправляется и правится, но не закрепляется (один warn на старте, один на первый отказ).
- **Настройки сессий.** В `--settings` cctg-сессий добавить `statusLine` (`"<cctg>" statusline`) и три async `ToolStatus` (как в `docs/hook-settings.json`/`docs/poc.md`). Если у пользователя в `statusLine` есть `padding`, `refreshInterval`, `hideVimModeIndicator`, их скопировать в этот `statusLine`. У текущего пользователя этих полей нет (проверено по `~/.claude/settings.json`).
- **Env.** Новых обязательных нет. `CCTG_STATUSLINE` ставит сам `cctg statusline` для дочерней команды (защита от рекурсии). `CLAUDE_CODE_GIT_BASH_PATH` уважается, как у Claude Code.
- **Платформа.** ⏹ только Windows. Проверено в скрытой conhost-консоли (проба planner-а во время размышления); Windows Terminal (ConPTY) ожидаемо работает, не проверено; mintty без консоли даёт `written=false` и одно уведомление.
- **Известные ограничения** (документированы, не чинятся здесь): прерывание во время размышления не даёт ни `Stop`, ни заметки, статус держит «Esc отправлен» до следующего промпта; после рестарта hub `Activity` пуста, статус покажет «💤 Ждёт вас» до следующего события (цифры сохраняются); неудачный pin не повторяется до рестарта; потерянный ответ на `sendMessage` статуса может оставить лишнее незакреплённое сообщение (как у тем TASK-011); приход служебного `pinned_message` от собственного pin бота в `getUpdates` не проверен живьём (аналог `forum_topic_edited` проверен), если не приходит, одно уведомление о закреплении остаётся на слот; Ctrl+B/⏬ не входит в задачу (отдельная задача после пробы).

## 5. Review notes (что изменено относительно PLAN_V2 и patch planner-а)

Проверенный контрпример до ревью (`R/disconfirmation.md`): «собственный pin бота не приходит в getUpdates, значит удаление notice невозможно». Не опровергнут и не подтверждён без живого API: единственный проверенный аналог (`forum_topic_edited` от своего `editForumTopic` приходит) говорит, что приходит; деградация безопасна. Зато найдена обратная дыра (удалялся pin-notice человека), исправлена.

Воспроизведено и исправлено (каждое с тестом и мутацией, которая возвращает старое поведение и убивается):

1. **Ctrl+B/⏬ удалён целиком** (решение orchestrator-а): `ConsoleKey::Background`, `status:bg`, `background_button`, определение фоновых вызовов в хуке, поле `ToolStart.background`, кнопка, ответы, тесты, docs. Мутация M13 planner-а удалена.
2. **Permission prompt** (контрпример PLAN_V2 подтверждён по коду: `busy` считал `waiting`, `press_status` слал Esc). Теперь ⏹ не рендерится при ожидающем prompt, нажатие старой кнопки отвечает `ANSWER_WAITING`, снимает подтверждение и ничего не шлёт. Тест `a_waiting_permission_prompt…`, мутации R2a, R2b.
3. **`ConsoleKeyDone{ok}` -> `ConsoleKeyWritten{written}`**, `written=true` больше не `Activity::stop()`, а фаза `InterruptSent` без кнопки до `Stop`/заметки/промпта/конца. Мутации R3, M5.
4. **Поздний ответ на ключ.** Путь воспроизведён: `KeyAsk` без conn, `on_key_done` сверял только сессию кадра и слал уведомление в тему слота, уже занятого новой сессией. Теперь `(slot, session, conn)` + `live_agent`. Тест `a_late_key_answer…`, мутация R4.
5. **Чужой pin-notice удалялся.** `classify` терял автора, `on_pinned` удалял любой notice про наш message_id. Теперь автор должен быть `getMe.id`. Тест в `hub::tests`, мутация R1.
6. **Застрявший «⚙️» после отказа в разрешении.** Отказ не даёт `PostToolUse*`; теперь `StreamItem::Result` закрывает вызов. Тест в `slots`, мутация R5. (Работает, когда у сессии идёт поток транскрипта, то есть у агента с `transcript_reads`; иначе до `Stop`.)
7. **Цифры терялись при рестарте.** Перенесены из `Activity` в `SessionEntry.metrics`. Тест `a_restarted_hub…`, мутация R6.
8. **Бюджет statusline.** `POST_TIMEOUT` 150 -> 80 мс; замер: лишнее время при лежащем hub ~80 мс (было ~160). Тест `a_stopped_hub_adds_well_under_150_ms`, мутация R8.
9. **Цепочка statusline байт в байт.** Добавлен процессный тест (stdin == stdout, ANSI, несколько строк, пустой вывод, рекурсия). Найдено сверх PLAN_V2: patch всегда выходил с 0, а документация statusline говорит, что ненулевой код гасит строку; теперь код выхода команды пробрасывается. Мутации R9, R10. `padding`/`refreshInterval`/`hideVimModeIndicator`: документированы в `docs/poc.md` как поля для копирования (генератора настроек в задаче нет).
10. **Проценты** `0..=1000` -> `0..=100` (документированный диапазон). Мутация R7.
11. **Покрытие e2e** расширено (таблица раздела 3); callback от неallowlisted уже покрыт существующими тестами `updates::classify` (`NotAllowed` для callback_query) и не дублируется.

Отклонено (с причиной; записано в `log.jsonl`):

- Печатать пустоту, если настроенная команда не запустилась или зависла (PLAN_V2 п. 8): наш поиск shell может разойтись с Claude Code, своя строка полезнее пустоты; при нормальном запуске вывод и код и так передаются как есть.
- Восстанавливать Ctrl-handler после нажатия (PLAN_V2 шаг 6): после `FreeConsole` у агента нет консоли, Ctrl-события до него не доходят.
- Убрать `cwd`/`transcript_path` из POST `status_line` (PLAN_V2): все хуки шлют их в тот же локальный hub, hub их для этого события не использует, убирать = особый случай провода.
- Повтор pin после временной ошибки: не в списке orchestrator-а, единственный эффект: незакреплённое, но живое сообщение до рестарта.
- Генерировать merged `--settings`: нет лаунчера в задаче; документация достаточна.

Размер: 34 файла (5 новых: `hub/status.rs`, `keys.rs`, `statusline.rs`, `tests/status_e2e.rs`, `tests/statusline_cli.rs`), +3594 −59 по `git apply --numstat`; около 1060 строк из прироста это два новых тестовых файла.

PCTX: планерские предложения в `PCTX_PROPOSALS.md` код не меняют и оставлены orchestrator-у. Стоит добавить (orchestrator решит): документация statusline: ненулевой код выхода или пустой вывод гасит строку, обёртка обязана пробрасывать код.
