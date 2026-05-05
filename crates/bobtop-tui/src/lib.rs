//! Ratatui rendering layer for building a shared terminal UI frame.
//!
//! - [`color`] — hex parsing and gradient interpolation primitives.
//! - [`theme`] — built-in registry, `.theme` parser, loader, and runtime theme model.
//! - [`layout`] — responsive frame splitting into named regions.
//! - [`widgets`] — generic, reusable controls and panels.
//! - [`prelude`] — the intended one-stop import for app code.

#![forbid(unsafe_code)]

pub mod color;
pub mod layout;
pub mod prelude;
pub mod theme;
pub mod text;
pub mod widgets;

pub use color::{dim, lerp_color, parse_btop_color, Gradient};
pub use layout::{compute as compute_layout, LayoutAreas, LayoutPreset};
pub use theme::{
    builtin_names, builtin_source, downsample_theme_to_256, load as load_theme, Theme,
    DEFAULT_THEME_NAME,
};
pub use text::{
    bool_label, display_width, format_bytes, format_bytes_compact, format_rate, truncate_chars,
    write_str_at, write_str_clipped,
};
pub use widgets::{
    ActionBar, BrailleGraph, BoxedPanel, CornerStyle, DataTable, DualMode, GraphStyle,
    LegendStyle, Meter, MiniMeter, ModalShell, SectionHeader, SelectableList, SettingRow,
    SettingValue, SettingsForm, Sparkline, StackedBar, StackedSegment, Cell, Column,
    ProcessTableGroupHeader, ProcessTableRow, ProcessTableRowMeta, ProcessTableSort, Row,
    RowKind, Table, ToggleRow, Trace,
};
