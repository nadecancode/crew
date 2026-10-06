use std::sync::Arc;
use std::time::Duration;

use crew_protocol::native::{GatewayOpened, HandshakeRequest, HexBytes, Notice, NoticeFrame};
use crew_protocol::rpc::{HelloInfo, RpcError};
use futures_util::{SinkExt, StreamExt};
use serde::Deserialize;
use sha2::{Digest, Sha256};
use tokio::net::TcpStream;
use tokio_rustls::client::TlsStream;
use tokio_tungstenite::tungstenite::{
    client::IntoClientRequest, protocol::WebSocketConfig, Message,
};
use tokio_tungstenite::WebSocketStream;

use super::{GatewayError, GatewayProfile};
use crate::framing::{validate_record, MAX_FRAME_BYTES};
use crate::TransportEnd;

pub(super) type Socket = WebSocketStream<TlsStream<TcpStream>>;
pub(super) const NETWORK_TIMEOUT: Duration = Duration::from_secs(5);
pub(super) const PROOF_ID: u64 = 0;

pub(super) async fn connect(profile: &GatewayProfile) -> Result<Socket, GatewayError> {
    let tcp = TcpStream::connect((profile.host.as_str(), profile.port))
        .await
        .map_err(|_| GatewayError::Transport(TransportEnd::Unreachable))?;
    let name = rustls::pki_types::ServerName::try_from(profile.host.clone())
        .map_err(|_| GatewayError::InvalidProfile)?;
    let tls = tokio_rustls::TlsConnector::from(profile.tls.clone())
        .connect(name, tcp)
        .await
        .map_err(|_| GatewayError::Trust)?;
    let connection = tls.get_ref().1;
    if connection.protocol_version() != Some(rustls::ProtocolVersion::TLSv1_3) {
        return Err(GatewayError::Trust);
    }
    let leaf = connection
        .peer_certificates()
        .and_then(|certs| certs.first())
        .ok_or(GatewayError::Trust)?;
    let actual = HexBytes(Sha256::digest(leaf.as_ref()).into());
    if actual != profile.public.tls_leaf_sha256 {
        return Err(GatewayError::Trust);
    }
    // Chain/name/time/handshake-signature and pin verification precede the
    // upgrade, challenge and every call into the platform signer.
    let request = profile
        .public
        .url
        .clone()
        .into_client_request()
        .map_err(|_| GatewayError::InvalidProfile)?;
    let config = WebSocketConfig::default()
        .read_buffer_size(8192)
        .write_buffer_size(0)
        .max_write_buffer_size(MAX_FRAME_BYTES + 1024)
        .max_message_size(Some(MAX_FRAME_BYTES))
        .max_frame_size(Some(MAX_FRAME_BYTES));
    let (socket, _) = tokio_tungstenite::client_async_with_config(request, tls, Some(config))
        .await
        .map_err(|_| GatewayError::Transport(TransportEnd::Malformed))?;
    Ok(socket)
}

pub(super) async fn next_text(socket: &mut Socket) -> Result<String, GatewayError> {
    loop {
        match socket.next().await {
            Some(Ok(Message::Text(text))) => {
                validate_record(text.as_str())
                    .map_err(|_| GatewayError::Transport(TransportEnd::Malformed))?;
                return Ok(text.to_string());
            }
            Some(Ok(Message::Ping(_))) => socket.flush().await.map_err(ws_error)?,
            Some(Ok(Message::Pong(_))) => {}
            Some(Ok(Message::Close(frame))) => {
                return Err(GatewayError::Transport(close_reason(
                    frame.map(|frame| u16::from(frame.code)),
                )))
            }
            Some(Err(error)) => return Err(ws_error(error)),
            Some(Ok(_)) => return Err(GatewayError::Transport(TransportEnd::Malformed)),
            None => return Err(GatewayError::Transport(TransportEnd::Closed)),
        }
    }
}

pub(super) async fn challenge(
    socket: &mut Socket,
) -> Result<crew_protocol::native::Nonce, GatewayError> {
    let text = next_text(socket).await?;
    if text.len() > 4096 {
        return Err(GatewayError::Transport(TransportEnd::Oversized));
    }
    let frame: NoticeFrame = serde_json::from_str(&text)
        .map_err(|_| GatewayError::Transport(TransportEnd::Malformed))?;
    match frame.native {
        Notice::Challenge { nonce } => Ok(nonce),
        _ => Err(GatewayError::Transport(TransportEnd::Malformed)),
    }
}

pub(super) async fn send_proof(
    socket: &mut Socket,
    proof: HandshakeRequest,
) -> Result<(), GatewayError> {
    let text = serde_json::to_string(&proof)
        .map_err(|_| GatewayError::Transport(TransportEnd::Malformed))?;
    if text.len() > 4096 {
        return Err(GatewayError::Transport(TransportEnd::Oversized));
    }
    socket.send(Message::text(text)).await.map_err(ws_error)
}

pub(super) fn opened(text: &str) -> Result<GatewayOpened<HelloInfo>, GatewayError> {
    #[derive(Deserialize)]
    #[serde(deny_unknown_fields)]
    struct Reply {
        id: u64,
        result: Option<GatewayOpened<HelloInfo>>,
        error: Option<RpcError>,
    }
    let reply: Reply =
        serde_json::from_str(text).map_err(|_| GatewayError::Transport(TransportEnd::Malformed))?;
    if reply.id != PROOF_ID || (reply.result.is_some() == reply.error.is_some()) {
        return Err(GatewayError::Transport(TransportEnd::Malformed));
    }
    if let Some(error) = reply.error {
        let end = match error.code.as_str() {
            "protocol" => TransportEnd::ProtocolMismatch,
            "unavailable" => TransportEnd::Unreachable,
            "forbidden" => TransportEnd::Permission,
            _ => TransportEnd::Authentication,
        };
        return Err(GatewayError::Transport(end));
    }
    reply
        .result
        .ok_or(GatewayError::Transport(TransportEnd::Malformed))
}

pub(super) fn close_reason(code: Option<u16>) -> TransportEnd {
    use crew_protocol::native::*;
    match code {
        Some(CLOSE_DEVICE_ACCESS) => TransportEnd::Revoked,
        Some(CLOSE_WORKSPACE_ACCESS) => TransportEnd::GrantChanged,
        Some(CLOSE_GATEWAY_STOP) => TransportEnd::Stopped,
        Some(CLOSE_INCOMPATIBLE) => TransportEnd::ProtocolMismatch,
        _ => TransportEnd::Closed,
    }
}

pub(super) fn ws_error(error: tokio_tungstenite::tungstenite::Error) -> GatewayError {
    use tokio_tungstenite::tungstenite::Error;
    GatewayError::Transport(match error {
        Error::Capacity(_) => TransportEnd::Oversized,
        Error::Utf8(_) | Error::Protocol(_) => TransportEnd::Malformed,
        _ => TransportEnd::Closed,
    })
}

pub(super) fn verify_signature(
    key: crew_protocol::native::PublicKey,
    bytes: &[u8],
    proof: crew_protocol::native::Signature,
) -> Result<(), GatewayError> {
    let key = ed25519_dalek::VerifyingKey::from_bytes(&key.0)
        .map_err(|_| GatewayError::Signer(super::SignerError::InvalidProof))?;
    if key.is_weak()
        || key
            .verify_strict(bytes, &ed25519_dalek::Signature::from_bytes(&proof.0))
            .is_err()
    {
        return Err(GatewayError::Signer(super::SignerError::InvalidProof));
    }
    Ok(())
}

pub(super) fn random_nonce() -> Result<crew_protocol::native::Nonce, GatewayError> {
    let mut bytes = [0; 32];
    rustls::crypto::ring::default_provider()
        .secure_random
        .fill(&mut bytes)
        .map_err(|_| GatewayError::Signer(super::SignerError::Unavailable))?;
    Ok(HexBytes(bytes))
}

pub(super) async fn unlock(
    signer: &Arc<dyn super::DeviceSigner>,
) -> Result<crew_protocol::native::PublicKey, GatewayError> {
    let key = tokio::time::timeout(Duration::from_secs(30), signer.public_key())
        .await
        .map_err(|_| GatewayError::Signer(super::SignerError::Canceled))?
        .map_err(GatewayError::Signer)?;
    let verified = ed25519_dalek::VerifyingKey::from_bytes(&key.0)
        .map_err(|_| GatewayError::Signer(super::SignerError::InvalidProof))?;
    if verified.is_weak() {
        return Err(GatewayError::Signer(super::SignerError::InvalidProof));
    }
    Ok(key)
}
