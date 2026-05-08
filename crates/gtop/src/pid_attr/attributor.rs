use async_trait::async_trait;

use crate::pid_attr::{AttributorTier, ProcessNetSample, Result};

/// Per-process network attribution backend.
///
/// One implementation per tier. Backends are created via their own
/// constructors (or [`crate::pid_attr::select`] for runtime tier selection) and held
/// behind a `Box<dyn NetworkAttributor>` so the daemon can swap tiers without
/// generic plumbing.
///
/// `available()` is `where Self: Sized` because it's a static probe — it
/// asks "could I be created on this host" without instantiating, so it can't
/// be dispatched through a trait object. That's intentional: callers that
/// want to know what's possible call it on each concrete type, while everyday
/// consumers just hold the `dyn` and call [`sample`].
#[async_trait]
pub trait NetworkAttributor: Send + Sync {
    /// Take one snapshot of per-process network state.
    async fn sample(&self) -> Result<Vec<ProcessNetSample>>;

    /// Which tier this backend reports as. Echoed into every
    /// [`ProcessNetSample`] so the TUI can format consistently.
    fn tier(&self) -> AttributorTier;

    /// Static availability probe. Implementations should be cheap and
    /// non-mutating: filesystem stat, capability lookup, etc.
    fn available() -> bool
    where
        Self: Sized;
}
