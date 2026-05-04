//! Convenient imports for apps built on top of `bobtop-tui`.
//!
//! This is the intended "framework" surface: theme, layout, text helpers,
//! and the generic widgets that other TUI apps can compose.

pub use crate::color::{dim, lerp_color, parse_btop_color, Gradient};
pub use crate::layout::{compute as compute_layout, LayoutAreas, LayoutPreset};
pub use crate::text::{bool_label, format_bytes, format_rate, truncate_chars, write_str_at};
pub use crate::theme::{
    builtin_names, builtin_source, downsample_theme_to_256, load as load_theme, Theme,
    DEFAULT_THEME_NAME,
};
pub use crate::widgets::{
    ActionBar, BrailleGraph, BoxedPanel, CornerStyle, DataTable, DualMode, GraphStyle,
    LegendStyle, Meter, MiniMeter, ModalShell, SectionHeader, SelectableList, SettingRow,
    SettingValue, SettingsForm, Sparkline, StackedBar, StackedSegment, Cell, Column,
    ProcessTableGroupHeader, ProcessTableRow, ProcessTableRowMeta, ProcessTableSort, Row,
    RowKind, Table, ToggleRow, Trace,
};
