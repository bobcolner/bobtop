//! Ratatui rendering layer.
//!
//! - [`color`] — hex parsing and gradient interpolation primitives.
//! - [`theme`] — btop `.theme` parser, embedded built-in registry, runtime
//!   loader with `~/.config/bobtop/themes/` and `~/.config/btop/themes/`
//!   search paths.
//! - [`widgets`] — `BrailleGraph` (the visual identity widget) and
//!   `BoxedPanel` (rounded box with btop-style inline title slots).
//!
//! Step 6 brings the layout engine and the remaining widgets (Meter,
//! MiniMeter, ProcessTable). For now this crate exposes everything needed
//! to render a single graph or box anywhere.

#![forbid(unsafe_code)]

pub mod color;
pub mod layout;
pub mod theme;
pub mod widgets;

pub use color::{dim, lerp_color, parse_btop_color, Gradient};
pub use layout::{compute as compute_layout, LayoutAreas, LayoutPreset};
pub use theme::{builtin_names, builtin_source, load as load_theme, Theme, DEFAULT_THEME_NAME};
pub use widgets::{
    BoxedPanel, BrailleGraph, CornerStyle, DualMode, GraphStyle, Meter, MiniMeter, ProcessSort,
    ProcessTable, Trace,
};
