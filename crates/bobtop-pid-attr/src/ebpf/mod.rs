//! Tier 3 — Linux eBPF (kprobes on `tcp_sendmsg` + `tcp_cleanup_rbuf`).
//!
//! ## Design
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
//! ## Build chain
//!
//! The BPF program is compiled by `build.rs` using `clang -target bpf`. When
//! the build succeeds, `build.rs` emits `--cfg bobtop_bpf_built` and the
//! object is embedded via `include_bytes!`. When clang isn't available the
//! tier compiles but reports `available() == false` so the runtime selector
//! falls through to a lower tier cleanly.
//!
//! ## Privileges
//!
//! Requires `CAP_BPF` + `CAP_PERFMON` (preferred), or root. Kernel ≥ 5.8 for
//! the unprivileged-CAP_BPF model; older kernels need `CAP_SYS_ADMIN`.

#![cfg(all(target_os = "linux", feature = "ebpf"))]
// Required for `unsafe impl aya::Pod for PidBytes` — the rest of the crate
// stays `forbid(unsafe_code)`.
#![allow(unsafe_code)]

use std::collections::{HashMap, HashSet};
use std::path::Path;
use std::sync::{Arc, Mutex};
use std::time::Instant;

use async_trait::async_trait;

use crate::{AttributorTier, NetError, NetworkAttributor, ProcessNetSample, Result};

// build.rs sets BOBTOP_BPF_OBJ + cfg(bobtop_bpf_built) on success.
#[cfg(bobtop_bpf_built)]
const BPF_OBJECT: &[u8] = include_bytes!(env!("BOBTOP_BPF_OBJ"));
#[cfg(not(bobtop_bpf_built))]
const BPF_OBJECT: &[u8] = &[];

const CGROUP2_PATH: &str = "/sys/fs/cgroup";
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

/// After this many consecutive zero-delta samples, evict the pid from the
/// BPF map. Keeps the map size bounded on long-running daemons; the pid
/// will reappear automatically the next time its kprobe fires. ~30 samples
/// at the default 1Hz net tier interval = ~30s of idle before eviction.
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

        // IMPORTANT: use `EbpfLoader` rather than the convenience
        // `aya::Ebpf::load(...)`. Despite the docs claiming they're
        // equivalent, the convenience function in aya 0.13.1 does an
        // additional kernel-BTF probe at load time that fails on hosts
        // whose kernel BTF doesn't match the BPF object exactly, returning
        // the cryptic "error parsing ELF data". `EbpfLoader::new()` skips
        // that probe and parses the bytes directly. Verified by sha256:
        // identical bytes that this loader rejects, the standalone aya
        // example accepts. Don't switch back without retesting on a host
        // where this previously failed.
        //
        // Try to load WITHOUT bumping RLIMIT_MEMLOCK first. Modern kernels
        // (5.11+) charge BPF allocations to memcg, not RLIMIT_MEMLOCK, so
        // the bump is unnecessary. Hosts with hardened sysctls forbid the
        // bump entirely; raising the limit eagerly would force this tier
        // to fall back to pcap on those hosts even though loading would
        // have succeeded. We only retry-with-bump on memory-pressure errors.
        let mut ebpf = match aya::EbpfLoader::new().load(BPF_OBJECT) {
            Ok(e) => e,
            Err(e) if is_memlock_error(&e) => {
                tracing::debug!("BPF load hit memlock limit, raising RLIMIT_MEMLOCK and retrying");
                bump_memlock_rlimit()?;
                aya::EbpfLoader::new()
                    .load(BPF_OBJECT)
                    .map_err(|e| NetError::Backend { backend: "aya-load", source: Box::new(e) })?
            }
            Err(e) => {
                return Err(NetError::Backend {
                    backend: "aya-load",
                    source: Box::new(e),
                })
            }
        };

        // Attach kprobes. If the second attach fails, returning Err drops
        // `ebpf` here — aya's Drop walks all programs and detaches them
        // (kprobe links are owned by the program; program drop = detach).
        // No userspace cleanup needed.
        attach_kprobe(&mut ebpf, SEND_PROG, "tcp_sendmsg")?;
        attach_kprobe(&mut ebpf, RECV_PROG, "tcp_cleanup_rbuf")?;

        tracing::info!(
            "ebpf attributor: kprobes attached (tcp_sendmsg, tcp_cleanup_rbuf)"
        );

        Ok(Self {
            inner: Arc::new(Mutex::new(Inner {
                ebpf,
                last_seen: HashMap::new(),
                idle_streak: HashMap::new(),
            })),
        })
    }
}

/// Recognise the specific aya/kernel error that means "increase RLIMIT_MEMLOCK".
/// On 5.11+ this is rare (memcg accounting); on older kernels or when memcg
/// is disabled, BPF map allocations check the rlimit. The kernel returns
/// EPERM with a message containing "memlock" or just `EAGAIN`/`ENOMEM` when
/// the rlimit is exhausted. We check the error text since aya doesn't
/// expose the underlying errno cleanly.
fn is_memlock_error(err: &aya::EbpfError) -> bool {
    let s = err.to_string().to_ascii_lowercase();
    s.contains("memlock")
        || s.contains("rlimit")
        || s.contains("operation not permitted")
        || s.contains("cannot allocate memory")
}

fn bump_memlock_rlimit() -> Result<()> {
    #[cfg(target_os = "linux")]
    {
        let lim = libc::rlimit {
            rlim_cur: libc::RLIM_INFINITY,
            rlim_max: libc::RLIM_INFINITY,
        };
        let rc = unsafe { libc::setrlimit(libc::RLIMIT_MEMLOCK, &lim) };
        if rc != 0 {
            return Err(NetError::other(format!(
                "failed to raise RLIMIT_MEMLOCK: {}",
                std::io::Error::last_os_error()
            )));
        }
    }
    Ok(())
}

fn attach_kprobe(ebpf: &mut aya::Ebpf, prog_name: &str, kernel_fn: &str) -> Result<()> {
    let prog = ebpf
        .program_mut(prog_name)
        .ok_or_else(|| NetError::other(format!("BPF program `{prog_name}` not found in object")))?;
    let kprobe: &mut aya::programs::KProbe = prog
        .try_into()
        .map_err(|e: aya::programs::ProgramError| NetError::Backend {
            backend: "aya-program",
            source: Box::new(e),
        })?;
    kprobe.load().map_err(|e| NetError::Backend {
        backend: "aya-kprobe-load",
        source: Box::new(e),
    })?;
    kprobe.attach(kernel_fn, 0).map_err(|e| NetError::Backend {
        backend: "aya-kprobe-attach",
        source: Box::new(e),
    })?;
    Ok(())
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
        // Track idle streak so we can evict long-idle pids from the BPF map.
        // The kprobe never deletes entries, so without userspace pruning the
        // map size grows monotonically with every pid that has ever sent
        // TCP traffic — making `iter()` above linearly slower over time.
        if is_zero_delta && g.last_seen.contains_key(pid) {
            let streak = g.idle_streak.entry(*pid).or_insert(0);
            *streak += 1;
            if *streak >= IDLE_EVICTION_THRESHOLD {
                to_evict.push(*pid);
            }
            continue;
        }
        // Active pid this tick — reset the idle streak.
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

    // Prune long-idle pids from the BPF map so iter() stays bounded by
    // the active set. Re-borrow `g.ebpf` separately from the iter borrow
    // above. Errors are non-fatal — if a remove fails (concurrent kprobe
    // re-insert, kernel race), we'll try again next sample.
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

fn read_proc_comm(pid: u32) -> Option<String> {
    let s = std::fs::read_to_string(format!("/proc/{pid}/comm")).ok()?;
    Some(s.trim_end_matches('\n').to_string())
}

fn is_cgroup_v2_unified_mounted() -> bool {
    Path::new(CGROUP2_PATH).join("cgroup.controllers").exists()
}

fn has_kernel_min_version(major: u32, minor: u32) -> bool {
    let release = std::fs::read_to_string("/proc/sys/kernel/osrelease").ok();
    let Some(release) = release else { return false };
    let mut parts = release.split('.');
    let Some(maj) = parts.next().and_then(|s| s.parse::<u32>().ok()) else {
        return false;
    };
    let Some(min) = parts.next().and_then(|s| s.parse::<u32>().ok()) else {
        return false;
    };
    (maj, min) >= (major, minor)
}

fn has_bpf_capability() -> bool {
    if uid_is_zero() {
        return true;
    }
    let Ok(status) = std::fs::read_to_string("/proc/self/status") else {
        return false;
    };
    for line in status.lines() {
        if let Some(rest) = line.strip_prefix("CapEff:") {
            if let Ok(mask) = u64::from_str_radix(rest.trim(), 16) {
                return mask & (1u64 << 39) != 0;
            }
        }
    }
    false
}

fn uid_is_zero() -> bool {
    let Ok(s) = std::fs::read_to_string("/proc/self/status") else {
        return false;
    };
    for line in s.lines() {
        if let Some(rest) = line.strip_prefix("Uid:") {
            return rest.split_ascii_whitespace().next() == Some("0");
        }
    }
    false
}
