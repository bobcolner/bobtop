//! Convenient imports for apps built on top of `gtui`.
//!
//! This is the intended "framework" surface: theme, layout, text helpers,
//! and the generic widgets that other TUI apps can compose.

pub use crate::color::{dim, lerp_color, parse_btop_color, Gradient};
pub use crate::layout::{compute as compute_layout, LayoutAreas, LayoutPreset};
pub use crate::text::{
    bool_label, display_width, format_bytes, format_bytes_compact, format_rate, sanitize_for_display,
    truncate_chars, write_str_at,
};
pub use crate::keymap::{Scope, ScopeResult, ScopeStack};
pub use crate::util::{middle_anchor_scroll, Nav};
pub use crate::theme::{
    builtin_names, builtin_source, downsample_theme_to_256, load as load_theme, RawTheme, Theme,
    DEFAULT_THEME_NAME,
};
pub use crate::widgets::{
    ActionBar, BrailleGraph, BoxedPanel, ColumnDef, ConfirmDialog, CornerStyle, DialogFooter,
    DualMode, GraphStyle, GroupAggregate, LegendStyle, LiveTable, Meter, MiniMeter, ModalShell,
    SectionHeader, SelectableList, SettingRow, SettingValue, SettingsForm, Sparkline,
    StackedBar, StackedSegment, TableAlign, TableCell, TableEntry, TableRowExt, WidthSpec,
    Cell, Column, Row, RowKind, Table, ToggleRow, Trace,
};
