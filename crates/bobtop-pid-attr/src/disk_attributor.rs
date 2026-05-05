//! Per-process disk I/O attribution.
//!
//! Mirrors [`crate::NetworkAttributor`]. Two tiers planned:
//! - **Tier 2 — eBPF.** Kretprobes on `vfs_read` / `vfs_write`, accurate
//!   per-pid byte counts with the *originating* tgid (not writeback threads).
//!   Linux ≥ 5.8 + `CAP_BPF` or root. (Implemented in a follow-up commit.)
//! - **Tier 1 — `/proc/[pid]/io`.** Single open per pid per sample. Suffers
//!   the well-known kernel issue where buffered writes are credited to the
//!   writeback thread, not the issuing process — but it's universal on
//!   Linux and needs no privilege.
//!
//! No equivalent of pcap exists for disk: there's no userspace packet-capture
//! moral equivalent. Either you have kernel tracing (eBPF) or you're stuck
//! with what `/proc` exposes.

use async_trait::async_trait;
use serde::{Deserialize, Serialize};

use crate::Result;

/// Per-process disk I/O snapshot. Rates are bytes/sec averaged over the
/// interval since the previous sample for that pid.
///
/// `None` means "we don't have data for this pid yet" (first sight, or
/// tier doesn't expose bytes — hypothetically). The daemon's join logic
/// falls back to the prior sysinfo-based rate when this is `None`.
#[derive(Debug, Clone)]
pub struct ProcessDiskSample {
    pub pid: u32,
    pub read_bytes_per_sec: Option<f64>,
    pub write_bytes_per_sec: Option<f64>,
    pub tier: DiskAttributorTier,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum DiskAttributorTier {
    /// Tier 2 — Linux + `CAP_BPF`. Kretprobes on `vfs_read` / `vfs_write`.
    /// Accurate attribution, captures bytes the process actually issued.
    EbpfKernel,
    /// Tier 1 — Linux `/proc/[pid]/io`. Universal but suffers writeback
    /// misattribution: buffered writes get credited to the kernel writeback
    /// thread, not the originating process.
    ProcIo,
    /// Tier 0 — no disk attribution. Disk columns fall back to whatever the
    /// process collector itself derived (today: sysinfo).
    Unavailable,
}

impl DiskAttributorTier {
    pub fn name(self) -> &'static str {
        match self {
            Self::EbpfKernel => "ebpf",
            Self::ProcIo => "proc_io",
            Self::Unavailable => "unavailable",
        }
    }

    /// Whether this tier provides per-pid byte rates (vs. unavailable).
    /// Both eBPF and proc_io return rates today; only `Unavailable` doesn't.
    pub fn has_rates(self) -> bool {
        !matches!(self, Self::Unavailable)
    }
}

/// Per-process disk I/O attribution backend.
#[async_trait]
pub trait DiskAttributor: Send + Sync {
    /// Take one snapshot of per-process disk state.
    async fn sample(&self) -> Result<Vec<ProcessDiskSample>>;

    /// Which tier this backend reports as. Echoed into every sample.
    fn tier(&self) -> DiskAttributorTier;

    /// Static availability probe. Cheap, non-mutating.
    fn available() -> bool
    where
        Self: Sized;
}
