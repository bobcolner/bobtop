//! Ratatui rendering layer for building a shared terminal UI frame.
//!
//! - [`color`] — hex parsing and gradient interpolation primitives.
//! - [`theme`] — built-in registry, `.theme` parser, loader, and runtime theme model.
//! - [`layout`] — responsive frame splitting into named regions.
//! - [`widgets`] — generic, reusable controls and panels.
//! - [`prelude`] — the intended one-stop import for app code.

#![forbid(unsafe_code)]

pub mod boxes;
pub mod browser;
pub mod color;
pub mod keymap;
pub mod layout;
pub mod prelude;
pub mod theme;
pub mod text;
pub mod tree;
pub mod util;
pub mod widgets;

pub use boxes::{Box, BoxesEnabled};
pub use color::{dim, lerp_color, parse_btop_color, Gradient};
pub use layout::{
    compute as compute_layout, compute_from_enabled, LayoutAreas, LayoutPreset, PanelSize,
    PanelSizes,
};
pub use theme::{
    builtin_names, builtin_source, downsample_theme_to_256, load as load_theme, RawTheme, Theme,
    DEFAULT_THEME_NAME,
};
pub use text::{
    bool_label, display_width, format_bytes, format_bytes_compact, format_rate, sanitize_for_display,
    truncate_chars, write_str_at, write_str_clipped,
};
pub use keymap::{Scope, ScopeResult, ScopeStack};
pub use util::{middle_anchor_scroll, Nav};
pub use widgets::{
    ActionBar, BrailleGraph, BrailleText, BoxedPanel, ColumnDef, ConfirmDialog, CornerStyle,
    DialogFooter, DualMode, EditableText, GraphStyle, GroupAggregate, LegendStyle, LiveTable,
    Meter, MillerColumn, MillerColumns, MiniMeter, ModalShell, ScrollableText, SectionHeader,
    SelectableList, SettingRow, SettingValue, SettingsForm, Sparkline, StackedBar,
    StackedSegment, TableAlign, TableCell, TableEntry, TableRowExt, WidthSpec, Cell, Column,
    Row, RowKind, Table, ToggleRow, Trace,
};
