def edit(p, pairs):
    s = open(p, encoding='utf-8').read()
    for old, new in pairs:
        assert s.count(old) == 1, (p, old, s.count(old))
        s = s.replace(old, new)
    open(p, 'w', encoding='utf-8', newline='').write(s)

edit('crates/cctg/src/hub/ingress.rs', [
("""                        | AgentMsg::FileChunk(_)
                        | AgentMsg::SessionAnswer { .. }),""",
"""                        | AgentMsg::FileChunk(_)
                        | AgentMsg::SessionAnswer { .. }
                        | AgentMsg::StatusLine { .. }),"""),
])

edit('crates/cctg/src/hub/slots.rs', [
("""    /// It reads its session's files ([`crate::wire::Register::session_reads`]).
    session_reads: bool,
    /// It is leaving""",
"""    /// It reads its session's files ([`crate::wire::Register::session_reads`]).
    session_reads: bool,
    /// It passes status line numbers on and is told its session
    /// ([`crate::wire::Register::status_lines`]).
    status_lines: bool,
    /// It is leaving"""),
("""                        files: register.files,
                        session_reads: register.session_reads,
                        leaving: false,
                    },
                );""",
"""                        files: register.files,
                        session_reads: register.session_reads,
                        status_lines: register.status_lines,
                        leaving: false,
                    },
                );
                self.tell_bound(conn);"""),
("""                    AgentMsg::SessionAnswer { read_id, answer } => {
                        self.on_session_answer(conn, read_id, answer);
                    }
                    _ => debug!(conn, "agent message not routed"),""",
"""                    AgentMsg::SessionAnswer { read_id, answer } => {
                        self.on_session_answer(conn, read_id, answer);
                    }
                    AgentMsg::StatusLine {
                        session_id,
                        model,
                        effort,
                        context,
                        five_hour,
                        seven_day,
                    } if session_id == session => {
                        let Some(host) = self.conns.get(&conn).map(|bound| bound.host.clone())
                        else {
                            return;
                        };
                        // The same event `cctg statusline` posts without an
                        // agent (TASK-058).
                        let numbers = HookEvent::StatusLine {
                            model,
                            effort,
                            context,
                            five_hour,
                            seven_day,
                        };
                        let post =
                            HookPost::new(host, session, String::new(), String::new(), numbers);
                        self.on_hook(&post);
                    }
                    _ => debug!(conn, "agent message not routed"),"""),
("""    /// The newest connection still open for `session` that belongs to the""",
"""    /// Tells an agent that passes status line numbers on which session
    /// `conn` is bound to now (TASK-058); a full queue drops it, the next
    /// registration tells again.
    fn tell_bound(&self, conn: u64) {
        let Some(bound) = self.conns.get(&conn).filter(|bound| bound.status_lines) else {
            return;
        };
        let told = HubMsg::Bound {
            session_id: bound.session.clone(),
        };
        if bound.to_agent.try_send(told).is_err() {
            debug!(conn, "agent queue full; its session not told");
        }
    }

    /// The newest connection still open for `session` that belongs to the"""),
("""        if self.registry.agent_connected(&session, conn) {
            info!(
                conn,
                from = short(&old),
                session = short(&session),
                "agent follows its claude process to a new session"
            );
        }
    }""",
"""        if self.registry.agent_connected(&session, conn) {
            info!(
                conn,
                from = short(&old),
                session = short(&session),
                "agent follows its claude process to a new session"
            );
        }
        self.tell_bound(conn);
    }"""),
])
print("ok")
