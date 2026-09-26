//! Device enrollment end to end (TASK-045), with the real binary on the
//! device side: `cctg hub code` mints a code into a hub state directory,
//! a hub (its real listeners, TLS with a certificate made here, in this
//! process) takes it, `cctg join` trades it for the device's secret and
//! writes it into a temp `~/.cctg/device.env`, `cctg doctor` gets in with
//! it, a second use of the code is refused, and after a revoke the device
//! is out. Neither the code nor the secret reaches any output or the hub's
//! logs. Its own test binary: it installs a global log subscriber.

use std::io;
use std::net::{Ipv4Addr, SocketAddr};
use std::path::{Path, PathBuf};
use std::process::Output;
use std::sync::{Arc, Mutex};

use cctg::hub::devices::Devices;
use cctg::hub::ingress::{self, Listener};
use cctg::wire::Secret;
use rustls::pki_types::pem::PemObject;
use tokio::sync::mpsc;

mod common;

const SHARED: &str = "join-e2e-shared-secret-0123456789";

#[derive(Clone, Default)]
struct Captured(Arc<Mutex<Vec<u8>>>);

impl io::Write for Captured {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        if let Ok(mut out) = self.0.lock() {
            out.extend_from_slice(buf);
        }
        Ok(buf.len())
    }
    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

struct Dir(PathBuf);

impl Dir {
    fn new(name: &str) -> Self {
        let path =
            std::env::temp_dir().join(format!("cctg-join-e2e-{}-{name}", std::process::id()));
        let _ = std::fs::remove_dir_all(&path);
        std::fs::create_dir_all(&path).unwrap();
        Self(path)
    }
}

impl Drop for Dir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

fn run(home: &Path, cwd: &Path, args: &[&str], env: &[(&str, &str)]) -> (Output, String) {
    let mut command = common::cctg(home);
    command.args(args).current_dir(cwd);
    for (key, value) in env {
        command.env(key, value);
    }
    let output = command.output().expect("cctg runs");
    let text = format!(
        "{}{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    (output, text)
}

#[test]
fn a_device_joins_with_a_code_and_is_out_after_a_revoke() {
    let captured = Captured::default();
    let writer = captured.clone();
    let subscriber = tracing_subscriber::fmt()
        .without_time()
        .with_max_level(tracing::Level::TRACE)
        .with_writer(move || writer.clone())
        .finish();
    tracing::subscriber::set_global_default(subscriber).expect("first subscriber");

    let root = Dir::new("root");
    let (home, state) = (root.0.join("home"), root.0.join("state"));
    std::fs::create_dir_all(home.join(".cctg")).unwrap();
    let runtime = tokio::runtime::Runtime::new().unwrap();

    // The hub: TLS on both listeners, the shared secret still on.
    let rcgen::CertifiedKey { cert, signing_key } =
        rcgen::generate_simple_self_signed(vec!["localhost".to_owned()]).unwrap();
    let key =
        rustls::pki_types::PrivateKeyDer::from_pem_slice(signing_key.serialize_pem().as_bytes())
            .unwrap();
    let (acceptor, pin) = cctg::tls::Acceptor::new(vec![cert.der().clone()], key).unwrap();
    let devices = Devices::open(&state, Some(Secret::parse(SHARED).unwrap())).unwrap();
    let (agent_addr, hook_addr) = runtime.block_on(async {
        let loopback = SocketAddr::from((Ipv4Addr::LOCALHOST, 0));
        let agents = ingress::bind(loopback).await.unwrap();
        let hooks = ingress::bind(loopback).await.unwrap();
        let addrs = (agents.local_addr().unwrap(), hooks.local_addr().unwrap());
        let (agents_tx, mut agents_rx) = mpsc::channel(16);
        let (hooks_tx, mut hooks_rx) = mpsc::channel(16);
        tokio::spawn(ingress::serve_agents(
            Listener::tls(agents, acceptor.clone()),
            devices.clone(),
            agents_tx,
        ));
        tokio::spawn(ingress::serve_hooks(
            Listener::tls(hooks, acceptor),
            devices.clone(),
            hooks_tx,
        ));
        tokio::spawn(async move { while agents_rx.recv().await.is_some() {} });
        tokio::spawn(async move { while hooks_rx.recv().await.is_some() {} });
        addrs
    });

    // The device as install.sh leaves it before the join: addresses, pin,
    // the shared secret, and a line of the user's.
    let env_file = home.join(".cctg").join("device.env");
    std::fs::write(
        &env_file,
        format!(
            "# mine\nCCTG_HOST=laptop\nCCTG_HUB_SECRET='{SHARED}'\nCCTG_HUB_AGENT_ADDR={agent_addr}\nCCTG_HUB_HOOK_ADDR={hook_addr}\nCCTG_HUB_CERT_SHA256={pin}\n"
        ),
    )
    .unwrap();

    // `cctg hub code`: only the code on stdout.
    let state_text = state.to_string_lossy().into_owned();
    let (output, _) = run(
        &home,
        &root.0,
        &["hub", "code"],
        &[("CCTG_STATE_DIR", &state_text)],
    );
    assert!(output.status.success(), "{output:?}");
    let code = String::from_utf8(output.stdout).unwrap().trim().to_owned();
    assert_eq!(code.len(), 19, "{code}");

    let (output, text) = run(&home, &root.0, &["join"], &[("CCTG_JOIN_CODE", &code)]);
    assert!(output.status.success(), "{text}");
    assert!(text.contains("this device is laptop ("), "{text}");
    let written = std::fs::read_to_string(&env_file).unwrap();
    let secret = written
        .lines()
        .find_map(|line| line.strip_prefix("CCTG_HUB_SECRET='"))
        .and_then(|value| value.strip_suffix('\''))
        .expect("the new secret line")
        .to_owned();
    assert!(secret.starts_with("cctgd_"), "{written}");
    assert!(
        !written.contains(SHARED),
        "the shared secret is gone: {written}"
    );
    assert!(
        written.starts_with("# mine\nCCTG_HOST=laptop\n"),
        "{written}"
    );
    assert!(!text.contains(&secret) && !text.contains(&code), "{text}");

    let (output, text) = run(&home, &root.0, &["doctor"], &[]);
    assert!(output.status.success(), "{text}");
    assert!(text.contains("took the secret"), "{text}");

    // The code is spent: a clear refusal, no hint which way.
    let (output, text) = run(&home, &root.0, &["join", &code.to_lowercase()], &[]);
    assert_eq!(output.status.code(), Some(1), "{text}");
    assert!(text.contains("wrong, already used or expired"), "{text}");
    assert_eq!(std::fs::read_to_string(&env_file).unwrap(), written);
    let (output, text) = run(&home, &root.0, &["join"], &[]);
    assert_eq!(output.status.code(), Some(2), "{text}");

    // Revoked: the device is out.
    let (listed, _) = devices.list();
    assert_eq!(listed.len(), 1);
    assert!(devices.revoke(&listed[0].id).is_some());
    let (output, text) = run(&home, &root.0, &["doctor"], &[]);
    assert!(!output.status.success(), "{text}");
    assert!(text.contains("rejected the secret"), "{text}");

    let logs = String::from_utf8(captured.0.lock().unwrap().clone()).unwrap();
    assert!(logs.contains("device enrolled with a join code"), "{logs}");
    assert!(logs.contains("join code refused"), "{logs}");
    let bare = code.replace('-', "");
    for secret_text in [
        secret.as_str(),
        &secret[15..],
        code.as_str(),
        bare.as_str(),
        SHARED,
    ] {
        assert!(
            !logs.contains(secret_text),
            "{secret_text} in the logs:\n{logs}"
        );
    }
    drop(runtime);
}
