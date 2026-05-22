//! `BrailleText` — banner-style text rendered with Unicode braille glyphs.
//!
//! Each character is drawn as a 5-wide × 7-tall pixel bitmap, packed into
//! 3 braille cells wide × 2 cells tall (each braille cell is 2 dot-cols ×
//! 4 dot-rows = 6 × 8 dot grid; the bottom dot-row stays blank as natural
//! inter-line padding).
//!
//! Decorations (`with_decoration("▰▰▰")` etc.) render symmetric ornaments
//! flanking the letter run — typed into the second row of cells so they
//! line up with the letter midline. Paired with `with_gradient(g)` you get
//! banners that rainbow across the width of the text.

use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::style::Style;
use ratatui::widgets::Widget;

use crate::color::Gradient;

const W: usize = 5;
const H: usize = 7;

/// One letter's bitmap as a 5×7 pixel grid: 7 rows, top 5 bits of each
/// byte represent the row's pixels (bit 4 = leftmost). Bit set = lit.
type Glyph = [u8; H];

const fn row(b0: u8, b1: u8, b2: u8, b3: u8, b4: u8) -> u8 {
    (b0 << 4) | (b1 << 3) | (b2 << 2) | (b3 << 1) | b4
}

#[allow(clippy::too_many_arguments)]
const fn glyph(
    r0: u8, r1: u8, r2: u8, r3: u8, r4: u8, r5: u8, r6: u8,
) -> Glyph {
    [r0, r1, r2, r3, r4, r5, r6]
}

const BLANK: Glyph = [0; H];

#[rustfmt::skip]
fn glyph_for(ch: char) -> Glyph {
    match ch.to_ascii_uppercase() {
        'A' => glyph(
            row(0,1,1,1,0),
            row(1,0,0,0,1),
            row(1,0,0,0,1),
            row(1,1,1,1,1),
            row(1,0,0,0,1),
            row(1,0,0,0,1),
            row(1,0,0,0,1),
        ),
        'B' => glyph(
            row(1,1,1,1,0),
            row(1,0,0,0,1),
            row(1,0,0,0,1),
            row(1,1,1,1,0),
            row(1,0,0,0,1),
            row(1,0,0,0,1),
            row(1,1,1,1,0),
        ),
        'C' => glyph(
            row(0,1,1,1,1),
            row(1,0,0,0,0),
            row(1,0,0,0,0),
            row(1,0,0,0,0),
            row(1,0,0,0,0),
            row(1,0,0,0,0),
            row(0,1,1,1,1),
        ),
        'D' => glyph(
            row(1,1,1,1,0),
            row(1,0,0,0,1),
            row(1,0,0,0,1),
            row(1,0,0,0,1),
            row(1,0,0,0,1),
            row(1,0,0,0,1),
            row(1,1,1,1,0),
        ),
        'E' => glyph(
            row(1,1,1,1,1),
            row(1,0,0,0,0),
            row(1,0,0,0,0),
            row(1,1,1,1,0),
            row(1,0,0,0,0),
            row(1,0,0,0,0),
            row(1,1,1,1,1),
        ),
        'F' => glyph(
            row(1,1,1,1,1),
            row(1,0,0,0,0),
            row(1,0,0,0,0),
            row(1,1,1,1,0),
            row(1,0,0,0,0),
            row(1,0,0,0,0),
            row(1,0,0,0,0),
        ),
        'G' => glyph(
            row(0,1,1,1,1),
            row(1,0,0,0,0),
            row(1,0,0,0,0),
            row(1,0,0,1,1),
            row(1,0,0,0,1),
            row(1,0,0,0,1),
            row(0,1,1,1,1),
        ),
        'H' => glyph(
            row(1,0,0,0,1),
            row(1,0,0,0,1),
            row(1,0,0,0,1),
            row(1,1,1,1,1),
            row(1,0,0,0,1),
            row(1,0,0,0,1),
            row(1,0,0,0,1),
        ),
        'I' => glyph(
            row(1,1,1,1,1),
            row(0,0,1,0,0),
            row(0,0,1,0,0),
            row(0,0,1,0,0),
            row(0,0,1,0,0),
            row(0,0,1,0,0),
            row(1,1,1,1,1),
        ),
        'J' => glyph(
            row(0,0,0,0,1),
            row(0,0,0,0,1),
            row(0,0,0,0,1),
            row(0,0,0,0,1),
            row(0,0,0,0,1),
            row(1,0,0,0,1),
            row(0,1,1,1,0),
        ),
        'K' => glyph(
            row(1,0,0,0,1),
            row(1,0,0,1,0),
            row(1,0,1,0,0),
            row(1,1,0,0,0),
            row(1,0,1,0,0),
            row(1,0,0,1,0),
            row(1,0,0,0,1),
        ),
        'L' => glyph(
            row(1,0,0,0,0),
            row(1,0,0,0,0),
            row(1,0,0,0,0),
            row(1,0,0,0,0),
            row(1,0,0,0,0),
            row(1,0,0,0,0),
            row(1,1,1,1,1),
        ),
        'M' => glyph(
            row(1,0,0,0,1),
            row(1,1,0,1,1),
            row(1,0,1,0,1),
            row(1,0,1,0,1),
            row(1,0,0,0,1),
            row(1,0,0,0,1),
            row(1,0,0,0,1),
        ),
        'N' => glyph(
            row(1,0,0,0,1),
            row(1,1,0,0,1),
            row(1,1,0,0,1),
            row(1,0,1,0,1),
            row(1,0,0,1,1),
            row(1,0,0,1,1),
            row(1,0,0,0,1),
        ),
        'O' => glyph(
            row(0,1,1,1,0),
            row(1,0,0,0,1),
            row(1,0,0,0,1),
            row(1,0,0,0,1),
            row(1,0,0,0,1),
            row(1,0,0,0,1),
            row(0,1,1,1,0),
        ),
        'P' => glyph(
            row(1,1,1,1,0),
            row(1,0,0,0,1),
            row(1,0,0,0,1),
            row(1,1,1,1,0),
            row(1,0,0,0,0),
            row(1,0,0,0,0),
            row(1,0,0,0,0),
        ),
        'Q' => glyph(
            row(0,1,1,1,0),
            row(1,0,0,0,1),
            row(1,0,0,0,1),
            row(1,0,0,0,1),
            row(1,0,1,0,1),
            row(1,0,0,1,0),
            row(0,1,1,0,1),
        ),
        'R' => glyph(
            row(1,1,1,1,0),
            row(1,0,0,0,1),
            row(1,0,0,0,1),
            row(1,1,1,1,0),
            row(1,0,1,0,0),
            row(1,0,0,1,0),
            row(1,0,0,0,1),
        ),
        'S' => glyph(
            row(0,1,1,1,1),
            row(1,0,0,0,0),
            row(1,0,0,0,0),
            row(0,1,1,1,0),
            row(0,0,0,0,1),
            row(0,0,0,0,1),
            row(1,1,1,1,0),
        ),
        'T' => glyph(
            row(1,1,1,1,1),
            row(0,0,1,0,0),
            row(0,0,1,0,0),
            row(0,0,1,0,0),
            row(0,0,1,0,0),
            row(0,0,1,0,0),
            row(0,0,1,0,0),
        ),
        'U' => glyph(
            row(1,0,0,0,1),
            row(1,0,0,0,1),
            row(1,0,0,0,1),
            row(1,0,0,0,1),
            row(1,0,0,0,1),
            row(1,0,0,0,1),
            row(0,1,1,1,0),
        ),
        'V' => glyph(
            row(1,0,0,0,1),
            row(1,0,0,0,1),
            row(1,0,0,0,1),
            row(1,0,0,0,1),
            row(1,0,0,0,1),
            row(0,1,0,1,0),
            row(0,0,1,0,0),
        ),
        'W' => glyph(
            row(1,0,0,0,1),
            row(1,0,0,0,1),
            row(1,0,0,0,1),
            row(1,0,1,0,1),
            row(1,0,1,0,1),
            row(1,1,0,1,1),
            row(1,0,0,0,1),
        ),
        'X' => glyph(
            row(1,0,0,0,1),
            row(1,0,0,0,1),
            row(0,1,0,1,0),
            row(0,0,1,0,0),
            row(0,1,0,1,0),
            row(1,0,0,0,1),
            row(1,0,0,0,1),
        ),
        'Y' => glyph(
            row(1,0,0,0,1),
            row(1,0,0,0,1),
            row(0,1,0,1,0),
            row(0,0,1,0,0),
            row(0,0,1,0,0),
            row(0,0,1,0,0),
            row(0,0,1,0,0),
        ),
        'Z' => glyph(
            row(1,1,1,1,1),
            row(0,0,0,0,1),
            row(0,0,0,1,0),
            row(0,0,1,0,0),
            row(0,1,0,0,0),
            row(1,0,0,0,0),
            row(1,1,1,1,1),
        ),
        '0' => glyph(
            row(0,1,1,1,0),
            row(1,0,0,1,1),
            row(1,0,1,0,1),
            row(1,0,1,0,1),
            row(1,0,1,0,1),
            row(1,1,0,0,1),
            row(0,1,1,1,0),
        ),
        '1' => glyph(
            row(0,0,1,0,0),
            row(0,1,1,0,0),
            row(0,0,1,0,0),
            row(0,0,1,0,0),
            row(0,0,1,0,0),
            row(0,0,1,0,0),
            row(0,1,1,1,0),
        ),
        '2' => glyph(
            row(0,1,1,1,0),
            row(1,0,0,0,1),
            row(0,0,0,0,1),
            row(0,0,0,1,0),
            row(0,0,1,0,0),
            row(0,1,0,0,0),
            row(1,1,1,1,1),
        ),
        '3' => glyph(
            row(1,1,1,1,1),
            row(0,0,0,1,0),
            row(0,0,1,0,0),
            row(0,0,0,1,0),
            row(0,0,0,0,1),
            row(1,0,0,0,1),
            row(0,1,1,1,0),
        ),
        '4' => glyph(
            row(0,0,0,1,0),
            row(0,0,1,1,0),
            row(0,1,0,1,0),
            row(1,0,0,1,0),
            row(1,1,1,1,1),
            row(0,0,0,1,0),
            row(0,0,0,1,0),
        ),
        '5' => glyph(
            row(1,1,1,1,1),
            row(1,0,0,0,0),
            row(1,1,1,1,0),
            row(0,0,0,0,1),
            row(0,0,0,0,1),
            row(1,0,0,0,1),
            row(0,1,1,1,0),
        ),
        '6' => glyph(
            row(0,0,1,1,0),
            row(0,1,0,0,0),
            row(1,0,0,0,0),
            row(1,1,1,1,0),
            row(1,0,0,0,1),
            row(1,0,0,0,1),
            row(0,1,1,1,0),
        ),
        '7' => glyph(
            row(1,1,1,1,1),
            row(0,0,0,0,1),
            row(0,0,0,1,0),
            row(0,0,1,0,0),
            row(0,1,0,0,0),
            row(0,1,0,0,0),
            row(0,1,0,0,0),
        ),
        '8' => glyph(
            row(0,1,1,1,0),
            row(1,0,0,0,1),
            row(1,0,0,0,1),
            row(0,1,1,1,0),
            row(1,0,0,0,1),
            row(1,0,0,0,1),
            row(0,1,1,1,0),
        ),
        '9' => glyph(
            row(0,1,1,1,0),
            row(1,0,0,0,1),
            row(1,0,0,0,1),
            row(0,1,1,1,1),
            row(0,0,0,0,1),
            row(0,0,0,1,0),
            row(0,1,1,0,0),
        ),
        '-' => glyph(
            row(0,0,0,0,0),
            row(0,0,0,0,0),
            row(0,0,0,0,0),
            row(1,1,1,1,1),
            row(0,0,0,0,0),
            row(0,0,0,0,0),
            row(0,0,0,0,0),
        ),
        '_' => glyph(
            row(0,0,0,0,0),
            row(0,0,0,0,0),
            row(0,0,0,0,0),
            row(0,0,0,0,0),
            row(0,0,0,0,0),
            row(0,0,0,0,0),
            row(1,1,1,1,1),
        ),
        '.' => glyph(
            row(0,0,0,0,0),
            row(0,0,0,0,0),
            row(0,0,0,0,0),
            row(0,0,0,0,0),
            row(0,0,0,0,0),
            row(0,1,1,0,0),
            row(0,1,1,0,0),
        ),
        '!' => glyph(
            row(0,0,1,0,0),
            row(0,0,1,0,0),
            row(0,0,1,0,0),
            row(0,0,1,0,0),
            row(0,0,1,0,0),
            row(0,0,0,0,0),
            row(0,0,1,0,0),
        ),
        '?' => glyph(
            row(0,1,1,1,0),
            row(1,0,0,0,1),
            row(0,0,0,0,1),
            row(0,0,1,1,0),
            row(0,0,1,0,0),
            row(0,0,0,0,0),
            row(0,0,1,0,0),
        ),
        '/' => glyph(
            row(0,0,0,0,1),
            row(0,0,0,0,1),
            row(0,0,0,1,0),
            row(0,0,1,0,0),
            row(0,1,0,0,0),
            row(1,0,0,0,0),
            row(1,0,0,0,0),
        ),
        ':' => glyph(
            row(0,0,0,0,0),
            row(0,1,1,0,0),
            row(0,1,1,0,0),
            row(0,0,0,0,0),
            row(0,1,1,0,0),
            row(0,1,1,0,0),
            row(0,0,0,0,0),
        ),
        ' ' => BLANK,
        _ => glyph(
            row(1,1,1,1,1),
            row(1,0,0,0,1),
            row(1,0,0,0,1),
            row(1,0,0,0,1),
            row(1,0,0,0,1),
            row(1,0,0,0,1),
            row(1,1,1,1,1),
        ),
    }
}

const fn pixel(g: &Glyph, col: u8, row: u8) -> bool {
    let bit = 4u8.saturating_sub(col);
    ((g[row as usize] >> bit) & 1) == 1
}

/// Map a (col, row) within a single braille cell's 2×4 dot grid to its
/// bit in the pattern byte. Standard Unicode dot numbering — see the
/// note in [`crate::widgets::braille_graph`] for layout.
const fn dot_bit(col: u8, row: u8) -> u8 {
    match (col, row) {
        (0, 0) => 1 << 0,
        (0, 1) => 1 << 1,
        (0, 2) => 1 << 2,
        (0, 3) => 1 << 6,
        (1, 0) => 1 << 3,
        (1, 1) => 1 << 4,
        (1, 2) => 1 << 5,
        (1, 3) => 1 << 7,
        _ => 0,
    }
}

/// Width of one letter's render in terminal cells (`5+1` dots / 2 dots
/// per cell, rounded up = 3).
const LETTER_CELLS_W: u16 = 3;
const LETTER_CELLS_H: u16 = 2;
/// Inter-letter gap in cells.
const LETTER_GAP: u16 = 1;

/// Banner-style text renderer using Unicode braille (U+2800..=U+28FF).
///
/// - 5×7 pixel font, drawn as 3 cells wide × 2 cells tall per letter.
/// - Optional gradient: each cell column samples a [`Gradient`] across
///   the banner's width.
/// - Optional horizontal rules: thin lines on each of the 2 rows that
///   extend left and right of the letter run, framing the text. Use
///   `with_rule('─', '─')` for symmetric mid-weight rules, or pair
///   different chars per row for a layered look (e.g. `('━', '─')`).
/// - When rendered into an area wider than the letters' intrinsic width,
///   the letters center horizontally and the rules fill the gap on
///   either side.
pub struct BrailleText<'a> {
    text: &'a str,
    style: Style,
    gradient: Option<Gradient>,
    rule_top: Option<char>,
    rule_bot: Option<char>,
    rule_style: Option<Style>,
}

impl<'a> BrailleText<'a> {
    pub fn new(text: &'a str) -> Self {
        Self {
            text,
            style: Style::default(),
            gradient: None,
            rule_top: None,
            rule_bot: None,
            rule_style: None,
        }
    }

    pub fn with_style(mut self, style: Style) -> Self {
        self.style = style;
        self
    }

    /// Horizontal gradient: each cell column samples the [`Gradient`]
    /// at `t = col / total_width`. Affects letters AND rules.
    pub fn with_gradient(mut self, gradient: Gradient) -> Self {
        self.gradient = Some(gradient);
        self
    }

    /// Add horizontal rules on each of the banner's 2 rows. The rules
    /// extend from the area's left edge to 1 cell before the letter run,
    /// and from 1 cell after the letter run to the area's right edge —
    /// so they only have visible effect when the rendered area is wider
    /// than the intrinsic letter width.
    ///
    /// Common pairings:
    /// - `('─', '─')`  thin line under and over (matched)
    /// - `('━', '━')`  heavy line both rows
    /// - `('═', '═')`  double line
    /// - `('─', '━')`  thin top, heavy bottom (asymmetric weight)
    /// - `('▔', '▁')`  ASCII-safe top/bottom rule chars
    /// - `('•', '•')`  dotted
    pub fn with_rule(mut self, top: char, bottom: char) -> Self {
        self.rule_top = Some(top);
        self.rule_bot = Some(bottom);
        self
    }

    /// Override the rule style (color/modifiers); defaults to `style`.
    pub fn with_rule_style(mut self, style: Style) -> Self {
        self.rule_style = Some(style);
        self
    }

    /// Intrinsic width: 3 cells per letter + 1 cell per inter-letter gap.
    /// The full rendered width grows to fill whatever Rect the caller
    /// passes, with the letters centered.
    pub fn intrinsic_width(&self) -> u16 {
        let n = self.text.chars().count() as u16;
        if n == 0 {
            0
        } else {
            n * LETTER_CELLS_W + n.saturating_sub(1) * LETTER_GAP
        }
    }

    /// Compatibility alias for the intrinsic width.
    pub fn width(&self) -> u16 {
        self.intrinsic_width()
    }

    /// Always 2 cells tall (5×7 pixel font fits in a 2-cell-tall braille block).
    pub fn height(&self) -> u16 {
        LETTER_CELLS_H
    }
}

impl<'a> Widget for &BrailleText<'a> {
    fn render(self, area: Rect, buf: &mut Buffer) {
        if area.width == 0 || area.height == 0 {
            return;
        }
        let right = area.x.saturating_add(area.width);
        let bot = area.y.saturating_add(area.height);
        let letters_w = self.intrinsic_width();

        // Center letters horizontally inside the area. When the area is
        // narrower than the letters, anchor at the left edge and let
        // trailing letters clip naturally.
        let letters_start_x = if area.width >= letters_w {
            area.x + (area.width - letters_w) / 2
        } else {
            area.x
        };
        let letters_end_x = letters_start_x.saturating_add(letters_w);

        // Render rules first on both rows. Skip a 1-cell gap between
        // the rule and the letters so they don't visually collide.
        const RULE_GAP: u16 = 1;
        let rule_left_end = letters_start_x.saturating_sub(RULE_GAP);
        let rule_right_start = letters_end_x.saturating_add(RULE_GAP).min(right);
        let rule_style_base = self.rule_style.unwrap_or(self.style);
        for (row_idx, rule_char) in [self.rule_top, self.rule_bot].iter().enumerate() {
            let Some(ch) = rule_char else { continue };
            let cy = area.y.saturating_add(row_idx as u16);
            if cy >= bot {
                continue;
            }
            // left side: [area.x .. rule_left_end)
            for cx in area.x..rule_left_end.min(right) {
                let style = self.color_for_column(
                    cx.saturating_sub(area.x),
                    area.width,
                    rule_style_base,
                );
                let cell = &mut buf[(cx, cy)];
                cell.set_char(*ch);
                cell.set_style(style);
            }
            // right side: [rule_right_start .. right)
            for cx in rule_right_start..right {
                let style = self.color_for_column(
                    cx.saturating_sub(area.x),
                    area.width,
                    rule_style_base,
                );
                let cell = &mut buf[(cx, cy)];
                cell.set_char(*ch);
                cell.set_style(style);
            }
        }

        // Render each letter into 3 cells × 2 cells starting at letters_start_x.
        let chars: Vec<char> = self.text.chars().collect();
        let mut x = letters_start_x;
        for (li, ch) in chars.iter().enumerate() {
            let g = glyph_for(*ch);
            for cell_row in 0..LETTER_CELLS_H {
                for cell_col in 0..LETTER_CELLS_W {
                    let cx = x.saturating_add(cell_col);
                    let cy = area.y.saturating_add(cell_row);
                    if cx >= right || cy >= bot {
                        continue;
                    }
                    let mut dots: u8 = 0;
                    for dot_row in 0..4 {
                        for dot_col in 0..2 {
                            let pixel_col = cell_col as u8 * 2 + dot_col;
                            let pixel_row = cell_row as u8 * 4 + dot_row;
                            if pixel_col >= W as u8 || pixel_row >= H as u8 {
                                continue;
                            }
                            if pixel(&g, pixel_col, pixel_row) {
                                dots |= dot_bit(dot_col, dot_row);
                            }
                        }
                    }
                    let glyph_char = if dots == 0 {
                        ' '
                    } else {
                        char::from_u32(0x2800 + dots as u32).unwrap_or(' ')
                    };
                    let col_in_area = cx.saturating_sub(area.x);
                    let style = self.color_for_column(col_in_area, area.width, self.style);
                    let cell = &mut buf[(cx, cy)];
                    cell.set_char(glyph_char);
                    cell.set_style(style);
                }
            }
            x = x.saturating_add(LETTER_CELLS_W);
            if li + 1 < chars.len() {
                x = x.saturating_add(LETTER_GAP);
            }
            if x >= right {
                return;
            }
        }
    }
}

impl<'a> BrailleText<'a> {
    fn color_for_column(&self, col: u16, total_w: u16, base: Style) -> Style {
        let Some(g) = &self.gradient else {
            return base;
        };
        if total_w <= 1 {
            return base.fg(g.sample(0.0));
        }
        let t = (col as f32 / (total_w - 1) as f32).clamp(0.0, 1.0);
        base.fg(g.sample(t))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn empty_string_has_zero_width() {
        let t = BrailleText::new("");
        assert_eq!(t.width(), 0);
    }

    #[test]
    fn single_letter_is_three_cells_wide() {
        let t = BrailleText::new("A");
        assert_eq!(t.width(), 3);
        assert_eq!(t.height(), 2);
    }

    #[test]
    fn six_letter_word_has_expected_width() {
        // 6 × 3 cells (letter) + 5 × 1 cell (gap) = 23 cells.
        let t = BrailleText::new("WIDGET");
        assert_eq!(t.width(), 23);
    }

    #[test]
    fn rules_fill_area_around_centered_letters() {
        // 23-cell intrinsic letters drawn in a 33-cell-wide area: 5 cells
        // of rule on each side (with 1-cell gap before/after letters).
        let t = BrailleText::new("WIDGET").with_rule('-', '=');
        let area = Rect::new(0, 0, 33, 2);
        let mut buf = Buffer::empty(area);
        (&t).render(area, &mut buf);
        // Far left should have the top rule char, far right too.
        assert_eq!(buf[(0, 0)].symbol(), "-");
        assert_eq!(buf[(0, 1)].symbol(), "=");
        assert_eq!(buf[(32, 0)].symbol(), "-");
        assert_eq!(buf[(32, 1)].symbol(), "=");
        // 1-cell gap between rule and letters.
        let gap_col = (33 - 23) / 2 - 1;
        assert_eq!(buf[(gap_col, 0)].symbol(), " ");
    }

    #[test]
    fn dot_bit_matches_unicode_braille_numbering() {
        assert_eq!(dot_bit(0, 0), 0x01);
        assert_eq!(dot_bit(1, 3), 0x80);
    }

    #[test]
    fn renders_into_buffer_without_panic() {
        let t = BrailleText::new("WIDGET");
        let area = Rect::new(0, 0, t.width(), t.height());
        let mut buf = Buffer::empty(area);
        (&t).render(area, &mut buf);
        assert_ne!(buf[(0, 0)].symbol(), " ");
    }

    #[test]
    fn render_clips_to_area() {
        let t = BrailleText::new("WIDGET");
        let area = Rect::new(0, 0, 5, 2);
        let mut buf = Buffer::empty(Rect::new(0, 0, 30, 2));
        (&t).render(area, &mut buf);
        assert_eq!(buf[(20, 0)].symbol(), " ");
    }

    #[test]
    fn no_rule_leaves_outer_cells_blank() {
        let t = BrailleText::new("HI");
        let area = Rect::new(0, 0, 20, 2);
        let mut buf = Buffer::empty(area);
        (&t).render(area, &mut buf);
        // Outermost columns have no letter pixels and no rule — stay blank.
        assert_eq!(buf[(0, 0)].symbol(), " ");
        assert_eq!(buf[(19, 0)].symbol(), " ");
    }
}
