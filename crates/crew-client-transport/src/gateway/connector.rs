use std::collections::BTreeSet;
use std::future::Future;
use std::pin::Pin;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use crew_client::api::{ClientScope, CommandKind};
use crew_client::policy::{method_policy, GatewayPermission};
use crew_protocol::native::{
    self, AuthParams, GatewayListing, HandshakeCall, HandshakeRequest, ListingNotice, Notice,
    NoticeFrame, PublicKey, WorkspaceGrant, WorkspaceId,
};
use crew_protocol::rpc::{ServerFrame, Workspaces};
use futures_util::SinkExt;
use serde::Deserialize;
use tokio::sync::{mpsc, watch, OwnedSemaphorePermit, Semaphore};
use tokio_tungstenite::tungstenite::Message;

use super::authority::{normalize, validate_listing, Fence};
use super::wire::{self, Socket, NETWORK_TIMEOUT, PROOF_ID};
use super::{AuthTranscript, DeviceSigner, GatewayError, GatewayProfile};
use crate::framing::validate_record;
use crate::queue::{ChannelSink, EventQueue, QUEUE_BYTES, QUEUE_FRAMES};
use crate::service::{ChannelHandle, Connector};
use crate::{ChannelPurpose, HostEffect, HostEvent, SendDisposition, TransportEnd};

pub(super) struct GatewayConnector {
    pub profile: GatewayProfile,
    pub signer: Arc<dyn DeviceSigner>,
    pub public_key: PublicKey,
    pub fence: Fence,
}

struct WriteFrame {
    id: u64,
    text: String,
    _permit: OwnedSemaphorePermit,
}

struct GatewayChannel {
    sender: mpsc::Sender<WriteFrame>,
    bytes: Arc<Semaphore>,
    stop: watch::Sender<Option<TransportEnd>>,
    finished: watch::Receiver<bool>,
    sink: ChannelSink,
    fence: Fence,
    registration: u64,
    purpose: native::ChannelPurpose,
    grant: Arc<Mutex<Option<WorkspaceGrant>>>,
}

impl ChannelHandle for GatewayChannel {
    fn send(&self, id: u64, text: String) -> Result<(), TransportEnd> {
        validate_record(&text).map_err(|_| TransportEnd::Malformed)?;
        if id == PROOF_ID || !self.fence.admitted(self.registration) {
            return Err(TransportEnd::Closed);
        }
        let grant = self.grant.lock().map_err(|_| TransportEnd::Stopped)?;
        request_kind(id, &text, self.purpose, grant.as_ref())?;
        drop(grant);
        let length = u32::try_from(text.len()).map_err(|_| TransportEnd::Oversized)?;
        let permit = self
            .bytes
            .clone()
            .try_acquire_many_owned(length)
            .map_err(|_| TransportEnd::Overflow)?;
        self.sender
            .try_send(WriteFrame {
                id,
                text,
                _permit: permit,
            })
            .map_err(|error| match error {
                mpsc::error::TrySendError::Full(_) => TransportEnd::Overflow,
                mpsc::error::TrySendError::Closed(_) => TransportEnd::Closed,
            })
    }
    fn close(&self) {
        let _ = self.stop.send(Some(TransportEnd::Stopped));
    }
    fn fail(&self, reason: TransportEnd) {
        self.sink.end(reason);
        let _ = self.stop.send(Some(reason));
    }
    fn wait_closed(&mut self) -> Pin<Box<dyn Future<Output = ()> + Send + '_>> {
        Box::pin(async move {
            while !*self.finished.borrow_and_update() {
                if self.finished.changed().await.is_err() {
                    break;
                }
            }
        })
    }
}

impl Drop for GatewayChannel {
    fn drop(&mut self) {
        self.close();
    }
}

impl Connector for GatewayConnector {
    fn open(
        &self,
        effect: &HostEffect,
        events: &EventQueue,
    ) -> Result<Box<dyn ChannelHandle>, TransportEnd> {
        let HostEffect::Open {
            channel_id,
            profile_id,
            endpoint_id,
            attempt,
            purpose,
            expected_scope,
        } = effect
        else {
            return Err(TransportEnd::Permission);
        };
        if profile_id != self.profile.profile_id() || endpoint_id != &self.profile.endpoint_id() {
            return Err(TransportEnd::Permission);
        }
        let purpose = match purpose {
            ChannelPurpose::Index => native::ChannelPurpose::Index,
            ChannelPurpose::Workspace => native::ChannelPurpose::Workspace,
            ChannelPurpose::Read => native::ChannelPurpose::Read,
            ChannelPurpose::Action => native::ChannelPurpose::Action,
            ChannelPurpose::Terminal { .. } => native::ChannelPurpose::Terminal,
        };
        let workspace = expected_scope
            .as_ref()
            .map(|scope| WorkspaceId::from_hex(&scope.workspace_id))
            .transpose()
            .map_err(|_| TransportEnd::Permission)?;
        if !purpose.accepts_workspace(workspace) {
            return Err(TransportEnd::Permission);
        }
        let sink = events.register(*channel_id)?;
        let (stop, cancel) = watch::channel(None);
        let registration = match self.fence.register(workspace, stop.clone(), sink.clone()) {
            Ok(id) => id,
            Err(reason) => {
                sink.end(reason);
                return Err(reason);
            }
        };
        let (sender, receiver) = mpsc::channel(QUEUE_FRAMES);
        let (completed, finished) = watch::channel(false);
        let grant = Arc::new(Mutex::new(None));
        let channel = GatewayChannel {
            sender,
            bytes: Arc::new(Semaphore::new(QUEUE_BYTES)),
            stop,
            finished,
            sink: sink.clone(),
            fence: self.fence.clone(),
            registration,
            purpose,
            grant: grant.clone(),
        };
        let worker = Worker {
            profile: self.profile.clone(),
            signer: self.signer.clone(),
            public_key: self.public_key,
            fence: self.fence.clone(),
            registration,
            channel_id: *channel_id,
            attempt: *attempt,
            purpose,
            workspace,
            expected: expected_scope.clone(),
            grant,
            sink: sink.clone(),
        };
        tokio::spawn(async move {
            let reason = worker.run(receiver, cancel).await;
            worker.fence.unregister(registration);
            sink.end(reason);
            let _ = completed.send(true);
        });
        Ok(Box::new(channel))
    }
}

struct Worker {
    profile: GatewayProfile,
    signer: Arc<dyn DeviceSigner>,
    public_key: PublicKey,
    fence: Fence,
    registration: u64,
    channel_id: u64,
    attempt: u64,
    purpose: native::ChannelPurpose,
    workspace: Option<WorkspaceId>,
    expected: Option<ClientScope>,
    grant: Arc<Mutex<Option<WorkspaceGrant>>>,
    sink: ChannelSink,
}

impl Worker {
    async fn authenticate(
        &self,
    ) -> Result<
        (
            Socket,
            crew_protocol::native::GatewayOpened<crew_protocol::rpc::HelloInfo>,
        ),
        GatewayError,
    > {
        let (mut socket, nonce) = tokio::time::timeout(NETWORK_TIMEOUT, async {
            let mut socket = wire::connect(&self.profile).await?;
            let nonce = wire::challenge(&mut socket).await?;
            Ok::<_, GatewayError>((socket, nonce))
        })
        .await
        .map_err(|_| GatewayError::Transport(TransportEnd::Timeout))??;
        let opened = tokio::time::timeout(NETWORK_TIMEOUT, async {
            let mut params = AuthParams {
                native_protocol: native::NATIVE_PROTOCOL,
                protocol: native::DAEMON_PROTOCOL,
                public_key: self.public_key,
                signature: native::HexBytes([0; 64]),
                purpose: self.purpose,
                workspace: self.workspace,
            };
            let bytes = native::auth_transcript(
                self.profile.public.endpoint_id,
                self.profile.public.tls_leaf_sha256,
                nonce,
                &params,
            )
            .map_err(|_| GatewayError::Transport(TransportEnd::Malformed))?;
            let transcript = AuthTranscript(bytes);
            params.signature = self
                .signer
                .sign_auth(transcript.clone())
                .await
                .map_err(GatewayError::Signer)?;
            wire::verify_signature(
                self.public_key,
                transcript.signing_bytes(),
                params.signature,
            )?;
            wire::send_proof(
                &mut socket,
                HandshakeRequest {
                    id: PROOF_ID,
                    call: HandshakeCall::NativeAuth(params),
                },
            )
            .await?;
            wire::opened(&wire::next_text(&mut socket).await?)
        })
        .await
        .map_err(|_| GatewayError::Transport(TransportEnd::Timeout))??;
        Ok((socket, opened))
    }

    async fn run(
        &self,
        mut receiver: mpsc::Receiver<WriteFrame>,
        mut cancel: watch::Receiver<Option<TransportEnd>>,
    ) -> TransportEnd {
        if let Some(reason) = *cancel.borrow() {
            return reason;
        }
        let authentication = tokio::select! {
            _ = cancel.changed() => return (*cancel.borrow()).unwrap_or(TransportEnd::Stopped),
            result = self.authenticate() => result,
        };
        let (mut socket, opened) = match authentication {
            Ok(value) => value,
            Err(error) => return error.transport_end(),
        };
        let context = match normalize(
            &opened,
            self.profile.public.endpoint_id,
            self.purpose,
            self.workspace,
            self.expected.as_ref(),
            self.attempt,
        ) {
            Ok(value) => value,
            Err(reason) => return reason,
        };
        if let Err(reason) =
            self.fence
                .admit(self.registration, &opened.authority, opened.grant.as_ref())
        {
            return reason;
        }
        match self.grant.lock() {
            Ok(mut grant) => *grant = opened.grant.clone(),
            Err(_) => return TransportEnd::Stopped,
        };
        let event = match context {
            Some((context, peer)) => HostEvent::WorkspaceOpened {
                channel_id: self.channel_id,
                context,
                peer,
            },
            None => HostEvent::EndpointOpened {
                channel_id: self.channel_id,
                endpoint_id: self.profile.endpoint_id(),
                attempt: self.attempt,
            },
        };
        if let Err(reason) = self.sink.try_send(event) {
            return reason;
        }
        let duration = Duration::from_millis(
            opened
                .authority
                .credential_expires_at
                .saturating_sub(opened.authority.server_time) as u64,
        );
        let Some(expiry) = tokio::time::Instant::now().checked_add(duration) else {
            return TransportEnd::Malformed;
        };
        let mut listings = BTreeSet::new();
        let reason = loop {
            if let Some(reason) = *cancel.borrow() {
                break reason;
            }
            tokio::select! {
                _ = cancel.changed() => break (*cancel.borrow()).unwrap_or(TransportEnd::Stopped),
                _ = tokio::time::sleep_until(expiry) => { self.fence.invalidate(None, TransportEnd::Revoked, false); break TransportEnd::Revoked; },
                frame = receiver.recv() => {
                    let Some(frame) = frame else { break TransportEnd::Stopped; };
                    if !self.fence.admitted(self.registration) { break TransportEnd::GrantChanged; }
                    if self.purpose == native::ChannelPurpose::Index {
                        match request_kind(frame.id, &frame.text, self.purpose, None) {
                            Ok(CommandKind::Workspaces) => { if listings.len() >= 64 { break TransportEnd::Overflow; } listings.insert(frame.id); },
                            Ok(_) => {}, Err(reason) => break reason,
                        }
                    }
                    if let Err(reason) = self.sink.try_send(HostEvent::SendProgress { channel_id: self.channel_id, request_id: frame.id, disposition: SendDisposition::WriteStarted }) { break reason; }
                    let sent = tokio::select! {
                        _ = cancel.changed() => break (*cancel.borrow()).unwrap_or(TransportEnd::Stopped),
                        result = tokio::time::timeout(Duration::from_secs(3), socket.send(Message::text(frame.text))) => result,
                    };
                    match sent { Ok(Ok(())) => {}, Ok(Err(error)) => break wire::ws_error(error).transport_end(), Err(_) => break TransportEnd::Timeout };
                    if let Err(reason) = self.sink.try_send(HostEvent::SendProgress { channel_id: self.channel_id, request_id: frame.id, disposition: SendDisposition::FullyWritten }) { break reason; }
                },
                record = wire::next_text(&mut socket) => {
                    let text = match record { Ok(text) => text, Err(error) => {
                        let end = error.transport_end();
                        if end == TransportEnd::Stopped { self.fence.invalidate(None, end, false); }
                        break end;
                    } };
                    match self.record(text, &mut listings) { Ok(()) => {}, Err(reason) => break reason };
                },
            }
        };
        if matches!(
            reason,
            TransportEnd::Revoked | TransportEnd::ProtocolMismatch
        ) {
            self.fence.invalidate(None, reason, false);
        } else if reason == TransportEnd::GrantChanged {
            self.fence.invalidate(self.workspace, reason, false);
        }
        let _ = tokio::time::timeout(Duration::from_millis(250), socket.close(None)).await;
        reason
    }

    fn record(&self, text: String, listings: &mut BTreeSet<u64>) -> Result<(), TransportEnd> {
        if !self.fence.admitted(self.registration) {
            return Err(TransportEnd::Closed);
        }
        let probe: serde_json::Value =
            serde_json::from_str(&text).map_err(|_| TransportEnd::Malformed)?;
        if probe.get("native").is_some() {
            let notice: NoticeFrame =
                serde_json::from_str(&text).map_err(|_| TransportEnd::Malformed)?;
            let (workspace, reason) = match notice.native {
                Notice::GrantChanged => (self.workspace, TransportEnd::GrantChanged),
                Notice::DeviceRevoked => (None, TransportEnd::Revoked),
                Notice::GatewayStopping => (None, TransportEnd::Stopped),
                _ => return Err(TransportEnd::Malformed),
            };
            self.fence.invalidate(workspace, reason, false);
            return Err(reason);
        }
        if self.purpose == native::ChannelPurpose::Index {
            let listing = if probe.get("native_listing").is_some() {
                let notice: ListingNotice<Workspaces> =
                    serde_json::from_str(&text).map_err(|_| TransportEnd::Malformed)?;
                Some((None, notice.native_listing))
            } else {
                let frame: ServerFrame =
                    serde_json::from_str(&text).map_err(|_| TransportEnd::Malformed)?;
                if frame.id.is_some_and(|id| listings.remove(&id)) {
                    if frame.error.is_some() {
                        return self.sink.try_send(HostEvent::Frame {
                            channel_id: self.channel_id,
                            text,
                        });
                    }
                    if frame.push.is_some() {
                        return Err(TransportEnd::Malformed);
                    }
                    let value: GatewayListing<Workspaces> =
                        serde_json::from_value(frame.result.ok_or(TransportEnd::Malformed)?)
                            .map_err(|_| TransportEnd::Malformed)?;
                    Some((frame.id, value))
                } else {
                    None
                }
            };
            if let Some((request_id, listing)) = listing {
                validate_listing(&listing, self.profile.public.endpoint_id)?;
                self.fence.listing(self.registration, &listing)?;
                return self.sink.try_send(HostEvent::Listing {
                    channel_id: self.channel_id,
                    request_id,
                    listing: listing.listing,
                });
            }
        }
        self.sink.try_send(HostEvent::Frame {
            channel_id: self.channel_id,
            text,
        })
    }
}

pub(super) fn request_kind(
    id: u64,
    text: &str,
    purpose: native::ChannelPurpose,
    grant: Option<&WorkspaceGrant>,
) -> Result<CommandKind, TransportEnd> {
    #[derive(Deserialize)]
    #[serde(deny_unknown_fields)]
    struct Envelope {
        id: u64,
        method: String,
        #[serde(default)]
        params: serde::de::IgnoredAny,
    }
    let envelope: Envelope = serde_json::from_str(text).map_err(|_| TransportEnd::Malformed)?;
    let _ = envelope.params;
    if envelope.id != id {
        return Err(TransportEnd::Malformed);
    }
    let kind = CommandKind::ALL
        .iter()
        .copied()
        .find(|kind| kind.method() == envelope.method)
        .ok_or(TransportEnd::Permission)?;
    if kind == CommandKind::Ping {
        return Ok(kind);
    }
    if purpose == native::ChannelPurpose::Index {
        return if kind == CommandKind::Workspaces {
            Ok(kind)
        } else {
            Err(TransportEnd::Permission)
        };
    }
    let grant = grant.ok_or(TransportEnd::Permission)?;
    if !grant
        .allowed_actions
        .iter()
        .any(|action| action == kind.method())
    {
        return Err(TransportEnd::Permission);
    }
    let permission = method_policy(kind).gateway;
    let capability = match permission {
        GatewayPermission::Read => native::Capability::Read,
        GatewayPermission::Control => native::Capability::Control,
        GatewayPermission::Terminal => native::Capability::Terminal,
        GatewayPermission::Denied => return Err(TransportEnd::Permission),
    };
    if !grant.capabilities.contains(&capability) {
        return Err(TransportEnd::Permission);
    }
    let allowed = match purpose {
        native::ChannelPurpose::Index => false,
        native::ChannelPurpose::Workspace => {
            permission == GatewayPermission::Read && kind != CommandKind::Workspaces
        }
        native::ChannelPurpose::Read => {
            permission == GatewayPermission::Read
                && !matches!(kind, CommandKind::Workspaces | CommandKind::Subscribe)
        }
        native::ChannelPurpose::Action => {
            permission == GatewayPermission::Control || kind == CommandKind::AgentInput
        }
        native::ChannelPurpose::Terminal => matches!(
            kind,
            CommandKind::Attach | CommandKind::AttachInput | CommandKind::AttachResize
        ),
    };
    if allowed {
        Ok(kind)
    } else {
        Err(TransportEnd::Permission)
    }
}
