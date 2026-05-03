//! Color helpers: hex parsing (btop format) and gradient interpolation.

use ratatui::style::Color;

/// Parse a btop-format color value.
///
/// Accepts:
/// - `""` (empty) → `None` (transparent / terminal default)
/// - `"#RRGGBB"` → RGB triple
/// - `"#NN"` → grayscale shorthand (`"#cc"` → `Rgb(0xcc, 0xcc, 0xcc)`)
/// - `"#"`, anything else → `None`
pub fn parse_btop_color(s: &str) -> Option<Color> {
    if s.is_empty() || s == "#" {
        return None;
    }
    let hex = s.strip_prefix('#')?;
    match hex.len() {
        6 => {
            let r = u8::from_str_radix(&hex[0..2], 16).ok()?;
            let g = u8::from_str_radix(&hex[2..4], 16).ok()?;
            let b = u8::from_str_radix(&hex[4..6], 16).ok()?;
            Some(Color::Rgb(r, g, b))
        }
        2 => {
            let v = u8::from_str_radix(hex, 16).ok()?;
            Some(Color::Rgb(v, v, v))
        }
        _ => None,
    }
}

/// 3-stop linear gradient (start → mid → end) interpolated in sRGB.
///
/// btop themes encode their visual identity in these triples. Vertical
/// position in a graph maps to `t ∈ [0.0, 1.0]` where `0.0` is `start` (graph
/// baseline) and `1.0` is `end` (graph peak).
#[derive(Debug, Clone, Copy)]
pub struct Gradient {
    pub start: Color,
    pub mid: Color,
    pub end: Color,
}

impl Gradient {
    pub const fn new(start: Color, mid: Color, end: Color) -> Self {
        Self { start, mid, end }
    }

    /// Sample the gradient. `t` is clamped to `[0.0, 1.0]`.
    pub fn sample(self, t: f32) -> Color {
        let t = t.clamp(0.0, 1.0);
        if t < 0.5 {
            lerp_color(self.start, self.mid, t * 2.0)
        } else {
            lerp_color(self.mid, self.end, (t - 0.5) * 2.0)
        }
    }
}

/// Linear interpolate between two colors. Falls back to `a` if either side
/// isn't an RGB color (terminal-named colors don't have a meaningful blend).
pub fn lerp_color(a: Color, b: Color, t: f32) -> Color {
    let t = t.clamp(0.0, 1.0);
    let (Color::Rgb(ar, ag, ab), Color::Rgb(br, bg, bb)) = (a, b) else {
        return a;
    };
    Color::Rgb(
        lerp_u8(ar, br, t),
        lerp_u8(ag, bg, t),
        lerp_u8(ab, bb, t),
    )
}

fn lerp_u8(a: u8, b: u8, t: f32) -> u8 {
    (a as f32 + (b as f32 - a as f32) * t).round().clamp(0.0, 255.0) as u8
}

/// Scale RGB channels by `factor` (0.0 = black, 1.0 = unchanged). Used to
/// dim fill cells beneath the trace so the trace itself reads as the bright
/// edge.
pub fn dim(c: Color, factor: f32) -> Color {
    let factor = factor.clamp(0.0, 1.0);
    let Color::Rgb(r, g, b) = c else {
        return c;
    };
    Color::Rgb(
        (r as f32 * factor) as u8,
        (g as f32 * factor) as u8,
        (b as f32 * factor) as u8,
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_six_char_hex() {
        assert_eq!(parse_btop_color("#282a36"), Some(Color::Rgb(0x28, 0x2a, 0x36)));
        assert_eq!(parse_btop_color("#FFFFFF"), Some(Color::Rgb(255, 255, 255)));
    }

    #[test]
    fn parses_grayscale_shorthand() {
        assert_eq!(parse_btop_color("#cc"), Some(Color::Rgb(0xcc, 0xcc, 0xcc)));
        assert_eq!(parse_btop_color("#10"), Some(Color::Rgb(0x10, 0x10, 0x10)));
    }

    #[test]
    fn empty_and_invalid_return_none() {
        assert_eq!(parse_btop_color(""), None);
        assert_eq!(parse_btop_color("#"), None);
        assert_eq!(parse_btop_color("#xyz"), None);
        assert_eq!(parse_btop_color("#12345"), None); // wrong length
        assert_eq!(parse_btop_color("ff0000"), None); // missing '#'
    }

    #[test]
    fn gradient_samples_endpoints_exactly() {
        let g = Gradient::new(
            Color::Rgb(0, 0, 0),
            Color::Rgb(128, 128, 128),
            Color::Rgb(255, 255, 255),
        );
        assert_eq!(g.sample(0.0), Color::Rgb(0, 0, 0));
        assert_eq!(g.sample(1.0), Color::Rgb(255, 255, 255));
        assert_eq!(g.sample(0.5), Color::Rgb(128, 128, 128));
    }

    #[test]
    fn gradient_clamps_out_of_range() {
        let g = Gradient::new(Color::Rgb(0, 0, 0), Color::Rgb(128, 128, 128), Color::Rgb(255, 255, 255));
        assert_eq!(g.sample(-1.0), Color::Rgb(0, 0, 0));
        assert_eq!(g.sample(2.0), Color::Rgb(255, 255, 255));
    }

    #[test]
    fn dim_scales_channels() {
        assert_eq!(dim(Color::Rgb(200, 100, 50), 0.5), Color::Rgb(100, 50, 25));
        assert_eq!(dim(Color::Rgb(255, 255, 255), 0.0), Color::Rgb(0, 0, 0));
    }
}
