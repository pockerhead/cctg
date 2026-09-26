import sys
root = sys.argv[1]


def patch(rel, pairs):
    p = root + '/' + rel
    s = open(p, encoding='utf-8').read()
    for old, new, *count in pairs:
        n = s.count(old)
        assert n == (count[0] if count else 1), (rel, old[:80], n)
        s = s.replace(old, new)
    open(p, 'w', encoding='utf-8', newline='\n').write(s)


patch('crates/cctg/tests/install_e2e.rs', [
("""    let cases: [(&[&str], Option<&str>, &str); 5] = [
        (&local, None, "no hub secret"),""", """    let bad_code = [&local[..], &["--join", "ABCD-EFGH;rm -rf"]].concat();
    let cases: [(&[&str], Option<&str>, &str); 6] = [
        (&local, None, "no hub secret"),
        (&bad_code, None, "a join code has letters"),"""),
("""        "#!/bin/sh\\nprintf '%s\\\\n' \\"$*\\" >> \\"$DOCKER_LOG\\"\\ncase \\"$*\\" in *' logs '*) echo 'hub-1  | INFO hub started, polling bot=x';; esac\\nexit 0\\n",
    );
    let docker_log = root.0.join("docker.log");
    let hub_dir = run.home.join("hub");""", """        "#!/bin/sh\\nprintf '%s\\\\n' \\"$*\\" >> \\"$DOCKER_LOG\\"\\ncase \\"$*\\" in *' logs '*) echo 'hub-1  | INFO hub started, polling bot=x';; *' exec -T hub cctg hub code'*) echo 'ABCD-EFGH-JKMN-PQRS';; esac\\nexit 0\\n",
    );
    let docker_log = root.0.join("docker.log");
    let hub_dir = run.home.join("hub");"""),
("""    assert_eq!(
        line,
        format!(
            "curl -fsSL https://raw.githubusercontent.com/pockerhead/cctg/{}/install.sh \\
             | CCTG_HUB_SECRET='{secret}' sh -s -- --hub-host hub.example.org --pin {pin}",
            release_tag()
        )
    );
    assert_eq!(
        text.matches(&secret).count(),
        1,
        "the secret only in the line"
    );""", """    // A one-time join code of the hub, never the shared secret (TASK-045).
    assert_eq!(
        line,
        format!(
            "curl -fsSL https://raw.githubusercontent.com/pockerhead/cctg/{}/install.sh \\
             | sh -s -- --hub-host hub.example.org --pin {pin} --join ABCD-EFGH-JKMN-PQRS",
            release_tag()
        )
    );
    assert!(!text.contains(&secret), "the shared secret was printed: {text}");"""),
("""    assert!(
        calls.contains("compose pull hub") && calls.contains("compose up -d --force-recreate hub"),
        "{calls}"
    );""", """    assert!(
        calls.contains("compose pull hub")
            && calls.contains("compose up -d --force-recreate hub")
            && calls.contains("compose exec -T hub cctg hub code"),
        "{calls}"
    );"""),
("""#[test]
fn a_bad_checksum_installs_nothing() {""", """/// A device trades a join code for its own secret (TASK-045): `--join`
/// writes it into device.env through `cctg join`, prints neither, and a
/// spent code changes nothing.
#[test]
fn a_device_joins_with_a_code() {
    let root = Root::new("join");
    let dist = root.dir("dist");
    release(&dist, &this_build());
    let (base, _) = serve(dist);
    let state = root.dir("hub-state");
    let runtime = tokio::runtime::Runtime::new().unwrap();
    let devices = cctg::hub::devices::Devices::open(&state, None).unwrap();
    let (agent_addr, hook_addr) = runtime.block_on(async {
        let hooks = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let hook_addr = hooks.local_addr().unwrap().to_string();
        let (tx, mut rx) = tokio::sync::mpsc::channel(8);
        tokio::spawn(cctg::hub::ingress::serve_hooks(hooks, devices.clone(), tx));
        tokio::spawn(async move { while rx.recv().await.is_some() {} });
        let agents = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let agent_addr = agents.local_addr().unwrap().to_string();
        tokio::spawn(async move {
            let mut kept = Vec::new();
            while let Ok((stream, _)) = agents.accept().await {
                kept.push(stream);
            }
        });
        (agent_addr, hook_addr)
    });
    let code = cctg::hub::devices::mint_code(&state, std::time::SystemTime::now()).unwrap();
    let mut run = Run::new(&root);
    run.env("CCTG_INSTALL_BASE_URL", &base);
    run.fake_program("claude", FAKE_CLAUDE);
    let args = [
        "--yes",
        "--agent-addr",
        &agent_addr,
        "--hook-addr",
        &hook_addr,
        "--host",
        "joined-box",
    ];
    let (output, text) = run.install(&[&args[..], &["--join", &code]].concat());
    assert!(output.status.success(), "{text}");
    assert!(text.contains("this device is joined-box ("), "{text}");
    assert!(text.contains("took the secret"), "cctg doctor: {text}");
    let env_file = run.cctg_dir().join("device.env");
    let env = std::fs::read_to_string(&env_file).unwrap();
    let secret = env
        .lines()
        .find_map(|line| line.strip_prefix("CCTG_HUB_SECRET='"))
        .and_then(|value| value.strip_suffix('\\''))
        .expect("the device's secret");
    assert!(secret.starts_with("cctgd_"), "{env}");
    assert!(!text.contains(secret) && !text.contains(&code), "{text}");
    assert_eq!(devices.list().0[0].name, "joined-box");

    // Again without a code: the device keeps its secret.
    let (output, text) = run.install(&args);
    assert!(output.status.success(), "{text}");
    assert_eq!(std::fs::read_to_string(&env_file).unwrap(), env);
    // The spent code: refused, device.env as it was.
    run.env("CCTG_JOIN_CODE", &code);
    let (output, text) = run.install(&args);
    assert!(!output.status.success(), "{text}");
    assert!(text.contains("wrong, already used or expired"), "{text}");
    assert_eq!(std::fs::read_to_string(&env_file).unwrap(), env);
    // Uninstall takes the secret line out like the other lines it wrote.
    run.env.retain(|(key, _)| key != "CCTG_JOIN_CODE");
    let (output, text) = run.install(&["--uninstall"]);
    assert!(output.status.success(), "{text}");
    assert!(!env_file.exists(), "{text}");
    drop(runtime);
}

#[test]
fn a_bad_checksum_installs_nothing() {"""),
])
print('ok')
