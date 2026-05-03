//! Process collector — sysinfo-backed listing for cross-platform parity.
//!
//! sysinfo gives us pid / name / cmdline / cpu / RSS / VSZ / parent / status
//! on every supported platform. Per-process network attribution is *not*
//! handled here — that comes from `bobtop-net` and is joined in the daemon.

use std::sync::Mutex;
use std::time::{Duration, Instant};

use async_trait::async_trait;
use bobtop_core::sample::{ProcessInfo, ProcessSample, ProcessState};
use bobtop_core::{Collector, Result};
use sysinfo::{ProcessStatus, ProcessesToUpdate, System};

const DEFAULT_INTERVAL_MS: u64 = 2000;

pub struct ProcessCollector {
    interval: Duration,
    sys: Mutex<System>,
    cpu_count: usize,
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
            cpu_count,
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
        sys.refresh_processes(ProcessesToUpdate::All, true);

        let cpu_count = self.cpu_count.max(1);
        let processes = sys
            .processes()
            .iter()
            .map(|(pid, p)| {
                let cmdline = p
                    .cmd()
                    .iter()
                    .map(|s| s.to_string_lossy())
                    .collect::<Vec<_>>()
                    .join(" ");
                ProcessInfo {
                    pid: pid.as_u32(),
                    parent_pid: p.parent().map(|pp| pp.as_u32()),
                    name: p.name().to_string_lossy().into_owned(),
                    cmdline,
                    user: p
                        .user_id()
                        .map(|u| u.to_string())
                        .unwrap_or_default(),
                    state: map_status(p.status()),
                    // sysinfo's cpu_usage is 0..(100*N) — normalise to a
                    // fraction of *one* core so values stay readable
                    // (1.0 = one fully-loaded core).
                    cpu_fraction: p.cpu_usage() / 100.0,
                    mem_rss_bytes: p.memory(),
                    mem_vsz_bytes: p.virtual_memory(),
                    threads: thread_count(p),
                    net_rx_bytes_per_sec: None,
                    net_tx_bytes_per_sec: None,
                }
            })
            .collect();

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
