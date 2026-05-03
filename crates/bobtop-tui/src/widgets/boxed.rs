//! Rounded-corner box with btop-style title slots.
//!
//! Anatomy (matches the screenshots in `Img/`):
//! ```text
//! ╭top_left──────────────────top_right╮
//! │           inner area              │
//! ╰bottom────────────────────────────╯
//! ```
//! - `top_left`: section title (e.g. `¹cpu`), drawn against the top border
//!   right after the corner.
//! - `top_right`: small controls (e.g. `<b br0 n>`, `auto zero`).
//! - `bottom`: keybind hint row (e.g. `+ select  info  terminate`).
//!
//! The widget delegates the actual border drawing to ratatui's `Block` for
//! correctness and adds the inline labels by writing into the buffer after
//! the block has rendered. The inner area returned by `block.inner(area)` is
//! what callers should use to position the inner widget.

use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::style::{Style, Stylize};
use ratatui::widgets::{Block, BorderType, Borders, Widget};

#[derive(Debug, Clone, Default)]
pub struct BoxedPanel {
    pub border_color: ratatui::style::Color,
    pub title_color: ratatui::style::Color,
    pub top_left: Option<String>,
    pub top_right: Option<String>,
    pub bottom: Option<String>,
}

impl BoxedPanel {
    pub fn new(border_color: ratatui::style::Color, title_color: ratatui::style::Color) -> Self {
        Self {
            border_color,
            title_color,
            top_left: None,
            top_right: None,
            bottom: None,
        }
    }

    pub fn with_title(mut self, title: impl Into<String>) -> Self {
        self.top_left = Some(title.into());
        self
    }

    pub fn with_controls(mut self, controls: impl Into<String>) -> Self {
        self.top_right = Some(controls.into());
        self
    }

    pub fn with_keybinds(mut self, hints: impl Into<String>) -> Self {
        self.bottom = Some(hints.into());
        self
    }

    /// The block we delegate border rendering to. Use `block.inner(area)` to
    /// get the area available to the inner content.
    pub fn block(&self) -> Block<'static> {
        Block::default()
            .borders(Borders::ALL)
            .border_type(BorderType::Rounded)
            .border_style(Style::default().fg(self.border_color))
    }

    /// Convenience: returns the inner rect after the border is reserved.
    pub fn inner(&self, area: Rect) -> Rect {
        self.block().inner(area)
    }
}

impl Widget for &BoxedPanel {
    fn render(self, area: Rect, buf: &mut Buffer) {
        if area.width < 2 || area.height < 2 {
            return;
        }
        // Draw the rounded border first.
        self.block().render(area, buf);

        let title_style = Style::default().fg(self.title_color).bold();

        // Top-left: render starting one cell after the left corner.
        if let Some(s) = &self.top_left {
            write_inline(buf, area.left() + 1, area.top(), s, title_style, area.right() - 1);
        }
        // Top-right: render ending one cell before the right corner.
        if let Some(s) = &self.top_right {
            let len = s.chars().count() as u16;
            let right_edge = area.right().saturating_sub(1);
            if len + 1 <= right_edge.saturating_sub(area.left()) {
                let x = right_edge.saturating_sub(len);
                write_inline(buf, x, area.top(), s, title_style, right_edge);
            }
        }
        // Bottom: render starting one cell after the left corner on the bottom border.
        if let Some(s) = &self.bottom {
            let y = area.bottom().saturating_sub(1);
            write_inline(buf, area.left() + 1, y, s, title_style, area.right() - 1);
        }
    }
}

fn write_inline(buf: &mut Buffer, x: u16, y: u16, s: &str, style: Style, right_limit: u16) {
    let mut col = x;
    for ch in s.chars() {
        if col >= right_limit {
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
    use ratatui::style::Color;

    use super::*;

    #[test]
    fn renders_rounded_corners() {
        let panel = BoxedPanel::new(Color::Reset, Color::Reset);
        let area = Rect::new(0, 0, 10, 4);
        let mut buf = Buffer::empty(area);
        (&panel).render(area, &mut buf);
        assert_eq!(buf[(0, 0)].symbol(), "╭");
        assert_eq!(buf[(9, 0)].symbol(), "╮");
        assert_eq!(buf[(0, 3)].symbol(), "╰");
        assert_eq!(buf[(9, 3)].symbol(), "╯");
    }

    #[test]
    fn embeds_title_in_top_border() {
        let panel = BoxedPanel::new(Color::Reset, Color::Reset).with_title("cpu");
        let area = Rect::new(0, 0, 20, 4);
        let mut buf = Buffer::empty(area);
        (&panel).render(area, &mut buf);
        assert_eq!(buf[(1, 0)].symbol(), "c");
        assert_eq!(buf[(2, 0)].symbol(), "p");
        assert_eq!(buf[(3, 0)].symbol(), "u");
    }

    #[test]
    fn embeds_controls_at_top_right() {
        let panel = BoxedPanel::new(Color::Reset, Color::Reset).with_controls("2000ms");
        let area = Rect::new(0, 0, 20, 4);
        let mut buf = Buffer::empty(area);
        (&panel).render(area, &mut buf);
        // "2000ms" right-aligned ends at col 18 (one before right corner at 19).
        assert_eq!(buf[(13, 0)].symbol(), "2");
        assert_eq!(buf[(18, 0)].symbol(), "s");
    }

    #[test]
    fn inner_excludes_border() {
        let panel = BoxedPanel::default();
        let area = Rect::new(0, 0, 10, 5);
        let inner = panel.inner(area);
        assert_eq!(inner, Rect::new(1, 1, 8, 3));
    }
}
