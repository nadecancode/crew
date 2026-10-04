//! One connection to the dashboard's port: its HTTP request, and when that
//! is the upgrade, the WebSocket that carries the daemon's protocol.
//!
//! One JSON object per text message, both ways, through the daemon's own
//! [`Session`]. Before `hello` succeeded a connection has 5 s, messages of
//! at most 4 KiB, and only `ping`, `web_pair` and `hello` do anything. The
//! server pings every 20 s and closes a connection silent for 40 s.

use std::time::Duration;

use futures_util::{SinkExt, StreamExt};
use tokio::net::TcpStream;
use tokio::sync::mpsc;
use tokio_tungstenite::tungstenite::protocol::frame::coding::CloseCode;
use tokio_tungstenite::tungstenite::protocol::{CloseFrame, Role, WebSocketConfig};
use tokio_tungstenite::tungstenite::{Error as WsError, Message};
use tokio_tungstenite::WebSocketStream;

use super::http::{self, Answer};
use super::Admitted;
use crate::daemon::server::{Session, Transport};
use crate::daemon::Daemon;

/// A WebSocket must authenticate within this.
pub(crate) const HELLO_TIMEOUT: Duration = Duration::from_secs(5);
/// The largest message before `hello` succeeded.
pub(crate) const MAX_UNAUTHENTICATED_MESSAGE: usize = 4 * 1024;
/// How often the server pings.
pub(crate) const PING_EVERY: Duration = Duration::from_secs(20);
/// A connection that sent nothing for this long is closed.
pub(crate) const SILENT_FOR: Duration = Duration::from_secs(40);

/// What the reader tells the writer.
enum Ctl {
    Close(CloseCode, &'static str),
}

pub(super) async fn serve(daemon: Daemon, mut stream: TcpStream, mut admitted: Admitted) {
    let Some(serving) = daemon.web.serving() else {
        return;
    };
    let host = serving.host();
    let mut close = admitted.close.clone();
    let head = tokio::select! {
        head = http::read_head(&mut stream) => head,
        _ = close.changed() => return,
    };
    let (head, rest) = match head {
        Ok(head) => head,
        Err(http::HeadError::Closed) => return,
        Err(http::HeadError::Bad) => {
            http::respond(&mut stream, &Answer::Status(400), &host).await;
            return;
        }
    };
    let key = match http::route(&head, &host) {
        Answer::Upgrade(key) => key,
        answer => {
            http::respond(&mut stream, &answer, &host).await;
            return;
        }
    };
    use tokio::io::AsyncWriteExt;
    if stream
        .write_all(&http::switching_protocols(&key))
        .await
        .is_err()
    {
        return;
    }
    let config = WebSocketConfig::default()
        .max_message_size(Some(crew_core::MAX_FRAME_BYTES))
        .max_frame_size(Some(crew_core::MAX_FRAME_BYTES));
    let socket =
        WebSocketStream::from_partially_read(stream, rest, Role::Server, Some(config)).await;
    run(daemon, socket, &mut admitted).await;
}

async fn run(daemon: Daemon, socket: WebSocketStream<TcpStream>, admitted: &mut Admitted) {
    let (mut sink, mut source) = socket.split();
    let (mut session, mut frames) = Session::new(&daemon, Transport::Web);
    let (ctl, mut ctl_rx) = mpsc::unbounded_channel::<Ctl>();
    let mut close = admitted.close.clone();

    // The writer: frames as they are queued, pings, and the close.
    let writer = {
        let mut close = close.clone();
        tokio::spawn(async move {
            let mut ping =
                tokio::time::interval_at(tokio::time::Instant::now() + PING_EVERY, PING_EVERY);
            let goodbye: Option<CloseFrame> = loop {
                tokio::select! {
                    biased;
                    frame = frames.recv() => {
                        let Some(frame) = frame else { break None };
                        let Ok(text) = serde_json::to_string(&frame) else { continue };
                        if sink.send(Message::text(text)).await.is_err() {
                            return;
                        }
                    }
                    order = ctl_rx.recv() => {
                        break order.map(|Ctl::Close(code, reason)| CloseFrame { code, reason: reason.into() });
                    }
                    _ = close.changed() => {
                        // What is queued (a `shutdown` push) goes first.
                        while let Ok(frame) = frames.try_recv() {
                            let Ok(text) = serde_json::to_string(&frame) else { continue };
                            if sink.send(Message::text(text)).await.is_err() {
                                return;
                            }
                        }
                        break Some(CloseFrame { code: CloseCode::Away, reason: "the dashboard was stopped".into() });
                    }
                    _ = ping.tick() => {
                        if sink.send(Message::Ping(Vec::new().into())).await.is_err() {
                            return;
                        }
                    }
                }
            };
            if let Some(frame) = goodbye {
                let _ = sink.send(Message::Close(Some(frame))).await;
            }
            let _ = sink.close().await;
        })
    };

    let hello_by = tokio::time::Instant::now() + HELLO_TIMEOUT;
    let mut silent_from = tokio::time::Instant::now();
    loop {
        let deadline = if session.authenticated() {
            silent_from + SILENT_FOR
        } else {
            hello_by.min(silent_from + SILENT_FOR)
        };
        let message = tokio::select! {
            message = source.next() => message,
            _ = close.changed() => break,
            _ = tokio::time::sleep_until(deadline) => {
                let _ = ctl.send(Ctl::Close(CloseCode::Policy, if session.authenticated() {
                    "silent for too long"
                } else {
                    "say hello first"
                }));
                break;
            }
        };
        silent_from = tokio::time::Instant::now();
        let text = match message {
            None | Some(Ok(Message::Close(_))) => break,
            Some(Ok(Message::Text(text))) => text,
            Some(Ok(Message::Binary(_))) => {
                let _ = ctl.send(Ctl::Close(CloseCode::Unsupported, "text messages only"));
                break;
            }
            Some(Ok(_)) => continue,
            Some(Err(WsError::Capacity(_))) => {
                let _ = ctl.send(Ctl::Close(CloseCode::Size, "message too large"));
                break;
            }
            Some(Err(_)) => break,
        };
        if !session.authenticated() && text.len() > MAX_UNAUTHENTICATED_MESSAGE {
            let _ = ctl.send(Ctl::Close(
                CloseCode::Size,
                "message too large before hello",
            ));
            break;
        }
        if !session.request(text.as_str()).await {
            break;
        }
        if let Some(id) = session.web_session() {
            admitted.authenticated(id);
        }
        if session.close_requested() {
            let _ = ctl.send(Ctl::Close(CloseCode::Policy, "refused"));
            break;
        }
    }
    session.close();
    drop(ctl);
    let _ = tokio::time::timeout(Duration::from_secs(2), writer).await;
}
