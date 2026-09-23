# PLAN FINAL — TASK-012: hook, события жизненного цикла и отчёт субагента

`T` = `maw/tasks/in_progress/TASK-012`. `REF` = `T/scratch/reviewer2/ws`: исправленная копия референса планировщика (`T/scratch/planner/ws`). Она собрана и проверена целиком (см. раздел 3). Все пути ниже от корня репозитория.

## 1. Summary

`cctg hook <Event>` читает JSON из stdin в отдельном потоке (300 мс, максимум 8 MiB), разбирает только нужные поля узкими `#[serde(default)]` структурами, чистой функцией `hook::build` собирает `wire::HookPost` и отправляет его одним POST через существующий `hook::post` (TASK-010). Процесс всегда завершается `exit 0`, stdout всегда пуст, в stderr только фиксированные строки. Поддержаны `SessionStart`, `SessionEnd`, `UserPromptSubmit`, `Stop`, `SubagentStart`, `SubagentStop`, а также `PreToolUse`/`PostToolUse` с `tool_name == "SubagentHandback"` (отчёт из `tool_input.message`). Для `SessionStart`/`SessionEnd` новый модуль `proctree` строит цепочку предков (один ToolHelp-снимок на Windows, `/proc/<pid>/stat` на Linux) и определяет собственный claude-процесс и claude-родителя. Новый модуль `device` читает конфиг устройства (process env, затем `~/.cctg/device.env`, без `set_var`) и канонизирует `cwd`. Регистрация хуков описана в `docs/hook-settings.json` (shell form, без секретов и путей). Wire, ingress и registry хаба не меняются.

## 2. Implementation steps

Исполнитель копирует готовый результат, а не пишет код заново.

### Шаг 0. Предусловия
- `git status` чистый (или содержит только чужие изменения вне 18 файлов ниже, их не трогать).
- Ветка задачи: `feature/hook-subcommand` по task spec; текущая рабочая ветка `feature/hook-lifecycle` тоже допустима, если orchestrator уже на ней. Ветку не переключать без указания orchestrator-а.

### Шаг 1. Применить патч
```bash
git apply --check maw/tasks/in_progress/TASK-012/scratch/reviewer2/task012.patch
git apply maw/tasks/in_progress/TASK-012/scratch/reviewer2/task012.patch
bash maw/tasks/in_progress/TASK-012/scratch/reviewer2/verify_hashes.sh   # ожидается 18 x OK
```
Патч собран `T/scratch/reviewer2/build_patch.py` из `REF` против HEAD `7f15279` и проверен `git apply --check` на нём. Патч трогает только файлы вне `maw/`, поэтому последующие коммиты pipeline в `maw/` его не ломают. Хэши считаются по LF-байтам (скрипт снимает CR, рабочее дерево на Windows в CRLF).

Альтернатива при сбое `git apply`: скопировать каждый из 18 файлов из `REF/<путь>` в `<путь>` и прогнать `verify_hashes.sh`.

**Нельзя** использовать `T/scratch/planner/task012.patch` и `T/scratch/planner/hashes.txt`: это версия до исправлений, `hook.rs`, `proctree.rs` и `tests/hook_cli.rs` в ней отличаются.

### Шаг 2. Что лежит в патче (18 файлов, sha256 по LF)

| Файл | sha256 | Суть |
|---|---|---|
| `Cargo.lock` | `82b0f1a0…6d8e` | у package `cctg` одна новая строка `"windows-sys 0.61.2"`; пакет уже в lockfile через tokio, скачиваний нет |
| `crates/cctg/Cargo.toml` | `20e90c23…1725` | `[target.'cfg(windows)'.dependencies] windows-sys = { version = "0.61", features = ["Win32_Foundation", "Win32_System_Diagnostics_ToolHelp"] }` |
| `crates/cctg/src/device.rs` (новый) | `710b85c7…d652` | `DeviceConfig { secret: Result<Secret, ConfigProblem>, hook_addr, host }`; `load()` никогда не падает; `from_vars` чистая; `read_env_file` через `dotenvy::from_path_iter`, ошибка dotenvy не сохраняется; `ConfigProblem` с фиксированным `Display`; `canonical_cwd` = `canonicalize` + снятие `\\?\` / `\\?\UNC\`, fallback на исходную строку |
| `crates/cctg/src/proctree.rs` (новый) | `23afceb8…66b2` | `Proc`, `Lineage { claude_pid, parent_claude_pid }`, `current_lineage`, чистая `lineage`, `ancestors` (глубина 64, защита от циклов), Windows `Snapshot` (один `unsafe`, `CloseHandle` ровно один раз), Linux `parse_stat`; правило выбора см. ниже |
| `crates/cctg/src/hook.rs` | `eb5cf62c…ef4d` | транспорт TASK-010 (`post`, `parse_status`, `PostError` и их тесты) без изменений; добавлены `run`, `build_here`, `read_stdin`, `Probe`, `Skip`, `build`, `post_timeout`, `cap_text` и `build_tests` |
| `crates/cctg/src/lib.rs` | `7e293098…6e25` | `pub mod device;`, `pub mod proctree;` |
| `crates/cctg/src/main.rs` | `a5335781…d85d` | ветка `Command::Hook`: panic hook с фиксированным `"cctg hook: internal error"`, `tokio::spawn(hook::run)`, join-результат игнорируется, `std::process::exit(0)`; `Command::Agent => {}` как было |
| `crates/cctg/tests/fixtures/hook/*.json` (9) | см. `T/scratch/reviewer2/hashes.txt` | обезличенные захваты TASK-003 (`~` вместо home) и синтетический `user_prompt_submit.json` |
| `crates/cctg/tests/hook_cli.rs` (новый) | `5fd072bd…a2c2` | CLI-тесты, 6 штук (см. раздел 3) |
| `docs/hook-settings.json` (новый) | `e6f41848…95c7` | 7 ключей: 6 событий + `PostToolUse` с matcher `SubagentHandback`, команды `cctg hook <Event>` |

Полные хэши в `T/scratch/reviewer2/hashes.txt`.

Поведение, которое должно получиться (для ревьюера кода):

1. **Собственный claude и родитель (`proctree::lineage`)**, цепочка `chain[0]` = сам хук:
   - `named` = ближайший предок с именем `claude`/`claude.exe` (без учёта регистра);
   - `by_env` = предок с pid == `CLAUDE_PID`;
   - own = `by_env`, если он ближе `named` и называется `node`/`node.exe` (npm-claude, вложенный в нативную сессию); иначе `named`; если `named` нет, то `by_env`; если и его нет, `claude_pid = CLAUDE_PID` из env;
   - parent = следующий выше own предок с именем `claude` и другим pid; иначе, только если env `CLAUDE_CODE_SESSION_ID` непуст и отличается от stdin `session_id`, parent = env `CLAUDE_PID` (если он не равен own). Родитель-`node` не распознаётся (npm-only установка вне поддержки).
2. **`hook::build`**: Skip при непарсящемся JSON, пустом `session_id`, `hook_event_name`, отличном от аргумента CLI, неизвестном событии. `SessionStart` → `source`, `claude_pid`, `parent_claude_pid`; `SessionEnd` → `reason`, только собственный `claude_pid`; `UserPromptSubmit` → только `prompt_id` (текст промпта не уходит); `Stop` → `prompt_id`, `last_assistant_message` (обрезка 128 KiB); `SubagentStart` → непустые `agent_id`, `agent_type`; `SubagentStop` → непустые `agent_id`, `agent_type`, **непустой `agent_transcript_path` и существующий `agent-<id>.jsonl` или `agent-<id>.meta.json`**, иначе `Skip("internal agent")`; `PreToolUse`/`PostToolUse` → только `SubagentHandback` с непустыми `agent_id` и `tool_input.message`. Дерево процессов запрашивается только для `SessionStart`/`SessionEnd`.
3. **Таймауты**: stdin 300 мс; POST 500 мс (`POST_TIMEOUT`) для всех событий, кроме `UserPromptSubmit`, у которого 300 мс (`PROMPT_POST_TIMEOUT`). Один вызов делает не больше одного POST, без повторов.

### Шаг 3. Не делать
- Не регистрировать хуки ни в каком реальном `settings.json` (`~/.claude/settings.json`, `.claude/settings*.json`).
- Не трогать `wire.rs`, `hub/*`, `agent.rs`.
- Не читать `.env`, не обращаться к Telegram API.
- Не коммитить `target/`, `.cctg/`, `.env`.

## 3. Test plan

Одна cargo-сборка за раз, target вне репо:
```powershell
$env:CARGO_TARGET_DIR = Join-Path $env:TEMP 'cctg-task012-final-target'
cargo test --workspace --offline -j 2
cargo clippy -p cctg --all-targets --offline -j 2 -- -D warnings
cargo fmt --all -- --check
git diff --check
```
Ожидается: 277 passed, 0 failed, 1 ignored (замер на `REF`, `T/scratch/reviewer2/workspace_test.txt`), clippy и fmt чистые. `git diff --stat` показывает ровно 18 файлов.

Соответствие acceptance criteria и тестов:

| Критерий | Тест |
|---|---|
| Шесть событий: нужные поля и ничего лишнего | `hook::build_tests::each_event_carries_its_fields_and_nothing_else` (точные множества ключей верхнего уровня и `event`), `hook_cli::every_event_reaches_the_hub` (настоящий `ingress::serve_hooks`, 7 вызовов) |
| Недоступный hub: exit 0, stdout пуст, stderr без входа и секрета | `hook_cli::a_silent_hub_keeps_session_end_well_inside_its_budget`, `hook_cli::no_hub_listening_is_quiet_and_fast` |
| Вложенность и parent по правилу TASK-003 | `proctree::tests::{top_level_session_has_no_parent, nested_session_reports_the_next_claude, env_stripped_nested_run_and_interactive_start, exec_form_hook_is_a_direct_child_of_claude, foreign_env_session_names_the_parent_only_as_a_fallback, a_node_claude_below_a_native_claude_is_the_own_process, npm_only_chain_keeps_the_own_pid_and_finds_no_parent, a_stale_env_pid_on_a_wrapper_does_not_move_the_own_process}`, `hook::build_tests::nesting_comes_from_the_lineage` |
| Битый, пустой, обрезанный stdin | `hook::build_tests::broken_input_is_skipped_without_panicking` (все префиксы реального входа), `hook_cli::broken_input_and_missing_config_exit_zero_quietly`, `tests/stdout.rs` |
| Зависший открытый stdin | `hook_cli::an_open_silent_stdin_does_not_hold_the_hook` (exit 0, stdout пуст, фиксированная строка, < 1.2 с) |
| `SessionEnd` < 1.5 с при недоступном hub | `hook_cli::a_silent_hub_…` и `no_hub_listening_…` (< 1.2 с по стене), `hook::build_tests::only_the_prompt_hook_uses_the_short_post_timeout` (`STDIN_TIMEOUT + POST_TIMEOUT <= 800 мс`) |
| Snippet: все события, без секретов и путей | `hook_cli::settings_snippet_registers_every_event_without_secrets_or_paths` |
| Handback + поля SubagentStop + фильтр внутренних | `each_event_…` (Pre и Post handback), `hook::build_tests::internal_agents_are_dropped` (пустой тип, тип без файлов, тип без пути, пустой путь, только `.meta.json` проходит), `other_tools_and_incomplete_handbacks_are_skipped` |
| `source` только из SessionStart, необязателен | `hook::build_tests::source_is_optional_and_read_only_from_session_start` |
| Канонизация cwd | `device::tests::canonical_cwd_*`, проверка `cwd` в `check()` |
| `parent_claude_pid` только при claude-предке, отличном от своего | тесты `proctree` выше, `every_event_reaches_the_hub` (parent != own на живом дереве) |
| `SessionEnd` несёт собственный pid | `nesting_comes_from_the_lineage`, `a_node_claude_below_a_native_claude_is_the_own_process` |
| Таймаут POST по событию | `hook::build_tests::only_the_prompt_hook_uses_the_short_post_timeout` |
| Existing tests pass | весь workspace |

Доказательства на `REF` (в `T/scratch/reviewer2/`):
- `repro_before_fix.out`: новые тесты на исходном референсе планировщика падают: mixed chain дал `claude_pid: Some(5), parent_claude_pid: None` (pid родителя как свой), typed `SubagentStop` без пути прошёл.
- `mutations.out.txt`: 6 мутаций (снять node-исключение; любое имя по env pid; env pid выше claude; typed stop без пути; один таймаут на всё; stdin 5 с), все KILLED.
- `flake.out.txt`: `hook_cli` 8 прогонов подряд, 8/8 зелёные, ~1.07 с.
- `workspace_test.txt`: 277 passed, 1 ignored.

Ручная проверка не требуется и невозможна в песочнице (регистрировать хуки в реальных settings запрещено). Живая проверка с Claude Code будет в задаче установки.

## 4. Rollout notes

- Миграций нет. Wire `VERSION = 1` и контракт `HookPost` не меняются, hub TASK-011 принимает все семь вариантов.
- Новая зависимость только `windows-sys 0.61` (target windows), уже в lockfile. Сборка `--offline` проходит.
- Конфиг устройства: `~/.cctg/device.env` или process env. `CCTG_HUB_SECRET` обязателен (то же значение, что у hub), `CCTG_HUB_HOOK_ADDR` по умолчанию `127.0.0.1:47292`, `CCTG_HOST` по умолчанию имя машины. Без секрета хук пишет одну фиксированную строку в stderr и выходит 0. На главном устройстве секрет лежит и в `.env` hub, и в `device.env` (принято orchestrator-ом).
- Установка (следующая задача): `cctg` в PATH, содержимое `docs/hook-settings.json` слить в `~/.claude/settings.json` (user scope). Shell form выполняется через Git Bash на Windows. Изменение settings подхватывается уже запущенными сессиями.
- Остановленный hub стоит каждому хуку его таймаут (Windows повторяет SYN на закрытый порт): 300 мс на промпт, 500 мс на остальные события.
- Обратная совместимость: `cctg hook` раньше был no-op, существующий `tests/stdout.rs` (пустой stdin → exit 0, пустой stdout) остаётся зелёным.

Известные риски, которые задача не закрывает:
- Оборванная цепочка процессов (умерший wrapper) делает вложенный запуск похожим на top-level (дыра TASK-003).
- Переиспользование pid: сверка creation time отложена (OPEN_DECISIONS п.3).
- npm-only Claude: свой pid верный, родитель-`node` не распознаётся, вложенный запуск выглядит top-level. Проект не ставит Node.
- Фильтр внутренних агентов по файлам основан на наблюдении TASK-003 (14/14 без файлов, настоящий Explore с файлами). Не проверено, какой `agent_transcript_path` приходит у главного потока сессии с `--agent`: если он укажет на существующий файл, такой шум пройдёт в hub (hub его только регистрирует). Если Claude Code перестанет писать файлы субагента к моменту `SubagentStop`, настоящий stop потеряется, `SubagentStart` при этом дойдёт.
- Потеря события при таймауте без повтора (fire-and-forget по домену). Если реальный RTT до hub по Tailscale окажется больше ~300 мс, таймауты надо делать настраиваемыми отдельной задачей.

## 5. Review notes

Disconfirmation: выбран контрпример "исправление own-pid из PLAN_V2 ломает нативные цепочки TASK-003 или даёт stale `CLAUDE_PID` на промежуточном shell сдвинуть собственный pid". Проверено тестами `a_stale_env_pid_on_a_wrapper_does_not_move_the_own_process` и тремя нативными тестами: контрпример не подтвердился для выбранного правила (node-only исключение). Мутации M2/M3 показывают, что более широкие варианты правила этот тест ловит. Вторичный контрпример (mixed chain в исходном референсе даёт pid родителя) подтвердился, `repro_before_fix.out`.

Изменения относительно PLAN_V2:
1. **План указывает на готовый, собранный и проверенный `REF` с новыми патчем и хэшами**, а не на "применить старый патч и доработать". PLAN_V2 оставлял исполнителю писать исправления самому, что противоречит задаче "исполнитель копирует файл за файлом".
2. **Правило own-pid сужено до `node`**: PLAN_V2 разрешал "claude или node", но предок с именем `claude` ближе первого `claude` не бывает, так что это одно и то же. Добавлены тесты mixed, npm-only, stale-wrapper. Исходный тест планировщика с сообщением "a claude by name wins over the env pid" закреплял ошибку и удалён.
3. **Фильтр `SubagentStop` исправлен как в PLAN_V2**: без пути или с пустым путём → `Skip("internal agent")`.
4. **Таймаут: 300 мс только для `UserPromptSubmit`, а не для всех "блокирующих" событий.** PLAN_V2 ставил 300 мс ещё на `Stop`, `SubagentStart/Stop` и handback. Это не исправление дефекта, а обмен надёжности на задержку: `Stop`, `SubagentStop` и handback несут текст отчёта, и потеря на медленном канале дороже 200 мс при остановленном hub. Пользователь реально ждёт только `UserPromptSubmit`, а он несёт лишь `prompt_id`. Решение записано в `log.jsonl`.
5. **Добавлен CLI-тест зависшего stdin** (как требовал PLAN_V2, теперь он есть в `REF`).
6. PLAN_V2 утверждал, что `lineage` при npm-only "own pid корректен": это верно только после исправления, в исходном коде own брался из env лишь при отсутствии claude по имени. Теперь закреплено тестом.
7. Проверка документации (code.claude.com/docs/en/hooks, 2026-09-23): общий бюджет `SessionEnd` 1.5 с; plain stdout попадает в контекст у `UserPromptSubmit`, `UserPromptExpansion`, `SessionStart`, `PostModelSwitch`; наш stdout всегда пуст, так что это не влияет.

PCTX-предложение про правило `CLAUDE_PID` ниже первого `claude.exe` добавлено в `T/PCTX_PROPOSALS.md`.
