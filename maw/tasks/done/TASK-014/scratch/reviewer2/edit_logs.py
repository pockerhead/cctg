import os
here = os.path.dirname(os.path.abspath(__file__))
p = os.path.join(here, 'ws/crates/cctg/tests/permission_logs.rs')
s = open(p, encoding='utf-8').read()


def rep(old, new, count=1):
    global s
    assert s.count(old) == count, (old, s.count(old))
    s = s.replace(old, new)


rep('''//! Permission relay through the real poll classification, the slot actor and
//! the scheduler, with log capture: a stranger's press sends nothing, an
//! allowlisted press sends one verdict, and neither the request fields, the
//! request id nor the user id reach the logs. Its own test binary with a
//! global subscriber (see `message_logs.rs`).''', '''//! Permission relay through the real poll classification, the slot actor and
//! the scheduler, with log capture: a stranger's press sends nothing, an
//! allowlisted press sends one verdict that the agent acknowledges, the end
//! of the session closes the next prompt, and neither the request fields,
//! the request or verdict id nor the user id reach the logs. Its own test
//! binary with a global subscriber (see `message_logs.rs`).''')
rep('''const REQUEST: &str = "qzxwv";''', '''const REQUEST: &str = "qzxwv";
const SECOND_REQUEST: &str = "wvxzq";''')
rep('''                claude_pid: Some(10),
                verdict_ack: false,
            },''', '''                claude_pid: Some(10),
                verdict_ack: true,
            },''')
rep('''    let got = tokio::time::timeout(Duration::from_secs(30), to_agent_rx.recv())
        .await
        .expect("verdict in time");
    assert_eq!(
        got,
        Some(HubMsg::PermissionVerdict {
            request_id: REQUEST.into(),
            behavior: Behavior::Allow,
        })
    );
    until("answer and edit", || {
        fake.ops().iter().any(|op| matches!(op, Op::Edit { .. }))
    })
    .await;''', '''    let got = tokio::time::timeout(Duration::from_secs(30), to_agent_rx.recv())
        .await
        .expect("verdict in time");
    let Some(HubMsg::PermissionVerdict {
        request_id,
        behavior: Behavior::Allow,
        verdict_id: Some(verdict_id),
    }) = got
    else {
        panic!("one allow verdict with an id: {got:?}");
    };
    assert_eq!(request_id, REQUEST);
    tokio::time::sleep(Duration::from_millis(200)).await;
    assert!(
        !fake.ops().iter().any(|op| matches!(op, Op::Edit { .. })),
        "decided only by the ack"
    );
    agents
        .send(AgentEvent::Message {
            conn: 1,
            msg: AgentMsg::PermissionAck { verdict_id },
        })
        .await
        .expect("ack");
    until("answer and edit", || {
        fake.ops().iter().any(|op| matches!(op, Op::Edit { .. }))
    })
    .await;''')
rep('''    assert!(to_agent_rx.try_recv().is_err(), "one verdict only");
    let _ = std::fs::remove_dir_all(&state);''', '''    assert!(to_agent_rx.try_recv().is_err(), "one verdict only");

    // A second prompt is still open when the session ends: it is closed.
    agents
        .send(AgentEvent::Message {
            conn: 1,
            msg: AgentMsg::PermissionRequest(PermissionRequest {
                request_id: SECOND_REQUEST.into(),
                tool_name: tool.clone(),
                description: description.clone(),
                input_preview: preview.clone(),
            }),
        })
        .await
        .expect("second request");
    let permission_sends = || {
        fake.ops()
            .iter()
            .filter(|op| {
                matches!(
                    op,
                    Op::Send {
                        permission: true,
                        ..
                    }
                )
            })
            .count()
    };
    until("second prompt", || permission_sends() == 2).await;
    tokio::time::sleep(Duration::from_millis(200)).await;
    hooks
        .send(HookPost::new(
            "box".into(),
            session.into(),
            r"C:\\w\\p".into(),
            String::new(),
            HookEvent::SessionEnd {
                reason: None,
                claude_pid: Some(10),
            },
        ))
        .await
        .expect("hook");
    until("closing edit", || {
        fake.ops().iter().any(|op| {
            matches!(op, Op::Edit { text, .. } if text == permissions::CLOSED_TEXT)
        })
    })
    .await;
    let _ = std::fs::remove_dir_all(&state);''')
rep('''    for expected in [
        "permission request queued for the topic",
        "permission verdict forwarded to the session agent",
    ] {''', '''    for expected in [
        "permission request queued for the topic",
        "permission answer chosen in Telegram",
        "permission verdict forwarded to the session agent",
        "permission verdict taken by the session agent",
        "permission prompt closed: its session ended",
    ] {''')
rep('''        REQUEST,
        &USER.to_string(),''', '''        REQUEST,
        SECOND_REQUEST,
        &verdict_id.to_string(),
        &USER.to_string(),''')
open(p, 'w', encoding='utf-8', newline='\n').write(s)

p = os.path.join(here, 'ws/crates/cctg/src/hub/permissions.rs')
s = open(p, encoding='utf-8').read()
rep('''        let gone = self.prompts.remove(&key)?;
        if let Some(message_id) = gone.message_id {
            self.by_message.remove(&message_id);
        }
        Some(gone)''', '''        let gone = self.prompts.remove(&key)?;
        if let Some(message_id) = gone.message_id
            && self.by_message.get(&message_id) == Some(&key)
        {
            self.by_message.remove(&message_id);
        }
        Some(gone)''')
open(p, 'w', encoding='utf-8', newline='\n').write(s)
print('ok')
