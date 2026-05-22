//! Shared types and traits for the gtop system monitor.
//!
//! Modules downstream of this one (`crate::collectors`, `crate::pid_attr`,
//! `crate::engine`) all build on the abstractions defined here:
//!
//! - [`Collector`] — the async data-source trait every subsystem implements.
//! - [`MetricEvent`] — the unified, fan-out-friendly enum that wraps every
//!   sample type emitted onto the [`DataBus`].
//! - [`DataBus`] — a `tokio::sync::broadcast`-backed pub/sub channel that
//!   decouples collectors from consumers (TUI, exporters, etc.).

#![forbid(unsafe_code)]
#![warn(missing_debug_implementations)]

pub mod bus;
pub mod collector;
pub mod error;
pub mod event;
pub mod history;
pub mod sample;
pub mod store;

// `Box` / `BoxesEnabled` live in `gtui` (the toolkit's layout module
// consumes them). Re-exported here so the daemon can keep importing
// them via `crate::core::Box`.
pub use gtui::{Box, BoxesEnabled};
pub use bus::DataBus;
pub use collector::Collector;
pub use error::{CoreError, Result};
pub use event::MetricEvent;
pub use history::{
    History, HistoryRing, HostMetrics, Metric, PeakResult, ProcRef, TopProcs, WindowStats,
};
pub use store::{HostSample, SampleStore};
pub use sample::{
    ConnectionDirection, CoreSample, CpuSample, DiskDeviceSample, DiskSample, FilesystemSample,
    GpuDeviceSample, GpuSample, HugePages, InterfaceSample, LoadAverage, MemorySample,
    NetworkSample, ProcessInfo, ProcessSample, ProcessState,
};
