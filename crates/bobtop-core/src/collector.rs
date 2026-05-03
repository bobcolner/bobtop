use std::time::Duration;

use async_trait::async_trait;

use crate::Result;

/// The async data-source contract every subsystem implements.
///
/// `collect` takes `&self` rather than `&mut self`, which means stateful
/// collectors (CPU deltas, network rates) must use interior mutability —
/// typically `tokio::sync::Mutex` for state that crosses await points or
/// `std::sync::Mutex` / atomics for purely synchronous bookkeeping. This
/// shape lets the daemon hold collectors behind `Arc` and share them across
/// the periodic-tick task and any ad-hoc readers.
#[async_trait]
pub trait Collector: Send + Sync {
    /// The strongly-typed sample produced by one `collect` call.
    type Sample: Send + Clone;

    /// Produce a single sample. Must not panic on malformed `/proc` data,
    /// missing files, or other recoverable I/O errors — return `Err` instead.
    async fn collect(&self) -> Result<Self::Sample>;

    /// Recommended polling interval. The daemon uses this to schedule the
    /// collector's tick; consumers may render at a different cadence.
    fn interval(&self) -> Duration;

    /// Stable identifier for logs and metrics ("cpu", "memory", ...).
    fn name(&self) -> &'static str;
}
