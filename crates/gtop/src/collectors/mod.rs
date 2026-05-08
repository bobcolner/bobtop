//! Per-subsystem data collectors.
//!
//! Each module exposes a struct implementing [`crate::core::Collector`]. The
//! daemon spawns a tick task per collector, calls `collect()` on its declared
//! interval, and publishes each sample onto the [`crate::core::DataBus`].
//!
//! Cross-platform shape: every collector compiles everywhere. Implementations
//! that haven't been written for a target return `Err(CoreError::Unsupported)`
//! from `collect()` and `false` from any availability probe — never a panic.

#![forbid(unsafe_code)]

pub(crate) mod container;
pub(crate) mod cpu;
pub(crate) mod disk;
pub(crate) mod memory;
pub(crate) mod network;
pub(crate) mod process;

pub use cpu::CpuCollector;
pub use disk::DiskCollector;
pub use memory::MemoryCollector;
pub use network::{
    classify_interface, is_virtual_interface, NetInterfaceKind, NetworkGlobalCollector,
};
pub use process::ProcessCollector;
