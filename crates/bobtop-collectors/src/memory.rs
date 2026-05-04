//! Memory utilization collector.
//!
//! - **Linux:** parses `/proc/meminfo`. `used = MemTotal - MemAvailable`,
//!   matching what `free(1)` reports — more accurate than
//!   `total - free - buffers - cached` because the kernel's "available"
//!   estimate accounts for reclaimable slab and page cache pressure.
//!   HugePages are populated only when configured (`HugePages_Total > 0`).
//! - **macOS:** placeholder — returns [`bobtop_core::CoreError::Unsupported`].
//!   Real impl will use `host_statistics64` / `vm_stat`.

use std::collections::HashMap;
use std::time::{Duration, Instant};

use async_trait::async_trait;
use bobtop_core::sample::{HugePages, MemoryPressure, MemorySample};
use bobtop_core::{Collector, CoreError, Result};

const DEFAULT_INTERVAL_MS: u64 = 1000;

#[derive(Debug)]
pub struct MemoryCollector {
    interval: Duration,
}

impl MemoryCollector {
    pub fn new() -> Self {
        Self::with_interval(Duration::from_millis(DEFAULT_INTERVAL_MS))
    }

    pub fn with_interval(interval: Duration) -> Self {
        Self { interval }
    }
}

impl Default for MemoryCollector {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl Collector for MemoryCollector {
    type Sample = MemorySample;

    async fn collect(&self) -> Result<MemorySample> {
        read_memory_sample()
    }

    fn interval(&self) -> Duration {
        self.interval
    }

    fn name(&self) -> &'static str {
        "memory"
    }
}

// -------------------------------------------------------------------------
// Linux reader
// -------------------------------------------------------------------------

#[cfg(target_os = "linux")]
fn read_memory_sample() -> Result<MemorySample> {
    let text = std::fs::read_to_string("/proc/meminfo")?;
    let map = parse_meminfo(&text);
    let mut sample = sample_from_meminfo(&map, Instant::now())?;
    // PSI is best-effort — kernel < 4.20 / unprivileged containers / WSL2
    // don't expose the file. Silent None on read or parse failure so the
    // memory collector stays useful where PSI is unavailable.
    sample.pressure = std::fs::read_to_string("/proc/pressure/memory")
        .ok()
        .and_then(|t| parse_pressure(&t));
    Ok(sample)
}

#[cfg(target_os = "linux")]
fn parse_pressure(text: &str) -> Option<MemoryPressure> {
    // Two lines:
    //   some avg10=0.00 avg60=0.00 avg300=0.00 total=0
    //   full avg10=0.00 avg60=0.00 avg300=0.00 total=0
    let mut p = MemoryPressure::default();
    let mut saw_some = false;
    let mut saw_full = false;
    for line in text.lines() {
        let mut tokens = line.split_ascii_whitespace();
        let kind = tokens.next()?;
        let mut a10 = 0.0_f32;
        let mut a60 = 0.0_f32;
        let mut a300 = 0.0_f32;
        for tok in tokens {
            if let Some(v) = tok.strip_prefix("avg10=") {
                a10 = v.parse().unwrap_or(0.0);
            } else if let Some(v) = tok.strip_prefix("avg60=") {
                a60 = v.parse().unwrap_or(0.0);
            } else if let Some(v) = tok.strip_prefix("avg300=") {
                a300 = v.parse().unwrap_or(0.0);
            }
        }
        match kind {
            "some" => {
                p.some_avg10 = a10;
                p.some_avg60 = a60;
                p.some_avg300 = a300;
                saw_some = true;
            }
            "full" => {
                p.full_avg10 = a10;
                p.full_avg60 = a60;
                p.full_avg300 = a300;
                saw_full = true;
            }
            _ => {}
        }
    }
    // Require at least the `some` line — `full` is optional on some kernels.
    if saw_some || saw_full {
        Some(p)
    } else {
        None
    }
}

#[cfg(target_os = "linux")]
fn parse_meminfo(text: &str) -> HashMap<&str, u64> {
    let mut map = HashMap::with_capacity(64);
    for line in text.lines() {
        // Each line looks like: "MemTotal:        16314276 kB"
        let Some((key, rest)) = line.split_once(':') else {
            continue;
        };
        let mut it = rest.split_ascii_whitespace();
        let Some(value_str) = it.next() else { continue };
        let Ok(value) = value_str.parse::<u64>() else {
            continue;
        };
        map.insert(key.trim(), value);
    }
    map
}

#[cfg(target_os = "linux")]
fn sample_from_meminfo(map: &HashMap<&str, u64>, now: Instant) -> Result<MemorySample> {
    let total_kb = map
        .get("MemTotal")
        .copied()
        .ok_or_else(|| CoreError::parse("/proc/meminfo: MemTotal missing"))?;
    let avail_kb = map
        .get("MemAvailable")
        .copied()
        // Kernels older than 3.14 don't expose MemAvailable; fall back to
        // free + buffers + cached as a coarse approximation.
        .unwrap_or_else(|| {
            map.get("MemFree").copied().unwrap_or(0)
                + map.get("Buffers").copied().unwrap_or(0)
                + map.get("Cached").copied().unwrap_or(0)
        });
    let swap_total_kb = map.get("SwapTotal").copied().unwrap_or(0);
    let swap_free_kb = map.get("SwapFree").copied().unwrap_or(0);

    let total_bytes = total_kb.saturating_mul(1024);
    let available_bytes = avail_kb.saturating_mul(1024);
    let used_bytes = total_bytes.saturating_sub(available_bytes);
    let swap_total_bytes = swap_total_kb.saturating_mul(1024);
    let swap_used_bytes = swap_total_kb
        .saturating_sub(swap_free_kb)
        .saturating_mul(1024);
    let cached_bytes = map
        .get("Cached")
        .copied()
        .unwrap_or(0)
        .saturating_mul(1024);
    let buffers_bytes = map
        .get("Buffers")
        .copied()
        .unwrap_or(0)
        .saturating_mul(1024);
    let free_bytes = map
        .get("MemFree")
        .copied()
        .unwrap_or(0)
        .saturating_mul(1024);

    let huge_pages = match map.get("HugePages_Total").copied().unwrap_or(0) {
        0 => None,
        total => Some(HugePages {
            total,
            free: map.get("HugePages_Free").copied().unwrap_or(0),
            size_bytes: map.get("Hugepagesize").copied().unwrap_or(0).saturating_mul(1024),
        }),
    };

    Ok(MemorySample {
        timestamp: now,
        total_bytes,
        used_bytes,
        available_bytes,
        swap_total_bytes,
        swap_used_bytes,
        huge_pages,
        cached_bytes,
        buffers_bytes,
        free_bytes,
        // PSI is filled by `read_memory_sample` after this constructor;
        // tests that go through `sample_from_meminfo` directly leave it None.
        pressure: None,
    })
}

// -------------------------------------------------------------------------
// macOS placeholder
// -------------------------------------------------------------------------

#[cfg(not(target_os = "linux"))]
fn read_memory_sample() -> Result<MemorySample> {
    Err(CoreError::Unsupported)
}

// -------------------------------------------------------------------------
// Tests
// -------------------------------------------------------------------------

#[cfg(all(test, target_os = "linux"))]
mod tests {
    use super::*;

    const SAMPLE: &str = "\
MemTotal:       16314276 kB
MemFree:         1026440 kB
MemAvailable:   12048888 kB
Buffers:          234112 kB
Cached:          9876544 kB
SwapTotal:       4194300 kB
SwapFree:        4000000 kB
HugePages_Total:       4
HugePages_Free:        2
Hugepagesize:       2048 kB
";

    #[test]
    fn parses_canonical_meminfo() {
        let map = parse_meminfo(SAMPLE);
        let s = sample_from_meminfo(&map, Instant::now()).expect("sample");
        assert_eq!(s.total_bytes, 16314276 * 1024);
        assert_eq!(s.available_bytes, 12048888 * 1024);
        assert_eq!(s.used_bytes, (16314276 - 12048888) * 1024);
        assert_eq!(s.swap_total_bytes, 4194300 * 1024);
        assert_eq!(s.swap_used_bytes, (4194300 - 4000000) * 1024);
        let hp = s.huge_pages.expect("hugepages present");
        assert_eq!(hp.total, 4);
        assert_eq!(hp.free, 2);
        assert_eq!(hp.size_bytes, 2048 * 1024);
    }

    #[test]
    fn missing_memavailable_falls_back_to_free_plus_cache() {
        let text = "MemTotal: 1000 kB\nMemFree: 200 kB\nBuffers: 100 kB\nCached: 300 kB\n";
        let map = parse_meminfo(text);
        let s = sample_from_meminfo(&map, Instant::now()).expect("sample");
        // available = 200 + 100 + 300 = 600 kB → used = 400 kB
        assert_eq!(s.available_bytes, 600 * 1024);
        assert_eq!(s.used_bytes, 400 * 1024);
    }

    #[test]
    fn no_hugepages_when_total_zero() {
        let text = "MemTotal: 1000 kB\nMemAvailable: 800 kB\nHugePages_Total: 0\n";
        let map = parse_meminfo(text);
        let s = sample_from_meminfo(&map, Instant::now()).expect("sample");
        assert!(s.huge_pages.is_none());
    }

    #[test]
    fn parses_pressure_some_and_full_lines() {
        let text = "\
some avg10=0.50 avg60=0.20 avg300=0.05 total=12345
full avg10=0.10 avg60=0.05 avg300=0.01 total=678
";
        let p = parse_pressure(text).expect("psi parse");
        assert!((p.some_avg10 - 0.50).abs() < 1e-6);
        assert!((p.some_avg60 - 0.20).abs() < 1e-6);
        assert!((p.some_avg300 - 0.05).abs() < 1e-6);
        assert!((p.full_avg10 - 0.10).abs() < 1e-6);
        assert!((p.full_avg60 - 0.05).abs() < 1e-6);
        assert!((p.full_avg300 - 0.01).abs() < 1e-6);
    }

    #[test]
    fn pressure_some_only_still_parses() {
        let text = "some avg10=1.5 avg60=0.7 avg300=0.2 total=999\n";
        let p = parse_pressure(text).expect("psi parse");
        assert!((p.some_avg10 - 1.5).abs() < 1e-6);
        assert_eq!(p.full_avg10, 0.0);
    }

    #[test]
    fn pressure_empty_returns_none() {
        assert!(parse_pressure("").is_none());
    }

    #[tokio::test]
    async fn live_collect_returns_plausible_values() {
        let c = MemoryCollector::new();
        let s = c.collect().await.expect("collect");
        assert!(s.total_bytes > 0);
        assert!(s.used_bytes <= s.total_bytes);
        assert!(s.available_bytes <= s.total_bytes);
    }
}
