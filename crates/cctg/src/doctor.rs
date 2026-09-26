//! `cctg doctor` (TASK-031): checks this device's hub settings the way the
//! hook and the agent use them, and says what is wrong. `install.sh` runs it
//! after an install.
//!
//! It reads the same config ([`DeviceConfig::load`]), opens both hub links
//! (TLS with the pin when one is set) and sends the secret once to the
//! hook endpoint's `POST /v1/ping`, which only checks it. It never prints the
//! secret. Exit code 0 when everything a session needs works.

use std::time::Duration;

use crate::client;
use crate::device::DeviceConfig;
use crate::hook::{self, PostError};
use crate::hub::devices::secret_device_id;

/// Per check; a far hub over TLS answers well within it.
pub const TIMEOUT: Duration = Duration::from_secs(5);

pub async fn run() -> i32 {
    let report = check(&DeviceConfig::load(), TIMEOUT).await;
    for line in &report.lines {
        println!("{line}");
    }
    if report.ok { 0 } else { 1 }
}

#[derive(Debug)]
pub struct Report {
    pub lines: Vec<String>,
    pub ok: bool,
}

pub async fn check(config: &DeviceConfig, timeout: Duration) -> Report {
    let mut lines = vec![format!("cctg {}", client::LONG_VERSION)];
    let mut ok = true;
    let mut fail = |lines: &mut Vec<String>, line: String| {
        ok = false;
        lines.push(line);
    };
    let secret = match &config.secret {
        Ok(secret) => {
            lines.push(match secret_device_id(secret) {
                Some(id) => format!("secret: set, this device's own (device {id})"),
                None => "secret: set, the hub's shared secret".to_owned(),
            });
            Some(secret)
        }
        Err(problem) => {
            fail(&mut lines, format!("secret: {problem}"));
            None
        }
    };
    match &config.pin {
        None => lines.push("certificate pin: none (plain TCP, a hub on this machine only)".into()),
        Some(Ok(_)) => lines.push("certificate pin: set (TLS)".into()),
        Some(Err(problem)) => fail(&mut lines, format!("certificate pin: {problem}")),
    }

    let agent = &config.agent_addr;
    match config.hub(agent) {
        Err(problem) => fail(&mut lines, format!("agent link {agent}: {problem}")),
        Ok(addr) => match tokio::time::timeout(timeout, addr.connect()).await {
            Ok(Ok(_)) => lines.push(format!("agent link {agent}: reachable")),
            Ok(Err(error)) => fail(
                &mut lines,
                format!("agent link {agent}: cannot connect ({:?})", error.kind()),
            ),
            Err(_) => fail(
                &mut lines,
                format!("agent link {agent}: no answer in {timeout:?}"),
            ),
        },
    }

    let hooks = &config.hook_addr;
    match (config.hub(hooks), secret) {
        (Err(problem), _) => fail(&mut lines, format!("hooks {hooks}: {problem}")),
        (Ok(_), None) => fail(
            &mut lines,
            format!("hooks {hooks}: not checked without a secret"),
        ),
        (Ok(addr), Some(secret)) => match hook::ping(&addr, secret, timeout).await {
            Ok(()) => lines.push(format!("hooks {hooks}: reachable, the hub took the secret")),
            Err(PostError::Status(401)) => fail(
                &mut lines,
                format!(
                    "hooks {hooks}: the hub rejected the secret (compare CCTG_HUB_SECRET; a device \
                     secret may have been revoked: join again with a new code, cctg join)"
                ),
            ),
            // A hub from before `cctg doctor` has no ping route.
            Err(PostError::Status(404)) => lines.push(format!(
                "hooks {hooks}: reachable; this hub is older and cannot check the secret"
            )),
            Err(error) => fail(&mut lines, format!("hooks {hooks}: {error}")),
        },
    }
    Report { lines, ok }
}

#[cfg(test)]
mod tests {
    use tokio::net::TcpListener;
    use tokio::sync::mpsc;

    use super::*;
    use crate::device::{AGENT_ADDR_VAR, HOOK_ADDR_VAR};
    use crate::hub::config::SECRET_VAR;
    use crate::hub::ingress;
    use crate::wire::Secret;

    const SECRET: &str = "doctor-secret-0123456789";
    const SHORT: Duration = Duration::from_secs(2);

    fn config(pairs: Vec<(&'static str, String)>) -> DeviceConfig {
        DeviceConfig::from_vars(move |name| {
            pairs
                .iter()
                .find(|(key, _)| *key == name)
                .map(|(_, value)| value.clone())
        })
    }

    /// A real hook endpoint and a listener that only accepts, like the
    /// hub's agent listener before a handshake.
    async fn hub() -> (String, String, mpsc::Receiver<crate::wire::HookPost>) {
        let hooks = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let hook_addr = hooks.local_addr().unwrap().to_string();
        let (tx, rx) = mpsc::channel(8);
        tokio::spawn(ingress::serve_hooks(
            hooks,
            Secret::parse(SECRET).unwrap(),
            tx,
        ));
        let agents = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let agent_addr = agents.local_addr().unwrap().to_string();
        tokio::spawn(async move {
            let mut kept = Vec::new();
            while let Ok((stream, _)) = agents.accept().await {
                kept.push(stream);
            }
        });
        (agent_addr, hook_addr, rx)
    }

    #[tokio::test]
    async fn a_working_setup_passes_and_the_ping_is_no_event() {
        let (agent, hooks, mut events) = hub().await;
        let report = check(
            &config(vec![
                (SECRET_VAR, SECRET.into()),
                (AGENT_ADDR_VAR, agent),
                (HOOK_ADDR_VAR, hooks),
            ]),
            SHORT,
        )
        .await;
        assert!(report.ok, "{:?}", report.lines);
        assert!(
            report
                .lines
                .iter()
                .any(|line| line.contains("took the secret"))
        );
        assert!(events.try_recv().is_err(), "a ping is not a hook event");
        assert!(!report.lines.concat().contains(SECRET));
    }

    #[tokio::test]
    async fn a_wrong_secret_and_a_missing_one_fail_without_echo() {
        let (agent, hooks, _events) = hub().await;
        let wrong = "doctor-wrong-secret-000000";
        let report = check(
            &config(vec![
                (SECRET_VAR, wrong.into()),
                (AGENT_ADDR_VAR, agent.clone()),
                (HOOK_ADDR_VAR, hooks.clone()),
            ]),
            SHORT,
        )
        .await;
        assert!(!report.ok);
        assert!(
            report
                .lines
                .iter()
                .any(|line| line.contains("rejected the secret"))
        );
        assert!(!report.lines.concat().contains(wrong));
        let report = check(
            &config(vec![(AGENT_ADDR_VAR, agent), (HOOK_ADDR_VAR, hooks)]),
            SHORT,
        )
        .await;
        assert!(!report.ok);
        assert!(report.lines.iter().any(|line| line.starts_with("secret: ")));
    }

    #[tokio::test]
    async fn a_remote_hub_without_a_pin_is_never_contacted() {
        let report = check(
            &config(vec![
                (SECRET_VAR, SECRET.into()),
                (AGENT_ADDR_VAR, "203.0.113.9:47291".into()),
                (HOOK_ADDR_VAR, "203.0.113.9:47292".into()),
            ]),
            SHORT,
        )
        .await;
        assert!(!report.ok);
        assert_eq!(
            report
                .lines
                .iter()
                .filter(|line| line.contains("CCTG_HUB_CERT_SHA256"))
                .count(),
            2,
            "{:?}",
            report.lines
        );
    }

    #[tokio::test]
    async fn an_older_hub_without_the_ping_route_still_passes() {
        // A stand-in that answers 404 like a hub from before TASK-031.
        let hooks = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let hook_addr = hooks.local_addr().unwrap().to_string();
        tokio::spawn(async move {
            use tokio::io::{AsyncReadExt, AsyncWriteExt};
            while let Ok((mut stream, _)) = hooks.accept().await {
                let mut buf = [0u8; 1024];
                let _ = stream.read(&mut buf).await;
                let _ = stream
                    .write_all(b"HTTP/1.1 404 Not Found\r\nContent-Length: 0\r\n\r\n")
                    .await;
            }
        });
        let (agent, _, _events) = hub().await;
        let report = check(
            &config(vec![
                (SECRET_VAR, SECRET.into()),
                (AGENT_ADDR_VAR, agent),
                (HOOK_ADDR_VAR, hook_addr),
            ]),
            SHORT,
        )
        .await;
        assert!(report.ok, "{:?}", report.lines);
        assert!(report.lines.iter().any(|line| line.contains("older")));
    }
}
