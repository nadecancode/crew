//! Native gateway adapters. Trust, proofs and setup policy remain in transport.

mod records;
mod setup;
mod signer;

pub use records::*;
pub(crate) use setup::GatewayState;
pub use setup::{EnrollmentHandle, SetupObserver};
pub use signer::{DeviceSigner, PublicKeyReply, ReplyError, RetirementReply, SignatureReply};
