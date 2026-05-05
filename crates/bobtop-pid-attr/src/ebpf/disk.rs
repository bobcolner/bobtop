//! Tier 2 — Linux eBPF (kretprobes on `vfs_read` + `vfs_write`).
//!
//! Mirror of [`super::net::EbpfAttributor`] for disk I/O. Two kretprobes
//! credit per-tgid `(read_bytes, write_bytes)` using the syscall return
//! value (positive ssize_t = bytes transferred, negative = -errno).
//!
//! VFS-layer attribution attributes I/O to the process that called
//! `read()` / `write()` directly via `bpf_get_current_pid_tgid()` — not
//! the kernel writeback thread that `/proc/[pid]/io` blames for buffered
//! writes. That's the primary reason this tier exists.
//!
//! Privileges: same as the net tier — `CAP_BPF` (or root) + kernel ≥ 5.8.

#![cfg(all(target_os = "linux", feature = "ebpf"))]
#![allow(unsafe_code)]

use std::collections::{HashMap, HashSet};
use std::sync::{Arc, Mutex};
use std::time::Instant;

use async_trait::async_trait;

use super::common::{
    attach_kprobe, has_bpf_capability, has_kernel_min_version, is_cgroup_v2_unified_mounted,
    load_with_memlock_fallback,
};
use crate::disk_attributor::{DiskAttributor, DiskAttributorTier, ProcessDiskSample};
use crate::{NetError, Result};

#[cfg(bobtop_bpf_built)]
const BPF_OBJECT: &[u8] = include_bytes!(env!("BOBTOP_BPF_DISK_OBJ"));
#[cfg(not(bobtop_bpf_built))]
const BPF_OBJECT: &[u8] = &[];

const MAP_NAME: &str = "pid_disk_bytes_map";
const READ_PROG: &str = "probe_vfs_read_ret";
const WRITE_PROG: &str = "probe_vfs_write_ret";

/// Mirror of `struct pid_disk_bytes` in the BPF C source.
#[repr(C)]
#[derive(Debug, Default, Clone, Copy)]
struct PidDiskBytes {
    r: u64,
    w: u64,
}

// SAFETY: PidDiskBytes is two u64s with no padding/pointers; any bit pattern
// is a valid value, satisfying aya's Pod contract.
unsafe impl aya::Pod for PidDiskBytes {}

pub struct EbpfDiskAttributor {
    inner: Arc<Mutex<Inner>>,
}

struct Inner {
    ebpf: aya::Ebpf,
    /// Last absolute (read_bytes, write_bytes, observed_at) per pid.
    last_seen: HashMap<u32, (u64, u64, Instant)>,
    /// Idle-streak counter for evicting long-quiet pids from the BPF map,
    /// matching the bounding behavior of the net tier.
    idle_streak: HashMap<u32, u32>,
}

const IDLE_EVICTION_THRESHOLD: u32 = 30;

impl std::fmt::Debug for EbpfDiskAttributor {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("EbpfDiskAttributor").finish()
    }
}

impl EbpfDiskAttributor {
    pub fn new() -> Result<Self> {
        if BPF_OBJECT.is_empty() {
            return Err(NetError::other(
                "Disk BPF object not compiled — install clang + libbpf-dev and rebuild with --features ebpf",
            ));
        }
        if !is_cgroup_v2_unified_mounted() {
            return Err(NetError::MissingCapability("cgroup v2 unified mount"));
        }
        if !has_kernel_min_version(5, 8) {
            return Err(NetError::MissingCapability("kernel >= 5.8"));
        }
        if !has_bpf_capability() {
            return Err(NetError::MissingCapability("CAP_BPF or root"));
        }

        let mut ebpf = load_with_memlock_fallback(BPF_OBJECT)?;

        // aya treats kretprobes through the same KProbe program type — the
        // kernel-side SEC("kretprobe/...") declaration is what makes it a
        // return-probe. attach() targets the entry symbol; kernel routes.
        attach_kprobe(&mut ebpf, READ_PROG, "vfs_read")?;
        attach_kprobe(&mut ebpf, WRITE_PROG, "vfs_write")?;

        tracing::info!("ebpf disk attributor: kretprobes attached (vfs_read, vfs_write)");

        Ok(Self {
            inner: Arc::new(Mutex::new(Inner {
                ebpf,
                last_seen: HashMap::new(),
                idle_streak: HashMap::new(),
            })),
        })
    }
}

#[async_trait]
impl DiskAttributor for EbpfDiskAttributor {
    async fn sample(&self) -> Result<Vec<ProcessDiskSample>> {
        let inner = Arc::clone(&self.inner);
        tokio::task::spawn_blocking(move || sample_blocking(&inner))
            .await
            .map_err(|e| NetError::other(format!("ebpf disk join: {e}")))?
    }

    fn tier(&self) -> DiskAttributorTier {
        DiskAttributorTier::EbpfKernel
    }

    fn available() -> bool {
        !BPF_OBJECT.is_empty()
            && is_cgroup_v2_unified_mounted()
            && has_kernel_min_version(5, 8)
            && has_bpf_capability()
    }
}

fn sample_blocking(inner: &Mutex<Inner>) -> Result<Vec<ProcessDiskSample>> {
    let mut g = inner.lock().unwrap_or_else(|p| p.into_inner());
    let now = Instant::now();

    let current: Vec<(u32, PidDiskBytes)> = {
        let map_data = g
            .ebpf
            .map_mut(MAP_NAME)
            .ok_or_else(|| NetError::other(format!("map `{MAP_NAME}` not found")))?;
        let map: aya::maps::HashMap<_, u32, PidDiskBytes> =
            aya::maps::HashMap::try_from(map_data)
                .map_err(|e| NetError::Backend { backend: "aya-map", source: Box::new(e) })?;
        map.iter().filter_map(|r| r.ok()).collect()
    };

    let mut out = Vec::with_capacity(current.len());
    let mut to_evict: Vec<u32> = Vec::new();
    for (pid, bytes) in &current {
        let (rr, wr) = match g.last_seen.get(pid) {
            Some((pr, pw, pt)) => {
                let dt = now.duration_since(*pt).as_secs_f64().max(0.001);
                (
                    bytes.r.saturating_sub(*pr) as f64 / dt,
                    bytes.w.saturating_sub(*pw) as f64 / dt,
                )
            }
            None => (0.0, 0.0),
        };
        let is_zero_delta = rr == 0.0 && wr == 0.0;
        if is_zero_delta && g.last_seen.contains_key(pid) {
            let streak = g.idle_streak.entry(*pid).or_insert(0);
            *streak += 1;
            if *streak >= IDLE_EVICTION_THRESHOLD {
                to_evict.push(*pid);
            }
            continue;
        }
        g.idle_streak.remove(pid);
        out.push(ProcessDiskSample {
            pid: *pid,
            read_bytes_per_sec: Some(rr),
            write_bytes_per_sec: Some(wr),
            tier: DiskAttributorTier::EbpfKernel,
        });
    }

    let live: HashSet<u32> = current.iter().map(|(p, _)| *p).collect();
    g.last_seen.retain(|pid, _| live.contains(pid));
    g.idle_streak.retain(|pid, _| live.contains(pid));
    for (pid, bytes) in current {
        g.last_seen.insert(pid, (bytes.r, bytes.w, now));
    }

    if !to_evict.is_empty() {
        if let Some(map_data) = g.ebpf.map_mut(MAP_NAME) {
            if let Ok(mut map) = aya::maps::HashMap::<_, u32, PidDiskBytes>::try_from(map_data) {
                for pid in &to_evict {
                    let _ = map.remove(pid);
                }
            }
        }
        for pid in &to_evict {
            g.last_seen.remove(pid);
            g.idle_streak.remove(pid);
        }
    }

    Ok(out)
}
