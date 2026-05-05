//! Tier 3 — Linux eBPF (kprobes on `tcp_sendmsg` + `tcp_cleanup_rbuf`).
//!
//! Two kprobes maintain a `BPF_MAP_TYPE_HASH` keyed by tgid (process pid)
//! holding `(rx, tx)` byte counters. Userspace polls the map every sample
//! interval and computes per-pid bytes-per-second from deltas. See the
//! companion C source at `crates/bobtop-pid-attr/bpf/bobtop_net.bpf.c`.
//!
//! - `tcp_sendmsg(struct sock*, struct msghdr*, size_t size)` → `+= size` to TX
//! - `tcp_cleanup_rbuf(struct sock*, int copied)` → `+= copied` to RX
//!
//! Both kernel symbols are stable across recent kernel versions.
//!
//! Privileges: `CAP_BPF` + `CAP_PERFMON` (preferred), or root. Kernel ≥ 5.8.

#![cfg(all(target_os = "linux", feature = "ebpf"))]
#![allow(unsafe_code)]

use std::collections::{HashMap, HashSet};
use std::sync::{Arc, Mutex};
use std::time::Instant;

use async_trait::async_trait;

use super::common::{
    attach_kprobe, has_bpf_capability, has_kernel_min_version, is_cgroup_v2_unified_mounted,
    load_with_memlock_fallback, read_proc_comm,
};
use crate::{AttributorTier, NetError, NetworkAttributor, ProcessNetSample, Result};

#[cfg(bobtop_bpf_built)]
const BPF_OBJECT: &[u8] = include_bytes!(env!("BOBTOP_BPF_OBJ"));
#[cfg(not(bobtop_bpf_built))]
const BPF_OBJECT: &[u8] = &[];

const MAP_NAME: &str = "pid_bytes_map";
const SEND_PROG: &str = "probe_tcp_sendmsg";
const RECV_PROG: &str = "probe_tcp_cleanup_rbuf";

/// Mirror of `struct pid_bytes` in the BPF C source. Field order and
/// alignment must match exactly.
#[repr(C)]
#[derive(Debug, Default, Clone, Copy)]
struct PidBytes {
    rx: u64,
    tx: u64,
}

// SAFETY: PidBytes has no padding (two u64 fields), no pointers, and any bit
// pattern is a valid value. That satisfies aya's Pod contract.
unsafe impl aya::Pod for PidBytes {}

pub struct EbpfAttributor {
    inner: Arc<Mutex<Inner>>,
}

struct Inner {
    ebpf: aya::Ebpf,
    /// Last absolute (rx, tx, observed_at) per pid, for delta computation.
    last_seen: HashMap<u32, (u64, u64, Instant)>,
    /// How many consecutive zero-delta samples each pid has produced. Used
    /// to evict idle pids from the BPF hash map so `iter()` in `sample()`
    /// stays bounded by *currently active* TCP-senders rather than every
    /// pid that has ever sent traffic since boot.
    idle_streak: HashMap<u32, u32>,
}

const IDLE_EVICTION_THRESHOLD: u32 = 30;

impl std::fmt::Debug for EbpfAttributor {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("EbpfAttributor").finish()
    }
}

impl EbpfAttributor {
    pub fn new() -> Result<Self> {
        if BPF_OBJECT.is_empty() {
            return Err(NetError::other(
                "BPF object not compiled — install clang + libbpf-dev and rebuild with --features ebpf",
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

        // Attach kprobes. If the second attach fails, returning Err drops
        // `ebpf` here — aya's Drop walks all programs and detaches them.
        attach_kprobe(&mut ebpf, SEND_PROG, "tcp_sendmsg")?;
        attach_kprobe(&mut ebpf, RECV_PROG, "tcp_cleanup_rbuf")?;

        tracing::info!("ebpf net attributor: kprobes attached (tcp_sendmsg, tcp_cleanup_rbuf)");

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
        !BPF_OBJECT.is_empty()
            && is_cgroup_v2_unified_mounted()
            && has_kernel_min_version(5, 8)
            && has_bpf_capability()
    }
}

fn sample_blocking(inner: &Mutex<Inner>) -> Result<Vec<ProcessNetSample>> {
    let mut g = inner.lock().unwrap_or_else(|p| p.into_inner());
    let now = Instant::now();

    // Pull the entire BPF hash map into a local Vec, then drop the borrow on
    // `g.ebpf` so we can mutate `g.last_seen` below.
    let current: Vec<(u32, PidBytes)> = {
        let map_data = g
            .ebpf
            .map_mut(MAP_NAME)
            .ok_or_else(|| NetError::other(format!("map `{MAP_NAME}` not found")))?;
        let map: aya::maps::HashMap<_, u32, PidBytes> = aya::maps::HashMap::try_from(map_data)
            .map_err(|e| NetError::Backend { backend: "aya-map", source: Box::new(e) })?;
        map.iter().filter_map(|r| r.ok()).collect()
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

    // Update last_seen and evict pids that have disappeared from the map.
    let live: HashSet<u32> = current.iter().map(|(p, _)| *p).collect();
    g.last_seen.retain(|pid, _| live.contains(pid));
    g.idle_streak.retain(|pid, _| live.contains(pid));
    for (pid, bytes) in current {
        g.last_seen.insert(pid, (bytes.rx, bytes.tx, now));
    }

    if !to_evict.is_empty() {
        if let Some(map_data) = g.ebpf.map_mut(MAP_NAME) {
            if let Ok(mut map) = aya::maps::HashMap::<_, u32, PidBytes>::try_from(map_data) {
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
