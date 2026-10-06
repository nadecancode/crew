//! One-shot foreign replies adapt synchronous callbacks to Rust signer futures.
//! No foreign callback executes on the connection runtime or under a mutex.

use std::fmt;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::mpsc::{self, SyncSender};
use std::sync::{Arc, Mutex};

use crew_client_transport::gateway as transport;
use crew_protocol::native::{self, HexBytes, PublicKey, Signature};
use tokio::sync::oneshot;

use super::{AuthTranscript, EnrollTranscript, GatewayError, SignerError};

const MAX_PENDING: usize = 32;
const CALLBACK_CAPACITY: usize = 64;

#[derive(Clone, Copy, Debug, PartialEq, Eq, uniffi::Error)]
pub enum ReplyError {
    AlreadyCompleted,
    InvalidProof,
}

impl fmt::Display for ReplyError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::AlreadyCompleted => "signer request already completed or canceled",
            Self::InvalidProof => "invalid public key or signature width",
        })
    }
}

impl std::error::Error for ReplyError {}

struct ReplyCell<T> {
    sender: Mutex<Option<oneshot::Sender<Result<T, SignerError>>>>,
}

impl<T> ReplyCell<T> {
    fn finish(&self, value: Result<T, SignerError>) -> Result<(), ReplyError> {
        let sender = self
            .sender
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .take()
            .ok_or(ReplyError::AlreadyCompleted)?;
        // Drop the cell lock before waking any waiter or dropping a value.
        sender.send(value).map_err(|_| ReplyError::AlreadyCompleted)
    }

    fn pending(&self) -> bool {
        self.sender
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .as_ref()
            .is_some_and(|sender| !sender.is_closed())
    }

    fn cancel(&self) {
        let _ = self.finish(Err(SignerError::Canceled));
    }
}

macro_rules! byte_reply {
    ($name:ident, $value:ty, $width:expr) => {
        #[derive(uniffi::Object)]
        pub struct $name {
            cell: Arc<ReplyCell<$value>>,
        }

        #[uniffi::export]
        impl $name {
            pub fn complete(&self, bytes: Vec<u8>) -> Result<(), ReplyError> {
                let value = bytes
                    .try_into()
                    .map(HexBytes::<$width>)
                    .map_err(|_: Vec<u8>| SignerError::InvalidProof);
                let invalid = value.is_err();
                self.cell.finish(value)?;
                if invalid {
                    Err(ReplyError::InvalidProof)
                } else {
                    Ok(())
                }
            }

            pub fn fail(&self, error: SignerError) -> Result<(), ReplyError> {
                self.cell.finish(Err(error))
            }

            pub fn is_pending(&self) -> bool {
                self.cell.pending()
            }
        }

        impl Drop for $name {
            fn drop(&mut self) {
                self.cell.cancel();
            }
        }

        impl fmt::Debug for $name {
            fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                f.write_str(concat!(stringify!($name), "(<redacted>)"))
            }
        }
    };
}

byte_reply!(PublicKeyReply, PublicKey, 32);
byte_reply!(SignatureReply, Signature, 64);

#[derive(uniffi::Object)]
pub struct RetirementReply {
    cell: Arc<ReplyCell<()>>,
}

#[uniffi::export]
impl RetirementReply {
    pub fn complete(&self) -> Result<(), ReplyError> {
        self.cell.finish(Ok(()))
    }

    pub fn fail(&self, error: SignerError) -> Result<(), ReplyError> {
        self.cell.finish(Err(error))
    }

    pub fn is_pending(&self) -> bool {
        self.cell.pending()
    }
}

impl Drop for RetirementReply {
    fn drop(&mut self) {
        self.cell.cancel();
    }
}

impl fmt::Debug for RetirementReply {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("RetirementReply(<redacted>)")
    }
}

/// The platform credential store. Each call enqueues bounded work on the
/// platform's credential actor and returns at once; the answer goes through
/// the reply object, from any thread. Only `public_key` may unlock or ask for
/// user presence, within 30 s. `sign_auth` and `sign_enrollment` run inside
/// the 5 s network deadline: they must not prompt, and fail with `Locked`,
/// `Unavailable` or `Canceled` when signing cannot proceed. Sign exactly the
/// transcript's bytes and never log them. No call returns or exports a
/// private key or store handle. Keep the reply while work runs; dropping it
/// unanswered cancels the request.
#[uniffi::export(callback_interface)]
pub trait DeviceSigner: Send + Sync {
    fn public_key(&self, reply: Arc<PublicKeyReply>);
    fn sign_auth(&self, transcript: Arc<AuthTranscript>, reply: Arc<SignatureReply>);
    fn sign_enrollment(&self, transcript: Arc<EnrollTranscript>, reply: Arc<SignatureReply>);
    fn retire(&self, reply: Arc<RetirementReply>);
}

enum Work {
    PublicKey(Arc<PublicKeyReply>),
    Auth(Arc<AuthTranscript>, Arc<SignatureReply>),
    Enrollment(Arc<EnrollTranscript>, Arc<SignatureReply>),
    Retire(Arc<RetirementReply>),
}

impl Work {
    fn pending(&self) -> bool {
        match self {
            Self::PublicKey(reply) => reply.is_pending(),
            Self::Auth(_, reply) | Self::Enrollment(_, reply) => reply.is_pending(),
            Self::Retire(reply) => reply.is_pending(),
        }
    }

    fn dispatch(&self, foreign: &dyn DeviceSigner) {
        match self {
            Self::PublicKey(reply) => foreign.public_key(reply.clone()),
            Self::Auth(transcript, reply) => foreign.sign_auth(transcript.clone(), reply.clone()),
            Self::Enrollment(transcript, reply) => {
                foreign.sign_enrollment(transcript.clone(), reply.clone())
            }
            Self::Retire(reply) => foreign.retire(reply.clone()),
        }
    }

    fn failed(&self) {
        let _ = match self {
            Self::PublicKey(reply) => reply.fail(SignerError::Unavailable),
            Self::Auth(_, reply) | Self::Enrollment(_, reply) => {
                reply.fail(SignerError::Unavailable)
            }
            Self::Retire(reply) => reply.fail(SignerError::Unavailable),
        };
    }
}

pub(crate) struct SignerBridge {
    sender: SyncSender<Work>,
    pending: Arc<AtomicUsize>,
}

impl SignerBridge {
    pub(crate) fn new(foreign: Box<dyn DeviceSigner>) -> Result<Arc<Self>, GatewayError> {
        let (sender, receiver) = mpsc::sync_channel::<Work>(CALLBACK_CAPACITY);
        std::thread::Builder::new()
            .name("crew-device-signer".into())
            .spawn(move || {
                for work in receiver {
                    if !work.pending() {
                        continue;
                    }
                    if std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                        work.dispatch(&*foreign);
                    }))
                    .is_err()
                    {
                        work.failed();
                    }
                }
                // The foreign callback and all queued objects are released on
                // this dispatcher, outside a gateway/core mutex.
            })
            .map_err(|_| {
                transport::GatewayError::Transport(crew_client_transport::TransportEnd::Stopped)
            })?;
        Ok(Arc::new(Self {
            sender,
            pending: Arc::new(AtomicUsize::new(0)),
        }))
    }

    fn request<T: Send + 'static>(
        &self,
        make: impl FnOnce(Arc<ReplyCell<T>>) -> Work,
    ) -> transport::SignerFuture<'_, T> {
        if self
            .pending
            .fetch_update(Ordering::AcqRel, Ordering::Acquire, |count| {
                (count < MAX_PENDING).then_some(count + 1)
            })
            .is_err()
        {
            return Box::pin(async { Err(SignerError::Unavailable) });
        }
        let (sender, receiver) = oneshot::channel();
        let cell = Arc::new(ReplyCell {
            sender: Mutex::new(Some(sender)),
        });
        let guard = RequestGuard {
            cell: cell.clone(),
            pending: self.pending.clone(),
        };
        if let Err(work) = self.sender.try_send(make(cell)) {
            match work {
                mpsc::TrySendError::Full(work) | mpsc::TrySendError::Disconnected(work) => {
                    work.failed();
                }
            }
        }
        // The guard is created before the future, so dropping an unpolled
        // future also cancels its reply and releases its capacity permit.
        Box::pin(async move {
            let _guard = guard;
            receiver.await.unwrap_or(Err(SignerError::Canceled))
        })
    }
}

struct RequestGuard<T> {
    cell: Arc<ReplyCell<T>>,
    pending: Arc<AtomicUsize>,
}

impl<T> Drop for RequestGuard<T> {
    fn drop(&mut self) {
        self.cell.cancel();
        self.pending.fetch_sub(1, Ordering::AcqRel);
    }
}

impl transport::DeviceSigner for SignerBridge {
    fn public_key(&self) -> transport::SignerFuture<'_, PublicKey> {
        self.request(|cell| Work::PublicKey(Arc::new(PublicKeyReply { cell })))
    }

    fn sign_auth(
        &self,
        transcript: transport::AuthTranscript,
    ) -> transport::SignerFuture<'_, Signature> {
        if transcript.signing_bytes().len() > native::AUTH_TRANSCRIPT_MAX {
            return Box::pin(async { Err(SignerError::InvalidProof) });
        }
        self.request(|cell| {
            Work::Auth(
                Arc::new(AuthTranscript { inner: transcript }),
                Arc::new(SignatureReply { cell }),
            )
        })
    }

    fn sign_enrollment(
        &self,
        transcript: transport::EnrollTranscript,
    ) -> transport::SignerFuture<'_, Signature> {
        self.request(|cell| {
            Work::Enrollment(
                Arc::new(EnrollTranscript { inner: transcript }),
                Arc::new(SignatureReply { cell }),
            )
        })
    }

    fn retire(&self) -> transport::SignerFuture<'_, ()> {
        self.request(|cell| Work::Retire(Arc::new(RetirementReply { cell })))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use transport::DeviceSigner as _;

    struct Capture(mpsc::Sender<Arc<PublicKeyReply>>);

    impl DeviceSigner for Capture {
        fn public_key(&self, reply: Arc<PublicKeyReply>) {
            self.0.send(reply).unwrap();
        }
        fn sign_auth(&self, _: Arc<AuthTranscript>, reply: Arc<SignatureReply>) {
            reply.fail(SignerError::Unavailable).unwrap();
        }
        fn sign_enrollment(&self, _: Arc<EnrollTranscript>, reply: Arc<SignatureReply>) {
            reply.fail(SignerError::Unavailable).unwrap();
        }
        fn retire(&self, reply: Arc<RetirementReply>) {
            reply.complete().unwrap();
        }
    }

    fn runtime() -> tokio::runtime::Runtime {
        tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap()
    }

    fn capture() -> (Arc<SignerBridge>, mpsc::Receiver<Arc<PublicKeyReply>>) {
        let (sender, receiver) = mpsc::channel();
        (
            SignerBridge::new(Box::new(Capture(sender))).unwrap(),
            receiver,
        )
    }

    #[test]
    fn one_shot_reply_completes_once_and_redacts_values() {
        let (bridge, received) = capture();
        let future = bridge.public_key();
        let reply = received
            .recv_timeout(std::time::Duration::from_secs(2))
            .unwrap();
        assert!(reply.is_pending());
        reply.complete(vec![7; 32]).unwrap();
        assert_eq!(
            reply.complete(vec![9; 32]),
            Err(ReplyError::AlreadyCompleted)
        );
        assert_eq!(
            reply.fail(SignerError::Locked),
            Err(ReplyError::AlreadyCompleted)
        );
        assert_eq!(runtime().block_on(future), Ok(HexBytes([7; 32])));
        assert!(!reply.is_pending());
        assert_eq!(format!("{reply:?}"), "PublicKeyReply(<redacted>)");
        assert_eq!(bridge.pending.load(Ordering::Acquire), 0);
    }

    #[test]
    fn dropping_unpolled_future_cancels_reply_without_a_foreign_callback() {
        let (bridge, received) = capture();
        let future = bridge.public_key();
        let reply = received
            .recv_timeout(std::time::Duration::from_secs(2))
            .unwrap();
        drop(future);
        assert!(!reply.is_pending());
        assert_eq!(
            reply.complete(vec![7; 32]),
            Err(ReplyError::AlreadyCompleted)
        );
        assert_eq!(bridge.pending.load(Ordering::Acquire), 0);
        assert!(received.try_recv().is_err());
    }

    #[test]
    fn dropped_foreign_reply_and_wrong_width_fail_typed_without_hanging() {
        let (bridge, received) = capture();
        let future = bridge.public_key();
        drop(
            received
                .recv_timeout(std::time::Duration::from_secs(2))
                .unwrap(),
        );
        assert_eq!(runtime().block_on(future), Err(SignerError::Canceled));

        let future = bridge.public_key();
        let reply = received
            .recv_timeout(std::time::Duration::from_secs(2))
            .unwrap();
        assert_eq!(reply.complete(vec![1; 31]), Err(ReplyError::InvalidProof));
        assert_eq!(runtime().block_on(future), Err(SignerError::InvalidProof));
        assert_eq!(
            reply.complete(vec![1; 32]),
            Err(ReplyError::AlreadyCompleted)
        );

        let (sender, received) = oneshot::channel();
        let reply = SignatureReply {
            cell: Arc::new(ReplyCell {
                sender: Mutex::new(Some(sender)),
            }),
        };
        assert_eq!(reply.complete(vec![1; 65]), Err(ReplyError::InvalidProof));
        assert_eq!(
            received.blocking_recv().unwrap(),
            Err(SignerError::InvalidProof)
        );
        assert_eq!(
            reply.complete(vec![1; 64]),
            Err(ReplyError::AlreadyCompleted)
        );
    }

    #[test]
    fn pending_capacity_is_bounded_and_released_by_cancellation() {
        let (bridge, received) = capture();
        let futures: Vec<_> = (0..MAX_PENDING).map(|_| bridge.public_key()).collect();
        let replies: Vec<_> = (0..MAX_PENDING)
            .map(|_| {
                received
                    .recv_timeout(std::time::Duration::from_secs(2))
                    .unwrap()
            })
            .collect();
        assert_eq!(
            runtime().block_on(bridge.public_key()),
            Err(SignerError::Unavailable)
        );
        drop(futures);
        assert!(replies.iter().all(|reply| !reply.is_pending()));
        assert_eq!(bridge.pending.load(Ordering::Acquire), 0);
        let future = bridge.public_key();
        received
            .recv_timeout(std::time::Duration::from_secs(2))
            .unwrap()
            .fail(SignerError::Locked)
            .unwrap();
        assert_eq!(runtime().block_on(future), Err(SignerError::Locked));
    }
}
