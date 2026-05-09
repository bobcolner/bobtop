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

    /// Sample the gradient by integer index in `[0, 100]` — matches btop's
    /// `Theme::g(name).at(idx)` lookup against its precomputed
    /// `array<string, 101>` (btop_theme.cpp:343). Out-of-range indices clamp.
    pub fn at(self, idx: i32) -> Color {
        let i = idx.clamp(0, 100) as f32 / 100.0;
        self.sample(i)
    }

    /// Build the same 101-color lookup table btop precomputes per gradient.
    /// Useful when a hot loop wants to avoid re-running `lerp_u8` per cell —
    /// build once on theme load, index by `(value * 100) as usize`.
    pub fn ramp(self) -> [Color; 101] {
        let mut out = [self.start; 101];
        for (i, slot) in out.iter_mut().enumerate() {
            *slot = self.at(i as i32);
        }
        out
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

/// Downsample a 24-bit RGB color to the closest 256-color terminal palette
/// index — matches btop's `truecolor_to_256` (btop_theme.cpp:155). Returns
/// `Color::Indexed(0..=255)`. Non-RGB colors pass through unchanged.
///
/// Strategy:
/// - **Greyscale shortcut**: when `R/11 ≈ G/11 ≈ B/11` (channels collapse to
///   the same coarse bucket), map onto the 24-step grayscale ramp at indices
///   232..=255. Avoids quantization to a faint colored cube cell.
/// - **6×6×6 RGB cube**: indices 16..=231 via `16 + 36·r6 + 6·g6 + b6` where
///   each `*6 = round(channel / 51)`.
pub fn truecolor_to_256(c: Color) -> Color {
    let (r, g, b) = match c {
        Color::Rgb(r, g, b) => (r, g, b),
        _ => return c,
    };
    // btop's greyscale check — channels divide to the same 0..=23 bucket.
    let gr = r / 11;
    let gg = g / 11;
    let gb = b / 11;
    if gr == gg && gg == gb {
        // 232 + bucket gives 232..=255 (24 grey shades). Clamp via min(23).
        let idx = 232u16 + gr.min(23) as u16;
        return Color::Indexed(idx as u8);
    }
    let r6 = ((r as u16 + 25) / 51).min(5) as u16;
    let g6 = ((g as u16 + 25) / 51).min(5) as u16;
    let b6 = ((b as u16 + 25) / 51).min(5) as u16;
    Color::Indexed((16 + 36 * r6 + 6 * g6 + b6) as u8)
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

    #[test]
    fn ramp_endpoints_match_sample() {
        let g = Gradient::new(
            Color::Rgb(0, 0, 0),
            Color::Rgb(128, 128, 128),
            Color::Rgb(255, 255, 255),
        );
        let r = g.ramp();
        assert_eq!(r[0], g.sample(0.0));
        assert_eq!(r[50], g.sample(0.5));
        assert_eq!(r[100], g.sample(1.0));
        // Monotonic in luminance for this gradient.
        for w in r.windows(2) {
            let (Color::Rgb(_, ag, _), Color::Rgb(_, bg, _)) = (w[0], w[1]) else { unreachable!() };
            assert!(bg >= ag, "ramp went backwards: {ag} → {bg}");
        }
    }

    #[test]
    fn at_clamps_out_of_range() {
        let g = Gradient::new(Color::Rgb(10, 10, 10), Color::Rgb(50, 50, 50), Color::Rgb(90, 90, 90));
        assert_eq!(g.at(-50), Color::Rgb(10, 10, 10));
        assert_eq!(g.at(200), Color::Rgb(90, 90, 90));
    }

    #[test]
    fn truecolor_to_256_grey_uses_greyscale_ramp() {
        // (128, 128, 128) → channels collapse to bucket 11 → idx 232+11 = 243.
        assert_eq!(truecolor_to_256(Color::Rgb(128, 128, 128)), Color::Indexed(243));
        // (0,0,0) → bucket 0 → idx 232 (very dark grey, btop's choice).
        assert_eq!(truecolor_to_256(Color::Rgb(0, 0, 0)), Color::Indexed(232));
    }

    #[test]
    fn truecolor_to_256_color_uses_6x6x6_cube() {
        // Pure red (255,0,0) → r6=5,g6=0,b6=0 → 16 + 180 + 0 + 0 = 196.
        assert_eq!(truecolor_to_256(Color::Rgb(255, 0, 0)), Color::Indexed(196));
        // Pure blue (0,0,255) → 16 + 0 + 0 + 5 = 21.
        assert_eq!(truecolor_to_256(Color::Rgb(0, 0, 255)), Color::Indexed(21));
        // Pure green (0,255,0) → 16 + 0 + 30 + 0 = 46.
        assert_eq!(truecolor_to_256(Color::Rgb(0, 255, 0)), Color::Indexed(46));
    }

    #[test]
    fn truecolor_to_256_passes_non_rgb_through() {
        assert_eq!(truecolor_to_256(Color::Reset), Color::Reset);
        assert_eq!(truecolor_to_256(Color::Indexed(42)), Color::Indexed(42));
    }
}
