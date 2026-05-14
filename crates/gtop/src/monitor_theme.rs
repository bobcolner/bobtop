//! `MonitorTheme` — gtop's thin overlay on top of `gtui::Theme`.
//!
//! The base `gtui::Theme` now carries every color *and* metric gradient
//! the suite uses (cpu/temp/memory/network/process). `MonitorTheme` is
//! down to a handful of gtop-only optionals for the process modal:
//! follow / pause backgrounds and the proc-banner colors. It `Deref`s
//! to `Theme` so existing callsites like `app.theme.cpu` /
//! `app.theme.cpu_box()` keep working unchanged.

use std::ops::Deref;

use gtui::color::truecolor_to_256;
use gtui::theme::{find_source, parser, RawTheme};
use gtui::Theme;
use ratatui::style::Color;

/// gtop's overlay theme — base [`Theme`] plus the process-modal
/// optionals btop themes define under `followed_*` and `proc_banner_*`
/// keys. Everything else (panel accents, metric gradients) lives on
/// the base theme now.
#[derive(Debug, Clone)]
pub struct MonitorTheme {
    pub base: Theme,

    pub followed_bg: Option<Color>,
    pub followed_fg: Option<Color>,
    pub proc_banner_bg: Option<Color>,
    pub proc_banner_fg: Option<Color>,
    pub proc_follow_bg: Option<Color>,
    pub proc_pause_bg: Option<Color>,
}

impl MonitorTheme {
    pub fn from_source(name: impl Into<String>, source: &str) -> Self {
        let raw = parser::parse(source);
        Self::from_raw(name.into(), &raw)
    }

    pub fn from_raw(name: impl Into<String>, raw: &RawTheme) -> Self {
        let name = name.into();
        let base = Theme::from_raw(name.clone(), raw);
        let fb = MonitorTheme::fallback();

        let opt_solid = |key: &str, default: Option<Color>| -> Option<Color> {
            match raw.get(key) {
                Some(v) => *v,
                None => default,
            }
        };

        Self {
            base,
            followed_bg: opt_solid("followed_bg", fb.followed_bg),
            followed_fg: opt_solid("followed_fg", fb.followed_fg),
            proc_banner_bg: opt_solid("proc_banner_bg", fb.proc_banner_bg),
            proc_banner_fg: opt_solid("proc_banner_fg", fb.proc_banner_fg),
            proc_follow_bg: opt_solid("proc_follow_bg", fb.proc_follow_bg),
            proc_pause_bg: opt_solid("proc_pause_bg", fb.proc_pause_bg),
        }
    }

    pub fn fallback() -> Self {
        Self {
            base: Theme::fallback(),
            followed_bg: None,
            followed_fg: None,
            proc_banner_bg: None,
            proc_banner_fg: None,
            proc_follow_bg: None,
            proc_pause_bg: None,
        }
    }
}

impl Default for MonitorTheme {
    fn default() -> Self {
        Self::fallback()
    }
}

impl Deref for MonitorTheme {
    type Target = Theme;
    fn deref(&self) -> &Theme {
        &self.base
    }
}

/// Locate a theme by name and parse both base and monitor fields.
pub fn load(name: &str) -> MonitorTheme {
    match find_source(name) {
        Some((src, origin)) => {
            tracing::debug!(theme = name, origin = %origin, "loaded theme");
            MonitorTheme::from_source(name, &src)
        }
        None => {
            tracing::warn!(theme = name, "theme not found; using fallback");
            let mut t = MonitorTheme::fallback();
            t.base.name = name.to_string();
            t
        }
    }
}

/// In-place 256-color downsample. Defers gradient + base-color
/// conversion to gtui (single source of truth), then converts the
/// gtop-only Option<Color> slots.
pub fn downsample_to_256(theme: &mut MonitorTheme) {
    gtui::downsample_theme_to_256(&mut theme.base);
    let conv = truecolor_to_256;
    if let Some(c) = theme.followed_bg {
        theme.followed_bg = Some(conv(c));
    }
    if let Some(c) = theme.followed_fg {
        theme.followed_fg = Some(conv(c));
    }
    if let Some(c) = theme.proc_banner_bg {
        theme.proc_banner_bg = Some(conv(c));
    }
    if let Some(c) = theme.proc_banner_fg {
        theme.proc_banner_fg = Some(conv(c));
    }
    if let Some(c) = theme.proc_follow_bg {
        theme.proc_follow_bg = Some(conv(c));
    }
    if let Some(c) = theme.proc_pause_bg {
        theme.proc_pause_bg = Some(conv(c));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn from_source_parses_proc_modal_keys() {
        let src = r##"
theme[followed_bg]="#101010"
theme[main_bg]="#000000"
"##;
        let t = MonitorTheme::from_source("test", src);
        assert_eq!(t.followed_bg, Some(Color::Rgb(0x10, 0x10, 0x10)));
        // Base fields still parsed via gtui::Theme.
        assert_eq!(t.base.main_bg, Some(Color::Rgb(0, 0, 0)));
        assert_eq!(t.main_bg, Some(Color::Rgb(0, 0, 0)));
    }

    #[test]
    fn gradients_now_live_on_base_theme() {
        let src = r##"
theme[cpu_start]="#101010"
theme[cpu_mid]="#808080"
theme[cpu_end]="#ffffff"
"##;
        let t = MonitorTheme::from_source("test", src);
        // Deref means `t.cpu` reaches the base theme's gradient.
        assert_eq!(t.cpu.start, Color::Rgb(0x10, 0x10, 0x10));
        assert_eq!(t.cpu.end, Color::Rgb(0xff, 0xff, 0xff));
    }

    #[test]
    fn box_helpers_index_panel_accents() {
        let t = MonitorTheme::fallback();
        assert_eq!(t.cpu_box(), t.base.panel_accents[0]);
        assert_eq!(t.mem_box(), t.base.panel_accents[1]);
        assert_eq!(t.net_box(), t.base.panel_accents[2]);
        assert_eq!(t.proc_box(), t.base.panel_accents[3]);
    }

    #[test]
    fn load_unknown_theme_yields_fallback_with_correct_name() {
        let t = load("definitely-not-a-real-theme-name");
        assert_eq!(t.base.name, "definitely-not-a-real-theme-name");
    }
}
