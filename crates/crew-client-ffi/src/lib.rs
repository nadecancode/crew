//! Typed native representations of the shared client API.
//!
//! The default build has no foreign-language runtime dependency. `bindings`
//! enables the generated Swift boundary; `bindgen` additionally enables the
//! host-only generator. Protocol frames, credentials and client effects are not
//! part of this interface.

#[cfg(feature = "bindings")]
uniffi::setup_scaffolding!("crew_client_ffi");

#[cfg(feature = "bindings")]
pub mod records;

#[cfg(feature = "bindings")]
pub mod callbacks;

#[cfg(feature = "bindings")]
pub mod service;

/// The daemon protocol understood by this binding's shared Rust dependency.
#[cfg(feature = "bindings")]
#[uniffi::export]
pub fn protocol_version() -> u32 {
    crew_protocol::rpc::PROTOCOL_VERSION
}
