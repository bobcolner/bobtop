//! Simple selectable key/value list for modal settings screens.

use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::style::{Color, Style};
use ratatui::widgets::Widget;

use crate::truncate_chars;

#[derive(Debug, Clone)]
pub struct SelectableList {
    pub rows: Vec<(String, String)>,
    pub cursor: usize,
    pub label_width: u16,
    pub selected_bg: Color,
    pub selected_fg: Color,
    pub normal_fg: Color,
}

impl SelectableList {
    pub fn new(rows: Vec<(String, String)>) -> Self {
        let label_width = rows
            .iter()
            .map(|(k, _)| k.chars().count())
            .max()
            .unwrap_or(8) as u16;
        Self {
            rows,
            cursor: 0,
            label_width,
            selected_bg: Color::Reset,
            selected_fg: Color::Reset,
            normal_fg: Color::Reset,
        }
    }

    pub fn with_cursor(mut self, cursor: usize) -> Self {
        self.cursor = cursor;
        self
    }

    pub fn with_colors(
        mut self,
        selected_bg: Color,
        selected_fg: Color,
        normal_fg: Color,
    ) -> Self {
        self.selected_bg = selected_bg;
        self.selected_fg = selected_fg;
        self.normal_fg = normal_fg;
        self
    }
}

impl Widget for &SelectableList {
    fn render(self, area: Rect, buf: &mut Buffer) {
        if area.width == 0 || area.height == 0 {
            return;
        }
        let max_rows = area.height as usize;
        for (i, (label, value)) in self.rows.iter().take(max_rows).enumerate() {
            let row_y = area.y + i as u16;
            let is_cursor = i == self.cursor;
            if is_cursor {
                for xx in area.x..area.x + area.width {
                    buf[(xx, row_y)].set_style(Style::default().bg(self.selected_bg));
                }
            }
            let prefix = if is_cursor { "▶ " } else { "  " };
            let row_bg = if is_cursor {
                self.selected_bg
            } else {
                Color::Reset
            };
            let row_fg = if is_cursor {
                self.selected_fg
            } else {
                self.normal_fg
            };
            let label = truncate_chars(label, self.label_width as usize);
            let line = format!("{prefix}{:<width$}  ◀ {value} ▶", label, width = self.label_width as usize);
            write_row(
                buf,
                area.x,
                row_y,
                &line,
                Style::default().bg(row_bg).fg(row_fg),
            );
        }
    }
}

fn write_row(
    buf: &mut Buffer,
    x: u16,
    y: u16,
    s: &str,
    style: Style,
) {
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
    fn selected_row_gets_cursor_marker() {
        let rows = vec![
            ("theme".to_string(), "dark".to_string()),
            ("tick_ms".to_string(), "500 ms".to_string()),
        ];
        let list = SelectableList::new(rows).with_cursor(1);
        let area = Rect::new(0, 0, 32, 2);
        let mut buf = Buffer::empty(area);
        (&list).render(area, &mut buf);
        assert_eq!(buf[(0, 1)].symbol(), "▶");
        assert_eq!(buf[(2, 1)].symbol(), "t");
    }
}
