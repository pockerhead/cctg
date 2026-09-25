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
//! CCTG_HUB_AGENT_ADDR=127.0.0.1:47291  # optional, the hub agent listener
//! CCTG_HOST=laptop                     # optional, defaults to the machine name
//! CCTG_STATE_DIR=/abs/state/dir         # optional, absolute; holds the hook spool
//! CCTG_HUB_CERT_SHA256=AB:CD:...        # optional: TLS to a hub with this certificate
//! ```
//!
//! Without `CCTG_HUB_CERT_SHA256` both links are plain TCP, and only to a
//! loopback address; a hub elsewhere needs the pin ([`crate::tls`]).

use std::collections::HashMap;
use std::fmt;
use std::path::{Path, PathBuf};

use crate::hub::config::{DEFAULT_AGENT_LISTEN, DEFAULT_HOOK_LISTEN, SECRET_VAR, STATE_VAR};
use crate::tls::{self, CertPin, HubAddr, PIN_VAR};
use crate::wire::Secret;

/// `host:port` of the hub hook endpoint.
pub const HOOK_ADDR_VAR: &str = "CCTG_HUB_HOOK_ADDR";
/// `host:port` of the hub agent listener (`cctg agent`).
pub const AGENT_ADDR_VAR: &str = "CCTG_HUB_AGENT_ADDR";
/// Optional override of the host name this device reports.
pub const HOST_VAR: &str = "CCTG_HOST";
/// Location of the device config file, relative to the home directory.
pub const DEVICE_ENV: &str = ".cctg/device.env";
/// Device state directory under the home directory (the hook spool lives in
/// `<state>/spool`), unless `CCTG_STATE_DIR` names an absolute one.
pub const DEVICE_STATE: &str = ".cctg";

/// Why the config is unusable. Never carries a value.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ConfigProblem {
    /// `CCTG_HUB_SECRET` is not set anywhere.
    NoSecret,
    /// `CCTG_HUB_SECRET` is set but does not pass [`Secret::parse`].
    BadSecret,
    /// The device env file exists but cannot be read or parsed.
    BadFile,
    /// `CCTG_HUB_CERT_SHA256` is set but is no sha256 fingerprint.
    BadPin,
    /// A hub address is not `host:port`.
    BadAddr,
    /// A hub address beyond loopback without `CCTG_HUB_CERT_SHA256`.
    PlainRemote,
}

impl fmt::Display for ConfigProblem {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::NoSecret => "CCTG_HUB_SECRET is not set (process env or ~/.cctg/device.env)",
            Self::BadSecret => "CCTG_HUB_SECRET is invalid (16+ visible ASCII characters)",
            Self::BadFile => "~/.cctg/device.env cannot be read (contents are not shown)",
            Self::BadPin => "CCTG_HUB_CERT_SHA256 is not a sha256 fingerprint (64 hex digits)",
            Self::BadAddr => "a CCTG_HUB_*_ADDR value is not host:port",
            Self::PlainRemote => {
                "the hub is not on this machine: set CCTG_HUB_CERT_SHA256 for TLS \
                 (the secret never goes over a network in plain text)"
            }
        })
    }
}

#[derive(Debug, Clone)]
pub struct DeviceConfig {
    pub secret: Result<Secret, ConfigProblem>,
    pub hook_addr: String,
    pub agent_addr: String,
    pub host: String,
    /// Where this device keeps its state: an absolute `CCTG_STATE_DIR`, else
    /// `<home>/.cctg`. `None` without either. A relative value is ignored: a
    /// hook runs in the session's folder and must not write there.
    pub state_dir: Option<PathBuf>,
    /// `CCTG_HUB_CERT_SHA256`: `None` unset (plain, loopback only),
    /// `Some(Err)` set but unusable.
    pub pin: Option<Result<CertPin, ConfigProblem>>,
}

impl DeviceConfig {
    /// Never fails: a missing or broken config is reported through `secret`,
    /// so a hook can still exit quietly.
    pub fn load() -> Self {
        let file = home_dir(&|name| std::env::var(name).ok())
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
        let agent_addr = value(AGENT_ADDR_VAR).unwrap_or_else(|| DEFAULT_AGENT_LISTEN.to_string());
        let state_dir = value(STATE_VAR)
            .map(PathBuf::from)
            .filter(|dir| dir.is_absolute())
            .or_else(|| home_dir(&var).map(|home| home.join(DEVICE_STATE)));
        let pin = value(PIN_VAR).map(|raw| CertPin::parse(&raw).ok_or(ConfigProblem::BadPin));
        Self {
            secret,
            hook_addr,
            agent_addr,
            host: host_name(&value),
            state_dir,
            pin,
        }
    }

    /// How to reach the hub at `addr` (one of the two addresses): TLS with
    /// the pin, else plain TCP to a loopback address only.
    pub fn hub(&self, addr: &str) -> Result<HubAddr, ConfigProblem> {
        match &self.pin {
            Some(Ok(pin)) => HubAddr::pinned(addr, *pin).map_err(|_| ConfigProblem::BadAddr),
            Some(Err(problem)) => Err(*problem),
            None if tls::is_loopback_addr(addr) => Ok(HubAddr::plain(addr)),
            None if tls::host_of(addr).is_none() => Err(ConfigProblem::BadAddr),
            None => Err(ConfigProblem::PlainRemote),
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

pub(crate) fn home_dir(var: &impl Fn(&str) -> Option<String>) -> Option<PathBuf> {
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
        assert_eq!(config.agent_addr, "127.0.0.1:47291");
        assert_eq!(config.host, "laptop");

        let config = DeviceConfig::from_vars(vars(&[
            (HOOK_ADDR_VAR, "hub.tail:47292"),
            (AGENT_ADDR_VAR, "hub.tail:47291"),
        ]));
        assert_eq!(config.agent_addr, "hub.tail:47291");
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
    fn the_state_dir_is_absolute_or_under_home() {
        let home = if cfg!(windows) { r"C:\h" } else { "/h" };
        let absolute = if cfg!(windows) { r"D:\state" } else { "/state" };
        let home_var = if cfg!(windows) { "USERPROFILE" } else { "HOME" };
        let config = DeviceConfig::from_vars(vars(&[(home_var, home)]));
        assert_eq!(config.state_dir, Some(Path::new(home).join(".cctg")));
        let config = DeviceConfig::from_vars(vars(&[(home_var, home), (STATE_VAR, absolute)]));
        assert_eq!(config.state_dir, Some(PathBuf::from(absolute)));
        // A relative value would land in the session's folder: ignored.
        let config = DeviceConfig::from_vars(vars(&[(home_var, home), (STATE_VAR, ".cctg")]));
        assert_eq!(config.state_dir, Some(Path::new(home).join(".cctg")));
        let config = DeviceConfig::from_vars(vars(&[(STATE_VAR, ".cctg")]));
        assert_eq!(config.state_dir, None);
    }

    #[test]
    fn plain_only_to_loopback_and_tls_only_with_a_valid_pin() {
        let pin = "BA:78:16:BF:8F:01:CF:EA:41:41:40:DE:5D:AE:22:23:B0:03:61:A3:96:17:7A:9C:B4:10:FF:61:F2:00:15:AD";
        let plain = DeviceConfig::from_vars(vars(&[]));
        assert!(plain.pin.is_none());
        let local = plain.hub("127.0.0.1:47292").unwrap();
        assert!(!local.is_tls());
        assert!(!plain.hub("localhost:47291").unwrap().is_tls());
        for remote in ["hub.tail:47292", "100.64.0.7:47292", "203.0.113.9:47292"] {
            assert_eq!(
                plain.hub(remote).unwrap_err(),
                ConfigProblem::PlainRemote,
                "{remote}"
            );
        }
        assert_eq!(plain.hub("no-port").unwrap_err(), ConfigProblem::BadAddr);

        let pinned = DeviceConfig::from_vars(vars(&[(PIN_VAR, pin)]));
        for addr in ["hub.example.org:47292", "127.0.0.1:47292", "[::1]:1"] {
            assert!(pinned.hub(addr).unwrap().is_tls(), "{addr}");
        }
        assert_eq!(pinned.hub("no-port").unwrap_err(), ConfigProblem::BadAddr);

        let broken = DeviceConfig::from_vars(vars(&[(PIN_VAR, "not-a-pin")]));
        assert_eq!(
            broken.hub("127.0.0.1:47292").unwrap_err(),
            ConfigProblem::BadPin,
            "a broken pin never falls back to plain"
        );
        assert!(!ConfigProblem::BadPin.to_string().contains("not-a-pin"));
        assert!(
            !ConfigProblem::PlainRemote.to_string().contains("  "),
            "one line of plain text"
        );
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
