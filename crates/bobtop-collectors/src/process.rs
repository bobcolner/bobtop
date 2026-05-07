//! Process collector — sysinfo-backed listing for cross-platform parity.
//!
//! sysinfo gives us pid / name / cmdline / cpu / RSS / VSZ / parent / status
//! on every supported platform. Per-process network attribution is *not*
//! handled here — that comes from `bobtop-net` and is joined in the daemon.

use std::collections::HashMap;
use std::sync::Mutex;
use std::time::{Duration, Instant};

/// Per-pid disk-IO rate state. We compute bytes/sec from the wall-clock
/// delta between this and the previous refresh; sysinfo's `disk_usage()`
/// fields are absolute totals.
#[derive(Debug, Clone, Copy)]
struct DiskRateState {
    last_total_read: u64,
    last_total_written: u64,
    last_at: Instant,
}

/// Cached /proc reads that almost never change after a process starts.
/// Keyed by pid, validated by `start_time` — if a pid is reused (start_time
/// differs), we re-read instead of inheriting the dead process's strings.
#[derive(Debug, Clone)]
struct StaticProcInfo {
    start_time: u64,
    cmdline: Option<String>,
    cgroup: Option<String>,
    /// Parsed container metadata when the cgroup path matched a known
    /// runtime (Docker, Podman, containerd, LXC). Resolved once per pid
    /// (cgroups don't change after exec) and cached here.
    container: Option<Container>,
}

use async_trait::async_trait;
use bobtop_core::sample::{Container, ProcessInfo, ProcessSample, ProcessState};
use bobtop_core::{Collector, Result};
use bobtop_pid_attr::AttributionStore;
use sysinfo::{ProcessRefreshKind, ProcessStatus, ProcessesToUpdate, System, UpdateKind, Users};

use crate::container::NameResolver;

const DEFAULT_INTERVAL_MS: u64 = 2000;

pub struct ProcessCollector {
    interval: Duration,
    sys: Mutex<System>,
    /// User registry for resolving UID → username. Refreshed on each
    /// collect — cheap (passwd entries are stable, sysinfo just reads
    /// /etc/passwd / NSS) and ensures newly-created users (rare) appear.
    users: Mutex<Users>,
    cpu_count: usize,
    /// Previous absolute disk totals per pid for rate computation.
    last_disk: Mutex<HashMap<u32, DiskRateState>>,
    /// Cache of /proc/[pid]/cmdline and /proc/[pid]/cgroup, validated by
    /// the process's `start_time` so PID reuse invalidates automatically.
    /// Avoids ~2 syscalls × N processes per tick — typically the heaviest
    /// per-tick cost after sysinfo's own /proc walk.
    static_cache: Mutex<HashMap<u32, StaticProcInfo>>,
    /// Optional per-pid net/disk attribution. When present, `collect()`
    /// joins the latest snapshot into each `ProcessInfo` before returning,
    /// so `ProcessSample` published on the bus carries authoritative
    /// rates instead of `None`. Lives outside the collector lifecycle —
    /// callers create one `AttributionStore`, hand it to the collector
    /// here and to the attributor sampling loops as the writer side.
    attribution: Option<AttributionStore>,
    /// Container name cache. Resolves friendly names from runtime
    /// metadata files (Docker / Podman) the first time we see each
    /// container id and reuses the result for the lifetime of the
    /// collector.
    name_resolver: Mutex<NameResolver>,
}

impl std::fmt::Debug for ProcessCollector {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ProcessCollector")
            .field("interval", &self.interval)
            .field("cpu_count", &self.cpu_count)
            .finish()
    }
}

impl ProcessCollector {
    pub fn new() -> Self {
        Self::with_interval(Duration::from_millis(DEFAULT_INTERVAL_MS))
    }

    pub fn with_interval(interval: Duration) -> Self {
        let mut sys = System::new();
        sys.refresh_cpu_all();
        let cpu_count = sys.cpus().len().max(1);
        Self {
            interval,
            sys: Mutex::new(sys),
            users: Mutex::new(Users::new_with_refreshed_list()),
            cpu_count,
            last_disk: Mutex::new(HashMap::new()),
            static_cache: Mutex::new(HashMap::new()),
            attribution: None,
            name_resolver: Mutex::new(NameResolver::new()),
        }
    }

    /// Attach an `AttributionStore` so `collect()` joins per-pid net/disk
    /// rates into each `ProcessInfo` before returning. Without this, the
    /// `net_*` fields stay `None` and `disk_*` reflects sysinfo's
    /// per-pid totals only.
    pub fn with_attribution(mut self, store: AttributionStore) -> Self {
        self.attribution = Some(store);
        self
    }
}

impl Default for ProcessCollector {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl Collector for ProcessCollector {
    type Sample = ProcessSample;

    async fn collect(&self) -> Result<ProcessSample> {
        // sysinfo refresh is synchronous; do it inside the lock and convert.
        let mut sys = self
            .sys
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        // Scope the refresh to fields we actually render. `everything()`
        // additionally pulls cwd/root/environ/exe — none of which we read,
        // each costing a /proc syscall per pid per tick. `OnlyIfNotSet` for
        // user/cmd means sysinfo populates them on first sight and reuses
        // the cached value afterward (uid/argv don't change post-exec).
        sys.refresh_processes_specifics(
            ProcessesToUpdate::All,
            true,
            ProcessRefreshKind::new()
                .with_cpu()
                .with_memory()
                .with_disk_usage()
                .with_user(UpdateKind::OnlyIfNotSet)
                .with_cmd(UpdateKind::OnlyIfNotSet),
        );

        let cpu_count = self.cpu_count.max(1);
        let now = Instant::now();
        let mut last_disk = self
            .last_disk
            .lock()
            .unwrap_or_else(|p| p.into_inner());
        let mut static_cache = self
            .static_cache
            .lock()
            .unwrap_or_else(|p| p.into_inner());
        let users = self
            .users
            .lock()
            .unwrap_or_else(|p| p.into_inner());

        let proc_iter = sys.processes();
        // Build next-tick state in lockstep with the output rows so we walk
        // sys.processes() exactly once. Old (pid, _) entries that aren't seen
        // this tick fall out naturally because we replace the map below.
        let mut next_disk: HashMap<u32, DiskRateState> = HashMap::with_capacity(proc_iter.len());
        let mut next_static: HashMap<u32, StaticProcInfo> =
            HashMap::with_capacity(proc_iter.len());
        let mut processes: Vec<ProcessInfo> = Vec::with_capacity(proc_iter.len());

        // Snapshot the attribution maps up-front so we hold the lock for a
        // single short critical section instead of acquiring it per pid.
        // Cloning two `HashMap<u32, _>` is cheap (~16 B per entry).
        let (net_map, disk_map, net_has_bw) = match self.attribution.as_ref() {
            Some(store) => store.read(|s| {
                (
                    s.net.clone(),
                    s.disk.clone(),
                    s.net_tier.has_bandwidth(),
                )
            }),
            None => (Default::default(), Default::default(), false),
        };

        for (pid, p) in proc_iter.iter() {
            let pid_u32 = pid.as_u32();
            let start_time = p.start_time();

            // Cache cmdline + cgroup keyed by (pid, start_time). Both come
            // from /proc files that the kernel only updates at exec/cgroup
            // change, so re-reading them every tick is pure waste. PID reuse
            // bumps start_time, invalidating the entry. We `remove` (rather
            // than `get().cloned()`) to transfer ownership of the cached
            // strings into the next-tick map without an extra clone.
            let static_info = match static_cache.remove(&pid_u32) {
                Some(c) if c.start_time == start_time => c,
                _ => {
                    // sysinfo's `cmd()` often returns just argv[0] (the
                    // executable name) — same string the Program column
                    // already shows. Read /proc/[pid]/cmdline directly so
                    // the full nul-separated argv comes through.
                    let cmdline = read_full_cmdline(pid_u32).or_else(|| {
                        let joined = p
                            .cmd()
                            .iter()
                            .map(|s| s.to_string_lossy())
                            .collect::<Vec<_>>()
                            .join(" ");
                        if joined.is_empty() {
                            None
                        } else {
                            Some(joined)
                        }
                    });
                    let (cgroup_leaf, cgroup_full) = read_cgroup_full(pid_u32);
                    let container = {
                        let mut r = self.name_resolver.lock().unwrap();
                        crate::container::detect(
                            cgroup_leaf.as_deref(),
                            cgroup_full.as_deref(),
                            &mut r,
                        )
                    };
                    StaticProcInfo {
                        start_time,
                        cmdline,
                        cgroup: cgroup_leaf,
                        container,
                    }
                }
            };

            let usage = p.disk_usage();
            // Compute rate as Δ(absolute total) / Δt against the per-pid
            // previous state. Falls back to None on the first sample.
            let (dr, dw) = match last_disk.get(&pid_u32) {
                Some(prev) => {
                    let dt = now.duration_since(prev.last_at).as_secs_f64().max(0.001);
                    let r = usage
                        .total_read_bytes
                        .saturating_sub(prev.last_total_read) as f64
                        / dt;
                    let w = usage
                        .total_written_bytes
                        .saturating_sub(prev.last_total_written)
                        as f64
                        / dt;
                    (Some(r), Some(w))
                }
                None => (None, None),
            };

            next_disk.insert(
                pid_u32,
                DiskRateState {
                    last_total_read: usage.total_read_bytes,
                    last_total_written: usage.total_written_bytes,
                    last_at: now,
                },
            );

            // Net join. Tier 1 backends (proc_inode / proc_pidinfo) can
            // enumerate connections but not bytes — `rx`/`tx` stay `None`
            // for those. When the active tier *does* report bandwidth,
            // pids missing from the snapshot get `Some(0.0)` rather than
            // `None` so the TUI's column visibility logic stays consistent
            // with what `apply_net` did historically.
            let (net_rx, net_tx) = match net_map.get(&pid_u32) {
                Some(a) => (a.rx_bytes_per_sec, a.tx_bytes_per_sec),
                None if net_has_bw => (Some(0.0), Some(0.0)),
                None => (None, None),
            };
            // Disk join. The attributor's `Some` overrides the sysinfo
            // value; `None` (warmup / pid missing from the snapshot)
            // lets the sysinfo-derived rate stand. Matches the prior
            // `App::rebuild_sorted` semantics.
            let (disk_r, disk_w) = match disk_map.get(&pid_u32) {
                Some(a) => (a.read_bytes_per_sec.or(dr), a.write_bytes_per_sec.or(dw)),
                None => (dr, dw),
            };

            processes.push(ProcessInfo {
                pid: pid_u32,
                parent_pid: p.parent().map(|pp| pp.as_u32()),
                name: p.name().to_string_lossy().into_owned(),
                cmdline: static_info.cmdline.clone().unwrap_or_default(),
                user: p
                    .user_id()
                    .and_then(|uid| {
                        users
                            .get_user_by_id(uid)
                            .map(|u| u.name().to_string())
                            // If the UID isn't in the user registry
                            // (rare — container with stale passwd),
                            // fall back to the numeric UID rather
                            // than empty string.
                            .or_else(|| Some(uid.to_string()))
                    })
                    .unwrap_or_default(),
                state: map_status(p.status()),
                cpu_fraction: p.cpu_usage() / 100.0,
                mem_rss_bytes: p.memory(),
                mem_vsz_bytes: p.virtual_memory(),
                threads: thread_count(p),
                net_rx_bytes_per_sec: net_rx,
                net_tx_bytes_per_sec: net_tx,
                disk_read_bytes_per_sec: disk_r,
                disk_write_bytes_per_sec: disk_w,
                cgroup: static_info.cgroup.clone(),
                container: static_info.container.clone(),
            });

            next_static.insert(pid_u32, static_info);
        }

        *last_disk = next_disk;
        *static_cache = next_static;

        let _ = cpu_count; // kept for future per-core breakdowns

        Ok(ProcessSample {
            timestamp: Instant::now(),
            processes,
        })
    }

    fn interval(&self) -> Duration {
        self.interval
    }

    fn name(&self) -> &'static str {
        "process"
    }
}

fn map_status(s: ProcessStatus) -> ProcessState {
    match s {
        ProcessStatus::Run => ProcessState::Running,
        ProcessStatus::Sleep => ProcessState::Sleeping,
        ProcessStatus::UninterruptibleDiskSleep => ProcessState::DiskSleep,
        ProcessStatus::Stop => ProcessState::Stopped,
        ProcessStatus::Tracing => ProcessState::TracingStop,
        ProcessStatus::Zombie => ProcessState::Zombie,
        ProcessStatus::Idle => ProcessState::Idle,
        ProcessStatus::Dead => ProcessState::Dead,
        other => {
            // Stringify the variant name and take the first char for diag.
            let s = format!("{other:?}");
            let c = s.chars().next().unwrap_or('?');
            ProcessState::Other(c)
        }
    }
}

#[cfg(target_os = "linux")]
fn thread_count(p: &sysinfo::Process) -> u32 {
    p.tasks().map(|t| t.len() as u32).unwrap_or(1)
}

#[cfg(not(target_os = "linux"))]
fn thread_count(_p: &sysinfo::Process) -> u32 {
    1
}

/// Read /proc/[pid]/cgroup and return the leaf segment of the cgroup v2
/// path. Modern systemd writes a single line of the form
/// `0::/user.slice/user-1000.slice/session-3.scope` — we strip everything
/// before the last `/` so the display is readable. Falls back to looking
/// at the v1 hierarchy line that has the longest path (heuristic).
/// Returns None on non-Linux, when the file is unreadable, or when the
/// cgroup is just `/` (the root, useless for grouping).
/// Read `/proc/[pid]/cgroup` and return both the leaf segment (for
/// display and the existing cgroup-grouping mode) and the full v2 path
/// (for container detection — Docker on the cgroupfs driver and LXC
/// put the id/name earlier in the path, not in the leaf).
#[cfg(target_os = "linux")]
fn read_cgroup_full(pid: u32) -> (Option<String>, Option<String>) {
    let Ok(s) = std::fs::read_to_string(format!("/proc/{pid}/cgroup")) else {
        return (None, None);
    };
    // Prefer v2 (line starts with "0::"). Fall back to longest v1 path.
    let v2 = s.lines().find(|l| l.starts_with("0::")).map(|l| &l[3..]);
    let path = match v2 {
        Some(p) => p,
        None => match s
            .lines()
            .filter_map(|l| l.splitn(3, ':').nth(2))
            .max_by_key(|p| p.len())
        {
            Some(p) => p,
            None => return (None, None),
        },
    };
    let full = if path.is_empty() || path == "/" {
        None
    } else {
        Some(path.to_string())
    };
    let leaf = path
        .rsplit('/')
        .find(|s| !s.is_empty())
        .filter(|l| !l.is_empty() && *l != "/")
        .map(|s| s.to_string());
    (leaf, full)
}

#[cfg(not(target_os = "linux"))]
fn read_cgroup_full(_pid: u32) -> (Option<String>, Option<String>) {
    (None, None)
}

/// Read /proc/[pid]/cmdline and join the NUL-separated argv vector
/// with single spaces. Returns None on non-Linux or when the file
/// can't be read (kernel threads, perms). When None, the caller
/// falls back to sysinfo's truncated `cmd()`.
#[cfg(target_os = "linux")]
fn read_full_cmdline(pid: u32) -> Option<String> {
    let bytes = std::fs::read(format!("/proc/{pid}/cmdline")).ok()?;
    if bytes.is_empty() {
        return None;
    }
    let s = bytes
        .split(|b| *b == 0)
        .filter(|s| !s.is_empty())
        .map(|s| String::from_utf8_lossy(s).into_owned())
        .collect::<Vec<_>>()
        .join(" ");
    if s.is_empty() {
        None
    } else {
        Some(s)
    }
}

#[cfg(not(target_os = "linux"))]
fn read_full_cmdline(_pid: u32) -> Option<String> {
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn live_collect_returns_some_processes() {
        let c = ProcessCollector::new();
        let s = c.collect().await.expect("collect");
        // We're definitely running at least the test runner — should see one.
        assert!(!s.processes.is_empty(), "expected at least one process");
        // Spot-check fields are populated.
        let any_named = s.processes.iter().any(|p| !p.name.is_empty());
        assert!(any_named);
    }

    #[tokio::test]
    async fn second_collect_reuses_static_cache_for_live_processes() {
        // Two back-to-back collects should leave each live pid in
        // static_cache exactly once, with the same start_time. This
        // doesn't directly assert syscall count but verifies the cache
        // is keyed/replaced correctly so PID reuse will invalidate.
        let c = ProcessCollector::new();
        let _ = c.collect().await.expect("first");
        let snap1: HashMap<u32, u64> = c
            .static_cache
            .lock()
            .unwrap()
            .iter()
            .map(|(p, e)| (*p, e.start_time))
            .collect();
        let _ = c.collect().await.expect("second");
        let snap2: HashMap<u32, u64> = c
            .static_cache
            .lock()
            .unwrap()
            .iter()
            .map(|(p, e)| (*p, e.start_time))
            .collect();
        // Any pid present in both snapshots must have the same start_time.
        for (pid, st1) in &snap1 {
            if let Some(st2) = snap2.get(pid) {
                assert_eq!(st1, st2, "start_time changed for live pid {pid}");
            }
        }
    }
}
