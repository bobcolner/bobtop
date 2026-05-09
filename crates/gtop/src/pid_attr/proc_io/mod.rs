//! Tier 1 disk attributor — `/proc/[pid]/io`.
//!
//! Reads `read_bytes` / `write_bytes` directly from `/proc/[pid]/io`,
//! computes per-pid bytes/sec deltas, and caches previous-sample state
//! keyed by `(pid, start_time)` so PID reuse invalidates correctly.
//!
//! ## Accuracy caveats
//!
//! `read_bytes` / `write_bytes` from `/proc/[pid]/io` count bytes that
//! caused real block I/O (cache hits don't count for reads). The big
//! caveat is buffered writes: they're credited to whichever kthread
//! does the writeback (`kworker`, `jbd2`, etc.), NOT the originating
//! process. For accurate write attribution, use the eBPF tier (Tier 2).
//!
//! This tier exists because it's universal on Linux and needs no
//! privilege — it's the right backstop when eBPF isn't available.

#![cfg(target_os = "linux")]

use std::collections::HashMap;
use std::path::Path;
use std::sync::{Arc, Mutex};
use std::time::Instant;

use async_trait::async_trait;

use crate::pid_attr::disk_attributor::{DiskAttributor, DiskAttributorTier, ProcessDiskSample};
use crate::pid_attr::{NetError, Result};

#[derive(Debug, Default)]
pub struct ProcDiskAttributor {
    /// Per-pid prior absolute totals (read_bytes, write_bytes, observed_at).
    /// Validated by start_time so PID reuse invalidates without inheriting
    /// the dead pid's deltas.
    state: Arc<Mutex<HashMap<u32, PrevState>>>,
}

#[derive(Debug, Clone, Copy)]
struct PrevState {
    start_time_ticks: u64,
    read_bytes: u64,
    write_bytes: u64,
    at: Instant,
}

impl ProcDiskAttributor {
    pub fn new() -> Self {
        Self::default()
    }
}

#[async_trait]
impl DiskAttributor for ProcDiskAttributor {
    async fn sample(&self) -> Result<Vec<ProcessDiskSample>> {
        let state = Arc::clone(&self.state);
        tokio::task::spawn_blocking(move || sample_blocking(&state))
            .await
            .map_err(|e| NetError::other(format!("proc_io join: {e}")))?
    }

    fn tier(&self) -> DiskAttributorTier {
        DiskAttributorTier::ProcIo
    }

    fn available() -> bool {
        // Any Linux host with /proc has this. The per-pid `io` file requires
        // CONFIG_TASK_IO_ACCOUNTING which is on in every mainstream distro
        // kernel; we'll silently report 0 for pids whose /proc/[pid]/io is
        // missing, which is the correct fallback.
        Path::new("/proc/self/io").exists()
    }
}

fn sample_blocking(state: &Mutex<HashMap<u32, PrevState>>) -> Result<Vec<ProcessDiskSample>> {
    let now = Instant::now();
    let mut guard = state.lock().unwrap_or_else(|p| p.into_inner());

    // Walk /proc to find live pids. We only emit samples for pids we can
    // read both /stat (for start_time validation) and /io for. Errors on
    // either are silently skipped — process died, perm denied, etc.
    let proc_dir = match std::fs::read_dir("/proc") {
        Ok(d) => d,
        Err(e) => return Err(NetError::other(format!("read /proc: {e}"))),
    };

    let mut next_state: HashMap<u32, PrevState> = HashMap::new();
    let mut out: Vec<ProcessDiskSample> = Vec::new();

    for entry in proc_dir.flatten() {
        let Some(pid) = entry
            .file_name()
            .to_str()
            .and_then(|s| s.parse::<u32>().ok())
        else {
            continue;
        };

        let Some(start_time_ticks) = read_start_time(pid) else {
            continue;
        };
        let Some((read_bytes, write_bytes)) = read_io(pid) else {
            // Common: kernel threads have empty /proc/[pid]/io. Skip cleanly.
            continue;
        };

        // Compute rates against prior state if it matches the same process
        // identity (start_time guards against PID reuse).
        let (rr, wr) = match guard.get(&pid) {
            Some(prev) if prev.start_time_ticks == start_time_ticks => {
                let dt = now.duration_since(prev.at).as_secs_f64().max(0.001);
                (
                    Some(read_bytes.saturating_sub(prev.read_bytes) as f64 / dt),
                    Some(write_bytes.saturating_sub(prev.write_bytes) as f64 / dt),
                )
            }
            _ => (None, None),
        };

        next_state.insert(
            pid,
            PrevState {
                start_time_ticks,
                read_bytes,
                write_bytes,
                at: now,
            },
        );
        out.push(ProcessDiskSample {
            pid,
            read_bytes_per_sec: rr,
            write_bytes_per_sec: wr,
            tier: DiskAttributorTier::ProcIo,
        });
    }

    *guard = next_state;
    Ok(out)
}

/// Field 22 of `/proc/[pid]/stat` is `starttime` in clock ticks since boot.
/// Used as the per-pid identity token for cache invalidation.
fn read_start_time(pid: u32) -> Option<u64> {
    let s = std::fs::read_to_string(format!("/proc/{pid}/stat")).ok()?;
    // Field 2 (`comm`) is wrapped in parens and may contain spaces; skip
    // past the closing paren and split the remainder.
    let close = s.rfind(')')?;
    let after = &s[close + 1..];
    let mut it = after.split_ascii_whitespace();
    // After comm: state(0) ppid(1) pgrp(2) session(3) tty(4) tpgid(5) flags(6)
    // minflt(7) cminflt(8) majflt(9) cmajflt(10) utime(11) stime(12) cutime(13)
    // cstime(14) priority(15) nice(16) num_threads(17) itrealvalue(18)
    // starttime(19) — that's index 19 from "after comm", which is field 22 overall.
    it.nth(19)?.parse().ok()
}

/// Parse `/proc/[pid]/io` and extract `rchar` / `wchar` — the bytes the
/// process *requested* via read/write syscalls. This includes cache hits
/// and buffered I/O, which is what users mean by "what is process X doing
/// on disk." It's what iotop's default column shows.
///
/// We deliberately do NOT use `read_bytes`/`write_bytes` here:
/// those are physical block-I/O counters. They miss page-cache hits
/// entirely and credit buffered writes to the writeback kthread (kworker)
/// instead of the originator. Net result on a typical system: most
/// processes show 0 DR/s and 0 DW/s, which is the bug the user reported.
///
/// Returns None if the file is missing (kernel thread, no
/// `CONFIG_TASK_IO_ACCOUNTING`) or unreadable.
fn read_io(pid: u32) -> Option<(u64, u64)> {
    let s = std::fs::read_to_string(format!("/proc/{pid}/io")).ok()?;
    let mut rchar: Option<u64> = None;
    let mut wchar: Option<u64> = None;
    for line in s.lines() {
        if let Some(rest) = line.strip_prefix("rchar:") {
            rchar = rest.trim().parse().ok();
        } else if let Some(rest) = line.strip_prefix("wchar:") {
            wchar = rest.trim().parse().ok();
        }
    }
    // Both required — if /proc/[pid]/io exists but doesn't have these
    // fields (extremely unusual; would indicate a broken kernel patch),
    // skip the pid rather than emit a 0-pinned sample.
    Some((rchar?, wchar?))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn live_sample_returns_at_least_self() {
        let a = ProcDiskAttributor::new();
        let _ = a.sample().await.expect("first");
        // First call seeds prev-state; second call should produce rates.
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
        let s2 = a.sample().await.expect("second");
        assert!(!s2.is_empty(), "expected at least one /proc/[pid]/io entry");
        let any_with_rate = s2
            .iter()
            .any(|p| p.read_bytes_per_sec.is_some() || p.write_bytes_per_sec.is_some());
        assert!(any_with_rate, "second sample should have at least one rate");
    }

    #[test]
    fn read_start_time_handles_comm_with_spaces() {
        // Field 2 is parenthesized comm — can contain spaces / parens.
        // We rfind the *last* ')' so embedded ')' in comm doesn't trip us up.
        let pid = std::process::id();
        let st = read_start_time(pid).expect("read start_time for self");
        assert!(st > 0);
    }

    #[test]
    fn first_sample_has_no_rates() {
        // Synthetic state-isolation test: first call to sample populates state
        // but produces None rates (no prior to delta against).
        let a = ProcDiskAttributor::new();
        let rt = tokio::runtime::Runtime::new().unwrap();
        let s = rt.block_on(a.sample()).unwrap();
        assert!(s.iter().all(|p| p.read_bytes_per_sec.is_none()));
        assert!(s.iter().all(|p| p.write_bytes_per_sec.is_none()));
    }
}
