//! Tier 3 — Linux eBPF (kprobes on `tcp_sendmsg` + `tcp_cleanup_rbuf`).
//!
//! Two kprobes maintain a `BPF_MAP_TYPE_HASH` keyed by tgid (process pid)
//! holding `(rx, tx)` byte counters. Userspace polls the map every sample
//! interval and computes per-pid bytes-per-second from deltas. See the
//! companion C source at `crates/bobtop-pid-attr/bpf/gtop_net.bpf.c`.
//!
//! - `tcp_sendmsg(struct sock*, struct msghdr*, size_t size)` → `+= size` to TX
//! - `tcp_cleanup_rbuf(struct sock*, int copied)` → `+= copied` to RX
//!
//! Both kernel symbols are stable across recent kernel versions.
//!
//! Privileges: `CAP_BPF` + `CAP_PERFMON` (preferred), or root. Kernel ≥ 5.8.
//!
//! ## Loader
//!
//! Uses libbpf-rs. The build script generates a typed skeleton at
//! `$OUT_DIR/net.skel.rs` exposing `BobtopNetSkelBuilder` and accessors
//! for each map and program. Migrated from aya 0.13 after persistent
//! "error parsing ELF data" failures on hosts where the BPF object was
//! valid (aya's from-scratch ELF parser disagreed with newer LLVM
//! output). libbpf is the canonical loader maintained alongside the
//! kernel and tracks ELF/BTF format changes correctly.

#![cfg(all(target_os = "linux", feature = "ebpf"))]

use std::collections::{HashMap, HashSet};
use std::mem::MaybeUninit;
use std::sync::{Arc, Mutex};
use std::time::Instant;

use async_trait::async_trait;
use libbpf_rs::skel::{OpenSkel, Skel, SkelBuilder};
use libbpf_rs::{MapCore as _, MapFlags};

use super::common::{
    has_bpf_capability, has_kernel_min_version, is_cgroup_v2_unified_mounted, read_proc_comm,
};
use crate::{AttributorTier, NetError, NetworkAttributor, ProcessNetSample, Result};

#[cfg(gtop_bpf_built)]
mod skel {
    include!(concat!(env!("OUT_DIR"), "/net.skel.rs"));
}
#[cfg(not(gtop_bpf_built))]
mod skel {
    /// Stub when build.rs couldn't compile the BPF object (no clang or
    /// libbpf-cargo failed). `available()` reports false in that case so
    /// the runtime tier-selector falls through cleanly.
    pub const STUB: () = ();
}

/// Mirror of `struct pid_bytes` in the BPF C source.
#[repr(C)]
#[derive(Debug, Default, Clone, Copy)]
struct PidBytes {
    rx: u64,
    tx: u64,
}

// SAFETY: PidBytes is two u64s with no padding/pointers; any bit pattern is
// valid. Required by libbpf-rs's plain-bytes round-trip when looking up map
// entries.
unsafe fn pid_bytes_from_slice(s: &[u8]) -> Option<PidBytes> {
    if s.len() < std::mem::size_of::<PidBytes>() {
        return None;
    }
    let mut out = PidBytes::default();
    let p = &mut out as *mut PidBytes as *mut u8;
    std::ptr::copy_nonoverlapping(s.as_ptr(), p, std::mem::size_of::<PidBytes>());
    Some(out)
}

const IDLE_EVICTION_THRESHOLD: u32 = 30;

pub struct EbpfAttributor {
    inner: Arc<Mutex<Inner>>,
}

struct Inner {
    /// The loaded skeleton owns the kernel-side BPF object + attached
    /// kprobes. Drop = unload + detach.
    #[cfg(gtop_bpf_built)]
    skel: skel::BobtopNetSkel<'static>,
    #[cfg(gtop_bpf_built)]
    _open_object: Box<MaybeUninit<libbpf_rs::OpenObject>>,
    /// Last absolute (rx, tx, observed_at) per pid, for delta computation.
    last_seen: HashMap<u32, (u64, u64, Instant)>,
    /// How many consecutive zero-delta samples each pid has produced; used
    /// to evict idle pids from the BPF map so the iter cost stays bounded
    /// by *currently active* TCP-senders.
    idle_streak: HashMap<u32, u32>,
}

impl std::fmt::Debug for EbpfAttributor {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("EbpfAttributor").finish()
    }
}

impl EbpfAttributor {
    #[cfg(gtop_bpf_built)]
    pub fn new() -> Result<Self> {
        if !is_cgroup_v2_unified_mounted() {
            return Err(NetError::MissingCapability("cgroup v2 unified mount"));
        }
        if !has_kernel_min_version(5, 8) {
            return Err(NetError::MissingCapability("kernel >= 5.8"));
        }
        if !has_bpf_capability() {
            return Err(NetError::MissingCapability("CAP_BPF or root"));
        }

        // The OpenObject must outlive the skel — it owns the kernel-side
        // BPF object's memory. We Box it so its address is stable while
        // the skel borrows it.
        let mut open_object: Box<MaybeUninit<libbpf_rs::OpenObject>> =
            Box::new(MaybeUninit::uninit());
        // SAFETY: extending the lifetime to 'static is sound because we
        // store both `open_object` and `skel` in the same `Inner`, so they
        // drop together. Skel never escapes Inner.
        let open_object_ref: &'static mut MaybeUninit<libbpf_rs::OpenObject> =
            unsafe { std::mem::transmute(&mut *open_object) };

        let builder = skel::BobtopNetSkelBuilder::default();
        let open = builder
            .open(open_object_ref)
            .map_err(|e| NetError::Backend { backend: "libbpf-open", source: Box::new(e) })?;
        let mut skel = open
            .load()
            .map_err(|e| NetError::Backend { backend: "libbpf-load", source: Box::new(e) })?;
        skel.attach()
            .map_err(|e| NetError::Backend { backend: "libbpf-attach", source: Box::new(e) })?;

        tracing::info!("ebpf net attributor: kprobes attached (tcp_sendmsg, tcp_cleanup_rbuf)");

        Ok(Self {
            inner: Arc::new(Mutex::new(Inner {
                skel,
                _open_object: open_object,
                last_seen: HashMap::new(),
                idle_streak: HashMap::new(),
            })),
        })
    }

    #[cfg(not(gtop_bpf_built))]
    pub fn new() -> Result<Self> {
        Err(NetError::other(
            "BPF object not compiled — install clang + libbpf-dev and rebuild with --features ebpf",
        ))
    }
}

#[async_trait]
impl NetworkAttributor for EbpfAttributor {
    async fn sample(&self) -> Result<Vec<ProcessNetSample>> {
        let inner = Arc::clone(&self.inner);
        tokio::task::spawn_blocking(move || sample_blocking(&inner))
            .await
            .map_err(|e| NetError::other(format!("ebpf join: {e}")))?
    }

    fn tier(&self) -> AttributorTier {
        AttributorTier::EbpfKernel
    }

    fn available() -> bool {
        cfg!(gtop_bpf_built)
            && is_cgroup_v2_unified_mounted()
            && has_kernel_min_version(5, 8)
            && has_bpf_capability()
    }
}

#[cfg(gtop_bpf_built)]
fn sample_blocking(inner: &Mutex<Inner>) -> Result<Vec<ProcessNetSample>> {
    let mut g = inner.lock().unwrap_or_else(|p| p.into_inner());
    let now = Instant::now();

    // Snapshot the BPF map into a Vec<(pid, PidBytes)> so we can drop the
    // map borrow before mutating last_seen / idle_streak below. libbpf-rs
    // exposes typed iteration via the generated `maps` accessor.
    let current: Vec<(u32, PidBytes)> = {
        let map = &g.skel.maps.pid_bytes_map;
        let mut out = Vec::new();
        for key_bytes in map.keys() {
            if key_bytes.len() < std::mem::size_of::<u32>() {
                continue;
            }
            let mut pid_bytes = [0u8; 4];
            pid_bytes.copy_from_slice(&key_bytes[..4]);
            let pid = u32::from_ne_bytes(pid_bytes);
            let Ok(Some(val)) = map.lookup(&key_bytes, MapFlags::ANY) else {
                continue;
            };
            // SAFETY: PidBytes is `#[repr(C)]` of two u64s with no pointers;
            // any bit pattern is valid (matches the kernel-side struct).
            let Some(v) = (unsafe { pid_bytes_from_slice(&val) }) else {
                continue;
            };
            out.push((pid, v));
        }
        out
    };

    let mut out = Vec::with_capacity(current.len());
    let mut to_evict: Vec<u32> = Vec::new();
    for (pid, bytes) in &current {
        let (rx_rate, tx_rate) = match g.last_seen.get(pid) {
            Some((prev_rx, prev_tx, prev_t)) => {
                let dt = now.duration_since(*prev_t).as_secs_f64().max(0.001);
                (
                    bytes.rx.saturating_sub(*prev_rx) as f64 / dt,
                    bytes.tx.saturating_sub(*prev_tx) as f64 / dt,
                )
            }
            None => (0.0, 0.0),
        };
        let is_zero_delta = rx_rate == 0.0 && tx_rate == 0.0;
        if is_zero_delta && g.last_seen.contains_key(pid) {
            let streak = g.idle_streak.entry(*pid).or_insert(0);
            *streak += 1;
            if *streak >= IDLE_EVICTION_THRESHOLD {
                to_evict.push(*pid);
            }
            continue;
        }
        g.idle_streak.remove(pid);
        out.push(ProcessNetSample {
            pid: *pid,
            name: read_proc_comm(*pid).unwrap_or_else(|| format!("pid:{pid}")),
            rx_bytes_per_sec: Some(rx_rate),
            tx_bytes_per_sec: Some(tx_rate),
            connections: Vec::new(),
            attributor_tier: AttributorTier::EbpfKernel,
        });
    }

    // Evict pids that disappeared from the BPF map (process exited).
    let live: HashSet<u32> = current.iter().map(|(p, _)| *p).collect();
    g.last_seen.retain(|pid, _| live.contains(pid));
    g.idle_streak.retain(|pid, _| live.contains(pid));
    for (pid, bytes) in current {
        g.last_seen.insert(pid, (bytes.rx, bytes.tx, now));
    }

    // Prune long-idle pids from the BPF map so iter() stays bounded by
    // active senders. Errors are non-fatal — kernel may have already
    // evicted via map pressure or a concurrent kprobe re-inserted.
    if !to_evict.is_empty() {
        let map = &g.skel.maps.pid_bytes_map;
        for pid in &to_evict {
            let _ = map.delete(&pid.to_ne_bytes());
        }
        for pid in &to_evict {
            g.last_seen.remove(pid);
            g.idle_streak.remove(pid);
        }
    }

    Ok(out)
}

#[cfg(not(gtop_bpf_built))]
fn sample_blocking(_inner: &Mutex<Inner>) -> Result<Vec<ProcessNetSample>> {
    Ok(Vec::new())
}
