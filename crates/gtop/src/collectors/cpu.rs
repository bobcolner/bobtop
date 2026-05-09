//! CPU utilization collector.
//!
//! - **Linux:** parses `/proc/stat` for per-core jiffy counters, computes
//!   busy/total deltas vs. the previous snapshot, reads `/proc/loadavg` for
//!   load average, and best-effort reads `cpufreq/scaling_cur_freq` per core.
//! - **macOS:** placeholder — returns [`crate::core::CoreError::Unsupported`].
//!   Real impl will use `host_processor_info` from the mach kernel API.

use std::sync::Mutex;
use std::time::{Duration, Instant};

use async_trait::async_trait;
use crate::core::sample::{CoreSample, CpuSample, LoadAverage};
use crate::core::{Collector, CoreError, Result};

const DEFAULT_INTERVAL_MS: u64 = 1000;

/// Periodic CPU sampler.
///
/// Stateful: holds the previous jiffy snapshot so each `collect()` can
/// compute a utilization delta. The first call after construction returns a
/// zero-utilization sample (we have no baseline to compare against yet) and
/// stores the current snapshot for use on the next call.
#[derive(Debug)]
pub struct CpuCollector {
    interval: Duration,
    state: Mutex<Option<Snapshot>>,
}

#[derive(Debug, Clone)]
struct Snapshot {
    aggregate: CpuTimes,
    cores: Vec<CpuTimes>,
}

#[derive(Debug, Clone, Copy, Default)]
struct CpuTimes {
    user: u64,
    nice: u64,
    system: u64,
    idle: u64,
    iowait: u64,
    irq: u64,
    softirq: u64,
    steal: u64,
}

impl CpuTimes {
    fn total(&self) -> u64 {
        self.user
            + self.nice
            + self.system
            + self.idle
            + self.iowait
            + self.irq
            + self.softirq
            + self.steal
    }

    fn idle_total(&self) -> u64 {
        self.idle + self.iowait
    }
}

impl CpuCollector {
    pub fn new() -> Self {
        Self::with_interval(Duration::from_millis(DEFAULT_INTERVAL_MS))
    }

    pub fn with_interval(interval: Duration) -> Self {
        Self {
            interval,
            state: Mutex::new(None),
        }
    }
}

impl Default for CpuCollector {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl Collector for CpuCollector {
    type Sample = CpuSample;

    async fn collect(&self) -> Result<CpuSample> {
        let snapshot = read_snapshot()?;
        let load = read_loadavg().ok();
        let frequencies = read_core_frequencies(snapshot.cores.len());

        // Lock briefly to swap previous-state. On poison (another thread
        // panicked while holding it) take the inner value and continue —
        // utilization math is stateless modulo the previous snapshot.
        let prev = match self.state.lock() {
            Ok(mut g) => g.replace(snapshot.clone()),
            Err(poisoned) => {
                let mut g = poisoned.into_inner();
                g.replace(snapshot.clone())
            }
        };

        let cores = build_core_samples(&snapshot, prev.as_ref(), &frequencies);
        let aggregate_utilization = match prev.as_ref() {
            Some(p) => utilization_fraction(p.aggregate, snapshot.aggregate),
            None => 0.0,
        };

        Ok(CpuSample {
            timestamp: Instant::now(),
            aggregate_utilization,
            cores,
            load_average: load,
        })
    }

    fn interval(&self) -> Duration {
        self.interval
    }

    fn name(&self) -> &'static str {
        "cpu"
    }
}

fn build_core_samples(
    cur: &Snapshot,
    prev: Option<&Snapshot>,
    frequencies: &[Option<u32>],
) -> Vec<CoreSample> {
    cur.cores
        .iter()
        .enumerate()
        .map(|(i, &times)| {
            // Match by index up to min(prev, cur). The previous strict
            // equality test made every core's fraction 0.0 for one tick
            // after CPU hotplug or a kexec where the core count changes.
            let utilization = prev
                .and_then(|p| p.cores.get(i))
                .map(|prev_times| utilization_fraction(*prev_times, times))
                .unwrap_or(0.0);
            CoreSample {
                id: i as u32,
                utilization,
                frequency_mhz: frequencies.get(i).copied().flatten(),
                temperature_c: None,
            }
        })
        .collect()
}

fn utilization_fraction(prev: CpuTimes, cur: CpuTimes) -> f32 {
    let total = cur.total().saturating_sub(prev.total());
    let idle = cur.idle_total().saturating_sub(prev.idle_total());
    if total == 0 {
        return 0.0;
    }
    let busy = total.saturating_sub(idle);
    (busy as f64 / total as f64).clamp(0.0, 1.0) as f32
}

// -------------------------------------------------------------------------
// Linux readers
// -------------------------------------------------------------------------

#[cfg(target_os = "linux")]
fn read_snapshot() -> Result<Snapshot> {
    let text = std::fs::read_to_string("/proc/stat")?;
    parse_proc_stat(&text)
        .ok_or_else(|| CoreError::parse("/proc/stat: missing aggregate `cpu` line"))
}

#[cfg(target_os = "linux")]
fn read_loadavg() -> Result<LoadAverage> {
    let text = std::fs::read_to_string("/proc/loadavg")?;
    parse_loadavg(&text).ok_or_else(|| CoreError::parse("/proc/loadavg malformed"))
}

#[cfg(target_os = "linux")]
fn read_core_frequencies(n: usize) -> Vec<Option<u32>> {
    let mut out: Vec<Option<u32>> = (0..n)
        .map(|i| {
            let path = format!("/sys/devices/system/cpu/cpu{i}/cpufreq/scaling_cur_freq");
            std::fs::read_to_string(path).ok().and_then(|s| {
                // File holds frequency in kHz.
                s.trim().parse::<u64>().ok().map(|khz| (khz / 1000) as u32)
            })
        })
        .collect();
    let fallback = read_proc_cpuinfo_frequencies(n);
    for (dst, src) in out.iter_mut().zip(fallback.into_iter()) {
        if dst.is_none() {
            *dst = src;
        }
    }
    out
}

#[cfg(target_os = "linux")]
fn read_proc_cpuinfo_frequencies(n: usize) -> Vec<Option<u32>> {
    let text = match std::fs::read_to_string("/proc/cpuinfo") {
        Ok(t) => t,
        Err(_) => return vec![None; n],
    };
    parse_proc_cpuinfo_frequencies(&text, n)
}

#[cfg(target_os = "linux")]
fn parse_proc_cpuinfo_frequencies(text: &str, n: usize) -> Vec<Option<u32>> {
    let mut out = vec![None; n];
    let mut current_idx: Option<usize> = None;
    for line in text.lines() {
        let mut parts = line.splitn(2, ':');
        let key = parts.next().map(str::trim).unwrap_or("");
        let value = parts.next().map(str::trim).unwrap_or("");
        match key {
            "processor" => {
                current_idx = value.parse::<usize>().ok().filter(|&i| i < n);
            }
            "cpu MHz" => {
                if let Some(i) = current_idx {
                    let mhz = value.parse::<f64>().ok().map(|v| v.round() as u32);
                    out[i] = mhz;
                }
            }
            _ => {}
        }
    }
    out
}

#[cfg(target_os = "linux")]
fn parse_proc_stat(text: &str) -> Option<Snapshot> {
    let mut aggregate: Option<CpuTimes> = None;
    let mut cores: Vec<CpuTimes> = Vec::new();
    for line in text.lines() {
        let mut it = line.split_ascii_whitespace();
        let Some(label) = it.next() else { continue };
        if !label.starts_with("cpu") {
            // /proc/stat continues with `intr`, `ctxt`, etc. — done with cpu rows.
            if aggregate.is_some() {
                break;
            }
            continue;
        }
        // A malformed per-core line (rare — kernel format change in field
        // count) used to abort the whole parse and freeze CPU forever.
        // The aggregate `cpu` line is mandatory; per-core lines are skipped
        // on parse failure so the aggregate still populates.
        let Some(times) = parse_cpu_times(it) else {
            if label == "cpu" {
                return None;
            }
            continue;
        };
        if label == "cpu" {
            aggregate = Some(times);
        } else {
            // label is "cpuN" — push into `cores` ordered by appearance.
            cores.push(times);
        }
    }
    Some(Snapshot {
        aggregate: aggregate?,
        cores,
    })
}

#[cfg(target_os = "linux")]
fn parse_cpu_times<'a, I: Iterator<Item = &'a str>>(mut fields: I) -> Option<CpuTimes> {
    // Fields after the label: user nice system idle iowait irq softirq steal [guest guest_nice]
    let user: u64 = fields.next()?.parse().ok()?;
    let nice: u64 = fields.next()?.parse().ok()?;
    let system: u64 = fields.next()?.parse().ok()?;
    let idle: u64 = fields.next()?.parse().ok()?;
    let iowait: u64 = fields.next().and_then(|s| s.parse().ok()).unwrap_or(0);
    let irq: u64 = fields.next().and_then(|s| s.parse().ok()).unwrap_or(0);
    let softirq: u64 = fields.next().and_then(|s| s.parse().ok()).unwrap_or(0);
    let steal: u64 = fields.next().and_then(|s| s.parse().ok()).unwrap_or(0);
    Some(CpuTimes {
        user,
        nice,
        system,
        idle,
        iowait,
        irq,
        softirq,
        steal,
    })
}

#[cfg(target_os = "linux")]
fn parse_loadavg(text: &str) -> Option<LoadAverage> {
    let mut it = text.split_ascii_whitespace();
    let one: f32 = it.next()?.parse().ok()?;
    let five: f32 = it.next()?.parse().ok()?;
    let fifteen: f32 = it.next()?.parse().ok()?;
    Some(LoadAverage {
        one,
        five,
        fifteen,
    })
}

// -------------------------------------------------------------------------
// macOS placeholder
// -------------------------------------------------------------------------

#[cfg(not(target_os = "linux"))]
fn read_snapshot() -> Result<Snapshot> {
    Err(CoreError::Unsupported)
}

#[cfg(not(target_os = "linux"))]
fn read_loadavg() -> Result<LoadAverage> {
    Err(CoreError::Unsupported)
}

#[cfg(not(target_os = "linux"))]
fn read_core_frequencies(_n: usize) -> Vec<Option<u32>> {
    Vec::new()
}

// -------------------------------------------------------------------------
// Tests
// -------------------------------------------------------------------------

#[cfg(all(test, target_os = "linux"))]
mod tests {
    use super::*;

    const SAMPLE: &str = "cpu  100 5 50 800 10 0 5 0 0 0\ncpu0 50 2 25 400 5 0 2 0 0 0\ncpu1 50 3 25 400 5 0 3 0 0 0\nintr 1234\nctxt 5678\n";

    #[test]
    fn parses_aggregate_and_per_core() {
        let snap = parse_proc_stat(SAMPLE).expect("parse");
        assert_eq!(snap.cores.len(), 2);
        assert_eq!(snap.aggregate.user, 100);
        assert_eq!(snap.aggregate.idle, 800);
        assert_eq!(snap.cores[1].system, 25);
    }

    #[test]
    fn malformed_per_core_line_is_skipped_not_fatal() {
        // cpu1 has only 3 fields — too few for parse_cpu_times. Should not
        // abort the snapshot; aggregate + cpu0 must still come through so
        // CPU keeps reporting on systems with quirky kernel-format changes.
        let sample = "cpu  100 5 50 800 10 0 5 0\ncpu0 50 2 25 400 5 0 2 0\ncpu1 50 3 25\nintr 1234\n";
        let snap = parse_proc_stat(sample).expect("parse");
        assert_eq!(snap.aggregate.user, 100);
        assert_eq!(snap.cores.len(), 1, "malformed cpu1 must be skipped");
        assert_eq!(snap.cores[0].user, 50);
    }

    #[test]
    fn malformed_aggregate_line_returns_none() {
        let sample = "cpu  100 5\ncpu0 50 2 25 400 5 0 2 0\n";
        assert!(parse_proc_stat(sample).is_none());
    }

    #[test]
    fn build_core_samples_handles_core_count_change() {
        // Previous tick had 4 cores; this tick has 2 (CPU hotplug down).
        // Old code returned utilization=0 for every core in this case.
        // New code matches by index up to min(prev, cur).
        let prev = Snapshot {
            aggregate: CpuTimes::default(),
            cores: vec![CpuTimes::default(); 4],
        };
        let cur = Snapshot {
            aggregate: CpuTimes {
                user: 30,
                idle: 70,
                ..Default::default()
            },
            cores: vec![
                CpuTimes { user: 30, idle: 70, ..Default::default() },
                CpuTimes { user: 30, idle: 70, ..Default::default() },
            ],
        };
        let samples = build_core_samples(&cur, Some(&prev), &[]);
        assert_eq!(samples.len(), 2);
        for s in &samples {
            assert!((s.utilization - 0.30).abs() < 1e-5, "got {}", s.utilization);
        }
    }

    #[test]
    fn utilization_zero_when_no_movement() {
        let t = CpuTimes {
            user: 100,
            idle: 800,
            ..Default::default()
        };
        assert_eq!(utilization_fraction(t, t), 0.0);
    }

    #[test]
    fn utilization_correct_for_known_delta() {
        let prev = CpuTimes {
            user: 0,
            idle: 0,
            ..Default::default()
        };
        let cur = CpuTimes {
            user: 30,
            idle: 70,
            ..Default::default()
        };
        // 30 busy out of 100 total = 0.30
        let u = utilization_fraction(prev, cur);
        assert!((u - 0.30).abs() < 1e-5, "got {u}");
    }

    #[test]
    fn loadavg_roundtrip() {
        let l = parse_loadavg("0.42 0.48 0.55 1/512 12345\n").unwrap();
        assert!((l.one - 0.42).abs() < 1e-3);
        assert!((l.fifteen - 0.55).abs() < 1e-3);
    }

    #[test]
    fn parses_proc_cpuinfo_frequencies_by_processor_index() {
        let sample = "\
processor   : 0
cpu MHz     : 2793.454

processor   : 1
cpu MHz     : 3199.998
";
        let freqs = parse_proc_cpuinfo_frequencies(sample, 2);
        assert_eq!(freqs, vec![Some(2793), Some(3200)]);
    }

    #[tokio::test]
    async fn first_collect_returns_zero_then_subsequent_returns_real() {
        let c = CpuCollector::new();
        let first = c.collect().await.expect("first");
        assert_eq!(first.aggregate_utilization, 0.0);
        assert!(!first.cores.is_empty(), "should report at least one core");

        // Burn a bit of CPU so the second sample sees non-zero busy time.
        let deadline = std::time::Instant::now() + Duration::from_millis(50);
        let mut x: u64 = 1;
        while std::time::Instant::now() < deadline {
            x = x.wrapping_mul(0x9e3779b97f4a7c15).wrapping_add(1);
        }
        std::hint::black_box(x);

        let second = c.collect().await.expect("second");
        assert!(
            (0.0..=1.0).contains(&second.aggregate_utilization),
            "utilization out of range: {}",
            second.aggregate_utilization
        );
    }
}
