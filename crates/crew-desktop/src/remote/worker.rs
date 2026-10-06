//! Remote profile requests on the native service worker. Setup and sessions
//! use the connector's `GatewayClient`; this module only chooses the platform
//! signer and keeps one client per profile so setup and sessions share its
//! authority fence. No handshake, grant parsing or key material lives here.

use std::collections::BTreeMap;
use std::sync::{mpsc, Arc};

use crew_client::api::ClientReason;
use crew_client_transport::gateway::{
    DeviceSigner, EnrollmentResult, EnrollmentUpdate, GatewayClient, GatewayError,
    MemoryDeviceSigner, SetupObserver, SignerError,
};
use crew_client_transport::{DriverConfig, NativeSession, Protocol6Codec};

use super::profile::{KeyStorage, RemoteProfile};
use super::secret::{self, KeyOrigin, SecretServiceSigner, StoreState};
use crate::service::Response;

pub enum RemoteRequest {
    /// Check Secret Service readiness for the device-name page.
    ProbeStore,
    /// Find or create this profile's device key after the user continues.
    Prepare {
        profile: RemoteProfile,
    },
    /// One explicit enrollment attempt with an already prepared key.
    Enroll {
        profile: RemoteProfile,
        device_label: String,
    },
    CancelEnrollment {
        profile_id: String,
        attempt: u64,
    },
    /// Authenticate the profile's key and open its authorized listing.
    Connect {
        profile: RemoteProfile,
    },
    /// Retire the local key and forget this profile's client.
    Remove {
        profile: RemoteProfile,
    },
    /// Leave the remote profile and return to the protected local service.
    UseLocal,
}

pub enum RemoteResponse {
    Store(StoreState),
    Prepared {
        profile_id: String,
        result: Result<KeyOrigin, ClientReason>,
    },
    Setup(EnrollmentUpdate),
    Enrolled {
        profile_id: String,
        result: Result<EnrollmentResult, ClientReason>,
    },
    /// The profile now bound to the service, or `None` for local.
    Active(Option<String>),
    ConnectFailed {
        profile_id: String,
        reason: ClientReason,
    },
    Removed {
        profile_id: String,
        result: Result<(), ClientReason>,
    },
}

/// What the service worker must do with its current session.
pub enum Step {
    Done,
    Bind {
        profile: Box<RemoteProfile>,
        session: NativeSession,
    },
    Local,
}

pub struct Worker {
    clients: BTreeMap<String, GatewayClient>,
    memory: BTreeMap<String, Arc<MemoryDeviceSigner>>,
    active: Option<String>,
    runtime: tokio::runtime::Runtime,
}

fn reason(error: GatewayError) -> ClientReason {
    error.reason()
}

fn signer_reason(error: SignerError) -> ClientReason {
    GatewayError::Signer(error).reason()
}

type Responses = tokio::sync::mpsc::Sender<Response>;

/// Setup progress arrives inside the connector's runtime, so it is queued
/// in order for a forwarding thread instead of blocking there.
struct Forward(mpsc::Sender<RemoteResponse>);
impl SetupObserver for Forward {
    fn enrollment_changed(&self, update: EnrollmentUpdate) {
        let _ = self.0.send(RemoteResponse::Setup(update));
    }
}

fn forwarder(responses: &Responses) -> mpsc::Sender<RemoteResponse> {
    let (sender, receiver) = mpsc::channel::<RemoteResponse>();
    let responses = responses.clone();
    let _ = std::thread::Builder::new()
        .name("crew-gtk-setup-delivery".into())
        .spawn(move || {
            while let Ok(response) = receiver.recv() {
                if responses.blocking_send(Response::Remote(response)).is_err() {
                    break;
                }
            }
        });
    sender
}

impl Worker {
    pub fn new() -> Option<Self> {
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .ok()?;
        Some(Self {
            clients: BTreeMap::new(),
            memory: BTreeMap::new(),
            active: None,
            runtime,
        })
    }

    pub fn active(&self) -> Option<&str> {
        self.active.as_deref()
    }

    fn signer(
        &mut self,
        profile: &RemoteProfile,
        create: bool,
    ) -> Result<Arc<dyn DeviceSigner>, ClientReason> {
        Ok(match profile.storage {
            KeyStorage::SecureStore => Arc::new(SecretServiceSigner::new(
                &profile.id,
                &profile.endpoint_id(),
                &profile.label,
            )),
            KeyStorage::Memory => match self.memory.get(&profile.id) {
                Some(signer) => signer.clone(),
                None if create => {
                    let signer = Arc::new(MemoryDeviceSigner::new().map_err(signer_reason)?);
                    self.memory.insert(profile.id.clone(), signer.clone());
                    signer
                }
                None => return Err(signer_reason(SignerError::Retired)),
            },
        })
    }

    fn client(&mut self, profile: &RemoteProfile) -> Result<GatewayClient, ClientReason> {
        if let Some(client) = self.clients.get(&profile.id) {
            if client.profile().public_profile() == &profile.public {
                return Ok(client.clone());
            }
            client.close();
            self.clients.remove(&profile.id);
        }
        let trusted = profile.trusted().map_err(reason)?;
        let signer = self.signer(profile, false)?;
        let client = GatewayClient::new(trusted, signer);
        self.clients.insert(profile.id.clone(), client.clone());
        Ok(client)
    }

    pub fn handle(&mut self, request: RemoteRequest, responses: &Responses) -> Step {
        match request {
            RemoteRequest::ProbeStore => {
                let delivery = forwarder(responses);
                let _ = std::thread::Builder::new()
                    .name("crew-secret-probe".into())
                    .spawn(move || {
                        let _ = delivery.send(RemoteResponse::Store(secret::probe()));
                    });
            }
            RemoteRequest::Prepare { profile } => {
                let delivery = forwarder(responses);
                let profile_id = profile.id.clone();
                match profile.storage {
                    KeyStorage::Memory => {
                        let result = self.signer(&profile, true).map(|_| KeyOrigin::Created);
                        let _ = delivery.send(RemoteResponse::Prepared { profile_id, result });
                    }
                    KeyStorage::SecureStore => {
                        let signer = SecretServiceSigner::new(
                            &profile.id,
                            &profile.endpoint_id(),
                            &profile.label,
                        );
                        let _ = std::thread::Builder::new()
                            .name("crew-secret-prepare".into())
                            .spawn(move || {
                                let result = signer.prepare().map_err(signer_reason);
                                let _ =
                                    delivery.send(RemoteResponse::Prepared { profile_id, result });
                            });
                    }
                }
            }
            RemoteRequest::Enroll {
                profile,
                device_label,
            } => {
                let delivery = forwarder(responses);
                let profile_id = profile.id.clone();
                match self.client(&profile) {
                    Ok(client) => {
                        let spawned = std::thread::Builder::new()
                            .name("crew-gtk-enrollment".into())
                            .spawn(move || {
                                let result = tokio::runtime::Builder::new_current_thread()
                                    .enable_all()
                                    .build()
                                    .map_err(|_| reason(GatewayError::Busy))
                                    .and_then(|runtime| {
                                        runtime
                                            .block_on(client.enroll(
                                                device_label,
                                                Arc::new(Forward(delivery.clone())),
                                            ))
                                            .map_err(reason)
                                    });
                                let _ =
                                    delivery.send(RemoteResponse::Enrolled { profile_id, result });
                            });
                        if spawned.is_err() {
                            let _ = responses.blocking_send(Response::Remote(
                                RemoteResponse::Enrolled {
                                    profile_id: profile.id,
                                    result: Err(reason(GatewayError::Busy)),
                                },
                            ));
                        }
                    }
                    Err(reason) => {
                        let _ = delivery.send(RemoteResponse::Enrolled {
                            profile_id,
                            result: Err(reason),
                        });
                    }
                }
            }
            RemoteRequest::CancelEnrollment {
                profile_id,
                attempt,
            } => {
                if let Some(client) = self.clients.get(&profile_id) {
                    client.cancel_enrollment(attempt);
                }
            }
            RemoteRequest::Connect { profile } => {
                let result = self.client(&profile).and_then(|client| {
                    let core = crate::service::core().map_err(|error| ClientReason {
                        code: "internal".into(),
                        message: error.to_string(),
                    })?;
                    self.runtime
                        .block_on(client.session(core, Protocol6Codec, DriverConfig::default()))
                        .map_err(reason)
                });
                match result {
                    Ok(session) => {
                        return Step::Bind {
                            profile: Box::new(profile),
                            session,
                        }
                    }
                    Err(reason) => {
                        let _ = responses.blocking_send(Response::Remote(
                            RemoteResponse::ConnectFailed {
                                profile_id: profile.id,
                                reason,
                            },
                        ));
                    }
                }
            }
            RemoteRequest::Remove { profile } => {
                let was_active = self.active.as_deref() == Some(profile.id.as_str());
                let result = match self.clients.remove(&profile.id) {
                    Some(client) => self.runtime.block_on(client.retire()).map_err(reason),
                    None => match self.signer(&profile, false) {
                        Ok(signer) => self
                            .runtime
                            .block_on(signer.retire())
                            .map_err(signer_reason),
                        // A temporary key that already ended needs no retirement.
                        Err(_) => Ok(()),
                    },
                };
                self.memory.remove(&profile.id);
                let _ = responses.blocking_send(Response::Remote(RemoteResponse::Removed {
                    profile_id: profile.id,
                    result,
                }));
                if was_active {
                    return Step::Local;
                }
            }
            RemoteRequest::UseLocal => {
                if self.active.is_some() {
                    return Step::Local;
                }
            }
        }
        Step::Done
    }

    /// The service bound this profile's session; open its authorized index.
    pub fn bound(
        &mut self,
        profile: &RemoteProfile,
        session: &NativeSession,
    ) -> Result<(), crew_client::api::ClientError> {
        if let Some(previous) = self.active.replace(profile.id.clone()) {
            if previous != profile.id {
                self.close(&previous);
            }
        }
        session.connect(profile.id.clone(), profile.endpoint_id(), None)
    }

    /// The service returned to local access. The previous profile's client
    /// closes; its OS key and host device record remain for later use.
    pub fn left(&mut self) {
        if let Some(previous) = self.active.take() {
            self.close(&previous);
        }
    }

    fn close(&mut self, profile_id: &str) {
        if let Some(client) = self.clients.remove(profile_id) {
            client.close();
        }
    }
}
