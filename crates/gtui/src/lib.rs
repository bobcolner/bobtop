//! Reusable Ratatui toolkit. Pulled out of the `gtop` system monitor
//! and the `gfb` file/database browser; designed so a third TUI app
//! on the same workspace would be cheap to add.
//!
//! ## Marquee modules
//!
//! - [`widgets`] — `LiveTable`, `BoxedPanel`, `BrailleGraph`,
//!   `Meter`, `MillerColumns`, `ConfirmDialog`, `EditableText`,
//!   and a pile of tighter primitives. Each is a thin Ratatui
//!   `Widget` impl with a builder API.
//! - [`tree`] — `Catalog` trait + `flatten()` walker + `TreeState`.
//!   Plug a custom `Catalog` impl in to render any hierarchical
//!   source as a flat row list with depth / ancestor-line metadata.
//! - [`browser`] — `BrowserShell`, the render helper for the
//!   tree-on-the-left + preview-on-the-right composition both
//!   `gtop fb` and `gfb` use.
//! - [`theme`] — btop's `.theme` format parser + 41 bundled themes.
//! - [`keymap`] — `ScopeStack` for layered keymaps (modal overlays,
//!   per-mode bindings).
//! - [`layout`] — responsive frame-splitting given a per-panel
//!   weight bitmap (`BoxesEnabled`).
//! - [`prelude`] — one-stop import for the surfaces apps usually
//!   touch.
//!
//! ## Helper modules
//!
//! [`color`], [`text`], [`util`] hold smaller primitives shared
//! across the widgets — gradient math, byte / rate formatters, the
//! `Nav` cursor type. Useful if you're building a custom widget that
//! needs to look like the rest of the toolkit.
//!
//! ## Examples
//!
//! Run any of the bundled examples to see widgets in action:
//!
//! ```bash
//! cargo run --example process_table   # sortable LiveTable
//! cargo run --example two_pane_browser # Catalog + BrowserShell
//! cargo run --example themes          # bundled-theme registry
//! ```

#![forbid(unsafe_code)]

pub mod boxes;
pub mod browser;
pub mod color;
pub mod keymap;
pub mod layout;
pub mod prelude;
pub mod theme;
pub mod text;
pub mod terminal;
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
pub use terminal::{TerminalPane, TerminalSession};
pub use keymap::{Scope, ScopeResult, ScopeStack};
pub use util::{middle_anchor_scroll, Nav};
pub use widgets::{
    ActionBar, BrailleGraph, BrailleText, BoxedPanel, ColumnDef, ConfirmDialog, CornerStyle,
    DialogFooter, DualMode, EditableText, GraphStyle, GroupAggregate, HelpModal, LegendStyle,
    LiveTable, MenuOutcome, Meter, MillerColumn, MillerColumns, MiniMeter, ModalShell,
    OptionsMenu, OptionsTab, ScrollableText, SectionHeader, SelectableList, SettingRow,
    SettingValue, SettingsForm, Sparkline, StackedBar, StackedSegment, TabResult, TableAlign,
    TableCell, TableEntry, TableRowExt, WidthSpec, Cell, Column, Row, RowKind, Table, ToggleRow,
    Trace,
};
