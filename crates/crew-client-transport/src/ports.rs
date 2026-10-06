use crew_client::api::{
    AttachmentId, ClientNotice, ClientScope, ConnectionContext, PeerInfo, TerminalChunk,
};
use crew_protocol::rpc::Workspaces;

pub type ChannelId = u64;

/// A terminal channel serves exactly one immutable attachment.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum ChannelPurpose {
    Index,
    Workspace,
    Read,
    Action,
    Terminal { attachment_id: AttachmentId },
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SendDisposition {
    NeverWritten,
    WriteStarted,
    FullyWritten,
}

/// Finite reasons do not embed frame text, IO diagnostics or credential values.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TransportEnd {
    Closed,
    Authentication,
    AgentContext,
    MissingCredential,
    CredentialLocked,
    CredentialUnavailable,
    Permission,
    ProtocolMismatch,
    TlsTrust,
    Revoked,
    GrantChanged,
    Malformed,
    Oversized,
    Overflow,
    Timeout,
    Unreachable,
    Stopped,
}

impl TransportEnd {
    pub fn reason(self) -> crew_client::api::ClientReason {
        let (code, message) = match self {
            Self::Closed => ("closed", "The connection closed."),
            Self::Authentication => (
                "authentication",
                "Device or operator access is unavailable.",
            ),
            Self::AgentContext => (
                "agent_context",
                "Operator access is unavailable in an agent process.",
            ),
            Self::MissingCredential => (
                "missing_credential",
                "No protected operator credential is available.",
            ),
            Self::CredentialLocked => ("key_locked", "Unlock device access before connecting."),
            Self::CredentialUnavailable => {
                ("key_unavailable", "Device secure storage is unavailable.")
            }
            Self::Permission => ("permission", "Protected local access is unavailable."),
            Self::ProtocolMismatch => (
                "protocol",
                "This client and host use incompatible versions.",
            ),
            Self::TlsTrust => ("tls_trust", "The host identity could not be verified."),
            Self::Revoked => ("revoked", "Device access ended."),
            Self::GrantChanged => ("grant_changed", "Workspace access changed."),
            Self::Malformed => ("malformed", "The host sent invalid data."),
            Self::Oversized => ("frame_limit", "A transport frame exceeded its size limit."),
            Self::Overflow => ("overflow", "The connection fell behind its bounded queue."),
            Self::Timeout => ("timeout", "The host did not answer in time."),
            Self::Unreachable => ("unreachable", "The host cannot be reached."),
            Self::Stopped => ("stopped", "The connection was stopped."),
        };
        crew_client::api::ClientReason {
            code: code.into(),
            message: message.into(),
        }
    }
}

/// Verified connector events. Index identity has no invented workspace scope.
#[derive(Clone, Debug)]
pub enum HostEvent {
    EndpointOpened {
        channel_id: ChannelId,
        endpoint_id: String,
        attempt: u64,
    },
    WorkspaceOpened {
        channel_id: ChannelId,
        context: ConnectionContext,
        peer: PeerInfo,
    },
    Listing {
        channel_id: ChannelId,
        request_id: Option<u64>,
        listing: Workspaces,
    },
    Frame {
        channel_id: ChannelId,
        text: String,
    },
    SendProgress {
        channel_id: ChannelId,
        request_id: u64,
        disposition: SendDisposition,
    },
    Ended {
        channel_id: ChannelId,
        reason: TransportEnd,
    },
    Wake {
        timer_id: u64,
    },
    Lifecycle {
        active: bool,
    },
}

impl HostEvent {
    pub fn channel_id(&self) -> Option<ChannelId> {
        match self {
            Self::EndpointOpened { channel_id, .. }
            | Self::WorkspaceOpened { channel_id, .. }
            | Self::Listing { channel_id, .. }
            | Self::Frame { channel_id, .. }
            | Self::SendProgress { channel_id, .. }
            | Self::Ended { channel_id, .. } => Some(*channel_id),
            Self::Wake { .. } | Self::Lifecycle { .. } => None,
        }
    }

    #[cfg(feature = "runtime")]
    pub(crate) fn queued_bytes(&self) -> usize {
        match self {
            Self::Frame { text, .. } => text.len(),
            // Typed metadata is bounded by the authenticated frame/connector.
            Self::Listing { listing, .. } => {
                struct Counter(usize);
                impl std::io::Write for Counter {
                    fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
                        self.0 = self.0.saturating_add(bytes.len());
                        if self.0 > crate::framing::MAX_FRAME_BYTES {
                            return Err(std::io::Error::other("frame limit"));
                        }
                        Ok(bytes.len())
                    }
                    fn flush(&mut self) -> std::io::Result<()> {
                        Ok(())
                    }
                }
                let mut count = Counter(0);
                if serde_json::to_writer(&mut count, listing).is_err() {
                    usize::MAX
                } else {
                    count.0
                }
            }
            _ => 0,
        }
    }
}

#[derive(Clone, Debug)]
pub enum HostEffect {
    Open {
        channel_id: ChannelId,
        profile_id: String,
        endpoint_id: String,
        attempt: u64,
        purpose: ChannelPurpose,
        expected_scope: Option<ClientScope>,
    },
    Send {
        channel_id: ChannelId,
        request_id: u64,
        text: String,
    },
    Close {
        channel_id: ChannelId,
    },
    Schedule {
        timer_id: u64,
        deadline_ms: u64,
    },
    CancelTimer {
        timer_id: u64,
    },
}

/// Native observers receive typed updates; callbacks never run inside a driver
/// or core lock. Terminal bytes have their own bounded publication path.
#[derive(Clone, Debug)]
pub struct HostUpdate {
    pub notice: ClientNotice,
    pub effects: Vec<HostEffect>,
    pub terminal: Vec<TerminalChunk>,
}
