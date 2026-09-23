use cctg::agent::LinkEvent;
use cctg::channel::{Hub, Server};
use cctg::wire::{Behavior, HubMsg};
use tokio::sync::mpsc;

#[tokio::test]
async fn duplicate_permission_id_is_closed_by_the_first_verdict() {
    let (tx, mut rx) = mpsc::channel(4);
    let mut server = Server::new(Hub::Link(tx));
    server.on_line(br#"{"jsonrpc":"2.0","id":0,"method":"initialize","params":{"protocolVersion":"2025-11-25"}}"#);
    server.on_line(br#"{"jsonrpc":"2.0","method":"notifications/initialized"}"#);
    let request = br#"{"jsonrpc":"2.0","method":"notifications/claude/channel/permission_request","params":{"request_id":"abcde","tool_name":"Bash","description":"d","input_preview":"p"}}"#;
    server.on_line(request);
    server.on_line(request);
    rx.recv().await.expect("first relay");
    rx.recv().await.expect("duplicate relay currently occurs");

    let verdict = LinkEvent::Message(HubMsg::PermissionVerdict {
        request_id: "abcde".into(),
        behavior: Behavior::Allow,
    });
    assert_eq!(server.on_link(verdict.clone()).len(), 1);
    assert!(
        server.on_link(verdict).is_empty(),
        "the first verdict must close the request id even after duplicate input"
    );
}
