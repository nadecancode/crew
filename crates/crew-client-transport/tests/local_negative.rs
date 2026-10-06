#![cfg(all(feature = "local", any(target_os = "linux", target_os = "macos")))]

mod common;

use crew_client_transport::framing::{LineBuffer, MAX_FRAME_BYTES};
use crew_client_transport::local::{LocalProfile, LocalTransport};
use crew_client_transport::queue::EventQueue;
use crew_client_transport::{ChannelPurpose, HostEffect, HostEvent, TransportEnd};
use serde_json::json;
use std::os::unix::fs::PermissionsExt;
use std::time::Duration;
use tokio::io::{AsyncReadExt, AsyncWriteExt};

async fn refused_reply(reply: Vec<u8>, expected: TransportEnd) {
    let root = tempfile::Builder::new()
        .prefix("crew-peer")
        .tempdir_in("/tmp")
        .unwrap();
    let socket = root.path().join("daemon.sock");
    let listener = tokio::net::UnixListener::bind(&socket).unwrap();
    std::fs::set_permissions(&socket, std::fs::Permissions::from_mode(0o600)).unwrap();
    crew_core::paths::write_private_file(
        &crew_core::paths::operator_token_path(&socket),
        b"synthetic-local-token",
    )
    .unwrap();
    let peer = tokio::spawn(async move {
        let (mut stream, _) = listener.accept().await.unwrap();
        let mut lines = LineBuffer::default();
        let mut buffer = [0; 4096];
        loop {
            let bytes = stream.read(&mut buffer).await.unwrap();
            assert!(bytes > 0);
            if !lines.feed(&buffer[..bytes]).unwrap().is_empty() {
                break;
            }
        }
        let _ = stream.write_all(&reply).await;
    });
    let profile = LocalProfile {
        id: "negative".into(),
        endpoint_id: "negative".into(),
        workspace_key: "provisional".into(),
        socket,
    };
    let queue = EventQueue::new(1).unwrap();
    let effect = HostEffect::Open {
        channel_id: 1,
        profile_id: profile.id.clone(),
        endpoint_id: profile.endpoint_id.clone(),
        attempt: 1,
        purpose: ChannelPurpose::Workspace,
        expected_scope: None,
    };
    let mut channel = LocalTransport::open(profile, &effect, &queue).unwrap();
    let event = tokio::time::timeout(Duration::from_secs(5), queue.recv())
        .await
        .unwrap()
        .unwrap();
    assert!(
        matches!(event, HostEvent::Ended { reason, .. } if reason == expected),
        "{event:?}"
    );
    assert!(!format!("{event:?}").contains("synthetic-sensitive-response"));
    channel.close();
    channel.wait_closed().await;
    peer.await.unwrap();
}

#[test]
fn protected_local_hello_rejects_protocol_role_identity_and_secret_proof_mismatch() {
    if common::isolated_local(
        "protected_local_hello_rejects_protocol_role_identity_and_secret_proof_mismatch",
    ) {
        return;
    }
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    runtime.block_on(async {
        let valid = json!({"id":0,"result":{"role":"operator","version":"0.1.0","protocol":6,"pid":std::process::id()}});
        let mut protocol = valid.clone(); protocol["result"]["protocol"] = 5.into();
        let mut role = valid.clone(); role["result"]["role"] = "agent".into();
        let mut identity = valid.clone(); identity["result"]["agent_id"] = "agent".into();
        let mut pid = valid.clone(); pid["result"]["pid"] = (std::process::id() + 1).into();
        let mut proof = valid; proof["result"]["web_proof"] = "synthetic-sensitive-response".into();
        let old = json!({"id":0,"error":{"code":"invalid","message":"client speaks protocol 6, daemon speaks 1"}});
        let refusal = json!({"id":0,"error":{"code":"unauthorized","message":"synthetic-sensitive-response","data":{"token":"synthetic-sensitive-response"}}});
        for (reply, expected) in [(protocol, TransportEnd::ProtocolMismatch), (old, TransportEnd::ProtocolMismatch),
            (role, TransportEnd::Authentication), (identity, TransportEnd::Authentication), (pid, TransportEnd::Authentication),
            (proof, TransportEnd::Authentication), (refusal, TransportEnd::Authentication)] {
            refused_reply(format!("{reply}\n").into_bytes(), expected).await;
        }
    });
}

#[test]
fn bounded_hello_refuses_oversized_unterminated_and_malformed_lines_before_opening() {
    if common::isolated_local(
        "bounded_hello_refuses_oversized_unterminated_and_malformed_lines_before_opening",
    ) {
        return;
    }
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    runtime.block_on(async {
        refused_reply(vec![b'x'; MAX_FRAME_BYTES + 1], TransportEnd::Oversized).await;
        refused_reply(b"{\"id\":0,\"result\":true}".to_vec(), TransportEnd::Malformed).await;
        refused_reply(b"synthetic-sensitive-response\n".to_vec(), TransportEnd::Malformed).await;
        refused_reply(b"{\"id\":0,\"result\":true,\"error\":{\"code\":\"invalid\",\"message\":\"synthetic-sensitive-response\"}}\n".to_vec(), TransportEnd::Malformed).await;
        refused_reply(b"\xff\n".to_vec(), TransportEnd::Malformed).await;
    });
}
