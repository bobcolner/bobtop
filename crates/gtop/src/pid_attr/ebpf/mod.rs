//! eBPF kernel attribution backends.
//!
//! Two independent BPF objects, one per subsystem:
//! - [`net::EbpfAttributor`]  — Tier 3 network (TCP kprobes)
//! - [`disk::EbpfDiskAttributor`] — Tier 2 disk (vfs_read/vfs_write kretprobes)
//!
//! Each loads its own libbpf-rs skeleton so failures are isolated (disk
//! attach fails → net still works). Common helpers (capability probes,
//! `/proc/[pid]/comm`) live in [`common`].
//!
//! Userspace loader: libbpf-rs (replaced aya 0.13 after persistent ELF-
//! parse failures on hosts where the BPF object was valid).

#![cfg(all(target_os = "linux", feature = "ebpf"))]
// libbpf-rs needs unsafe blocks for the `MaybeUninit<OpenObject>` lifetime
// extension and for byte-level map-value reads. The rest of the crate
// stays `forbid(unsafe_code)`.
#![allow(unsafe_code)]

pub(crate) mod common;
pub mod disk;
pub mod net;

// Re-exports preserve the `crate::pid_attr::ebpf::EbpfAttributor` import path that
// existed before the disk tier landed.
pub use net::EbpfAttributor;
