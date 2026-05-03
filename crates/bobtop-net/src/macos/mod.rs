//! Tier 1 — macOS `proc_pidinfo(PROC_PIDLISTFDS)`.
//!
//! Real implementation lands later (libproc / direct libc calls). Until then
//! this is an honest stub: `available()` returns `false`, so [`crate::select`]
//! falls through to [`crate::UnavailableAttributor`] on macOS rather than
//! pretending to work.

use async_trait::async_trait;

use crate::{AttributorTier, NetError, NetworkAttributor, ProcessNetSample, Result};

#[derive(Debug, Default)]
pub struct ProcPidInfoAttributor;

impl ProcPidInfoAttributor {
    pub fn new() -> Self {
        Self
    }
}

#[async_trait]
impl NetworkAttributor for ProcPidInfoAttributor {
    async fn sample(&self) -> Result<Vec<ProcessNetSample>> {
        Err(NetError::Unsupported)
    }

    fn tier(&self) -> AttributorTier {
        AttributorTier::ProcPidInfo
    }

    fn available() -> bool {
        false
    }
}
