import sys; sys.path.insert(0, 'maw/tasks/in_progress/TASK-049/scratch')
from sub import Sub
f = Sub('crates/cctg/src/hub/ingress.rs'); rep = f.rep
rep("""    #[tokio::test]
    async fn a_split_agent_line_survives_concurrent_outbound_traffic() {""","""    /// Short heartbeat for the TASK-049 tests.
    const BEAT: Heartbeat = Heartbeat {
        interval: Duration::from_millis(100),
        timeout: Duration::from_millis(600),
    };

    /// A registered peer of a hub with [`BEAT`]; `heartbeat`: what the peer
    /// announces.
    async fn beating_peer(heartbeat: bool) -> (Peer, mpsc::Receiver<AgentEvent>, u64) {
        let listener = bind(loopback()).await.unwrap();
        let addr = listener.local_addr().unwrap();
        let (tx, mut events) = mpsc::channel(16);
        tokio::spawn(serve_agents_with(listener, secret(), tx, BEAT));
        let mut peer = Peer::connect(addr).await;
        peer.send(&AgentMsg::Hello { secret: secret() }).await;
        let register = Register {
            heartbeat,
            ..register()
        };
        peer.send(&AgentMsg::Register(register)).await;
        assert_eq!(
            peer.recv().await,
            Ok(HubMsg::Registered {
                files: true,
                heartbeat: true,
            })
        );
        let Some(AgentEvent::Registered { conn, .. }) = within(events.recv()).await else {
            panic!("expected registration");
        };
        (peer, events, conn)
    }

    /// TASK-049: an agent that went silent (its connection died on the way
    /// without a close) is pinged, then unbound like a closed one.
    #[tokio::test]
    async fn a_silent_agent_is_pinged_then_unbound() {
        let (mut peer, mut events, conn) = beating_peer(true).await;
        let registered = Instant::now();
        assert_eq!(peer.recv().await, Ok(HubMsg::Ping));
        match within(events.recv()).await {
            Some(AgentEvent::Disconnected { conn: gone }) => assert_eq!(gone, conn),
            other => panic!("expected disconnect, got {other:?}"),
        }
        assert!(registered.elapsed() >= BEAT.timeout, "{:?}", registered.elapsed());
        loop {
            match peer.recv().await {
                Ok(HubMsg::Ping) => continue,
                other => {
                    assert_eq!(other, Err(WireError::Closed));
                    break;
                }
            }
        }
    }

    /// An agent's pings keep it bound; they reach the hub's actor never.
    #[tokio::test]
    async fn an_agent_that_pings_stays_bound() {
        let (mut peer, mut events, _) = beating_peer(true).await;
        let started = Instant::now();
        while started.elapsed() < BEAT.timeout * 3 {
            peer.send(&AgentMsg::Ping).await;
            tokio::time::sleep(BEAT.interval).await;
        }
        assert!(events.try_recv().is_err());
        assert_eq!(peer.recv().await, Ok(HubMsg::Ping));
    }

    /// An agent before TASK-049 gets no ping and is never timed out.
    #[tokio::test]
    async fn an_agent_without_heartbeat_gets_no_pings_and_stays() {
        let (mut peer, mut events, _) = beating_peer(false).await;
        let quiet = tokio::time::timeout(
            BEAT.timeout * 2,
            wire::read_line(&mut peer.reader, &mut peer.line),
        )
        .await;
        assert!(quiet.is_err(), "{:?}", peer.line);
        assert!(events.try_recv().is_err());
    }

    #[tokio::test]
    async fn a_split_agent_line_survives_concurrent_outbound_traffic() {""")
f.save()
