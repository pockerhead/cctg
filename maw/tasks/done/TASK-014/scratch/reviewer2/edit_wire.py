import sys
p = sys.argv[1]
s = open(p, encoding='utf-8').read()


def rep(old, new, count=1):
    global s
    assert s.count(old) == count, (old, s.count(old))
    s = s.replace(old, new)


rep('''//! A new optional field (`#[serde(default)]`) keeps [`VERSION`]; a new message
//! type or a changed meaning bumps it. Errors never carry the offending input:
//! a line can contain the secret.''',
'''//! A new optional field (`#[serde(default)]`) keeps [`VERSION`]; so does a new
//! message type that a peer sends only after the other side announced it in
//! such a field (`permission_ack`, see [`Register::verdict_ack`]). Any other
//! new message type or a changed meaning bumps it. Errors never carry the
//! offending input: a line can contain the secret.''')
rep('''    #[serde(default)]
    pub claude_pid: Option<u32>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PermissionRequest {''',
'''    #[serde(default)]
    pub claude_pid: Option<u32>,
    /// The agent answers a `permission_verdict` that carries a `verdict_id`
    /// with `permission_ack`. Agents built before TASK-014 leave it out: the
    /// hub then takes a verdict handed to their link as delivered.
    #[serde(default)]
    pub verdict_ack: bool,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PermissionRequest {''')
rep('''    PermissionRequest(PermissionRequest),
}

impl Kinds for AgentMsg {
    const KINDS: &'static [&'static str] = &["hello", "register", "reply", "permission_request"];
}''',
'''    PermissionRequest(PermissionRequest),
    /// The agent queued the verdict `verdict_id` for Claude Code. Sent only
    /// to a hub that put the id into the verdict.
    PermissionAck {
        verdict_id: u64,
    },
}

impl Kinds for AgentMsg {
    const KINDS: &'static [&'static str] = &[
        "hello",
        "register",
        "reply",
        "permission_request",
        "permission_ack",
    ];
}''')
rep('''    PermissionVerdict {
        request_id: String,
        behavior: Behavior,
    },
}''',
'''    PermissionVerdict {
        request_id: String,
        behavior: Behavior,
        /// Set only for an agent that registered with `verdict_ack`; the same
        /// id comes again when the hub re-sends the same answer.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        verdict_id: Option<u64>,
    },
}''')
rep('''                claude_pid: Some(4242),
            }),''', '''                claude_pid: Some(4242),
                verdict_ack: true,
            }),''')
rep('''                input_preview: "{\\"command\\":\\"cargo test\\"}".into(),
            }),
        ]
    }''', '''                input_preview: "{\\"command\\":\\"cargo test\\"}".into(),
            }),
            AgentMsg::PermissionAck {
                verdict_id: u64::MAX,
            },
        ]
    }''')
rep('''            HubMsg::PermissionVerdict {
                request_id: "abcde".into(),
                behavior: Behavior::Allow,
            },
            HubMsg::PermissionVerdict {
                request_id: "abcde".into(),
                behavior: Behavior::Deny,
            },''', '''            HubMsg::PermissionVerdict {
                request_id: "abcde".into(),
                behavior: Behavior::Allow,
                verdict_id: None,
            },
            HubMsg::PermissionVerdict {
                request_id: "abcde".into(),
                behavior: Behavior::Deny,
                verdict_id: Some(u64::MAX),
            },''')
rep('''                cwd: "/w".into(),
                claude_pid: None,
            }))
        );
    }''', '''                cwd: "/w".into(),
                claude_pid: None,
                verdict_ack: false,
            }))
        );
    }

    #[test]
    fn verdict_acks_stay_compatible_with_version_one_peers() {
        // A hub before TASK-014 sends no id; an agent before it reads past one.
        let legacy =
            br#"{"v":1,"type":"permission_verdict","request_id":"abcde","behavior":"allow"}"#;
        assert_eq!(
            decode::<HubMsg>(legacy),
            Ok(HubMsg::PermissionVerdict {
                request_id: "abcde".into(),
                behavior: Behavior::Allow,
                verdict_id: None,
            })
        );
        let without_id = encode(&HubMsg::PermissionVerdict {
            request_id: "abcde".into(),
            behavior: Behavior::Allow,
            verdict_id: None,
        });
        assert_eq!(without_id, [&legacy[..], b"\\n"].concat());
        let with_id = encode(&HubMsg::PermissionVerdict {
            request_id: "abcde".into(),
            behavior: Behavior::Deny,
            verdict_id: Some(7),
        });
        let value: Value = serde_json::from_slice(&with_id).unwrap();
        assert_eq!(value["v"], 1);
        assert_eq!(value["verdict_id"], 7);
        let line = br#"{"v":1,"type":"register","session_id":"s","host":"h","cwd":"/w","verdict_ack":true}"#;
        assert!(matches!(
            decode::<AgentMsg>(line),
            Ok(AgentMsg::Register(Register {
                verdict_ack: true,
                ..
            }))
        ));
        assert_eq!(
            decode::<AgentMsg>(br#"{"v":1,"type":"permission_ack"}"#),
            Err(WireError::Malformed)
        );
    }''')
open(p, 'w', encoding='utf-8', newline='\n').write(s)
print('ok')
