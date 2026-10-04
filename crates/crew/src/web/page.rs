//! A page's connection to the dashboard server (notes/web-multi.md
//! §multi-transport): every WebSocket starts with `web_pair` (once per
//! browser) and `hello`, answered by the server itself.
//!
//! - `hello` without `workspace` makes the **index** connection: the
//!   listing (`workspaces`, and a push when it changed: the first at once,
//!   then at most one per 100 ms, each as it is when sent), the dashboard's
//!   preferences, `ping`.
//! - `hello` with `workspace` makes a **workspace** connection: admitted
//!   under the membership's generation, its daemon dialled for the browser
//!   (`web: true`), and from then on lines passed through unparsed, both
//!   ways. It ends with 4001 when the daemon's side ends, with 4002 when the
//!   workspace was taken out; either way the daemon's side is let go before
//!   the goodbye.
//!
//! Before `hello`: 5 s, 4 KiB, `ping`, `web_pair`, `hello`. A text message
//! with a line break closes with 1008: on a daemon's socket it would be two
//! requests.

use std::sync::Arc;
use std::time::Duration;

use crew_core::rpc::{
    Call, HelloInfo, Push, Request, Role, RpcError, ServerFrame, CLOSE_WORKSPACE_ENDED,
    CLOSE_WORKSPACE_REMOVED, PROTOCOL_VERSION,
};
use futures_util::stream::{SplitSink, SplitStream};
use futures_util::{SinkExt, StreamExt};
use serde_json::{json, Value};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpStream;
use tokio::sync::{mpsc, watch};
use tokio_tungstenite::tungstenite::protocol::frame::coding::CloseCode;
use tokio_tungstenite::tungstenite::protocol::CloseFrame;
use tokio_tungstenite::tungstenite::{Error as WsError, Message};
use tokio_tungstenite::WebSocketStream;

use super::daemons::{self, Dialled};
use super::hub::Admission;
use super::process::Server;
use super::ws::{
    self, AbortOnDrop, HELLO_TIMEOUT, MAX_UNAUTHENTICATED_MESSAGE, PING_EVERY, SILENT_FOR,
};
use super::{spawn_held, Admitted};

/// A page's index connection gets at most one listing push per this long.
pub(crate) const PUSH_EVERY: Duration = Duration::from_millis(100);
const REMOVED: &str = "taken out of this dashboard";
const LONGEST_LINE: usize = crew_core::MAX_FRAME_BYTES + 1024;

type Sink = SplitSink<WebSocketStream<TcpStream>, Message>;
type Source = SplitStream<WebSocketStream<TcpStream>>;

/// What the connection's task tells its writer.
enum Out {
    Text(String),
    Close(u16, &'static str),
}

/// How a connection ends: the close frame it gets, if any.
type Goodbye = Option<(u16, &'static str)>;

const AWAY: Goodbye = Some((1001, "the dashboard was stopped"));

pub(crate) async fn serve(server: Arc<Server>, stream: TcpStream, admitted: Admitted) {
    let Some(serving) = server.web.serving() else {
        return;
    };
    let mut close = admitted.close.clone();
    let Some(socket) = ws::upgrade(stream, &serving.host(), &mut close).await else {
        return;
    };
    let (sink, source) = socket.split();
    // One frame is buffered, not a queue: a browser that does not read
    // holds the connection's task, which stops reading the daemon.
    let (out, rx) = mpsc::channel::<Out>(1);
    let mut writer = AbortOnDrop(spawn_held(Some(admitted.lease.clone()), write(sink, rx)));
    let mut conn = Conn {
        server,
        source,
        out,
        close,
        lease: admitted.lease.id,
    };
    let goodbye = conn.run().await;
    let Conn { source, out, .. } = conn;
    if let Some((code, reason)) = goodbye {
        let _ =
            tokio::time::timeout(Duration::from_secs(1), out.send(Out::Close(code, reason))).await;
    }
    drop(out);
    let sink = tokio::time::timeout(Duration::from_secs(2), &mut writer.0)
        .await
        .ok()
        .and_then(Result::ok)
        .flatten();
    linger(source, sink).await;
}

/// The writer: what the task queues, pings, the close.
async fn write(mut sink: Sink, mut rx: mpsc::Receiver<Out>) -> Option<Sink> {
    let mut ping = tokio::time::interval_at(tokio::time::Instant::now() + PING_EVERY, PING_EVERY);
    loop {
        tokio::select! {
            biased;
            out = rx.recv() => match out {
                Some(Out::Text(text)) => {
                    if sink.send(Message::text(text)).await.is_err() {
                        return None;
                    }
                }
                Some(Out::Close(code, reason)) => {
                    let frame = CloseFrame { code: CloseCode::from(code), reason: reason.into() };
                    let _ = sink.send(Message::Close(Some(frame))).await;
                    break;
                }
                None => break,
            },
            _ = ping.tick() => {
                if sink.send(Message::Ping(Vec::new().into())).await.is_err() {
                    return None;
                }
            }
        }
    }
    let _ = sink.close().await;
    Some(sink)
}

/// A lingering close: the end of the stream is sent, and what the browser
/// still sends is read and dropped until it closes too (closing with that
/// unread would reset the connection under the close frame).
async fn linger(source: Source, sink: Option<Sink>) {
    let Some(sink) = sink else { return };
    let Ok(mut socket) = source.reunite(sink) else {
        return;
    };
    let stream = socket.get_mut();
    if stream.shutdown().await.is_err() {
        return;
    }
    let mut buf = vec![0u8; 16 * 1024];
    let _ = tokio::time::timeout(ws::LINGER, async {
        while matches!(stream.read(&mut buf).await, Ok(n) if n > 0) {}
    })
    .await;
}

/// One message from the browser, as the connection sees it.
enum Received {
    Text(String),
    /// Ends the connection, with this goodbye.
    End(Goodbye),
    /// Nothing to act on (a pong).
    Nothing,
}

fn received(message: Option<Result<Message, WsError>>) -> Received {
    match message {
        None | Some(Ok(Message::Close(_))) => Received::End(None),
        Some(Ok(Message::Text(text))) => {
            let text = text.as_str();
            if text.bytes().any(|b| b == b'\n' || b == b'\r') {
                return Received::End(Some((1008, "one request per message: no line breaks")));
            }
            Received::Text(text.to_string())
        }
        Some(Ok(Message::Binary(_))) => Received::End(Some((1003, "text messages only"))),
        Some(Ok(_)) => Received::Nothing,
        Some(Err(WsError::Capacity(_))) => Received::End(Some((1009, "message too large"))),
        Some(Err(_)) => Received::End(None),
    }
}

struct Conn {
    server: Arc<Server>,
    source: Source,
    out: mpsc::Sender<Out>,
    close: watch::Receiver<bool>,
    lease: u64,
}

impl Conn {
    async fn send(&self, frame: &ServerFrame) -> bool {
        let Ok(text) = serde_json::to_string(frame) else {
            return true;
        };
        self.out.send(Out::Text(text)).await.is_ok()
    }

    async fn run(&mut self) -> Goodbye {
        let hello_by = tokio::time::Instant::now() + HELLO_TIMEOUT;
        loop {
            let message = tokio::select! {
                message = self.source.next() => message,
                _ = self.close.changed() => return AWAY,
                _ = tokio::time::sleep_until(hello_by) => return Some((1008, "say hello first")),
            };
            let text = match received(message) {
                Received::Text(text) => text,
                Received::End(goodbye) => return goodbye,
                Received::Nothing => continue,
            };
            if text.len() > MAX_UNAUTHENTICATED_MESSAGE {
                return Some((1009, "message too large before hello"));
            }
            let request = match serde_json::from_str::<Request>(&text) {
                Ok(request) => request,
                Err(e) => {
                    let id = salvaged_id(&text);
                    let frame =
                        ServerFrame::err(id, RpcError::invalid(format!("malformed request: {e}")));
                    if !self.reply_before(hello_by, &frame).await {
                        return Some((1008, "say hello first"));
                    }
                    continue;
                }
            };
            let id = request.id;
            let web = self.server.web.clone();
            let frame = match request.call {
                Call::Ping => ServerFrame::ok(id, json!({"pong": true})),
                Call::WebPair { code } => match web.pair(&code) {
                    Ok(paired) => {
                        ServerFrame::ok(id, serde_json::to_value(paired).unwrap_or_default())
                    }
                    Err(e) => ServerFrame::err(id, e),
                },
                Call::Hello(hello) => {
                    if hello.protocol != PROTOCOL_VERSION {
                        web.refused();
                        ServerFrame::err(
                            id,
                            RpcError::new(
                                "protocol",
                                format!(
                                    "client speaks protocol {}, the dashboard server speaks {PROTOCOL_VERSION}",
                                    hello.protocol
                                ),
                            ),
                        )
                    } else if hello.role != Role::Operator {
                        web.refused();
                        ServerFrame::err(
                            id,
                            RpcError::unauthorized(
                                "the web dashboard signs in as the operator only",
                            ),
                        )
                    } else {
                        match web.hello(self.lease, hello.token.as_deref()) {
                            Err(e) => {
                                let full = e.code == "unavailable";
                                if !self.reply_before(hello_by, &ServerFrame::err(id, e)).await
                                    || full
                                {
                                    return Some((1008, "refused"));
                                }
                                continue;
                            }
                            Ok((_, secret)) => {
                                return match hello.workspace {
                                    None => self.index(id, secret).await,
                                    Some(workspace) => self.workspace(id, workspace, secret).await,
                                };
                            }
                        }
                    }
                }
                _ => ServerFrame::err(id, RpcError::unauthorized("say hello first")),
            };
            if !self.reply_before(hello_by, &frame).await {
                return Some((1008, "say hello first"));
            }
        }
    }

    /// Before `hello` the deadline holds through a reply that waits for
    /// room behind a browser that does not read.
    async fn reply_before(&self, by: tokio::time::Instant, frame: &ServerFrame) -> bool {
        matches!(
            tokio::time::timeout_at(by, self.send(frame)).await,
            Ok(true)
        )
    }

    /// The index connection, the page's link to its server.
    async fn index(&mut self, hello_id: u64, secret: String) -> Goodbye {
        let hub = self.server.hub.clone();
        let reply = HelloInfo {
            role: Role::Operator,
            agent_id: None,
            project: None,
            version: crate::daemon::VERSION.to_string(),
            protocol: PROTOCOL_VERSION,
            pid: std::process::id(),
            web_proof: Some(secret),
        };
        if !self
            .send(&ServerFrame::ok(
                hello_id,
                serde_json::to_value(&reply).unwrap_or_default(),
            ))
            .await
        {
            return None;
        }
        let mut changes = hub.listing_changes();
        // What it has now it asks for (`workspaces`); a push follows a change.
        let mut pushed = *changes.borrow_and_update();
        let mut due = false;
        let mut next_push = tokio::time::Instant::now();
        let mut silent_from = tokio::time::Instant::now();
        loop {
            enum Step {
                Message(Option<Result<Message, WsError>>),
                Changed(bool),
                Push,
                Close,
                Silent,
            }
            let step = tokio::select! {
                message = self.source.next() => Step::Message(message),
                changed = changes.changed() => Step::Changed(changed.is_ok()),
                _ = tokio::time::sleep_until(next_push), if due => Step::Push,
                _ = self.close.changed() => Step::Close,
                _ = tokio::time::sleep_until(silent_from + SILENT_FOR) => Step::Silent,
            };
            match step {
                Step::Message(message) => {
                    silent_from = tokio::time::Instant::now();
                    let text = match received(message) {
                        Received::Text(text) => text,
                        Received::End(goodbye) => return goodbye,
                        Received::Nothing => continue,
                    };
                    let frame = self.index_call(&text);
                    if !self.send(&frame).await {
                        return None;
                    }
                }
                Step::Changed(true) => due = *changes.borrow_and_update() > pushed,
                Step::Changed(false) => return AWAY,
                Step::Push => {
                    due = false;
                    let listing = hub.listing();
                    if listing.rev > pushed {
                        pushed = listing.rev;
                        if !self
                            .send(&ServerFrame::push(Push::Workspaces(listing)))
                            .await
                        {
                            return None;
                        }
                        next_push = tokio::time::Instant::now() + PUSH_EVERY;
                    }
                }
                Step::Close => {
                    let _ = self.send(&ServerFrame::push(Push::Shutdown)).await;
                    return AWAY;
                }
                Step::Silent => return Some((1008, "silent for too long")),
            }
        }
    }

    /// A call of the index connection.
    fn index_call(&self, text: &str) -> ServerFrame {
        let request = match serde_json::from_str::<Request>(text) {
            Ok(request) => request,
            Err(e) => {
                return ServerFrame::err(
                    salvaged_id(text),
                    RpcError::invalid(format!("malformed request: {e}")),
                )
            }
        };
        let id = request.id;
        match request.call {
            Call::Ping => ServerFrame::ok(id, json!({"pong": true})),
            Call::Workspaces => {
                self.server.hub.asked_for_listing();
                ServerFrame::ok(
                    id,
                    serde_json::to_value(self.server.hub.listing()).unwrap_or_default(),
                )
            }
            Call::WebPrefs { set } => match self.server.prefs(set) {
                Ok(prefs) => ServerFrame::ok(id, json!({"prefs": prefs})),
                Err(e) => ServerFrame::err(id, e),
            },
            call => ServerFrame::err(
                id,
                RpcError::invalid(format!(
                    "{} is not a call of the dashboard's index connection",
                    call.method()
                )),
            ),
        }
    }

    /// A workspace connection: admitted, dialled, committed, then a pipe.
    async fn workspace(&mut self, hello_id: u64, id: String, secret: String) -> Goodbye {
        let hub = self.server.hub.clone();
        let refused = Some((1008, "refused"));
        let admission = match hub.admit(&id, self.lease) {
            Ok(admission) => admission,
            Err(e) => {
                let _ = self.send(&ServerFrame::err(hello_id, e)).await;
                return refused;
            }
        };
        let mut revoked = admission.revoked.clone();
        // A removal cancels the dial: nothing waits for a daemon to answer.
        let dialled = tokio::select! {
            dialled = hub.dial(&admission) => Some(dialled),
            _ = revoked.wait_for(|r| *r) => None,
        };
        let goodbye = match dialled {
            None => {
                drop(admission);
                let _ = self.send(&ServerFrame::err(hello_id, gone(&id))).await;
                refused
            }
            Some(Err(e)) => {
                drop(admission);
                hub.reconcile(&id);
                let message = format!("{id} did not answer: {}", e.detail());
                let _ = self
                    .send(&ServerFrame::err(hello_id, RpcError::unavailable(message)))
                    .await;
                refused
            }
            Some(Ok(dialled)) => self.committed(hello_id, admission, dialled, secret).await,
        };
        hub.left(&id, self.lease);
        goodbye
    }

    async fn committed(
        &mut self,
        hello_id: u64,
        admission: Admission,
        dialled: Dialled,
        secret: String,
    ) -> Goodbye {
        #[cfg(test)]
        self.server.hooks.before_commit().await;
        let Admission {
            id,
            mut revoked,
            guard,
            ..
        } = admission;
        // Commit: the connection is the workspace's only if its generation
        // still stands.
        if *revoked.borrow() {
            drop(dialled);
            drop(guard);
            let _ = self.send(&ServerFrame::err(hello_id, gone(&id))).await;
            return Some((1008, "refused"));
        }
        let Dialled {
            mut lines,
            mut writer,
            mut hello,
        } = dialled;
        hello.web_proof = Some(secret);
        if !self
            .send(&ServerFrame::ok(
                hello_id,
                serde_json::to_value(&hello).unwrap_or_default(),
            ))
            .await
        {
            return None;
        }
        let hub = self.server.hub.clone();
        let mut buf = Vec::with_capacity(4096);
        let mut silent_from = tokio::time::Instant::now();
        // The daemon's side is let go of before any goodbye is sent.
        loop {
            enum Step {
                Revoked,
                Close,
                Daemon(Option<()>),
                Browser(Option<Result<Message, WsError>>),
                Silent,
            }
            let step = tokio::select! {
                biased;
                _ = revoked.wait_for(|r| *r) => Step::Revoked,
                _ = self.close.changed() => Step::Close,
                line = daemons::read_line(&mut lines, &mut buf, LONGEST_LINE) => Step::Daemon(line),
                message = self.source.next() => Step::Browser(message),
                _ = tokio::time::sleep_until(silent_from + SILENT_FOR) => Step::Silent,
            };
            match step {
                Step::Revoked => {
                    #[cfg(test)]
                    self.server.hooks.letting_go().await;
                    drop((lines, writer, guard));
                    return Some((CLOSE_WORKSPACE_REMOVED, REMOVED));
                }
                Step::Close => {
                    drop((lines, writer, guard));
                    return AWAY;
                }
                Step::Daemon(None) => {
                    drop((lines, writer, guard));
                    // This connection ended on the daemon's side; whether the
                    // daemon is gone is the listing's to say.
                    hub.reconcile(&id);
                    return Some((CLOSE_WORKSPACE_ENDED, "the workspace's connection ended"));
                }
                Step::Daemon(Some(())) => {
                    let Ok(text) = String::from_utf8(std::mem::take(&mut buf)) else {
                        drop((lines, writer, guard));
                        hub.reconcile(&id);
                        return Some((CLOSE_WORKSPACE_ENDED, "the workspace's connection ended"));
                    };
                    if self.out.send(Out::Text(text)).await.is_err() {
                        drop((lines, writer, guard));
                        return None;
                    }
                }
                Step::Browser(message) => {
                    silent_from = tokio::time::Instant::now();
                    match received(message) {
                        Received::Text(text) => {
                            let mut line = text.into_bytes();
                            line.push(b'\n');
                            if writer.write_all(&line).await.is_err() {
                                drop((lines, writer, guard));
                                hub.reconcile(&id);
                                return Some((
                                    CLOSE_WORKSPACE_ENDED,
                                    "the workspace's connection ended",
                                ));
                            }
                        }
                        Received::End(goodbye) => {
                            drop((lines, writer, guard));
                            return goodbye;
                        }
                        Received::Nothing => {}
                    }
                }
                Step::Silent => {
                    drop((lines, writer, guard));
                    return Some((1008, "silent for too long"));
                }
            }
        }
    }
}

fn gone(id: &str) -> RpcError {
    RpcError::not_found(format!("{id} was {REMOVED}"))
}

/// The id of a request that could not be read, if one can be salvaged.
fn salvaged_id(text: &str) -> u64 {
    serde_json::from_str::<Value>(text)
        .ok()
        .and_then(|v| v.get("id").and_then(Value::as_u64))
        .unwrap_or(0)
}
