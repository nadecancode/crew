//! Setup futures run on Rust workers. Foreign code receives real source
//! progress/results and never manufactures an authenticated connection scope.

use std::future::Future;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, SyncSender};
use std::sync::{Arc, Mutex, OnceLock};

use crew_client::{ClientConfig, ClientCore, SnapshotMode};
use crew_client_transport::gateway as transport;
use crew_client_transport::{DriverConfig, NativeSession, Protocol6Codec, TransportEnd};

use super::signer::SignerBridge;
use super::{
    DeviceSigner, EnrollmentOutcome, EnrollmentResult, EnrollmentUpdate, GatewayError,
    GatewayProfile, SetupContext,
};
use crate::records::ClientReason;

fn stopped() -> transport::GatewayError {
    transport::GatewayError::Transport(TransportEnd::Stopped)
}

fn runtime() -> Result<&'static tokio::runtime::Runtime, GatewayError> {
    static RUNTIME: OnceLock<Result<tokio::runtime::Runtime, ()>> = OnceLock::new();
    RUNTIME
        .get_or_init(|| {
            tokio::runtime::Builder::new_multi_thread()
                .worker_threads(2)
                .thread_name("crew-device-setup")
                .enable_all()
                .build()
                .map_err(|_| ())
        })
        .as_ref()
        .map_err(|_| stopped().into())
}

/// Only native platform blocking workers call this function. No gateway/core
/// lock is held while waiting for an OS credential response.
fn wait<T: Send + 'static>(
    future: impl Future<Output = Result<T, transport::GatewayError>> + Send + 'static,
) -> Result<T, GatewayError> {
    let (sender, received) = mpsc::sync_channel(1);
    runtime()?.spawn(async move {
        let result = future.await;
        let _ = sender.send(result);
    });
    received
        .recv()
        .map_err(|_| GatewayError::from(stopped()))?
        .map_err(Into::into)
}

pub(crate) struct GatewayState {
    pub(crate) source: transport::GatewayClient,
    active_enrollment: Arc<AtomicBool>,
    closed: AtomicBool,
}

impl GatewayState {
    pub(crate) fn new(
        profile: &GatewayProfile,
        signer: Box<dyn DeviceSigner>,
    ) -> Result<Arc<Self>, GatewayError> {
        Ok(Arc::new(Self {
            source: transport::GatewayClient::new(
                profile.inner.clone(),
                SignerBridge::new(signer)?,
            ),
            active_enrollment: Arc::new(AtomicBool::new(false)),
            closed: AtomicBool::new(false),
        }))
    }

    pub(crate) fn session(&self) -> Result<NativeSession, GatewayError> {
        let source = self.source.clone();
        let core = ClientCore::new(ClientConfig {
            limits: crew_client::policy::NATIVE_LIMITS,
            snapshot_mode: SnapshotMode::OrderedConnection,
        })
        .map_err(|_| GatewayError::from(stopped()))?;
        wait(async move {
            source
                .session(core, Protocol6Codec, DriverConfig::default())
                .await
        })
    }

    pub(crate) fn enroll(
        &self,
        label: String,
        foreign: Box<dyn SetupObserver>,
    ) -> Result<Arc<EnrollmentHandle>, GatewayError> {
        if self.closed.load(Ordering::Acquire) {
            return Err(stopped().into());
        }
        let runtime = runtime()?;
        if self.active_enrollment.swap(true, Ordering::AcqRel) {
            return Err(transport::GatewayError::Busy.into());
        }
        let lease = EnrollmentLease(self.active_enrollment.clone());
        let control = Arc::new(EnrollmentControl {
            source: self.source.clone(),
            state: Mutex::new(EnrollmentStatus::default()),
        });
        let (delivery, events) = mpsc::sync_channel::<Publication>(4);
        let observer_control = control.clone();
        std::thread::Builder::new()
            .name("crew-device-observer".into())
            .spawn(move || {
                for event in events {
                    let final_event = !matches!(&event, Publication::Changed(_));
                    if std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| match event {
                        Publication::Changed(update) => foreign.changed(update),
                        Publication::Finished(result) => foreign.finished(result),
                        Publication::Failed(reason) => foreign.failed(reason),
                    }))
                    .is_err()
                    {
                        observer_control.cancel();
                    }
                    if final_event {
                        break;
                    }
                }
            })
            .map_err(|_| GatewayError::from(stopped()))?;
        let adapter: Arc<dyn transport::SetupObserver> = Arc::new(ProgressAdapter {
            control: control.clone(),
            delivery: delivery.clone(),
        });
        let source = self.source.clone();
        let task_control = control.clone();
        runtime.spawn(async move {
            // A provider panic must also release the handle and publish a safe
            // terminal failure. No panic is propagated across the FFI boundary.
            let result = tokio::spawn(async move { source.enroll(label, adapter).await })
                .await
                .unwrap_or_else(|_| Err(stopped()));
            let final_event = task_control.finish(result);
            // Release the lease before the terminal callback can run, so an
            // observer retrying from it never finds its own attempt busy.
            drop(lease);
            // The source publishes at most Connecting, AwaitingApproval, and
            // one terminal event. Capacity four reserves space for the result
            // even while a foreign UI callback is blocked.
            let _ = delivery.try_send(final_event);
        });
        Ok(Arc::new(EnrollmentHandle { control }))
    }

    pub(crate) fn close(&self) {
        self.closed.store(true, Ordering::Release);
        self.source.close();
    }

    pub(crate) fn retire(&self) -> Result<(), GatewayError> {
        self.closed.store(true, Ordering::Release);
        let source = self.source.clone();
        wait(async move { source.retire().await })
    }
}

impl Drop for GatewayState {
    fn drop(&mut self) {
        self.source.close();
    }
}

struct EnrollmentLease(Arc<AtomicBool>);

impl Drop for EnrollmentLease {
    fn drop(&mut self) {
        self.0.store(false, Ordering::Release);
    }
}

/// One Rust thread per attempt delivers these callbacks in order; each should
/// hand short work to the UI executor and return. Every update and result
/// carries the attempt's `SetupContext`; check it before publishing. Exactly
/// one of `finished` or `failed` ends an attempt. `failed` means the attempt
/// could not start, before any context existed. `AwaitingApproval` arrives
/// only after the pinned host accepted this device's proof.
#[uniffi::export(callback_interface)]
pub trait SetupObserver: Send + Sync {
    fn changed(&self, update: EnrollmentUpdate);
    fn finished(&self, result: EnrollmentResult);
    fn failed(&self, reason: ClientReason);
}

#[derive(Default)]
struct EnrollmentStatus {
    context: Option<SetupContext>,
    cancel_requested: bool,
    finished: bool,
}

struct EnrollmentControl {
    source: transport::GatewayClient,
    state: Mutex<EnrollmentStatus>,
}

impl EnrollmentControl {
    fn cancel(&self) -> bool {
        let context = {
            let mut state = self
                .state
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            if state.finished {
                return false;
            }
            state.cancel_requested = true;
            state.context.clone()
        };
        match context {
            Some(context) => self.source.cancel_enrollment(context.attempt),
            None => true,
        }
    }

    fn progress(&self, context: &SetupContext) {
        let cancel = {
            let mut state = self
                .state
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            state.context = Some(context.clone());
            state.cancel_requested
        };
        if cancel {
            self.source.cancel_enrollment(context.attempt);
        }
    }

    fn finish(&self, result: Result<EnrollmentResult, transport::GatewayError>) -> Publication {
        let context = {
            let mut state = self
                .state
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            state.finished = true;
            state.context.clone()
        };
        match result {
            Ok(result) => Publication::Finished(result),
            Err(error) => match context {
                Some(context) => Publication::Finished(EnrollmentResult {
                    context,
                    outcome: EnrollmentOutcome::Failed {
                        reason: error.reason(),
                    },
                }),
                None => Publication::Failed(error.reason()),
            },
        }
    }
}

struct ProgressAdapter {
    control: Arc<EnrollmentControl>,
    delivery: SyncSender<Publication>,
}

impl transport::SetupObserver for ProgressAdapter {
    fn enrollment_changed(&self, update: EnrollmentUpdate) {
        self.control.progress(&update.context);
        if self
            .delivery
            .try_send(Publication::Changed(update))
            .is_err()
        {
            self.control.cancel();
        }
    }
}

enum Publication {
    Changed(EnrollmentUpdate),
    Finished(EnrollmentResult),
    Failed(ClientReason),
}

/// Controls one enrollment attempt. Dropping the handle cancels the attempt;
/// its terminal callback is still delivered.
#[derive(uniffi::Object)]
pub struct EnrollmentHandle {
    control: Arc<EnrollmentControl>,
}

#[uniffi::export]
impl EnrollmentHandle {
    /// Request cancellation; false once the attempt has finished. If the proof
    /// may already have reached the host, the result is `OutcomeUnknown`, not
    /// `Canceled`: recover by connecting with the same key, never by enrolling
    /// again automatically.
    pub fn cancel(&self) -> bool {
        self.control.cancel()
    }

    pub fn is_finished(&self) -> bool {
        self.control
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .finished
    }
}

impl Drop for EnrollmentHandle {
    fn drop(&mut self) {
        self.control.cancel();
    }
}
