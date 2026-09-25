//! `cctg run [-- <claude args>]`: starts claude in this console and starts
//! it again when its agent asked for it (TASK-040).
//!
//! claude is a child in the same console: same window, same stdin and
//! stdout. Ctrl+C and Ctrl+Break reach claude as before; `cctg run` itself
//! only listens for them and does nothing (tokio's handler; never
//! `SetConsoleCtrlHandler(NULL, TRUE)`, whose ignore flag claude would
//! inherit). claude
//! gets env `CCTG_RUN` (this pid) and `CCTG_RUN_ARGS` (its arguments). When
//! claude exits and `<state>/restart/<this pid>.json` is there, the file is
//! removed and claude starts again with the arguments it lists (the worker
//! agent made them: `--resume <session>`, the options, no prompt, see
//! [`crate::update::relaunch_args`]). The development channels dialog of
//! every start, the first one too, is answered with Enter when option 1 is
//! selected. Without a request
//! `cctg run` exits with claude's exit code.
//!
//! Not updated while it runs, so nothing else lives here: no versions, no
//! network, no protocol, no knowledge of claude's options.

use std::ffi::OsString;
use std::path::PathBuf;
use std::process::Command;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use crate::update::{self, RUN_ARGS_VAR, RUN_VAR, Request};

/// Override of the claude program (tests use a stand-in).
pub const CLAUDE_VAR: &str = "CCTG_CLAUDE";
const DIALOG_WAIT: Duration = Duration::from_secs(60);
const DIALOG_POLL: Duration = Duration::from_millis(300);

/// Runs claude until it exits without a restart request; returns its code.
pub async fn run(args: Vec<String>, state_dir: Option<PathBuf>) -> i32 {
    // Listening is enough: a signal with a listener does not end this
    // process, and claude in the same console gets its own.
    tokio::spawn(async { while tokio::signal::ctrl_c().await.is_ok() {} });
    #[cfg(windows)]
    tokio::spawn(async {
        if let Ok(mut signal) = tokio::signal::windows::ctrl_break() {
            while signal.recv().await.is_some() {}
        }
    });
    tokio::task::spawn_blocking(move || run_claude(args, state_dir))
        .await
        .unwrap_or(1)
}

fn run_claude(args: Vec<String>, state_dir: Option<PathBuf>) -> i32 {
    let request = state_dir.map(|state| update::request_path(&state, std::process::id()));
    if let Some(stale) = &request {
        let _ = std::fs::remove_file(stale);
    }
    let program = std::env::var_os(CLAUDE_VAR).unwrap_or_else(|| OsString::from("claude"));
    let run_args = serde_json::to_string(&args).unwrap_or_default();
    let mut claude_args = args;
    loop {
        let mut child = match Command::new(&program)
            .args(&claude_args)
            .env(RUN_VAR, std::process::id().to_string())
            .env(RUN_ARGS_VAR, &run_args)
            .spawn()
        {
            Ok(child) => child,
            Err(error) => {
                eprintln!("cctg run: cannot start claude: {error}");
                return 1;
            }
        };
        let running = Arc::new(AtomicBool::new(true));
        {
            let running = running.clone();
            std::thread::spawn(move || answer_channels_dialog(&running));
        }
        let code = child
            .wait()
            .ok()
            .and_then(|status| status.code())
            .unwrap_or(1);
        running.store(false, Ordering::Relaxed);
        let Some(next) = request.as_ref().and_then(take_request) else {
            return code;
        };
        eprintln!("cctg run: starting claude again");
        claude_args = next;
    }
}

/// The arguments a request file lists; the file is removed either way.
fn take_request(path: &PathBuf) -> Option<Vec<String>> {
    let bytes = std::fs::read(path).ok()?;
    let _ = std::fs::remove_file(path);
    let request: Request = serde_json::from_slice(&bytes).ok()?;
    (!request.args.is_empty()).then_some(request.args)
}

/// The development channels dialog on `screen`: `Some(true)` when option 1
/// ("I am using this for local development") is the selected one. Only the
/// rows below the last dialog header count: the window still shows the
/// claude that exited (its `❯ /exit` and empty input box) above it.
pub fn channels_dialog(screen: &[String]) -> Option<bool> {
    let header = screen
        .iter()
        .rposition(|line| line.contains("Loading development channels"))?;
    let selected = screen[header + 1..]
        .iter()
        .map(|line| line.trim_start())
        .take_while(|line| !line.starts_with("Enter to confirm"))
        .find(|line| line.starts_with('>') || line.starts_with('\u{276f}'))?;
    Some(selected.contains("1.") && selected.contains("local development"))
}

/// Watches this console until the dialog shows (Enter when option 1 is
/// selected), claude exits or [`DIALOG_WAIT`] passes.
fn answer_channels_dialog(running: &AtomicBool) {
    let until = Instant::now() + DIALOG_WAIT;
    while running.load(Ordering::Relaxed) && Instant::now() < until {
        std::thread::sleep(DIALOG_POLL);
        let Some(screen) = crate::keys::visible_lines() else {
            return;
        };
        match channels_dialog(&screen) {
            Some(true) => {
                crate::keys::write_text("\r");
                return;
            }
            Some(false) => {
                eprintln!("cctg run: the channels dialog has another option selected; left to you");
                return;
            }
            None => {}
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_channels_dialog_is_answered_only_on_option_one() {
        // TASK-013 QA screen of the dialog.
        let dialog = |options: &[&str]| -> Vec<String> {
            let mut screen = vec![
                "────────────────────────────────",
                "  WARNING: Loading development channels",
                "",
                "  --dangerously-load-development-channels is for local channel development only.",
                "",
                "  Channels: server:cctg",
                "",
            ];
            screen.extend_from_slice(options);
            screen.push("  Enter to confirm · Esc to cancel");
            screen.into_iter().map(str::to_owned).collect()
        };
        assert_eq!(
            channels_dialog(&dialog(&[
                "  > 1. I am using this for local development",
                "    2. Exit"
            ])),
            Some(true)
        );
        assert_eq!(
            channels_dialog(&dialog(&["  ❯ 1. I am using this for local development"])),
            Some(true)
        );
        assert_eq!(
            channels_dialog(&dialog(&[
                "    1. I am using this for local development",
                "  > 2. Exit"
            ])),
            Some(false)
        );
        assert_eq!(channels_dialog(&["❯ hello".to_owned()]), None);
    }

    #[test]
    fn the_exited_claude_above_the_dialog_is_not_the_selection() {
        // Same window after /exit (probe TASK-040 P2 safe_idle): the old
        // frame, even its own answered dialog, stays above the new one.
        let old: Vec<String> = [
            "  WARNING: Loading development channels",
            "  ❯ 2. Exit",
            " ▐▛███▛█   Claude Code v2.1.281",
            "❯ /exit",
            "────────────────────────────────",
            "❯",
            "────────────────────────────────",
            "cctg run: starting claude again",
        ]
        .into_iter()
        .map(str::to_owned)
        .collect();
        let with = |rows: &[&str]| -> Vec<String> {
            let mut screen = old.clone();
            screen.extend(rows.iter().map(|row| (*row).to_owned()));
            screen
        };
        assert_eq!(
            channels_dialog(&with(&[
                "  WARNING: Loading development channels",
                "",
                "  Channels: server:cctg",
                "  ❯ 1. I am using this for local development",
                "    2. Exit",
                "  Enter to confirm · Esc to cancel",
            ])),
            Some(true)
        );
        assert_eq!(
            channels_dialog(&with(&[
                "  WARNING: Loading development channels",
                "  Channels: server:cctg"
            ])),
            None,
            "the new dialog not drawn yet: nothing chosen, keep watching"
        );
        assert_eq!(
            channels_dialog(&with(&[
                "  WARNING: Loading development channels",
                "  Enter to confirm · Esc to cancel",
                "❯ typed later",
            ])),
            None,
            "nothing below the confirm line counts"
        );
    }

    #[test]
    fn only_a_valid_request_restarts() {
        let dir = crate::hub::testdir::TempDir::new("run-request");
        let path = update::request_path(dir.path(), 1);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(&path, r#"{"args":["--resume","5e55-1"]}"#).unwrap();
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
        assert_eq!(take_request(&path), None);
    }
}
