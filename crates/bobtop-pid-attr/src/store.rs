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

use crate::{
    AttributorTier, ConnectionInfo, DiskAttributorTier, ProcessDiskSample, ProcessNetSample,
};

/// Compact per-pid net rate. Bandwidth fields are `Option<f64>` because
/// Tier 1 backends can enumerate connections but can't measure bytes —
/// `None` means "we don't know" (different from `Some(0.0)`).
#[derive(Debug, Clone, Copy, Default)]
pub struct NetAttribution {
    pub rx_bytes_per_sec: Option<f64>,
    pub tx_bytes_per_sec: Option<f64>,
}

/// One row of the flow view: a pid + a single connection it owns.
/// Built by flattening every active `ProcessNetSample.connections` —
/// downstream consumers (the flow panel, the agent) iterate this
/// directly rather than re-traversing the per-pid map.
#[derive(Debug, Clone)]
pub struct FlowRow {
    pub pid: u32,
    /// Process name at sample time. Stored on each row so flow tables
    /// don't need to dereference back into the live process table —
    /// the flow may outlive the process by one tick.
    pub name: String,
    pub conn: ConnectionInfo,
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
    /// Flat list of every (pid, connection) pair from the latest net
    /// sample. Replaced wholesale on each `set_net` call so stale flows
    /// from departed pids drop out automatically. Kept flat (rather
    /// than `HashMap<pid, Vec<ConnectionInfo>>`) so the flow panel can
    /// iterate, sort, and render without an extra hash lookup per row.
    pub flows: Vec<FlowRow>,
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

    /// Replace the per-pid byte snapshot. Called by the active byte
    /// attributor loop (eBPF / pcap / proc_inode). When the byte tier
    /// also enumerates connections (proc_inode does, eBPF and pcap
    /// don't), the flows view is updated from the same samples — that
    /// way single-tier setups don't need a separate flow loop. When
    /// the byte tier reports zero connections we leave the flow list
    /// untouched so the flow enumerator can keep ownership of it.
    pub fn set_net(&self, samples: Vec<ProcessNetSample>, tier: AttributorTier) {
        // Recover from a poisoned lock instead of propagating panic.
        // A poisoned lock means a previous writer panicked mid-write,
        // not that the data is corrupt — clearing the maps fully on
        // every set is safe regardless.
        let mut g = self
            .inner
            .write()
            .unwrap_or_else(|p| p.into_inner());
        g.net.clear();
        g.net.reserve(samples.len());
        let total_conns: usize = samples.iter().map(|s| s.connections.len()).sum();
        let supplies_conns = total_conns > 0;
        if supplies_conns {
            g.flows.clear();
            g.flows.reserve(total_conns);
        }
        for s in samples {
            g.net.insert(
                s.pid,
                NetAttribution {
                    rx_bytes_per_sec: s.rx_bytes_per_sec,
                    tx_bytes_per_sec: s.tx_bytes_per_sec,
                },
            );
            if supplies_conns {
                for conn in s.connections {
                    g.flows.push(FlowRow {
                        pid: s.pid,
                        name: s.name.clone(),
                        conn,
                    });
                }
            }
        }
        g.net_tier = tier;
    }

    /// Replace just the flow list — used by the dedicated flow
    /// enumerator loop (always proc_inode when available) so the flow
    /// panel still has data when the active byte tier is eBPF / pcap
    /// (those don't enumerate connections — they only count bytes).
    pub fn set_net_flows(&self, samples: Vec<ProcessNetSample>) {
        let mut g = self
            .inner
            .write()
            .unwrap_or_else(|p| p.into_inner());
        g.flows.clear();
        let total: usize = samples.iter().map(|s| s.connections.len()).sum();
        g.flows.reserve(total);
        for s in samples {
            for conn in s.connections {
                g.flows.push(FlowRow {
                    pid: s.pid,
                    name: s.name.clone(),
                    conn,
                });
            }
        }
    }

    /// Replace the disk snapshot. Called by the disk attributor loop.
    pub fn set_disk(&self, samples: Vec<ProcessDiskSample>, tier: DiskAttributorTier) {
        let mut g = self
            .inner
            .write()
            .unwrap_or_else(|p| p.into_inner());
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
        let g = self
            .inner
            .read()
            .unwrap_or_else(|p| p.into_inner());
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

    /// Snapshot the current flow list. Returns an owned copy so the
    /// caller can sort/filter without holding the read lock — flow
    /// counts run into the low thousands at most, so the clone is
    /// cheap relative to the render work the panel does next.
    pub fn flows(&self) -> Vec<FlowRow> {
        self.read(|s| s.flows.clone())
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

    fn n_with_conns(
        pid: u32,
        rx: Option<f64>,
        tx: Option<f64>,
        conns: Vec<ConnectionInfo>,
    ) -> ProcessNetSample {
        ProcessNetSample {
            pid,
            name: format!("p{pid}"),
            rx_bytes_per_sec: rx,
            tx_bytes_per_sec: tx,
            connections: conns,
            attributor_tier: AttributorTier::ProcInode,
        }
    }

    fn conn_v4(local_port: u16, remote_port: u16) -> ConnectionInfo {
        use crate::sample::AddrEndpoint;
        use std::net::Ipv4Addr;
        ConnectionInfo {
            local: AddrEndpoint::V4 {
                addr: Ipv4Addr::new(127, 0, 0, 1),
                port: local_port,
            },
            remote: AddrEndpoint::V4 {
                addr: Ipv4Addr::new(8, 8, 8, 8),
                port: remote_port,
            },
            state: crate::sample::SocketState::Established,
            protocol: crate::sample::Protocol::Tcp,
        }
    }

    #[test]
    fn set_net_flattens_connections_into_flow_view() {
        let s = AttributionStore::new();
        s.set_net(
            vec![
                n_with_conns(
                    10,
                    Some(1000.0),
                    None,
                    vec![conn_v4(1234, 80), conn_v4(1234, 443)],
                ),
                n_with_conns(20, None, None, vec![conn_v4(5678, 22)]),
            ],
            AttributorTier::ProcInode,
        );
        let flows = s.flows();
        assert_eq!(flows.len(), 3, "all connections must appear");
        assert!(flows.iter().any(|f| f.pid == 10 && f.conn.remote.port() == 80));
        assert!(flows.iter().any(|f| f.pid == 10 && f.conn.remote.port() == 443));
        assert!(flows.iter().any(|f| f.pid == 20 && f.conn.remote.port() == 22));
        assert!(flows.iter().all(|f| !f.name.is_empty()));
    }

    #[test]
    fn set_net_flows_replaces_stale_entries() {
        let s = AttributionStore::new();
        // Seed flows via the dedicated flow path.
        s.set_net_flows(vec![n_with_conns(99, None, None, vec![conn_v4(1, 2)])]);
        assert_eq!(s.flows().len(), 1);
        // Re-publish without pid 99 — its flow row must vanish.
        s.set_net_flows(vec![n_with_conns(100, None, None, vec![])]);
        assert!(s.flows().is_empty(), "stale pid's flows linger");
    }

    #[test]
    fn set_net_preserves_flows_when_byte_tier_reports_no_connections() {
        // Mimic the eBPF / pcap case: byte attributor publishes per-pid
        // rates with empty `connections`. The flow list (owned by the
        // separate flow enumerator) must not be wiped.
        let s = AttributionStore::new();
        s.set_net_flows(vec![n_with_conns(7, None, None, vec![conn_v4(1, 80)])]);
        assert_eq!(s.flows().len(), 1);
        s.set_net(
            vec![n_with_conns(7, Some(1000.0), None, vec![])],
            AttributorTier::EbpfKernel,
        );
        assert_eq!(s.flows().len(), 1, "byte tier wiped flows it doesn't own");
        assert_eq!(s.net_for(7).unwrap().rx_bytes_per_sec, Some(1000.0));
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
