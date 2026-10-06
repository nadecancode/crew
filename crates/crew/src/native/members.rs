//! Selected memberships only; no mutation of the browser's dashboard hub.

use std::collections::{BTreeMap, BTreeSet};
use std::sync::Arc;
use std::time::Duration;

use crew_core::paths::DaemonInfo;
use crew_core::rpc::{Call, DaemonRef, Push, Request, ServerFrame, WorkspaceEntry};
use crew_core::WorkspaceState;
use crew_protocol::native::WorkspaceId;
use tokio::io::AsyncWriteExt;
use tokio::task::JoinHandle;

use super::service::{lock, Gateway};
use super::store::{Membership, MEMBERSHIP_GENERATION};
use crate::web::daemons::{self, Daemons, Registered};

impl Gateway {
    fn update_member(&self, id: WorkspaceId, entry: WorkspaceEntry, info: Option<DaemonInfo>) {
        let changed = {
            let mut members = lock(&self.members);
            let changed = members.get(&id).is_none_or(|(old, _)| old != &entry);
            members.insert(id, (entry, info));
            changed
        };
        if changed {
            self.notify();
        }
    }
}

fn entry(
    member: &Membership,
    info: Option<&DaemonInfo>,
    state: WorkspaceState,
    detail: &str,
) -> WorkspaceEntry {
    WorkspaceEntry {
        id: member.id.to_hex(),
        generation: MEMBERSHIP_GENERATION,
        daemon: (state == WorkspaceState::Running)
            .then(|| {
                info.map(|i| DaemonRef {
                    pid: i.pid,
                    started_at: i.started_at,
                })
            })
            .flatten(),
        project: info.and_then(|i| i.project.clone()),
        workspace: member.workspace.clone(),
        state,
        detail: detail.into(),
        summary: None,
        joined_at: member.joined_at,
    }
}

pub(crate) async fn run(gateway: Arc<Gateway>) {
    let mut stop = gateway.stopping.subscribe();
    let mut changes = gateway.changed.subscribe();
    let mut tick = tokio::time::interval(Duration::from_secs(1));
    tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
    let mut watches: BTreeMap<WorkspaceId, (u32, i64, JoinHandle<()>)> = BTreeMap::new();
    loop {
        tokio::select! {
            _ = stop.changed() => break,
            _ = changes.changed() => {},
            _ = tick.tick() => {},
        }
        if *stop.borrow() {
            break;
        }
        let selected = {
            let state = lock(&gateway.state);
            let ids = state
                .store
                .registry
                .devices
                .values()
                .filter(|d| d.valid_at(super::now_ms()))
                .flat_map(|d| d.grants.keys().copied())
                .collect::<BTreeSet<_>>();
            state
                .store
                .registry
                .memberships
                .values()
                .filter(|m| ids.contains(&m.id))
                .cloned()
                .collect::<Vec<_>>()
        };
        let sessions = Registered.sessions().await;
        let wanted = selected.iter().map(|m| m.id).collect::<BTreeSet<_>>();
        let obsolete = watches
            .keys()
            .filter(|id| !wanted.contains(id))
            .copied()
            .collect::<Vec<_>>();
        for id in obsolete {
            if let Some((_, _, task)) = watches.remove(&id) {
                task.abort();
                let _ = task.await;
            }
            lock(&gateway.members).remove(&id);
        }
        for member in selected {
            let info = sessions
                .iter()
                .find(|i| i.state_dir == member.state_dir)
                .cloned();
            let Some(info) = info else {
                if let Some((_, _, task)) = watches.remove(&member.id) {
                    task.abort();
                    let _ = task.await;
                }
                gateway.update_member(
                    member.id,
                    entry(&member, None, WorkspaceState::Stopped, ""),
                    None,
                );
                continue;
            };
            if Registered.check(&info).is_err() {
                if let Some((_, _, task)) = watches.remove(&member.id) {
                    task.abort();
                    let _ = task.await;
                }
                gateway.update_member(
                    member.id,
                    entry(
                        &member,
                        None,
                        WorkspaceState::Unreachable,
                        "daemon unavailable",
                    ),
                    None,
                );
                continue;
            }
            if watches.get(&member.id).is_some_and(|(pid, started, task)| {
                *pid == info.pid && *started == info.started_at && !task.is_finished()
            }) {
                continue;
            }
            if let Some((_, _, task)) = watches.remove(&member.id) {
                task.abort();
                let _ = task.await;
            }
            let id = member.id;
            let pid = info.pid;
            let started = info.started_at;
            let task = tokio::spawn(watch_member(gateway.clone(), member, info));
            watches.insert(id, (pid, started, task));
        }
    }
    for (_, (_, _, task)) in watches {
        task.abort();
        let _ = task.await;
    }
}

async fn watch_member(gateway: Arc<Gateway>, member: Membership, info: DaemonInfo) {
    let mut stop = gateway.stopping.subscribe();
    let dial = tokio::select! { dial = Registered.dial(&info, true) => dial, _ = stop.changed() => return };
    let mut dial = match dial {
        Ok(dial)
            if dial.hello.protocol == crew_core::rpc::PROTOCOL_VERSION
                && dial.hello.role == crew_core::rpc::Role::Operator =>
        {
            dial
        }
        Ok(_) | Err(daemons::DialError::Protocol(_)) => {
            gateway.update_member(
                member.id,
                entry(
                    &member,
                    Some(&info),
                    WorkspaceState::Incompatible,
                    "daemon protocol is incompatible",
                ),
                None,
            );
            return;
        }
        Err(_) => {
            gateway.update_member(
                member.id,
                entry(
                    &member,
                    Some(&info),
                    WorkspaceState::Unreachable,
                    "daemon unavailable",
                ),
                None,
            );
            return;
        }
    };
    let request = Request {
        id: 1,
        call: Call::Summary { watch: true },
    };
    let mut bytes = match serde_json::to_vec(&request) {
        Ok(bytes) => bytes,
        Err(_) => return,
    };
    bytes.push(b'\n');
    if tokio::time::timeout(Duration::from_secs(3), dial.writer.write_all(&bytes))
        .await
        .is_err()
    {
        return;
    }
    let mut current = entry(&member, Some(&info), WorkspaceState::Running, "");
    current.project = dial.hello.project;
    gateway.update_member(member.id, current.clone(), Some(info.clone()));
    let mut buffer = Vec::new();
    loop {
        let read = tokio::select! { read = daemons::read_line(&mut dial.lines, &mut buffer, crew_core::MAX_FRAME_BYTES) => read, _ = stop.changed() => return };
        if read.is_none() {
            break;
        }
        let frame = serde_json::from_slice::<ServerFrame>(&buffer);
        buffer.clear();
        match frame {
            Ok(frame) => {
                if let Some(Push::Summary(summary)) = frame.push {
                    current.summary = Some(summary);
                } else if frame.id == Some(1) {
                    if let Some(value) = frame.result {
                        if let Ok(summary) = serde_json::from_value(value) {
                            current.summary = Some(summary);
                        }
                    }
                }
                gateway.update_member(member.id, current.clone(), Some(info.clone()));
            }
            Err(_) => break,
        }
    }
    gateway.update_member(
        member.id,
        entry(
            &member,
            Some(&info),
            WorkspaceState::Unreachable,
            "daemon unavailable",
        ),
        None,
    );
}
