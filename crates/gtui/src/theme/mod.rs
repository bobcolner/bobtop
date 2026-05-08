//! Theme system.
//!
//! - Primary format: btop's `.theme` (parsed by [`parser`]).
//! - 41 upstream themes are embedded at build time (see [`builtin`]).
//! - Runtime search path: built-in → `~/.config/gtop/themes/` →
//!   `~/.config/btop/themes/` (so existing btop users inherit their themes).
//!
//! The [`Theme`] struct is the *generic* toolkit theme — visual primitives
//! every TUI app in the suite needs. Apps that need monitor-specific colors
//! (CPU/MEM/NET gradients, process-modal slots) layer their own struct on
//! top — see `gtop::monitor_theme::MonitorTheme` for the canonical
//! example. The `.theme` parser stays generic; per-app overlays read
//! whatever extra keys they need from the same `RawTheme`.

use std::path::PathBuf;

use ratatui::style::Color;

pub mod builtin;
pub mod parser;

pub use builtin::{builtin_names, builtin_source, BUILTIN_THEMES, DEFAULT_THEME_NAME};
pub use parser::RawTheme;

/// Generic theme — the visual primitives any TUI app in the suite needs.
///
/// `panel_accents` corresponds to btop's `cpu_box` / `mem_box` / `net_box` /
/// `proc_box` keys (in that order). Apps index whichever they like; the
/// suite keeps four slots so btop themes round-trip without loss.
///
/// `accent_subtle` corresponds to btop's `proc_misc` key. Used for tree
/// branch glyphs and other subdued accents.
#[derive(Debug, Clone)]
pub struct Theme {
    pub name: String,

    pub main_bg: Option<Color>,
    pub main_fg: Color,
    pub title: Color,
    pub hi_fg: Color,
    pub selected_bg: Color,
    pub selected_fg: Color,
    pub inactive_fg: Color,
    pub graph_text: Color,
    pub meter_bg: Color,
    pub div_line: Color,

    /// Four panel-accent colors, parsed from btop's `cpu_box` / `mem_box` /
    /// `net_box` / `proc_box` keys in that order.
    pub panel_accents: [Color; 4],

    /// Subdued accent — tree glyphs, subtle highlights. From btop `proc_misc`.
    pub accent_subtle: Color,
}

impl Theme {
    /// Parse a theme from raw `.theme` source. Missing keys fall back to
    /// [`Theme::fallback`].
    pub fn from_source(name: impl Into<String>, source: &str) -> Self {
        let raw = parser::parse(source);
        Self::from_raw(name.into(), &raw)
    }

    /// Build a `Theme` from an already-parsed `RawTheme`. Useful when an app
    /// also reads extra keys for its own overlay (e.g. `MonitorTheme`).
    pub fn from_raw(name: impl Into<String>, raw: &RawTheme) -> Self {
        let fb = Theme::fallback();

        let solid = |key: &str, default: Color| -> Color {
            raw.get(key).and_then(|v| *v).unwrap_or(default)
        };
        let opt_solid = |key: &str, default: Option<Color>| -> Option<Color> {
            match raw.get(key) {
                Some(v) => *v,
                None => default,
            }
        };

        Self {
            name: name.into(),
            main_bg: opt_solid("main_bg", fb.main_bg),
            main_fg: solid("main_fg", fb.main_fg),
            title: solid("title", fb.title),
            hi_fg: solid("hi_fg", fb.hi_fg),
            selected_bg: solid("selected_bg", fb.selected_bg),
            selected_fg: solid("selected_fg", fb.selected_fg),
            inactive_fg: solid("inactive_fg", fb.inactive_fg),
            graph_text: solid("graph_text", fb.graph_text),
            meter_bg: solid("meter_bg", fb.meter_bg),
            div_line: solid("div_line", fb.div_line),
            panel_accents: [
                solid("cpu_box", fb.panel_accents[0]),
                solid("mem_box", fb.panel_accents[1]),
                solid("net_box", fb.panel_accents[2]),
                solid("proc_box", fb.panel_accents[3]),
            ],
            accent_subtle: solid("proc_misc", fb.accent_subtle),
        }
    }

    /// Hardcoded fallback theme (Dracula-derived). Used both as the default
    /// when the user hasn't selected a theme and as the source of fallback
    /// values for individual missing keys.
    pub fn fallback() -> Self {
        let rgb = |r: u8, g: u8, b: u8| Color::Rgb(r, g, b);
        Self {
            name: "fallback".into(),
            main_bg: Some(rgb(0x28, 0x2a, 0x36)),
            main_fg: rgb(0xf8, 0xf8, 0xf2),
            title: rgb(0xf8, 0xf8, 0xf2),
            hi_fg: rgb(0x62, 0x72, 0xa4),
            selected_bg: rgb(0xff, 0x79, 0xc6),
            selected_fg: rgb(0xf8, 0xf8, 0xf2),
            inactive_fg: rgb(0x44, 0x47, 0x5a),
            graph_text: rgb(0xf8, 0xf8, 0xf2),
            meter_bg: rgb(0x44, 0x47, 0x5a),
            div_line: rgb(0x44, 0x47, 0x5a),
            panel_accents: [
                rgb(0x50, 0xfa, 0x7b), // cpu_box
                rgb(0xf1, 0xfa, 0x8c), // mem_box
                rgb(0xbd, 0x93, 0xf9), // net_box
                rgb(0xff, 0x55, 0x55), // proc_box
            ],
            accent_subtle: rgb(0xbd, 0x93, 0xf9),
        }
    }
}

impl Default for Theme {
    fn default() -> Self {
        Self::fallback()
    }
}

/// In-place downsample every color in `theme` to the 256-color terminal
/// palette using the same algorithm btop applies when `truecolor=false`
/// (see `truecolor_to_256` in `color.rs`). Run this once on theme load
/// rather than per-cell at render time so the 24×N RGB conversions don't
/// hit the hot path.
pub fn downsample_theme_to_256(theme: &mut Theme) {
    let conv = crate::color::truecolor_to_256;
    if let Some(bg) = theme.main_bg {
        theme.main_bg = Some(conv(bg));
    }
    theme.main_fg = conv(theme.main_fg);
    theme.title = conv(theme.title);
    theme.hi_fg = conv(theme.hi_fg);
    theme.selected_bg = conv(theme.selected_bg);
    theme.selected_fg = conv(theme.selected_fg);
    theme.inactive_fg = conv(theme.inactive_fg);
    theme.graph_text = conv(theme.graph_text);
    theme.meter_bg = conv(theme.meter_bg);
    theme.div_line = conv(theme.div_line);
    for accent in &mut theme.panel_accents {
        *accent = conv(*accent);
    }
    theme.accent_subtle = conv(theme.accent_subtle);
}

/// Locate a theme by name. Search order: built-in → `~/.config/gtop/themes/`
/// → `~/.config/btop/themes/`. Returns the source string and a label
/// describing where it was found, for logging.
pub fn find_source(name: &str) -> Option<(String, String)> {
    if let Some(src) = builtin_source(name) {
        return Some((src.to_string(), format!("built-in:{name}")));
    }
    for dir in user_theme_dirs() {
        let path = dir.join(format!("{name}.theme"));
        match std::fs::read_to_string(&path) {
            Ok(src) => return Some((src, path.display().to_string())),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                // Expected — try the next search dir without noise.
            }
            Err(e) => {
                tracing::warn!(
                    theme = name,
                    path = %path.display(),
                    error = %e,
                    "theme file unreadable; trying next search path",
                );
            }
        }
    }
    None
}

/// Load a theme by name with full search path. Falls back to [`Theme::fallback`]
/// (logged as a warning) if the name resolves to nothing.
pub fn load(name: &str) -> Theme {
    match find_source(name) {
        Some((src, origin)) => {
            tracing::debug!(theme = name, origin = %origin, "loaded theme");
            Theme::from_source(name, &src)
        }
        None => {
            tracing::warn!(theme = name, "theme not found; using fallback");
            let mut t = Theme::fallback();
            t.name = name.to_string();
            t
        }
    }
}

fn user_theme_dirs() -> Vec<PathBuf> {
    let mut out = Vec::with_capacity(2);
    let home = std::env::var_os("HOME").map(PathBuf::from);
    let xdg = std::env::var_os("XDG_CONFIG_HOME").map(PathBuf::from);
    let config_root = xdg
        .or_else(|| home.as_ref().map(|h| h.join(".config")))
        .unwrap_or_else(|| PathBuf::from("."));
    out.push(config_root.join("gtop").join("themes"));
    out.push(config_root.join("btop").join("themes"));
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn from_source_uses_known_keys() {
        let src = r##"
theme[main_bg]="#000000"
theme[cpu_box]="#abcdef"
theme[proc_misc]="#112233"
"##;
        let t = Theme::from_source("test", src);
        assert_eq!(t.main_bg, Some(Color::Rgb(0, 0, 0)));
        assert_eq!(t.panel_accents[0], Color::Rgb(0xab, 0xcd, 0xef));
        assert_eq!(t.accent_subtle, Color::Rgb(0x11, 0x22, 0x33));
    }

    #[test]
    fn from_source_falls_back_for_missing_keys() {
        let t = Theme::from_source("sparse", "");
        let fb = Theme::fallback();
        assert_eq!(t.main_fg, fb.main_fg);
        assert_eq!(t.panel_accents, fb.panel_accents);
        assert_eq!(t.accent_subtle, fb.accent_subtle);
    }

    #[test]
    fn empty_main_bg_means_transparent() {
        let t = Theme::from_source("transparent", r#"theme[main_bg]=""#.to_string().as_str());
        assert_eq!(t.main_bg, None);
    }

    #[test]
    fn every_builtin_parses() {
        for name in builtin_names() {
            let src = builtin_source(name).expect("builtin lookup");
            let t = Theme::from_source(name, src);
            assert_eq!(t.name, name);
            assert!(matches!(t.panel_accents[0], Color::Rgb(..)));
        }
    }

    #[test]
    fn load_unknown_theme_yields_fallback_with_correct_name() {
        let t = load("definitely-not-a-real-theme-name");
        assert_eq!(t.name, "definitely-not-a-real-theme-name");
    }
}
