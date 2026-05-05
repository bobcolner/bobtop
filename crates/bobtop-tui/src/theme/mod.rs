//! Theme system.
//!
//! - Primary format: btop's `.theme` (parsed by [`parser`]).
//! - 41 upstream themes are embedded at build time (see [`builtin`]).
//! - Runtime search path: built-in → `~/.config/bobtop/themes/` →
//!   `~/.config/btop/themes/` (so existing btop users inherit their themes).
//!
//! The [`Theme`] struct is a strongly-typed view over the canonical key set.
//! Missing keys fall through to a sensible fallback so new btop releases that
//! add keys never break us.

use std::path::PathBuf;

use ratatui::style::Color;

use crate::color::Gradient;

pub mod builtin;
pub mod parser;

pub use builtin::{builtin_names, builtin_source, BUILTIN_THEMES, DEFAULT_THEME_NAME};

/// Strongly-typed theme. Solid keys are required (filled from a fallback if
/// missing). Gradient triples are required. Optional keys (`main_bg`,
/// `proc_misc`, etc.) reflect btop's own optionality — `None` for
/// `main_bg` means "use the terminal default background."
#[derive(Debug, Clone)]
pub struct Theme {
    pub name: String,

    // Solid colors (15 canonical + a few extras observed across themes)
    pub main_bg: Option<Color>,
    pub main_fg: Color,
    pub title: Color,
    pub hi_fg: Color,
    pub selected_bg: Color,
    pub selected_fg: Color,
    pub inactive_fg: Color,
    pub graph_text: Color,
    pub meter_bg: Color,
    pub proc_misc: Color,
    pub cpu_box: Color,
    pub mem_box: Color,
    pub net_box: Color,
    pub proc_box: Color,
    pub div_line: Color,

    // Optional theme-specific solids
    pub followed_bg: Option<Color>,
    pub followed_fg: Option<Color>,
    pub proc_banner_bg: Option<Color>,
    pub proc_banner_fg: Option<Color>,
    pub proc_follow_bg: Option<Color>,
    pub proc_pause_bg: Option<Color>,

    // 9 gradient triples
    pub cpu: Gradient,
    pub temp: Gradient,
    pub used: Gradient,
    pub available: Gradient,
    pub cached: Gradient,
    pub free: Gradient,
    pub download: Gradient,
    pub upload: Gradient,
    pub process: Gradient,
}

impl Theme {
    /// Parse a theme from raw `.theme` source. Missing keys fall back to the
    /// hardcoded fallback ([`Theme::fallback`]).
    pub fn from_source(name: impl Into<String>, source: &str) -> Self {
        let raw = parser::parse(source);
        Self::from_raw(name.into(), &raw)
    }

    fn from_raw(name: String, raw: &parser::RawTheme) -> Self {
        let fb = Theme::fallback();

        // Solid: required → fallback if absent. We treat `Some(None)` (key
        // explicitly empty) as "use fallback solid color" since most solid
        // slots aren't visually meaningful as transparent.
        let solid = |key: &str, default: Color| -> Color {
            raw.get(key).and_then(|v| *v).unwrap_or(default)
        };
        // Optional: `Some(None)` (empty value) collapses to None alongside missing.
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
            name,
            main_bg: opt_solid("main_bg", fb.main_bg),
            main_fg: solid("main_fg", fb.main_fg),
            title: solid("title", fb.title),
            hi_fg: solid("hi_fg", fb.hi_fg),
            selected_bg: solid("selected_bg", fb.selected_bg),
            selected_fg: solid("selected_fg", fb.selected_fg),
            inactive_fg: solid("inactive_fg", fb.inactive_fg),
            graph_text: solid("graph_text", fb.graph_text),
            meter_bg: solid("meter_bg", fb.meter_bg),
            proc_misc: solid("proc_misc", fb.proc_misc),
            cpu_box: solid("cpu_box", fb.cpu_box),
            mem_box: solid("mem_box", fb.mem_box),
            net_box: solid("net_box", fb.net_box),
            proc_box: solid("proc_box", fb.proc_box),
            div_line: solid("div_line", fb.div_line),
            followed_bg: opt_solid("followed_bg", fb.followed_bg),
            followed_fg: opt_solid("followed_fg", fb.followed_fg),
            proc_banner_bg: opt_solid("proc_banner_bg", fb.proc_banner_bg),
            proc_banner_fg: opt_solid("proc_banner_fg", fb.proc_banner_fg),
            proc_follow_bg: opt_solid("proc_follow_bg", fb.proc_follow_bg),
            proc_pause_bg: opt_solid("proc_pause_bg", fb.proc_pause_bg),
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
        }
    }

    /// Hardcoded fallback theme (Dracula-derived). Used both as the default
    /// when the user hasn't selected a theme and as the source of fallback
    /// values for individual missing keys.
    pub fn fallback() -> Self {
        // Values cribbed from themes/dracula.theme so the fallback is a
        // visually complete theme even if every other lookup misses.
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
            proc_misc: rgb(0xbd, 0x93, 0xf9),
            cpu_box: rgb(0x50, 0xfa, 0x7b),
            mem_box: rgb(0xf1, 0xfa, 0x8c),
            net_box: rgb(0xbd, 0x93, 0xf9),
            proc_box: rgb(0xff, 0x55, 0x55),
            div_line: rgb(0x44, 0x47, 0x5a),
            followed_bg: None,
            followed_fg: None,
            proc_banner_bg: None,
            proc_banner_fg: None,
            proc_follow_bg: None,
            proc_pause_bg: None,
            cpu: Gradient::new(rgb(0x50, 0xfa, 0x7b), rgb(0xf1, 0xfa, 0x8c), rgb(0xff, 0x55, 0x55)),
            temp: Gradient::new(rgb(0x8b, 0xe9, 0xfd), rgb(0xff, 0xb8, 0x6c), rgb(0xff, 0x55, 0x55)),
            used: Gradient::new(rgb(0x44, 0x47, 0x5a), rgb(0xff, 0xb8, 0x6c), rgb(0xff, 0x55, 0x55)),
            available: Gradient::new(rgb(0x44, 0x47, 0x5a), rgb(0xf1, 0xfa, 0x8c), rgb(0x50, 0xfa, 0x7b)),
            cached: Gradient::new(rgb(0x44, 0x47, 0x5a), rgb(0x8b, 0xe9, 0xfd), rgb(0xbd, 0x93, 0xf9)),
            free: Gradient::new(rgb(0x44, 0x47, 0x5a), rgb(0x8b, 0xe9, 0xfd), rgb(0x50, 0xfa, 0x7b)),
            download: Gradient::new(rgb(0x44, 0x47, 0x5a), rgb(0xbd, 0x93, 0xf9), rgb(0xff, 0x79, 0xc6)),
            upload: Gradient::new(rgb(0x44, 0x47, 0x5a), rgb(0xff, 0x79, 0xc6), rgb(0xff, 0xb8, 0x6c)),
            process: Gradient::new(rgb(0x44, 0x47, 0x5a), rgb(0xf1, 0xfa, 0x8c), rgb(0x50, 0xfa, 0x7b)),
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
    let conv_grad = |g: Gradient| -> Gradient {
        Gradient::new(conv(g.start), conv(g.mid), conv(g.end))
    };
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
    theme.proc_misc = conv(theme.proc_misc);
    theme.cpu_box = conv(theme.cpu_box);
    theme.mem_box = conv(theme.mem_box);
    theme.net_box = conv(theme.net_box);
    theme.proc_box = conv(theme.proc_box);
    theme.div_line = conv(theme.div_line);
    if let Some(c) = theme.followed_bg { theme.followed_bg = Some(conv(c)); }
    if let Some(c) = theme.followed_fg { theme.followed_fg = Some(conv(c)); }
    if let Some(c) = theme.proc_banner_bg { theme.proc_banner_bg = Some(conv(c)); }
    if let Some(c) = theme.proc_banner_fg { theme.proc_banner_fg = Some(conv(c)); }
    if let Some(c) = theme.proc_follow_bg { theme.proc_follow_bg = Some(conv(c)); }
    if let Some(c) = theme.proc_pause_bg { theme.proc_pause_bg = Some(conv(c)); }
    theme.cpu = conv_grad(theme.cpu);
    theme.temp = conv_grad(theme.temp);
    theme.used = conv_grad(theme.used);
    theme.available = conv_grad(theme.available);
    theme.cached = conv_grad(theme.cached);
    theme.free = conv_grad(theme.free);
    theme.download = conv_grad(theme.download);
    theme.upload = conv_grad(theme.upload);
    theme.process = conv_grad(theme.process);
}

/// Locate a theme by name. Search order: built-in → `~/.config/bobtop/themes/`
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
                // Permission denied / broken symlink / IO error on a file
                // that exists. The user expected this theme to load and
                // it didn't — surface the reason rather than silently
                // falling back to built-in.
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
    out.push(config_root.join("bobtop").join("themes"));
    out.push(config_root.join("btop").join("themes"));
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fallback_has_complete_gradients() {
        let t = Theme::fallback();
        // No way to assert gradient validity directly, but at least confirm
        // sample() doesn't panic across the range.
        for i in 0..=10 {
            let _ = t.cpu.sample(i as f32 / 10.0);
            let _ = t.used.sample(i as f32 / 10.0);
        }
    }

    #[test]
    fn from_source_uses_known_keys() {
        // Use `r##"..."##` because content contains `"#` (closing-quote +
        // hex prefix) which would otherwise terminate `r#"..."#`.
        let src = r##"
theme[main_bg]="#000000"
theme[cpu_start]="#101010"
theme[cpu_mid]="#808080"
theme[cpu_end]="#ffffff"
"##;
        let t = Theme::from_source("test", src);
        assert_eq!(t.main_bg, Some(Color::Rgb(0, 0, 0)));
        assert_eq!(t.cpu.start, Color::Rgb(0x10, 0x10, 0x10));
        assert_eq!(t.cpu.end, Color::Rgb(0xff, 0xff, 0xff));
    }

    #[test]
    fn from_source_falls_back_for_missing_keys() {
        let t = Theme::from_source("sparse", "");
        let fb = Theme::fallback();
        assert_eq!(t.main_fg, fb.main_fg);
        assert_eq!(t.cpu_box, fb.cpu_box);
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
            // Spot-check at least one gradient endpoint resolved to RGB.
            assert!(matches!(t.cpu.start, Color::Rgb(..)));
        }
    }

    #[test]
    fn load_unknown_theme_yields_fallback_with_correct_name() {
        let t = load("definitely-not-a-real-theme-name");
        assert_eq!(t.name, "definitely-not-a-real-theme-name");
    }
}
