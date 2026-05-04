//! Custom widgets that build the bobtop UI.
//!
//! - [`braille_graph::BrailleGraph`] — area-fill graph with vertical gradient.
//! - [`boxed::BoxedPanel`] — rounded-corner panel with btop-style title slots.
//!
//! Step 6 (layout) adds the meter / mini-meter / process table widgets. Their
//! shapes are sketched in `crates/bobtop-tui/themes/NOTICE`-adjacent docs and
//! the screenshot review notes in conversation memory.

pub mod boxed;
pub mod action_bar;
pub mod braille_graph;
pub mod meter;
pub mod modal;
pub mod mini_meter;
pub mod process_table;
pub mod selectable_list;
pub mod settings_form;
pub mod section_header;
pub mod sparkline;
pub mod stacked_bar;
pub mod toggle_row;

pub use boxed::{panel, BoxedPanel, CornerStyle};
pub use action_bar::ActionBar;
pub use braille_graph::{BrailleGraph, DualMode, GraphStyle, Trace, DEFAULT_DIM_FILL};
pub use meter::Meter;
pub use modal::ModalShell;
pub use mini_meter::MiniMeter;
pub use process_table::{
    DisplayRow, GroupHeader, ProcessRowMeta, ProcessSort, ProcessTable, TableLayout,
};
pub use selectable_list::SelectableList;
pub use settings_form::{SettingRow, SettingValue, SettingsForm};
pub use section_header::SectionHeader;
pub use sparkline::Sparkline;
pub use stacked_bar::{LegendStyle, StackedBar, StackedSegment};
pub use toggle_row::ToggleRow;
