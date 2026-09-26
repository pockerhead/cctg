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
//! On Linux and macOS (TASK-044), when stdin and stdout are terminals,
//! claude runs in a pseudo-terminal `cctg run` holds ([`crate::term`]):
//! same window, every key relayed as it is typed, the window size followed,
//! a claude that suspends itself (Ctrl+Z) suspends `cctg run` with it; the
//! agent reads that terminal and types into it through a socket. Without a
//! terminal (piped, `-p`) claude runs as before, without console keys.
//!
//! Not updated while it runs, so nothing else lives here: no versions, no
//! network, no protocol, no knowledge of claude's options.

use std::ffi::{OsStr, OsString};
use std::path::PathBuf;
use std::process::{Child, Command};
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
    let place = Place::new(state_dir.as_deref());
    #[cfg(unix)]
    if let Place::Pty(host) = &place {
        follow_signals(host);
    }
    let code = tokio::task::spawn_blocking({
        let place = place.clone();
        move || run_claude(args, state_dir, &place)
    })
    .await
    .unwrap_or(1);
    place.finish();
    code
}

/// Where claude runs: in this console (Windows, or wherever `cctg run` has
/// no terminal to hold), or in the pseudo-terminal of `cctg run` (Unix).
#[derive(Clone)]
enum Place {
    Console,
    #[cfg(unix)]
    Pty(Arc<crate::term::Host>),
}

impl Place {
    #[cfg(unix)]
    fn new(state_dir: Option<&std::path::Path>) -> Self {
        let socket = state_dir.map(|state| crate::term::socket_path(state, std::process::id()));
        crate::term::Host::start(socket).map_or(Place::Console, Place::Pty)
    }

    #[cfg(not(unix))]
    fn new(_state_dir: Option<&std::path::Path>) -> Self {
        Place::Console
    }

    fn command(&self, program: &OsStr) -> std::io::Result<Command> {
        match self {
            Place::Console => Ok(Command::new(program)),
            #[cfg(unix)]
            Place::Pty(host) => host.command(program),
        }
    }

    /// claude's exit code.
    fn wait(&self, mut child: Child) -> i32 {
        match self {
            Place::Console => child
                .wait()
                .ok()
                .and_then(|status| status.code())
                .unwrap_or(1),
            #[cfg(unix)]
            Place::Pty(host) => host.wait(child.id()),
        }
    }

    fn lines(&self) -> Option<Vec<String>> {
        match self {
            Place::Console => crate::keys::visible_lines(),
            #[cfg(unix)]
            Place::Pty(host) => host.lines(),
        }
    }

    fn enter(&self) -> bool {
        match self {
            Place::Console => crate::keys::write_text("\r"),
            #[cfg(unix)]
            Place::Pty(host) => host.write(b"\r"),
        }
    }

    /// `cctg run` was told to stop (SIGTERM, SIGHUP): no restart.
    fn stopping(&self) -> bool {
        match self {
            Place::Console => false,
            #[cfg(unix)]
            Place::Pty(host) => host.stopping(),
        }
    }

    /// A line of `cctg run` on stderr (in raw mode a line needs its `\r`).
    fn say(&self, text: &str) {
        match self {
            Place::Console => eprintln!("{text}"),
            #[cfg(unix)]
            Place::Pty(_) => eprint!("{text}\r\n"),
        }
    }

    fn finish(&self) {
        #[cfg(unix)]
        if let Place::Pty(host) = self {
            host.finish();
        }
    }
}

/// The user's window size goes to claude's terminal; SIGTERM and SIGHUP
/// reach claude as SIGHUP (its terminal is gone) and end `cctg run` after it.
#[cfg(unix)]
fn follow_signals(host: &Arc<crate::term::Host>) {
    use tokio::signal::unix::{SignalKind, signal};

    if let Ok(mut resized) = signal(SignalKind::window_change()) {
        let host = host.clone();
        tokio::spawn(async move {
            while resized.recv().await.is_some() {
                host.sync_size();
            }
        });
    }
    for kind in [SignalKind::terminate(), SignalKind::hangup()] {
        if let Ok(mut stop) = signal(kind) {
            let host = host.clone();
            tokio::spawn(async move {
                while stop.recv().await.is_some() {
                    host.hang_up();
                }
            });
        }
    }
}

fn run_claude(args: Vec<String>, state_dir: Option<PathBuf>, place: &Place) -> i32 {
    let request = state_dir.map(|state| update::request_path(&state, std::process::id()));
    if let Some(stale) = &request {
        let _ = std::fs::remove_file(stale);
    }
    let program = std::env::var_os(CLAUDE_VAR).unwrap_or_else(|| OsString::from("claude"));
    let run_args = serde_json::to_string(&args).unwrap_or_default();
    let mut claude_args = args;
    loop {
        let spawned = place.command(&program).and_then(|mut command| {
            command
                .args(&claude_args)
                .env(RUN_VAR, std::process::id().to_string())
                .env(RUN_ARGS_VAR, &run_args)
                .spawn()
        });
        let child = match spawned {
            Ok(child) => child,
            Err(error) => {
                place.say(&format!("cctg run: cannot start claude: {error}"));
                return 1;
            }
        };
        let running = Arc::new(AtomicBool::new(true));
        {
            let running = running.clone();
            let place = place.clone();
            std::thread::spawn(move || answer_channels_dialog(&running, &place));
        }
        let code = place.wait(child);
        running.store(false, Ordering::Relaxed);
        if place.stopping() {
            return code;
        }
        let Some(next) = request.as_ref().and_then(take_request) else {
            return code;
        };
        place.say("cctg run: starting claude again");
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

/// Watches claude's screen until the dialog shows (Enter when option 1 is
/// selected), claude exits or [`DIALOG_WAIT`] passes.
fn answer_channels_dialog(running: &AtomicBool, place: &Place) {
    let until = Instant::now() + DIALOG_WAIT;
    while running.load(Ordering::Relaxed) && Instant::now() < until {
        std::thread::sleep(DIALOG_POLL);
        let Some(screen) = place.lines() else {
            return;
        };
        match channels_dialog(&screen) {
            Some(true) => {
                place.enter();
                return;
            }
            Some(false) => {
                place.say("cctg run: the channels dialog has another option selected; left to you");
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
