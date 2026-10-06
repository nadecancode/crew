//! Verify the selected daemon on the same socket that will carry device calls.

use std::time::Duration;

use crew_core::paths::DaemonInfo;
use crew_core::rpc::{Call, DaemonRef, HelloInfo, Request, Role, ServerFrame, Snapshot};
use crew_protocol::native::{GatewayRoute, WorkspaceId};
use tokio::io::AsyncWriteExt;
use tokio::sync::watch;

use super::service::Gateway;
use super::store::{Device, Membership, MEMBERSHIP_GENERATION};
use crate::web::daemons::{self, Daemons, Dialled, Registered};

pub(crate) async fn open(
    gateway: &Gateway,
    device: &Device,
    workspace: WorkspaceId,
    connection: u64,
    cancel: &mut watch::Receiver<bool>,
) -> Option<(Dialled, GatewayRoute)> {
    if *cancel.borrow() {
        return None;
    }
    let member = gateway.route_membership(device, workspace)?;
    if !gateway.acquire_pending(connection) {
        return None;
    }
    let verify = async {
        // Discovery and socket validation are fresh for every channel. An
        // earlier listing or summary watch can still name a stopped daemon.
        let info = Registered
            .sessions()
            .await
            .into_iter()
            .find(|info| info.state_dir == member.state_dir)?;
        if info.workspace != member.workspace || Registered.check(&info).is_err() {
            return None;
        }
        if gateway.route_membership(device, workspace).as_ref() != Some(&member) {
            return None;
        }
        let mut dial = Registered.dial(&info, true).await.ok()?;
        if dial.hello.protocol != crew_core::rpc::PROTOCOL_VERSION
            || dial.hello.role != Role::Operator
            || dial.hello.pid != info.pid
        {
            return None;
        }
        if gateway.route_membership(device, workspace).as_ref() != Some(&member) {
            return None;
        }
        // This request belongs to the gateway, not the device or its purpose.
        // Both the hello and this reply are consumed before the opened result.
        let request = Request {
            id: 0,
            call: Call::Snapshot {
                messages: Some(0),
                events: Some(0),
            },
        };
        let mut bytes = serde_json::to_vec(&request).ok()?;
        bytes.push(b'\n');
        dial.writer.write_all(&bytes).await.ok()?;
        let mut buffer = Vec::new();
        daemons::read_line(&mut dial.lines, &mut buffer, crew_core::MAX_FRAME_BYTES).await?;
        let frame: ServerFrame = serde_json::from_slice(&buffer).ok()?;
        if frame.id != Some(0) || frame.error.is_some() || frame.push.is_some() {
            return None;
        }
        let snapshot: Snapshot = serde_json::from_value(frame.result?).ok()?;
        let route = checked_route(&member, &info, &dial.hello, &snapshot)?;
        Some((dial, route))
    };
    let opened = tokio::select! {
        opened = tokio::time::timeout(Duration::from_secs(3), verify) => opened.ok().flatten(),
        _ = cancel.changed() => None,
    };
    gateway.release_pending(connection);
    if *cancel.borrow() || gateway.route_membership(device, workspace).as_ref() != Some(&member) {
        return None;
    }
    opened
}

fn checked_route(
    member: &Membership,
    info: &DaemonInfo,
    hello: &HelloInfo,
    snapshot: &Snapshot,
) -> Option<GatewayRoute> {
    let session = &snapshot.session;
    if info.workspace != member.workspace
        || info.state_dir != member.state_dir
        || session.workspace != member.workspace
        || session.state_dir != member.state_dir
        || hello.pid != info.pid
        || session.daemon_pid != hello.pid
        || session.daemon_started_at != info.started_at
    {
        return None;
    }
    Some(GatewayRoute {
        workspace_id: member.id,
        workspace_generation: MEMBERSHIP_GENERATION,
        daemon: DaemonRef {
            pid: session.daemon_pid,
            started_at: session.daemon_started_at,
        },
    })
}
