//! Simple centered section header with a horizontal divider.

use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::style::{Color, Style};
use ratatui::widgets::Widget;

use crate::truncate_chars;

#[derive(Debug, Clone)]
pub struct SectionHeader {
    pub title: String,
    pub border_fg: Color,
    pub title_fg: Color,
}

impl SectionHeader {
    pub fn new(title: impl Into<String>) -> Self {
        Self {
            title: title.into(),
            border_fg: Color::Reset,
            title_fg: Color::Reset,
        }
    }

    pub fn with_colors(mut self, border_fg: Color, title_fg: Color) -> Self {
        self.border_fg = border_fg;
        self.title_fg = title_fg;
        self
    }
}

impl Widget for &SectionHeader {
    fn render(self, area: Rect, buf: &mut Buffer) {
        if area.width == 0 || area.height == 0 {
            return;
        }
        let y = area.y;
        let mut x = area.x;
        let right = area.x + area.width;
        while x < right {
            let cell = &mut buf[(x, y)];
            cell.set_char('─');
            cell.set_style(Style::default().fg(self.border_fg));
            x = x.saturating_add(1);
        }
        let title = truncate_chars(&self.title, area.width.saturating_sub(4) as usize);
        let title_w = title.chars().count() as u16;
        if title_w == 0 || area.width < title_w + 2 {
            return;
        }
        let start = area.x + (area.width - title_w) / 2;
        for (i, ch) in title.chars().enumerate() {
            let cell = &mut buf[(start + i as u16, y)];
            cell.set_char(ch);
            cell.set_style(Style::default().fg(self.title_fg));
        }
    }
}
