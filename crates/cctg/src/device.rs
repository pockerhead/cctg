//! Device side of the hub link, shared by `cctg hook` and `cctg agent`: where
//! the hub is, the shared secret, this device's host name, and the folder
//! spelling that goes into a slot key.
//!
//! Configuration comes from the process environment first, then from
//! `<home>/.cctg/device.env` (dotenv syntax). The file is read into memory;
//! nothing is put into the process environment, so the secret never reaches
//! the children of a hook. The Claude Code settings snippet never carries any
//! of these values.
//!
//! ```text
//! CCTG_HUB_SECRET=<same value as on the hub>
//! CCTG_HUB_HOOK_ADDR=127.0.0.1:47292   # optional, ip:port or host:port
//! CCTG_HOST=laptop                     # optional, defaults to the machine name
//! ```

use std::collections::HashMap;
use std::fmt;
use std::path::{Path, PathBuf};

use crate::hub::config::{DEFAULT_HOOK_LISTEN, SECRET_VAR};
use crate::wire::Secret;

/// `host:port` of the hub hook endpoint.
pub const HOOK_ADDR_VAR: &str = "CCTG_HUB_HOOK_ADDR";
/// Optional override of the host name this device reports.
pub const HOST_VAR: &str = "CCTG_HOST";
/// Location of the device config file, relative to the home directory.
pub const DEVICE_ENV: &str = ".cctg/device.env";

/// Why the config is unusable. Never carries a value.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ConfigProblem {
    /// `CCTG_HUB_SECRET` is not set anywhere.
    NoSecret,
    /// `CCTG_HUB_SECRET` is set but does not pass [`Secret::parse`].
    BadSecret,
    /// The device env file exists but cannot be read or parsed.
    BadFile,
}

impl fmt::Display for ConfigProblem {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::NoSecret => "CCTG_HUB_SECRET is not set (process env or ~/.cctg/device.env)",
            Self::BadSecret => "CCTG_HUB_SECRET is invalid (16+ visible ASCII characters)",
            Self::BadFile => "~/.cctg/device.env cannot be read (contents are not shown)",
        })
    }
}

#[derive(Debug, Clone)]
pub struct DeviceConfig {
    pub secret: Result<Secret, ConfigProblem>,
    pub hook_addr: String,
    pub host: String,
}

impl DeviceConfig {
    /// Never fails: a missing or broken config is reported through `secret`,
    /// so a hook can still exit quietly.
    pub fn load() -> Self {
        let file = home_dir(|name| std::env::var(name).ok())
            .map(|home| read_env_file(&home.join(DEVICE_ENV)))
            .unwrap_or(Ok(HashMap::new()));
        let (file_vars, file_ok) = match file {
            Ok(vars) => (vars, true),
            Err(()) => (HashMap::new(), false),
        };
        let mut config = Self::from_vars(|name| {
            prefer_non_empty(std::env::var(name).ok(), file_vars.get(name).cloned())
        });
        if !file_ok && config.secret == Err(ConfigProblem::NoSecret) {
            config.secret = Err(ConfigProblem::BadFile);
        }
        config
    }

    pub fn from_vars(var: impl Fn(&str) -> Option<String>) -> Self {
        let value = |name: &str| non_empty(var(name));
        let secret = match value(SECRET_VAR) {
            None => Err(ConfigProblem::NoSecret),
            Some(raw) => Secret::parse(&raw).map_err(|_| ConfigProblem::BadSecret),
        };
        let hook_addr = value(HOOK_ADDR_VAR).unwrap_or_else(|| DEFAULT_HOOK_LISTEN.to_string());
        Self {
            secret,
            hook_addr,
            host: host_name(&value),
        }
    }
}

fn prefer_non_empty(primary: Option<String>, fallback: Option<String>) -> Option<String> {
    non_empty(primary).or_else(|| non_empty(fallback))
}

fn non_empty(value: Option<String>) -> Option<String> {
    value
        .map(|value| value.trim().to_owned())
        .filter(|value| !value.is_empty())
}

fn home_dir(var: impl Fn(&str) -> Option<String>) -> Option<PathBuf> {
    let names: &[&str] = if cfg!(windows) {
        &["USERPROFILE", "HOME"]
    } else {
        &["HOME"]
    };
    names
        .iter()
        .find_map(|name| var(name).filter(|value| !value.trim().is_empty()))
        .map(PathBuf::from)
}

/// A missing file is an empty config; any other failure is `Err(())`, with no
/// `dotenvy::Error` kept: its parse error quotes the file contents.
fn read_env_file(path: &Path) -> Result<HashMap<String, String>, ()> {
    match dotenvy::from_path_iter(path) {
        Ok(iter) => iter.collect::<Result<_, _>>().map_err(|_| ()),
        Err(error) if error.not_found() => Ok(HashMap::new()),
        Err(_) => Err(()),
    }
}

/// `CCTG_HOST`, else the OS machine name, else `unknown`. The hook and the
/// agent of one device must report the same value: both call this.
fn host_name(value: &impl Fn(&str) -> Option<String>) -> String {
    value(HOST_VAR)
        .or_else(|| {
            if cfg!(windows) {
                value("COMPUTERNAME")
            } else {
                std::fs::read_to_string("/proc/sys/kernel/hostname")
                    .ok()
                    .map(|name| name.trim().to_owned())
                    .filter(|name| !name.is_empty())
                    .or_else(|| value("HOSTNAME"))
            }
        })
        .unwrap_or_else(|| "unknown".to_owned())
}

/// The folder as this device resolves it: symlinks, junctions and 8.3 short
/// names become one spelling. Falls back to `cwd` unchanged when the folder
/// cannot be resolved (deleted, no access). The hub only normalizes lexically.
pub fn canonical_cwd(cwd: &str) -> String {
    if cwd.is_empty() {
        return String::new();
    }
    match std::fs::canonicalize(cwd) {
        Ok(path) => match path.into_os_string().into_string() {
            Ok(path) => strip_verbatim(&path),
            Err(_) => cwd.to_owned(),
        },
        Err(_) => cwd.to_owned(),
    }
}

/// Windows `canonicalize` answers in the `\\?\` form; drop it where a plain
/// spelling exists (`\\?\C:\x` -> `C:\x`, `\\?\UNC\srv\share` -> `\\srv\share`).
fn strip_verbatim(path: &str) -> String {
    if let Some(rest) = path.strip_prefix(r"\\?\UNC\") {
        return format!(r"\\{rest}");
    }
    match path.strip_prefix(r"\\?\") {
        Some(rest) if rest.as_bytes().get(1) == Some(&b':') => rest.to_owned(),
        _ => path.to_owned(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const SECRET: &str = "0123456789abcdef-secret";

    fn vars<'a>(pairs: &'a [(&'a str, &'a str)]) -> impl Fn(&str) -> Option<String> + 'a {
        move |name| {
            pairs
                .iter()
                .find(|(key, _)| *key == name)
                .map(|(_, value)| (*value).to_owned())
        }
    }

    #[test]
    fn defaults_and_overrides() {
        let config = DeviceConfig::from_vars(vars(&[(SECRET_VAR, SECRET), (HOST_VAR, " laptop ")]));
        assert_eq!(config.secret.as_ref().map(Secret::expose), Ok(SECRET));
        assert_eq!(config.hook_addr, "127.0.0.1:47292");
        assert_eq!(config.host, "laptop");

        let config = DeviceConfig::from_vars(vars(&[(HOOK_ADDR_VAR, "hub.tail:47292")]));
        assert_eq!(config.secret.err(), Some(ConfigProblem::NoSecret));
        assert_eq!(config.hook_addr, "hub.tail:47292");
        assert!(!config.host.is_empty());
    }

    #[test]
    fn empty_process_values_do_not_override_device_file_values() {
        let process = vars(&[(SECRET_VAR, "  "), (HOOK_ADDR_VAR, ""), (HOST_VAR, "\t")]);
        let file = vars(&[
            (SECRET_VAR, SECRET),
            (HOOK_ADDR_VAR, "hub.tail:47292"),
            (HOST_VAR, "laptop"),
        ]);
        let config = DeviceConfig::from_vars(|name| prefer_non_empty(process(name), file(name)));
        assert_eq!(config.secret.as_ref().map(Secret::expose), Ok(SECRET));
        assert_eq!(config.hook_addr, "hub.tail:47292");
        assert_eq!(config.host, "laptop");
    }

    #[test]
    fn a_bad_secret_is_named_but_not_echoed() {
        let config = DeviceConfig::from_vars(vars(&[(SECRET_VAR, "short-secret")]));
        let problem = config.secret.clone().unwrap_err();
        assert_eq!(problem, ConfigProblem::BadSecret);
        assert!(!format!("{problem} {problem:?} {config:?}").contains("short-secret"));
        let config = DeviceConfig::from_vars(vars(&[(SECRET_VAR, SECRET)]));
        assert!(!format!("{config:?}").contains(SECRET));
    }

    #[test]
    fn verbatim_prefixes_are_dropped() {
        assert_eq!(strip_verbatim(r"\\?\C:\Users\u\dev"), r"C:\Users\u\dev");
        assert_eq!(strip_verbatim(r"\\?\UNC\srv\share\x"), r"\\srv\share\x");
        assert_eq!(strip_verbatim(r"\\?\Volume{0000}\x"), r"\\?\Volume{0000}\x");
        assert_eq!(strip_verbatim("/home/u/dev"), "/home/u/dev");
    }

    #[test]
    fn canonical_cwd_resolves_or_keeps_the_input() {
        let missing = "/definitely/not/here/cctg-device-test";
        assert_eq!(canonical_cwd(missing), missing);
        assert_eq!(canonical_cwd(""), "");
        let here = std::env::current_dir().unwrap();
        let resolved = canonical_cwd(here.to_str().unwrap());
        assert!(!resolved.starts_with(r"\\?\"), "{resolved}");
        // A lexically different spelling of the same folder resolves the same.
        let dotted = here.join(".").join("..").join(here.file_name().unwrap());
        assert_eq!(canonical_cwd(dotted.to_str().unwrap()), resolved);
    }

    #[cfg(windows)]
    #[test]
    fn canonical_cwd_expands_short_names_and_case() {
        let here = std::env::current_dir().unwrap();
        let resolved = canonical_cwd(here.to_str().unwrap());
        let upper = here.to_str().unwrap().to_uppercase();
        assert_eq!(canonical_cwd(&upper), resolved);
    }

    #[test]
    fn env_file_is_read_without_touching_the_environment() {
        let dir = std::env::temp_dir().join(format!("cctg-device-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let file = dir.join("device.env");
        std::fs::write(&file, format!("{SECRET_VAR}={SECRET}\n{HOST_VAR}=box\n")).unwrap();
        let vars = read_env_file(&file).unwrap();
        assert_eq!(vars.get(HOST_VAR).map(String::as_str), Some("box"));
        std::fs::write(&file, "CCTG_HUB_SECRET='unterminated\n").unwrap();
        assert_eq!(read_env_file(&file), Err(()));
        assert_eq!(read_env_file(&dir.join("absent.env")), Ok(HashMap::new()));
        let _ = std::fs::remove_dir_all(&dir);
    }
}
