//! Human documentation observer. Source registration and authentication belong
//! to the daemon; public RPC records are the frozen crew-protocol Docs types.
//! This service never sends agent messages or invokes snapshotting VCS commands.

pub mod assets;
pub mod config;
pub mod markdown;
pub mod source;

pub mod history;
pub mod index;

/// Stop serialization at the byte budget, including worst-case JSON escaping.
/// This is also used before allocating a serde_json::Value. P2 separately checks
/// the complete ServerFrame envelope at its transport queue boundary.
pub fn bounded_size(
    value: &impl serde::Serialize,
    limit: usize,
) -> Result<usize, crew_core::rpc::docs::DocFailureCode> {
    struct Count {
        bytes: usize,
        limit: usize,
    }
    impl std::io::Write for Count {
        fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
            if bytes.len() > self.limit.saturating_sub(self.bytes) {
                return Err(std::io::Error::other("Docs encoding limit"));
            }
            self.bytes += bytes.len();
            Ok(bytes.len())
        }
        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }
    let mut count = Count { bytes: 0, limit };
    serde_json::to_writer(&mut count, value)
        .map_err(|_| crew_core::rpc::docs::DocFailureCode::TooLarge)?;
    Ok(count.bytes)
}
