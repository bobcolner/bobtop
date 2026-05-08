//! Simple on/off row for checkbox-style settings screens.

use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::style::{Color, Style};
use ratatui::widgets::Widget;

use crate::truncate_chars;

#[derive(Debug, Clone)]
pub struct ToggleRow {
    pub label: String,
    pub enabled: bool,
    pub cursor: bool,
    pub selected_bg: Color,
    pub selected_fg: Color,
    pub normal_fg: Color,
    pub inactive_fg: Color,
}

impl ToggleRow {
    pub fn new(label: impl Into<String>, enabled: bool) -> Self {
        Self {
            label: label.into(),
            enabled,
            cursor: false,
            selected_bg: Color::Reset,
            selected_fg: Color::Reset,
            normal_fg: Color::Reset,
            inactive_fg: Color::Reset,
        }
    }

    pub fn with_cursor(mut self, cursor: bool) -> Self {
        self.cursor = cursor;
        self
    }

    pub fn with_colors(
        mut self,
        selected_bg: Color,
        selected_fg: Color,
        normal_fg: Color,
        inactive_fg: Color,
    ) -> Self {
        self.selected_bg = selected_bg;
        self.selected_fg = selected_fg;
        self.normal_fg = normal_fg;
        self.inactive_fg = inactive_fg;
        self
    }
}

impl Widget for &ToggleRow {
    fn render(self, area: Rect, buf: &mut Buffer) {
        if area.width == 0 || area.height == 0 {
            return;
        }
        let row_bg = if self.cursor {
            self.selected_bg
        } else {
            Color::Reset
        };
        let row_fg = if self.cursor {
            self.selected_fg
        } else if self.enabled {
            self.normal_fg
        } else {
            self.inactive_fg
        };
        let prefix = if self.cursor { "▶ " } else { "  " };
        let mark = if self.enabled { "[x]" } else { "[ ]" };
        let label = truncate_chars(&self.label, area.width.saturating_sub(8) as usize);
        let line = format!("{prefix}{mark}  {label}");
        write_row(
            buf,
            area.x,
            area.y,
            &line,
            Style::default().bg(row_bg).fg(row_fg),
        );
    }
}

fn write_row(buf: &mut Buffer, x: u16, y: u16, s: &str, style: Style) {
    let mut col = x;
    let right = buf.area.right();
    for ch in s.chars() {
        if col >= right {
            break;
        }
        let cell = &mut buf[(col, y)];
        cell.set_char(ch);
        cell.set_style(style);
        col = col.saturating_add(1);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ratatui::buffer::Buffer;

    #[test]
    fn shows_checked_state() {
        let row = ToggleRow::new("cpu", true).with_cursor(true);
        let area = Rect::new(0, 0, 16, 1);
        let mut buf = Buffer::empty(area);
        (&row).render(area, &mut buf);
        assert_eq!(buf[(2, 0)].symbol(), "[");
        assert_eq!(buf[(3, 0)].symbol(), "x");
    }
}
