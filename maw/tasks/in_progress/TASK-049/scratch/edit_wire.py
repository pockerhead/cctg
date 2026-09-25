import sys; sys.path.insert(0, 'maw/tasks/in_progress/TASK-049/scratch')
from sub import Sub
f = Sub('crates/cctg/src/wire.rs'); rep = f.rep
rep("""//! [`Register::session_reads`]). Any other""","""//! [`Register::session_reads`]; `ping` either way, see
//! [`Register::heartbeat`]). Any other""")
rep("""use std::time::{SystemTime, UNIX_EPOCH};
""","""use std::time::{Duration, SystemTime, UNIX_EPOCH};
""")
rep("""use tokio::io::{AsyncBufRead, AsyncBufReadExt, AsyncReadExt, AsyncWrite, AsyncWriteExt};
""","""use tokio::io::{AsyncBufRead, AsyncBufReadExt, AsyncReadExt, AsyncWrite, AsyncWriteExt};
use tokio::time::Instant;
""")
rep("""    #[serde(default)]
    pub session_reads: bool,
}
""","""    #[serde(default)]
    pub session_reads: bool,
    /// The agent sends `ping` when it wrote nothing for a while and drops
    /// a link that brought nothing for longer ([`Heartbeat`], TASK-049);
    /// the hub does the same once both announced it. Agents built before
    /// leave it out and get no pings.
    #[serde(default)]
    pub heartbeat: bool,
}
""")
rep("""        answer: SessionAnswer,
    },
}

/// What the hub asks""","""        answer: SessionAnswer,
    },
    /// Only that the link lives ([`Heartbeat`]); sent only to a hub whose
    /// `registered` said `heartbeat`. Never answered.
    Ping,
}

/// What the hub asks""")
rep("""        "session_answer",
    ];
}
""","""        "session_answer",
        "ping",
    ];
}
""")
rep("""    Registered {
        #[serde(default, skip_serializing_if = "std::ops::Not::not")]
        files: bool,
    },""","""    Registered {
        #[serde(default, skip_serializing_if = "std::ops::Not::not")]
        files: bool,
        /// The hub keeps the [`Heartbeat`] with an agent that announced
        /// [`Register::heartbeat`] (TASK-049). Hubs built before leave it
        /// out: the agent then sends no pings and waits for the hub as long
        /// as the connection stays open.
        #[serde(default, skip_serializing_if = "std::ops::Not::not")]
        heartbeat: bool,
    },""")
rep("""        ask: SessionAsk,
    },
}

impl Kinds for HubMsg {""","""        ask: SessionAsk,
    },
    /// Only that the link lives ([`Heartbeat`]); sent only to an agent that
    /// registered with `heartbeat`. Never answered.
    Ping,
}

impl Kinds for HubMsg {""")
rep("""        "session_read",
    ];
}
""","""        "session_read",
        "ping",
    ];
}

/// Idle ping and dead-peer timeout of the agent link (TASK-049). A half-open
/// connection (a NAT or tunnel on the way forgot it) never reports an
/// error: only the silence of the peer shows it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Heartbeat {
    /// A side that wrote nothing for this long sends `ping`.
    pub interval: Duration,
    /// A side that read nothing for this long drops the link.
    pub timeout: Duration,
}

pub const HEARTBEAT_INTERVAL: Duration = Duration::from_secs(30);
pub const HEARTBEAT_TIMEOUT: Duration = Duration::from_secs(90);

impl Default for Heartbeat {
    fn default() -> Self {
        Self {
            interval: HEARTBEAT_INTERVAL,
            timeout: HEARTBEAT_TIMEOUT,
        }
    }
}

/// What a link's [`Liveness`] asks for next.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Beat {
    /// Nothing written for the interval: send `ping`.
    Ping,
    /// Nothing read for the timeout: the peer is gone.
    Dead,
}

/// When one end of a link last read and wrote; off without a heartbeat.
#[derive(Debug, Clone, Copy)]
pub struct Liveness {
    heartbeat: Option<Heartbeat>,
    heard: Instant,
    said: Instant,
}

impl Liveness {
    pub fn new(heartbeat: Option<Heartbeat>) -> Self {
        let now = Instant::now();
        Self {
            heartbeat,
            heard: now,
            said: now,
        }
    }

    /// A line came from the peer.
    pub fn heard(&mut self) {
        self.heard = Instant::now();
    }

    /// A line went to the peer.
    pub fn said(&mut self) {
        self.said = Instant::now();
    }

    /// The next beat and when it is due; `None` without a heartbeat.
    pub fn next(&self) -> Option<(Instant, Beat)> {
        let heartbeat = self.heartbeat?;
        let dead = self.heard + heartbeat.timeout;
        let ping = self.said + heartbeat.interval;
        Some(if dead <= ping {
            (dead, Beat::Dead)
        } else {
            (ping, Beat::Ping)
        })
    }
}

/// Waits for `next` ([`Liveness::next`]); never ends for `None`. Takes the
/// value, not the [`Liveness`], so a `select!` branch borrows nothing.
pub async fn beat(next: Option<(Instant, Beat)>) -> Beat {
    match next {
        Some((at, beat)) => {
            tokio::time::sleep_until(at).await;
            beat
        }
        None => std::future::pending().await,
    }
}
""")
rep("""                files: true,
                session_reads: true,
            }),
            AgentMsg::Reply {""","""                files: true,
                session_reads: true,
                heartbeat: true,
            }),
            AgentMsg::Reply {""")
rep("""                answer: SessionAnswer::Refused,
            },
        ]
    }
""","""                answer: SessionAnswer::Refused,
            },
            AgentMsg::Ping,
        ]
    }
""")
rep("""            HubMsg::Registered { files: false },
            HubMsg::Registered { files: true },""","""            HubMsg::Registered {
                files: false,
                heartbeat: false,
            },
            HubMsg::Registered {
                files: true,
                heartbeat: true,
            },
            HubMsg::Ping,""")
rep("""                files: false,
                session_reads: false,
            }))
        );
    }
""","""                files: false,
                session_reads: false,
                heartbeat: false,
            }))
        );
    }
""")
rep("""            Ok(HubMsg::Registered { files: false })
        );""","""            Ok(HubMsg::Registered {
                files: false,
                heartbeat: false,
            })
        );""", 2)
rep("""            files: false,
            session_reads: false,
            client: Some(Client {""","""            files: false,
            session_reads: false,
            heartbeat: false,
            client: Some(Client {""")
rep("""            encode(&HubMsg::Registered { files: false }),""","""            encode(&HubMsg::Registered {
                files: false,
                heartbeat: false,
            }),""")
rep("""            serde_json::from_slice(&encode(&HubMsg::Registered { files: true })).unwrap();""","""            serde_json::from_slice(&encode(&HubMsg::Registered {
                files: true,
                heartbeat: false,
            }))
            .unwrap();""")
f.save()
