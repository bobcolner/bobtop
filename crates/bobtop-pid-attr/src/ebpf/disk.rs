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
//! Loader: libbpf-rs (see [`super::net`] for migration rationale).

#![cfg(all(target_os = "linux", feature = "ebpf"))]

use std::collections::{HashMap, HashSet};
use std::mem::MaybeUninit;
use std::sync::{Arc, Mutex};
use std::time::Instant;

use async_trait::async_trait;
use libbpf_rs::skel::{OpenSkel, Skel, SkelBuilder};
use libbpf_rs::{MapCore as _, MapFlags};

use super::common::{has_bpf_capability, has_kernel_min_version, is_cgroup_v2_unified_mounted};
use crate::disk_attributor::{DiskAttributor, DiskAttributorTier, ProcessDiskSample};
use crate::{NetError, Result};

#[cfg(bobtop_bpf_built)]
mod skel {
    include!(concat!(env!("OUT_DIR"), "/disk.skel.rs"));
}

#[repr(C)]
#[derive(Debug, Default, Clone, Copy)]
struct PidDiskBytes {
    r: u64,
    w: u64,
}

unsafe fn pid_disk_bytes_from_slice(s: &[u8]) -> Option<PidDiskBytes> {
    if s.len() < std::mem::size_of::<PidDiskBytes>() {
        return None;
    }
    let mut out = PidDiskBytes::default();
    let p = &mut out as *mut PidDiskBytes as *mut u8;
    std::ptr::copy_nonoverlapping(s.as_ptr(), p, std::mem::size_of::<PidDiskBytes>());
    Some(out)
}

const IDLE_EVICTION_THRESHOLD: u32 = 30;

pub struct EbpfDiskAttributor {
    inner: Arc<Mutex<Inner>>,
}

struct Inner {
    #[cfg(bobtop_bpf_built)]
    skel: skel::BobtopDiskSkel<'static>,
    #[cfg(bobtop_bpf_built)]
    _open_object: Box<MaybeUninit<libbpf_rs::OpenObject>>,
    last_seen: HashMap<u32, (u64, u64, Instant)>,
    idle_streak: HashMap<u32, u32>,
}

impl std::fmt::Debug for EbpfDiskAttributor {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("EbpfDiskAttributor").finish()
    }
}

impl EbpfDiskAttributor {
    #[cfg(bobtop_bpf_built)]
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

        let mut open_object: Box<MaybeUninit<libbpf_rs::OpenObject>> =
            Box::new(MaybeUninit::uninit());
        let open_object_ref: &'static mut MaybeUninit<libbpf_rs::OpenObject> =
            unsafe { std::mem::transmute(&mut *open_object) };

        let builder = skel::BobtopDiskSkelBuilder::default();
        let open = builder
            .open(open_object_ref)
            .map_err(|e| NetError::Backend { backend: "libbpf-open", source: Box::new(e) })?;
        let mut skel = open
            .load()
            .map_err(|e| NetError::Backend { backend: "libbpf-load", source: Box::new(e) })?;
        skel.attach()
            .map_err(|e| NetError::Backend { backend: "libbpf-attach", source: Box::new(e) })?;

        tracing::info!("ebpf disk attributor: kretprobes attached (vfs_read, vfs_write)");

        Ok(Self {
            inner: Arc::new(Mutex::new(Inner {
                skel,
                _open_object: open_object,
                last_seen: HashMap::new(),
                idle_streak: HashMap::new(),
            })),
        })
    }

    #[cfg(not(bobtop_bpf_built))]
    pub fn new() -> Result<Self> {
        Err(NetError::other(
            "Disk BPF object not compiled — install clang + libbpf-dev and rebuild with --features ebpf",
        ))
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
        cfg!(bobtop_bpf_built)
            && is_cgroup_v2_unified_mounted()
            && has_kernel_min_version(5, 8)
            && has_bpf_capability()
    }
}

#[cfg(bobtop_bpf_built)]
fn sample_blocking(inner: &Mutex<Inner>) -> Result<Vec<ProcessDiskSample>> {
    let mut g = inner.lock().unwrap_or_else(|p| p.into_inner());
    let now = Instant::now();

    let current: Vec<(u32, PidDiskBytes)> = {
        let map = &g.skel.maps.pid_disk_bytes_map;
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
            // SAFETY: PidDiskBytes is `#[repr(C)]` of two u64s, no
            // pointers; any bit pattern is a valid value.
            let Some(v) = (unsafe { pid_disk_bytes_from_slice(&val) }) else {
                continue;
            };
            out.push((pid, v));
        }
        out
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
        let map = &g.skel.maps.pid_disk_bytes_map;
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

#[cfg(not(bobtop_bpf_built))]
fn sample_blocking(_inner: &Mutex<Inner>) -> Result<Vec<ProcessDiskSample>> {
    Ok(Vec::new())
}
