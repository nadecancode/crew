//! The one HTTP request a connection to the dashboard's port may make: read
//! and check its head, answer with an embedded file or a status, or accept
//! the WebSocket upgrade (notes/web-design.md §security, "What each request
//! must pass").
//!
//! Every check happens before any routing, and nothing that arrives before
//! authentication is unbounded: the head has a deadline, a size and a
//! header count, and there is never a body.

use std::time::Duration;

use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};

use super::assets::{self, Asset};

/// The head must arrive within this.
pub(crate) const HEAD_TIMEOUT: Duration = Duration::from_secs(5);
/// At most this many bytes of head.
pub(crate) const MAX_HEAD_BYTES: usize = 8 * 1024;
/// At most this many headers.
pub(crate) const MAX_HEADERS: usize = 48;

/// What the server needs to know of a request head.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(crate) struct Head {
    pub method: String,
    /// Without the query string.
    pub path: String,
    pub host: Option<String>,
    pub origin: Option<String>,
    /// `Upgrade: websocket`.
    pub websocket: bool,
    pub ws_version: Option<String>,
    pub ws_key: Option<String>,
    /// It announced a body (a `Content-Length` other than 0, or a
    /// `Transfer-Encoding`), or a header appeared twice that must not.
    pub malformed: bool,
}

/// Why a head could not be read.
#[derive(Debug, PartialEq, Eq)]
pub(crate) enum HeadError {
    /// Too long, too many headers, not HTTP/1.1, or it never ended.
    Bad,
    /// The connection closed before the head was complete.
    Closed,
}

/// Parse a complete head (ending in an empty line).
pub(crate) fn parse(bytes: &[u8]) -> Result<Option<(Head, usize)>, HeadError> {
    let mut headers = [httparse::EMPTY_HEADER; MAX_HEADERS];
    let mut request = httparse::Request::new(&mut headers);
    let len = match request.parse(bytes) {
        Ok(httparse::Status::Complete(len)) => len,
        Ok(httparse::Status::Partial) => return Ok(None),
        Err(_) => return Err(HeadError::Bad),
    };
    if request.version != Some(1) {
        return Err(HeadError::Bad);
    }
    let target = request.path.unwrap_or_default();
    let path = target.split(['?', '#']).next().unwrap_or_default();
    let mut head = Head {
        method: request.method.unwrap_or_default().to_string(),
        path: path.to_string(),
        ..Head::default()
    };
    let mut hosts = 0;
    let mut origins = 0;
    for header in request.headers.iter() {
        let value = || String::from_utf8_lossy(header.value).trim().to_string();
        match header.name.to_ascii_lowercase().as_str() {
            "host" => {
                hosts += 1;
                head.host = Some(value());
            }
            "origin" => {
                origins += 1;
                head.origin = Some(value());
            }
            "upgrade" => {
                head.websocket = value()
                    .split(',')
                    .any(|p| p.trim().eq_ignore_ascii_case("websocket"))
            }
            "sec-websocket-version" => head.ws_version = Some(value()),
            "sec-websocket-key" => head.ws_key = Some(value()),
            "content-length" if value() != "0" => head.malformed = true,
            "transfer-encoding" => head.malformed = true,
            _ => {}
        }
    }
    if hosts > 1 || origins > 1 {
        head.malformed = true;
    }
    Ok(Some((head, len)))
}

/// Read one request head from `stream`: within [`HEAD_TIMEOUT`], at most
/// [`MAX_HEAD_BYTES`]. Returns the head and what followed it.
pub(crate) async fn read_head<S: AsyncRead + Unpin>(
    stream: &mut S,
) -> Result<(Head, Vec<u8>), HeadError> {
    let read = async {
        let mut buf = Vec::with_capacity(1024);
        let mut chunk = [0u8; 1024];
        loop {
            let n = stream
                .read(&mut chunk)
                .await
                .map_err(|_| HeadError::Closed)?;
            if n == 0 {
                return Err(HeadError::Closed);
            }
            buf.extend_from_slice(&chunk[..n]);
            if let Some((head, len)) = parse(&buf)? {
                if len > MAX_HEAD_BYTES {
                    return Err(HeadError::Bad);
                }
                return Ok((head, buf.split_off(len)));
            }
            if buf.len() > MAX_HEAD_BYTES {
                return Err(HeadError::Bad);
            }
        }
    };
    tokio::time::timeout(HEAD_TIMEOUT, read)
        .await
        .map_err(|_| HeadError::Bad)?
}

/// What to answer a request.
#[derive(Debug, PartialEq, Eq)]
pub(crate) enum Answer {
    File(Asset),
    /// Accept the WebSocket upgrade with this key.
    Upgrade(String),
    Status(u16),
}

/// The checks, in order, then the route. `host` is the server's name with
/// its port (`<label>.localhost:<port>`).
pub(crate) fn route(head: &Head, host: &str) -> Answer {
    if head.malformed {
        return Answer::Status(400);
    }
    if head.host.as_deref() != Some(host) {
        return Answer::Status(421);
    }
    if head.method != "GET" {
        return Answer::Status(405);
    }
    if head.path == "/ws" && head.websocket {
        if head.origin.as_deref() != Some(&format!("http://{host}")) {
            return Answer::Status(403);
        }
        return match (&head.ws_version, &head.ws_key) {
            (Some(version), Some(key)) if version == "13" && !key.is_empty() => {
                Answer::Upgrade(key.clone())
            }
            _ => Answer::Status(400),
        };
    }
    let path = match head.path.as_str() {
        "/" => "index.html",
        other => match other.strip_prefix('/') {
            Some(rest) => rest,
            None => return Answer::Status(404),
        },
    };
    match assets::find(path) {
        Some(asset) => Answer::File(asset),
        None => Answer::Status(404),
    }
}

fn reason(status: u16) -> &'static str {
    match status {
        200 => "OK",
        400 => "Bad Request",
        403 => "Forbidden",
        404 => "Not Found",
        405 => "Method Not Allowed",
        421 => "Misdirected Request",
        _ => "Error",
    }
}

/// The policy of `index.html`.
pub(crate) fn content_security_policy(host: &str) -> String {
    format!(
        "default-src 'none'; script-src 'self'; style-src-elem 'self' 'unsafe-inline'; \
style-src-attr 'none'; img-src 'self' data:; font-src 'self'; connect-src 'self' ws://{host}; \
base-uri 'none'; form-action 'none'; frame-ancestors 'none'"
    )
}

/// The response to `answer` (not an upgrade), whole.
pub(crate) fn response(answer: &Answer, host: &str) -> Vec<u8> {
    let (status, content_type, body): (u16, &str, &[u8]) = match answer {
        Answer::File(asset) => (200, asset.content_type, asset.bytes),
        Answer::Status(status) => (
            *status,
            "text/plain; charset=utf-8",
            reason(*status).as_bytes(),
        ),
        Answer::Upgrade(_) => (400, "text/plain; charset=utf-8", b"Bad Request"),
    };
    let mut head = format!(
        "HTTP/1.1 {status} {}\r\nContent-Type: {content_type}\r\nContent-Length: {}\r\n\
Connection: close\r\nX-Content-Type-Options: nosniff\r\nReferrer-Policy: no-referrer\r\n\
Cross-Origin-Resource-Policy: same-origin\r\n",
        reason(status),
        body.len()
    );
    match answer {
        Answer::File(asset) if asset.path == "index.html" => {
            head.push_str("Cache-Control: no-store\r\nX-Frame-Options: DENY\r\n");
            head.push_str("Cross-Origin-Opener-Policy: same-origin\r\n");
            head.push_str(&format!(
                "Content-Security-Policy: {}\r\n",
                content_security_policy(host)
            ));
        }
        Answer::File(_) => {
            head.push_str("Cache-Control: public, max-age=31536000, immutable\r\n");
        }
        _ => {
            head.push_str("Cache-Control: no-store\r\n");
            if *answer == Answer::Status(405) {
                head.push_str("Allow: GET\r\n");
            }
        }
    }
    head.push_str("\r\n");
    let mut bytes = head.into_bytes();
    bytes.extend_from_slice(body);
    bytes
}

/// Write the response to `answer` and close the writing side.
pub(crate) async fn respond<S: AsyncWrite + Unpin>(stream: &mut S, answer: &Answer, host: &str) {
    let _ = stream.write_all(&response(answer, host)).await;
    let _ = stream.shutdown().await;
}

/// The `101` that accepts an upgrade with `key`.
pub(crate) fn switching_protocols(key: &str) -> Vec<u8> {
    let accept = tokio_tungstenite::tungstenite::handshake::derive_accept_key(key.as_bytes());
    format!(
        "HTTP/1.1 101 Switching Protocols\r\nUpgrade: websocket\r\nConnection: Upgrade\r\n\
Sec-WebSocket-Accept: {accept}\r\n\r\n"
    )
    .into_bytes()
}

#[cfg(test)]
mod tests {
    use super::*;

    const HOST: &str = "c7f3a09b52e14d6f8a1b3c5d7e9f0a2b.localhost:23417";

    fn head(text: &str) -> Head {
        parse(text.as_bytes()).unwrap().unwrap().0
    }

    fn get(path: &str, host: &str) -> Head {
        head(&format!("GET {path} HTTP/1.1\r\nHost: {host}\r\n\r\n"))
    }

    fn upgrade(origin: Option<&str>, version: &str, key: &str) -> Head {
        let origin = origin
            .map(|o| format!("Origin: {o}\r\n"))
            .unwrap_or_default();
        head(&format!(
            "GET /ws HTTP/1.1\r\nHost: {HOST}\r\nUpgrade: websocket\r\nConnection: Upgrade\r\n\
{origin}Sec-WebSocket-Version: {version}\r\nSec-WebSocket-Key: {key}\r\n\r\n"
        ))
    }

    #[test]
    fn routing_by_path_host_and_method() {
        assert!(matches!(route(&get("/", HOST), HOST), Answer::File(a) if a.path == "index.html"));
        assert!(
            matches!(route(&get("/?x=1", HOST), HOST), Answer::File(a) if a.path == "index.html")
        );
        assert!(matches!(
            route(&get("/index.html", HOST), HOST),
            Answer::File(_)
        ));
        for path in [
            "/nope",
            "/ws",
            "/../Cargo.toml",
            "/assets/%2e%2e/x",
            "//index.html",
        ] {
            assert_eq!(route(&get(path, HOST), HOST), Answer::Status(404), "{path}");
        }
        for host in [
            "127.0.0.1:23417",
            "localhost:23417",
            "[::1]:23417",
            "ffffffffffffffffffffffffffffffff.localhost:23417",
            "evil.example",
            "c7f3a09b52e14d6f8a1b3c5d7e9f0a2b.localhost",
            "C7F3A09B52E14D6F8A1B3C5D7E9F0A2B.localhost:23417",
        ] {
            assert_eq!(route(&get("/", host), HOST), Answer::Status(421), "{host}");
        }
        let no_host = head("GET / HTTP/1.1\r\n\r\n");
        assert_eq!(route(&no_host, HOST), Answer::Status(421));
        for method in ["POST", "OPTIONS", "HEAD", "PUT", "DELETE"] {
            let h = head(&format!("{method} / HTTP/1.1\r\nHost: {HOST}\r\n\r\n"));
            assert_eq!(route(&h, HOST), Answer::Status(405), "{method}");
        }
        // Wrong host before wrong method: nothing of this server answers
        // another name.
        let h = head("POST / HTTP/1.1\r\nHost: localhost:23417\r\n\r\n");
        assert_eq!(route(&h, HOST), Answer::Status(421));
    }

    #[test]
    fn bodies_and_bad_heads_are_refused() {
        for extra in [
            "Content-Length: 5\r\n",
            "Transfer-Encoding: chunked\r\n",
            "Host: other\r\n",
        ] {
            let h = head(&format!("GET / HTTP/1.1\r\nHost: {HOST}\r\n{extra}\r\n"));
            assert_eq!(route(&h, HOST), Answer::Status(400), "{extra}");
        }
        let h = head(&format!(
            "GET / HTTP/1.1\r\nHost: {HOST}\r\nContent-Length: 0\r\n\r\n"
        ));
        assert!(matches!(route(&h, HOST), Answer::File(_)));
        assert_eq!(
            parse(format!("GET / HTTP/1.0\r\nHost: {HOST}\r\n\r\n").as_bytes()),
            Err(HeadError::Bad)
        );
        assert_eq!(parse(b"\x00\x01garbage\r\n\r\n"), Err(HeadError::Bad));
        let many: String = (0..MAX_HEADERS + 1)
            .map(|i| format!("X-{i}: v\r\n"))
            .collect();
        assert_eq!(
            parse(format!("GET / HTTP/1.1\r\nHost: {HOST}\r\n{many}\r\n").as_bytes()),
            Err(HeadError::Bad)
        );
        assert_eq!(parse(b"GET / HTTP/1.1\r\nHost: x\r\n"), Ok(None));
    }

    #[tokio::test]
    async fn heads_are_bounded_in_size_and_time() {
        // Too long.
        let mut long = format!("GET / HTTP/1.1\r\nHost: {HOST}\r\nX: ").into_bytes();
        long.extend(std::iter::repeat_n(b'a', MAX_HEAD_BYTES));
        long.extend_from_slice(b"\r\n\r\n");
        assert_eq!(read_head(&mut &long[..]).await.unwrap_err(), HeadError::Bad);
        // What follows the head is kept.
        let text = format!("GET /ws HTTP/1.1\r\nHost: {HOST}\r\n\r\nrest");
        let (head, rest) = read_head(&mut text.as_bytes()).await.unwrap();
        assert_eq!((head.path.as_str(), rest.as_slice()), ("/ws", &b"rest"[..]));
    }

    #[test]
    fn the_upgrade_needs_the_origin_a_version_and_a_key() {
        let origin = format!("http://{HOST}");
        assert_eq!(
            route(
                &upgrade(Some(&origin), "13", "dGhlIHNhbXBsZSBub25jZQ=="),
                HOST
            ),
            Answer::Upgrade("dGhlIHNhbXBsZSBub25jZQ==".into())
        );
        for other in [
            None,
            Some("http://evil.example"),
            Some("http://127.0.0.1:5173"),
            Some("http://127.0.0.1:23417"),
            Some("http://localhost:23417"),
            Some("https://c7f3a09b52e14d6f8a1b3c5d7e9f0a2b.localhost:23417"),
            Some("http://ffffffffffffffffffffffffffffffff.localhost:23417"),
            Some("null"),
        ] {
            assert_eq!(
                route(&upgrade(other, "13", "k"), HOST),
                Answer::Status(403),
                "{other:?}"
            );
        }
        assert_eq!(
            route(&upgrade(Some(&origin), "8", "k"), HOST),
            Answer::Status(400)
        );
        assert_eq!(
            route(&upgrade(Some(&origin), "13", ""), HOST),
            Answer::Status(400)
        );
        // RFC 6455's example.
        let accept = String::from_utf8(switching_protocols("dGhlIHNhbXBsZSBub25jZQ==")).unwrap();
        assert!(accept.contains("Sec-WebSocket-Accept: s3pPLMBiTxaQ9kYGzzhZRbK+xOo=\r\n"));
    }

    #[test]
    fn responses_carry_the_policy_and_never_cors() {
        let index = String::from_utf8(response(&route(&get("/", HOST), HOST), HOST)).unwrap();
        for want in [
            "HTTP/1.1 200 OK\r\n",
            "Connection: close\r\n",
            "X-Content-Type-Options: nosniff\r\n",
            "Referrer-Policy: no-referrer\r\n",
            "Cross-Origin-Resource-Policy: same-origin\r\n",
            "Cache-Control: no-store\r\n",
            "X-Frame-Options: DENY\r\n",
            "Cross-Origin-Opener-Policy: same-origin\r\n",
            &format!("connect-src 'self' ws://{HOST};"),
            "script-src 'self';",
            "frame-ancestors 'none'",
        ] {
            assert!(index.contains(want), "{want} in {index}");
        }
        let refused = String::from_utf8(response(&Answer::Status(405), HOST)).unwrap();
        assert!(refused.starts_with("HTTP/1.1 405 Method Not Allowed\r\n"));
        assert!(refused.contains("Allow: GET\r\n"));
        for answer in [
            Answer::Status(421),
            Answer::Status(404),
            Answer::Status(403),
        ] {
            let text = String::from_utf8(response(&answer, HOST)).unwrap();
            assert!(
                !text.to_ascii_lowercase().contains("access-control-"),
                "{text}"
            );
            assert!(!text.contains("Content-Security-Policy"), "{text}");
        }
        assert!(!index.to_ascii_lowercase().contains("access-control-"));
    }
}
