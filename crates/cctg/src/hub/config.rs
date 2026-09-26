//! Hub configuration from the process environment and an optional `.env` file.
//!
//! Values are never echoed back: errors name the variable, not its content.

use std::collections::{HashMap, HashSet};
use std::fmt;
use std::net::{Ipv4Addr, SocketAddr, SocketAddrV4};
use std::path::{Path, PathBuf};

use crate::tls::{CERT_VAR, KEY_VAR};
use crate::wire::{Secret, SecretError};

pub const TOKEN_VAR: &str = "CCTG_BOT_TOKEN";
pub const CHAT_VAR: &str = "CCTG_CHAT_ID";
pub const ALLOWLIST_VAR: &str = "CCTG_ALLOWED_USER_IDS";
/// Optional: hub state directory (the saved `getUpdates` offset); defaults to `.cctg`.
pub const STATE_VAR: &str = "CCTG_STATE_DIR";
pub const DEFAULT_STATE_DIR: &str = ".cctg";
/// Shared secret of agents and hooks; required by `cctg hub` while
/// [`SHARED_VAR`] is on.
pub const SECRET_VAR: &str = "CCTG_HUB_SECRET";
/// Optional, `on` (default) or `off`: whether the hub still takes
/// [`SECRET_VAR`] from agents and hooks. Off once every device has its own
/// secret (TASK-045, `cctg join`).
pub const SHARED_VAR: &str = "CCTG_SHARED_SECRET";
/// Optional: agent TCP listener `ip:port`; defaults to loopback.
pub const AGENT_LISTEN_VAR: &str = "CCTG_AGENT_LISTEN";
/// Optional: hook HTTP listener `ip:port`; defaults to loopback.
pub const HOOK_LISTEN_VAR: &str = "CCTG_HOOK_LISTEN";
pub const DEFAULT_AGENT_LISTEN: SocketAddr =
    SocketAddr::V4(SocketAddrV4::new(Ipv4Addr::LOCALHOST, 47291));
pub const DEFAULT_HOOK_LISTEN: SocketAddr =
    SocketAddr::V4(SocketAddrV4::new(Ipv4Addr::LOCALHOST, 47292));
/// Optional, test only: Bot API base URL; defaults to Telegram. Tests point
/// it at a fake. Plain `http://` only to a loopback host: the token travels
/// in the URL path.
pub const API_URL_VAR: &str = "CCTG_BOT_API_URL";

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
    #[error("{SECRET_VAR} is invalid: {0}")]
    Secret(SecretError),
    #[error("{SHARED_VAR} must be on or off")]
    Shared,
    #[error("{0} must be an ip:port address such as 127.0.0.1:47291 (host names are not resolved)")]
    ListenAddr(&'static str),
    #[error("{API_URL_VAR} must start with https://, or http:// to a loopback host")]
    ApiUrl,
    #[error("{CERT_VAR} and {KEY_VAR} go together: set both for TLS, or neither")]
    TlsPair,
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

    /// More than one person may write (TASK-036): messages and button
    /// answers then carry their author's name.
    pub fn is_team(&self) -> bool {
        self.0.len() > 1
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
    pub state_dir: PathBuf,
    /// `None` when `CCTG_HUB_SECRET` is unset; `cctg hub` refuses to start
    /// then, unless `shared_secret` is off.
    pub hub_secret: Option<Secret>,
    /// [`SHARED_VAR`]: agents and hooks may use `hub_secret`.
    pub shared_secret: bool,
    /// Loopback unless configured; any other address is an explicit choice.
    pub agent_listen: SocketAddr,
    pub hook_listen: SocketAddr,
    /// Bot API base URL, [`super::api::TELEGRAM_API`] unless configured.
    pub api_url: String,
    /// `CCTG_TLS_CERT` and `CCTG_TLS_KEY`: both listeners take TLS with
    /// them ([`crate::tls`]); `None`: plain TCP.
    pub tls: Option<TlsFiles>,
}

/// PEM files of the hub certificate and its private key.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TlsFiles {
    pub cert: PathBuf,
    pub key: PathBuf,
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

        let state_dir = PathBuf::from(optional(STATE_VAR).as_deref().unwrap_or(DEFAULT_STATE_DIR));
        let hub_secret = optional(SECRET_VAR)
            .map(|value| Secret::parse(&value).map_err(ConfigError::Secret))
            .transpose()?;
        let shared_secret = match optional(SHARED_VAR).map(|value| value.to_ascii_lowercase()) {
            None => true,
            Some(value) if value == "on" => true,
            Some(value) if value == "off" => false,
            Some(_) => return Err(ConfigError::Shared),
        };
        let listen = |name: &'static str, default: SocketAddr| {
            optional(name).map_or(Ok(default), |value| {
                value.parse().map_err(|_| ConfigError::ListenAddr(name))
            })
        };
        let agent_listen = listen(AGENT_LISTEN_VAR, DEFAULT_AGENT_LISTEN)?;
        let hook_listen = listen(HOOK_LISTEN_VAR, DEFAULT_HOOK_LISTEN)?;
        let api_url = optional(API_URL_VAR).unwrap_or_else(|| super::api::TELEGRAM_API.to_owned());
        if !(api_url.starts_with("https://") || is_loopback_http(&api_url)) {
            return Err(ConfigError::ApiUrl);
        }
        let tls = match (optional(CERT_VAR), optional(KEY_VAR)) {
            (Some(cert), Some(key)) => Some(TlsFiles {
                cert: PathBuf::from(cert),
                key: PathBuf::from(key),
            }),
            (None, None) => None,
            _ => return Err(ConfigError::TlsPair),
        };

        Ok(Self {
            token: BotToken(token),
            chat_id,
            allowlist,
            state_dir,
            hub_secret,
            shared_secret,
            agent_listen,
            hook_listen,
            api_url,
            tls,
        })
    }
}

/// The hub state directory alone ([`STATE_VAR`], process environment first,
/// then `env_file` or `./.env`), for `cctg hub code`: it needs neither the
/// token nor the other settings.
pub fn state_dir(env_file: Option<&Path>) -> Result<PathBuf, ConfigError> {
    let file_vars = match env_file {
        Some(path) => load_env_file(path)?,
        None if Path::new(".env").is_file() => load_env_file(Path::new(".env"))?,
        None => HashMap::new(),
    };
    let value = std::env::var(STATE_VAR)
        .ok()
        .or_else(|| file_vars.get(STATE_VAR).cloned())
        .map(|value| value.trim().to_owned())
        .filter(|value| !value.is_empty());
    Ok(PathBuf::from(value.as_deref().unwrap_or(DEFAULT_STATE_DIR)))
}

/// `http://localhost`, `http://127.x.x.x` or `http://[::1]`, any port and path.
fn is_loopback_http(url: &str) -> bool {
    let Some(rest) = url.strip_prefix("http://") else {
        return false;
    };
    let authority = rest.split(['/', '?', '#']).next().unwrap_or_default();
    let host = match authority.strip_prefix('[') {
        Some(v6) => v6.split(']').next().unwrap_or_default(),
        None => authority.split(':').next().unwrap_or_default(),
    };
    host.eq_ignore_ascii_case("localhost")
        || host
            .parse::<std::net::IpAddr>()
            .is_ok_and(|ip| ip.is_loopback())
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
        assert_eq!(config.state_dir, Path::new(DEFAULT_STATE_DIR));

        let overridden = [base.as_slice(), &[(STATE_VAR, " state ")]].concat();
        let config = Config::from_vars(vars(&overridden)).unwrap();
        assert_eq!(config.state_dir, Path::new("state"));
    }

    #[test]
    fn api_url_defaults_to_telegram_and_must_be_http() {
        let base = [
            (TOKEN_VAR, TOKEN),
            (CHAT_VAR, "-1001"),
            (ALLOWLIST_VAR, "1"),
        ];
        let config = Config::from_vars(vars(&base)).unwrap();
        assert_eq!(config.api_url, crate::hub::api::TELEGRAM_API);
        let fake = [base.as_slice(), &[(API_URL_VAR, " http://127.0.0.1:9 ")]].concat();
        let config = Config::from_vars(vars(&fake)).unwrap();
        assert_eq!(config.api_url, "http://127.0.0.1:9");
        for bad in [
            "127.0.0.1:9",
            "http://api.example.org",
            "http://10.0.0.1:8081",
            "http://127.0.0.1.example.org",
            "http://user@example.org",
        ] {
            let bad = [base.as_slice(), &[(API_URL_VAR, bad)]].concat();
            assert_eq!(
                Config::from_vars(vars(&bad)).unwrap_err(),
                ConfigError::ApiUrl
            );
        }
        for good in [
            "https://api.example.org",
            "http://localhost:8081/",
            "http://[::1]:8081",
            "http://127.0.0.2",
        ] {
            let good = [base.as_slice(), &[(API_URL_VAR, good)]].concat();
            assert!(Config::from_vars(vars(&good)).is_ok(), "{good:?}");
        }
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
    fn debug_hides_token_user_ids_and_hub_secret() {
        let config = Config::from_vars(vars(&[
            (TOKEN_VAR, TOKEN),
            (CHAT_VAR, "-1001"),
            (ALLOWLIST_VAR, "987654"),
            (SECRET_VAR, HUB_SECRET),
        ]))
        .unwrap();
        assert!(config.hub_secret.is_some());
        let debug = format!("{config:?}");
        assert!(!debug.contains("test-secret"));
        assert!(!debug.contains("987654"));
        assert!(!debug.contains(HUB_SECRET));
    }

    const HUB_SECRET: &str = "hub-secret-marker-0123456789";

    #[test]
    fn listeners_default_to_loopback() {
        let config = Config::from_vars(vars(&[
            (TOKEN_VAR, TOKEN),
            (CHAT_VAR, "-1001"),
            (ALLOWLIST_VAR, "1"),
        ]))
        .unwrap();
        assert!(config.hub_secret.is_none());
        assert_eq!(config.agent_listen, DEFAULT_AGENT_LISTEN);
        assert_eq!(config.hook_listen, DEFAULT_HOOK_LISTEN);
        assert!(config.agent_listen.ip().is_loopback());
        assert!(config.hook_listen.ip().is_loopback());
        assert_ne!(config.agent_listen.port(), config.hook_listen.port());
    }

    #[test]
    fn non_loopback_listeners_need_an_explicit_address() {
        let config = Config::from_vars(vars(&[
            (TOKEN_VAR, TOKEN),
            (CHAT_VAR, "-1001"),
            (ALLOWLIST_VAR, "1"),
            (AGENT_LISTEN_VAR, " 100.64.0.7:5000 "),
            (HOOK_LISTEN_VAR, "[::]:5001"),
        ]))
        .unwrap();
        assert_eq!(config.agent_listen, "100.64.0.7:5000".parse().unwrap());
        assert!(!config.agent_listen.ip().is_loopback());
        assert_eq!(config.hook_listen, "[::]:5001".parse().unwrap());

        for bad in ["localhost:5000", "0.0.0.0", "5000", "10.0.0.1:99999"] {
            let error = Config::from_vars(vars(&[
                (TOKEN_VAR, TOKEN),
                (CHAT_VAR, "-1001"),
                (ALLOWLIST_VAR, "1"),
                (AGENT_LISTEN_VAR, bad),
            ]))
            .unwrap_err();
            assert_eq!(error, ConfigError::ListenAddr(AGENT_LISTEN_VAR), "{bad}");
        }
    }

    #[test]
    fn tls_needs_both_files_or_none() {
        let base = [
            (TOKEN_VAR, TOKEN),
            (CHAT_VAR, "-1001"),
            (ALLOWLIST_VAR, "1"),
        ];
        assert_eq!(Config::from_vars(vars(&base)).unwrap().tls, None);
        let both = [
            base.as_slice(),
            &[(CERT_VAR, " /tls/cert.pem "), (KEY_VAR, "/tls/key.pem")],
        ]
        .concat();
        assert_eq!(
            Config::from_vars(vars(&both)).unwrap().tls,
            Some(TlsFiles {
                cert: PathBuf::from("/tls/cert.pem"),
                key: PathBuf::from("/tls/key.pem"),
            })
        );
        for one in [(CERT_VAR, "/tls/cert.pem"), (KEY_VAR, "/tls/key.pem")] {
            let half = [base.as_slice(), &[one]].concat();
            assert_eq!(
                Config::from_vars(vars(&half)).unwrap_err(),
                ConfigError::TlsPair
            );
        }
    }

    #[test]
    fn the_shared_secret_is_on_unless_turned_off() {
        let base = [
            (TOKEN_VAR, "123:abc"),
            (CHAT_VAR, "-1001"),
            (ALLOWLIST_VAR, "1"),
        ];
        let with = |extra: &[(&'static str, &'static str)]| {
            let pairs: Vec<_> = base.iter().chain(extra).copied().collect();
            Config::from_vars(vars(&pairs))
        };
        assert!(with(&[]).unwrap().shared_secret);
        assert!(with(&[(SHARED_VAR, " On ")]).unwrap().shared_secret);
        assert!(!with(&[(SHARED_VAR, "OFF")]).unwrap().shared_secret);
        let error = with(&[(SHARED_VAR, "maybe-secret-value")]).unwrap_err();
        assert_eq!(error, ConfigError::Shared);
        assert!(!error.to_string().contains("maybe-secret-value"));
    }

    #[test]
    fn a_bad_hub_secret_is_named_but_not_echoed() {
        for (value, reason) in [
            ("short-sec", SecretError::TooShort),
            ("with space inside the secret", SecretError::Charset),
        ] {
            let error = Config::from_vars(vars(&[
                (TOKEN_VAR, TOKEN),
                (CHAT_VAR, "-1001"),
                (ALLOWLIST_VAR, "1"),
                (SECRET_VAR, value),
            ]))
            .unwrap_err();
            assert_eq!(error, ConfigError::Secret(reason));
            assert!(error.to_string().contains(SECRET_VAR));
            assert!(!error.to_string().contains(value));
        }
    }
}
