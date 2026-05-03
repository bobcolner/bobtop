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
}

#[derive(Debug, Clone, Copy)]
pub struct HugePages {
    pub total: u64,
    pub free: u64,
    pub size_bytes: u64,
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
