//! Latest-value per-pid attribution snapshot.
//!
//! Sits between the attributor sampler tasks (which produce
//! `Vec<ProcessNetSample>` / `Vec<ProcessDiskSample>` periodically) and
//! the process collector (which joins per-pid rates into `ProcessInfo`
//! before publishing on the bus).
//!
//! Architectural intent: the store decouples **who measures** (eBPF / pcap
//! / /proc inode for net; eBPF / /proc IO for disk) from **who consumes**
//! (process collector, agent socket, future exporters), so per-pid
//! bandwidth flows through one source of truth instead of being written
//! directly into the TUI's `App` mutex.

use std::collections::HashMap;
use std::sync::{Arc, RwLock};

use crate::{AttributorTier, DiskAttributorTier, ProcessDiskSample, ProcessNetSample};

/// Compact per-pid net rate. Bandwidth fields are `Option<f64>` because
/// Tier 1 backends can enumerate connections but can't measure bytes —
/// `None` means "we don't know" (different from `Some(0.0)`).
#[derive(Debug, Clone, Copy, Default)]
pub struct NetAttribution {
    pub rx_bytes_per_sec: Option<f64>,
    pub tx_bytes_per_sec: Option<f64>,
}

/// Compact per-pid disk rate. `None` rates fall back to whatever the
/// process collector's sysinfo path produced, so missing pids don't blank
/// the disk columns during attributor warmup.
#[derive(Debug, Clone, Copy, Default)]
pub struct DiskAttribution {
    pub read_bytes_per_sec: Option<f64>,
    pub write_bytes_per_sec: Option<f64>,
}

/// Inner state held behind a single `RwLock`. One writer per channel
/// (net loop, disk loop), N readers (process collector primarily).
#[derive(Debug, Default)]
pub struct AttributionState {
    pub net: HashMap<u32, NetAttribution>,
    pub disk: HashMap<u32, DiskAttribution>,
    /// Active net tier; surfaced on the wire so consumers can interpret
    /// `None` bandwidth fields ("Tier 1 can't measure" vs. "warming up").
    pub net_tier: AttributorTier,
    pub disk_tier: DiskAttributorTier,
}

/// Cheap-to-clone handle. The `RwLock` is fine: writes are 1 Hz per
/// channel, reads are 1 Hz from the process collector and rare from
/// agent queries — contention is essentially zero.
#[derive(Debug, Clone, Default)]
pub struct AttributionStore {
    inner: Arc<RwLock<AttributionState>>,
}

impl AttributionStore {
    pub fn new() -> Self {
        Self::default()
    }

    /// Replace the net snapshot. Called by the network attributor loop.
    pub fn set_net(&self, samples: Vec<ProcessNetSample>, tier: AttributorTier) {
        let mut g = self.inner.write().expect("attribution store poisoned");
        g.net.clear();
        g.net.reserve(samples.len());
        for s in &samples {
            g.net.insert(
                s.pid,
                NetAttribution {
                    rx_bytes_per_sec: s.rx_bytes_per_sec,
                    tx_bytes_per_sec: s.tx_bytes_per_sec,
                },
            );
        }
        g.net_tier = tier;
    }

    /// Replace the disk snapshot. Called by the disk attributor loop.
    pub fn set_disk(&self, samples: Vec<ProcessDiskSample>, tier: DiskAttributorTier) {
        let mut g = self.inner.write().expect("attribution store poisoned");
        g.disk.clear();
        g.disk.reserve(samples.len());
        for s in &samples {
            g.disk.insert(
                s.pid,
                DiskAttribution {
                    read_bytes_per_sec: s.read_bytes_per_sec,
                    write_bytes_per_sec: s.write_bytes_per_sec,
                },
            );
        }
        g.disk_tier = tier;
    }

    /// One-shot read snapshot via the closure. Avoids exposing the lock.
    pub fn read<R>(&self, f: impl FnOnce(&AttributionState) -> R) -> R {
        let g = self.inner.read().expect("attribution store poisoned");
        f(&g)
    }

    /// Convenience: lookup a single pid's net attribution.
    pub fn net_for(&self, pid: u32) -> Option<NetAttribution> {
        self.read(|s| s.net.get(&pid).copied())
    }

    /// Convenience: lookup a single pid's disk attribution.
    pub fn disk_for(&self, pid: u32) -> Option<DiskAttribution> {
        self.read(|s| s.disk.get(&pid).copied())
    }

    /// Active net tier (defaults to `Unavailable` until first set).
    pub fn net_tier(&self) -> AttributorTier {
        self.read(|s| s.net_tier)
    }

    /// Active disk tier (defaults to `Unavailable` until first set).
    pub fn disk_tier(&self) -> DiskAttributorTier {
        self.read(|s| s.disk_tier)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn n(pid: u32, rx: Option<f64>, tx: Option<f64>) -> ProcessNetSample {
        ProcessNetSample {
            pid,
            name: "p".into(),
            rx_bytes_per_sec: rx,
            tx_bytes_per_sec: tx,
            connections: vec![],
            attributor_tier: AttributorTier::Unavailable,
        }
    }

    fn d(pid: u32, r: Option<f64>, w: Option<f64>) -> ProcessDiskSample {
        ProcessDiskSample {
            pid,
            read_bytes_per_sec: r,
            write_bytes_per_sec: w,
            tier: DiskAttributorTier::Unavailable,
        }
    }

    #[test]
    fn set_net_replaces_existing_snapshot() {
        let s = AttributionStore::new();
        s.set_net(vec![n(1, Some(100.0), None)], AttributorTier::ProcInode);
        assert_eq!(
            s.net_for(1).unwrap().rx_bytes_per_sec,
            Some(100.0)
        );
        // Replace — pid 1 must be cleared, pid 2 added.
        s.set_net(vec![n(2, Some(200.0), None)], AttributorTier::ProcInode);
        assert!(s.net_for(1).is_none());
        assert_eq!(s.net_for(2).unwrap().rx_bytes_per_sec, Some(200.0));
    }

    #[test]
    fn set_disk_records_tier_and_per_pid_rates() {
        let s = AttributionStore::new();
        s.set_disk(
            vec![d(7, Some(1024.0), Some(2048.0))],
            DiskAttributorTier::ProcIo,
        );
        assert_eq!(s.disk_tier(), DiskAttributorTier::ProcIo);
        let a = s.disk_for(7).unwrap();
        assert_eq!(a.read_bytes_per_sec, Some(1024.0));
        assert_eq!(a.write_bytes_per_sec, Some(2048.0));
    }

    #[test]
    fn empty_store_has_unavailable_tiers() {
        let s = AttributionStore::new();
        assert_eq!(s.net_tier(), AttributorTier::Unavailable);
        assert_eq!(s.disk_tier(), DiskAttributorTier::Unavailable);
        assert!(s.net_for(1).is_none());
        assert!(s.disk_for(1).is_none());
    }
}
