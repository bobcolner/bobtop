//! eBPF kernel attribution backends.
//!
//! Two independent BPF objects, one per subsystem:
//! - [`net::EbpfAttributor`]  — Tier 3 network (TCP kprobes)
//! - [`disk::EbpfDiskAttributor`] — Tier 2 disk (vfs_read/vfs_write kretprobes)
//!
//! Each loads its own `aya::Ebpf` so failures are isolated (disk attach
//! fails → net still works). Common helpers (capability probes, RLIMIT
//! fallback, kprobe attach) live in [`common`].

#![cfg(all(target_os = "linux", feature = "ebpf"))]

pub(crate) mod common;
pub mod disk;
pub mod net;

// Re-exports preserve the `crate::ebpf::EbpfAttributor` import path that
// existed before the disk tier landed.
pub use net::EbpfAttributor;
