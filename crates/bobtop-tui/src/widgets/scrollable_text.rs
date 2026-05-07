//! Vertically scrollable text viewport.
//!
//! Backs the file-browser preview pane and is sized to also serve as a
//! generic log/output viewer. Content is a `Vec<Line<'static>>` so callers
//! can stream pre-styled output (syntect highlights, markdown spans) in
//! without re-styling at draw time.
//!
//! The widget does not own its scroll state — `scroll_offset` is set by
//! the caller, who also tracks whatever cursor / follow-tail behavior is
//! relevant. Keeping it stateless mirrors `Table` / `DataTable`.

use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::style::{Modifier, Style};
use ratatui::text::Line;
use ratatui::widgets::Widget;
use unicode_width::UnicodeWidthChar;

use crate::Theme;

#[derive(Debug, Clone)]
pub struct ScrollableText<'a> {
    pub lines: &'a [Line<'a>],
    pub scroll_offset: usize,
    pub cursor: Option<usize>,
    pub show_line_numbers: bool,
    pub wrap: bool,
    pub theme: &'a Theme,
}

impl<'a> ScrollableText<'a> {
    pub fn new(lines: &'a [Line<'a>], theme: &'a Theme) -> Self {
        Self {
            lines,
            scroll_offset: 0,
            cursor: None,
            show_line_numbers: false,
            wrap: false,
            theme,
        }
    }

    pub fn with_scroll(mut self, offset: usize) -> Self {
        self.scroll_offset = offset;
        self
    }

    pub fn with_cursor(mut self, cursor: Option<usize>) -> Self {
        self.cursor = cursor;
        self
    }

    pub fn with_line_numbers(mut self, on: bool) -> Self {
        self.show_line_numbers = on;
        self
    }

    pub fn with_wrap(mut self, on: bool) -> Self {
        self.wrap = on;
        self
    }

    /// Width of the line-number gutter for `total_lines`. The `+ 2` is one
    /// space of padding between gutter and text plus a 1-cell margin.
    pub fn gutter_width(total: usize) -> u16 {
        let digits = total.max(1).to_string().len() as u16;
        digits + 2
    }
}

impl<'a> Widget for &ScrollableText<'a> {
    fn render(self, area: Rect, buf: &mut Buffer) {
        if area.width == 0 || area.height == 0 || self.lines.is_empty() {
            return;
        }
        let gutter_w = if self.show_line_numbers {
            ScrollableText::gutter_width(self.lines.len())
        } else {
            0
        };
        if gutter_w >= area.width {
            return;
        }
        let body_x = area.x + gutter_w;
        let body_w = area.width - gutter_w;
        let gutter_style = Style::default().fg(self.theme.inactive_fg);
        let cursor_bg = self.theme.selected_bg;
        let cursor_fg = self.theme.selected_fg;

        // Walk the source lines starting at scroll_offset and emit at most
        // `area.height` visual rows. With wrap=false a source line maps 1:1
        // to a row; with wrap=true a source line may produce N rows.
        let mut row = 0u16;
        let max_rows = area.height;
        let mut idx = self.scroll_offset;
        while row < max_rows && idx < self.lines.len() {
            let y = area.y + row;
            let is_cursor = self.cursor == Some(idx);
            if is_cursor {
                for x in area.x..area.x + area.width {
                    buf[(x, y)].set_style(Style::default().bg(cursor_bg));
                }
            }
            if self.show_line_numbers {
                let label = format!("{:>width$} ", idx + 1, width = (gutter_w - 1) as usize);
                let style = if is_cursor {
                    gutter_style.bg(cursor_bg).add_modifier(Modifier::BOLD)
                } else {
                    gutter_style
                };
                write_plain(buf, area.x, y, &label, gutter_w as usize, style);
            }
            let base_style = if is_cursor {
                Style::default().bg(cursor_bg).fg(cursor_fg)
            } else {
                Style::default().fg(self.theme.main_fg)
            };
            if self.wrap {
                let consumed = render_line_wrapped(
                    buf,
                    body_x,
                    y,
                    body_w,
                    max_rows - row,
                    &self.lines[idx],
                    base_style,
                );
                row += consumed.max(1);
            } else {
                render_line_clipped(buf, body_x, y, body_w, &self.lines[idx], base_style);
                row += 1;
            }
            idx += 1;
        }
    }
}

/// Draw a single source line into `(x, y)..(x+max_w, y)`, clipping any
/// content past the right edge.
fn render_line_clipped(
    buf: &mut Buffer,
    x: u16,
    y: u16,
    max_w: u16,
    line: &Line<'_>,
    base_style: Style,
) {
    let mut col = x;
    let right = x.saturating_add(max_w);
    for span in &line.spans {
        if col >= right {
            break;
        }
        let style = base_style.patch(span.style);
        col = write_span(buf, col, y, right, span.content.as_ref(), style);
    }
}

/// Draw a source line wrapping at the right edge. Emits at most
/// `max_rows` rows; returns the number of rows actually consumed.
/// Wrapping is character-width aware (CJK, emoji) but not word-aware —
/// this is preview/log content, not paragraph text.
fn render_line_wrapped(
    buf: &mut Buffer,
    x: u16,
    y: u16,
    max_w: u16,
    max_rows: u16,
    line: &Line<'_>,
    base_style: Style,
) -> u16 {
    if max_rows == 0 || max_w == 0 {
        return 0;
    }
    let mut row: u16 = 0;
    let mut col = x;
    let right = x.saturating_add(max_w).min(buf.area.right());
    for span in &line.spans {
        if row >= max_rows {
            break;
        }
        let style = base_style.patch(span.style);
        for ch in span.content.chars() {
            let cw = UnicodeWidthChar::width(ch).unwrap_or(0) as u16;
            if cw == 0 {
                continue;
            }
            if col.saturating_add(cw) > right {
                row += 1;
                if row >= max_rows {
                    return row;
                }
                col = x;
            }
            let cell = &mut buf[(col, y + row)];
            cell.set_char(ch);
            cell.set_style(cell.style().patch(style));
            col += cw;
        }
    }
    row + 1
}

/// Write `s` into the buffer starting at `x`, advancing per-cell display
/// width and respecting `right`. Returns the next column.
fn write_span(buf: &mut Buffer, x: u16, y: u16, right: u16, s: &str, style: Style) -> u16 {
    let mut col = x;
    for ch in s.chars() {
        let cw = UnicodeWidthChar::width(ch).unwrap_or(0) as u16;
        if cw == 0 {
            continue;
        }
        if col.saturating_add(cw) > right {
            break;
        }
        let cell = &mut buf[(col, y)];
        cell.set_char(ch);
        cell.set_style(cell.style().patch(style));
        col += cw;
    }
    col
}

fn write_plain(buf: &mut Buffer, x: u16, y: u16, s: &str, max_w: usize, style: Style) {
    let right = x.saturating_add(max_w as u16).min(buf.area.right());
    write_span(buf, x, y, right, s, style);
}

#[cfg(test)]
mod tests {
    use super::*;
    use ratatui::style::Color;
    use ratatui::text::Span;

    fn theme() -> Theme {
        Theme::fallback()
    }

    fn raw_lines(strs: &[&str]) -> Vec<Line<'static>> {
        strs.iter().map(|s| Line::from(s.to_string())).collect()
    }

    #[test]
    fn scroll_offset_skips_leading_lines() {
        let theme = theme();
        let lines = raw_lines(&["alpha", "beta", "gamma", "delta"]);
        let area = Rect::new(0, 0, 10, 2);
        let mut buf = Buffer::empty(area);
        let w = ScrollableText::new(&lines, &theme).with_scroll(2);
        (&w).render(area, &mut buf);
        let row0: String = (0..area.width).map(|x| buf[(x, 0)].symbol().to_string()).collect();
        let row1: String = (0..area.width).map(|x| buf[(x, 1)].symbol().to_string()).collect();
        assert!(row0.starts_with("gamma"), "row0={row0:?}");
        assert!(row1.starts_with("delta"), "row1={row1:?}");
    }

    #[test]
    fn cursor_row_paints_full_width_highlight() {
        let theme = theme();
        let lines = raw_lines(&["a", "b", "c"]);
        let area = Rect::new(0, 0, 6, 3);
        let mut buf = Buffer::empty(area);
        let w = ScrollableText::new(&lines, &theme).with_cursor(Some(1));
        (&w).render(area, &mut buf);
        for x in 0..area.width {
            assert_eq!(buf[(x, 1)].style().bg, Some(theme.selected_bg), "selected col {x}");
            assert_ne!(buf[(x, 0)].style().bg, Some(theme.selected_bg));
            assert_ne!(buf[(x, 2)].style().bg, Some(theme.selected_bg));
        }
    }

    #[test]
    fn line_numbers_respect_total_width() {
        // 12 lines → 2-digit gutter + 1 padding = 3 cells.
        let theme = theme();
        let lines: Vec<Line<'static>> =
            (0..12).map(|i| Line::from(format!("L{i}"))).collect();
        let area = Rect::new(0, 0, 8, 3);
        let mut buf = Buffer::empty(area);
        let w = ScrollableText::new(&lines, &theme).with_line_numbers(true);
        (&w).render(area, &mut buf);
        let row0: String = (0..area.width).map(|x| buf[(x, 0)].symbol().to_string()).collect();
        assert!(row0.starts_with(" 1 L0") || row0.starts_with("  1 L0"), "row0={row0:?}");
    }

    #[test]
    fn wrap_emits_multiple_visual_rows_for_one_source_line() {
        let theme = theme();
        let lines = vec![Line::from("abcdef")];
        // Width 3 → "abc", "def" on two rows.
        let area = Rect::new(0, 0, 3, 2);
        let mut buf = Buffer::empty(area);
        let w = ScrollableText::new(&lines, &theme).with_wrap(true);
        (&w).render(area, &mut buf);
        let row0: String = (0..area.width).map(|x| buf[(x, 0)].symbol().to_string()).collect();
        let row1: String = (0..area.width).map(|x| buf[(x, 1)].symbol().to_string()).collect();
        assert_eq!(row0, "abc");
        assert_eq!(row1, "def");
    }

    #[test]
    fn no_wrap_clips_long_lines() {
        let theme = theme();
        let lines = vec![Line::from("abcdef")];
        let area = Rect::new(0, 0, 3, 2);
        let mut buf = Buffer::empty(area);
        let w = ScrollableText::new(&lines, &theme);
        (&w).render(area, &mut buf);
        let row0: String = (0..area.width).map(|x| buf[(x, 0)].symbol().to_string()).collect();
        let row1: String = (0..area.width).map(|x| buf[(x, 1)].symbol().to_string()).collect();
        assert_eq!(row0, "abc");
        // Row 1 should be empty (next source line, but there isn't one).
        assert!(row1.trim().is_empty(), "row1={row1:?}");
    }

    #[test]
    fn span_styles_applied() {
        let theme = theme();
        let lines = vec![Line::from(vec![
            Span::styled("red", Style::default().fg(Color::Red)),
            Span::raw(" plain"),
        ])];
        let area = Rect::new(0, 0, 12, 1);
        let mut buf = Buffer::empty(area);
        let w = ScrollableText::new(&lines, &theme);
        (&w).render(area, &mut buf);
        assert_eq!(buf[(0, 0)].style().fg, Some(Color::Red));
        assert_eq!(buf[(1, 0)].style().fg, Some(Color::Red));
        assert_eq!(buf[(2, 0)].style().fg, Some(Color::Red));
        // The space span uses base (theme.main_fg).
        assert_eq!(buf[(3, 0)].style().fg, Some(theme.main_fg));
    }
}
