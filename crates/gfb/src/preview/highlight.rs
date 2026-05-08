//! Shared syntect resources.
//!
//! The `SyntaxSet` and `ThemeSet` are loaded once on first use (a few MB
//! of regex tables) and reused for every text preview *and* the in-place
//! editor. The active syntect theme is hardcoded to `base16-ocean.dark`
//! today; tying it to the active gtop theme's luminance is a polish
//! item tracked in MEMORY.md.

use std::path::Path;
use std::sync::OnceLock;

use ratatui::style::{Modifier, Style};
use syntect::highlighting::{FontStyle, Style as SynStyle, Theme as SynTheme, ThemeSet};
use syntect::parsing::{SyntaxReference, SyntaxSet};

pub fn syntaxes() -> &'static SyntaxSet {
    static SET: OnceLock<SyntaxSet> = OnceLock::new();
    SET.get_or_init(SyntaxSet::load_defaults_newlines)
}

pub fn theme() -> &'static SynTheme {
    static T: OnceLock<SynTheme> = OnceLock::new();
    T.get_or_init(|| {
        let ts = ThemeSet::load_defaults();
        ts.themes
            .get("base16-ocean.dark")
            .cloned()
            .unwrap_or_else(|| {
                ts.themes
                    .values()
                    .next()
                    .cloned()
                    .expect("syntect ships defaults")
            })
    })
}

/// Pick the best syntax for `path`. Falls back to plain text if the
/// extension is unknown and we have no content sample to sniff.
pub fn syntax_for_path<'s>(path: &Path, syntax: &'s SyntaxSet) -> &'s SyntaxReference {
    match path.extension().and_then(|e| e.to_str()) {
        Some(ext) => syntax
            .find_syntax_by_extension(ext)
            .unwrap_or_else(|| syntax.find_syntax_plain_text()),
        None => syntax.find_syntax_plain_text(),
    }
}

pub fn to_ratatui(s: SynStyle) -> Style {
    let mut style = Style::default().fg(ratatui::style::Color::Rgb(
        s.foreground.r,
        s.foreground.g,
        s.foreground.b,
    ));
    if s.font_style.contains(FontStyle::BOLD) {
        style = style.add_modifier(Modifier::BOLD);
    }
    if s.font_style.contains(FontStyle::ITALIC) {
        style = style.add_modifier(Modifier::ITALIC);
    }
    if s.font_style.contains(FontStyle::UNDERLINE) {
        style = style.add_modifier(Modifier::UNDERLINED);
    }
    style
}
