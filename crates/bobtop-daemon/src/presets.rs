//! Preset bank for the Shift+1..4 keybinds.
//!
//! A "preset" bundles a layout + sort + sort direction — a complete
//! saved view recallable with one keystroke. Mirrors btop's preset
//! model. Theme is intentionally NOT in here — it's a per-session
//! choice that survives preset swaps.

use crate::widgets::TableSort as TableSort;
use bobtop_tui::LayoutPreset;

#[derive(Debug, Clone, Copy)]
pub struct Preset {
    pub label: &'static str,
    pub layout: LayoutPreset,
    pub sort: TableSort,
    pub descending: bool,
}

/// Default 4-slot preset bank. Slot 0 (key `!`) is the everything-on
/// view; slots 1..3 (keys `@`/`#`/`$`) sharpen focus on memory,
/// network, and minimal layouts respectively.
pub const DEFAULT_PRESETS: [Preset; 4] = [
    Preset {
        label: "all panels, sort by CPU",
        layout: LayoutPreset::Full,
        sort: TableSort::Cpu,
        descending: true,
    },
    Preset {
        label: "all panels, sort by MEM",
        layout: LayoutPreset::Full,
        sort: TableSort::Mem,
        descending: true,
    },
    Preset {
        label: "all panels, sort by NET RX",
        layout: LayoutPreset::Full,
        sort: TableSort::NetRx,
        descending: true,
    },
    Preset {
        label: "minimal (CPU + processes only)",
        layout: LayoutPreset::Minimal,
        sort: TableSort::Cpu,
        descending: true,
    },
];
