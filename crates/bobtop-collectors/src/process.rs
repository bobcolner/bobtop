//! Process collector — sysinfo-backed listing for cross-platform parity.
//!
//! sysinfo gives us pid / name / cmdline / cpu / RSS / VSZ / parent / status
//! on every supported platform. Per-process network attribution is *not*
//! handled here — that comes from `bobtop-net` and is joined in the daemon.

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

use async_trait::async_trait;
use bobtop_core::sample::{ProcessInfo, ProcessSample, ProcessState};
use bobtop_core::{Collector, Result};
use sysinfo::{ProcessRefreshKind, ProcessStatus, ProcessesToUpdate, System, Users};

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
    last_disk: Mutex<std::collections::HashMap<u32, DiskRateState>>,
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
            last_disk: Mutex::new(std::collections::HashMap::new()),
        }
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
        // `refresh_processes` uses the *default* ProcessRefreshKind, which
        // omits `user`. Without an explicit `with_user`, `p.user_id()`
        // returns None for every process and the User column renders blank.
        // Refresh everything — the per-tick cost is dominated by the syscalls
        // we'd already pay for cpu/mem anyway.
        sys.refresh_processes_specifics(
            ProcessesToUpdate::All,
            true,
            ProcessRefreshKind::everything(),
        );

        let cpu_count = self.cpu_count.max(1);
        let now = Instant::now();
        let mut last_disk = self
            .last_disk
            .lock()
            .unwrap_or_else(|p| p.into_inner());
        let users = self
            .users
            .lock()
            .unwrap_or_else(|p| p.into_inner());

        let processes: Vec<_> = sys
            .processes()
            .iter()
            .map(|(pid, p)| {
                let pid_u32 = pid.as_u32();
                // sysinfo's `cmd()` often returns just argv[0] (the
                // executable name) — same string the Program column
                // already shows. Read /proc/[pid]/cmdline directly so
                // the full nul-separated argv comes through; users
                // expect the Command column to show actual flags/paths.
                let cmdline = read_full_cmdline(pid_u32).unwrap_or_else(|| {
                    p.cmd()
                        .iter()
                        .map(|s| s.to_string_lossy())
                        .collect::<Vec<_>>()
                        .join(" ")
                });
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
                ProcessInfo {
                    pid: pid_u32,
                    parent_pid: p.parent().map(|pp| pp.as_u32()),
                    name: p.name().to_string_lossy().into_owned(),
                    cmdline,
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
                    net_rx_bytes_per_sec: None,
                    net_tx_bytes_per_sec: None,
                    disk_read_bytes_per_sec: dr,
                    disk_write_bytes_per_sec: dw,
                    cgroup: read_cgroup(pid_u32),
                }
            })
            .collect();

        // Rebuild the per-pid disk-rate state from this sample's totals.
        last_disk.clear();
        for (pid, p) in sys.processes().iter() {
            let usage = p.disk_usage();
            last_disk.insert(
                pid.as_u32(),
                DiskRateState {
                    last_total_read: usage.total_read_bytes,
                    last_total_written: usage.total_written_bytes,
                    last_at: now,
                },
            );
        }

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
#[cfg(target_os = "linux")]
fn read_cgroup(pid: u32) -> Option<String> {
    let s = std::fs::read_to_string(format!("/proc/{pid}/cgroup")).ok()?;
    // Prefer v2 (line starts with "0::"). Fall back to longest v1 path.
    let v2 = s.lines().find(|l| l.starts_with("0::")).map(|l| &l[3..]);
    let path = v2.or_else(|| {
        s.lines()
            .filter_map(|l| l.splitn(3, ':').nth(2))
            .max_by_key(|p| p.len())
    })?;
    let leaf = path.rsplit('/').find(|s| !s.is_empty())?;
    if leaf.is_empty() || leaf == "/" {
        None
    } else {
        Some(leaf.to_string())
    }
}

#[cfg(not(target_os = "linux"))]
fn read_cgroup(_pid: u32) -> Option<String> {
    None
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
}
