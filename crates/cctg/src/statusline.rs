//! `cctg statusline`: the `statusLine` command of cctg sessions (TASK-029).
//!
//! Claude Code runs it with its status line JSON on stdin after every
//! assistant message (debounced 300 ms) and shows what it prints. It sends
//! the numbers the status message in Telegram shows (model, effort, context
//! and rate limit percentages) to the hub as one `status_line` hook event,
//! with a short timeout and never waiting for more than that, and prints the
//! status line the terminal would have shown without cctg: the
//! `statusLine.command` of the user's own settings (`$CLAUDE_CONFIG_DIR` or
//! `~/.claude/settings.json`, read on every call), run with the same stdin
//! through the shell Claude Code uses for it, or, without one, a short line
//! of its own. It exits with the user's command's exit code (Claude Code
//! blanks the line on a non-zero one, as it would without cctg), else 0;
//! problems go to stderr as fixed text, never the input or the secret.

use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::time::Duration;

use serde_json::Value;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tracing::{debug, warn};

use crate::device::{self, DeviceConfig};
use crate::hook;
use crate::wire::{HookEvent, HookPost};

/// Budget of the POST to the hub. It runs next to the user's command, but
/// the terminal waits for this process, so a stopped hub may cost the status
/// line this much (a closed local port on Windows is retried until the
/// timeout). Kept well under the 150 ms the whole call may add.
pub const POST_TIMEOUT: Duration = Duration::from_millis(80);
/// Claude Code writes the whole input at once and closes stdin.
const STDIN_TIMEOUT: Duration = Duration::from_millis(500);
const MAX_INPUT: u64 = 1 << 20;
/// Longest run of the user's own command; Claude Code cancels a status line
/// run anyway when a newer update comes.
const CHAIN_TIMEOUT: Duration = Duration::from_secs(10);
/// Longest output of the user's command passed on.
const MAX_OUTPUT: u64 = 64 << 10;
/// Set for the user's command: a `cctg statusline` inside it neither chains
/// again nor posts a second time.
pub const GUARD_VAR: &str = "CCTG_STATUSLINE";
const CONFIG_DIR_VAR: &str = "CLAUDE_CONFIG_DIR";
/// Where Claude Code looks for Git Bash first (its documented override).
const GIT_BASH_VAR: &str = "CLAUDE_CODE_GIT_BASH_PATH";
/// Longest model and effort names sent.
const MAX_NAME: usize = 64;

/// Runs one status line call and returns the exit code: the user's command's,
/// or 0 when cctg printed its own line. Never fails.
pub async fn run() -> i32 {
    let input = hook::read_stdin(MAX_INPUT, STDIN_TIMEOUT).unwrap_or_default();
    let nested = std::env::var_os(GUARD_VAR).is_some();
    let value: Value = serde_json::from_slice(&input).unwrap_or(Value::Null);
    let posting =
        (!nested)
            .then(|| event(&value))
            .flatten()
            .map(|(session, cwd, transcript, event)| {
                let config = DeviceConfig::load();
                let post = HookPost::new(
                    config.host.clone(),
                    session,
                    device::canonical_cwd(&cwd),
                    transcript,
                    event,
                );
                tokio::spawn(async move {
                    let Ok(secret) = &config.secret else {
                        return;
                    };
                    if let Err(error) =
                        hook::post(&config.hook_addr, secret, &post, POST_TIMEOUT).await
                    {
                        debug!(%error, "status line numbers not delivered");
                    }
                })
            });
    let chained = match (!nested).then(user_command).flatten() {
        Some(command) => run_chained(&command, &input).await,
        None => None,
    };
    let (output, code) = chained.unwrap_or_else(|| (own_line(&value).into_bytes(), 0));
    {
        let mut stdout = std::io::stdout().lock();
        let _ = std::io::Write::write_all(&mut stdout, &output);
        let _ = std::io::Write::flush(&mut stdout);
    }
    if let Some(posting) = posting {
        let _ = posting.await;
    }
    code
}

fn text(value: &Value, path: &[&str]) -> Option<String> {
    let mut at = value;
    for key in path {
        at = at.get(key)?;
    }
    at.as_str()
        .map(str::trim)
        .filter(|text| !text.is_empty())
        .map(|text| text.chars().take(MAX_NAME).collect())
}

/// A percentage rounded to a whole number; anything outside 0..=100 (the
/// documented range) is dropped.
fn percent(value: &Value, path: &[&str]) -> Option<u32> {
    let mut at = value;
    for key in path {
        at = at.get(key)?;
    }
    let number = at.as_f64()?;
    (0.0..=100.0)
        .contains(&number)
        .then(|| number.round() as u32)
}

/// The session and the `status_line` event of one status line input, or
/// `None` without a session id.
pub fn event(input: &Value) -> Option<(String, String, String, HookEvent)> {
    let session = text(input, &["session_id"])?;
    let cwd = text(input, &["workspace", "current_dir"])
        .or_else(|| text(input, &["cwd"]))
        .unwrap_or_default();
    let transcript = input
        .get("transcript_path")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_owned();
    let event = HookEvent::StatusLine {
        model: text(input, &["model", "display_name"]).or_else(|| text(input, &["model", "id"])),
        effort: text(input, &["effort", "level"]),
        context: percent(input, &["context_window", "used_percentage"]),
        five_hour: percent(input, &["rate_limits", "five_hour", "used_percentage"]),
        seven_day: percent(input, &["rate_limits", "seven_day", "used_percentage"]),
    };
    Some((session, cwd, transcript, event))
}

/// cctg's own short line: `Opus · ctx 50% · 5h 3% · 7d 92%`, the parts
/// Claude Code gave.
pub fn own_line(input: &Value) -> String {
    let mut parts = Vec::new();
    if let Some(model) =
        text(input, &["model", "display_name"]).or_else(|| text(input, &["model", "id"]))
    {
        parts.push(model);
    }
    for (label, path) in [
        ("ctx", &["context_window", "used_percentage"][..]),
        ("5h", &["rate_limits", "five_hour", "used_percentage"]),
        ("7d", &["rate_limits", "seven_day", "used_percentage"]),
    ] {
        if let Some(value) = percent(input, path) {
            parts.push(format!("{label} {value}%"));
        }
    }
    parts.join(" · ")
}

/// The user's own status line command from their Claude Code settings.
fn user_command() -> Option<String> {
    let path = settings_path(&|name| std::env::var(name).ok())?;
    let settings = std::fs::read_to_string(path).ok()?;
    command_of(&settings)
}

/// `$CLAUDE_CONFIG_DIR/settings.json`, else `<home>/.claude/settings.json`.
pub fn settings_path(var: &impl Fn(&str) -> Option<String>) -> Option<PathBuf> {
    if let Some(dir) = var(CONFIG_DIR_VAR).filter(|dir| !dir.trim().is_empty()) {
        return Some(Path::new(dir.trim()).join("settings.json"));
    }
    device::home_dir(var).map(|home| home.join(".claude").join("settings.json"))
}

/// `statusLine.command` of a settings file, when its type is `command`.
pub fn command_of(settings: &str) -> Option<String> {
    let settings: Value = serde_json::from_str(settings).ok()?;
    let line = settings.get("statusLine")?;
    if line.get("type").and_then(Value::as_str) != Some("command") {
        return None;
    }
    line.get("command")
        .and_then(Value::as_str)
        .filter(|command| !command.trim().is_empty())
        .map(str::to_owned)
}

/// Runs the user's command with `input` on stdin and returns what it
/// printed, at most [`MAX_OUTPUT`] (the rest is read and dropped, so the
/// command is never stuck on a full pipe), and its exit code (1 without
/// one). `None` when it could not start or ran out of time; then it is
/// killed.
async fn run_chained(command: &str, input: &[u8]) -> Option<(Vec<u8>, i32)> {
    let mut child = match shell(command)
        .env(GUARD_VAR, "1")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .kill_on_drop(true)
        .spawn()
    {
        Ok(child) => child,
        Err(error) => {
            warn!(kind = ?error.kind(), "own status line command did not start");
            return None;
        }
    };
    let mut stdin = child.stdin.take()?;
    let mut stdout = child.stdout.take()?;
    let input = input.to_vec();
    let work = async move {
        let feed = async move {
            let _ = stdin.write_all(&input).await;
            drop(stdin);
        };
        let read = async {
            let mut output = Vec::new();
            let _ = (&mut stdout)
                .take(MAX_OUTPUT)
                .read_to_end(&mut output)
                .await;
            let _ = tokio::io::copy(&mut stdout, &mut tokio::io::sink()).await;
            output
        };
        let ((), output) = tokio::join!(feed, read);
        let code = child
            .wait()
            .await
            .ok()
            .and_then(|status| status.code())
            .unwrap_or(1);
        (output, code)
    };
    // On a timeout `work` and the child in it are dropped: `kill_on_drop`.
    match tokio::time::timeout(CHAIN_TIMEOUT, work).await {
        Ok(output) => Some(output),
        Err(_) => {
            warn!("own status line command ran too long; cctg's line shown");
            None
        }
    }
}

/// The shell Claude Code runs status line commands with: Git Bash on
/// Windows when it is installed, else PowerShell; `sh` elsewhere.
fn shell(command: &str) -> tokio::process::Command {
    if cfg!(windows) {
        if let Some(bash) = git_bash(&|name| std::env::var(name).ok(), &|path| path.is_file()) {
            let mut shell = tokio::process::Command::new(bash);
            shell.arg("-c").arg(command);
            return shell;
        }
        let mut shell = tokio::process::Command::new("powershell");
        shell.args(["-NoProfile", "-Command", command]);
        return shell;
    }
    let mut shell = tokio::process::Command::new("sh");
    shell.arg("-c").arg(command);
    shell
}

/// Git Bash: `CLAUDE_CODE_GIT_BASH_PATH`, then `SHELL` when it names a
/// `bash.exe` file, then `bash.exe` in `EXEPATH` (set in
/// Git Bash sessions), then next to a `git.exe` found on `PATH`
/// (`<git>\cmd\git.exe` or `<git>\bin\git.exe` -> `<git>\bin\bash.exe`).
pub fn git_bash(
    var: &impl Fn(&str) -> Option<String>,
    is_file: &impl Fn(&Path) -> bool,
) -> Option<PathBuf> {
    let set = |name: &str| var(name).filter(|value| !value.trim().is_empty());
    if let Some(path) = set(GIT_BASH_VAR).map(PathBuf::from)
        && is_file(&path)
    {
        return Some(path);
    }
    // Claude Code's own status line runs see `SHELL` as the Windows path of
    // the bash.exe it uses (TASK-029 probe, 2.1.281).
    if let Some(path) = set("SHELL")
        .map(PathBuf::from)
        .filter(|path| path.file_name().is_some_and(|name| name == "bash.exe"))
        && is_file(&path)
    {
        return Some(path);
    }
    if let Some(path) = set("EXEPATH").map(|dir| Path::new(&dir).join("bash.exe"))
        && is_file(&path)
    {
        return Some(path);
    }
    let paths = set("PATH")?;
    std::env::split_paths(&paths)
        .filter(|dir| is_file(&dir.join("git.exe")))
        .filter_map(|dir| dir.parent().map(|git| git.join("bin").join("bash.exe")))
        .find(|bash| is_file(bash))
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    /// The shape of a real status line input (Claude Code 2.1.281, values
    /// replaced): TASK-029 `scratch/planner/statusline_input_shape.json`.
    fn sample() -> Value {
        json!({
            "session_id": "5e551017-0000-4000-8000-000000000001",
            "transcript_path": "/p/s.jsonl",
            "cwd": "/w",
            "effort": { "level": "medium" },
            "model": { "id": "claude-opus-5-5[1m]", "display_name": "Opus 5.5 (1M context)" },
            "workspace": { "current_dir": "/w/app", "project_dir": "/w", "added_dirs": [] },
            "context_window": { "used_percentage": 50, "remaining_percentage": 50,
                "current_usage": { "input_tokens": 2 } },
            "rate_limits": {
                "five_hour": { "used_percentage": 3, "resets_at": 1790273400 },
                "seven_day": { "used_percentage": 91.6, "resets_at": 1790614800 },
            },
        })
    }

    #[test]
    fn the_numbers_come_from_the_status_line_input() {
        let (session, cwd, transcript, event) = event(&sample()).unwrap();
        assert_eq!(session, "5e551017-0000-4000-8000-000000000001");
        assert_eq!(cwd, "/w/app");
        assert_eq!(transcript, "/p/s.jsonl");
        assert_eq!(
            event,
            HookEvent::StatusLine {
                model: Some("Opus 5.5 (1M context)".into()),
                effort: Some("medium".into()),
                context: Some(50),
                five_hour: Some(3),
                seven_day: Some(92),
            }
        );
        assert_eq!(
            own_line(&sample()),
            "Opus 5.5 (1M context) · ctx 50% · 5h 3% · 7d 92%"
        );
    }

    #[test]
    fn missing_null_and_odd_fields_are_left_out() {
        // Early in a session: no rate limits, a null context, no effort.
        let early = json!({
            "session_id": "s",
            "model": { "id": "claude-x" },
            "context_window": { "used_percentage": null },
            "rate_limits": { "five_hour": { "used_percentage": "3" } },
        });
        let (_, cwd, _, event) = event(&early).unwrap();
        assert_eq!(cwd, "");
        assert_eq!(
            event,
            HookEvent::StatusLine {
                model: Some("claude-x".into()),
                effort: None,
                context: None,
                five_hour: None,
                seven_day: None,
            }
        );
        assert_eq!(own_line(&early), "claude-x");
        assert!(super::event(&json!({ "model": {} })).is_none());
        assert!(super::event(&Value::Null).is_none());
        assert_eq!(own_line(&Value::Null), "");
        for bad in [json!(-1), json!(100.6), json!(999), json!(f64::MAX)] {
            let input = json!({ "session_id": "s", "context_window": { "used_percentage": bad },
                "rate_limits": { "seven_day": { "used_percentage": bad } } });
            assert!(
                matches!(
                    super::event(&input),
                    Some((
                        _,
                        _,
                        _,
                        HookEvent::StatusLine {
                            context: None,
                            seven_day: None,
                            ..
                        }
                    ))
                ),
                "{bad}"
            );
        }
        let edges = json!({ "session_id": "s", "context_window": { "used_percentage": 0 },
            "rate_limits": { "seven_day": { "used_percentage": 100 } } });
        assert!(matches!(
            super::event(&edges),
            Some((
                _,
                _,
                _,
                HookEvent::StatusLine {
                    context: Some(0),
                    seven_day: Some(100),
                    ..
                }
            ))
        ));
        assert!(POST_TIMEOUT <= Duration::from_millis(100));
    }

    #[test]
    fn the_users_command_is_read_from_their_settings() {
        assert_eq!(
            command_of(r#"{"statusLine":{"type":"command","command":"'py' 'sl.py'"}}"#).as_deref(),
            Some("'py' 'sl.py'")
        );
        for settings in [
            r#"{"statusLine":{"type":"static","command":"x"}}"#,
            r#"{"statusLine":{"type":"command","command":"  "}}"#,
            r#"{"statusLine":{"command":"x"}}"#,
            r#"{"hooks":{}}"#,
            "not json",
        ] {
            assert_eq!(command_of(settings), None, "{settings}");
        }
        let vars = |config: Option<&str>| {
            let config = config.map(str::to_owned);
            move |name: &str| match name {
                "CLAUDE_CONFIG_DIR" => config.clone(),
                "USERPROFILE" | "HOME" => Some("/home/u".to_owned()),
                _ => None,
            }
        };
        assert_eq!(
            settings_path(&vars(Some("/cfg"))),
            Some(Path::new("/cfg").join("settings.json"))
        );
        assert_eq!(
            settings_path(&vars(Some("  "))),
            Some(Path::new("/home/u").join(".claude").join("settings.json"))
        );
    }

    #[test]
    fn git_bash_is_found_like_claude_code_finds_it() {
        let git = Path::new("C:/Git");
        let files = [
            git.join("cmd").join("git.exe"),
            git.join("bin").join("bash.exe"),
        ];
        let is_file = |path: &Path| files.iter().any(|file| file == path);
        let path = std::env::join_paths([Path::new("C:/other"), &git.join("cmd")])
            .unwrap()
            .into_string()
            .unwrap();
        let vars = |override_path: Option<&str>| {
            let (override_path, path) = (override_path.map(str::to_owned), path.clone());
            move |name: &str| match name {
                "CLAUDE_CODE_GIT_BASH_PATH" => override_path.clone(),
                "PATH" => Some(path.clone()),
                // What the Bash tool sees: an MSYS path, not a file.
                "SHELL" => Some("/usr/bin/bash".to_owned()),
                _ => None,
            }
        };
        assert_eq!(
            git_bash(&vars(None), &is_file),
            Some(git.join("bin").join("bash.exe"))
        );
        // The override wins only when it names a file.
        let custom = git.join("bin").join("bash.exe");
        assert_eq!(
            git_bash(&vars(Some(custom.to_str().unwrap())), &is_file),
            Some(custom)
        );
        assert_eq!(
            git_bash(&vars(Some("C:/missing/bash.exe")), &is_file),
            Some(git.join("bin").join("bash.exe"))
        );
        assert_eq!(git_bash(&vars(None), &|_: &Path| false), None);
    }
}
