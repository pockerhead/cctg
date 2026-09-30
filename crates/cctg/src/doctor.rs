//! `cctg doctor` (TASK-031): checks this device's hub settings the way the
//! hook and the agent use them, and says what is wrong. `install.sh` runs it
//! after an install.
//!
//! It reads the same config ([`DeviceConfig::load`]), opens both hub links
//! (TLS with the pin when one is set) and sends the secret once to the
//! hook endpoint's `POST /v1/ping`, which only checks it. It never prints the
//! secret. Exit code 0 when everything a session needs works.
//!
//! Last, the sandbox (TASK-087): how many folders are marked and, when any
//! are, whether this device can start them ([`sandbox_report`]). It installs
//! nothing and runs no model.

use std::time::Duration;

use crate::client;
use crate::device::DeviceConfig;
use crate::hook::{self, PostError};
use crate::hub::devices::secret_device_id;
use crate::sandbox::{self, marks, preflight};

/// Per check; a far hub over TLS answers well within it.
pub const TIMEOUT: Duration = Duration::from_secs(5);

pub async fn run() -> i32 {
    let mut report = check(&DeviceConfig::load(), TIMEOUT).await;
    if let Ok((lines, ok)) =
        tokio::task::spawn_blocking(|| sandbox_report(&preflight::RealProbe)).await
    {
        report.lines.extend(lines);
        report.ok &= ok;
    }
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

/// The sandbox lines and whether marked folders can start. Paths go to the
/// local terminal only.
pub fn sandbox_report(probe: &dyn preflight::Probe) -> (Vec<String>, bool) {
    let not_used = || (vec!["sandbox: not used".to_owned()], true);
    let Some(home) = probe.home() else {
        return not_used();
    };
    let marked = match marks::load(&sandbox::marks_file(&home)) {
        Ok(marked) if marked.is_empty() => return not_used(),
        Ok(marked) => marked,
        Err(error) => {
            return (
                vec![format!(
                    "sandbox: {error}; folders named in it or in folders.json.bak stay \
                     sandboxed, folders it never named start as usual, and where that cannot \
                     be told (a cut-off file without its .bak) claude-cctg starts nothing; \
                     fix or remove it (docs/sandbox.md)"
                )],
                false,
            );
        }
    };
    let mut lines = vec![format!("sandbox: {} folder(s) marked", marked.len())];
    lines.extend(
        marked
            .iter()
            .map(|folder| format!("  {}", folder.display())),
    );
    let mut ok = true;
    if probe.os() == preflight::Os::Windows {
        ok &= windows_report(probe, &marked, &mut lines);
    }
    match preflight::device(probe).and_then(|ready| {
        preflight::wrapper(probe)?;
        Ok(ready)
    }) {
        Ok(ready) => {
            lines.push(format!(
                "sandbox: device ready (Claude Code {})",
                ready.claude
            ));
            (lines, ok)
        }
        Err(refusal) => {
            lines.push(format!("sandbox: marked folders will not start: {refusal}"));
            (lines, false)
        }
    }
}

/// Windows-only sandbox lines: install state, per-mark ACE, the Secondary
/// Logon service and a `git safe.directory = *` warning. Returns whether all
/// is well.
fn windows_report(
    probe: &dyn preflight::Probe,
    marked: &[std::path::PathBuf],
    lines: &mut Vec<String>,
) -> bool {
    let Some(installed) = probe.sandbox_install() else {
        lines.push("sandbox: not installed (run cctg sandbox-install)".to_owned());
        return false;
    };
    lines.push(format!("sandbox: installed, {} slots", installed.slots));
    if !installed.owner_is_me {
        lines.push("sandbox: installed by another Windows user".to_owned());
    }
    let live = windows_live_checks(probe, marked, installed.slots, lines);
    installed.owner_is_me && live
}

/// The checks that need the real machine: slot counts, the slot ACE of each
/// mark, the Secondary Logon service, `safe.directory = *`. Off Windows (a
/// `Fake` with `Os::Windows` in tests) there is nothing to look at.
#[cfg(windows)]
fn windows_live_checks(
    probe: &dyn preflight::Probe,
    marked: &[std::path::PathBuf],
    slots: u32,
    lines: &mut Vec<String>,
) -> bool {
    use crate::sandbox::win;
    let mut ok = true;
    if let Some(home) = probe.home() {
        let win_dir = win::win_dir(&home);
        if let Ok((active, retired, fresh)) = win::slots::counts(&win_dir, slots) {
            lines.push(format!(
                "sandbox: slots {active} active, {retired} retired, {fresh} fresh"
            ));
        }
        let mark = win::read_mark();
        for folder in marked {
            let folder_str = folder.to_string_lossy().to_string();
            let slot = win::slots::active_slot(&win_dir, &folder_str)
                .ok()
                .flatten();
            let has_ace = match (&mark, slot) {
                (Some(mark), Some(k)) => mark
                    .slot_sid(k)
                    .is_some_and(|sid| win::acl::has_ace(folder, sid)),
                _ => false,
            };
            if !has_ace {
                lines.push(format!(
                    "  {}: no slot ACE (run cctg sandbox on)",
                    folder.display()
                ));
                ok = false;
            }
        }
    }
    // The Secondary Logon service must not be disabled.
    if let Some(ran) = probe.run("sc", &["qc", "seclogon"], std::time::Duration::from_secs(5))
        && ran.stdout.contains("DISABLED")
    {
        lines.push("sandbox: the Secondary Logon service is disabled; enable it".to_owned());
        ok = false;
    }
    // git safe.directory = * would let the user's git run a slot-created config.
    if let Some(ran) = probe.run(
        "git",
        &["config", "--global", "--get-all", "safe.directory"],
        std::time::Duration::from_secs(3),
    ) && ran.stdout.lines().any(|l| l.trim() == "*")
    {
        lines.push(
            "sandbox: git trusts repositories of any owner (safe.directory = *); a .git \
             created by a sandbox command would run its config — remove it"
                .to_owned(),
        );
    }
    ok
}

#[cfg(not(windows))]
fn windows_live_checks(
    _probe: &dyn preflight::Probe,
    _marked: &[std::path::PathBuf],
    _slots: u32,
    _lines: &mut Vec<String>,
) -> bool {
    true
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

    #[test]
    fn the_sandbox_section_follows_the_marks_and_the_device() {
        let (_dir, home, folder) = preflight::tests::home_and_folder("doctor-sandbox");
        let mut fake = preflight::tests::Fake::linux(&home);
        assert_eq!(
            sandbox_report(&fake),
            (vec!["sandbox: not used".to_owned()], true)
        );
        marks::add(&sandbox::marks_file(&home), &folder).unwrap();
        let (lines, ok) = sandbox_report(&fake);
        assert!(ok, "{lines:?}");
        assert!(lines[0].contains("1 folder(s) marked"), "{lines:?}");
        assert!(lines.last().unwrap().contains("device ready"), "{lines:?}");
        fake.answer("claude", 0, "2.1.1 (Claude Code)");
        let (lines, ok) = sandbox_report(&fake);
        assert!(!ok);
        assert!(
            lines.last().unwrap().contains("will not start"),
            "{lines:?}"
        );
        // Windows, installed: the report names the install; a marked folder
        // with no slot ACE on the real machine is not ready (that is fine
        // here). Not installed with marks present is also not ready.
        let win = preflight::tests::Fake::windows(&home);
        let (lines, _ok) = sandbox_report(&win);
        assert!(
            lines.iter().any(|l| l.contains("sandbox: installed")),
            "{lines:?}"
        );
        let mut bare = preflight::tests::Fake::windows(&home);
        bare.installed = None;
        let (lines, ok) = sandbox_report(&bare);
        assert!(!ok);
        assert!(
            lines.iter().any(|l| l.contains("not installed")),
            "{lines:?}"
        );

        std::fs::write(sandbox::marks_file(&home), b"{broken").unwrap();
        let (lines, ok) = sandbox_report(&fake);
        assert!(!ok && lines[0].contains("starts nothing"), "{lines:?}");
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
