use async_trait::async_trait;

use crate::{AttributorTier, NetworkAttributor, ProcessNetSample, Result};

/// Tier 0 fallback. Returns an empty sample. Picked when no other backend is
/// available so the rest of the app doesn't have to special-case `None`.
#[derive(Debug, Default, Clone, Copy)]
pub struct UnavailableAttributor;

#[async_trait]
impl NetworkAttributor for UnavailableAttributor {
    async fn sample(&self) -> Result<Vec<ProcessNetSample>> {
        Ok(Vec::new())
    }

    fn tier(&self) -> AttributorTier {
        AttributorTier::Unavailable
    }

    fn available() -> bool {
        true
    }
}
