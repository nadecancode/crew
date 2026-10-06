//! Native-only upgrade. No assets, browser session, query or reflected errors.

use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};
use tokio_tungstenite::tungstenite::handshake::derive_accept_key;

pub(crate) async fn upgrade_head<S: AsyncRead + AsyncWrite + Unpin>(
    stream: &mut S,
    host: &str,
) -> Option<Vec<u8>> {
    let mut bytes = Vec::with_capacity(2048);
    loop {
        let mut headers = [httparse::EMPTY_HEADER; 48];
        let mut request = httparse::Request::new(&mut headers);
        match request.parse(&bytes) {
            Ok(httparse::Status::Complete(end)) => {
                let mut hosts = Vec::new();
                let mut keys = Vec::new();
                let mut versions = Vec::new();
                let mut upgrade = false;
                let mut connection = false;
                let mut bad = request.method != Some("GET")
                    || request.path != Some("/native/ws")
                    || request.version != Some(1);
                let mut origin = false;
                for header in request.headers {
                    let Ok(value) = std::str::from_utf8(header.value) else {
                        bad = true;
                        continue;
                    };
                    let value = value.trim();
                    match header.name.to_ascii_lowercase().as_str() {
                        "host" => hosts.push(value),
                        "origin" => origin = true,
                        "sec-websocket-key" => keys.push(value),
                        "sec-websocket-version" => versions.push(value),
                        "upgrade" => upgrade |= value.eq_ignore_ascii_case("websocket"),
                        "connection" => {
                            connection |= value
                                .split(',')
                                .any(|p| p.trim().eq_ignore_ascii_case("upgrade"))
                        }
                        "content-length" => bad |= value != "0",
                        "transfer-encoding" => bad = true,
                        _ => {}
                    }
                }
                if origin
                    || bad
                    || hosts != [host]
                    || keys.len() != 1
                    || versions != ["13"]
                    || !upgrade
                    || !connection
                {
                    refuse(stream, if origin { 403 } else { 400 }).await;
                    return None;
                }
                use base64::Engine;
                if !matches!(base64::engine::general_purpose::STANDARD.decode(keys[0]), Ok(ref key) if key.len() == 16)
                {
                    refuse(stream, 400).await;
                    return None;
                }
                let accept = derive_accept_key(keys[0].as_bytes());
                let response = format!("HTTP/1.1 101 Switching Protocols\r\nUpgrade: websocket\r\nConnection: Upgrade\r\nSec-WebSocket-Accept: {accept}\r\n\r\n");
                stream.write_all(response.as_bytes()).await.ok()?;
                stream.flush().await.ok()?;
                return Some(bytes[end..].to_vec());
            }
            Ok(httparse::Status::Partial) => {}
            Err(_) => {
                refuse(stream, 400).await;
                return None;
            }
        }
        if bytes.len() >= 8 * 1024 {
            refuse(stream, 400).await;
            return None;
        }
        let mut part = [0; 1024];
        let room = (8 * 1024 - bytes.len()).min(part.len());
        let n = stream.read(&mut part[..room]).await.ok()?;
        if n == 0 {
            return None;
        }
        bytes.extend_from_slice(&part[..n]);
    }
}

async fn refuse<S: AsyncWrite + Unpin>(stream: &mut S, status: u16) {
    let response = format!("HTTP/1.1 {status} Request Unavailable\r\nContent-Length: 0\r\nCache-Control: no-store\r\nConnection: close\r\n\r\n");
    let _ = stream.write_all(response.as_bytes()).await;
    let _ = stream.flush().await;
    let _ = stream.shutdown().await;
}
