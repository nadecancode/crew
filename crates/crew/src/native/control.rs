//! Private local administration, never reachable through the TLS listener.

use std::sync::Arc;
use std::time::Duration;

use crew_core::rpc::{Hello, RpcError, ServerFrame};
use crew_protocol::native::{ComparisonCode, DeviceId, EnrollmentId, HexBytes, WorkspaceId};
use serde::Deserialize;
use serde_json::{json, Value};
use tokio::io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader};
use tokio::net::{UnixListener, UnixStream};

use super::service::{lock, Gateway, GrantSelection};

#[derive(Deserialize)]
#[serde(
    tag = "method",
    content = "params",
    rename_all = "snake_case",
    deny_unknown_fields
)]
enum AdminCall {
    Hello(Hello),
    NativeStatus {},
    NativeEnroll {},
    NativePending {},
    NativeDevices {},
    NativeApprove {
        enrollment_id: EnrollmentId,
        comparison_code: ComparisonCode,
        public_key_hash: HexBytes<32>,
        grants: Vec<GrantSelection>,
        lifetime_secs: u64,
    },
    NativeGrant {
        device_id: DeviceId,
        selection: GrantSelection,
    },
    NativeRevoke {
        device_id: DeviceId,
        #[serde(default)]
        workspace: Option<WorkspaceId>,
    },
    NativeStop {},
}

pub(crate) async fn accept(gateway: Arc<Gateway>, listener: UnixListener, token: String) {
    let mut stop = gateway.stopping.subscribe();
    let capacity = Arc::new(tokio::sync::Semaphore::new(16));
    let mut tasks = tokio::task::JoinSet::new();
    loop {
        let stream =
            tokio::select! { result = listener.accept() => result, _ = stop.changed() => break };
        let Ok((stream, _)) = stream else {
            break;
        };
        let Ok(permit) = capacity.clone().try_acquire_owned() else {
            continue;
        };
        let gateway = gateway.clone();
        let token = token.clone();
        tasks.spawn(async move {
            let _permit = permit;
            handle(gateway, stream, token).await;
        });
        while tasks.try_join_next().is_some() {}
    }
    // Give the stop reply a chance to leave, then retire every old control
    // session before its credential files or startup lock can be reused.
    let _ = tokio::time::timeout(Duration::from_millis(200), async {
        while tasks.join_next().await.is_some() {}
    })
    .await;
    tasks.abort_all();
    while tasks.join_next().await.is_some() {}
}

async fn handle(gateway: Arc<Gateway>, stream: UnixStream, token: String) {
    if !stream
        .peer_cred()
        .is_ok_and(|peer| peer.uid() == crew_core::paths::uid())
    {
        return;
    }
    let (read, mut write) = stream.into_split();
    let mut reader = BufReader::new(read);
    let mut authenticated = false;
    loop {
        let mut bytes = Vec::new();
        let read = tokio::time::timeout(
            Duration::from_secs(10),
            (&mut reader)
                .take(64 * 1024 + 1)
                .read_until(b'\n', &mut bytes),
        )
        .await;
        if !matches!(read, Ok(Ok(n)) if n > 0) || bytes.len() > 64 * 1024 || !bytes.ends_with(b"\n")
        {
            return;
        }
        let value = serde_json::from_slice::<Value>(&bytes);
        let id = value
            .as_ref()
            .ok()
            .and_then(|v| v.get("id"))
            .and_then(Value::as_u64)
            .unwrap_or(0);
        let call = value.and_then(|mut value| {
            let map = value.as_object_mut().ok_or_else(|| {
                serde_json::Error::io(std::io::Error::from(std::io::ErrorKind::InvalidData))
            })?;
            if map
                .keys()
                .any(|key| !matches!(key.as_str(), "id" | "method" | "params"))
            {
                return Err(serde_json::Error::io(std::io::Error::from(
                    std::io::ErrorKind::InvalidData,
                )));
            }
            map.remove("id");
            serde_json::from_value::<AdminCall>(value)
        });
        let result = match call {
            Ok(AdminCall::Hello(hello)) => {
                if authenticated {
                    Err(RpcError::forbidden("native control already authenticated"))
                } else if hello.protocol != crew_core::rpc::PROTOCOL_VERSION {
                    Err(RpcError::new(
                        "protocol",
                        "native control protocol is incompatible",
                    ))
                } else if hello.role != crew_core::rpc::Role::Operator
                    || hello.agent_id.is_some()
                    || hello.web
                    || !crew_core::util::ct_eq(&token, hello.token.as_deref().unwrap_or_default())
                {
                    Err(RpcError::unauthorized("native control credential required"))
                } else {
                    authenticated = true;
                    Ok(json!({"authenticated":true}))
                }
            }
            Ok(call) if authenticated => answer(&gateway, call).await,
            Ok(_) => Err(RpcError::unauthorized("native control credential required")),
            Err(_) => Err(RpcError::invalid("invalid native control request")),
        };
        let frame = match result {
            Ok(value) => ServerFrame::ok(id, value),
            Err(error) => ServerFrame::err(id, error),
        };
        let Ok(mut bytes) = serde_json::to_vec(&frame) else {
            return;
        };
        bytes.push(b'\n');
        if !matches!(
            tokio::time::timeout(Duration::from_secs(3), write.write_all(&bytes)).await,
            Ok(Ok(()))
        ) {
            return;
        }
    }
}

async fn answer(gateway: &Gateway, call: AdminCall) -> Result<Value, RpcError> {
    if lock(&gateway.state).stopping {
        return Err(RpcError::unavailable("native gateway is stopping"));
    }
    let error = |_: anyhow::Error| RpcError::invalid("native administrative operation unavailable");
    match call {
        AdminCall::Hello(_) => unreachable!(),
        AdminCall::NativeStatus {} => {
            Ok(json!({"profile":gateway.profile(),"endpoint_epoch":gateway.epoch}))
        }
        AdminCall::NativeEnroll {} => {
            gateway.open_enrollment().map_err(error)?;
            Ok(json!({"enrollment_open":true,"expires_in_secs":120}))
        }
        AdminCall::NativePending {} => Ok(gateway.pending()),
        AdminCall::NativeDevices {} => Ok(json!(lock(&gateway.state)
            .store
            .registry
            .devices
            .values()
            .collect::<Vec<_>>())),
        AdminCall::NativeApprove {
            enrollment_id,
            comparison_code,
            public_key_hash,
            grants,
            lifetime_secs,
        } => {
            let id = gateway
                .approve(
                    enrollment_id,
                    comparison_code,
                    public_key_hash,
                    grants,
                    lifetime_secs,
                )
                .map_err(error)?;
            Ok(json!({"device_id":id,"approved":true}))
        }
        AdminCall::NativeGrant {
            device_id,
            selection,
        } => {
            gateway
                .change_grant(device_id, selection)
                .await
                .map_err(error)?;
            Ok(json!({"granted":true}))
        }
        AdminCall::NativeRevoke {
            device_id,
            workspace,
        } => {
            gateway.revoke(device_id, workspace).await.map_err(error)?;
            Ok(json!({"revoked":true}))
        }
        AdminCall::NativeStop {} => {
            gateway.stop().await.map_err(error)?;
            Ok(json!({"stopped":true}))
        }
    }
}
