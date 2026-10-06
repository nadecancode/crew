use std::sync::Arc;
use std::time::Duration;

use crew_core::rpc::{Call, Request, RpcError, ServerFrame};
use crew_protocol::native::{
    ChannelPurpose, GatewayOpened, HandshakeCall, HandshakeRequest, Notice, NoticeFrame,
};
use futures_util::{SinkExt, StreamExt};
use serde::Serialize;
use tokio::io::AsyncWriteExt;
use tokio::net::TcpStream;
use tokio::sync::{oneshot, watch};
use tokio_rustls::server::TlsStream;
use tokio_tungstenite::tungstenite::{
    protocol::{Role, WebSocketConfig},
    Message,
};
use tokio_tungstenite::WebSocketStream;

use super::service::{lock, Connection, Gateway, NativeHello};
use super::store::Device;
use crate::web::daemons;

type Ws = WebSocketStream<TlsStream<TcpStream>>;

async fn send<T: Serialize>(ws: &mut Ws, value: &T, cancel: &mut watch::Receiver<bool>) -> bool {
    if *cancel.borrow() {
        return false;
    }
    let Ok(text) = serde_json::to_string(value) else {
        return false;
    };
    if text.len() > crew_core::MAX_FRAME_BYTES {
        return false;
    }
    tokio::select! {
        result = tokio::time::timeout(Duration::from_secs(3), ws.send(Message::text(text))) => matches!(result, Ok(Ok(()))),
        _ = cancel.changed() => false,
    }
}

async fn reject(ws: &mut Ws, id: u64, code: &str, cancel: &mut watch::Receiver<bool>) {
    let _ = send(
        ws,
        &ServerFrame::err(id, RpcError::new(code, "native request unavailable")),
        cancel,
    )
    .await;
}

pub(crate) async fn serve(
    gateway: Arc<Gateway>,
    stream: TcpStream,
    connection: Arc<Connection>,
    unauth: tokio::sync::OwnedSemaphorePermit,
) {
    let mut cancel = connection.cancel.subscribe();
    if *cancel.borrow() {
        return;
    }
    let setup = async {
        let mut tls = tokio_rustls::TlsAcceptor::from(gateway.identity.config.clone())
            .accept(stream)
            .await
            .ok()?;
        let rest = super::http::upgrade_head(&mut tls, &gateway.identity.host).await?;
        let config = WebSocketConfig::default()
            .max_message_size(Some(4096))
            .max_frame_size(Some(4096));
        Some(WebSocketStream::from_partially_read(tls, rest, Role::Server, Some(config)).await)
    };
    let socket = tokio::select! { socket = tokio::time::timeout(Duration::from_secs(5), setup) => socket.ok().flatten(), _ = cancel.changed() => None };
    let Some(mut ws) = socket else {
        return;
    };
    let Ok(nonce) = super::fresh() else {
        return;
    };
    if !send(
        &mut ws,
        &NoticeFrame {
            native: Notice::Challenge { nonce },
        },
        &mut cancel,
    )
    .await
    {
        return;
    }
    let message = tokio::select! { message = tokio::time::timeout(Duration::from_secs(5), ws.next()) => message.ok().flatten(), _ = cancel.changed() => None };
    let Some(Ok(Message::Text(text))) = message else {
        return;
    };
    if text.len() > 4096 || text.contains(['\n', '\r']) {
        reject(&mut ws, 0, "invalid", &mut cancel).await;
        return;
    }
    let Ok(request) = serde_json::from_str::<HandshakeRequest>(&text) else {
        reject(&mut ws, 0, "invalid", &mut cancel).await;
        return;
    };
    let source = {
        lock(&gateway.state)
            .connections
            .get(&connection.id)
            .map(|e| e.source)
    };
    let Some(source) = source else {
        return;
    };
    let (device, purpose, workspace) = match request.call {
        HandshakeCall::NativeAuth(params) => {
            let endpoint = lock(&gateway.state).store.registry.endpoint_id;
            if !super::auth::verify_auth(endpoint, gateway.identity.leaf_hash, nonce, &params) {
                reject(&mut ws, request.id, "unauthorized", &mut cancel).await;
                return;
            }
            let Some(device) = gateway.bind(connection.id, params.public_key, params.workspace)
            else {
                reject(&mut ws, request.id, "unauthorized", &mut cancel).await;
                return;
            };
            if params.native_protocol != 1 || params.protocol != crew_core::rpc::PROTOCOL_VERSION {
                reject(&mut ws, request.id, "protocol", &mut cancel).await;
                return;
            }
            (device, params.purpose, params.workspace)
        }
        HandshakeCall::NativeEnroll(params) => {
            if !lock(&gateway.enroll_attempts).admit(source)
                || params.native_protocol != 1
                || params.protocol != 6
            {
                reject(&mut ws, request.id, "unauthorized", &mut cancel).await;
                return;
            }
            let (approval, approved) = oneshot::channel();
            *lock(&connection.approval) = Some(approval);
            let accepted = {
                let mut state = lock(&gateway.state);
                if state.stopping || state.store.registry.by_key(params.public_key).is_some() {
                    Err("enrollment unavailable")
                } else {
                    let endpoint = state.store.registry.endpoint_id;
                    state.enrollment.begin(
                        endpoint,
                        gateway.identity.leaf_hash,
                        nonce,
                        &params,
                        connection.id,
                        super::now_ms(),
                    )
                }
            };
            let Ok(accepted) = accepted else {
                reject(&mut ws, request.id, "unauthorized", &mut cancel).await;
                return;
            };
            if !send(
                &mut ws,
                &NoticeFrame {
                    native: Notice::EnrollmentAccepted {
                        intent: accepted.clone(),
                    },
                },
                &mut cancel,
            )
            .await
            {
                return;
            }
            let deadline = tokio::time::Instant::now()
                + Duration::from_millis(
                    accepted.expires_at.saturating_sub(super::now_ms()).max(0) as u64
                );
            let mut approved = approved;
            let device_id = loop {
                tokio::select! {
                    id = &mut approved => break id.ok(),
                    _ = cancel.changed() => break None,
                    _ = tokio::time::sleep_until(deadline) => break None,
                    message = ws.next() => match message {
                        Some(Ok(Message::Ping(data))) => { if ws.send(Message::Pong(data)).await.is_err() { break None; } },
                        Some(Ok(Message::Pong(_))) => {},
                        _ => break None,
                    }
                }
            };
            let Some(device_id) = device_id else {
                return;
            };
            let key = {
                lock(&gateway.state)
                    .store
                    .registry
                    .devices
                    .get(&device_id)
                    .map(|d| d.public_key)
            };
            let Some(device) = key.and_then(|key| gateway.bind(connection.id, key, None)) else {
                return;
            };
            (device, ChannelPurpose::Index, None)
        }
    };
    let Ok(authenticated) = gateway.authenticated.clone().try_acquire_owned() else {
        reject(&mut ws, request.id, "unavailable", &mut cancel).await;
        return;
    };
    drop(unauth);
    // A connector sends no application frames before its opened reply.
    // Retire the bounded pre-auth codec; bytes pipelined into that codec
    // before authentication are discarded, never forwarded as requests.
    ws = WebSocketStream::from_raw_socket(
        ws.into_inner(),
        Role::Server,
        Some(
            WebSocketConfig::default()
                .max_message_size(Some(crew_core::MAX_FRAME_BYTES))
                .max_frame_size(Some(crew_core::MAX_FRAME_BYTES)),
        ),
    )
    .await;
    if purpose == ChannelPurpose::Index {
        let hello: NativeHello = GatewayOpened {
            native_protocol: 1,
            protocol: 6,
            authority: gateway.authority(&device),
            purpose,
            grant: None,
            route: None,
            hello: None,
        };
        if send(
            &mut ws,
            &ServerFrame::ok(request.id, serde_json::to_value(hello).unwrap()),
            &mut cancel,
        )
        .await
        {
            index(gateway, ws, device, cancel, connection.clone()).await;
        }
    } else if let Some(workspace) = workspace {
        workspace_channel(
            gateway,
            ws,
            device,
            (purpose, workspace),
            request.id,
            cancel,
            connection.clone(),
        )
        .await;
    }
    drop(authenticated);
}

fn parse_request(text: &str) -> Result<Request, ()> {
    if text.contains(['\n', '\r']) || text.len() > crew_core::MAX_FRAME_BYTES {
        return Err(());
    }
    let value: serde_json::Value = serde_json::from_str(text).map_err(|_| ())?;
    let map = value.as_object().ok_or(())?;
    if map
        .keys()
        .any(|key| !matches!(key.as_str(), "id" | "method" | "params"))
    {
        return Err(());
    }
    serde_json::from_value(value).map_err(|_| ())
}

async fn index(
    gateway: Arc<Gateway>,
    mut ws: Ws,
    device: Device,
    mut cancel: watch::Receiver<bool>,
    connection: Arc<Connection>,
) {
    let mut changes = gateway.changed.subscribe();
    let mut tick = tokio::time::interval(Duration::from_secs(1));
    let mut ping = tokio::time::interval_at(
        tokio::time::Instant::now() + Duration::from_secs(20),
        Duration::from_secs(20),
    );
    let mut last_input = tokio::time::Instant::now();
    let mut revision = 0;
    let expiry = tokio::time::Instant::now()
        + Duration::from_millis(device.expires_at.saturating_sub(super::now_ms()).max(0) as u64);
    loop {
        if !gateway.current(&device, None) || *cancel.borrow() {
            break;
        }
        tokio::select! {
            _ = cancel.changed() => break,
            _ = tokio::time::sleep_until(expiry) => break,
            _ = changes.changed() => {
                    if let Some(listing) = gateway.listing(&device) { if listing.listing.rev > revision { revision = listing.listing.rev; if !send(&mut ws, &crew_protocol::native::ListingNotice { native_listing: listing }, &mut cancel).await { break; } } }
            },
            _ = tick.tick() => { if last_input.elapsed() >= Duration::from_secs(40) { break; } },
            _ = ping.tick() => { if ws.send(Message::Ping(Vec::new().into())).await.is_err() { break; } },
            message = ws.next() => {
                last_input = tokio::time::Instant::now();
                match message {
                    Some(Ok(Message::Text(text))) => {
                        let Ok(request) = parse_request(&text) else { reject(&mut ws, 0, "invalid", &mut cancel).await; break; };
                        if super::policy::authorize(&request.call, ChannelPurpose::Index, &[], false).is_err() { reject(&mut ws, request.id, "forbidden", &mut cancel).await; continue; }
                        let result = match request.call {
                            Call::Ping => serde_json::json!({"pong":true}),
                            Call::Workspaces => { let Some(listing) = gateway.listing(&device) else { break; }; revision = listing.listing.rev; serde_json::to_value(listing).unwrap() },
                            _ => unreachable!(),
                        };
                        if !send(&mut ws, &ServerFrame::ok(request.id, result), &mut cancel).await { break; }
                    },
                    Some(Ok(Message::Ping(data))) => { if ws.send(Message::Pong(data)).await.is_err() { break; } },
                    Some(Ok(Message::Pong(_))) => {},
                    _ => break,
                }
            }
        }
    }
    close_socket(&mut ws, &connection).await;
}

async fn workspace_channel(
    gateway: Arc<Gateway>,
    mut ws: Ws,
    device: Device,
    route: (ChannelPurpose, crew_protocol::native::WorkspaceId),
    hello_id: u64,
    mut cancel: watch::Receiver<bool>,
    connection: Arc<Connection>,
) {
    let (purpose, workspace) = route;
    let Some(grant) = device.grants.get(&workspace) else {
        return;
    };
    if purpose == ChannelPurpose::Terminal
        && !grant
            .capabilities
            .contains(&crew_protocol::native::Capability::Terminal)
    {
        reject(&mut ws, hello_id, "forbidden", &mut cancel).await;
        return;
    }
    let Some((mut dial, route)) =
        super::route::open(&gateway, &device, workspace, connection.id, &mut cancel).await
    else {
        reject(&mut ws, hello_id, "unavailable", &mut cancel).await;
        return;
    };
    let hello = GatewayOpened {
        native_protocol: 1,
        protocol: 6,
        authority: gateway.authority(&device),
        purpose,
        grant: Some(grant.view(workspace)),
        route: Some(route),
        hello: Some(dial.hello.clone()),
    };
    if !send(
        &mut ws,
        &ServerFrame::ok(hello_id, serde_json::to_value(hello).unwrap()),
        &mut cancel,
    )
    .await
    {
        return;
    }
    let mut buffer = Vec::new();
    let mut pending = std::collections::BTreeSet::new();
    let mut attached = false;
    let mut tick = tokio::time::interval(Duration::from_secs(1));
    let mut ping = tokio::time::interval_at(
        tokio::time::Instant::now() + Duration::from_secs(20),
        Duration::from_secs(20),
    );
    let mut last_input = tokio::time::Instant::now();
    let expiry = tokio::time::Instant::now()
        + Duration::from_millis(device.expires_at.saturating_sub(super::now_ms()).max(0) as u64);
    loop {
        if !gateway.current(&device, Some(workspace)) || *cancel.borrow() {
            break;
        }
        tokio::select! {
            _ = cancel.changed() => break,
            _ = tokio::time::sleep_until(expiry) => break,
            _ = tick.tick() => { if last_input.elapsed() >= Duration::from_secs(40) { break; } },
            _ = ping.tick() => { if ws.send(Message::Ping(Vec::new().into())).await.is_err() { break; } },
            read = daemons::read_line(&mut dial.lines, &mut buffer, crew_core::MAX_FRAME_BYTES) => {
                if read.is_none() { break; }
                let frame = serde_json::from_slice::<ServerFrame>(&buffer); buffer.clear();
                let Ok(mut frame) = frame else { break; };
                if frame.id.is_some_and(|id| pending.remove(&id)) { gateway.release_pending(connection.id); }
                if let Some(error) = frame.error.as_mut() { error.data = None; error.message = "daemon request unavailable".into(); }
                if !send(&mut ws, &frame, &mut cancel).await { break; }
            },
            message = ws.next() => {
                last_input = tokio::time::Instant::now();
                match message {
                    Some(Ok(Message::Text(text))) => {
                        let Ok(request) = parse_request(&text) else { reject(&mut ws, 0, "invalid", &mut cancel).await; break; };
                        if let Err(error) = super::policy::authorize(&request.call, purpose, &grant.capabilities, attached) { if !send(&mut ws, &ServerFrame::err(request.id, error), &mut cancel).await { break; } continue; }
                        if !gateway.current(&device, Some(workspace)) || *cancel.borrow() { break; }
                        let is_attach = matches!(request.call, Call::Attach { .. });
                        let acknowledged = !matches!(request.call, Call::AttachInput { .. } | Call::AttachResize { .. });
                        if acknowledged && (pending.contains(&request.id) || !gateway.acquire_pending(connection.id)) {
                            reject(&mut ws, request.id, "unavailable", &mut cancel).await; continue;
                        }
                        if acknowledged { pending.insert(request.id); }
                        let mut bytes = match serde_json::to_vec(&request) { Ok(bytes) => bytes, Err(_) => break }; bytes.push(b'\n');
                        let sent = tokio::select! { sent = tokio::time::timeout(Duration::from_secs(3), dial.writer.write_all(&bytes)) => matches!(sent, Ok(Ok(()))), _ = cancel.changed() => false };
                        if !sent { break; }
                        attached |= is_attach;
                    },
                    Some(Ok(Message::Ping(data))) => { if ws.send(Message::Pong(data)).await.is_err() { break; } },
                    Some(Ok(Message::Pong(_))) => {},
                    _ => break,
                }
            }
        }
    }
    close_socket(&mut ws, &connection).await;
}

async fn close_socket(ws: &mut Ws, connection: &Connection) {
    let code = connection
        .close_code
        .load(std::sync::atomic::Ordering::Relaxed);
    let close = tokio_tungstenite::tungstenite::protocol::CloseFrame {
        code: code.into(),
        reason: "native channel unavailable".into(),
    };
    let _ = tokio::time::timeout(Duration::from_millis(500), ws.close(Some(close))).await;
}
