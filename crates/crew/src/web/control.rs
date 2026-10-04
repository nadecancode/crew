//! The dashboard server's control socket, `web.sock` (notes/web-multi.md
//! §multi-server "The control socket"): `crate::rpc` frames, one per line,
//! like a daemon's socket, for `crew web`. Only this user, and only with the
//! token of `web.token`, which no browser and no agent has.
//!
//! | call | does |
//! |---|---|
//! | `ping`, `hello` | as on a daemon |
//! | `web_open {workspace?, all?, restore?, port?}` | joins, makes a pairing code |
//! | `web_close {workspace?}` | takes a workspace out; without one, stops the server |
//! | `workspaces` | the listing |
//! | `web_rescan` | asks for a rescan |

use std::sync::Arc;

use crew_core::paths;
use crew_core::rpc::{Call, HelloInfo, Request, Role, RpcError, ServerFrame, PROTOCOL_VERSION};
use crew_core::util::ct_eq;
use serde_json::{json, Value};
use tokio::io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader};
use tokio::net::{UnixListener, UnixStream};

use super::process::Server;

/// The longest request line.
const MAX_LINE: u64 = 64 * 1024;

pub(crate) async fn accept(server: Arc<Server>, listener: UnixListener, token: String) {
    let token = Arc::new(token);
    loop {
        let Ok((stream, _)) = listener.accept().await else {
            tokio::time::sleep(std::time::Duration::from_millis(50)).await;
            continue;
        };
        tokio::spawn(handle(server.clone(), stream, token.clone()));
    }
}

async fn handle(server: Arc<Server>, stream: UnixStream, token: Arc<String>) {
    if !stream
        .peer_cred()
        .is_ok_and(|cred| cred.uid() == paths::uid())
    {
        return;
    }
    let (read, mut write) = stream.into_split();
    let mut reader = BufReader::new(read);
    let mut signed_in = false;
    let mut line = Vec::new();
    loop {
        line.clear();
        match (&mut reader)
            .take(MAX_LINE)
            .read_until(b'\n', &mut line)
            .await
        {
            Ok(0) | Err(_) => return,
            Ok(_) => {}
        }
        let text = String::from_utf8_lossy(&line);
        let frame = match serde_json::from_str::<Request>(text.trim()) {
            Err(e) => {
                let id = serde_json::from_str::<Value>(text.trim())
                    .ok()
                    .and_then(|v| v.get("id").and_then(Value::as_u64))
                    .unwrap_or(0);
                ServerFrame::err(id, RpcError::invalid(format!("malformed request: {e}")))
            }
            Ok(request) => {
                let id = request.id;
                match answer(&server, &token, &mut signed_in, request.call).await {
                    Ok(result) => ServerFrame::ok(id, result),
                    Err(error) => ServerFrame::err(id, error),
                }
            }
        };
        let Ok(mut out) = serde_json::to_vec(&frame) else {
            return;
        };
        out.push(b'\n');
        if write.write_all(&out).await.is_err() {
            return;
        }
    }
}

async fn answer(
    server: &Arc<Server>,
    token: &str,
    signed_in: &mut bool,
    call: Call,
) -> Result<Value, RpcError> {
    if let Call::Ping = call {
        return Ok(json!({"pong": true}));
    }
    if let Call::Hello(hello) = call {
        if *signed_in {
            return Err(RpcError::forbidden("this connection said hello already"));
        }
        if hello.protocol != PROTOCOL_VERSION {
            return Err(RpcError::new(
                "protocol",
                format!(
                    "client speaks protocol {}, the dashboard server speaks {PROTOCOL_VERSION}",
                    hello.protocol
                ),
            ));
        }
        let given = hello.token.unwrap_or_default();
        if hello.role != Role::Operator || !ct_eq(token, &given) {
            return Err(RpcError::unauthorized(
                "the dashboard server's control socket takes the token of web.token",
            ));
        }
        *signed_in = true;
        return serde_json::to_value(HelloInfo {
            role: Role::Operator,
            agent_id: None,
            project: None,
            version: crate::daemon::VERSION.to_string(),
            protocol: PROTOCOL_VERSION,
            pid: std::process::id(),
            web_proof: None,
        })
        .map_err(|e| RpcError::internal(e.to_string()));
    }
    if !*signed_in {
        return Err(RpcError::unauthorized("say hello first"));
    }
    match call {
        Call::WebOpen {
            port,
            workspace,
            all,
            restore,
        } => {
            let first = !server.opened();
            let opened = server.open(port, workspace, all, restore).await;
            // A server whose first `web_open` leaves it empty does not
            // linger.
            if first && server.hub.is_empty() {
                server.request_stop("its first web_open joined nothing");
            }
            serde_json::to_value(opened?).map_err(|e| RpcError::internal(e.to_string()))
        }
        Call::WebClose {
            workspace: Some(which),
        } => {
            server.hub.remove(&which, &server.web).await?;
            Ok(json!({"removed": true}))
        }
        Call::WebClose { workspace: None } => {
            let shown = server.hub.listing().joined.len();
            server.shut(None).await;
            Ok(json!({"stopped": true, "workspaces": shown}))
        }
        Call::Workspaces => serde_json::to_value(server.hub.listing())
            .map_err(|e| RpcError::internal(e.to_string())),
        Call::WebRescan => {
            server.hub.rescan();
            Ok(json!({"rescanning": true}))
        }
        call => Err(RpcError::invalid(format!(
            "{} is not a call of the dashboard server's control socket",
            call.method()
        ))),
    }
}
