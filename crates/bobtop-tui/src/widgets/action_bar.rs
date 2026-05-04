//! Footer hint row for modal dialogs and settings screens.

use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::style::{Color, Style};
use ratatui::widgets::Widget;

use crate::truncate_chars;

#[derive(Debug, Clone)]
pub struct ActionBar {
    pub actions: Vec<(String, String)>,
    pub cursor: Option<usize>,
    pub border_fg: Color,
    pub key_fg: Color,
    pub text_fg: Color,
    pub selected_bg: Color,
}

impl ActionBar {
    pub fn new(actions: Vec<(String, String)>) -> Self {
        Self {
            actions,
            cursor: None,
            border_fg: Color::Reset,
            key_fg: Color::Reset,
            text_fg: Color::Reset,
            selected_bg: Color::Reset,
        }
    }

    pub fn with_cursor(mut self, cursor: Option<usize>) -> Self {
        self.cursor = cursor;
        self
    }

    pub fn with_colors(
        mut self,
        border_fg: Color,
        key_fg: Color,
        text_fg: Color,
        selected_bg: Color,
    ) -> Self {
        self.border_fg = border_fg;
        self.key_fg = key_fg;
        self.text_fg = text_fg;
        self.selected_bg = selected_bg;
        self
    }
}

impl Widget for &ActionBar {
    fn render(self, area: Rect, buf: &mut Buffer) {
        if area.width == 0 || area.height == 0 {
            return;
        }
        let y = area.y;
        let mut x = area.x;
        let right = area.x + area.width;
        for (i, (key, desc)) in self.actions.iter().enumerate() {
            if x >= right {
                break;
            }
            if i > 0 {
                let sep = "  ";
                for ch in sep.chars() {
                    if x >= right {
                        break;
                    }
                    let cell = &mut buf[(x, y)];
                    cell.set_char(ch);
                    cell.set_style(Style::default().fg(self.border_fg));
                    x = x.saturating_add(1);
                }
            }
            let is_cursor = self.cursor == Some(i);
            let key_text = truncate_chars(key, 20);
            let desc_text = truncate_chars(desc, right.saturating_sub(x + 1) as usize);
            let style = if is_cursor {
                Style::default().bg(self.selected_bg).fg(self.text_fg)
            } else {
                Style::default().fg(self.text_fg)
            };
            for ch in key_text.chars() {
                if x >= right {
                    break;
                }
                let cell = &mut buf[(x, y)];
                cell.set_char(ch);
                cell.set_style(style.fg(self.key_fg));
                x = x.saturating_add(1);
            }
            if x < right {
                let cell = &mut buf[(x, y)];
                cell.set_char(' ');
                cell.set_style(style);
                x = x.saturating_add(1);
            }
            for ch in desc_text.chars() {
                if x >= right {
                    break;
                }
                let cell = &mut buf[(x, y)];
                cell.set_char(ch);
                cell.set_style(style);
                x = x.saturating_add(1);
            }
        }
    }
}
