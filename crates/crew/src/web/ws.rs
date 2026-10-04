//! One connection to the dashboard's port: its HTTP request, and when that
//! is the upgrade, the WebSocket that carries the daemon's protocol.
//!
//! One JSON object per text message, both ways, through the daemon's own
//! [`Session`]. Before `hello` succeeded a connection has 5 s, messages of
//! at most 4 KiB, and only `ping`, `web_pair` and `hello` do anything. The
//! server pings every 20 s and closes a connection silent for 40 s.

use std::time::Duration;

use futures_util::{SinkExt, StreamExt};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpStream;
use tokio::sync::mpsc;
use tokio::task::JoinHandle;
use tokio_tungstenite::tungstenite::protocol::frame::coding::CloseCode;
use tokio_tungstenite::tungstenite::protocol::{CloseFrame, Role, WebSocketConfig};
use tokio_tungstenite::tungstenite::{Error as WsError, Message};
use tokio_tungstenite::WebSocketStream;

use super::http::{self, Answer};
use super::{spawn_held, Admitted};
use crate::daemon::server::Session;
use crate::daemon::Daemon;

/// A WebSocket must authenticate within this.
pub(crate) const HELLO_TIMEOUT: Duration = Duration::from_secs(5);
/// The largest message before `hello` succeeded.
pub(crate) const MAX_UNAUTHENTICATED_MESSAGE: usize = 4 * 1024;
/// How often the server pings.
pub(crate) const PING_EVERY: Duration = Duration::from_secs(20);
/// A connection that sent nothing for this long is closed.
pub(crate) const SILENT_FOR: Duration = Duration::from_secs(40);
/// How long a closed connection waits for the browser to close too.
const LINGER: Duration = Duration::from_secs(1);

/// What the reader tells the writer.
enum Ctl {
    Close(CloseCode, &'static str),
}

/// The close frame a connection ends with: the reader's reason, else the
/// server's when it is stopping, else none.
fn goodbye(order: Option<Ctl>, stopping: bool) -> Option<CloseFrame> {
    match order {
        Some(Ctl::Close(code, reason)) => Some(CloseFrame {
            code,
            reason: reason.into(),
        }),
        None if stopping => Some(CloseFrame {
            code: CloseCode::Away,
            reason: "the dashboard was stopped".into(),
        }),
        None => None,
    }
}

pub(crate) async fn serve(daemon: Daemon, mut stream: TcpStream, admitted: Admitted) {
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
    run(daemon, socket, &admitted).await;
}

/// Aborts its task when dropped: the writer does not outlive the
/// connection's task, however that ends.
struct AbortOnDrop<T>(JoinHandle<T>);

impl<T> Drop for AbortOnDrop<T> {
    fn drop(&mut self) {
        self.0.abort();
    }
}

async fn run(daemon: Daemon, socket: WebSocketStream<TcpStream>, admitted: &Admitted) {
    let (mut sink, mut source) = socket.split();
    let (mut session, mut frames) = Session::web(&daemon, admitted.lease.clone());
    let (ctl, mut ctl_rx) = mpsc::unbounded_channel::<Ctl>();
    let mut close = admitted.close.clone();

    // The writer: frames as they are queued, pings, and the close. It holds
    // the connection's lease, and is aborted if this task is.
    let mut writer = {
        let mut close = close.clone();
        AbortOnDrop(spawn_held(Some(admitted.lease.clone()), async move {
            let mut ping =
                tokio::time::interval_at(tokio::time::Instant::now() + PING_EVERY, PING_EVERY);
            let last: Option<CloseFrame> = loop {
                tokio::select! {
                    biased;
                    frame = frames.recv() => {
                        // The session ended: the reader said why before it
                        // ended it, or the server is stopping.
                        let Some(frame) = frame else {
                            break goodbye(ctl_rx.try_recv().ok(), *close.borrow());
                        };
                        let Ok(text) = serde_json::to_string(&frame) else { continue };
                        if sink.send(Message::text(text)).await.is_err() {
                            return None;
                        }
                    }
                    order = ctl_rx.recv() => break goodbye(order, *close.borrow()),
                    _ = close.changed() => {
                        // What is queued (a `shutdown` push) goes first.
                        while let Ok(frame) = frames.try_recv() {
                            let Ok(text) = serde_json::to_string(&frame) else { continue };
                            if sink.send(Message::text(text)).await.is_err() {
                                return None;
                            }
                        }
                        break goodbye(None, true);
                    }
                    _ = ping.tick() => {
                        if sink.send(Message::Ping(Vec::new().into())).await.is_err() {
                            return None;
                        }
                    }
                }
            };
            if let Some(frame) = last {
                let _ = sink.send(Message::Close(Some(frame))).await;
            }
            let _ = sink.close().await;
            Some(sink)
        }))
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
        let handled = if session.authenticated() {
            session.request(text.as_str()).await
        } else {
            // Before hello a request does nothing that takes time; what can
            // wait is its reply's place in the queue, behind a browser that
            // does not read. The deadline holds through it.
            match tokio::time::timeout_at(hello_by, session.request(text.as_str())).await {
                Ok(handled) => handled,
                Err(_) => {
                    let _ = ctl.send(Ctl::Close(CloseCode::Policy, "say hello first"));
                    break;
                }
            }
        };
        if !handled {
            break;
        }
        if session.close_requested() {
            let _ = ctl.send(Ctl::Close(CloseCode::Policy, "refused"));
            break;
        }
    }
    drop(session);
    drop(ctl);
    let sink = tokio::time::timeout(Duration::from_secs(2), &mut writer.0)
        .await
        .ok()
        .and_then(Result::ok)
        .flatten();
    // A lingering close: the end of the stream is sent, and what the browser
    // still sends (the rest of a message too large, its own close) is read
    // and dropped until it closes too. Closing with that unread would reset
    // the connection and lose the close frame on the way.
    let Some(sink) = sink else { return };
    let Ok(mut socket) = source.reunite(sink) else {
        return;
    };
    let stream = socket.get_mut();
    if stream.shutdown().await.is_err() {
        return;
    }
    let mut buf = vec![0u8; 16 * 1024];
    let _ = tokio::time::timeout(LINGER, async {
        while matches!(stream.read(&mut buf).await, Ok(n) if n > 0) {}
    })
    .await;
}
