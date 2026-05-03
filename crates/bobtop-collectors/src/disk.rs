//! Disk I/O collector (Step 9).
//!
//! - **Linux:** parses `/proc/diskstats`. Sector size is the conventional
//!   512 B (Linux exposes raw sector counts regardless of physical sector
//!   size). Utilization comes from `ms_io` (column 13) — fraction of wall
//!   clock the device was busy.
//! - **macOS:** placeholder returning [`bobtop_core::CoreError::Unsupported`].
//!   Real impl will use IOKit's `IOServiceGetMatchingServices` against
//!   `IOBlockStorageDriver` and read its statistics dict.

use std::collections::HashMap;
use std::sync::Mutex;
use std::time::{Duration, Instant};

use async_trait::async_trait;
use bobtop_core::sample::{DiskDeviceSample, DiskSample};
use bobtop_core::{Collector, Result};
#[cfg(not(target_os = "linux"))]
use bobtop_core::CoreError;

const DEFAULT_INTERVAL_MS: u64 = 1000;
const SECTOR_BYTES: u64 = 512;

#[derive(Debug, Clone, Copy, Default)]
struct RawDisk {
    reads_completed: u64,
    sectors_read: u64,
    writes_completed: u64,
    sectors_written: u64,
    ms_io: u64,
}

#[derive(Debug)]
pub struct DiskCollector {
    interval: Duration,
    last: Mutex<HashMap<String, (RawDisk, Instant)>>,
}

impl DiskCollector {
    pub fn new() -> Self {
        Self::with_interval(Duration::from_millis(DEFAULT_INTERVAL_MS))
    }

    pub fn with_interval(interval: Duration) -> Self {
        Self {
            interval,
            last: Mutex::new(HashMap::new()),
        }
    }
}

impl Default for DiskCollector {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl Collector for DiskCollector {
    type Sample = DiskSample;

    async fn collect(&self) -> Result<DiskSample> {
        let now = Instant::now();
        let raw = read_raw()?;
        let mut last = self
            .last
            .lock()
            .unwrap_or_else(|p| p.into_inner());

        let mut devices = Vec::with_capacity(raw.len());
        for (name, cur) in raw.iter() {
            let dev = match last.get(name) {
                Some((prev, prev_t)) => {
                    let dt = now.duration_since(*prev_t).as_secs_f64().max(0.001);
                    let read_bytes = sub(cur.sectors_read, prev.sectors_read) * SECTOR_BYTES;
                    let write_bytes = sub(cur.sectors_written, prev.sectors_written) * SECTOR_BYTES;
                    let read_iops = sub(cur.reads_completed, prev.reads_completed) as f64 / dt;
                    let write_iops = sub(cur.writes_completed, prev.writes_completed) as f64 / dt;
                    let busy_ms = sub(cur.ms_io, prev.ms_io) as f64;
                    let util = (busy_ms / (dt * 1000.0)).clamp(0.0, 1.0) as f32;
                    DiskDeviceSample {
                        name: name.clone(),
                        read_bytes_per_sec: read_bytes as f64 / dt,
                        write_bytes_per_sec: write_bytes as f64 / dt,
                        read_iops,
                        write_iops,
                        utilization: util,
                    }
                }
                None => DiskDeviceSample {
                    name: name.clone(),
                    read_bytes_per_sec: 0.0,
                    write_bytes_per_sec: 0.0,
                    read_iops: 0.0,
                    write_iops: 0.0,
                    utilization: 0.0,
                },
            };
            devices.push(dev);
        }

        last.clear();
        for (name, cur) in raw {
            last.insert(name, (cur, now));
        }
        devices.sort_by(|a, b| a.name.cmp(&b.name));

        Ok(DiskSample {
            timestamp: now,
            devices,
        })
    }

    fn interval(&self) -> Duration {
        self.interval
    }

    fn name(&self) -> &'static str {
        "disk"
    }
}

fn sub(cur: u64, prev: u64) -> u64 {
    cur.saturating_sub(prev)
}

#[cfg(target_os = "linux")]
fn read_raw() -> Result<HashMap<String, RawDisk>> {
    let text = std::fs::read_to_string("/proc/diskstats")?;
    Ok(parse_diskstats(&text))
}

#[cfg(not(target_os = "linux"))]
fn read_raw() -> Result<HashMap<String, RawDisk>> {
    Err(CoreError::Unsupported)
}

#[cfg(target_os = "linux")]
fn parse_diskstats(text: &str) -> HashMap<String, RawDisk> {
    let mut out = HashMap::new();
    for line in text.lines() {
        let fields: Vec<&str> = line.split_ascii_whitespace().collect();
        if fields.len() < 14 {
            continue;
        }
        // 0:major 1:minor 2:name 3:reads 4:rmerged 5:sectors_read 6:ms_reading
        // 7:writes 8:wmerged 9:sectors_written 10:ms_writing 11:in_flight 12:ms_io
        let name = fields[2].to_string();
        if !is_top_level_block_device(&name) {
            continue;
        }
        let parse = |i: usize| fields.get(i).and_then(|s| s.parse::<u64>().ok()).unwrap_or(0);
        out.insert(
            name,
            RawDisk {
                reads_completed: parse(3),
                sectors_read: parse(5),
                writes_completed: parse(7),
                sectors_written: parse(9),
                ms_io: parse(12),
            },
        );
    }
    out
}

/// Heuristic — keep whole disks (sda, nvme0n1, vda) and skip partitions
/// (sda1, nvme0n1p1) and ramdisks (ram0, loop0). Avoids double-counting in
/// the UI when a device + its partitions both report I/O.
#[cfg(target_os = "linux")]
fn is_top_level_block_device(name: &str) -> bool {
    if name.starts_with("ram") || name.starts_with("loop") {
        return false;
    }
    // Partition detection: ends with a digit. NVMe is the awkward case
    // (nvme0n1 is a disk, nvme0n1p1 is its partition) — use 'p' before the
    // digit as the marker.
    let last = name.chars().last().unwrap_or(' ');
    if !last.is_ascii_digit() {
        return true;
    }
    if name.contains("nvme") {
        return !name.contains('p');
    }
    if name.starts_with("mmcblk") {
        return !name.contains('p');
    }
    // Generic: trailing digit on a non-NVMe name → partition (sda1, vda1, hda1).
    false
}

#[cfg(all(test, target_os = "linux"))]
mod tests {
    use super::*;

    const SAMPLE: &str = "   8       0 sda 12345 0 100000 4567 6789 0 200000 9876 0 12000 1000\n   8       1 sda1 100 0 800 5 50 0 200 1 0 100 1\n 259       0 nvme0n1 9999 0 50000 100 1234 0 80000 200 0 5000 500\n 259       1 nvme0n1p1 50 0 400 1 25 0 100 0 0 50 0\n   1       0 ram0 0 0 0 0 0 0 0 0 0 0 0\n";

    #[test]
    fn parses_top_level_disks_only() {
        let map = parse_diskstats(SAMPLE);
        assert!(map.contains_key("sda"));
        assert!(map.contains_key("nvme0n1"));
        assert!(!map.contains_key("sda1"), "partition should be skipped");
        assert!(!map.contains_key("nvme0n1p1"), "nvme partition should be skipped");
        assert!(!map.contains_key("ram0"));
    }

    #[test]
    fn raw_field_indices_correct() {
        let map = parse_diskstats(SAMPLE);
        let sda = map.get("sda").unwrap();
        assert_eq!(sda.reads_completed, 12345);
        assert_eq!(sda.sectors_read, 100000);
        assert_eq!(sda.writes_completed, 6789);
        assert_eq!(sda.sectors_written, 200000);
        // Field index 12 = ms_io (sample's 13th column).
        assert_eq!(sda.ms_io, 12000);
    }

    #[tokio::test]
    async fn first_call_returns_zero_rates() {
        let c = DiskCollector::new();
        let s = c.collect().await.expect("collect");
        for d in &s.devices {
            assert_eq!(d.read_bytes_per_sec, 0.0);
            assert_eq!(d.write_bytes_per_sec, 0.0);
            assert_eq!(d.utilization, 0.0);
        }
    }

    #[test]
    fn top_level_block_classification() {
        assert!(is_top_level_block_device("sda"));
        assert!(is_top_level_block_device("vda"));
        assert!(is_top_level_block_device("nvme0n1"));
        assert!(is_top_level_block_device("mmcblk0"));
        assert!(!is_top_level_block_device("sda1"));
        assert!(!is_top_level_block_device("nvme0n1p1"));
        assert!(!is_top_level_block_device("mmcblk0p2"));
        assert!(!is_top_level_block_device("loop0"));
        assert!(!is_top_level_block_device("ram1"));
    }
}
