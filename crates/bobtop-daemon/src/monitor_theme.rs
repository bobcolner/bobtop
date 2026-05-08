//! `MonitorTheme` — system-monitor overlay on top of the generic `bobtop_tui::Theme`.
//!
//! The toolkit `Theme` carries only colors every TUI app in the suite needs
//! (bg/fg/title/accent/divider/selected/panel-accents). Monitor-specific
//! slots — the 9 metric gradients (CPU/MEM/NET/disk/process) and the
//! process-modal optionals — live here so they don't pollute the generic
//! toolkit surface.
//!
//! Parsing is delegated to `bobtop_tui::theme::parser`: we read the same
//! `.theme` files as before, just split the field set across two structs.

use std::ops::Deref;

use bobtop_tui::color::{truecolor_to_256, Gradient};
use bobtop_tui::theme::{find_source, parser, RawTheme};
use bobtop_tui::Theme;
use ratatui::style::Color;

/// Monitor theme — generic [`Theme`] plus the metric gradients and
/// process-modal slots only `bobtop-daemon` cares about. `Deref`s to the
/// base theme so callers can read shared fields (`title`, `selected_bg`,
/// etc.) without going through `.base`.
#[derive(Debug, Clone)]
pub struct MonitorTheme {
    pub base: Theme,

    pub cpu: Gradient,
    pub temp: Gradient,
    pub used: Gradient,
    pub available: Gradient,
    pub cached: Gradient,
    pub free: Gradient,
    pub download: Gradient,
    pub upload: Gradient,
    pub process: Gradient,

    pub followed_bg: Option<Color>,
    pub followed_fg: Option<Color>,
    pub proc_banner_bg: Option<Color>,
    pub proc_banner_fg: Option<Color>,
    pub proc_follow_bg: Option<Color>,
    pub proc_pause_bg: Option<Color>,
}

impl MonitorTheme {
    /// Parse a monitor theme from raw `.theme` source. Missing keys fall
    /// back to [`MonitorTheme::fallback`].
    pub fn from_source(name: impl Into<String>, source: &str) -> Self {
        let raw = parser::parse(source);
        Self::from_raw(name.into(), &raw)
    }

    /// Build from an already-parsed `RawTheme` — useful when the caller
    /// already paid the parse cost (e.g. when constructing both base
    /// [`Theme`] and [`MonitorTheme`] from the same source).
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
        let grad = |start: &str, mid: &str, end: &str, default: Gradient| -> Gradient {
            let s = raw.get(start).and_then(|v| *v).unwrap_or(default.start);
            let m = raw.get(mid).and_then(|v| *v).unwrap_or(default.mid);
            let e = raw.get(end).and_then(|v| *v).unwrap_or(default.end);
            Gradient::new(s, m, e)
        };

        Self {
            base,
            cpu: grad("cpu_start", "cpu_mid", "cpu_end", fb.cpu),
            temp: grad("temp_start", "temp_mid", "temp_end", fb.temp),
            used: grad("used_start", "used_mid", "used_end", fb.used),
            available: grad(
                "available_start",
                "available_mid",
                "available_end",
                fb.available,
            ),
            cached: grad("cached_start", "cached_mid", "cached_end", fb.cached),
            free: grad("free_start", "free_mid", "free_end", fb.free),
            download: grad(
                "download_start",
                "download_mid",
                "download_end",
                fb.download,
            ),
            upload: grad("upload_start", "upload_mid", "upload_end", fb.upload),
            process: grad(
                "process_start",
                "process_mid",
                "process_end",
                fb.process,
            ),
            followed_bg: opt_solid("followed_bg", fb.followed_bg),
            followed_fg: opt_solid("followed_fg", fb.followed_fg),
            proc_banner_bg: opt_solid("proc_banner_bg", fb.proc_banner_bg),
            proc_banner_fg: opt_solid("proc_banner_fg", fb.proc_banner_fg),
            proc_follow_bg: opt_solid("proc_follow_bg", fb.proc_follow_bg),
            proc_pause_bg: opt_solid("proc_pause_bg", fb.proc_pause_bg),
        }
    }

    /// Hardcoded fallback (Dracula-derived gradients). Used both as the
    /// default and as the source of fallback values for individual missing
    /// keys.
    pub fn fallback() -> Self {
        let rgb = |r: u8, g: u8, b: u8| Color::Rgb(r, g, b);
        Self {
            base: Theme::fallback(),
            cpu: Gradient::new(rgb(0x50, 0xfa, 0x7b), rgb(0xf1, 0xfa, 0x8c), rgb(0xff, 0x55, 0x55)),
            temp: Gradient::new(rgb(0x8b, 0xe9, 0xfd), rgb(0xff, 0xb8, 0x6c), rgb(0xff, 0x55, 0x55)),
            used: Gradient::new(rgb(0x44, 0x47, 0x5a), rgb(0xff, 0xb8, 0x6c), rgb(0xff, 0x55, 0x55)),
            available: Gradient::new(rgb(0x44, 0x47, 0x5a), rgb(0xf1, 0xfa, 0x8c), rgb(0x50, 0xfa, 0x7b)),
            cached: Gradient::new(rgb(0x44, 0x47, 0x5a), rgb(0x8b, 0xe9, 0xfd), rgb(0xbd, 0x93, 0xf9)),
            free: Gradient::new(rgb(0x44, 0x47, 0x5a), rgb(0x8b, 0xe9, 0xfd), rgb(0x50, 0xfa, 0x7b)),
            download: Gradient::new(rgb(0x44, 0x47, 0x5a), rgb(0xbd, 0x93, 0xf9), rgb(0xff, 0x79, 0xc6)),
            upload: Gradient::new(rgb(0x44, 0x47, 0x5a), rgb(0xff, 0x79, 0xc6), rgb(0xff, 0xb8, 0x6c)),
            process: Gradient::new(rgb(0x44, 0x47, 0x5a), rgb(0xf1, 0xfa, 0x8c), rgb(0x50, 0xfa, 0x7b)),
            followed_bg: None,
            followed_fg: None,
            proc_banner_bg: None,
            proc_banner_fg: None,
            proc_follow_bg: None,
            proc_pause_bg: None,
        }
    }

    /// Semantic alias for `panel_accents[0]` — btop's `cpu_box`.
    pub fn cpu_box(&self) -> Color { self.base.panel_accents[0] }
    /// Semantic alias for `panel_accents[1]` — btop's `mem_box`.
    pub fn mem_box(&self) -> Color { self.base.panel_accents[1] }
    /// Semantic alias for `panel_accents[2]` — btop's `net_box`.
    pub fn net_box(&self) -> Color { self.base.panel_accents[2] }
    /// Semantic alias for `panel_accents[3]` — btop's `proc_box`.
    pub fn proc_box(&self) -> Color { self.base.panel_accents[3] }
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

/// Locate a theme by name and parse both base and monitor fields. Falls
/// back to [`MonitorTheme::fallback`] (logged) if the name resolves to
/// nothing.
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

/// In-place 256-color downsample. Mirrors the toolkit's
/// [`bobtop_tui::downsample_theme_to_256`] for the base, then converts
/// each monitor extra so the truecolor → 256 conversion never hits the
/// render hot path.
pub fn downsample_to_256(theme: &mut MonitorTheme) {
    bobtop_tui::downsample_theme_to_256(&mut theme.base);
    let conv = truecolor_to_256;
    let conv_grad = |g: Gradient| Gradient::new(conv(g.start), conv(g.mid), conv(g.end));
    theme.cpu = conv_grad(theme.cpu);
    theme.temp = conv_grad(theme.temp);
    theme.used = conv_grad(theme.used);
    theme.available = conv_grad(theme.available);
    theme.cached = conv_grad(theme.cached);
    theme.free = conv_grad(theme.free);
    theme.download = conv_grad(theme.download);
    theme.upload = conv_grad(theme.upload);
    theme.process = conv_grad(theme.process);
    if let Some(c) = theme.followed_bg { theme.followed_bg = Some(conv(c)); }
    if let Some(c) = theme.followed_fg { theme.followed_fg = Some(conv(c)); }
    if let Some(c) = theme.proc_banner_bg { theme.proc_banner_bg = Some(conv(c)); }
    if let Some(c) = theme.proc_banner_fg { theme.proc_banner_fg = Some(conv(c)); }
    if let Some(c) = theme.proc_follow_bg { theme.proc_follow_bg = Some(conv(c)); }
    if let Some(c) = theme.proc_pause_bg { theme.proc_pause_bg = Some(conv(c)); }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn from_source_uses_monitor_keys() {
        let src = r##"
theme[cpu_start]="#101010"
theme[cpu_mid]="#808080"
theme[cpu_end]="#ffffff"
theme[main_bg]="#000000"
"##;
        let t = MonitorTheme::from_source("test", src);
        assert_eq!(t.cpu.start, Color::Rgb(0x10, 0x10, 0x10));
        assert_eq!(t.cpu.end, Color::Rgb(0xff, 0xff, 0xff));
        // Base fields parsed too.
        assert_eq!(t.base.main_bg, Some(Color::Rgb(0, 0, 0)));
        // Deref to base works.
        assert_eq!(t.main_bg, Some(Color::Rgb(0, 0, 0)));
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
    fn missing_keys_use_fallback() {
        let t = MonitorTheme::from_source("sparse", "");
        let fb = MonitorTheme::fallback();
        assert_eq!(t.cpu.start, fb.cpu.start);
        assert_eq!(t.cpu.end, fb.cpu.end);
    }

    #[test]
    fn load_unknown_theme_yields_fallback_with_correct_name() {
        let t = load("definitely-not-a-real-theme-name");
        assert_eq!(t.base.name, "definitely-not-a-real-theme-name");
    }
}
