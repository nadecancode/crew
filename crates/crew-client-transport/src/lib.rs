//! Native transports share one scoped session driver. The portable client owns
//! domain policy; this crate owns request correlation, bounded IO and clocks.
//! Rust transport ports are internal host interfaces, not a Swift frame API.

pub mod clock;
pub mod driver;
pub mod framing;
pub mod ports;

#[cfg(all(feature = "local", any(target_os = "linux", target_os = "macos")))]
pub mod local;

#[cfg(feature = "runtime")]
pub mod queue;

#[cfg(feature = "runtime")]
pub mod service;

#[cfg(feature = "runtime")]
pub use service::{ClientObserver, NativeSession};

pub use clock::{Clock, SystemClock};
pub use driver::{CommandCodec, DriverConfig, DriverPlan, Protocol6Codec, SessionDriver};
pub use ports::*;
