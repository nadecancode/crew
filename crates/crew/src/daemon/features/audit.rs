//! Closed, body-free observer audit records. Never attach these to a member.

use crew_core::rpc::asides::{AsideAnswered, AsideAsked};
use crew_core::rpc::{Push, RpcError};
use crew_core::Event;

use super::super::Daemon;

pub enum OperatorAudit {
    AsideAsked(AsideAsked),
    AsideAnswered(AsideAnswered),
}

impl Daemon {
    /// Store and publish after the domain's receipt/state commit. This port
    /// takes no model lock, adds no member/task association, and cannot accept
    /// question or answer text. The domain must not call it inside a DB callback.
    pub fn record_operator_audit(&self, record: OperatorAudit) -> Result<Event, RpcError> {
        let (kind, data) = match record {
            OperatorAudit::AsideAsked(record) => ("aside.asked", serde_json::to_value(record)),
            OperatorAudit::AsideAnswered(record) => {
                ("aside.answered", serde_json::to_value(record))
            }
        };
        let data = data.map_err(|_| RpcError::internal("observer audit encoding failed"))?;
        let event = self
            .store
            .append_event(crate::persistence::NewEvent::new("asides", kind, kind).data(data))
            .map_err(|_| RpcError::internal("observer audit persistence failed"))?;
        // Snapshot, Subscribe and terminal attach already require the operator.
        // Agent Events reads additionally exclude this closed event family.
        self.push(Push::Event(event.clone()));
        Ok(event)
    }
}
