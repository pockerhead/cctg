import sys; sys.path.insert(0, 'maw/tasks/in_progress/TASK-049/scratch')
from sub import Sub
f = Sub('crates/cctg/src/agent.rs'); rep = f.rep
rep("""    fn device(secret: Option<&str>) -> DeviceConfig {
        DeviceConfig::from_vars(|name| {""","""    /// Short heartbeat for the TASK-049 tests: pings every 100 ms, a link
    /// silent for 600 ms is dead.
    const BEAT: Heartbeat = Heartbeat {
        interval: Duration::from_millis(100),
        timeout: Duration::from_millis(600),
    };

    fn beating(addr: SocketAddr) -> LinkConfig {
        let backoff = Backoff {
            initial: Duration::from_millis(20),
            max: Duration::from_millis(40),
        };
        let mut link = config(addr, backoff);
        link.register.heartbeat = true;
        link.heartbeat = BEAT;
        link
    }

    /// A hub stand-in: takes `hello` and `register`, answers `registered`
    /// with `heartbeat` as given, and returns the open connection.
    async fn fake_hub(
        listener: &TcpListener,
        heartbeat: bool,
    ) -> (BufReader<OwnedReadHalf>, OwnedWriteHalf) {
        let (stream, _) = tokio::time::timeout(WAIT, listener.accept())
            .await
            .expect("agent connected")
            .unwrap();
        let (read, mut write) = stream.into_split();
        let mut reader = BufReader::new(read);
        let mut line = Vec::new();
        for _ in 0..2 {
            wire::read_line(&mut reader, &mut line).await.unwrap();
            line.clear();
        }
        let registered = HubMsg::Registered {
            files: false,
            heartbeat,
        };
        wire::write_msg(&mut write, &registered).await.unwrap();
        (reader, write)
    }

    /// TASK-049: a hub that stops answering but never closes (a NAT on the
    /// way forgot the connection) is left after the timeout, and the agent
    /// connects again.
    #[tokio::test]
    async fn a_frozen_hub_is_left_after_the_heartbeat_timeout() {
        let listener = TcpListener::bind(SocketAddr::from((Ipv4Addr::LOCALHOST, 0)))
            .await
            .unwrap();
        let (_outbox, mut events) = spawn(beating(listener.local_addr().unwrap()));
        let (mut reader, _write) = fake_hub(&listener, true).await;
        assert_eq!(next(&mut events).await, LinkEvent::Up { files: false });
        let up = tokio::time::Instant::now();
        // The agent pings the quiet hub.
        let mut line = Vec::new();
        tokio::time::timeout(WAIT, wire::read_line(&mut reader, &mut line))
            .await
            .unwrap()
            .unwrap();
        assert_eq!(wire::decode::<AgentMsg>(&line), Ok(AgentMsg::Ping));
        // The hub never writes: the link is given up once the timeout passed.
        assert_eq!(next(&mut events).await, LinkEvent::Down);
        assert!(up.elapsed() >= BEAT.timeout, "{:?}", up.elapsed());
        let (_reader, _write) = fake_hub(&listener, true).await;
        assert_eq!(next(&mut events).await, LinkEvent::Up { files: false });
    }

    /// A hub before TASK-049 gets no ping and is waited for as long as the
    /// connection stays open; so is any hub when the agent announced none.
    #[tokio::test]
    async fn without_a_heartbeat_on_both_ends_nothing_is_sent_or_timed() {
        for (agent, hub) in [(true, false), (false, true)] {
            let listener = TcpListener::bind(SocketAddr::from((Ipv4Addr::LOCALHOST, 0)))
                .await
                .unwrap();
            let mut link = beating(listener.local_addr().unwrap());
            link.register.heartbeat = agent;
            let (_outbox, mut events) = spawn(link);
            let (mut reader, _write) = fake_hub(&listener, hub).await;
            assert_eq!(next(&mut events).await, LinkEvent::Up { files: false });
            let mut line = Vec::new();
            let quiet = BEAT.timeout * 2;
            let read = tokio::time::timeout(quiet, wire::read_line(&mut reader, &mut line)).await;
            assert!(read.is_err(), "agent {agent}, hub {hub}: {line:?}");
            assert!(events.try_recv().is_err(), "agent {agent}, hub {hub}");
        }
    }

    /// Pings keep a quiet link up while the owner reads nothing (a worker
    /// that hands over or waits for claude to exit, TASK-040), and one-way
    /// traffic either way (file chunks, TASK-032) does not trip the side
    /// that only reads.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn the_heartbeat_holds_a_quiet_or_one_way_link() {
        let listener = ingress::bind(SocketAddr::from((Ipv4Addr::LOCALHOST, 0)))
            .await
            .unwrap();
        let addr = listener.local_addr().unwrap();
        let (hub_tx, mut hub_rx) = mpsc::channel(16);
        let _hub = tokio::spawn(ingress::serve_agents_with(
            listener,
            Secret::parse(SECRET).unwrap(),
            hub_tx,
            BEAT,
        ));
        let (outbox, mut events) = spawn(beating(addr));
        let (_, to_agent) = registered(&mut hub_rx).await;
        assert_eq!(next(&mut events).await, LinkEvent::Up { files: true });

        // Quiet, and nobody reads the link events.
        let quiet = tokio::time::timeout(BEAT.timeout * 3, hub_rx.recv()).await;
        assert!(quiet.is_err(), "{quiet:?}");
        assert!(events.try_recv().is_err());

        // The agent writes, the hub only reads.
        let span = BEAT.timeout * 2;
        let started = tokio::time::Instant::now();
        let mut replies = 0;
        while started.elapsed() < span {
            let reply = AgentMsg::Reply {
                text: "chunk".into(),
            };
            outbox.send(reply).await.unwrap();
            match within(hub_rx.recv()).await {
                Some(AgentEvent::Message { .. }) => replies += 1,
                other => panic!("expected the reply, got {other:?}"),
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
        assert!(replies > 10, "{replies}");

        // The hub writes, the agent only reads.
        let started = tokio::time::Instant::now();
        while started.elapsed() < span {
            let inbound = HubMsg::Inbound {
                content: "chunk".into(),
                meta: Default::default(),
            };
            to_agent.send(inbound.clone()).await.unwrap();
            assert_eq!(next(&mut events).await, LinkEvent::Message(inbound));
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
        assert!(hub_rx.try_recv().is_err(), "the hub kept the agent");
        assert!(events.try_recv().is_err(), "the agent kept the hub");
    }

    async fn within<T>(future: impl std::future::Future<Output = T>) -> T {
        tokio::time::timeout(WAIT, future).await.expect("in time")
    }

    fn device(secret: Option<&str>) -> DeviceConfig {
        DeviceConfig::from_vars(|name| {""")
f.save()
