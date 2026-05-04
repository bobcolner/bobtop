//! Strongly-typed samples emitted by collectors.
//!
//! Each subsystem (CPU, memory, network, disk, processes, GPU) defines a
//! top-level `*Sample` containing a monotonic timestamp and the relevant
//! payload. Rate values are `f64` because they're computed as deltas over an
//! interval; raw counters and sizes stay `u64`; bounded ratios use `f32`.

use std::time::Instant;

use serde::{Deserialize, Serialize};

// ---------------------------------------------------------------------------
// CPU
// ---------------------------------------------------------------------------

#[derive(Debug, Clone)]
pub struct CpuSample {
    pub timestamp: Instant,
    /// Aggregate utilization across all cores, normalized 0.0..=1.0.
    pub aggregate_utilization: f32,
    pub cores: Vec<CoreSample>,
    pub load_average: Option<LoadAverage>,
}

#[derive(Debug, Clone)]
pub struct CoreSample {
    pub id: u32,
    /// 0.0..=1.0
    pub utilization: f32,
    pub frequency_mhz: Option<u32>,
    pub temperature_c: Option<f32>,
}

#[derive(Debug, Clone, Copy)]
pub struct LoadAverage {
    pub one: f32,
    pub five: f32,
    pub fifteen: f32,
}

// ---------------------------------------------------------------------------
// Memory
// ---------------------------------------------------------------------------

#[derive(Debug, Clone)]
pub struct MemorySample {
    pub timestamp: Instant,
    pub total_bytes: u64,
    pub used_bytes: u64,
    pub available_bytes: u64,
    pub swap_total_bytes: u64,
    pub swap_used_bytes: u64,
    pub huge_pages: Option<HugePages>,
    /// Page cache. Reclaimable under pressure — visible to apps as "used"
    /// in some tools, but really free for new allocations.
    pub cached_bytes: u64,
    /// Kernel I/O buffers. Same reclaimable category as `cached_bytes`,
    /// kept separate so the breakdown widget can show both stripes.
    pub buffers_bytes: u64,
    /// Truly unallocated memory (`MemFree` in `/proc/meminfo`). Not the
    /// same as `available_bytes` — `free` doesn't count cached pages.
    pub free_bytes: u64,
    /// Pressure-stall info from `/proc/pressure/memory`. Linux 4.20+;
    /// `None` on hosts that don't expose it (older kernels, some VMs).
    pub pressure: Option<MemoryPressure>,
}

#[derive(Debug, Clone, Copy)]
pub struct HugePages {
    pub total: u64,
    pub free: u64,
    pub size_bytes: u64,
}

/// Pressure-stall info (`/proc/pressure/memory`). Each window is the
/// percentage of wall-clock time in that interval where at least `some` /
/// `full` tasks were stalled waiting on memory. Numbers come straight from
/// the kernel — no smoothing needed (kernel already does the EWMA).
#[derive(Debug, Clone, Copy, Default)]
pub struct MemoryPressure {
    pub some_avg10: f32,
    pub some_avg60: f32,
    pub some_avg300: f32,
    pub full_avg10: f32,
    pub full_avg60: f32,
    pub full_avg300: f32,
}

// ---------------------------------------------------------------------------
// Network (global, per-interface)
// ---------------------------------------------------------------------------

#[derive(Debug, Clone)]
pub struct NetworkSample {
    pub timestamp: Instant,
    pub interfaces: Vec<InterfaceSample>,
}

#[derive(Debug, Clone)]
pub struct InterfaceSample {
    pub name: String,
    pub rx_bytes_per_sec: f64,
    pub tx_bytes_per_sec: f64,
    pub rx_packets_per_sec: f64,
    pub tx_packets_per_sec: f64,
    pub rx_errors: u64,
    pub tx_errors: u64,
}

// ---------------------------------------------------------------------------
// Disk
// ---------------------------------------------------------------------------

#[derive(Debug, Clone)]
pub struct DiskSample {
    pub timestamp: Instant,
    pub devices: Vec<DiskDeviceSample>,
    /// Filesystem-level capacity per mountpoint, sourced separately from
    /// per-device IO counters (different kernel APIs).
    pub filesystems: Vec<FilesystemSample>,
}

#[derive(Debug, Clone)]
pub struct DiskDeviceSample {
    pub name: String,
    pub read_bytes_per_sec: f64,
    pub write_bytes_per_sec: f64,
    pub read_iops: f64,
    pub write_iops: f64,
    /// Fraction of wall-clock time the device was busy, 0.0..=1.0.
    pub utilization: f32,
}

#[derive(Debug, Clone)]
pub struct FilesystemSample {
    /// Display name — shortened mountpoint ("/", "/home", "swap", ...).
    pub label: String,
    /// Backing block-device basename when known (sda1, nvme0n1p2, "swap").
    pub device: String,
    pub mount_point: String,
    pub total_bytes: u64,
    pub used_bytes: u64,
    pub available_bytes: u64,
    /// Mirrored from the matching [`DiskDeviceSample::utilization`] when the
    /// daemon can join them; `None` otherwise.
    pub io_utilization: Option<f32>,
    /// Joined from the matching block device's read/write rates so the disk
    /// panel can show throughput per mount, not just utilization.
    pub read_bytes_per_sec: Option<f64>,
    pub write_bytes_per_sec: Option<f64>,
}

// ---------------------------------------------------------------------------
// Processes
// ---------------------------------------------------------------------------

#[derive(Debug, Clone)]
pub struct ProcessSample {
    pub timestamp: Instant,
    pub processes: Vec<ProcessInfo>,
}

#[derive(Debug, Clone)]
pub struct ProcessInfo {
    pub pid: u32,
    pub parent_pid: Option<u32>,
    pub name: String,
    pub cmdline: String,
    pub user: String,
    pub state: ProcessState,
    /// 0.0..=1.0, summed across all cores (so 4.0 == saturating 4 cores).
    pub cpu_fraction: f32,
    pub mem_rss_bytes: u64,
    pub mem_vsz_bytes: u64,
    pub threads: u32,
    /// `None` when no [`NetworkAttributor`] tier is providing per-process
    /// bandwidth (Tier 1 / Unavailable).
    pub net_rx_bytes_per_sec: Option<f64>,
    pub net_tx_bytes_per_sec: Option<f64>,
    /// Per-process block-IO rates from sysinfo (`disk_usage` deltas). `None`
    /// when the kernel doesn't expose per-pid IO accounting (rare; default
    /// since Linux 2.6.20 with CONFIG_TASK_IO_ACCOUNTING).
    pub disk_read_bytes_per_sec: Option<f64>,
    pub disk_write_bytes_per_sec: Option<f64>,
    /// Last segment of the process's cgroup v2 path, e.g.
    /// `firefox.service`, `user@1000.service`, `docker-<sha>.scope`.
    /// Read from `/proc/[pid]/cgroup` — drives the "group by cgroup"
    /// view (the actually-useful clustering on modern systemd hosts).
    /// `None` on non-Linux or when the file is unreadable.
    pub cgroup: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum ProcessState {
    Running,
    Sleeping,
    DiskSleep,
    Stopped,
    TracingStop,
    Zombie,
    Idle,
    Dead,
    Other(char),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum ConnectionDirection {
    Inbound,
    Outbound,
    Unknown,
}

// ---------------------------------------------------------------------------
// GPU
// ---------------------------------------------------------------------------

#[derive(Debug, Clone)]
pub struct GpuSample {
    pub timestamp: Instant,
    pub devices: Vec<GpuDeviceSample>,
}

#[derive(Debug, Clone)]
pub struct GpuDeviceSample {
    pub name: String,
    /// 0.0..=1.0
    pub utilization: f32,
    pub mem_used_bytes: u64,
    pub mem_total_bytes: u64,
    pub temperature_c: Option<f32>,
    pub power_w: Option<f32>,
}
