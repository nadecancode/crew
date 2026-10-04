//! The daemon's Unix-socket server: connection handling and authentication.
//!
//! Access control has three layers: the socket lives in a `0700` directory
//! and is `0600`; the peer's uid is checked on accept; and a connection that
//! claims to be an agent must present that agent's token. The daemon stamps
//! the sender on everything, so an agent cannot speak as another agent.

use crew_core::paths::uid;
use crew_core::rpc::{Call, Push, Request, Role, RpcError, ServerFrame};
use crew_core::util::ct_eq;
use crew_core::MAX_FRAME_BYTES;
use serde_json::Value;
use tokio::io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader};
use tokio::net::{UnixListener, UnixStream};
use tokio::sync::{broadcast, mpsc};
use tokio::task::JoinHandle;

use super::Daemon;

/// Who a connection has authenticated as.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum Who {
    Operator,
    Agent(String),
}

impl Who {
    /// The sender id stamped on messages from this connection.
    pub fn sender(&self) -> &str {
        match self {
            Who::Operator => crew_core::OPERATOR_ID,
            Who::Agent(id) => id,
        }
    }
}

pub(crate) struct Conn {
    pub id: u64,
    pub who: Option<Who>,
    pub tx: mpsc::Sender<ServerFrame>,
    /// Start streaming pushes once the pending response has been written.
    pub subscribe_after_reply: bool,
    /// This connection is an agent's delivery channel.
    pub listening: Option<String>,
    /// Forwarder for pushes or terminal output on this connection.
    pub stream_task: Option<JoinHandle<()>>,
    /// Agent whose terminal this connection is attached to.
    pub attached: Option<String>,
}

pub(crate) async fn accept_loop(daemon: Daemon, listener: UnixListener) {
    loop {
        match listener.accept().await {
            Ok((stream, _)) => {
                let daemon = daemon.clone();
                tokio::spawn(async move {
                    handle(daemon, stream).await;
                });
            }
            Err(e) => {
                tracing::error!("accept failed: {e}");
                tokio::time::sleep(std::time::Duration::from_millis(100)).await;
            }
        }
    }
}

async fn handle(daemon: Daemon, stream: UnixStream) {
    // Only the user who owns the daemon may talk to it.
    match stream.peer_cred() {
        Ok(cred) if cred.uid() == uid() => {}
        Ok(cred) => {
            tracing::warn!("rejected connection from uid {}", cred.uid());
            return;
        }
        Err(e) => {
            tracing::warn!("rejected connection without credentials: {e}");
            return;
        }
    }
    let (read, mut write) = stream.into_split();
    let (tx, mut rx) = mpsc::channel::<ServerFrame>(1024);
    let writer = tokio::spawn(async move {
        while let Some(frame) = rx.recv().await {
            let Ok(mut line) = serde_json::to_vec(&frame) else {
                continue;
            };
            line.push(b'\n');
            if write.write_all(&line).await.is_err() {
                break;
            }
        }
        let _ = write.shutdown().await;
    });

    let mut conn = Conn {
        id: daemon
            .next_conn
            .fetch_add(1, std::sync::atomic::Ordering::Relaxed),
        who: None,
        tx,
        subscribe_after_reply: false,
        listening: None,
        stream_task: None,
        attached: None,
    };
    let mut reader = BufReader::new(read);
    let mut buf: Vec<u8> = Vec::new();
    loop {
        buf.clear();
        // Bound the line length so a client cannot make the daemon buffer
        // without limit.
        let mut limited = (&mut reader).take(MAX_FRAME_BYTES as u64 + 1);
        match limited.read_until(b'\n', &mut buf).await {
            Ok(0) | Err(_) => break,
            Ok(_) => {}
        }
        if buf.len() > MAX_FRAME_BYTES {
            let _ = conn
                .tx
                .send(ServerFrame::err(
                    0,
                    RpcError::too_large(format!("request exceeds {MAX_FRAME_BYTES} bytes")),
                ))
                .await;
            break;
        }
        let line = String::from_utf8_lossy(&buf);
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        let frame = match serde_json::from_str::<Request>(line) {
            Ok(request) => {
                let id = request.id;
                // Input for an attached terminal is not acknowledged.
                let silent = matches!(
                    request.call,
                    Call::AttachInput { .. } | Call::AttachResize { .. }
                );
                match daemon.dispatch(&mut conn, request.call).await {
                    Ok(_) if silent => continue,
                    Ok(result) => ServerFrame::ok(id, result),
                    Err(error) => ServerFrame::err(id, error),
                }
            }
            Err(e) => {
                // Malformed request: answer with the id if one can be salvaged.
                let id = serde_json::from_str::<Value>(line)
                    .ok()
                    .and_then(|v| v.get("id").and_then(Value::as_u64))
                    .unwrap_or(0);
                ServerFrame::err(id, RpcError::invalid(format!("malformed request: {e}")))
            }
        };
        if conn.tx.send(frame).await.is_err() {
            break;
        }
        if std::mem::take(&mut conn.subscribe_after_reply) {
            // Subscribing only now guarantees the response precedes any push.
            daemon.start_subscription(&mut conn);
        }
    }
    if let Some(task) = conn.stream_task.take() {
        task.abort();
    }
    if let Some(agent) = conn.listening.take() {
        daemon.listener_gone(&agent, conn.id);
    }
    drop(conn);
    let _ = writer.await;
}

impl Daemon {
    /// Authenticate a connection.
    pub(crate) fn authenticate(
        &self,
        role: Role,
        agent_id: Option<String>,
        token: Option<String>,
    ) -> Result<Who, RpcError> {
        match role {
            Role::Operator => Ok(Who::Operator),
            Role::Agent => {
                let id = agent_id.ok_or_else(|| RpcError::unauthorized("agent_id is required"))?;
                let token = token.ok_or_else(|| RpcError::unauthorized("token is required"))?;
                let model = self.model();
                let expected = model
                    .tokens
                    .get(&id)
                    .ok_or_else(|| RpcError::unauthorized("unknown agent or bad token"))?;
                if !ct_eq(expected, &token) {
                    return Err(RpcError::unauthorized("unknown agent or bad token"));
                }
                if model.agent(&id).is_none() {
                    return Err(RpcError::unauthorized("unknown agent or bad token"));
                }
                Ok(Who::Agent(id))
            }
        }
    }

    /// Start streaming pushes to a connection.
    pub(crate) fn start_subscription(&self, conn: &mut Conn) {
        if let Some(task) = conn.stream_task.take() {
            task.abort();
        }
        let mut rx = self.push.subscribe();
        let tx = conn.tx.clone();
        conn.stream_task = Some(tokio::spawn(async move {
            loop {
                let frame = match rx.recv().await {
                    Ok(push) => ServerFrame::push(push),
                    // The subscriber fell behind; tell it to refetch.
                    Err(broadcast::error::RecvError::Lagged(_)) => ServerFrame::push(Push::Resync),
                    Err(broadcast::error::RecvError::Closed) => break,
                };
                if tx.send(frame).await.is_err() {
                    break;
                }
            }
        }));
    }
}
