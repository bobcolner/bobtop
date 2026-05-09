//! Single-row utilization bar used for per-core CPU and similar dense rows.
//!
//! Layout:
//! ```text
//! C0  ▆▆▆▆▆▆▆▆░░░░░░  9%   46°C
//! ```
//! - `label`: short identifier (left).
//! - bar fills the middle width using `▁▂▃▄▅▆▇█` density chars so partial
//!   fills are smooth.
//! - `value_text`: percentage or other right-aligned reading.
//! - `trailing` (optional): extra info (temperature, frequency).

use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::style::{Color, Style};
use ratatui::widgets::Widget;

use crate::color::Gradient;

const LEVELS: [char; 9] = [' ', '▁', '▂', '▃', '▄', '▅', '▆', '▇', '█'];

#[derive(Debug, Clone)]
pub struct MiniMeter {
    pub label: String,
    pub fraction: f64,
    pub value_text: String,
    pub trailing: Option<String>,
    pub gradient: Gradient,
    pub label_color: Color,
    pub text_color: Color,
    pub empty_color: Color,
    /// How many cells to reserve for the leading label column. Bar starts
    /// after that.
    pub label_width: u16,
    /// How many cells to reserve for value + trailing on the right.
    pub right_width: u16,
}

impl MiniMeter {
    pub fn new(label: impl Into<String>, fraction: f64, value_text: impl Into<String>) -> Self {
        Self {
            label: label.into(),
            fraction: fraction.clamp(0.0, 1.0),
            value_text: value_text.into(),
            trailing: None,
            gradient: Gradient::new(
                Color::Rgb(80, 250, 123),
                Color::Rgb(241, 250, 140),
                Color::Rgb(255, 85, 85),
            ),
            label_color: Color::Rgb(248, 248, 242),
            text_color: Color::Rgb(248, 248, 242),
            empty_color: Color::Rgb(68, 71, 90),
            label_width: 4,
            right_width: 6,
        }
    }

    pub fn with_trailing(mut self, t: impl Into<String>) -> Self {
        self.trailing = Some(t.into());
        self
    }

    pub fn with_gradient(mut self, g: Gradient) -> Self {
        self.gradient = g;
        self
    }

    pub fn with_widths(mut self, label_width: u16, right_width: u16) -> Self {
        self.label_width = label_width;
        self.right_width = right_width;
        self
    }
}

impl Widget for &MiniMeter {
    fn render(self, area: Rect, buf: &mut Buffer) {
        if area.width == 0 || area.height == 0 {
            return;
        }
        let y = area.y;
        let total_w = area.width;
        let label_w = self.label_width.min(total_w);
        let right_w_raw = self.right_width
            + self
                .trailing
                .as_ref()
                .map(|s| s.chars().count() as u16 + 1)
                .unwrap_or(0);
        let right_w = right_w_raw.min(total_w.saturating_sub(label_w));
        let bar_w = total_w.saturating_sub(label_w + right_w);

        // Label
        write_left(buf, area.x, y, &self.label, label_w as usize, Style::default().fg(self.label_color));

        // Bar
        if bar_w > 0 {
            let bar_x = area.x + label_w;
            let total_eighths = (self.fraction * (bar_w as f64) * 8.0).round() as usize;
            let bar_color = self.gradient.sample(self.fraction as f32);
            for i in 0..bar_w as usize {
                let cell_eighths = total_eighths
                    .saturating_sub(i * 8)
                    .min(8);
                let glyph = LEVELS[cell_eighths];
                let cell = &mut buf[(bar_x + i as u16, y)];
                cell.set_char(glyph);
                let fg = if cell_eighths > 0 {
                    bar_color
                } else {
                    self.empty_color
                };
                cell.set_style(Style::default().fg(fg));
            }
        }

        // Right side: value + optional trailing.
        let right_x = area.x + label_w + bar_w;
        if right_w > 0 {
            let mut s = self.value_text.clone();
            if let Some(t) = &self.trailing {
                s.push(' ');
                s.push_str(t);
            }
            write_right(buf, right_x, y, right_w, &s, Style::default().fg(self.text_color));
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
        write_left(buf, x, y, s, width as usize, style);
        return;
    }
    let start = x + width - len;
    write_left(buf, start, y, s, len as usize, style);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn renders_label_and_value_text() {
        let m = MiniMeter::new("C0", 0.5, "50%").with_widths(4, 5);
        let area = Rect::new(0, 0, 20, 1);
        let mut buf = Buffer::empty(area);
        (&m).render(area, &mut buf);
        assert_eq!(buf[(0, 0)].symbol(), "C");
        assert_eq!(buf[(1, 0)].symbol(), "0");
        // "50%" right-aligned within last 5 cells (cols 15..20) → "50%" at 17..20.
        assert_eq!(buf[(17, 0)].symbol(), "5");
        assert_eq!(buf[(18, 0)].symbol(), "0");
        assert_eq!(buf[(19, 0)].symbol(), "%");
    }

    #[test]
    fn full_bar_at_one() {
        let m = MiniMeter::new("C0", 1.0, "100%").with_widths(3, 5);
        let area = Rect::new(0, 0, 20, 1);
        let mut buf = Buffer::empty(area);
        (&m).render(area, &mut buf);
        // Bar occupies cols 3..15. All should be "█".
        for x in 3..15 {
            assert_eq!(buf[(x, 0)].symbol(), "█", "col {x}");
        }
    }

    #[test]
    fn empty_bar_at_zero() {
        let m = MiniMeter::new("C0", 0.0, "0%").with_widths(3, 5);
        let area = Rect::new(0, 0, 20, 1);
        let mut buf = Buffer::empty(area);
        (&m).render(area, &mut buf);
        for x in 3..15 {
            assert_eq!(buf[(x, 0)].symbol(), " ", "col {x}");
        }
    }
}
