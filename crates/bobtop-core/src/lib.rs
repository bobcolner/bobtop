//! Shared types and traits for the bobtop system monitor.
//!
//! Crates downstream of this one (`bobtop-collectors`, `bobtop-net`,
//! `bobtop-tui`, `bobtop-daemon`) all build on the abstractions defined here:
//!
//! - [`Collector`] — the async data-source trait every subsystem implements.
//! - [`MetricEvent`] — the unified, fan-out-friendly enum that wraps every
//!   sample type emitted onto the [`DataBus`].
//! - [`DataBus`] — a `tokio::sync::broadcast`-backed pub/sub channel that
//!   decouples collectors from consumers (TUI, exporters, etc.).
//!
//! The crate is intentionally dependency-light so it can be reused from
//! anywhere in the workspace without dragging in TUI or collector code.

#![forbid(unsafe_code)]
#![warn(missing_debug_implementations)]

pub mod boxes;
pub mod bus;
pub mod collector;
pub mod error;
pub mod event;
pub mod sample;

pub use boxes::{Box, BoxesEnabled};
pub use bus::DataBus;
pub use collector::Collector;
pub use error::{CoreError, Result};
pub use event::MetricEvent;
pub use sample::{
    ConnectionDirection, CoreSample, CpuSample, DiskDeviceSample, DiskSample, FilesystemSample,
    GpuDeviceSample, GpuSample, HugePages, InterfaceSample, LoadAverage, MemorySample,
    NetworkSample, ProcessInfo, ProcessSample, ProcessState,
};
