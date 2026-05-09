//! Print every bundled theme's name, then a small swatch of its
//! main colors. Demonstrates the [`gtui::theme`] registry +
//! parser. Renders to stdout (no TUI), so it works in CI without a
//! TTY.
//!
//! Run with: `cargo run -p gtui --example themes`

use gtui::theme::{builtin_names, builtin_source};
use gtui::theme::Theme;
use ratatui::style::Color;

fn main() {
    let names: Vec<&str> = builtin_names().collect();
    println!("gtui ships {} themes:", names.len());
    println!();

    // Width the longest name needs, padded to 2 cells of slack so
    // the swatch column lines up across rows.
    let label_w = names.iter().map(|n| n.len()).max().unwrap_or(0) + 2;

    for name in &names {
        let src = builtin_source(name).expect("builtin theme present");
        let theme = Theme::from_source(name.to_string(), src);
        let swatch = format!(
            "bg={}  fg={}  hi={}  title={}  accent[0]={}  accent[1]={}",
            describe(theme.main_bg.unwrap_or(Color::Reset)),
            describe(theme.main_fg),
            describe(theme.hi_fg),
            describe(theme.title),
            describe(theme.panel_accents[0]),
            describe(theme.panel_accents[1]),
        );
        println!("  {name:<label_w$}{swatch}");
    }

    println!();
    println!(
        "User-supplied themes live in ~/.config/gtop/themes/<name>.theme"
    );
    println!(
        "or ~/.config/btop/themes/<name>.theme — both directories are"
    );
    println!("searched by gtui::load_theme().");
}

/// Compact one-line stringifier for a Ratatui Color. We print
/// "#rrggbb" for RGB colors, the variant name otherwise.
fn describe(c: Color) -> String {
    match c {
        Color::Rgb(r, g, b) => format!("#{r:02x}{g:02x}{b:02x}"),
        other => format!("{other:?}"),
    }
}
