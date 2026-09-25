import sys; sys.path.insert(0, 'maw/tasks/in_progress/TASK-049/scratch')
from sub import Sub
f = Sub('crates/cctg/src/wire.rs'); rep = f.rep
rep("""    #[test]
    fn unknown_fields_are_ignored() {""","""    /// TASK-049: the heartbeat is announced both ways; a peer before it sees
    /// the lines it saw before and never gets a `ping`.
    #[test]
    fn heartbeats_stay_compatible_with_version_one_peers() {
        assert_eq!(encode(&HubMsg::Ping), b"{\\"v\\":1,\\"type\\":\\"ping\\"}\n");
        assert_eq!(encode(&AgentMsg::Ping), b"{\\"v\\":1,\\"type\\":\\"ping\\"}\n");
        // A hub before it: no heartbeat.
        assert_eq!(
            decode::<HubMsg>(br#"{"v":1,"type":"registered","files":true}"#),
            Ok(HubMsg::Registered {
                files: true,
                heartbeat: false,
            })
        );
        // An agent before it: no heartbeat.
        let old = br#"{"v":1,"type":"register","session_id":"s","host":"h","cwd":"/w"}"#;
        match decode::<AgentMsg>(old) {
            Ok(AgentMsg::Register(register)) => assert!(!register.heartbeat),
            other => panic!("{other:?}"),
        }
        let line = br#"{"v":1,"type":"register","session_id":"s","host":"h","cwd":"/w","heartbeat":true}"#;
        assert!(matches!(
            decode::<AgentMsg>(line),
            Ok(AgentMsg::Register(Register {
                heartbeat: true,
                ..
            }))
        ));
    }

    #[tokio::test(start_paused = true)]
    async fn liveness_pings_when_quiet_and_dies_when_deaf() {
        assert_eq!(Liveness::new(None).next(), None);
        let heartbeat = Heartbeat {
            interval: Duration::from_secs(3),
            timeout: Duration::from_secs(9),
        };
        let start = Instant::now();
        let mut live = Liveness::new(Some(heartbeat));
        assert_eq!(
            live.next(),
            Some((start + heartbeat.interval, Beat::Ping))
        );
        tokio::time::advance(Duration::from_secs(8)).await;
        live.said();
        // One second to the timeout, three to the next ping.
        assert_eq!(live.next(), Some((start + heartbeat.timeout, Beat::Dead)));
        live.heard();
        assert_eq!(
            live.next(),
            Some((start + Duration::from_secs(11), Beat::Ping))
        );
        assert_eq!(beat(live.next()).await, Beat::Ping);
        assert_eq!(Instant::now(), start + Duration::from_secs(11));
    }

    #[test]
    fn unknown_fields_are_ignored() {""")
f.save()
