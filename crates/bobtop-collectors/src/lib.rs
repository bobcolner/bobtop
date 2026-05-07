//! Per-subsystem data collectors.
//!
//! Each module exposes a struct implementing [`bobtop_core::Collector`]. The
//! daemon spawns a tick task per collector, calls `collect()` on its declared
//! interval, and publishes each sample onto the [`bobtop_core::DataBus`].
//!
//! Cross-platform shape: every collector compiles everywhere. Implementations
//! that haven't been written for a target return `Err(CoreError::Unsupported)`
//! from `collect()` and `false` from any availability probe — never a panic.

#![forbid(unsafe_code)]

pub mod container;
pub mod cpu;
pub mod disk;
pub mod memory;
pub mod network;
pub mod process;

pub use cpu::CpuCollector;
pub use disk::DiskCollector;
pub use memory::MemoryCollector;
pub use network::{
    classify_interface, is_virtual_interface, NetInterfaceKind, NetworkGlobalCollector,
};
pub use process::ProcessCollector;
