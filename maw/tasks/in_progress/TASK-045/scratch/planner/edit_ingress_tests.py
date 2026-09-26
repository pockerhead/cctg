import sys
p = sys.argv[1] + '/crates/cctg/src/hub/ingress.rs'
s = open(p, encoding='utf-8').read()
old = """    async fn exchange_raw(addr: SocketAddr, raw: &[u8]) -> Vec<u8> {
        let mut stream = TcpStream::connect(addr).await.unwrap();
        let _ = stream.write_all(raw).await;
        let mut response = Vec::new();
        let _ = within(stream.read_to_end(&mut response)).await;
        response
    }
}"""
new = """    async fn exchange_raw(addr: SocketAddr, raw: &[u8]) -> Vec<u8> {
        let mut stream = TcpStream::connect(addr).await.unwrap();
        let _ = stream.write_all(raw).await;
        let mut response = Vec::new();
        let _ = within(stream.read_to_end(&mut response)).await;
        response
    }

    // ------------------------------------------------ devices (TASK-045)

    use crate::hub::devices::{Devices, mint_code};
    use crate::hub::testdir::TempDir;

    fn join_request(body: &[u8], auth: Option<&str>) -> Vec<u8> {
        let mut head = format!(
            "POST {JOIN_PATH} HTTP/1.1\\r\\nHost: x\\r\\nContent-Length: {}\\r\\n",
            body.len()
        );
        if let Some(auth) = auth {
            head.push_str(&format!("Authorization: {auth}\\r\\n"));
        }
        head.push_str("\\r\\n");
        [head.as_bytes(), body].concat()
    }

    fn join_body(code: &str) -> Vec<u8> {
        serde_json::to_vec(&wire::JoinPost {
            v: wire::VERSION,
            code: code.into(),
            name: "laptop".into(),
        })
        .unwrap()
    }

    /// The status and body of one raw exchange.
    async fn answer(addr: SocketAddr, raw: &[u8]) -> (u16, Vec<u8>) {
        let response = exchange_raw(addr, raw).await;
        let head_end = response
            .windows(4)
            .position(|window| window == b"\\r\\n\\r\\n")
            .expect("a complete answer");
        let status = std::str::from_utf8(&response[9..12]).unwrap().parse().unwrap();
        (status, response[head_end + 4..].to_vec())
    }

    /// Both listeners over one device book of `dir`.
    async fn device_hub(
        devices: &Devices,
    ) -> (SocketAddr, SocketAddr, mpsc::Receiver<AgentEvent>, mpsc::Receiver<HookPost>) {
        let agents = bind(loopback()).await.unwrap();
        let hooks = bind(loopback()).await.unwrap();
        let (agent_addr, hook_addr) = (agents.local_addr().unwrap(), hooks.local_addr().unwrap());
        let (agents_tx, agents_rx) = mpsc::channel(16);
        let (hooks_tx, hooks_rx) = mpsc::channel(16);
        tokio::spawn(serve_agents(agents, devices.clone(), agents_tx));
        tokio::spawn(serve_hooks(hooks, devices.clone(), hooks_tx));
        (agent_addr, hook_addr, agents_rx, hooks_rx)
    }

    #[tokio::test]
    async fn a_join_code_buys_one_device_secret_and_nothing_else() {
        let dir = TempDir::new("ingress-join");
        let devices = Devices::open(dir.path(), None).unwrap();
        let (_, addr, _agents, mut hooks) = device_hub(&devices).await;
        let code = mint_code(dir.path(), std::time::SystemTime::now()).unwrap();

        // Any Authorization header is ignored: the code is the credential.
        let (status, body) = answer(addr, &join_request(&join_body(&code), Some("Bearer nothing-at-all-000"))).await;
        assert_eq!(status, 200, "{}", String::from_utf8_lossy(&body));
        let joined: JoinAnswer = serde_json::from_slice(&body).unwrap();
        assert_eq!(joined.name, "laptop");
        let bearer = format!("Bearer {}", joined.secret.expose());
        assert_eq!(exchange(addr, &request(Some(&bearer), &body_of_start())).await, 204);
        assert!(within(hooks.recv()).await.is_some());

        // The same code again, a made-up one and an expired one: the same
        // 403, after the pause of a wrong secret.
        let old = mint_code(
            dir.path(),
            std::time::SystemTime::now() - crate::hub::devices::CODE_TTL - Duration::from_secs(1),
        )
        .unwrap();
        for code in [code.as_str(), "ABCD-EFGH-JKMN-PQRS", old.as_str()] {
            let started = Instant::now();
            let (status, body) = answer(addr, &join_request(&join_body(code), None)).await;
            assert_eq!((status, body.len()), (403, 0), "{code}");
            assert!(started.elapsed() >= AUTH_FAIL_DELAY);
        }
        // A join body is short and must parse; the hook secret is no code.
        let long = join_body(&"A".repeat(MAX_JOIN_BODY));
        assert_eq!(answer(addr, &join_request(&long, None)).await.0, 413);
        assert_eq!(answer(addr, &join_request(b"{}", None)).await.0, 400);
        assert_eq!(devices.list().0.len(), 1);
    }

    fn body_of_start() -> Vec<u8> {
        body(&post(start()))
    }

    #[tokio::test]
    async fn a_revoked_device_loses_its_link_and_its_hooks_at_once() {
        let dir = TempDir::new("ingress-revoke");
        let shared = Secret::parse(SECRET).unwrap();
        let devices = Devices::open(dir.path(), Some(shared)).unwrap();
        let (agent_addr, hook_addr, mut agents, _hooks) = device_hub(&devices).await;
        let code = mint_code(dir.path(), std::time::SystemTime::now()).unwrap();
        let joined = devices.join(&code, "old box").unwrap();

        let mut device = Peer::connect(agent_addr).await;
        device.send(&AgentMsg::Hello { secret: joined.secret.clone() }).await;
        device.send(&AgentMsg::Register(register())).await;
        assert!(matches!(device.recv().await, Ok(HubMsg::Registered { .. })));
        let Some(AgentEvent::Registered { conn, .. }) = within(agents.recv()).await else {
            panic!("expected registration");
        };
        let mut other = Peer::connect(agent_addr).await;
        other.send(&AgentMsg::Hello { secret: secret() }).await;
        other.send(&AgentMsg::Register(register())).await;
        assert!(matches!(other.recv().await, Ok(HubMsg::Registered { .. })));
        let Some(AgentEvent::Registered { conn: shared_conn, .. }) = within(agents.recv()).await else {
            panic!("expected registration");
        };

        assert!(devices.revoke(&joined.id).is_some());
        assert!(matches!(
            within(agents.recv()).await,
            Some(AgentEvent::Disconnected { conn: gone }) if gone == conn
        ));
        assert_eq!(device.recv().await, Err(WireError::Closed));
        let bearer = format!("Bearer {}", joined.secret.expose());
        assert_eq!(exchange(hook_addr, &request(Some(&bearer), &body_of_start())).await, 401);
        let mut again = Peer::connect(agent_addr).await;
        again.send(&AgentMsg::Hello { secret: joined.secret.clone() }).await;
        again.send(&AgentMsg::Register(register())).await;
        assert_eq!(again.recv().await, Ok(HubMsg::Rejected { reason: Rejection::Auth }));

        // The shared secret's link stays.
        tokio::time::sleep(Duration::from_millis(100)).await;
        assert!(agents.try_recv().is_err(), "shared link {shared_conn} untouched");
        assert_eq!(
            exchange(hook_addr, &request(Some(&format!("Bearer {SECRET}")), &body_of_start())).await,
            204
        );
    }

    #[tokio::test]
    async fn with_the_shared_secret_off_only_devices_get_in() {
        let dir = TempDir::new("ingress-shared-off");
        let devices = Devices::open(dir.path(), None).unwrap();
        let (agent_addr, hook_addr, _agents, _hooks) = device_hub(&devices).await;
        assert_eq!(
            exchange(hook_addr, &request(Some(&format!("Bearer {SECRET}")), &body_of_start())).await,
            401
        );
        let mut peer = Peer::connect(agent_addr).await;
        peer.send(&AgentMsg::Hello { secret: secret() }).await;
        peer.send(&AgentMsg::Register(register())).await;
        assert_eq!(peer.recv().await, Ok(HubMsg::Rejected { reason: Rejection::Auth }));
    }

    #[tokio::test]
    async fn a_waiting_hook_of_a_revoked_device_is_dropped_unanswered() {
        let dir = TempDir::new("ingress-revoke-wait");
        let devices = Devices::open(dir.path(), None).unwrap();
        let code = mint_code(dir.path(), std::time::SystemTime::now()).unwrap();
        let joined = devices.join(&code, "box").unwrap();
        let listener = bind(loopback()).await.unwrap();
        let addr = listener.local_addr().unwrap();
        let (events, _events) = mpsc::channel(4);
        let (asks_tx, mut asks) = mpsc::channel(4);
        tokio::spawn(serve_hooks_and_permissions(listener, devices.clone(), events, asks_tx));
        let permission = serde_json::to_vec(&PermissionPost {
            v: wire::VERSION,
            host: "box".into(),
            session_id: "5e551017-0000-4000-8000-000000000001".into(),
            tool_name: "Bash".into(),
            description: String::new(),
            input_preview: String::new(),
        })
        .unwrap();
        let raw = [
            format!(
                "POST {PERMISSION_PATH} HTTP/1.1\\r\\nAuthorization: Bearer {}\\r\\nContent-Length: {}\\r\\n\\r\\n",
                joined.secret.expose(),
                permission.len()
            )
            .into_bytes(),
            permission,
        ]
        .concat();
        let waiting = tokio::spawn(async move { exchange_raw(addr, &raw).await });
        let ask = within(asks.recv()).await.expect("the hook waits for an answer");
        devices.revoke(&joined.id).unwrap();
        assert!(within(waiting).await.unwrap().is_empty(), "no answer at all");
        drop(ask);
    }
}"""
assert s.count(old) == 1
s = s.replace(old, new)
open(p, 'w', encoding='utf-8', newline='\n').write(s)
print('ok')
