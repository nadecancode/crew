use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use crew_client::api::{ClientApi, ClientReason};
use crew_protocol::native::{
    self, EnrollParams, HandshakeCall, HandshakeRequest, HexBytes, Notice, NoticeFrame, PublicKey,
};
use sha2::{Digest, Sha256};
use tokio::sync::watch;

use super::authority::{normalize, Fence};
use super::connector::GatewayConnector;
use super::wire::{self, NETWORK_TIMEOUT, PROOF_ID};
use super::{DeviceSigner, EnrollTranscript, GatewayError, GatewayProfile};
use crate::{Clock, CommandCodec, DriverConfig, NativeSession, SystemClock, TransportEnd};

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct GatewayDeviceAccess {
    pub endpoint_id: String,
    pub device_id: String,
    pub credential_expires_at_ms: i64,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum EnrollmentProgress {
    Connecting,
    /// Only published after a verified pinned host accepts this exact proof.
    AwaitingApproval {
        enrollment_id: String,
        comparison_code: String,
        expires_at_ms: i64,
    },
}

/// Setup identity is independent of workspace/daemon authority. Attempts are
/// unique across GatewayClient instances, so late callbacks cannot apply to a
/// replacement profile or a newer ceremony.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SetupContext {
    pub profile_id: String,
    pub endpoint_id: String,
    pub attempt: u64,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct EnrollmentUpdate {
    pub context: SetupContext,
    pub progress: EnrollmentProgress,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct EnrollmentResult {
    pub context: SetupContext,
    pub outcome: EnrollmentOutcome,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum EnrollmentOutcome {
    Approved {
        access: GatewayDeviceAccess,
    },
    Canceled,
    /// A proof might have reached durable host approval. Authenticate the same
    /// key to recover; never retry enrollment or rotate automatically.
    OutcomeUnknown,
    Failed {
        reason: ClientReason,
    },
}

pub trait SetupObserver: Send + Sync + 'static {
    /// Enqueue into the platform executor. No metadata mutex is held here.
    fn enrollment_changed(&self, update: EnrollmentUpdate);
}

struct PendingEnrollment {
    attempt: u64,
    cancel: watch::Sender<bool>,
}

static NEXT_SETUP: AtomicU64 = AtomicU64::new(1);

struct Inner {
    profile: GatewayProfile,
    signer: Arc<dyn DeviceSigner>,
    fence: Fence,
    key: Mutex<Option<PublicKey>>,
    enrollment: Mutex<Option<PendingEnrollment>>,
    retired: AtomicBool,
    closed: AtomicBool,
}

/// A trusted public profile and a platform signer. Native factories use the
/// same object for setup and sessions; neither Swift nor GTK owns a socket,
/// handshake DTO, grant parser or private key representation.
#[derive(Clone)]
pub struct GatewayClient(Arc<Inner>);

impl GatewayClient {
    pub fn new(profile: GatewayProfile, signer: Arc<dyn DeviceSigner>) -> Self {
        Self(Arc::new(Inner {
            profile,
            signer,
            fence: Fence::default(),
            key: Mutex::new(None),
            enrollment: Mutex::new(None),
            retired: AtomicBool::new(false),
            closed: AtomicBool::new(false),
        }))
    }

    pub fn profile(&self) -> &GatewayProfile {
        &self.0.profile
    }

    async fn public_key(&self) -> Result<PublicKey, GatewayError> {
        if self.0.retired.load(Ordering::Acquire) {
            return Err(GatewayError::Signer(super::SignerError::Retired));
        }
        if self.0.closed.load(Ordering::Acquire) {
            return Err(GatewayError::Transport(TransportEnd::Stopped));
        }
        let key = wire::unlock(&self.0.signer).await?;
        let changed = {
            let mut prior = self
                .0
                .key
                .lock()
                .map_err(|_| GatewayError::Transport(TransportEnd::Stopped))?;
            let changed = prior.is_some_and(|old| old != key);
            if !changed {
                *prior = Some(key);
            }
            changed
        };
        if changed {
            self.0.fence.invalidate(None, TransportEnd::Revoked, false);
            return Err(GatewayError::Signer(super::SignerError::InvalidProof));
        }
        Ok(key)
    }

    /// Unlocks before starting any network deadline. Constructing this real
    /// NativeSession starts no connection; call connect(profileId,endpointId,
    /// None) for the authenticated Index, then select a returned workspace.
    pub async fn session<C: ClientApi + Send + 'static, W: CommandCodec + 'static>(
        &self,
        core: C,
        codec: W,
        config: DriverConfig,
    ) -> Result<NativeSession, GatewayError> {
        let public_key = self.public_key().await?;
        NativeSession::with_connector(
            core,
            codec,
            GatewayConnector {
                profile: self.0.profile.clone(),
                signer: self.0.signer.clone(),
                public_key,
                fence: self.0.fence.clone(),
            },
            config,
        )
        .map_err(|_| GatewayError::Transport(TransportEnd::Stopped))
    }

    /// Native facades run this future on their Rust setup worker. Progress and
    /// the returned outcome carry the same identity; cancel captures its id.
    pub async fn enroll(
        &self,
        label: String,
        observer: Arc<dyn SetupObserver>,
    ) -> Result<EnrollmentResult, GatewayError> {
        if !native::valid_label(&label) {
            return Err(GatewayError::InvalidProfile);
        }
        let attempt = NEXT_SETUP
            .fetch_update(Ordering::AcqRel, Ordering::Acquire, |id| id.checked_add(1))
            .map_err(|_| GatewayError::Transport(TransportEnd::Overflow))?;
        let context = SetupContext {
            profile_id: self.0.profile.profile_id().into(),
            endpoint_id: self.0.profile.endpoint_id(),
            attempt,
        };
        let (cancel, mut canceled) = watch::channel(false);
        {
            let mut pending = self
                .0
                .enrollment
                .lock()
                .map_err(|_| GatewayError::Transport(TransportEnd::Stopped))?;
            if pending.is_some() {
                return Err(GatewayError::Busy);
            }
            *pending = Some(PendingEnrollment { attempt, cancel });
        }
        struct Guard(Arc<Inner>);
        impl Drop for Guard {
            fn drop(&mut self) {
                if let Ok(mut pending) = self.0.enrollment.lock() {
                    pending.take();
                }
            }
        }
        let _guard = Guard(self.0.clone());
        let attempted = AtomicBool::new(false);
        let result = tokio::select! {
            _ = canceled.changed() => return Ok(EnrollmentResult { context, outcome: if attempted.load(Ordering::Acquire) {
                EnrollmentOutcome::OutcomeUnknown
            } else { EnrollmentOutcome::Canceled } }),
            result = self.enrollment(&label, &observer, &context, &attempted) => result,
        };
        let outcome = match result {
            Ok(access) => EnrollmentOutcome::Approved { access },
            Err(error)
                if attempted.load(Ordering::Acquire)
                    && matches!(
                        error,
                        GatewayError::Transport(
                            TransportEnd::Closed
                                | TransportEnd::Timeout
                                | TransportEnd::Unreachable
                        )
                    ) =>
            {
                EnrollmentOutcome::OutcomeUnknown
            }
            Err(error) => failed(error),
        };
        Ok(EnrollmentResult { context, outcome })
    }

    async fn enrollment(
        &self,
        label: &str,
        observer: &Arc<dyn SetupObserver>,
        context: &SetupContext,
        attempted: &AtomicBool,
    ) -> Result<GatewayDeviceAccess, GatewayError> {
        publish(observer, context, EnrollmentProgress::Connecting)?;
        let public_key = self.public_key().await?;
        let (mut socket, nonce) = tokio::time::timeout(NETWORK_TIMEOUT, async {
            let mut socket = wire::connect(&self.0.profile).await?;
            let nonce = wire::challenge(&mut socket).await?;
            Ok::<_, GatewayError>((socket, nonce))
        })
        .await
        .map_err(|_| GatewayError::Transport(TransportEnd::Timeout))??;
        let mut params = EnrollParams {
            native_protocol: native::NATIVE_PROTOCOL,
            protocol: native::DAEMON_PROTOCOL,
            public_key,
            client_nonce: wire::random_nonce()?,
            signature: HexBytes([0; 64]),
            label: label.to_owned(),
        };
        let bytes = native::enroll_transcript(
            self.0.profile.public.endpoint_id,
            self.0.profile.public.tls_leaf_sha256,
            nonce,
            &params,
        );
        let transcript = EnrollTranscript(
            bytes
                .try_into()
                .map_err(|_| GatewayError::Transport(TransportEnd::Malformed))?,
        );
        let expected = HexBytes::<8>(
            Sha256::digest(transcript.signing_bytes())[..8]
                .try_into()
                .map_err(|_| GatewayError::Transport(TransportEnd::Malformed))?,
        );
        tokio::time::timeout(NETWORK_TIMEOUT, async {
            params.signature = self
                .0
                .signer
                .sign_enrollment(transcript.clone())
                .await
                .map_err(GatewayError::Signer)?;
            wire::verify_signature(public_key, transcript.signing_bytes(), params.signature)?;
            // Mark before the first write poll: a canceled partial write might
            // already have supplied a complete proof to the pinned host.
            attempted.store(true, Ordering::Release);
            wire::send_proof(
                &mut socket,
                HandshakeRequest {
                    id: PROOF_ID,
                    call: HandshakeCall::NativeEnroll(params),
                },
            )
            .await
        })
        .await
        .map_err(|_| GatewayError::Transport(TransportEnd::Timeout))??;
        let first = tokio::time::timeout(NETWORK_TIMEOUT, wire::next_text(&mut socket))
            .await
            .map_err(|_| GatewayError::Transport(TransportEnd::Timeout))??;
        let frame: NoticeFrame = match serde_json::from_str(&first) {
            Ok(frame) => frame,
            Err(_) => {
                wire::opened(&first)?;
                return Err(GatewayError::Transport(TransportEnd::Malformed));
            }
        };
        let Notice::EnrollmentAccepted { intent } = frame.native else {
            return Err(GatewayError::Transport(TransportEnd::Malformed));
        };
        if intent.comparison_code != expected || intent.expires_at <= 0 {
            return Err(GatewayError::Transport(TransportEnd::Malformed));
        }
        let delay = approval_delay(intent.expires_at, SystemClock::default().sample().wall_ms)?;
        publish(
            observer,
            context,
            EnrollmentProgress::AwaitingApproval {
                enrollment_id: intent.enrollment_id.to_hex(),
                comparison_code: expected.to_hex(),
                expires_at_ms: intent.expires_at,
            },
        )?;
        let wait = async {
            let opened = wire::opened(&wire::next_text(&mut socket).await?)?;
            normalize(
                &opened,
                self.0.profile.public.endpoint_id,
                native::ChannelPurpose::Index,
                None,
                None,
                0,
            )
            .map_err(GatewayError::Transport)?;
            Ok(GatewayDeviceAccess {
                endpoint_id: opened.authority.endpoint_id.to_hex(),
                device_id: opened.authority.device_id.to_hex(),
                credential_expires_at_ms: opened.authority.credential_expires_at,
            })
        };
        tokio::time::timeout(delay, wait)
            .await
            .map_err(|_| GatewayError::Transport(TransportEnd::Timeout))?
    }

    pub fn cancel_enrollment(&self, attempt: u64) -> bool {
        self.0.enrollment.lock().is_ok_and(|pending| {
            pending.as_ref().is_some_and(|pending| {
                pending.attempt == attempt && pending.cancel.send(true).is_ok()
            })
        })
    }

    /// Dispose this profile's local sessions/setup. This leaves the OS key and
    /// host device record available for explicit same-key recovery.
    pub fn close(&self) {
        self.0.closed.store(true, Ordering::Release);
        if let Ok(pending) = self.0.enrollment.lock() {
            if let Some(pending) = pending.as_ref() {
                let _ = pending.cancel.send(true);
            }
        }
        self.0.fence.invalidate(None, TransportEnd::Stopped, true);
    }

    /// Cancels local work and fences all sessions before retiring the OS key.
    /// The host grant remains until explicitly revoked on its trusted console.
    pub async fn retire(&self) -> Result<(), GatewayError> {
        self.0.retired.store(true, Ordering::Release);
        self.0.closed.store(true, Ordering::Release);
        if let Ok(pending) = self.0.enrollment.lock() {
            if let Some(pending) = pending.as_ref() {
                let _ = pending.cancel.send(true);
            }
        }
        self.0.fence.invalidate(None, TransportEnd::Revoked, true);
        tokio::time::timeout(Duration::from_secs(30), self.0.signer.retire())
            .await
            .map_err(|_| GatewayError::Signer(super::SignerError::Canceled))?
            .map_err(GatewayError::Signer)
    }
}

fn publish(
    observer: &Arc<dyn SetupObserver>,
    context: &SetupContext,
    progress: EnrollmentProgress,
) -> Result<(), GatewayError> {
    std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        observer.enrollment_changed(EnrollmentUpdate {
            context: context.clone(),
            progress,
        })
    }))
    .map_err(|_| GatewayError::Transport(TransportEnd::Stopped))
}

fn failed(error: GatewayError) -> EnrollmentOutcome {
    EnrollmentOutcome::Failed {
        reason: error.reason(),
    }
}

pub(super) fn approval_delay(expires_at: i64, wall_now: i64) -> Result<Duration, GatewayError> {
    let remaining = expires_at.saturating_sub(wall_now).clamp(0, 120_000);
    if remaining == 0 {
        return Err(GatewayError::Transport(TransportEnd::Timeout));
    }
    Ok(Duration::from_millis(remaining as u64))
}
