//! Compile the owned pure modules before the core owner wires its exports.
//! Both this harness and the later library exports execute the same source.

pub mod api {
    pub use crew_client::api::*;
}
pub use crew_client::satellite_parent_id;

#[path = "../src/topology.rs"]
mod topology;
#[path = "../src/usage.rs"]
mod usage;
