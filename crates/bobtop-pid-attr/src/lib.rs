//! Tiered per-process kernel attribution for bobtop — network and disk I/O.
//!
//! Four backends, ordered by accuracy and privilege requirements:
//!
//! | Tier | Backend                              | Privileges          | Bandwidth | Connections |
//! |------|--------------------------------------|---------------------|-----------|-------------|
//! | 3    | [`ebpf::EbpfAttributor`]             | Linux + `CAP_BPF`   | exact     | exact       |
//! | 2    | [`pcap_backend::PcapAttributor`]     | `CAP_NET_RAW`       | sampled   | exact       |
//! | 1    | [`proc_inode::ProcInodeAttributor`]  | none (Linux)        | —         | exact       |
//! | 1    | [`macos::ProcPidInfoAttributor`]     | none (macOS)        | —         | exact       |
//! | 0    | [`UnavailableAttributor`]            | none                | —         | —           |
//!
//! [`select`] probes the available backends at runtime and returns the
//! highest-tier one that initializes successfully. Callers can also force a
//! specific tier or pass capability hints (`--no-ebpf`, `--no-pcap`) via
//! [`SelectOptions`].

// `deny` rather than `forbid` so the `ebpf` module can opt in for the single
// `unsafe impl aya::Pod for PidBytes`. Every other unsafe block in the crate
// remains a hard error.
#![deny(unsafe_code)]

pub mod attributor;
pub mod disk_attributor;
pub mod error;
pub mod sample;
pub mod store;
pub mod tier;
pub mod unavailable;

#[cfg(target_os = "linux")]
pub mod proc_inode;

#[cfg(target_os = "linux")]
pub mod proc_io;

#[cfg(target_os = "linux")]
pub(crate) mod proc_walk;

#[cfg(target_os = "macos")]
pub mod macos;

#[cfg(feature = "pcap")]
pub mod pcap_backend;

#[cfg(all(target_os = "linux", feature = "ebpf"))]
pub mod ebpf;

pub use attributor::NetworkAttributor;
pub use disk_attributor::{DiskAttributor, DiskAttributorTier, ProcessDiskSample};
pub use error::{NetError, Result};
pub use sample::{
    AddrEndpoint, ConnectionInfo, ProcessNetSample, Protocol, SocketState,
};
pub use store::{AttributionState, AttributionStore, DiskAttribution, FlowRow, NetAttribution};
pub use tier::AttributorTier;
pub use unavailable::UnavailableAttributor;

/// Knobs that influence runtime tier selection.
#[derive(Debug, Clone, Copy)]
pub struct SelectOptions {
    /// If `false`, eBPF is excluded from selection even when available.
    /// Wired to the `--no-ebpf` CLI flag in the daemon.
    pub allow_ebpf: bool,
    /// If `false`, libpcap is excluded from selection even when available.
    /// Wired to the `--no-pcap` CLI flag in the daemon.
    pub allow_pcap: bool,
}

impl Default for SelectOptions {
    fn default() -> Self {
        Self {
            allow_ebpf: true,
            allow_pcap: true,
        }
    }
}

/// Probe each backend in descending tier order and return the first one that
/// reports itself available.
///
/// Logs the chosen tier and the rejection reason for each higher tier — when
/// users wonder why bobtop is showing connections-only and no per-process
/// bandwidth, the log line is the first place to look.
pub fn select(opts: SelectOptions) -> Box<dyn NetworkAttributor> {
    // Tier 3 — eBPF (Linux only, feature-gated).
    #[cfg(all(target_os = "linux", feature = "ebpf"))]
    if opts.allow_ebpf && ebpf::EbpfAttributor::available() {
        match ebpf::EbpfAttributor::new() {
            Ok(a) => {
                tracing::info!(tier = "ebpf", "network attribution: tier 3 (eBPF) selected");
                return Box::new(a);
            }
            Err(e) => tracing::warn!(error = %e, "eBPF available but failed to initialize"),
        }
    } else {
        #[cfg(all(target_os = "linux", feature = "ebpf"))]
        tracing::debug!(allow = opts.allow_ebpf, "eBPF tier skipped");
    }

    // Tier 2 — libpcap (cross-platform, feature-gated).
    #[cfg(feature = "pcap")]
    if opts.allow_pcap && pcap_backend::PcapAttributor::available() {
        match pcap_backend::PcapAttributor::new() {
            Ok(a) => {
                tracing::info!(tier = "pcap", "network attribution: tier 2 (libpcap) selected");
                return Box::new(a);
            }
            Err(e) => tracing::warn!(error = %e, "libpcap available but failed to initialize"),
        }
    }

    // Tier 1 — Linux /proc inode walk.
    #[cfg(target_os = "linux")]
    if proc_inode::ProcInodeAttributor::available() {
        tracing::info!(
            tier = "proc_inode",
            "network attribution: tier 1 (/proc inode) selected — connections only, no bandwidth"
        );
        return Box::new(proc_inode::ProcInodeAttributor::new());
    }

    // Tier 1 — macOS proc_pidinfo.
    #[cfg(target_os = "macos")]
    if macos::ProcPidInfoAttributor::available() {
        tracing::info!(
            tier = "proc_pidinfo",
            "network attribution: tier 1 (proc_pidinfo) selected — connections only, no bandwidth"
        );
        return Box::new(macos::ProcPidInfoAttributor::new());
    }

    tracing::warn!(
        "no network attribution backend available — process panel will hide net columns"
    );
    let _ = opts; // silence unused-warning when no tiers are compiled in
    Box::new(UnavailableAttributor::default())
}

/// Probe each disk-attribution backend in descending tier order. Returns
/// the highest-tier backend that initializes successfully, or `None` when
/// nothing is available (the daemon falls back to sysinfo's per-pid disk).
///
/// Honors the same `allow_ebpf` knob as the net selector — a single
/// `--no-ebpf` flag disables eBPF for both subsystems.
pub fn select_disk(opts: SelectOptions) -> Option<Box<dyn DiskAttributor>> {
    // Tier 2 — eBPF disk probes (vfs_read/vfs_write kretprobes).
    #[cfg(all(target_os = "linux", feature = "ebpf"))]
    if opts.allow_ebpf && ebpf::disk::EbpfDiskAttributor::available() {
        match ebpf::disk::EbpfDiskAttributor::new() {
            Ok(a) => {
                tracing::info!(
                    tier = "ebpf",
                    "disk attribution: tier 2 (eBPF) selected — accurate per-pid attribution"
                );
                return Some(Box::new(a));
            }
            Err(e) => tracing::warn!(error = %e, "eBPF disk available but failed to initialize"),
        }
    }

    // Tier 1 — /proc/[pid]/io.
    #[cfg(target_os = "linux")]
    if proc_io::ProcDiskAttributor::available() {
        tracing::info!(
            tier = "proc_io",
            "disk attribution: tier 1 (/proc/[pid]/io) selected — buffered writes credited to writeback threads"
        );
        return Some(Box::new(proc_io::ProcDiskAttributor::new()));
    }

    let _ = opts.allow_ebpf; // silence unused-warning on non-Linux / no-ebpf builds
    tracing::debug!("no disk attribution backend available — falling back to sysinfo");
    None
}
