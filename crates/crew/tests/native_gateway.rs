//! Isolated native TLS clients against the real crew daemon and gateway.
mod common;

#[cfg(not(feature = "native-gateway"))]
#[test]
fn gateway_is_not_built_or_started_by_default() {
    let session = common::Session::new();
    let output = session.crew(&["native", "status"]);
    assert!(!output.status.success());
    assert!(String::from_utf8_lossy(&output.stderr).contains("not built with the gateway"));
    assert!(!session.root().join("run/crew/native.sock").exists());
}

#[cfg(feature = "native-gateway")]
mod enabled {
    use super::common::*;
    use crew_protocol::native::*;
    use ed25519_dalek::{Signer, SigningKey};
    use futures_util::{SinkExt, StreamExt};
    use serde_json::{json, Value};
    use sha2::{Digest, Sha256};
    use std::net::TcpListener;
    use std::sync::Arc;
    use std::time::Duration;
    use tokio::net::TcpStream;
    use tokio_rustls::client::TlsStream;
    use tokio_tungstenite::{
        tungstenite::{client::IntoClientRequest, Message},
        WebSocketStream,
    };

    const CERT: &[u8] = include_bytes!("fixtures/native/cert.pem");
    const KEY: &[u8] = include_bytes!("fixtures/native/key.pem");
    type Ws = WebSocketStream<TlsStream<TcpStream>>;

    struct Host {
        session: Session,
        profile: PublicProfile,
        port: u16,
    }
    impl Host {
        fn start() -> Self {
            Self::start_certificate(CERT)
        }
        fn start_certificate(certificate: &[u8]) -> Self {
            let session = Session::new();
            std::fs::create_dir_all(&session.workspace).unwrap();
            session.ok(&["start", "-C", &session.workspace.to_string_lossy()]);
            let native = session.root().join("run/crew/native");
            crew_core::paths::ensure_private_dir(&native).unwrap();
            crew_core::paths::write_private_file(&native.join("key.pem"), KEY).unwrap();
            std::fs::write(native.join("cert.pem"), certificate).unwrap();
            let port = TcpListener::bind("127.0.0.1:0")
                .unwrap()
                .local_addr()
                .unwrap()
                .port();
            let listen = format!("127.0.0.1:{port}");
            let endpoint = format!("wss://127.0.0.1:{port}/native/ws");
            let out = session.ok(&[
                "native",
                "start",
                "--listen",
                &listen,
                "--endpoint",
                &endpoint,
                "--tls-cert",
                &native.join("cert.pem").to_string_lossy(),
                "--tls-key",
                &native.join("key.pem").to_string_lossy(),
                "--name",
                "Test host",
            ]);
            let status: Value = serde_json::from_str(&out).unwrap();
            Self {
                profile: serde_json::from_value(status["profile"].clone()).unwrap(),
                session,
                port,
            }
        }
        fn command(&self, args: &[&str]) -> Value {
            serde_json::from_str(&self.session.ok(args)).unwrap()
        }
        fn workspace(&self) -> WorkspaceId {
            WorkspaceId::from_hex(&crew_core::paths::session_id(
                &self.session.workspace.join(".crew"),
            ))
            .unwrap()
        }
        fn restart(&self) {
            self.command(&["native", "stop"]);
            let native = self.session.root().join("run/crew/native");
            self.command(&[
                "native",
                "start",
                "--listen",
                &format!("127.0.0.1:{}", self.port),
                "--endpoint",
                &self.profile.url,
                "--tls-cert",
                &native.join("cert.pem").to_string_lossy(),
                "--tls-key",
                &native.join("key.pem").to_string_lossy(),
                "--name",
                "Test host",
            ]);
        }
    }
    impl Drop for Host {
        fn drop(&mut self) {
            let _ = self.session.crew(&["native", "stop"]);
        }
    }

    async fn tls_stream(host: &Host, pin: LeafHash) -> anyhow::Result<TlsStream<TcpStream>> {
        tls_named(
            host,
            pin,
            rustls::pki_types::ServerName::IpAddress(std::net::Ipv4Addr::LOCALHOST.into()),
        )
        .await
    }
    async fn tls_named(
        host: &Host,
        pin: LeafHash,
        name: rustls::pki_types::ServerName<'static>,
    ) -> anyhow::Result<TlsStream<TcpStream>> {
        let certs = rustls_pemfile::certs(&mut std::io::BufReader::new(
            host.profile.trust_pem.as_bytes(),
        ))
        .collect::<std::io::Result<Vec<_>>>()?;
        let mut roots = rustls::RootCertStore::empty();
        roots.add(certs[0].clone())?;
        let config = rustls::ClientConfig::builder_with_provider(Arc::new(
            rustls::crypto::ring::default_provider(),
        ))
        .with_protocol_versions(&[&rustls::version::TLS13])?
        .with_root_certificates(roots)
        .with_no_client_auth();
        let stream = TcpStream::connect(("127.0.0.1", host.port)).await?;
        let tls = tokio_rustls::TlsConnector::from(Arc::new(config))
            .connect(name, stream)
            .await?;
        let actual: [u8; 32] =
            Sha256::digest(tls.get_ref().1.peer_certificates().unwrap()[0].as_ref()).into();
        anyhow::ensure!(actual == pin.0, "wrong TLS pin");
        Ok(tls)
    }

    async fn connect(host: &Host, pin: LeafHash) -> anyhow::Result<(Ws, Nonce)> {
        let tls = tls_stream(host, pin).await?;
        let (mut ws, _) =
            tokio_tungstenite::client_async(host.profile.url.clone().into_client_request()?, tls)
                .await?;
        let challenge = next(&mut ws).await;
        let frame: NoticeFrame = serde_json::from_value(challenge).unwrap();
        let Notice::Challenge { nonce } = frame.native else {
            panic!("challenge required")
        };
        Ok((ws, nonce))
    }

    async fn next(ws: &mut Ws) -> Value {
        loop {
            match tokio::time::timeout(Duration::from_secs(10), ws.next())
                .await
                .expect("native frame timeout")
                .expect("native stream ended")
                .expect("native frame")
            {
                Message::Text(text) => {
                    let frame: Value = serde_json::from_str(&text).unwrap();
                    // Check replies and unsolicited pushes, including those
                    // skipped while waiting for a request's correlated reply.
                    if let Some(listing) = frame.get("native_listing").or_else(|| {
                        frame
                            .get("result")
                            .filter(|result| result.get("listing").is_some())
                    }) {
                        let listing: GatewayListing<crew_core::rpc::Workspaces> =
                            serde_json::from_value(listing.clone()).unwrap();
                        let granted = listing
                            .grants
                            .iter()
                            .map(|grant| grant.workspace_id.to_hex())
                            .collect::<Vec<_>>();
                        let joined = listing
                            .listing
                            .joined
                            .iter()
                            .map(|entry| entry.id.clone())
                            .collect::<Vec<_>>();
                        assert_eq!(granted, joined, "native listing grants must match members");
                        assert!(listing.listing.others.is_empty());
                    }
                    return frame;
                }
                Message::Ping(data) => {
                    ws.send(Message::Pong(data)).await.unwrap();
                }
                other => panic!("unexpected native frame: {other:?}"),
            }
        }
    }

    async fn result(ws: &mut Ws, id: u64) -> Value {
        loop {
            let frame = next(ws).await;
            if frame["id"] == id {
                return frame;
            }
        }
    }

    async fn call(ws: &mut Ws, id: u64, method: &str, params: Value) -> Value {
        ws.send(Message::text(
            json!({"id":id,"method":method,"params":params}).to_string(),
        ))
        .await
        .unwrap();
        result(ws, id).await
    }

    async fn enroll(
        host: &Host,
        key: &SigningKey,
        control: bool,
        terminal: bool,
    ) -> (Ws, DeviceId) {
        enroll_lifetime(host, key, control, terminal, 2592000).await
    }

    async fn enroll_lifetime(
        host: &Host,
        key: &SigningKey,
        control: bool,
        terminal: bool,
        lifetime: u64,
    ) -> (Ws, DeviceId) {
        enroll_workspace(
            host,
            key,
            control,
            terminal,
            lifetime,
            &host.session.workspace,
        )
        .await
    }

    async fn enroll_workspace(
        host: &Host,
        key: &SigningKey,
        control: bool,
        terminal: bool,
        lifetime: u64,
        selected: &std::path::Path,
    ) -> (Ws, DeviceId) {
        host.command(&["native", "enroll"]);
        let (mut ws, nonce) = connect(host, host.profile.tls_leaf_sha256).await.unwrap();
        let mut params = EnrollParams {
            native_protocol: 1,
            protocol: 6,
            public_key: HexBytes(key.verifying_key().to_bytes()),
            client_nonce: HexBytes([5; 32]),
            signature: HexBytes([0; 64]),
            label: "Test phone".into(),
        };
        params.signature = HexBytes(
            key.sign(&enroll_transcript(
                host.profile.endpoint_id,
                host.profile.tls_leaf_sha256,
                nonce,
                &params,
            ))
            .to_bytes(),
        );
        ws.send(Message::text(
            serde_json::to_string(&HandshakeRequest {
                id: 1,
                call: HandshakeCall::NativeEnroll(params),
            })
            .unwrap(),
        ))
        .await
        .unwrap();
        let notice: NoticeFrame = serde_json::from_value(next(&mut ws).await).unwrap();
        let Notice::EnrollmentAccepted { intent } = notice.native else {
            panic!("accepted intent required")
        };
        let key_hash =
            HexBytes::<32>(Sha256::digest(key.verifying_key().as_bytes()).into()).to_hex();
        let mut args = vec!["native", "approve"];
        let enrollment_id = intent.enrollment_id.to_hex();
        let code = intent.comparison_code.to_hex();
        let workspace = selected.to_string_lossy().into_owned();
        args.extend([
            &enrollment_id,
            "--code",
            &code,
            "--key-hash",
            &key_hash,
            "--workspace",
            &workspace,
        ]);
        if control {
            args.push("--control");
        }
        if terminal {
            args.push("--terminal");
        }
        let lifetime = lifetime.to_string();
        args.extend(["--lifetime-secs", &lifetime]);
        let approved = host.command(&args);
        let device: DeviceId = serde_json::from_value(approved["device_id"].clone()).unwrap();
        let opened = result(&mut ws, 1).await;
        assert_eq!(opened["result"]["purpose"], "index");
        assert!(opened["result"]["route"].is_null());
        assert_eq!(opened["result"]["authority"]["device_id"], device.to_hex());
        let deadline = tokio::time::Instant::now() + Duration::from_secs(10);
        loop {
            let listing = call(&mut ws, 2, "workspaces", json!({})).await;
            assert_eq!(listing["result"]["listing"]["others"], json!([]));
            if listing["result"]["listing"]["joined"]
                .as_array()
                .is_some_and(|entries| entries.iter().any(|e| e["state"] == "running"))
            {
                break;
            }
            assert!(
                tokio::time::Instant::now() < deadline,
                "workspace never became live: {listing}"
            );
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
        (ws, device)
    }

    async fn begin_auth(
        host: &Host,
        key: &SigningKey,
        purpose: ChannelPurpose,
        workspace: Option<WorkspaceId>,
    ) -> (Ws, AuthParams) {
        let (mut ws, nonce) = connect(host, host.profile.tls_leaf_sha256).await.unwrap();
        let mut params = AuthParams {
            native_protocol: 1,
            protocol: 6,
            public_key: HexBytes(key.verifying_key().to_bytes()),
            signature: HexBytes([0; 64]),
            purpose,
            workspace,
        };
        params.signature = HexBytes(
            key.sign(
                &auth_transcript(
                    host.profile.endpoint_id,
                    host.profile.tls_leaf_sha256,
                    nonce,
                    &params,
                )
                .unwrap(),
            )
            .to_bytes(),
        );
        ws.send(Message::text(
            serde_json::to_string(&HandshakeRequest {
                id: 1,
                call: HandshakeCall::NativeAuth(params.clone()),
            })
            .unwrap(),
        ))
        .await
        .unwrap();
        (ws, params)
    }

    async fn auth(
        host: &Host,
        key: &SigningKey,
        purpose: ChannelPurpose,
        workspace: Option<WorkspaceId>,
    ) -> (Ws, Value, AuthParams) {
        let (mut ws, params) = begin_auth(host, key, purpose, workspace).await;
        let opened = result(&mut ws, 1).await;
        (ws, opened, params)
    }

    #[test]
    fn grant_revocation_cancels_route_verification_without_an_opened_reply() {
        struct PausedDaemon(u32);
        impl Drop for PausedDaemon {
            fn drop(&mut self) {
                // SAFETY: this PID came from this test's isolated real daemon.
                unsafe { libc::kill(self.0 as libc::pid_t, libc::SIGCONT) };
            }
        }

        let host = Host::start();
        tokio::runtime::Runtime::new().unwrap().block_on(async {
            let key = SigningKey::from_bytes(&[27; 32]);
            let (mut index, device) = enroll(&host, &key, true, true).await;
            let pid = host.session.snapshot().session.daemon_pid;
            let paused = PausedDaemon(pid);
            // Hold the actual upstream hello, rather than adding a delay or
            // proof bypass to the product. The gateway's local admin stays live.
            // SAFETY: only this test's isolated daemon is signaled.
            assert_eq!(unsafe { libc::kill(pid as libc::pid_t, libc::SIGSTOP) }, 0);
            let (mut ws, _) = begin_auth(
                &host,
                &key,
                ChannelPurpose::Terminal,
                Some(host.workspace()),
            )
            .await;
            assert!(tokio::time::timeout(Duration::from_millis(100), ws.next())
                .await
                .is_err());
            let started = std::time::Instant::now();
            host.command(&[
                "native",
                "revoke",
                &device.to_hex(),
                "--workspace",
                &host.workspace().to_hex(),
            ]);
            assert!(started.elapsed() < Duration::from_secs(2));
            let closed = tokio::time::timeout(Duration::from_secs(2), ws.next())
                .await
                .expect("revoked verification stayed live");
            assert!(!matches!(closed, Some(Ok(Message::Text(_)))), "{closed:?}");
            drop(paused);

            let listing = call(&mut index, 4, "workspaces", json!({})).await;
            assert_eq!(listing["result"]["listing"]["joined"], json!([]));
            let (_denied, denied, _) = auth(
                &host,
                &key,
                ChannelPurpose::Terminal,
                Some(host.workspace()),
            )
            .await;
            assert_eq!(denied["error"]["code"], "unauthorized");
            assert_eq!(host.session.snapshot().session.daemon_pid, pid);
        });
    }

    #[test]
    fn opened_routes_prove_the_live_daemon_and_refuse_changed_registry_identity() {
        let host = Host::start();
        tokio::runtime::Runtime::new().unwrap().block_on(async {
            let key = SigningKey::from_bytes(&[25; 32]);
            let (mut index, _) = enroll(&host, &key, true, true).await;
            let listing = call(&mut index, 3, "workspaces", json!({})).await;
            let generation = listing["result"]["listing"]["joined"][0]["generation"]
                .as_u64()
                .unwrap();
            let snapshot = host.session.snapshot();
            let actual = crew_core::rpc::DaemonRef {
                pid: snapshot.session.daemon_pid,
                started_at: snapshot.session.daemon_started_at,
            };
            for purpose in [
                ChannelPurpose::Workspace,
                ChannelPurpose::Read,
                ChannelPurpose::Action,
                ChannelPurpose::Terminal,
            ] {
                let (mut ws, opened, _) = auth(&host, &key, purpose, Some(host.workspace())).await;
                let opened: GatewayOpened<crew_core::rpc::HelloInfo> =
                    serde_json::from_value(opened["result"].clone()).unwrap();
                assert_eq!(opened.purpose, purpose);
                assert_eq!(
                    opened.route.unwrap(),
                    GatewayRoute {
                        workspace_id: host.workspace(),
                        workspace_generation: generation,
                        daemon: actual,
                    }
                );
                assert_eq!(opened.hello.unwrap().pid, actual.pid);
                // The internal snapshot uses id 0. It must be consumed before
                // opened, even on a purpose that cannot issue device snapshots.
                assert_eq!(
                    call(&mut ws, 0, "ping", json!({})).await["result"]["pong"],
                    true
                );
                if purpose == ChannelPurpose::Terminal {
                    let denied = call(&mut ws, 2, "snapshot", json!({})).await;
                    assert_eq!(denied["error"]["code"], "forbidden");
                }
                ws.close(None).await.unwrap();
            }

            let registry = host
                .session
                .root()
                .join("state/crew/sessions")
                .join(format!("{}.json", host.workspace().to_hex()));
            let original: crew_core::paths::DaemonInfo =
                serde_json::from_slice(&std::fs::read(&registry).unwrap()).unwrap();
            for altered in [
                crew_core::paths::DaemonInfo {
                    started_at: original.started_at + 1,
                    ..original.clone()
                },
                crew_core::paths::DaemonInfo {
                    pid: std::process::id(),
                    pid_start: None,
                    boot_id: None,
                    ..original.clone()
                },
                crew_core::paths::DaemonInfo {
                    workspace: host
                        .session
                        .root()
                        .join("other")
                        .to_string_lossy()
                        .into_owned(),
                    ..original.clone()
                },
            ] {
                crew_core::paths::write_private_file(
                    &registry,
                    &serde_json::to_vec(&altered).unwrap(),
                )
                .unwrap();
                let (mut ws, refused, _) = auth(
                    &host,
                    &key,
                    ChannelPurpose::Terminal,
                    Some(host.workspace()),
                )
                .await;
                assert!(refused.get("result").is_none(), "{refused}");
                assert_eq!(refused["error"]["code"], "unavailable");
                assert!(refused["error"].get("data").is_none());
                assert!(!refused.to_string().contains(&original.state_dir));
                let _ = ws.close(None).await;
            }
            crew_core::paths::write_private_file(
                &registry,
                &serde_json::to_vec(&original).unwrap(),
            )
            .unwrap();
            let (_ws, opened, _) =
                auth(&host, &key, ChannelPurpose::Action, Some(host.workspace())).await;
            assert_eq!(opened["result"]["route"]["daemon"]["pid"], actual.pid);
        });
    }

    #[test]
    fn restart_between_listing_and_open_returns_the_new_verified_incarnation() {
        let host = Host::start();
        tokio::runtime::Runtime::new().unwrap().block_on(async {
            let key = SigningKey::from_bytes(&[26; 32]);
            let (mut index, _) = enroll(&host, &key, true, true).await;
            let listing = call(&mut index, 3, "workspaces", json!({})).await;
            let stale_entry = listing["result"]["listing"]["joined"][0].clone();
            let old: crew_core::rpc::DaemonRef =
                serde_json::from_value(stale_entry["daemon"].clone()).unwrap();
            host.session
                .ok(&["stop", "-C", &host.session.workspace.to_string_lossy()]);
            host.session
                .ok(&["start", "-C", &host.session.workspace.to_string_lossy()]);
            let actual = host.session.snapshot().session;
            let (mut ws, opened, _) =
                auth(&host, &key, ChannelPurpose::Action, Some(host.workspace())).await;
            let route: GatewayRoute = serde_json::from_value(opened["result"]["route"].clone())
                .unwrap_or_else(|e| panic!("missing fresh route: {e}: {opened}"));
            assert_eq!(route.workspace_id, host.workspace());
            assert_eq!(
                route.workspace_generation,
                stale_entry["generation"].as_u64().unwrap()
            );
            assert_eq!(route.daemon.pid, actual.daemon_pid);
            assert_eq!(route.daemon.started_at, actual.daemon_started_at);
            assert_ne!(
                route.daemon, old,
                "an index hint must never become route proof"
            );
            assert_eq!(
                call(&mut ws, 0, "ping", json!({})).await["result"]["pong"],
                true
            );
        });
    }

    #[test]
    fn real_daemon_read_flow_is_scoped_and_denies_raw_control_and_terminal() {
        let host = Host::start();
        tokio::runtime::Runtime::new().unwrap().block_on(async {
            let key = SigningKey::from_bytes(&[7; 32]);
            let (_index, _device) = enroll(&host, &key, false, false).await;
            let (mut ws, opened, _) = auth(
                &host,
                &key,
                ChannelPurpose::Workspace,
                Some(host.workspace()),
            )
            .await;
            assert!(opened["result"]["hello"].is_object(), "{opened}");
            let subscribed = call(&mut ws, 3, "subscribe", json!({})).await;
            assert!(subscribed["result"]["subscribed"].as_bool().unwrap());
            let snapshot = call(
                &mut ws,
                4,
                "snapshot",
                json!({"messages":300,"events":1000}),
            )
            .await;
            assert!(snapshot["result"]["session"].is_object(), "{snapshot}");
            for (method, params) in [
                (
                    "send",
                    json!({"to":"*","body":"must not run","id":"m-native-denied"}),
                ),
                ("shutdown", json!({"stop_agents":true})),
                (
                    "agent_input",
                    json!({"id":"a","text":"command","submit":true}),
                ),
                ("summary", json!({"watch":true})),
                ("git_status", json!({})),
            ] {
                let denied = call(&mut ws, 5, method, params).await;
                assert!(
                    matches!(
                        denied["error"]["code"].as_str(),
                        Some("forbidden" | "invalid")
                    ),
                    "{denied}"
                );
            }
            assert!(host.session.snapshot().session.project.is_none());
            let (_bad, refused, _) = auth(
                &host,
                &key,
                ChannelPurpose::Workspace,
                Some(HexBytes([9; 8])),
            )
            .await;
            assert_eq!(refused["error"]["code"], "unauthorized");
            let (_bad, refused, _) = auth(
                &host,
                &key,
                ChannelPurpose::Terminal,
                Some(host.workspace()),
            )
            .await;
            assert_eq!(refused["error"]["code"], "forbidden");
        });
    }

    #[test]
    fn device_revocation_drains_one_device_and_survives_gateway_restart() {
        let host = Host::start();
        tokio::runtime::Runtime::new().unwrap().block_on(async {
            let a = SigningKey::from_bytes(&[7; 32]);
            let b = SigningKey::from_bytes(&[8; 32]);
            let (_index_a, id_a) = enroll(&host, &a, false, false).await;
            let (_index_b, _) = enroll(&host, &b, false, false).await;
            let (mut a_ws, opened, _) =
                auth(&host, &a, ChannelPurpose::Workspace, Some(host.workspace())).await;
            assert!(opened["result"].is_object());
            let (mut b_ws, opened, _) =
                auth(&host, &b, ChannelPurpose::Workspace, Some(host.workspace())).await;
            assert!(opened["result"].is_object());
            host.command(&["native", "revoke", &id_a.to_hex()]);
            let closed = tokio::time::timeout(Duration::from_secs(2), a_ws.next())
                .await
                .unwrap()
                .unwrap()
                .unwrap();
            assert!(
                matches!(closed,Message::Close(Some(ref frame)) if u16::from(frame.code)==4101),
                "{closed:?}"
            );
            let healthy = call(&mut b_ws, 2, "snapshot", json!({})).await;
            assert!(healthy["result"]["session"].is_object());
            let (_, refused, _) =
                auth(&host, &a, ChannelPurpose::Workspace, Some(host.workspace())).await;
            assert_eq!(refused["error"]["code"], "unauthorized");
            assert!(host.session.snapshot().session.project.is_none());
            host.restart();
            let (_, refused, _) = auth(&host, &a, ChannelPurpose::Index, None).await;
            assert_eq!(refused["error"]["code"], "unauthorized");
            let (mut index, opened, _) = auth(&host, &b, ChannelPurpose::Index, None).await;
            assert!(opened["result"].is_object());
            let listing = call(&mut index, 2, "workspaces", json!({})).await;
            assert_eq!(
                listing["result"]["authority"]["endpoint_id"],
                host.profile.endpoint_id.to_hex()
            );
        });
    }

    #[test]
    fn explicit_terminal_grant_echoes_then_grant_revoke_closes_attachment() {
        let host = Host::start();
        let team = host
            .session
            .team("native-terminal", vec![agent("a", "worker", &[])], vec![]);
        host.session.run(&team);
        host.session.wait("agent ready", |snapshot| {
            find_agent(snapshot, "a").status != crew_core::AgentStatus::Spawning
        });
        tokio::runtime::Runtime::new().unwrap().block_on(async {
            let key = SigningKey::from_bytes(&[11; 32]);
            let (_index, device) = enroll(&host, &key, false, true).await;
            let (mut terminal, opened, _) = auth(
                &host,
                &key,
                ChannelPurpose::Terminal,
                Some(host.workspace()),
            )
            .await;
            assert!(opened["result"].is_object());
            let attached = call(
                &mut terminal,
                2,
                "attach",
                json!({"agent":"a","rows":24,"cols":80}),
            )
            .await;
            assert_eq!(attached["result"]["attached"], "a");
            use base64::Engine;
            let typed = base64::engine::general_purpose::STANDARD.encode("native-terminal-echo");
            terminal
                .send(Message::text(
                    json!({"id":3,"method":"attach_input","params":{"data":typed}}).to_string(),
                ))
                .await
                .unwrap();
            let mut screen = String::new();
            loop {
                let frame = next(&mut terminal).await;
                assert_ne!(frame["id"], 3, "no synthetic success acknowledgement");
                if frame["push"]["kind"] == "pty" {
                    let data = base64::engine::general_purpose::STANDARD
                        .decode(frame["push"]["data"]["data"].as_str().unwrap())
                        .unwrap();
                    screen.push_str(&String::from_utf8_lossy(&data));
                    if screen.contains("native-terminal-echo") {
                        break;
                    }
                }
            }
            host.command(&[
                "native",
                "revoke",
                &device.to_hex(),
                "--workspace",
                &host.workspace().to_hex(),
            ]);
            let close = tokio::time::timeout(Duration::from_secs(2), terminal.next())
                .await
                .unwrap()
                .unwrap()
                .unwrap();
            assert!(
                matches!(close,Message::Close(Some(ref c)) if u16::from(c.code)==4102),
                "{close:?}"
            );
            let (_, denied, _) = auth(
                &host,
                &key,
                ChannelPurpose::Terminal,
                Some(host.workspace()),
            )
            .await;
            assert_eq!(denied["error"]["code"], "unauthorized");
            assert!(!find_agent(&host.session.snapshot(), "a")
                .status
                .is_terminal());
        });
    }

    #[test]
    fn grant_updates_keep_membership_identity_separate_and_revoke_old_channels() {
        let host = Host::start();
        tokio::runtime::Runtime::new().unwrap().block_on(async {
            let key = SigningKey::from_bytes(&[28; 32]);
            let (mut index, device) = enroll(&host, &key, false, false).await;
            let original = call(&mut index, 3, "workspaces", json!({})).await;
            let membership = original["result"]["listing"]["joined"][0]["generation"]
                .as_u64()
                .unwrap();
            let first_grant = original["result"]["grants"][0]["grant_generation"]
                .as_u64()
                .unwrap();
            let (mut old, opened, _) = auth(
                &host,
                &key,
                ChannelPurpose::Workspace,
                Some(host.workspace()),
            )
            .await;
            assert_eq!(
                opened["result"]["route"]["workspace_generation"],
                membership
            );
            host.command(&[
                "native",
                "grant",
                &device.to_hex(),
                "--workspace",
                &host.session.workspace.to_string_lossy(),
                "--control",
            ]);
            let closed = tokio::time::timeout(Duration::from_secs(2), old.next())
                .await
                .unwrap()
                .unwrap()
                .unwrap();
            assert!(matches!(closed, Message::Close(Some(ref c)) if u16::from(c.code) == 4102));
            let changed = call(&mut index, 4, "workspaces", json!({})).await;
            assert!(
                changed["result"]["listing"]["rev"].as_u64().unwrap()
                    > original["result"]["listing"]["rev"].as_u64().unwrap()
            );
            assert_eq!(
                changed["result"]["listing"]["joined"][0]["generation"],
                membership
            );
            let next_grant = changed["result"]["grants"][0]["grant_generation"]
                .as_u64()
                .unwrap();
            assert!(next_grant > first_grant);
            let (mut fresh, opened, _) = auth(
                &host,
                &key,
                ChannelPurpose::Workspace,
                Some(host.workspace()),
            )
            .await;
            assert_eq!(
                opened["result"]["route"]["workspace_generation"],
                membership
            );
            assert_eq!(opened["result"]["grant"]["grant_generation"], next_grant);
            assert_eq!(
                opened["result"]["route"]["daemon"],
                changed["result"]["listing"]["joined"][0]["daemon"]
            );
            assert!(
                call(&mut fresh, 2, "snapshot", json!({})).await["result"]["session"].is_object()
            );

            host.command(&[
                "native",
                "revoke",
                &device.to_hex(),
                "--workspace",
                &host.workspace().to_hex(),
            ]);
            let closed = tokio::time::timeout(Duration::from_secs(2), fresh.next())
                .await
                .unwrap()
                .unwrap()
                .unwrap();
            assert!(matches!(closed, Message::Close(Some(ref c)) if u16::from(c.code) == 4102));
            let removed = call(&mut index, 5, "workspaces", json!({})).await;
            assert_eq!(removed["result"]["listing"]["joined"], json!([]));
            assert_eq!(removed["result"]["grants"], json!([]));
            let (_, denied, _) = auth(
                &host,
                &key,
                ChannelPurpose::Workspace,
                Some(host.workspace()),
            )
            .await;
            assert_eq!(denied["error"]["code"], "unauthorized");

            host.command(&[
                "native",
                "grant",
                &device.to_hex(),
                "--workspace",
                &host.session.workspace.to_string_lossy(),
                "--control",
            ]);
            // Revoking the final grant retires its summary watch. Regrant
            // discovery is asynchronous, just as it is after enrollment.
            let deadline = tokio::time::Instant::now() + Duration::from_secs(5);
            let rejoined = loop {
                let listing = call(&mut index, 6, "workspaces", json!({})).await;
                if listing["result"]["listing"]["joined"][0]["state"] == "running" {
                    break listing;
                }
                assert!(tokio::time::Instant::now() < deadline, "{listing}");
                tokio::time::sleep(Duration::from_millis(50)).await;
            };
            assert_eq!(
                rejoined["result"]["listing"]["joined"][0]["generation"],
                membership
            );
            let third_grant = rejoined["result"]["grants"][0]["grant_generation"]
                .as_u64()
                .unwrap();
            assert!(third_grant > next_grant);
            for purpose in [ChannelPurpose::Workspace, ChannelPurpose::Action] {
                let (mut ws, opened, _) = auth(&host, &key, purpose, Some(host.workspace())).await;
                assert_eq!(
                    opened["result"]["route"]["workspace_generation"],
                    membership
                );
                assert_eq!(opened["result"]["grant"]["grant_generation"], third_grant);
                assert_eq!(
                    opened["result"]["route"]["daemon"],
                    rejoined["result"]["listing"]["joined"][0]["daemon"]
                );
                assert_eq!(
                    call(&mut ws, 0, "ping", json!({})).await["result"]["pong"],
                    true
                );
            }
        });
    }

    #[test]
    fn explicit_control_messages_are_allowed_but_do_not_grant_terminal() {
        let host = Host::start();
        host.session.run(&host.session.team(
            "native-control",
            vec![agent("a", "worker", &[])],
            vec![],
        ));
        tokio::runtime::Runtime::new().unwrap().block_on(async {
            let key = SigningKey::from_bytes(&[12; 32]);
            let (_index, device) = enroll(&host, &key, true, false).await;
            let (mut action, opened, _) =
                auth(&host, &key, ChannelPurpose::Action, Some(host.workspace())).await;
            assert!(opened["result"].is_object());
            let sent = call(
                &mut action,
                2,
                "send",
                json!({"to":"a","body":"native control message","id":"m-native-control"}),
            )
            .await;
            assert!(sent["result"].is_object(), "{sent}");
            assert!(host
                .session
                .messages()
                .iter()
                .any(|m| m.body == "native control message"));
            let denied = call(
                &mut action,
                3,
                "agent_input",
                json!({"id":"a","text":"must not run","submit":true}),
            )
            .await;
            assert_eq!(denied["error"]["code"], "forbidden");
            let current_generation = opened["result"]["grant"]["grant_generation"]
                .as_u64()
                .unwrap();
            host.command(&[
                "native",
                "grant",
                &device.to_hex(),
                "--workspace",
                &host.session.workspace.to_string_lossy(),
            ]);
            let (mut read, new, _) = auth(
                &host,
                &key,
                ChannelPurpose::Workspace,
                Some(host.workspace()),
            )
            .await;
            assert!(
                new["result"]["grant"]["grant_generation"].as_u64().unwrap() > current_generation
            );
            let state = call(&mut read, 2, "snapshot", json!({})).await;
            assert!(state["result"]["session"].is_object());
        });
    }

    #[test]
    fn wrong_pin_and_replayed_signature_never_authenticate() {
        let host = Host::start();
        tokio::runtime::Runtime::new().unwrap().block_on(async {
            assert!(connect(&host, HexBytes([0; 32])).await.is_err());
            assert!(tls_named(
                &host,
                host.profile.tls_leaf_sha256,
                rustls::pki_types::ServerName::try_from("wrong.example".to_owned()).unwrap()
            )
            .await
            .is_err());
            let key = SigningKey::from_bytes(&[7; 32]);
            let (_index, _) = enroll(&host, &key, false, false).await;
            let (_ws, opened, params) = auth(
                &host,
                &key,
                ChannelPurpose::Workspace,
                Some(host.workspace()),
            )
            .await;
            assert!(opened["result"].is_object());
            let (mut ws, _) = connect(&host, host.profile.tls_leaf_sha256).await.unwrap();
            ws.send(Message::text(
                serde_json::to_string(&HandshakeRequest {
                    id: 1,
                    call: HandshakeCall::NativeAuth(params),
                })
                .unwrap(),
            ))
            .await
            .unwrap();
            let rejected = result(&mut ws, 1).await;
            assert_eq!(rejected["error"]["code"], "unauthorized");
            let out = host
                .session
                .command()
                .env(
                    "CREW_IDENTITY",
                    host.session.root().join("missing.identity"),
                )
                .args(["native", "enroll"])
                .output()
                .unwrap();
            assert!(!out.status.success());
            assert!(String::from_utf8_lossy(&out.stderr)
                .contains("operator commands are not available"));
        });
    }

    #[test]
    fn expired_host_certificate_is_rejected_before_device_proof() {
        let host = Host::start_certificate(include_bytes!("fixtures/native/expired.pem"));
        tokio::runtime::Runtime::new().unwrap().block_on(async {
            let error = connect(&host, host.profile.tls_leaf_sha256)
                .await
                .expect_err("expired certificate rejected");
            assert!(
                error.to_string().to_ascii_lowercase().contains("expired"),
                "{error}"
            );
        });
        assert_eq!(host.command(&["native", "devices"]), json!([]));
    }

    #[test]
    fn no_assets_origin_queries_or_pre_auth_metadata_are_served() {
        let host = Host::start();
        tokio::runtime::Runtime::new().unwrap().block_on(async{
            use tokio::io::{AsyncReadExt,AsyncWriteExt};
            for target in ["/","/native/ws?x=1","/version"]{
                let mut stream=tls_stream(&host,host.profile.tls_leaf_sha256).await.unwrap();
                stream.write_all(format!("GET {target} HTTP/1.1\r\nHost: 127.0.0.1:{}\r\n\r\n",host.port).as_bytes()).await.unwrap();
                let mut bytes=Vec::new();stream.read_to_end(&mut bytes).await.unwrap();let text=String::from_utf8_lossy(&bytes);
                assert!(text.starts_with("HTTP/1.1 400"),"{text}");assert!(!text.contains("protocol"));assert!(!text.contains(".crew"));
            }
            let mut stream=tls_stream(&host,host.profile.tls_leaf_sha256).await.unwrap();
            stream.write_all(format!("GET /native/ws HTTP/1.1\r\nHost: 127.0.0.1:{}\r\nOrigin: https://evil.example\r\nUpgrade: websocket\r\nConnection: Upgrade\r\nSec-WebSocket-Version: 13\r\nSec-WebSocket-Key: dGhlIHNhbXBsZSBub25jZQ==\r\n\r\n",host.port).as_bytes()).await.unwrap();
            let mut bytes=Vec::new();stream.read_to_end(&mut bytes).await.unwrap();assert!(String::from_utf8_lossy(&bytes).starts_with("HTTP/1.1 403"));
            let(mut ws,_)=connect(&host,host.profile.tls_leaf_sha256).await.unwrap();
            ws.send(Message::text(json!({"id":1,"method":"native_status","params":{},"token":"SYNTHETIC-DO-NOT-ECHO"}).to_string())).await.unwrap();
            let frame=next(&mut ws).await;assert_eq!(frame["error"]["code"],"invalid");assert!(!frame.to_string().contains("SYNTHETIC-DO-NOT-ECHO"));assert!(frame["error"]["data"].is_null());
        });
    }

    #[test]
    fn canceled_enrollment_is_unapprovable_and_private_key_never_reaches_registry() {
        let host = Host::start();
        tokio::runtime::Runtime::new().unwrap().block_on(async {
            host.command(&["native", "enroll"]);
            let key = SigningKey::from_bytes(&[13; 32]);
            let (mut ws, nonce) = connect(&host, host.profile.tls_leaf_sha256).await.unwrap();
            let mut p = EnrollParams {
                native_protocol: 1,
                protocol: 6,
                public_key: HexBytes(key.verifying_key().to_bytes()),
                client_nonce: HexBytes([5; 32]),
                signature: HexBytes([0; 64]),
                label: "Cancel device".into(),
            };
            p.signature = HexBytes(
                key.sign(&enroll_transcript(
                    host.profile.endpoint_id,
                    host.profile.tls_leaf_sha256,
                    nonce,
                    &p,
                ))
                .to_bytes(),
            );
            ws.send(Message::text(
                serde_json::to_string(&HandshakeRequest {
                    id: 1,
                    call: HandshakeCall::NativeEnroll(p),
                })
                .unwrap(),
            ))
            .await
            .unwrap();
            let notice: NoticeFrame = serde_json::from_value(next(&mut ws).await).unwrap();
            let Notice::EnrollmentAccepted { intent } = notice.native else {
                panic!("accepted required")
            };
            ws.close(None).await.unwrap();
            drop(ws);
            let deadline = std::time::Instant::now() + Duration::from_secs(2);
            while !host
                .command(&["native", "pending"])
                .as_array()
                .unwrap()
                .is_empty()
            {
                assert!(std::time::Instant::now() < deadline);
                tokio::time::sleep(Duration::from_millis(20)).await;
            }
            let key_hash =
                HexBytes::<32>(Sha256::digest(key.verifying_key().as_bytes()).into()).to_hex();
            let output = host.session.crew(&[
                "native",
                "approve",
                &intent.enrollment_id.to_hex(),
                "--code",
                &intent.comparison_code.to_hex(),
                "--key-hash",
                &key_hash,
                "--workspace",
                &host.session.workspace.to_string_lossy(),
            ]);
            assert!(!output.status.success());
            let (_index, _) = enroll(&host, &key, false, false).await;
            let registry =
                std::fs::read_to_string(host.session.root().join("run/crew/native/devices.json"))
                    .unwrap();
            assert!(!registry.contains(&HexBytes(key.to_bytes()).to_hex()));
            let token =
                std::fs::read_to_string(host.session.root().join("run/crew/native.token")).unwrap();
            assert!(!host
                .command(&["native", "status"])
                .to_string()
                .contains(token.trim()));
        });
    }

    #[test]
    fn literal_line_break_cannot_forward_two_requests_and_daemon_restart_resynchronizes() {
        let host = Host::start();
        tokio::runtime::Runtime::new().unwrap().block_on(async{
            let key=SigningKey::from_bytes(&[14;32]);let(_index,_)=enroll(&host,&key,false,false).await;
            let(mut ws,_,_)=auth(&host,&key,ChannelPurpose::Workspace,Some(host.workspace())).await;
            ws.send(Message::text("{\"id\":2,\"method\":\"snapshot\",\"params\":{}}\n{\"id\":3,\"method\":\"shutdown\",\"params\":{}}")).await.unwrap();
            let invalid=next(&mut ws).await;assert_eq!(invalid["error"]["code"],"invalid");
            host.session.ok(&["stop","-C",&host.session.workspace.to_string_lossy()]);
            host.session.ok(&["start","-C",&host.session.workspace.to_string_lossy()]);
            let deadline=tokio::time::Instant::now()+Duration::from_secs(10);
            loop {let(mut fresh,opened,_)=auth(&host,&key,ChannelPurpose::Workspace,Some(host.workspace())).await;if opened["result"].is_object(){let snapshot=call(&mut fresh,2,"snapshot",json!({})).await;assert!(snapshot["result"]["session"].is_object());break;}assert!(tokio::time::Instant::now()<deadline);tokio::time::sleep(Duration::from_millis(100)).await;}
        });
    }

    #[test]
    fn device_expiry_closes_an_authenticated_session() {
        let host = Host::start();
        tokio::runtime::Runtime::new().unwrap().block_on(async {
            let key = SigningKey::from_bytes(&[15; 32]);
            let (mut index, _) = enroll_lifetime(&host, &key, false, false, 2).await;
            let deadline = tokio::time::Instant::now() + Duration::from_secs(4);
            loop {
                let message = tokio::time::timeout_at(deadline, index.next())
                    .await
                    .unwrap()
                    .unwrap()
                    .unwrap();
                match message {
                    Message::Close(Some(c)) => {
                        assert_eq!(u16::from(c.code), 4101);
                        break;
                    }
                    Message::Ping(data) => index.send(Message::Pong(data)).await.unwrap(),
                    _ => {}
                }
            }
            let (_, denied, _) = auth(&host, &key, ChannelPurpose::Index, None).await;
            assert_eq!(denied["error"]["code"], "unauthorized");
        });
    }

    #[test]
    fn private_paths_are_hidden_from_real_sandbox_with_both_state_home_placements() {
        let binary = std::path::Path::new(CREW).canonicalize().unwrap();
        if let Err(why) = crew::sandbox::probe::probe(&binary, std::env::var_os("PATH").as_ref()) {
            if std::env::var("CREW_TEST_SANDBOX").as_deref() == Ok("require") {
                panic!("native sandbox regression required: {why:?}");
            }
            eprintln!("skipped native sandbox regression: {why:?}");
            return;
        }
        for inside in [false, true] {
            let session = Session::new();
            std::fs::create_dir_all(&session.workspace).unwrap();
            let home = session.root().join("operator-home");
            std::fs::create_dir_all(&home).unwrap();
            let state = if inside {
                home.join("state")
            } else {
                session.root().join("external-state")
            };
            let runtime = session.root().join("run/crew");
            let native = runtime.join("native");
            crew_core::paths::ensure_private_dir(&native).unwrap();
            let targets = [
                native.join("devices.json"),
                native.join("key.pem"),
                runtime.join("native.token"),
            ];
            for path in &targets {
                crew_core::paths::write_private_file(path, b"native-private-sentinel").unwrap();
                assert_eq!(std::fs::read(path).unwrap(), b"native-private-sentinel");
            }
            let output_home = session.workspace.join(".crew/agents/a/rw/home");
            let mut script = String::new();
            for path in &targets {
                script.push_str(&format!(
                    "if cat '{}' >/dev/null 2>&1; then echo LEAK; else echo DENIED; fi\n",
                    path.display()
                ));
            }
            let actor=crew_core::config::AgentSpec{id:"a".into(),command:vec!["sh".into(),"-c".into(),format!("{{\n{script}\n}} > '{0}/report' 2>&1; touch '{0}/report.done'; exec sleep 600",output_home.display())],..Default::default()};
            let mut team = session.team("native-private", vec![actor], vec![]);
            team.workspace.sandbox.mode = crew_core::config::SandboxMode::Required;
            std::fs::write(session.root().join("team.yaml"), team.to_yaml().unwrap()).unwrap();
            let output = session
                .command()
                .env("HOME", &home)
                .env("XDG_STATE_HOME", &state)
                .args(["run", "team.yaml", "--no-attach"])
                .output()
                .unwrap();
            assert!(
                output.status.success(),
                "{}",
                String::from_utf8_lossy(&output.stderr)
            );
            session.wait_for("native sandbox report", || {
                output_home.join("report.done").exists()
            });
            let report = std::fs::read_to_string(output_home.join("report")).unwrap();
            assert!(!report.contains("LEAK"), "{report}");
            assert_eq!(report.matches("DENIED").count(), 3, "{report}");
            session.stop();
        }
    }

    #[test]
    fn two_devices_receive_only_their_selected_workspace_metadata() {
        let host = Host::start();
        let other = host.session.root().join("other-workspace");
        std::fs::create_dir_all(&other).unwrap();
        host.session.ok(&["start", "-C", &other.to_string_lossy()]);
        tokio::runtime::Runtime::new().unwrap().block_on(async {
            let a = SigningKey::from_bytes(&[16; 32]);
            let b = SigningKey::from_bytes(&[17; 32]);
            let (mut a_index, _) =
                enroll_workspace(&host, &a, false, false, 2592000, &host.session.workspace).await;
            let (mut b_index, _) = enroll_workspace(&host, &b, false, false, 2592000, &other).await;
            let a_list = call(&mut a_index, 3, "workspaces", json!({})).await;
            let b_list = call(&mut b_index, 3, "workspaces", json!({})).await;
            assert_eq!(
                a_list["result"]["listing"]["joined"]
                    .as_array()
                    .unwrap()
                    .len(),
                1
            );
            assert_eq!(
                b_list["result"]["listing"]["joined"]
                    .as_array()
                    .unwrap()
                    .len(),
                1
            );
            assert!(!a_list
                .to_string()
                .contains(&other.to_string_lossy().into_owned()));
            assert!(!b_list
                .to_string()
                .contains(&host.session.workspace.to_string_lossy().into_owned()));
            assert_eq!(a_list["result"]["listing"]["others"], json!([]));
            assert_eq!(b_list["result"]["listing"]["others"], json!([]));
            let other_id =
                WorkspaceId::from_hex(&crew_core::paths::session_id(&other.join(".crew"))).unwrap();
            let (_, denied, _) = auth(&host, &a, ChannelPurpose::Workspace, Some(other_id)).await;
            assert_eq!(denied["error"]["code"], "unauthorized");
        });
        host.session.ok(&["stop", "-C", &other.to_string_lossy()]);
    }

    #[test]
    fn failed_authority_save_closes_sessions_and_never_reports_revocation_success() {
        let host = Host::start();
        tokio::runtime::Runtime::new().unwrap().block_on(async {
            let key = SigningKey::from_bytes(&[18; 32]);
            let (_index, device) = enroll(&host, &key, false, false).await;
            let (mut ws, opened, _) = auth(
                &host,
                &key,
                ChannelPurpose::Workspace,
                Some(host.workspace()),
            )
            .await;
            assert!(opened["result"].is_object());
            let registry = host.session.root().join("run/crew/native/devices.json");
            let backup = registry.with_extension("backup");
            std::fs::rename(&registry, &backup).unwrap();
            std::fs::create_dir(&registry).unwrap();
            let failed = host.session.crew(&["native", "revoke", &device.to_hex()]);
            assert!(!failed.status.success());
            let closed = tokio::time::timeout(Duration::from_secs(3), ws.next())
                .await
                .unwrap();
            assert!(matches!(
                closed,
                Some(Ok(Message::Close(_))) | None | Some(Err(_))
            ));
            std::fs::remove_dir(&registry).unwrap();
            std::fs::rename(&backup, &registry).unwrap();
            let saved: Value =
                serde_json::from_str(&std::fs::read_to_string(registry).unwrap()).unwrap();
            assert_eq!(
                saved["devices"][device.to_hex()]["active"],
                true,
                "failed save never persisted a successful revoke"
            );
        });
    }

    #[test]
    fn subscribe_snapshot_barrier_and_slow_reader_preserve_healthy_stream() {
        let host = Host::start();
        let mut team = host
            .session
            .team("native-stream", vec![agent("a", "worker", &[])], vec![]);
        team.limits.max_message_bytes = 64 * 1024;
        host.session.run(&team);
        tokio::runtime::Runtime::new().unwrap().block_on(async {
            let key = SigningKey::from_bytes(&[19; 32]);
            let (_index, _) = enroll(&host, &key, true, false).await;
            let (mut state, _, _) = auth(
                &host,
                &key,
                ChannelPurpose::Workspace,
                Some(host.workspace()),
            )
            .await;
            state
                .send(Message::text(
                    json!({"id":2,"method":"subscribe","params":{}}).to_string(),
                ))
                .await
                .unwrap();
            let subscribed = next(&mut state).await;
            assert_eq!(subscribed["id"], 2);
            assert_eq!(subscribed["result"]["subscribed"], true);
            let (mut action, _, _) =
                auth(&host, &key, ChannelPurpose::Action, Some(host.workspace())).await;
            call(
                &mut action,
                2,
                "send",
                json!({"to":"a","body":"before snapshot","id":"m-before-snapshot"}),
            )
            .await;
            let snapshot = call(
                &mut state,
                3,
                "snapshot",
                json!({"messages":300,"events":1000}),
            )
            .await;
            assert!(snapshot["result"]["messages"]
                .as_array()
                .unwrap()
                .iter()
                .any(|m| m["id"] == "m-before-snapshot"));
            call(
                &mut action,
                3,
                "send",
                json!({"to":"a","body":"after snapshot","id":"m-after-snapshot"}),
            )
            .await;
            loop {
                let frame = next(&mut state).await;
                if frame["push"]["kind"] == "message"
                    && frame["push"]["data"]["id"] == "m-after-snapshot"
                {
                    break;
                }
            }
            let (mut slow, _, _) = auth(
                &host,
                &key,
                ChannelPurpose::Workspace,
                Some(host.workspace()),
            )
            .await;
            call(&mut slow, 2, "subscribe", json!({})).await;
            let body = "x".repeat(60 * 1024);
            for n in 0..24 {
                let sent = call(
                    &mut action,
                    10 + n,
                    "send",
                    json!({"to":"a","body":body,"id":format!("m-slow-{n}")}),
                )
                .await;
                assert!(sent["result"].is_object(), "{sent}");
            }
            // A second subscription that is not being drained cannot block
            // independent reads. Its bounded writer may disconnect/resync.
            let (mut reader, _, _) =
                auth(&host, &key, ChannelPurpose::Read, Some(host.workspace())).await;
            let fresh = call(&mut reader, 2, "snapshot", json!({"messages":1,"events":1})).await;
            assert!(fresh["result"]["session"].is_object());
            drop(slow);
            drop(state);
        });
    }

    #[test]
    fn unauthenticated_connection_budget_leaves_existing_device_and_control_usable() {
        let host = Host::start();
        tokio::runtime::Runtime::new().unwrap().block_on(async {
            let key = SigningKey::from_bytes(&[20; 32]);
            let (_index, _) = enroll(&host, &key, false, false).await;
            let (mut state, opened, _) = auth(
                &host,
                &key,
                ChannelPurpose::Workspace,
                Some(host.workspace()),
            )
            .await;
            assert!(opened["result"].is_object());
            let mut pending = Vec::new();
            for _ in 0..4 {
                pending.push(TcpStream::connect(("127.0.0.1", host.port)).await.unwrap());
            }
            tokio::time::sleep(Duration::from_millis(50)).await;
            let mut extra = TcpStream::connect(("127.0.0.1", host.port)).await.unwrap();
            use tokio::io::AsyncReadExt;
            let mut byte = [0];
            let read = tokio::time::timeout(Duration::from_secs(1), extra.read(&mut byte))
                .await
                .unwrap();
            assert!(
                matches!(read, Ok(0) | Err(_)),
                "fifth unauthenticated connection must not enter TLS"
            );
            let snapshot = call(&mut state, 2, "snapshot", json!({})).await;
            assert!(snapshot["result"]["session"].is_object());
            assert_eq!(
                host.command(&["native", "devices"])
                    .as_array()
                    .unwrap()
                    .len(),
                1
            );
            drop(pending);
        });
    }

    /// Read pushes and replies until a listing names exactly these workspaces.
    async fn listing_until(ws: &mut Ws, expected: &[WorkspaceId]) -> Value {
        let mut wanted: Vec<_> = expected.iter().map(|id| id.to_hex()).collect();
        wanted.sort();
        let deadline = tokio::time::Instant::now() + Duration::from_secs(10);
        loop {
            assert!(tokio::time::Instant::now() < deadline, "listing {wanted:?}");
            let frame = next(ws).await;
            let Some(listing) = frame
                .get("native_listing")
                .or_else(|| frame.get("result").filter(|r| r.get("listing").is_some()))
            else {
                continue;
            };
            let mut joined: Vec<_> = listing["listing"]["joined"]
                .as_array()
                .unwrap()
                .iter()
                .map(|entry| entry["id"].as_str().unwrap().to_owned())
                .collect();
            joined.sort();
            if joined == wanted {
                return listing.clone();
            }
        }
    }

    #[test]
    fn workspace_revoke_after_restart_keeps_the_device_index_and_remaining_workspace() {
        let host = Host::start();
        let other = host.session.root().join("other-workspace");
        std::fs::create_dir_all(&other).unwrap();
        host.session.ok(&["start", "-C", &other.to_string_lossy()]);
        let other_id =
            WorkspaceId::from_hex(&crew_core::paths::session_id(&other.join(".crew"))).unwrap();
        tokio::runtime::Runtime::new().unwrap().block_on(async {
            let key = SigningKey::from_bytes(&[21; 32]);
            let (_enrolled, device) = enroll(&host, &key, false, true).await;
            host.command(&[
                "native",
                "grant",
                &device.to_hex(),
                "--workspace",
                &other.to_string_lossy(),
            ]);
            host.command(&[
                "native",
                "grant",
                &device.to_hex(),
                "--workspace",
                &host.session.workspace.to_string_lossy(),
                "--control",
                "--terminal",
            ]);
            host.restart();

            // The same key authenticates the restarted gateway's index and
            // a route to the selected workspace.
            let (mut index, opened, _) = auth(&host, &key, ChannelPurpose::Index, None).await;
            assert!(opened["result"].is_object(), "{opened}");
            let restarted = call(&mut index, 2, "workspaces", json!({})).await;
            let rev = restarted["result"]["listing"]["rev"].as_u64().unwrap();
            assert_eq!(
                restarted["result"]["listing"]["joined"]
                    .as_array()
                    .unwrap()
                    .len(),
                2
            );
            let (mut route, opened, _) =
                auth(&host, &key, ChannelPurpose::Workspace, Some(host.workspace())).await;
            assert!(opened["result"]["route"].is_object(), "{opened}");

            host.command(&[
                "native",
                "revoke",
                &device.to_hex(),
                "--workspace",
                &host.workspace().to_hex(),
            ]);
            let closed = tokio::time::timeout(Duration::from_secs(5), route.next())
                .await
                .unwrap();
            assert!(
                matches!(closed, Some(Ok(Message::Close(Some(ref frame)))) if u16::from(frame.code) == 4102),
                "{closed:?}"
            );
            // The index stays open and pushes a newer listing of what remains.
            let remaining = listing_until(&mut index, &[other_id]).await;
            assert!(remaining["listing"]["rev"].as_u64().unwrap() > rev);
            assert_eq!(remaining["grants"][0]["workspace_id"], other_id.to_hex());
            let ping = call(&mut index, 3, "ping", json!({})).await;
            assert_eq!(ping["result"]["pong"], true);

            // A fresh device authentication still succeeds. Only the revoked
            // workspace route is refused, with the generic pre-route answer.
            let (_fresh, opened, _) = auth(&host, &key, ChannelPurpose::Index, None).await;
            assert!(opened["result"].is_object(), "{opened}");
            let (_, refused, _) =
                auth(&host, &key, ChannelPurpose::Workspace, Some(host.workspace())).await;
            assert_eq!(refused["error"]["code"], "unauthorized");
            let (mut kept, opened, _) =
                auth(&host, &key, ChannelPurpose::Workspace, Some(other_id)).await;
            assert!(opened["result"]["route"].is_object(), "{opened}");
            let snapshot = call(&mut kept, 2, "snapshot", json!({})).await;
            assert!(snapshot["result"]["session"].is_object());
            assert_eq!(
                host.command(&["native", "devices"])
                    .as_array()
                    .unwrap()
                    .len(),
                1
            );
        });
        host.session.ok(&["stop", "-C", &other.to_string_lossy()]);
    }
}
