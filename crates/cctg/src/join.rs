//! `cctg join` (TASK-045): this device trades a one-time join code for its
//! own secret.
//!
//! The hub address and certificate pin come from the device config, as for
//! the hook ([`DeviceConfig`]): TLS with the pin, or plain TCP to a hub on
//! this machine only. The request (`POST /v1/join`) carries the code and
//! this device's host name, no secret. The answer's secret replaces the
//! `CCTG_HUB_SECRET` line of `~/.cctg/device.env` (file mode 600 on Unix);
//! it is never printed, and neither is the code.

use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::time::Duration;

use tokio::io::{AsyncReadExt, AsyncWriteExt};

use crate::device::{self, ConfigProblem, DEVICE_ENV, DeviceConfig};
use crate::hub::config::SECRET_VAR;
use crate::hub::devices::secret_device_id;
use crate::tls::HubAddr;
use crate::wire::{JOIN_PATH, JoinAnswer, JoinPost, Secret, VERSION};

/// The code when it is not an argument (install.sh passes it so).
pub const CODE_VAR: &str = "CCTG_JOIN_CODE";
/// The whole exchange, TLS handshake included.
pub const TIMEOUT: Duration = Duration::from_secs(10);
/// Longest answer read.
const MAX_ANSWER: u64 = 4096;

#[derive(Debug, PartialEq, Eq, thiserror::Error)]
pub enum JoinFailed {
    #[error("{0}")]
    Config(ConfigProblem),
    #[error(
        "the hub refused the join code: it is wrong, already used or expired. \
         Ask for a new one (/devices in the Telegram group says how)"
    )]
    Refused,
    #[error(
        "this hub is older than join codes: update it, or give this device the \
         shared secret (CCTG_HUB_SECRET)"
    )]
    OldHub,
    #[error(
        "the hub cannot enroll a device now (its device list is full, or a disk problem; see its log)"
    )]
    Unavailable,
    #[error("the hub answered HTTP {0}")]
    Status(u16),
    #[error("the hub did not answer within {0:?}")]
    Timeout(Duration),
    #[error("cannot reach the hub: {0:?}")]
    Io(io::ErrorKind),
    #[error("the hub's answer is not a join answer")]
    BadAnswer,
}

pub async fn run(code: Option<String>) -> i32 {
    let code = code.or_else(|| std::env::var(CODE_VAR).ok());
    let Some(code) = code
        .map(|code| code.trim().to_owned())
        .filter(|code| !code.is_empty())
    else {
        eprintln!("cctg join: no join code: give it as the argument or in {CODE_VAR}");
        return 2;
    };
    let Some(env_file) =
        device::home_dir(&|name| std::env::var(name).ok()).map(|home| home.join(DEVICE_ENV))
    else {
        eprintln!("cctg join: no home directory (HOME / USERPROFILE) for {DEVICE_ENV}");
        return 1;
    };
    let config = DeviceConfig::load();
    let answer = match join(&config, &code, TIMEOUT).await {
        Ok(answer) => answer,
        Err(error) => {
            eprintln!("cctg join: {error}");
            return 1;
        }
    };
    if let Err(error) = write_secret(&env_file, &answer.secret) {
        eprintln!(
            "cctg join: the hub enrolled this device as {}, but {} cannot be written ({:?}): revoke {} in /devices and join again with a new code",
            answer.device_id,
            env_file.display(),
            error.kind(),
            answer.device_id
        );
        return 1;
    }
    println!(
        "cctg join: this device is {} ({}) on the hub; its secret is in {}",
        answer.name,
        answer.device_id,
        env_file.display()
    );
    if let Some(old) = config.secret.as_ref().ok().and_then(secret_device_id) {
        println!(
            "cctg join: the secret before it (device {old}) still gets in until it is revoked in /devices"
        );
    }
    if std::env::var(SECRET_VAR).is_ok_and(|value| !value.trim().is_empty()) {
        println!(
            "cctg join: note: {SECRET_VAR} of this environment wins over the file; unset it for the new secret to count"
        );
    }
    println!(
        "cctg join: running claude sessions keep their old secret until they restart; hooks take the new one at once"
    );
    0
}

/// Trades `code` for a device secret at the hub's hook address.
pub async fn join(
    config: &DeviceConfig,
    code: &str,
    timeout: Duration,
) -> Result<JoinAnswer, JoinFailed> {
    let addr = config.hub(&config.hook_addr).map_err(JoinFailed::Config)?;
    let post = JoinPost {
        v: VERSION,
        code: code.to_owned(),
        name: config.host.clone(),
    };
    let body = serde_json::to_vec(&post).expect("a join post always serializes");
    let answer = tokio::time::timeout(timeout, exchange(&addr, &body))
        .await
        .map_err(|_| JoinFailed::Timeout(timeout))??;
    let (status, body) = split_answer(&answer).ok_or(JoinFailed::BadAnswer)?;
    match status {
        200 => {
            let answer: JoinAnswer =
                serde_json::from_slice(body).map_err(|_| JoinFailed::BadAnswer)?;
            // The hub's secret must be one the hook and agent can send.
            Secret::parse(answer.secret.expose()).map_err(|_| JoinFailed::BadAnswer)?;
            Ok(answer)
        }
        403 => Err(JoinFailed::Refused),
        404 => Err(JoinFailed::OldHub),
        503 => Err(JoinFailed::Unavailable),
        code => Err(JoinFailed::Status(code)),
    }
}

async fn exchange(addr: &HubAddr, body: &[u8]) -> Result<Vec<u8>, JoinFailed> {
    let io = |error: io::Error| JoinFailed::Io(error.kind());
    let head = format!(
        "POST {JOIN_PATH} HTTP/1.1\r\nHost: cctg-hub\r\nContent-Type: application/json\r\n\
         Content-Length: {}\r\nConnection: close\r\n\r\n",
        body.len()
    );
    let mut stream = addr.connect().await.map_err(io)?;
    stream
        .write_all(&[head.as_bytes(), body].concat())
        .await
        .map_err(io)?;
    stream.flush().await.map_err(io)?;
    let mut answer = Vec::new();
    (&mut stream)
        .take(MAX_ANSWER)
        .read_to_end(&mut answer)
        .await
        .map_err(io)?;
    Ok(answer)
}

/// The status of a complete `HTTP/1.1 NNN ...` answer and its body.
fn split_answer(answer: &[u8]) -> Option<(u16, &[u8])> {
    let head_end = answer.windows(4).position(|window| window == b"\r\n\r\n")?;
    let line_end = answer.windows(2).position(|pair| pair == b"\r\n")?;
    let rest = answer[..line_end].strip_prefix(b"HTTP/1.1 ")?;
    let (code, tail) = rest.split_at_checked(3)?;
    if !code.iter().all(u8::is_ascii_digit) || !(tail.is_empty() || tail[0] == b' ') {
        return None;
    }
    let status = std::str::from_utf8(code).ok()?.parse().ok()?;
    Some((status, &answer[head_end + 4..]))
}

/// Puts `CCTG_HUB_SECRET='<secret>'` into `path` in place of every line
/// that sets it; every other line stays. Temp file and rename, so a
/// failure leaves the file as it was.
pub fn write_secret(path: &Path, secret: &Secret) -> io::Result<()> {
    let old = match std::fs::read_to_string(path) {
        Ok(text) => text,
        Err(error) if error.kind() == io::ErrorKind::NotFound => String::new(),
        Err(error) => return Err(error),
    };
    let mut text: String = old
        .lines()
        .filter(|line| !sets_secret(line))
        .flat_map(|line| [line, "\n"])
        .collect();
    // A device secret is letters, digits and `_`: nothing to escape.
    text.push_str(&format!("{SECRET_VAR}='{}'\n", secret.expose()));
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
    }
    let temp = temp_path(path);
    let mut options = std::fs::OpenOptions::new();
    options.write(true).create(true).truncate(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    let mut file = options.open(&temp)?;
    file.write_all(text.as_bytes())?;
    file.sync_all()?;
    drop(file);
    std::fs::rename(&temp, path)
}

fn temp_path(path: &Path) -> PathBuf {
    let mut name = path.file_name().unwrap_or_default().to_os_string();
    name.push(".join-tmp");
    path.with_file_name(name)
}

/// `CCTG_HUB_SECRET=...`, also with `export` and spaces, as dotenv reads it.
fn sets_secret(line: &str) -> bool {
    let line = line.trim_start();
    let line = line
        .strip_prefix("export")
        .filter(|rest| rest.starts_with([' ', '\t']))
        .map_or(line, str::trim_start);
    line.strip_prefix(SECRET_VAR)
        .is_some_and(|rest| rest.trim_start().starts_with('='))
}

#[cfg(test)]
mod tests {
    use std::time::SystemTime;

    use tokio::net::TcpListener;
    use tokio::sync::mpsc;

    use super::*;
    use crate::device::{AGENT_ADDR_VAR, HOOK_ADDR_VAR, HOST_VAR};
    use crate::hub::devices::{Devices, mint_code};
    use crate::hub::ingress;
    use crate::hub::testdir::TempDir;

    fn config(hook_addr: &str) -> DeviceConfig {
        let hook_addr = hook_addr.to_owned();
        DeviceConfig::from_vars(move |name| match name {
            HOOK_ADDR_VAR => Some(hook_addr.clone()),
            AGENT_ADDR_VAR => Some("127.0.0.1:1".into()),
            HOST_VAR => Some("laptop".into()),
            _ => None,
        })
    }

    async fn hub(devices: Devices) -> String {
        let hooks = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = hooks.local_addr().unwrap().to_string();
        let (tx, mut rx) = mpsc::channel(8);
        tokio::spawn(ingress::serve_hooks(hooks, devices, tx));
        tokio::spawn(async move { while rx.recv().await.is_some() {} });
        addr
    }

    #[tokio::test]
    async fn a_code_is_traded_once_for_a_secret_the_hub_takes() {
        let dir = TempDir::new("join-client");
        let devices = Devices::open(dir.path(), None).unwrap();
        let addr = hub(devices.clone()).await;
        let code = mint_code(dir.path(), SystemTime::now()).unwrap();
        let answer = join(&config(&addr), &code, TIMEOUT).await.unwrap();
        assert_eq!(answer.name, "laptop");
        assert!(devices.check(answer.secret.expose().as_bytes()).is_some());
        assert_eq!(
            join(&config(&addr), &code, TIMEOUT).await.unwrap_err(),
            JoinFailed::Refused
        );
        assert_eq!(
            join(&config(&addr), "not a code", TIMEOUT)
                .await
                .unwrap_err(),
            JoinFailed::Refused
        );
    }

    #[tokio::test]
    async fn a_remote_hub_without_a_pin_is_never_asked() {
        assert_eq!(
            join(&config("203.0.113.9:47292"), "ABCD-EFGH-JKMN-PQRS", TIMEOUT)
                .await
                .unwrap_err(),
            JoinFailed::Config(ConfigProblem::PlainRemote)
        );
    }

    #[test]
    fn the_secret_line_is_replaced_and_the_rest_kept() {
        let dir = TempDir::new("join-env");
        let path = dir.path().join(".cctg").join("device.env");
        let secret = Secret::parse(&format!("cctgd_0123abcd_{}", "a".repeat(64))).unwrap();
        write_secret(&path, &secret).unwrap();
        assert_eq!(
            std::fs::read_to_string(&path).unwrap(),
            format!("CCTG_HUB_SECRET='{}'\n", secret.expose())
        );
        std::fs::write(
            &path,
            "# mine\nCCTG_HUB_SECRET=old-shared-secret-000\n  export CCTG_HUB_SECRET = 'x'\nCCTG_HUB_SECRET_OTHER=1\nCCTG_HUB_HOOK_ADDR=h:1",
        )
        .unwrap();
        write_secret(&path, &secret).unwrap();
        assert_eq!(
            std::fs::read_to_string(&path).unwrap(),
            format!(
                "# mine\nCCTG_HUB_SECRET_OTHER=1\nCCTG_HUB_HOOK_ADDR=h:1\nCCTG_HUB_SECRET='{}'\n",
                secret.expose()
            )
        );
        // dotenv (the hook's reader) takes the value as written.
        let vars: std::collections::HashMap<String, String> = dotenvy::from_path_iter(&path)
            .unwrap()
            .collect::<Result<_, _>>()
            .unwrap();
        assert_eq!(vars[SECRET_VAR], secret.expose());
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = std::fs::metadata(&path).unwrap().permissions().mode();
            assert_eq!(mode & 0o777, 0o600);
        }
    }

    #[test]
    fn answers_are_read_strictly() {
        assert_eq!(
            split_answer(b"HTTP/1.1 200 OK\r\nContent-Length: 2\r\n\r\n{}"),
            Some((200, &b"{}"[..]))
        );
        assert_eq!(
            split_answer(b"HTTP/1.1 403 Forbidden\r\n\r\n"),
            Some((403, &b""[..]))
        );
        assert_eq!(split_answer(b"HTTP/1.0 200 OK\r\n\r\n"), None);
        assert_eq!(split_answer(b"HTTP/1.1 200 OK\r\n"), None);
    }
}
