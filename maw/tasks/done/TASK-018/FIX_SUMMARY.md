# TASK-018 FIX_SUMMARY (fixer, claude opus, medium)

## 0. Проверка до правок

Самое конкретное утверждение ревью, которое при буквальном исполнении могло сломать тест: minor 2, "arm после `wait_for("both permission prompts")`" с прежними смещениями `[3, 12]`. После prompt строки всплеска A #2 склеиваются (83 строки в 54 сообщениях за весь прогон), сколько stream-сообщений остаётся после prompt, никто не считал. Второй 429 мог уехать в фазу 5 или не случиться вовсе, и тогда падает `assert_eq!(floods.len(), 2)`. Поэтому смещения уменьшены до `[2, 6]` и добавлена проверка, что оба 429 случились до конца всплеска. Сама находка (задержка в прогоне ревьюера 1007/1053 мс с пометкой "includes a 429 pause") подтверждена кодом: `arm_flood(&[3, 12])` стоял до записи всплеска, первый 429 приходился на окно `t0..p_a2`.

## 1. Fixed

1. **Minor 1** (live читает все ждущие апдейты). Подтверждено: `OffsetStore::open` на свежем state, `poll` стартует с `offset = None`. Сделано:
   - `docs/soak.md`, шаг 1: точные условия. Первый `getUpdates` читает и подтверждает все ждущие апдейты, включая написанные, пока hub стоял. Запускать прогон сразу после остановки hub, не писать в группу до конца прогона, потом сразу запустить hub.
   - `tests/soak.rs`, live preflight: один `getUpdates` без offset (он ничего не подтверждает) до старта hub. Число печатается в stderr и добавляется в отчёт одной строкой, только число: Telegram отдаёт не больше 100. В шаблон отчёта в docs добавлена строка "(live only)". Offset не трогаю.
2. **Minor 2** (429 внутри окна задержки permission). `arm_flood` перенесён: теперь он вызывается сразу после того, как оба prompt приняты, со смещениями `[2, 6]`. После ожидания "the burst drained" добавлена проверка `tg.flood` пуст ("both planned 429s fell into the burst"). Проверки 429 не менялись: пауза всей очереди `retry_after`, ровно один повтор, `floods.len() == 2`. В `docs/soak.md` пункт 4 сценария обновлён.
3. **Minor 3** (при сбое подготовки остаётся `%TEMP%/cctg-soak-<pid>`). Подтверждено: копия бинарника и live preflight идут до защищённого `tokio::spawn`. Добавлен `RemoveOnDrop(root)` сразу после начального `remove_dir_all`, до `create_dir_all`. Штатная уборка с повторами осталась. Проверка (`scratch/fixer/prep_failure.txt`): `CCTG_SOAK_LIVE=1` с несуществующим `CCTG_SOAK_ENV`, настоящий `.env` не читается, Telegram не вызывается. Паника на `live config`, exit 101, каталогов `cctg-soak-*` осталось 0.
4. **Nit** (Stop без state dir пишет "not kept: NotFound"). Подтверждено. `hook.rs`: если спула нет и событие не хранимое (`!spool::keeps`), результат `NotKept`, и в stderr идёт строка "hook event not delivered". Для Start/End без спула остаётся `Io(NotFound)`. Новый тест `hook_cli::an_undelivered_stop_without_a_state_dir_is_not_a_spool_failure` запускает настоящий `cctg hook Stop` без HOME/USERPROFILE/CCTG_STATE_DIR и с hub, который не слушает. На старом `hook.rs` тест падает: `hook event not delivered and not kept ... problem=spool write failed: NotFound`. С фиксом проходит.
5. **Known limitations**. В `IMPL_SUMMARY.md` добавлен раздел 5: окно около 500 мс, в котором агент делает replay раньше, чем `SessionStart`-хук положил событие в спул (сессия невидима до своего следующего хука), и порядок/возраст спула по `SystemTime`.

## 2. Skipped

- Nit `spool.rs:218` (читать `metadata().len()` до чтения файла), nit `PostError::Timeout(Duration::ZERO)` ("within 0ns"), nit про разный смысл относительного `CCTG_STATE_DIR` у hub и устройства: вне заданного объёма. Поведение они не ломают.
- Missing coverage "SessionEnd прошлого запуска из спула перед `SessionStart(resume)`": вне объёма. Ревью само говорит, что по коду путь верный.
- Альтернатива в minor 1 "брать стартовый offset из `offset` штатного hub" не принята. Для этого пришлось бы читать state пользователя, а апдейты всё равно поглощаются (запись decision в log.jsonl).

## 3. Test results

Один `CARGO_TARGET_DIR=$TEMP/cctg-018-fix-target`, `CARGO_PROFILE_DEV_DEBUG=0`, `-j 1`, `--offline`. После работы каталог удалён. Логи лежат в `scratch/fixer/`.

- `cargo fmt --all --check`: exit 0 (`fmt.txt`)
- `cargo clippy -j 1 --offline --workspace --all-targets -- -D warnings`: exit 0 (`clippy.txt`)
- `cargo test --workspace --no-fail-fast -j 1 --offline`: exit 0 (`workspace_test.txt`). cctg lib 407 passed, 1 ignored. hook_cli 8 (было 7). spool_e2e 4, stream_e2e 11, остальные цели зелёные, `soak: skipped`. После прогона каталогов `cctg-test-*` в `%TEMP%`: 0.
- `cargo test -j 1 --offline -p cctg --test soak -- --ignored`, fake, три прогона подряд (`soak_fake.txt`, это финальные три прогона, уже с проверкой пустого `flood`): все exit 0, `soak: ok`. 15.1-15.3 с, 102 вызова. 2 x 429, после каждого пауза всей очереди и один повтор. Пометки "includes a 429 pause" нет ни в одном прогоне. Задержка permission: A 124/15/15 мс, A #2 263/60/45 мс. 28 строк A #2 ушли после prompt. Каталогов `cctg-soak-*` не осталось.
- Живой прогон не запускался, как и требовалось.
