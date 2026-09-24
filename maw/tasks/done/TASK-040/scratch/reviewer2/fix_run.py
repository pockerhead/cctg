# Reviewer-2 edit script for run.rs, run_e2e.rs, agent.rs, wire.rs, slots.rs,
# docs (applied to the %TEMP% reference workspace; kept as evidence).
# Usage: python fix_run.py <workspace root>
import sys, os
root = sys.argv[1]
files = {}


def load(rel):
    if rel not in files:
        files[rel] = open(os.path.join(root, rel), encoding='utf-8').read()
    return files[rel]


def rep(rel, old, new, cnt=1):
    s = load(rel)
    assert s.count(old) == cnt, (rel, old[:100], s.count(old))
    files[rel] = s.replace(old, new)


RUN = 'crates/cctg/src/run.rs'
rep(RUN, '''//! gets env `CCTG_RUN` (this pid) and `CCTG_RUN_ARGS` (its arguments). When
//! claude exits and `<state>/restart/<this pid>.json` names a session, the
//! file is removed and claude starts again with `--resume <session>` and
//! the other arguments; its development channels dialog is answered with
//! Enter when option 1 is selected. Without a request `cctg run` exits with
//! claude's exit code.
//!
//! Not updated while it runs, so nothing else lives here: no versions, no
//! network, no protocol.''', '''//! gets env `CCTG_RUN` (this pid) and `CCTG_RUN_ARGS` (its arguments). When
//! claude exits and `<state>/restart/<this pid>.json` is there, the file is
//! removed and claude starts again with the arguments it lists (the worker
//! agent made them: `--resume <session>`, the options, no prompt, see
//! [`crate::update::relaunch_args`]); its development channels dialog is
//! answered with Enter when option 1 is selected. Without a request
//! `cctg run` exits with claude's exit code.
//!
//! Not updated while it runs, so nothing else lives here: no versions, no
//! network, no protocol, no knowledge of claude's options.''')
rep(RUN, '''    let run_args = serde_json::to_string(&args).unwrap_or_default();
    let mut claude_args = args.clone();
    let mut resumed = false;''', '''    let run_args = serde_json::to_string(&args).unwrap_or_default();
    let mut claude_args = args;
    let mut resumed = false;''')
rep(RUN, '''        let Some(session) = request.as_ref().and_then(take_request) else {
            return code;
        };
        eprintln!("cctg run: starting claude again (--resume {session})");
        claude_args = resume_args(&args, &session);
        resumed = true;''', '''        let Some(next) = request.as_ref().and_then(take_request) else {
            return code;
        };
        eprintln!("cctg run: starting claude again");
        claude_args = next;
        resumed = true;''')
rep(RUN, '''/// The session a request file names; the file is removed either way.
fn take_request(path: &PathBuf) -> Option<String> {
    let bytes = std::fs::read(path).ok()?;
    let _ = std::fs::remove_file(path);
    let request: Request = serde_json::from_slice(&bytes).ok()?;
    update::is_session_id(&request.session_id).then_some(request.session_id)
}

/// `args` without what picks a session (`-c`/`--continue`, `-r`/`--resume`
/// with its id, `--session-id <id>`, `--fork-session`), then `--resume
/// <session>`. Everything else stays, including a prompt given as an
/// argument (it would be sent again).
pub fn resume_args(args: &[String], session: &str) -> Vec<String> {
    let mut kept = Vec::new();
    let mut skip_value = false;
    for arg in args {
        if skip_value {
            skip_value = false;
            if !arg.starts_with('-') {
                continue;
            }
        }
        match arg.as_str() {
            "-c" | "--continue" | "--fork-session" => {}
            "-r" | "--resume" | "--session-id" => skip_value = true,
            _ if arg.starts_with("--resume=") || arg.starts_with("--session-id=") => {}
            _ => kept.push(arg.clone()),
        }
    }
    kept.push("--resume".to_owned());
    kept.push(session.to_owned());
    kept
}
''', '''/// The arguments a request file lists; the file is removed either way.
fn take_request(path: &PathBuf) -> Option<Vec<String>> {
    let bytes = std::fs::read(path).ok()?;
    let _ = std::fs::remove_file(path);
    let request: Request = serde_json::from_slice(&bytes).ok()?;
    (!request.args.is_empty()).then_some(request.args)
}
''')
rep(RUN, '''    #[test]
    fn a_restart_resumes_the_session_with_the_other_arguments() {
        let base = [
            "--mcp-config",
            "m.json",
            "--settings",
            "s.json",
            "--dangerously-load-development-channels",
            "server:cctg",
        ];
        let expected: Vec<String> = args(&base)
            .into_iter()
            .chain(args(&["--resume", "5e55"]))
            .collect();
        for extra in [
            &[][..],
            &["-c"],
            &["--continue"],
            &["--resume", "old-id"],
            &["-r", "old-id"],
            &["--resume=old-id"],
            &["--session-id", "old-id", "--fork-session"],
            &["-r"],
        ] {
            let mut given = args(&base);
            given.extend(args(extra));
            assert_eq!(resume_args(&given, "5e55"), expected, "{extra:?}");
        }
        // `--resume` without an id followed by a flag keeps the flag.
        assert_eq!(
            resume_args(&args(&["--resume", "--model", "haiku"]), "x"),
            args(&["--model", "haiku", "--resume", "x"])
        );
    }

''', '')
rep(RUN, '''    fn args(list: &[&str]) -> Vec<String> {
        list.iter().map(|arg| (*arg).to_owned()).collect()
    }

''', '')
rep(RUN, '''        std::fs::write(&path, r#"{"session_id":"5e55-1"}"#).unwrap();
        assert_eq!(take_request(&path).as_deref(), Some("5e55-1"));
        assert!(!path.exists(), "a request is taken once");
        std::fs::write(&path, r#"{"session_id":"../../x y"}"#).unwrap();
        assert_eq!(take_request(&path), None);
        assert!(!path.exists());
        assert_eq!(take_request(&path), None);''', '''        std::fs::write(&path, r#"{"args":["--resume","5e55-1"]}"#).unwrap();
        assert_eq!(
            take_request(&path),
            Some(vec!["--resume".to_owned(), "5e55-1".to_owned()])
        );
        assert!(!path.exists(), "a request is taken once");
        for bad in [r#"{"args":[]}"#, r#"{"session_id":"5e55"}"#, "not json"] {
            std::fs::write(&path, bad).unwrap();
            assert_eq!(take_request(&path), None, "{bad}");
            assert!(!path.exists());
        }
        assert_eq!(take_request(&path), None);''')

E2E = 'crates/cctg/tests/run_e2e.rs'
rep(E2E, '''//! The first claude asks for a restart the way the worker agent does (a
//! request file named after `CCTG_RUN`) and exits; `cctg run` starts it again
//! with `--resume <session>` and the other arguments; the second exits with
//! code 7 and no request, which `cctg run` returns. No console, no window,
//! temp home and state.''', '''//! The first claude asks for a restart the way the worker agent does (a
//! request file named after `CCTG_RUN` with `relaunch_args` of its
//! `CCTG_RUN_ARGS`) and exits; `cctg run` starts it again with exactly those
//! arguments (`--resume <session>`, no prompt); the second exits with code 7
//! and no request, which `cctg run` returns. No console, no window, temp home
//! and state.''')
rep(E2E, '''    let state = std::path::PathBuf::from(std::env::var("CCTG_STATE_DIR").unwrap());
    let request = cctg::update::request_path(&state, run.parse().unwrap());
    std::fs::create_dir_all(request.parent().unwrap()).unwrap();
    std::fs::write(&request, json!({ "session_id": SESSION }).to_string()).unwrap();''', '''    let state = std::path::PathBuf::from(std::env::var("CCTG_STATE_DIR").unwrap());
    let request = cctg::update::request_path(&state, run.parse().unwrap());
    std::fs::create_dir_all(request.parent().unwrap()).unwrap();
    let first: Vec<String> =
        serde_json::from_str(&std::env::var("CCTG_RUN_ARGS").unwrap()).unwrap();
    let request_args = cctg::update::relaunch_args(&first, SESSION);
    std::fs::write(&request, json!({ "args": request_args }).to_string()).unwrap();''')
rep(E2E, '''    let given = ["--settings", "s.json", "--continue", "--model", "haiku"];''', '''    let given = [
        "--settings",
        "s.json",
        "--continue",
        "--model",
        "haiku",
        "the first prompt",
    ];''')

AGENT = 'crates/cctg/src/agent.rs'
rep(AGENT, '''    let mut worker = Worker::from_env(
        |name| std::env::var(name).ok(),
        claude_pid,
        config.state_dir.clone(),
        presser.is_some(),
    );''', '''    let mut worker = Worker::from_env(
        |name| std::env::var(name).ok(),
        claude_pid,
        config.state_dir.clone(),
        presser.is_some(),
    );
    // Only the `cctg run` that started this claude restarts it; an inherited
    // `CCTG_RUN` of another session's terminal does not count.
    if let (Some(run), Some(claude)) = (worker.run_pid, claude_pid) {
        let chain = tokio::task::spawn_blocking(move || proctree::ancestors(claude))
            .await
            .ok()
            .flatten()
            .unwrap_or_default();
        if !update::launched_by(&chain, run) {
            debug!("CCTG_RUN is not this claude's parent; no restarts");
            worker.run_pid = None;
        }
    }''')
rep(AGENT, '''use crate::update::{Plan, Worker};''', '''use crate::update::{self, Plan, Worker};''')
rep(AGENT, '''        self_update: worker.self_update(),
        restartable: worker.restartable(),
    });''', '''        self_update: worker.self_update(),
    });''')

WIRE = 'crates/cctg/src/wire.rs'
rep(WIRE, '''    #[serde(default)]
    pub self_update: bool,
    /// Its claude runs under `cctg run` and the agent can type into its
    /// console: a restart of claude in the same window is possible.
    #[serde(default)]
    pub restartable: bool,
}''', '''    #[serde(default)]
    pub self_update: bool,
}''')
rep(WIRE, '''    /// Nothing newer than the running agent is on this machine.
    NoNewBinary,
''', '')
rep(WIRE, '''                self_update: true,
                restartable: false,
            }),''', '''                self_update: true,
            }),''')

SLOTS = 'crates/cctg/src/hub/slots.rs'
rep(SLOTS, '''                text: status::outdated_text(agent.as_deref(), crate::client::short(&hub)),
                html: None,
                reply_markup: Some(keyboard),
                permission: false,
                reply_to: None,
                notify: false,''', '''                text: status::outdated_text(agent.as_deref(), crate::client::short(&hub)),
                html: None,
                reply_markup: Some(keyboard),
                permission: false,
                reply_to: None,
                // Loud (decision 2026-09-24): the user asked to be told.
                notify: true,''')
rep(SLOTS, '''            build: build.into(),
            self_update,
            restartable: false,
        })''', '''            build: build.into(),
            self_update,
        })''')
rep(SLOTS, '''    /// One warning per hub build in the topic of each live current session
    /// whose agent is outdated, with ⬆️ Обновить.''', '''    /// One loud warning per hub build in the topic of each live current
    /// session whose agent is outdated, with ⬆️ Обновить.''')

UE2E = 'crates/cctg/tests/update_e2e.rs'
rep(UE2E, '''    assert!(client.self_update, "under the shim");
    assert!(!client.restartable, "no cctg run");
''', '''    assert!(client.self_update, "under the shim");
''')

DOC = 'docs/poc.md'
rep(DOC, '''Заданием от агента (файл `<state>/restart/<pid cctg run>.json`, `<state>` это `CCTG_STATE_DIR` или `~/.cctg`) он запускает `claude --resume <сессия>` с теми же аргументами (без `-c`, `-r`, `--session-id`, `--fork-session`) в том же окне и сам отвечает Enter на вопрос про development channels. Промпт, переданный аргументом, при таком перезапуске уйдёт ещё раз.''', '''По заявке агента (файл `<state>/restart/<pid cctg run>.json`, `<state>` это `CCTG_STATE_DIR` или `~/.cctg`) он запускает claude ещё раз в том же окне с аргументами из заявки и сам отвечает Enter на вопрос про development channels. Аргументы собирает агент: те же опции, без `-c`, `-r`, `--session-id`, `--fork-session` и без промпта, переданного аргументом (он не уходит второй раз), плюс `--resume <сессия>`. Заявку принимает только тот `cctg run`, который сам запустил этого claude.''')

for rel, s in files.items():
    open(os.path.join(root, rel), 'w', encoding='utf-8', newline='\n').write(s)
print('ok', sorted(files))
