//! Horizontal gradient-filled bar — used for memory categories (used /
//! available / cached / free) and disk used/free in btop's mem panel.
//!
//! Anatomy (matches the screenshots):
//! ```text
//! Used:                       8.90 GiB
//! 57%
//! ████████████░░░░░░░░░░░░░░░░
//! ```
//!
//! - Row 0: label (left) and right-aligned value text.
//! - Row 1 (optional, when `compact == false`): "NN%" percentage.
//! - Last row: filled bar. Filled portion uses `gradient.sample(fraction)`
//!   as a single color (low fill ⇒ start color, high fill ⇒ end color).
//!   Empty portion is rendered with `meter_bg` using `╶─╴`-style dashes.

use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::style::{Color, Style};
use ratatui::widgets::Widget;

use crate::color::Gradient;

const FILLED: char = '█';
const EMPTY: char = '─';

#[derive(Debug, Clone)]
pub struct Meter {
    pub label: String,
    pub value: String,
    pub fraction: f64,
    pub gradient: Gradient,
    pub meter_bg: Color,
    pub text_color: Color,
    pub label_color: Color,
    /// When `true`, only renders 2 rows (label/value + bar). When `false`,
    /// inserts a `NN%` row between them.
    pub compact: bool,
}

impl Meter {
    pub fn new(label: impl Into<String>, value: impl Into<String>, fraction: f64) -> Self {
        Self {
            label: label.into(),
            value: value.into(),
            fraction: fraction.clamp(0.0, 1.0),
            gradient: Gradient::new(
                Color::Rgb(80, 250, 123),
                Color::Rgb(241, 250, 140),
                Color::Rgb(255, 85, 85),
            ),
            meter_bg: Color::Rgb(68, 71, 90),
            text_color: Color::Rgb(248, 248, 242),
            label_color: Color::Rgb(248, 248, 242),
            compact: false,
        }
    }

    pub fn with_gradient(mut self, gradient: Gradient) -> Self {
        self.gradient = gradient;
        self
    }

    pub fn with_meter_bg(mut self, c: Color) -> Self {
        self.meter_bg = c;
        self
    }

    pub fn with_text_colors(mut self, label: Color, value: Color) -> Self {
        self.label_color = label;
        self.text_color = value;
        self
    }

    pub fn compact(mut self) -> Self {
        self.compact = true;
        self
    }

    /// Minimum row count this meter needs to render anything.
    pub fn min_height(&self) -> u16 {
        if self.compact {
            2
        } else {
            3
        }
    }
}

impl Widget for &Meter {
    fn render(self, area: Rect, buf: &mut Buffer) {
        if area.width == 0 || area.height == 0 {
            return;
        }

        // Row 0 — label + right-aligned value.
        if area.height >= 1 {
            write_left(
                buf,
                area.x,
                area.y,
                &self.label,
                area.width as usize,
                Style::default().fg(self.label_color),
            );
            write_right(
                buf,
                area.x,
                area.y,
                area.width,
                &self.value,
                Style::default().fg(self.text_color),
            );
        }

        // Optional percentage row.
        let bar_y = if self.compact {
            area.y.saturating_add(1)
        } else if area.height >= 3 {
            let pct = format!("{}%", (self.fraction * 100.0).round() as u32);
            write_left(
                buf,
                area.x,
                area.y + 1,
                &pct,
                area.width as usize,
                Style::default().fg(self.text_color),
            );
            area.y + 2
        } else {
            area.y.saturating_add(1)
        };

        if bar_y >= area.y + area.height {
            return;
        }

        // Bar row.
        let bar_color = self.gradient.sample(self.fraction as f32);
        let total = area.width as usize;
        let filled_cells = ((self.fraction * total as f64).round() as usize).min(total);
        for i in 0..total {
            let x = area.x + i as u16;
            let cell = &mut buf[(x, bar_y)];
            if i < filled_cells {
                cell.set_char(FILLED);
                cell.set_style(Style::default().fg(bar_color));
            } else {
                cell.set_char(EMPTY);
                cell.set_style(Style::default().fg(self.meter_bg));
            }
        }
    }
}

fn write_left(buf: &mut Buffer, x: u16, y: u16, s: &str, max_cols: usize, style: Style) {
    let mut col = x;
    let right = x.saturating_add(max_cols as u16).min(buf.area.right());
    for ch in s.chars() {
        if col >= right {
            break;
        }
        let c = &mut buf[(col, y)];
        c.set_char(ch);
        c.set_style(style);
        col = col.saturating_add(1);
    }
}

fn write_right(buf: &mut Buffer, x: u16, y: u16, width: u16, s: &str, style: Style) {
    let len = s.chars().count() as u16;
    if len > width {
        return;
    }
    let start = x + width - len;
    write_left(buf, start, y, s, len as usize, style);
}

#[cfg(test)]
mod tests {
    use super::*;

    fn grad() -> Gradient {
        Gradient::new(Color::Rgb(0, 255, 0), Color::Rgb(255, 255, 0), Color::Rgb(255, 0, 0))
    }

    #[test]
    fn full_bar_at_fraction_one() {
        let m = Meter::new("Used:", "1.0 GiB", 1.0).with_gradient(grad());
        let area = Rect::new(0, 0, 20, 3);
        let mut buf = Buffer::empty(area);
        (&m).render(area, &mut buf);
        for x in 0..area.width {
            assert_eq!(buf[(x, 2)].symbol(), "█", "col {x} should be filled");
        }
    }

    #[test]
    fn empty_bar_at_fraction_zero() {
        let m = Meter::new("Free:", "0", 0.0).with_gradient(grad());
        let area = Rect::new(0, 0, 20, 3);
        let mut buf = Buffer::empty(area);
        (&m).render(area, &mut buf);
        for x in 0..area.width {
            assert_eq!(buf[(x, 2)].symbol(), "─", "col {x} should be empty");
        }
    }

    #[test]
    fn half_bar_fills_half_columns() {
        let m = Meter::new("Used:", "0.5 GiB", 0.5).with_gradient(grad());
        let area = Rect::new(0, 0, 20, 3);
        let mut buf = Buffer::empty(area);
        (&m).render(area, &mut buf);
        let filled: usize = (0..area.width)
            .filter(|x| buf[(*x, 2)].symbol() == "█")
            .count();
        assert_eq!(filled, 10);
    }

    #[test]
    fn label_and_value_render_on_first_row() {
        let m = Meter::new("Used:", "8.90 GiB", 0.57).with_gradient(grad());
        let area = Rect::new(0, 0, 20, 3);
        let mut buf = Buffer::empty(area);
        (&m).render(area, &mut buf);
        assert_eq!(buf[(0, 0)].symbol(), "U");
        // "8.90 GiB" is 8 chars, right-aligned at col 12..20.
        assert_eq!(buf[(12, 0)].symbol(), "8");
        assert_eq!(buf[(19, 0)].symbol(), "B");
    }

    #[test]
    fn percentage_appears_when_not_compact() {
        let m = Meter::new("Used:", "x", 0.57).with_gradient(grad());
        let area = Rect::new(0, 0, 20, 3);
        let mut buf = Buffer::empty(area);
        (&m).render(area, &mut buf);
        assert_eq!(buf[(0, 1)].symbol(), "5");
        assert_eq!(buf[(1, 1)].symbol(), "7");
        assert_eq!(buf[(2, 1)].symbol(), "%");
    }

    #[test]
    fn compact_skips_percentage_row() {
        let m = Meter::new("Used:", "x", 1.0).with_gradient(grad()).compact();
        let area = Rect::new(0, 0, 20, 2);
        let mut buf = Buffer::empty(area);
        (&m).render(area, &mut buf);
        // Bar should be on row 1 (not row 2), since compact uses 2 rows.
        for x in 0..area.width {
            assert_eq!(buf[(x, 1)].symbol(), "█");
        }
    }
}
