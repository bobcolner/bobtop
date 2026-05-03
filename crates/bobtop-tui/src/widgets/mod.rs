//! Custom widgets that build the bobtop UI.
//!
//! - [`braille_graph::BrailleGraph`] — area-fill graph with vertical gradient.
//! - [`boxed::BoxedPanel`] — rounded-corner panel with btop-style title slots.
//!
//! Step 6 (layout) adds the meter / mini-meter / process table widgets. Their
//! shapes are sketched in `crates/bobtop-tui/themes/NOTICE`-adjacent docs and
//! the screenshot review notes in conversation memory.

pub mod boxed;
pub mod braille_graph;
pub mod meter;
pub mod mini_meter;
pub mod process_table;

pub use boxed::BoxedPanel;
pub use braille_graph::{BrailleGraph, DualMode, GraphStyle, Trace, DEFAULT_DIM_FILL};
pub use meter::Meter;
pub use mini_meter::MiniMeter;
pub use process_table::{ProcessSort, ProcessTable};
