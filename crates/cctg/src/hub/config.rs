//! Hub configuration from the process environment and an optional `.env` file.
//!
//! Values are never echoed back: errors name the variable, not its content.

use std::collections::{HashMap, HashSet};
use std::fmt;
use std::path::{Path, PathBuf};

pub const TOKEN_VAR: &str = "CCTG_BOT_TOKEN";
pub const CHAT_VAR: &str = "CCTG_CHAT_ID";
pub const ALLOWLIST_VAR: &str = "CCTG_ALLOWED_USER_IDS";
/// Optional: Claude Code projects directory; defaults to `<home>/.claude/projects`.
pub const PROJECTS_VAR: &str = "CCTG_PROJECTS_DIR";
/// Optional: hub state directory (the saved `getUpdates` offset); defaults to `.cctg`.
pub const STATE_VAR: &str = "CCTG_STATE_DIR";
pub const DEFAULT_STATE_DIR: &str = ".cctg";

#[derive(Debug, PartialEq, Eq, thiserror::Error)]
pub enum ConfigError {
    #[error("{0} is not set")]
    Missing(&'static str),
    #[error("{TOKEN_VAR} does not look like a bot token (expected <bot id>:<secret>)")]
    Token,
    #[error(
        "{CHAT_VAR} must be the Bot API supergroup id in the -100<id> form \
         (the web client shows it without the 100 prefix)"
    )]
    ChatId,
    #[error("{ALLOWLIST_VAR} entry #{0} is not a numeric Telegram user id")]
    AllowlistEntry(usize),
    #[error("{ALLOWLIST_VAR} is empty; list the Telegram user ids allowed to talk to the hub")]
    AllowlistEmpty,
    /// Carries no `dotenvy::Error`: its parse error quotes the file contents.
    #[error("cannot load env file {path}: {reason} (contents are not shown)")]
    EnvFile { path: String, reason: &'static str },
}

/// Bot token. `Debug` never prints it.
#[derive(Clone)]
pub struct BotToken(String);

impl BotToken {
    pub fn expose(&self) -> &str {
        &self.0
    }
}

impl fmt::Debug for BotToken {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("BotToken(<redacted>)")
    }
}

/// Telegram user ids allowed to reach handlers. `Debug` prints only the count.
#[derive(Clone, Default)]
pub struct Allowlist(HashSet<i64>);

impl Allowlist {
    pub fn contains(&self, user_id: i64) -> bool {
        self.0.contains(&user_id)
    }
}

impl FromIterator<i64> for Allowlist {
    fn from_iter<I: IntoIterator<Item = i64>>(iter: I) -> Self {
        Self(iter.into_iter().collect())
    }
}

impl fmt::Debug for Allowlist {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "Allowlist({} ids)", self.0.len())
    }
}

#[derive(Debug, Clone)]
pub struct Config {
    pub token: BotToken,
    /// Supergroup id in the Bot API `-100...` form.
    pub chat_id: i64,
    pub allowlist: Allowlist,
    /// Where Claude Code keeps `<encoded-cwd>/<session-id>.jsonl`. `None` when
    /// `CCTG_PROJECTS_DIR` is unset and no home directory is known.
    pub projects_dir: Option<PathBuf>,
    pub state_dir: PathBuf,
}

impl Config {
    /// Reads `env_file` (or `./.env` when it exists) without changing the process
    /// environment. Already-set process variables take precedence over the file.
    pub fn load(env_file: Option<&Path>) -> Result<Self, ConfigError> {
        let file_vars = match env_file {
            Some(path) => load_env_file(path)?,
            None => {
                let default = Path::new(".env");
                if default.is_file() {
                    load_env_file(default)?
                } else {
                    HashMap::new()
                }
            }
        };
        Self::from_vars(|name| {
            std::env::var(name)
                .ok()
                .or_else(|| file_vars.get(name).cloned())
        })
    }

    pub fn from_vars(var: impl Fn(&str) -> Option<String>) -> Result<Self, ConfigError> {
        let optional = |name: &str| {
            var(name)
                .map(|value| value.trim().to_owned())
                .filter(|value| !value.is_empty())
        };
        let required = |name: &'static str| optional(name).ok_or(ConfigError::Missing(name));

        let token = required(TOKEN_VAR)?;
        let valid_token = token
            .split_once(':')
            .is_some_and(|(id, secret)| id.parse::<u64>().is_ok() && !secret.is_empty());
        if !valid_token {
            return Err(ConfigError::Token);
        }

        let chat = required(CHAT_VAR)?;
        let chat_id = chat
            .strip_prefix("-100")
            .filter(|rest| !rest.is_empty())
            .and_then(|_| chat.parse::<i64>().ok())
            .ok_or(ConfigError::ChatId)?;

        let allowlist = required(ALLOWLIST_VAR).map_err(|_| ConfigError::AllowlistEmpty)?;
        let allowlist = allowlist
            .split(',')
            .map(str::trim)
            .filter(|entry| !entry.is_empty())
            .enumerate()
            .map(|(index, entry)| {
                entry
                    .parse::<i64>()
                    .map_err(|_| ConfigError::AllowlistEntry(index + 1))
            })
            .collect::<Result<Allowlist, _>>()?;
        if allowlist.0.is_empty() {
            return Err(ConfigError::AllowlistEmpty);
        }

        let home = if cfg!(windows) {
            optional("USERPROFILE").or_else(|| optional("HOME"))
        } else {
            optional("HOME")
        };
        let projects_dir = optional(PROJECTS_VAR)
            .map(PathBuf::from)
            .or_else(|| home.map(|home| PathBuf::from(home).join(".claude").join("projects")));
        let state_dir = PathBuf::from(optional(STATE_VAR).as_deref().unwrap_or(DEFAULT_STATE_DIR));

        Ok(Self {
            token: BotToken(token),
            chat_id,
            allowlist,
            projects_dir,
            state_dir,
        })
    }
}

fn env_file_error(path: &Path, error: dotenvy::Error) -> ConfigError {
    ConfigError::EnvFile {
        path: path.display().to_string(),
        reason: match error {
            dotenvy::Error::LineParse(..) => "a line cannot be parsed, check quotes",
            error if error.not_found() => "file not found",
            _ => "the file cannot be read",
        },
    }
}

fn load_env_file(path: &Path) -> Result<HashMap<String, String>, ConfigError> {
    let iter = dotenvy::from_path_iter(path).map_err(|error| env_file_error(path, error))?;
    iter.map(|entry| entry.map_err(|error| env_file_error(path, error)))
        .collect()
}

#[cfg(test)]
mod tests {
    use std::process::Command;

    use super::*;

    fn vars<'a>(pairs: &'a [(&'a str, &'a str)]) -> impl Fn(&str) -> Option<String> + 'a {
        move |name| {
            pairs
                .iter()
                .find(|(key, _)| *key == name)
                .map(|(_, value)| (*value).to_owned())
        }
    }

    const TOKEN: &str = "123:test-secret";

    #[test]
    fn reads_a_complete_config() {
        let config = Config::from_vars(vars(&[
            (TOKEN_VAR, TOKEN),
            (CHAT_VAR, "-1001234"),
            (ALLOWLIST_VAR, " 11, 22 ,"),
        ]))
        .unwrap();
        assert_eq!(config.chat_id, -1001234);
        assert!(config.allowlist.contains(11) && config.allowlist.contains(22));
        assert!(!config.allowlist.contains(33));
    }

    #[test]
    fn paths_have_defaults_and_overrides() {
        let base = [
            (TOKEN_VAR, TOKEN),
            (CHAT_VAR, "-1001"),
            (ALLOWLIST_VAR, "1"),
        ];
        let config = Config::from_vars(vars(&base)).unwrap();
        assert_eq!(config.projects_dir, None);
        assert_eq!(config.state_dir, Path::new(DEFAULT_STATE_DIR));

        let home_var = if cfg!(windows) { "USERPROFILE" } else { "HOME" };
        let with_home = [base.as_slice(), &[(home_var, "home-dir")]].concat();
        let config = Config::from_vars(vars(&with_home)).unwrap();
        assert_eq!(
            config.projects_dir,
            Some(Path::new("home-dir").join(".claude").join("projects"))
        );

        let overridden = [
            with_home.as_slice(),
            &[(PROJECTS_VAR, " other-projects "), (STATE_VAR, "state")],
        ]
        .concat();
        let config = Config::from_vars(vars(&overridden)).unwrap();
        assert_eq!(config.projects_dir, Some(PathBuf::from("other-projects")));
        assert_eq!(config.state_dir, Path::new("state"));
    }

    #[test]
    fn chat_id_must_use_the_bot_api_form() {
        for chat in ["-4401", "4401", "-100", "-100abc", "abc"] {
            let result = Config::from_vars(vars(&[
                (TOKEN_VAR, TOKEN),
                (CHAT_VAR, chat),
                (ALLOWLIST_VAR, "1"),
            ]));
            assert_eq!(result.unwrap_err(), ConfigError::ChatId, "{chat}");
        }
    }

    #[test]
    fn missing_and_bad_values_are_named_but_not_echoed() {
        let missing = Config::from_vars(vars(&[])).unwrap_err();
        assert_eq!(missing, ConfigError::Missing(TOKEN_VAR));

        let bad_token = Config::from_vars(vars(&[(TOKEN_VAR, "no-colon-secret")])).unwrap_err();
        assert!(!bad_token.to_string().contains("no-colon-secret"));

        let bad_entry = Config::from_vars(vars(&[
            (TOKEN_VAR, TOKEN),
            (CHAT_VAR, "-1001"),
            (ALLOWLIST_VAR, "5,x77"),
        ]))
        .unwrap_err();
        assert_eq!(bad_entry, ConfigError::AllowlistEntry(2));
        assert!(!bad_entry.to_string().contains("x77"));

        let empty = Config::from_vars(vars(&[
            (TOKEN_VAR, TOKEN),
            (CHAT_VAR, "-1001"),
            (ALLOWLIST_VAR, " , "),
        ]))
        .unwrap_err();
        assert_eq!(empty, ConfigError::AllowlistEmpty);
    }

    #[test]
    fn missing_env_file_is_named() {
        let path = Path::new("no-such-dir-cctg").join("missing.env");
        let error = Config::load(Some(&path)).unwrap_err();
        assert_eq!(
            error,
            ConfigError::EnvFile {
                path: path.display().to_string(),
                reason: "file not found",
            }
        );
    }

    #[test]
    fn env_file_token_does_not_enter_process_environment() {
        const PROCESS_TOKEN: &str = "456:process-value";
        let path = std::env::temp_dir().join(format!(
            "cctg-config-no-env-leak-{}.env",
            std::process::id()
        ));
        std::fs::write(
            &path,
            format!("{TOKEN_VAR}={TOKEN}\n{CHAT_VAR}=-1001\n{ALLOWLIST_VAR}=1\n"),
        )
        .expect("write synthetic env file");

        let test_exe = std::env::current_exe().expect("current test executable");
        let child = || {
            let mut command = Command::new(&test_exe);
            command
                .arg("--exact")
                .arg("hub::config::tests::env_file_token_does_not_enter_process_environment_child")
                .arg("--ignored")
                .env("CCTG_TEST_ENV_FILE", &path)
                .env_remove(CHAT_VAR)
                .env_remove(ALLOWLIST_VAR);
            command
        };

        let output = child()
            .env_remove(TOKEN_VAR)
            .output()
            .expect("run isolated config test");
        let precedence_output = child()
            .env(TOKEN_VAR, PROCESS_TOKEN)
            .env("CCTG_TEST_PROCESS_TOKEN", PROCESS_TOKEN)
            .output()
            .expect("run isolated precedence test");
        std::fs::remove_file(&path).expect("remove synthetic env file");

        assert!(
            output.status.success(),
            "isolated config test failed: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        assert!(
            precedence_output.status.success(),
            "isolated precedence test failed: {}",
            String::from_utf8_lossy(&precedence_output.stderr)
        );
    }

    #[test]
    #[ignore = "run by env_file_token_does_not_enter_process_environment in an isolated process"]
    fn env_file_token_does_not_enter_process_environment_child() {
        let path = std::env::var_os("CCTG_TEST_ENV_FILE").expect("test env file path");
        let process_token = std::env::var("CCTG_TEST_PROCESS_TOKEN").ok();
        if process_token.is_none() {
            assert!(std::env::var(TOKEN_VAR).is_err());
        }
        let config = Config::load(Some(Path::new(&path))).expect("load synthetic env file");
        if let Some(process_token) = process_token {
            assert!(config.token.expose() == process_token);
            assert!(std::env::var(TOKEN_VAR).is_ok_and(|value| value == process_token));
        } else {
            assert!(config.token.expose() == TOKEN);
            assert!(std::env::var(TOKEN_VAR).is_err());
        }
    }

    #[test]
    fn debug_hides_token_and_user_ids() {
        let config = Config::from_vars(vars(&[
            (TOKEN_VAR, TOKEN),
            (CHAT_VAR, "-1001"),
            (ALLOWLIST_VAR, "987654"),
        ]))
        .unwrap();
        let debug = format!("{config:?}");
        assert!(!debug.contains("test-secret"));
        assert!(!debug.contains("987654"));
    }
}
