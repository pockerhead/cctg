# TASK-056 IMPL_SUMMARY

Коммит `9b7b101` на `feature/builtin-statusline`: `feat: cctg's own status line matches the user's statusline.py (TASK-056)`.

## 1. Что сделано

| Файл | Изменение |
|---|---|
| `crates/cctg/src/statusline.rs` | +257/−20 (всего +348/−26 по 4 файлам; `statusline_cli.rs` +87/−5, `poc.md` +1/−1). `own_line(input, cwd, branch, email)` стала чистой функцией с форматом `statusline.py`: строка 1 `\x1b[1m<model>\x1b[0m [effort] \x1b[36mbr:<branch>[*]\x1b[0m dir:<folder> ctx:NN%`, строка 2 `acc:<email> 5h:NN% 7d:NN%` (жёлтый `\x1b[33m` от 80, красный `\x1b[31m` от 95, сброс `\x1b[0m`). Модели нет, будет `?` (как в py). Вторая строка только если в ней что-то есть. `sh:` не выводится. IO вынесено в `own_output`: `session_dir` (`workspace.current_dir`, потом `cwd`, потом cwd процесса, без обрезки до 64 символов), `git_branch` (`git -C <cwd> --no-optional-locks branch --show-current` и `status --porcelain` параллельно, `git` из `PATH`, stdin/stderr null, `kill_on_drop`, `GIT_TIMEOUT` = 400 мс на каждый вызов; если истёк таймаут у branch, ветки нет, если у status, нет `*`), `account_email` → `claude_json_path` (`$CLAUDE_CONFIG_DIR/.claude.json`, иначе `<home>/.claude.json`) + `email_of` (узкий serde `oauthAccount.emailAddress`, trim, не пустой, ≤254 символов, без управляющих символов). Email попадает только в строку stdout. `HookPost` строится из `event()`, как и раньше, email туда не передаётся. Новых крейтов нет (serde `Deserialize` уже был в зависимостях). |
| `crates/cctg/tests/statusline_cli.rs` | `run` → `run_input(home, env, input)`: `current_dir(home)` и `GIT_CEILING_DIRECTORIES=<home parent>`, чтобы target внутри клона не давал ветку. Ожидания своей строки обновлены. Новый тест `the_own_line_shows_the_branch_and_the_account_and_the_hub_never_gets_the_email`: временный `git init -b topic`, фейковый `.claude.json` с `statusline-cli@example.invalid` в тестовом home; проверяет `br:topic` на чистом дереве и `br:topic*` после untracked-файла, `acc:<fake>`, что в stderr нет email, что в сериализованном теле и Debug полученного hub `HookPost` нет email, и что прогон занял меньше 3 с. Без git тест пропускается. |
| `crates/cctg/tests/install_e2e.rs` | +3: явная проверка `settings["statusLine"]["type"] == "command"` (команда `"<exe>" statusline` уже проверялась). |
| `docs/poc.md` | Абзац про statusline: вместо «короткая строка cctg» описаны две строки, цвета, git с таймаутом 400 мс и email только для терминала. |

Юнит-тесты в `statusline.rs`: `the_own_line_is_the_users_statusline_py` (полный формат с веткой и email; без git и логина; пороги 79.4/79.5/94.4/95/100; имена папок Windows/Unix/корень), `the_account_email_comes_from_claude_json` (email, null, пусто, ESC-инъекция, не строка, нет поля, не JSON; пути с `CLAUDE_CONFIG_DIR` и без; `GIT_TIMEOUT ≤ 500 мс`). Старые тесты `own_line` переведены на новую сигнатуру.

## 2. Отступления и находки

- **Почему на свежей машине не было даже `own_line`: дыры в коде не нашёл, ничего не менял.** `install.sh` пишет `statusLine` (`"<cctg>" statusline`, `type: command`) в `~/.cctg/claude/settings.json` одним heredoc для всех ОС (`write_claude_files`, строка ~604), так было с первого коммита install.sh (`124e915`). Обёртки `claude-cctg` и `.cmd` передают `--settings <этот файл>` последним аргументом. `install_e2e` это проверяет, CI гоняет его на ubuntu/windows/macos. `cctg run` при рестарте сохраняет `--settings` (`update::relaunch_args` оставляет прочие опции, тест с `CCTG_RUN_ARGS=["--settings","s.json"]`). Правдоподобные причины без репро на той машине, не проверены:
  (a) в начале сессии `context_window.used_percentage` = null, а `rate_limits` бывают только у подписки, поэтому старая `own_line` печатала одно имя модели (это совпадает с «не видно контекста»);
  (b) если на новую машину скопирован `~/.claude/settings.json` со `statusLine` на `python …statusline.py`, а Python там нет, команда падает с ненулевым кодом, cctg по дизайну TASK-029 пробрасывает код, и Claude Code гасит строку;
  (c) claude запущен как `claude`, а не `claude-cctg`.
  Если нужно, (b) закрывается отдельной задачей: при коде 127 показывать свою строку.
- Замеченная старая странность, не трогал: `event()` берёт cwd через `text()`, а она режет строку до `MAX_NAME` = 64 символа, так что в hub уходит cwd длиннее 64 символов уже обрезанным. `own_line` читает папку без обрезки.
- Проценты округляются как в hub (`f64::round`, половина от нуля), а не банковским `round()` Python. Значения вне 0..=100 отбрасываются, как раньше.

## 3. Тесты

Окружение: `CARGO_TARGET_DIR=C:/Users/user/dev/cctg/target`, `CARGO_PROFILE_DEV_DEBUG=0`, `-j 1`, перед прогоном `touch` на `lib.rs`/`main.rs`.
- `cargo fmt --all --check`: ok.
- `cargo clippy -j 1 --workspace --all-targets -- -D warnings`: ok.
- `cargo test -j 1 --workspace --no-fail-fast`: rc=0, 878 passed, 0 failed, 3 ignored (`scratch/full_test.txt`).
- Флейки на холодном первом прогоне после сборки, с моим изменением не связаны: `statusline_cli` (первый запуск: hub не получил POST за 80 мс, в том числе в тесте `cat`, где git не вызывается) и `hook_cli::every_event_reaches_the_hub` (SessionStart не уложился в 500 мс, ушёл в spool). Повторы: `statusline_cli` 5/5 зелёные, `hook_cli` зелёный.
- Замер (`scratch/measure_own_line.py`, вывод в `scratch/measure_own_line.out.txt`, debug-сборка, без hub, 20 прогонов): в репозитории медиана 50 мс (макс. 99), вне репозитория 28 мс (макс. 79).

## 4. Как проверить руками

1. `printf '{"session_id":"s","model":{"display_name":"Opus"},"effort":{"level":"high"},"workspace":{"current_dir":"<путь к репо>"},"context_window":{"used_percentage":42},"rate_limits":{"five_hour":{"used_percentage":85},"seven_day":{"used_percentage":97}}}' | HOME=<временный home> USERPROFILE=<тот же> cctg statusline`. Ожидается жирная модель, `[high]`, голубая `br:<ветка>[*]`, `dir:<папка>`, `ctx:42%`, на второй строке жёлтый `5h:85%` и красный `7d:97%`. Если во временном home положить `.claude.json` с `oauthAccount.emailAddress`, появится `acc:<email>`.
2. В живой сессии `claude-cctg` без своего `statusLine` в `~/.claude/settings.json` внизу терминала будут две строки. В теме Telegram статус не меняется (`Opus · high · ctx … · 5h … · 7d …`), email там нет.
