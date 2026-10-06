use super::authority::{normalize, validate_listing, Fence};
use super::*;
use crew_client::api::{ClientScope, GrantTier};
use crew_protocol::native::{self, *};
use crew_protocol::rpc::{DaemonRef, HelloInfo, Role, WorkspaceEntry, Workspaces};
use crew_protocol::WorkspaceState;
use sha2::{Digest, Sha256};

const CERT: &[u8] = include_bytes!("../../tests/fixtures/gateway/cert.pem");

#[test]
fn fresh_grant_proof_ahead_of_index_preserves_new_channel_and_never_rolls_back() {
    let fence = Fence::default();
    let queue = crate::queue::EventQueue::new(4).unwrap();
    let (stop, _cancel) = tokio::sync::watch::channel(None);
    let index = fence
        .register(None, stop, queue.register(1).unwrap())
        .unwrap();
    fence.admit(index, &authority(), None).unwrap();
    let prior = GatewayListing {
        authority: authority(),
        grants: vec![grant()],
        listing: Workspaces {
            rev: 1,
            joined: vec![],
            others: vec![],
        },
    };
    fence.listing(index, &prior).unwrap();
    let (stop, _cancel) = tokio::sync::watch::channel(None);
    let old = fence
        .register(Some(grant().workspace_id), stop, queue.register(2).unwrap())
        .unwrap();
    fence.admit(old, &authority(), Some(&grant())).unwrap();
    let (stop, _cancel) = tokio::sync::watch::channel(None);
    let fresh = fence
        .register(Some(grant().workspace_id), stop, queue.register(3).unwrap())
        .unwrap();
    let mut upgraded = grant();
    upgraded.grant_generation = 2;
    upgraded.capabilities.push(Capability::Control);
    upgraded.allowed_actions.push("send".into());
    fence.admit(fresh, &authority(), Some(&upgraded)).unwrap();
    assert!(fence.admitted(fresh));
    assert!(!fence.admitted(old));
    assert!(fence.admitted(index));
    assert!(matches!(
        queue.try_recv(),
        Some(crate::HostEvent::Ended {
            channel_id: 2,
            reason: crate::TransportEnd::GrantChanged
        })
    ));
    fence.listing(index, &prior).unwrap();
    assert!(fence.admitted(fresh));
    let current = GatewayListing {
        grants: vec![upgraded],
        listing: Workspaces {
            rev: 2,
            ..prior.listing.clone()
        },
        ..prior
    };
    fence.listing(index, &current).unwrap();
    assert!(fence.admitted(fresh));
    let (stop, _cancel) = tokio::sync::watch::channel(None);
    let stale = fence
        .register(Some(grant().workspace_id), stop, queue.register(4).unwrap())
        .unwrap();
    assert_eq!(
        fence.admit(stale, &authority(), Some(&grant())),
        Err(crate::TransportEnd::GrantChanged)
    );
    assert!(!fence.admitted(stale));
    assert!(fence.admitted(fresh));
}

#[test]
fn live_shared_policy_and_gateway_purposes_gate_every_app_method() {
    use crew_client::api::CommandKind;
    use crew_client::policy::{method_policy, GatewayPermission};
    let mut all = grant();
    all.capabilities = vec![Capability::Read, Capability::Control, Capability::Terminal];
    all.allowed_actions = CommandKind::ALL
        .iter()
        .filter(|kind| method_policy(**kind).gateway != GatewayPermission::Denied)
        .map(|kind| kind.method().to_string())
        .collect();
    for kind in CommandKind::ALL {
        let text = serde_json::json!({"id":7,"method":kind.method(),"params":{}}).to_string();
        for purpose in [
            native::ChannelPurpose::Index,
            native::ChannelPurpose::Workspace,
            native::ChannelPurpose::Read,
            native::ChannelPurpose::Action,
            native::ChannelPurpose::Terminal,
        ] {
            let permission = method_policy(*kind).gateway;
            let allowed = *kind == CommandKind::Ping
                || match purpose {
                    native::ChannelPurpose::Index => *kind == CommandKind::Workspaces,
                    native::ChannelPurpose::Workspace => {
                        permission == GatewayPermission::Read && *kind != CommandKind::Workspaces
                    }
                    native::ChannelPurpose::Read => {
                        permission == GatewayPermission::Read
                            && !matches!(kind, CommandKind::Workspaces | CommandKind::Subscribe)
                    }
                    native::ChannelPurpose::Action => {
                        permission == GatewayPermission::Control || *kind == CommandKind::AgentInput
                    }
                    native::ChannelPurpose::Terminal => matches!(
                        kind,
                        CommandKind::Attach | CommandKind::AttachInput | CommandKind::AttachResize
                    ),
                };
            assert_eq!(
                super::connector::request_kind(7, &text, purpose, Some(&all)).is_ok(),
                allowed,
                "{} {purpose:?}",
                kind.method()
            );
        }
    }
    for method in [
        "hello",
        "native_auth",
        "native_enroll",
        "raw_unknown",
        "agent_focus",
    ] {
        assert_eq!(
            super::connector::request_kind(
                7,
                &serde_json::json!({"id":7,"method":method}).to_string(),
                native::ChannelPurpose::Action,
                Some(&all)
            ),
            Err(crate::TransportEnd::Permission)
        );
    }
    let text = serde_json::json!({"id":7,"method":"agent_input","params":{}}).to_string();
    assert_eq!(
        super::connector::request_kind(7, &text, native::ChannelPurpose::Action, Some(&grant())),
        Err(crate::TransportEnd::Permission)
    );
    let duplicate = r#"{"id":7,"id":7,"method":"ping"}"#;
    assert_eq!(
        super::connector::request_kind(7, duplicate, native::ChannelPurpose::Index, None),
        Err(crate::TransportEnd::Malformed)
    );
}

#[test]
fn authority_generations_expiry_listing_revision_and_retirement_fence_old_channels() {
    for change in [0, 1] {
        let fence = Fence::default();
        let queue = crate::queue::EventQueue::new(2).unwrap();
        let (stop, _cancel) = tokio::sync::watch::channel(None);
        let index = fence
            .register(None, stop, queue.register(1).unwrap())
            .unwrap();
        fence.admit(index, &authority(), None).unwrap();
        fence
            .listing(
                index,
                &GatewayListing {
                    authority: authority(),
                    grants: vec![],
                    listing: Workspaces {
                        rev: 2,
                        joined: vec![],
                        others: vec![],
                    },
                },
            )
            .unwrap();
        assert_eq!(
            fence.listing(
                index,
                &GatewayListing {
                    authority: authority(),
                    grants: vec![],
                    listing: Workspaces {
                        rev: 1,
                        joined: vec![],
                        others: vec![]
                    }
                }
            ),
            Err(crate::TransportEnd::Malformed)
        );
        let mut changed = authority();
        if change == 0 {
            changed.device_generation += 1;
        } else {
            changed.credential_expires_at += 1;
        }
        assert_eq!(
            fence.admit(index, &changed, None),
            Err(crate::TransportEnd::Revoked)
        );
        assert!(!fence.admitted(index));
    }
    let fence = Fence::default();
    let queue = crate::queue::EventQueue::new(2).unwrap();
    for id in 1..=2 {
        let (stop, _cancel) = tokio::sync::watch::channel(None);
        let registration = fence
            .register(None, stop, queue.register(id).unwrap())
            .unwrap();
        fence.admit(registration, &authority(), None).unwrap();
    }
    fence.invalidate(None, crate::TransportEnd::Revoked, true);
    assert!(matches!(
        queue.try_recv(),
        Some(crate::HostEvent::Ended {
            reason: crate::TransportEnd::Revoked,
            ..
        })
    ));
    assert!(matches!(
        queue.try_recv(),
        Some(crate::HostEvent::Ended {
            reason: crate::TransportEnd::Revoked,
            ..
        })
    ));
    let (stop, _) = tokio::sync::watch::channel(None);
    assert!(matches!(
        fence.register(None, stop, queue.register(3).unwrap()),
        Err(crate::TransportEnd::Permission)
    ));
}

#[test]
fn approval_wait_uses_accepted_expiry_and_never_exceeds_protocol_window() {
    assert_eq!(
        super::setup::approval_delay(20_000, 10_000).unwrap(),
        std::time::Duration::from_secs(10)
    );
    assert_eq!(
        super::setup::approval_delay(i64::MAX, 10_000).unwrap(),
        std::time::Duration::from_secs(120)
    );
    assert_eq!(
        super::setup::approval_delay(10_000, 10_000),
        Err(GatewayError::Transport(crate::TransportEnd::Timeout))
    );
}

fn authority() -> GatewayAuthority {
    GatewayAuthority {
        endpoint_id: HexBytes([1; 16]),
        endpoint_epoch: HexBytes([2; 16]),
        device_id: HexBytes([3; 16]),
        device_generation: 1,
        credential_expires_at: 90_000,
        server_time: 10_000,
    }
}

fn grant() -> WorkspaceGrant {
    WorkspaceGrant {
        workspace_id: HexBytes([4; 8]),
        grant_generation: 1,
        capabilities: vec![Capability::Read],
        allowed_actions: vec!["snapshot".into(), "subscribe".into(), "agents_list".into()],
    }
}

fn opened() -> GatewayOpened<HelloInfo> {
    GatewayOpened {
        native_protocol: 1,
        protocol: 6,
        authority: authority(),
        purpose: native::ChannelPurpose::Workspace,
        grant: Some(grant()),
        route: Some(GatewayRoute {
            workspace_id: HexBytes([4; 8]),
            workspace_generation: 2,
            daemon: DaemonRef {
                pid: 77,
                started_at: 12_000,
            },
        }),
        hello: Some(HelloInfo {
            role: Role::Operator,
            agent_id: None,
            project: None,
            version: "test".into(),
            protocol: 6,
            pid: 77,
            web_proof: None,
        }),
    }
}

#[test]
fn route_is_the_only_scope_proof_and_captured_expectations_never_replace_it() {
    let mut value = opened();
    let expected = ClientScope {
        endpoint_id: value.authority.endpoint_id.to_hex(),
        workspace_id: grant().workspace_id.to_hex(),
        generation: None,
        daemon_pid: None,
        daemon_started_at_ms: None,
        attempt: 8,
    };
    let (context, _) = normalize(
        &value,
        value.authority.endpoint_id,
        value.purpose,
        Some(grant().workspace_id),
        Some(&expected),
        8,
    )
    .unwrap()
    .unwrap();
    assert_eq!(context.scope.generation, Some(2));
    assert_eq!(context.scope.daemon_pid, Some(77));
    assert_eq!(context.scope.daemon_started_at_ms, Some(12_000));
    assert_eq!(context.grant.tiers, [GrantTier::Read]);
    value.route = None;
    assert_eq!(
        normalize(
            &value,
            value.authority.endpoint_id,
            value.purpose,
            Some(grant().workspace_id),
            Some(&context.scope),
            8
        ),
        Err(crate::TransportEnd::Malformed)
    );
    value = opened();
    for contradiction in [
        ClientScope {
            generation: Some(7),
            ..context.scope.clone()
        },
        ClientScope {
            daemon_pid: Some(88),
            ..context.scope.clone()
        },
        ClientScope {
            daemon_started_at_ms: Some(13_000),
            ..context.scope.clone()
        },
    ] {
        assert_eq!(
            normalize(
                &value,
                value.authority.endpoint_id,
                value.purpose,
                Some(grant().workspace_id),
                Some(&contradiction),
                8
            ),
            Err(crate::TransportEnd::GrantChanged)
        );
    }
    value.hello.as_mut().unwrap().pid = 88;
    assert_eq!(
        normalize(
            &value,
            value.authority.endpoint_id,
            value.purpose,
            Some(grant().workspace_id),
            None,
            8
        ),
        Err(crate::TransportEnd::Malformed)
    );
    value = opened();
    value.route.as_mut().unwrap().workspace_generation = 0;
    assert_eq!(
        normalize(
            &value,
            value.authority.endpoint_id,
            value.purpose,
            Some(grant().workspace_id),
            None,
            8
        ),
        Err(crate::TransportEnd::Malformed)
    );
    value = opened();
    value.purpose = native::ChannelPurpose::Terminal;
    assert_eq!(
        normalize(
            &value,
            value.authority.endpoint_id,
            value.purpose,
            Some(grant().workspace_id),
            None,
            8
        ),
        Err(crate::TransportEnd::Permission)
    );
}

#[test]
fn index_has_no_workspace_and_opaque_grant_changes_only_with_verified_identity() {
    let mut value = opened();
    let (first, _) = normalize(
        &value,
        value.authority.endpoint_id,
        value.purpose,
        Some(grant().workspace_id),
        None,
        1,
    )
    .unwrap()
    .unwrap();
    value.authority.server_time += 1;
    let (second, _) = normalize(
        &value,
        value.authority.endpoint_id,
        value.purpose,
        Some(grant().workspace_id),
        None,
        2,
    )
    .unwrap()
    .unwrap();
    assert_eq!(first.grant, second.grant);
    value.authority.endpoint_epoch = HexBytes([9; 16]);
    let (third, _) = normalize(
        &value,
        value.authority.endpoint_id,
        value.purpose,
        Some(grant().workspace_id),
        None,
        3,
    )
    .unwrap()
    .unwrap();
    assert_ne!(first.grant.grant_id, third.grant.grant_id);
    value.purpose = native::ChannelPurpose::Index;
    assert_eq!(
        normalize(
            &value,
            value.authority.endpoint_id,
            value.purpose,
            None,
            None,
            0
        ),
        Err(crate::TransportEnd::Malformed)
    );
    value.route = None;
    value.hello = None;
    value.grant = None;
    for field in 0..3 {
        let mut probe = value.clone();
        match field {
            0 => probe.route = opened().route,
            1 => probe.hello = opened().hello,
            _ => probe.grant = opened().grant,
        }
        assert_eq!(
            normalize(
                &probe,
                probe.authority.endpoint_id,
                probe.purpose,
                None,
                None,
                0
            ),
            Err(crate::TransportEnd::Malformed)
        );
    }
    assert_eq!(
        normalize(
            &value,
            value.authority.endpoint_id,
            value.purpose,
            None,
            None,
            0
        ),
        Ok(None)
    );
}

#[test]
fn grant_change_fences_workspace_lanes_before_listing_and_keeps_index_independent() {
    let fence = Fence::default();
    let queue = crate::queue::EventQueue::new(3).unwrap();
    let (index_stop, _) = tokio::sync::watch::channel(None);
    let index = fence
        .register(None, index_stop, queue.register(1).unwrap())
        .unwrap();
    fence.admit(index, &authority(), None).unwrap();
    let (workspace_stop, _) = tokio::sync::watch::channel(None);
    let workspace = fence
        .register(
            Some(grant().workspace_id),
            workspace_stop,
            queue.register(2).unwrap(),
        )
        .unwrap();
    fence
        .admit(workspace, &authority(), Some(&grant()))
        .unwrap();
    fence
        .listing(
            index,
            &GatewayListing {
                authority: authority(),
                grants: vec![],
                listing: Workspaces {
                    rev: 1,
                    joined: vec![],
                    others: vec![],
                },
            },
        )
        .unwrap();
    assert!(fence.admitted(index));
    assert!(!fence.admitted(workspace));
    assert!(matches!(
        queue.try_recv(),
        Some(crate::HostEvent::Ended {
            channel_id: 2,
            reason: crate::TransportEnd::GrantChanged
        })
    ));
    let mut changed = authority();
    changed.endpoint_epoch = HexBytes([7; 16]);
    assert_eq!(
        fence.admit(index, &changed, None),
        Err(crate::TransportEnd::Closed)
    );
    assert!(!fence.admitted(index));
    // A retired old epoch cannot flip the accepted identity back.
    assert_eq!(
        fence.admit(workspace, &authority(), Some(&grant())),
        Err(crate::TransportEnd::Closed)
    );
}

#[test]
fn native_listing_never_exposes_others_or_ungranted_duplicate_workspaces() {
    let mut value = GatewayListing {
        authority: authority(),
        grants: vec![grant()],
        listing: Workspaces {
            rev: 1,
            joined: vec![WorkspaceEntry {
                id: grant().workspace_id.to_hex(),
                generation: 2,
                daemon: Some(DaemonRef {
                    pid: 999,
                    started_at: 777,
                }),
                project: None,
                workspace: "/public selected workspace".into(),
                state: WorkspaceState::Running,
                detail: String::new(),
                summary: None,
                joined_at: 10_000,
            }],
            others: vec![],
        },
    };
    assert!(validate_listing(&value, authority().endpoint_id).is_ok());
    value.listing.joined.push(value.listing.joined[0].clone());
    assert_eq!(
        validate_listing(&value, authority().endpoint_id),
        Err(crate::TransportEnd::Malformed)
    );
    value.listing.joined.pop();
    value.grants.clear();
    assert_eq!(
        validate_listing(&value, authority().endpoint_id),
        Err(crate::TransportEnd::Malformed)
    );
}

#[test]
fn comparison_vector_and_debug_never_serialize_private_material() {
    let (endpoint, leaf, nonce, params, expected) = native::comparison_vector();
    let bytes = native::enroll_transcript(endpoint, leaf, nonce, &params);
    assert_eq!(&Sha256::digest(&bytes)[..8], expected.0);
    assert_eq!(bytes.len(), native::ENROLL_TRANSCRIPT_BYTES);
    let marker = b"synthetic-private-key-marker";
    assert!(!format!("{:?}", AuthTranscript(marker.to_vec()))
        .contains(std::str::from_utf8(marker).unwrap()));
    assert!(!format!(
        "{:?}",
        EnrollTranscript([b'x'; native::ENROLL_TRANSCRIPT_BYTES])
    )
    .contains(&"x".repeat(32)));
    assert_eq!(wire::close_reason(Some(4101)), crate::TransportEnd::Revoked);
    assert_eq!(
        wire::close_reason(Some(4102)),
        crate::TransportEnd::GrantChanged
    );
    assert_eq!(wire::close_reason(Some(4103)), crate::TransportEnd::Stopped);
    assert_eq!(
        wire::close_reason(Some(4104)),
        crate::TransportEnd::ProtocolMismatch
    );
    assert!(
        !format!("{:?}", GatewayError::Signer(SignerError::InvalidProof))
            .contains(std::str::from_utf8(marker).unwrap())
    );
}

#[test]
fn trust_requires_explicit_pin_and_rejects_credentials_plaintext_and_hidden_routes() {
    let cert = rustls_pemfile::certs(&mut std::io::BufReader::new(CERT))
        .next()
        .unwrap()
        .unwrap();
    let profile = PublicProfile {
        endpoint_id: HexBytes([1; 16]),
        name: "Test host".into(),
        url: "wss://127.0.0.1:1234/native/ws".into(),
        tls_leaf_sha256: HexBytes(Sha256::digest(cert.as_ref()).into()),
        trust_pem: std::str::from_utf8(CERT).unwrap().into(),
    };
    assert!(
        GatewayProfile::trust("profile".into(), profile.clone(), profile.tls_leaf_sha256).is_ok()
    );
    let mut marker = profile.clone();
    marker.name = "synthetic-private-key-marker".into();
    let trusted = GatewayProfile::trust("profile".into(), marker, profile.tls_leaf_sha256).unwrap();
    assert!(!format!("{trusted:?}").contains("synthetic-private-key-marker"));
    assert!(matches!(
        GatewayProfile::trust("profile".into(), profile.clone(), HexBytes([9; 32])),
        Err(GatewayError::Trust)
    ));
    for url in [
        "ws://127.0.0.1:1234/native/ws",
        "wss://u:p@localhost:1234/native/ws",
        "wss://localhost/native/ws",
        "wss://localhost:1234/native/ws?x=1",
        "wss://localhost:1234/native/ws#fragment",
        "wss://localhost:1234/other",
    ] {
        let mut probe = profile.clone();
        probe.url = url.into();
        assert!(matches!(
            GatewayProfile::trust("profile".into(), probe, profile.tls_leaf_sha256),
            Err(GatewayError::InvalidProfile)
        ));
    }
}
