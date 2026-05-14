//! Reusable widgets — the surface most consumers spend their time
//! in. Builder-style on every widget; nothing renders until
//! [`ratatui::Frame::render_widget`] is called.
//!
//! Marquee primitives:
//!
//! - [`live_table::LiveTable`] — sortable, groupable, tree-aware
//!   table with sticky-by-key selection.
//! - [`boxed::BoxedPanel`] — rounded-corner panel with btop-style
//!   title + control-hint slots.
//! - [`braille_graph::BrailleGraph`] — area-fill graph with
//!   vertical gradient.
//! - [`meter::Meter`] / [`mini_meter::MiniMeter`] — progress meters
//!   in two sizes.
//! - [`miller_columns::MillerColumns`] — three-pane Miller layout.
//!
//! Plus a constellation of smaller pieces (modals, forms, text
//! views) listed in the per-module index below.

pub mod boxed;
pub mod action_bar;
pub mod braille_graph;
pub mod braille_text;
pub mod confirm_dialog;
pub mod meter;
pub mod help_modal;
pub mod keybind_footer;
pub mod modal;
pub mod options_menu;
pub mod mini_meter;
pub mod live_table;
pub mod editable_text;
pub mod miller_columns;
pub mod scrollable_text;
pub mod selectable_list;
pub mod settings_form;
pub mod section_header;
pub mod sparkline;
pub mod stacked_bar;
pub mod toggle_row;
pub mod table;

pub use boxed::{panel, BoxedPanel, CornerStyle};
pub use action_bar::ActionBar;
pub use braille_graph::{BrailleGraph, DualMode, GraphStyle, Trace, DEFAULT_DIM_FILL};
pub use braille_text::BrailleText;
pub use confirm_dialog::{ConfirmDialog, DialogFooter};
pub use meter::Meter;
pub use help_modal::HelpModal;
pub use modal::ModalShell;
pub use options_menu::{MenuOutcome, OptionsMenu, OptionsTab, TabResult};
pub use mini_meter::MiniMeter;
pub use live_table::{
    cycle_sort_column, Align as TableAlign, Cell as TableCell, ColumnDef, GroupAggregate,
    LiveTable, TableEntry, TableRowExt, WidthSpec,
};
pub use editable_text::EditableText;
pub use miller_columns::{MillerColumn, MillerColumns};
pub use scrollable_text::ScrollableText;
pub use selectable_list::SelectableList;
pub use settings_form::{SettingRow, SettingValue, SettingsForm};
pub use section_header::SectionHeader;
pub use sparkline::Sparkline;
pub use stacked_bar::{LegendStyle, StackedBar, StackedSegment};
pub use table::{Cell, Column, Row, RowKind, Table};
pub use toggle_row::ToggleRow;
