//! `BrailleText` — banner-style text rendered with Unicode braille glyphs.
//!
//! Each character is drawn as a 3-wide × 5-tall pixel bitmap, packed into
//! 2 braille cells wide × 2 cells tall (each braille cell is 2 dot-cols ×
//! 4 dot-rows = 2 × 4 pixels). Letters fit cleanly inside the 4 × 8 dot
//! grid; the bottom dot row stays blank as inter-line padding.
//!
//! Renders 1 cell of horizontal padding between letters for readability.
//! A 6-letter title like "BOBTOP" therefore takes:
//!     6 letters × 2 cells (letter) + 5 × 1 cell (gap) = 17 cells wide.
//! Vertically: 2 cells tall for everything.

use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::style::Style;
use ratatui::widgets::Widget;

/// Each letter is a 3-wide × 5-tall bitmap packed as a u16. The MSB-first
/// scan order is row-major: bit 14 = (col=0, row=0), bit 13 = (col=1, row=0),
/// bit 12 = (col=2, row=0), bit 11 = (col=0, row=1), … bit 0 = (col=2, row=4).
/// Bit 15 is unused. A `1` bit is a lit pixel.
const fn glyph(bits: u16) -> u16 {
    bits
}

const fn px(g: u16, col: u8, row: u8) -> bool {
    let idx = 14 - (row as u32 * 3 + col as u32);
    ((g >> idx) & 1) == 1
}

const W: usize = 3;
const H: usize = 5;

/// Letter forms. 3×5 grid; rows top-to-bottom, cols left-to-right.
/// Designed for ASCII A-Z, 0-9, and a few punctuation. Lowercase and
/// uppercase share the same form (rendered uppercase) since 3×5 isn't
/// enough resolution for descenders / case distinction.
fn glyph_for(ch: char) -> u16 {
    let upper = ch.to_ascii_uppercase();
    match upper {
        'A' => glyph(0b010_101_111_101_101 << 0),
        'B' => glyph(0b110_101_110_101_110 << 0),
        'C' => glyph(0b011_100_100_100_011 << 0),
        'D' => glyph(0b110_101_101_101_110 << 0),
        'E' => glyph(0b111_100_111_100_111 << 0),
        'F' => glyph(0b111_100_110_100_100 << 0),
        'G' => glyph(0b011_100_101_101_011 << 0),
        'H' => glyph(0b101_101_111_101_101 << 0),
        'I' => glyph(0b111_010_010_010_111 << 0),
        'J' => glyph(0b001_001_001_101_010 << 0),
        'K' => glyph(0b101_110_100_110_101 << 0),
        'L' => glyph(0b100_100_100_100_111 << 0),
        'M' => glyph(0b101_111_111_101_101 << 0),
        'N' => glyph(0b101_111_111_111_101 << 0),
        'O' => glyph(0b010_101_101_101_010 << 0),
        'P' => glyph(0b110_101_110_100_100 << 0),
        'Q' => glyph(0b010_101_101_111_011 << 0),
        'R' => glyph(0b110_101_110_101_101 << 0),
        'S' => glyph(0b011_100_010_001_110 << 0),
        'T' => glyph(0b111_010_010_010_010 << 0),
        'U' => glyph(0b101_101_101_101_010 << 0),
        'V' => glyph(0b101_101_101_010_010 << 0),
        'W' => glyph(0b101_101_111_111_101 << 0),
        'X' => glyph(0b101_101_010_101_101 << 0),
        'Y' => glyph(0b101_101_010_010_010 << 0),
        'Z' => glyph(0b111_001_010_100_111 << 0),
        '0' => glyph(0b010_101_101_101_010 << 0),
        '1' => glyph(0b010_110_010_010_111 << 0),
        '2' => glyph(0b110_001_010_100_111 << 0),
        '3' => glyph(0b110_001_010_001_110 << 0),
        '4' => glyph(0b101_101_111_001_001 << 0),
        '5' => glyph(0b111_100_110_001_110 << 0),
        '6' => glyph(0b011_100_110_101_010 << 0),
        '7' => glyph(0b111_001_010_010_010 << 0),
        '8' => glyph(0b010_101_010_101_010 << 0),
        '9' => glyph(0b010_101_011_001_110 << 0),
        '-' => glyph(0b000_000_111_000_000 << 0),
        '_' => glyph(0b000_000_000_000_111 << 0),
        '.' => glyph(0b000_000_000_000_010 << 0),
        '!' => glyph(0b010_010_010_000_010 << 0),
        '?' => glyph(0b110_001_010_000_010 << 0),
        '/' => glyph(0b001_001_010_100_100 << 0),
        ':' => glyph(0b000_010_000_010_000 << 0),
        ' ' => glyph(0),
        _ => glyph(0b111_101_101_101_111 << 0), // unknown → block
    }
}

/// Banner-style text renderer using Unicode braille (U+2800..=U+28FF).
/// Stateless — re-render with new text/style each frame, cheap.
pub struct BrailleText<'a> {
    text: &'a str,
    style: Style,
}

impl<'a> BrailleText<'a> {
    pub fn new(text: &'a str) -> Self {
        Self {
            text,
            style: Style::default(),
        }
    }

    pub fn with_style(mut self, style: Style) -> Self {
        self.style = style;
        self
    }

    /// Width in terminal cells the text will occupy. Each letter takes
    /// 2 cells; a 1-cell gap separates adjacent letters.
    pub fn width(&self) -> u16 {
        let n = self.text.chars().count() as u16;
        if n == 0 {
            0
        } else {
            n * 2 + n.saturating_sub(1)
        }
    }

    /// Always 2 cells tall.
    pub fn height(&self) -> u16 {
        2
    }
}

impl<'a> Widget for &BrailleText<'a> {
    fn render(self, area: Rect, buf: &mut Buffer) {
        if area.width == 0 || area.height == 0 {
            return;
        }
        let mut cx = area.x;
        let cy = area.y;
        let right = area.x.saturating_add(area.width);
        let bot = area.y.saturating_add(area.height);
        for ch in self.text.chars() {
            // Each letter occupies a 4-col × 8-row dot grid (2 braille
            // cells wide × 2 tall). The 3×5 pixel font fits in the
            // top-left of that grid, leaving the bottom dot-row + the
            // right column as natural inter-letter padding.
            let g = glyph_for(ch);
            for cell_row in 0..2 {
                for cell_col in 0..2 {
                    let x = cx + cell_col;
                    let y = cy + cell_row;
                    if x >= right || y >= bot {
                        continue;
                    }
                    let mut dots: u8 = 0;
                    for dot_row in 0..4 {
                        for dot_col in 0..2 {
                            let pixel_col = (cell_col * 2 + dot_col) as u8;
                            let pixel_row = (cell_row * 4 + dot_row) as u8;
                            if pixel_col >= W as u8 || pixel_row >= H as u8 {
                                continue;
                            }
                            if px(g, pixel_col, pixel_row) {
                                dots |= dot_bit(dot_col as u8, dot_row as u8);
                            }
                        }
                    }
                    let glyph_char = if dots == 0 {
                        ' '
                    } else {
                        char::from_u32(0x2800 + dots as u32).unwrap_or(' ')
                    };
                    let cell = &mut buf[(x, y)];
                    cell.set_char(glyph_char);
                    cell.set_style(self.style);
                }
            }
            // Advance 2 cells (letter width) + 1 cell (gap).
            cx = cx.saturating_add(3);
            if cx >= right {
                break;
            }
        }
    }
}

/// Map a (col, row) within a single braille cell's 2×4 dot grid to its
/// bit in the pattern byte. Unicode dot numbering:
///
/// ```text
///   col=0  col=1
///   ┌───┬───┐
///   │ 1 │ 4 │  row 0
///   │ 2 │ 5 │  row 1
///   │ 3 │ 6 │  row 2
///   │ 7 │ 8 │  row 3
///   └───┴───┘
/// ```
///
/// Bit positions: dot_n = bit (n - 1).
const fn dot_bit(col: u8, row: u8) -> u8 {
    match (col, row) {
        (0, 0) => 1 << 0, // dot 1
        (0, 1) => 1 << 1, // dot 2
        (0, 2) => 1 << 2, // dot 3
        (0, 3) => 1 << 6, // dot 7
        (1, 0) => 1 << 3, // dot 4
        (1, 1) => 1 << 4, // dot 5
        (1, 2) => 1 << 5, // dot 6
        (1, 3) => 1 << 7, // dot 8
        _ => 0,
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
    fn single_letter_is_two_cells_wide() {
        let t = BrailleText::new("A");
        assert_eq!(t.width(), 2);
        assert_eq!(t.height(), 2);
    }

    #[test]
    fn six_letter_word_has_expected_width() {
        // 6 letters × 2 cells + 5 × 1 cell gap = 17 cells.
        let t = BrailleText::new("BOBTOP");
        assert_eq!(t.width(), 17);
    }

    #[test]
    fn dot_bit_matches_unicode_braille_numbering() {
        // dot 1 (col 0, row 0) → bit 0 → glyph U+2801 = ⠁
        assert_eq!(dot_bit(0, 0), 0x01);
        // dot 8 (col 1, row 3) → bit 7 → glyph U+2880 = ⢀
        assert_eq!(dot_bit(1, 3), 0x80);
    }

    #[test]
    fn renders_into_buffer_without_panic() {
        let t = BrailleText::new("BOBTOP");
        let area = Rect::new(0, 0, t.width(), t.height());
        let mut buf = Buffer::empty(area);
        (&t).render(area, &mut buf);
        // First cell should not be blank — 'B' has dots at col=0.
        assert_ne!(buf[(0, 0)].symbol(), " ");
    }

    #[test]
    fn render_clips_to_area() {
        let t = BrailleText::new("BOBTOP");
        // Provide only 5 cells of width — shouldn't panic; partial render OK.
        let area = Rect::new(0, 0, 5, 2);
        let mut buf = Buffer::empty(Rect::new(0, 0, 10, 2));
        (&t).render(area, &mut buf);
        // Cells past col 4 should remain blank (default Buffer state).
        assert_eq!(buf[(8, 0)].symbol(), " ");
    }
}
