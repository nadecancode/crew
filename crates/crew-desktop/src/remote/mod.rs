//! Remote gateway connections for the Linux app (`native-linux-gateway`).
//!
//! `connections` renders the Connections settings and trust/enrollment
//! ceremony in GTK; `worker` runs setup and gateway sessions on the native
//! service worker through the connector's `GatewayClient`; `secret` is the
//! Linux OS credential adapter (Secret Service `DeviceSigner`); `profile`
//! persists public host metadata only. Authority, grants and eligibility stay
//! in the shared connector and core.

mod connections;
mod copy;
pub mod profile;
pub mod secret;
mod worker;

#[cfg(test)]
mod tests;

pub use connections::{Host, LocalEntry, Remote};
pub use worker::{RemoteRequest, RemoteResponse, Step, Worker};
