//! Parser for btop's `.theme` format.
//!
//! Lines look like `theme[key]="value"`. Values are hex colors (`"#RRGGBB"`,
//! `"#NN"` grayscale shorthand, or `""` for transparent). Anything else on the
//! line — including `#`-prefixed comments — is ignored.

use std::collections::HashMap;

use ratatui::style::Color;

use crate::color::parse_btop_color;

/// Raw key → optional color map. `None` is preserved as transparent intent
/// (a key explicitly set to `""`); missing keys simply don't appear in the map.
pub type RawTheme = HashMap<String, Option<Color>>;

/// Parse the contents of a `.theme` file. Unknown / malformed lines are
/// dropped silently — btop tolerates them and so do we.
pub fn parse(text: &str) -> RawTheme {
    let mut out = RawTheme::new();
    for line in text.lines() {
        if let Some((key, value)) = parse_line(line) {
            out.insert(key, value);
        }
    }
    out
}

fn parse_line(line: &str) -> Option<(String, Option<Color>)> {
    // Strip leading whitespace and skip comments / blanks.
    let line = line.trim_start();
    if line.is_empty() || line.starts_with('#') {
        return None;
    }
    // Expect `theme[<key>]=<value>`.
    let rest = line.strip_prefix("theme[")?;
    let (key, rest) = rest.split_once(']')?;
    let rest = rest.trim_start().strip_prefix('=')?.trim_start();
    // Value is a quoted string; trim trailing comments / whitespace after the
    // closing quote.
    let rest = rest.strip_prefix('"')?;
    let value = rest.split('"').next()?;
    Some((key.trim().to_string(), parse_btop_color(value)))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_simple_key_value() {
        let map = parse("theme[main_bg]=\"#282a36\"\n");
        assert_eq!(
            map.get("main_bg").copied().flatten(),
            Some(Color::Rgb(0x28, 0x2a, 0x36))
        );
    }

    #[test]
    fn empty_value_is_transparent_intent() {
        let map = parse("theme[main_bg]=\"\"\n");
        assert!(map.contains_key("main_bg"));
        assert_eq!(map.get("main_bg").copied().flatten(), None);
    }

    #[test]
    fn ignores_comments_blanks_and_whitespace() {
        let text = "\
# this is a comment
   # indented comment

theme[title]=\"#cc\"
theme[hi_fg]   =   \"#969696\"
not a theme line at all
";
        let map = parse(text);
        assert_eq!(map.get("title").copied().flatten(), Some(Color::Rgb(0xcc, 0xcc, 0xcc)));
        assert_eq!(
            map.get("hi_fg").copied().flatten(),
            Some(Color::Rgb(0x96, 0x96, 0x96))
        );
        assert_eq!(map.len(), 2);
    }

    #[test]
    fn tolerates_trailing_garbage_after_value() {
        // btop accepts trailing comments / extra tokens.
        let map = parse("theme[main_fg]=\"#f8f8f2\" # foreground\n");
        assert_eq!(
            map.get("main_fg").copied().flatten(),
            Some(Color::Rgb(0xf8, 0xf8, 0xf2))
        );
    }

    #[test]
    fn malformed_lines_are_dropped_silently() {
        let map = parse("theme[broken=\"#fff\"\nnope\n[main_bg]=\"#000000\"\n");
        assert!(map.is_empty());
    }
}
