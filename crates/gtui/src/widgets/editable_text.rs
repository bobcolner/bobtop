//! Read-only render-side of an editable text buffer.
//!
//! The widget handles painting: lines, optional line-number gutter,
//! and a cursor cell highlight at `(row, col)`. It does *not* mutate
//! the buffer or interpret keystrokes — the caller owns that state
//! and feeds the widget a snapshot each frame.
//!
//! For the blinking native cursor, the caller should additionally
//! invoke `Frame::set_cursor_position` with the same screen coords —
//! `cursor_screen_xy()` returns them so the math doesn't drift
//! between widget and caller.

use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::style::{Modifier, Style};
use ratatui::text::Line;
use ratatui::widgets::Widget;
use unicode_width::UnicodeWidthChar;

use crate::Theme;

#[derive(Debug, Clone)]
pub struct EditableText<'a> {
    pub lines: &'a [String],
    /// Optional pre-styled overlay aligned line-for-line with `lines`.
    /// When present, spans drive painting (colors / emphasis); when
    /// empty or shorter than `lines`, the missing rows fall back to
    /// plain rendering using `theme.main_fg`.
    pub styled: &'a [Line<'a>],
    /// Top source-line index shown in the viewport.
    pub scroll_row: usize,
    /// Leftmost source-column shown — the editor scrolls horizontally
    /// for long lines so the cursor stays visible.
    pub scroll_col: usize,
    /// (row, col) in *characters* (not cells). Caller is responsible
    /// for clamping into `lines` length / line widths.
    pub cursor: (usize, usize),
    pub show_line_numbers: bool,
    pub theme: &'a Theme,
}

impl<'a> EditableText<'a> {
    pub fn new(lines: &'a [String], cursor: (usize, usize), theme: &'a Theme) -> Self {
        Self {
            lines,
            styled: &[],
            scroll_row: 0,
            scroll_col: 0,
            cursor,
            show_line_numbers: true,
            theme,
        }
    }

    pub fn with_scroll(mut self, row: usize, col: usize) -> Self {
        self.scroll_row = row;
        self.scroll_col = col;
        self
    }

    pub fn with_line_numbers(mut self, on: bool) -> Self {
        self.show_line_numbers = on;
        self
    }

    /// Attach a pre-styled overlay (e.g. syntect highlights). The
    /// overlay is indexed by source-line, parallel to `lines`. Spans
    /// are painted in their styled colors; cursor math still operates
    /// on the raw `&[String]` so column counting stays in chars.
    pub fn with_styled(mut self, styled: &'a [Line<'a>]) -> Self {
        self.styled = styled;
        self
    }

    /// Width of the line-number gutter for `total_lines` source lines.
    pub fn gutter_width(total: usize) -> u16 {
        let digits = total.max(1).to_string().len() as u16;
        digits + 2
    }

    /// Compute the on-screen `(x, y)` position the cursor renders at,
    /// given an `area` rect. Returns `None` if the cursor is currently
    /// scrolled out of view. Match the public render path so callers
    /// can wire `Frame::set_cursor_position` against the same coords.
    pub fn cursor_screen_xy(&self, area: Rect) -> Option<(u16, u16)> {
        if area.width == 0 || area.height == 0 {
            return None;
        }
        let gutter_w = if self.show_line_numbers {
            Self::gutter_width(self.lines.len())
        } else {
            0
        };
        if gutter_w >= area.width {
            return None;
        }
        let body_x = area.x + gutter_w;
        let body_w = area.width - gutter_w;
        let (cur_row, cur_col) = self.cursor;
        if cur_row < self.scroll_row {
            return None;
        }
        let screen_row = (cur_row - self.scroll_row) as u16;
        if screen_row >= area.height {
            return None;
        }
        let line = self.lines.get(cur_row).map(|s| s.as_str()).unwrap_or("");
        // Convert char col to display cells, accounting for scroll_col.
        let mut cell_col: u16 = 0;
        for (i, ch) in line.chars().enumerate() {
            if i >= cur_col {
                break;
            }
            let cw = UnicodeWidthChar::width(ch).unwrap_or(0) as u16;
            cell_col = cell_col.saturating_add(cw);
        }
        if (cell_col as usize) < self.scroll_col {
            return None;
        }
        let visible_col = cell_col - self.scroll_col as u16;
        if visible_col >= body_w {
            return None;
        }
        Some((body_x + visible_col, area.y + screen_row))
    }
}

impl<'a> Widget for &EditableText<'a> {
    fn render(self, area: Rect, buf: &mut Buffer) {
        if area.width == 0 || area.height == 0 {
            return;
        }
        let gutter_w = if self.show_line_numbers {
            EditableText::gutter_width(self.lines.len())
        } else {
            0
        };
        if gutter_w >= area.width {
            return;
        }
        let body_x = area.x + gutter_w;
        let body_w = area.width - gutter_w;
        let gutter_style = Style::default().fg(self.theme.inactive_fg);
        let cur_row_idx = self.cursor.0;

        let max_rows = area.height as usize;
        let visible = self
            .lines
            .iter()
            .enumerate()
            .skip(self.scroll_row)
            .take(max_rows);
        for (row_in_view, (idx, line)) in visible.enumerate() {
            let y = area.y + row_in_view as u16;
            let is_cursor_row = idx == cur_row_idx;
            if self.show_line_numbers {
                let label = format!("{:>width$} ", idx + 1, width = (gutter_w - 1) as usize);
                let style = if is_cursor_row {
                    gutter_style.add_modifier(Modifier::BOLD)
                } else {
                    gutter_style
                };
                write_plain(buf, area.x, y, &label, gutter_w as usize, style);
            }
            let base_style = Style::default().fg(self.theme.main_fg);
            // Prefer the styled overlay if a Line for this source row is
            // available; otherwise paint the raw chars in `base_style`.
            // `styled` may be a prefix of `lines` (e.g. when the editor
            // skips highlighting for very large buffers) — bounds-check.
            let styled_line = self.styled.get(idx);
            let mut col_cells: u16 = 0;
            let mut x = body_x;
            let right = body_x + body_w;
            if let Some(sl) = styled_line {
                'spans: for span in sl.spans.iter() {
                    // `patch` overlays the span's set fields onto the
                    // base — span colors win when set, base.main_fg
                    // covers any span that didn't specify a color.
                    let style = base_style.patch(span.style);
                    for ch in span.content.chars() {
                        let cw = UnicodeWidthChar::width(ch).unwrap_or(0) as u16;
                        if cw == 0 {
                            continue;
                        }
                        if (col_cells as usize) < self.scroll_col {
                            col_cells = col_cells.saturating_add(cw);
                            continue;
                        }
                        if x.saturating_add(cw) > right {
                            break 'spans;
                        }
                        let cell = &mut buf[(x, y)];
                        cell.set_char(ch);
                        cell.set_style(style);
                        x = x.saturating_add(cw);
                        col_cells = col_cells.saturating_add(cw);
                    }
                }
            } else {
                for ch in line.chars() {
                    let cw = UnicodeWidthChar::width(ch).unwrap_or(0) as u16;
                    if cw == 0 {
                        continue;
                    }
                    if (col_cells as usize) < self.scroll_col {
                        col_cells = col_cells.saturating_add(cw);
                        continue;
                    }
                    if x.saturating_add(cw) > right {
                        break;
                    }
                    let cell = &mut buf[(x, y)];
                    cell.set_char(ch);
                    cell.set_style(base_style);
                    x = x.saturating_add(cw);
                    col_cells = col_cells.saturating_add(cw);
                }
            }
        }
    }
}

fn write_plain(buf: &mut Buffer, x: u16, y: u16, s: &str, max_w: usize, style: Style) {
    let mut col = x;
    let right = x.saturating_add(max_w as u16).min(buf.area.right());
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
        cell.set_style(style);
        col = col.saturating_add(cw);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn theme() -> Theme {
        Theme::fallback()
    }

    #[test]
    fn renders_lines_and_gutter() {
        let theme = theme();
        let lines = vec!["hello".to_string(), "world".to_string()];
        let area = Rect::new(0, 0, 12, 2);
        let mut buf = Buffer::empty(area);
        let w = EditableText::new(&lines, (0, 0), &theme);
        (&w).render(area, &mut buf);
        let row0: String = (0..area.width).map(|x| buf[(x, 0)].symbol().to_string()).collect();
        // 2 lines → 1-digit gutter + 1 padding = 2 cells, plus body.
        assert!(row0.starts_with(" 1 hello"), "row0={row0:?}");
    }

    #[test]
    fn cursor_screen_xy_tracks_chars_to_cells() {
        let theme = theme();
        let lines = vec!["abcdef".to_string()];
        let area = Rect::new(0, 0, 12, 2);
        let w = EditableText::new(&lines, (0, 3), &theme);
        // Gutter is 3 cells (digits=1 + pad=2). Cursor at char col 3 →
        // cell col 3 → screen x = 3 (gutter) + 3 = 6.
        let xy = w.cursor_screen_xy(area).unwrap();
        assert_eq!(xy, (6, 0));
    }

    #[test]
    fn cursor_offscreen_when_scrolled_past() {
        let theme = theme();
        let lines = vec!["abc".to_string(), "def".to_string(), "ghi".to_string()];
        let area = Rect::new(0, 0, 12, 1);
        let w = EditableText::new(&lines, (2, 0), &theme).with_scroll(0, 0);
        // Row 2 is scrolled off (only 1 visible row) → None.
        assert!(w.cursor_screen_xy(area).is_none());
    }

    #[test]
    fn styled_overlay_paints_span_colors() {
        let theme = theme();
        let lines = vec!["abc".to_string()];
        let red = ratatui::style::Color::Rgb(255, 0, 0);
        let blue = ratatui::style::Color::Rgb(0, 0, 255);
        let styled = vec![Line::from(vec![
            ratatui::text::Span::styled("a", Style::default().fg(red)),
            ratatui::text::Span::styled("bc", Style::default().fg(blue)),
        ])];
        let area = Rect::new(0, 0, 12, 1);
        let mut buf = Buffer::empty(area);
        let w = EditableText::new(&lines, (0, 0), &theme).with_styled(&styled);
        (&w).render(area, &mut buf);
        // Gutter is 3 cells (1 digit + 2 padding); body starts at x=3.
        let cell_a = &buf[(3, 0)];
        let cell_b = &buf[(4, 0)];
        let cell_c = &buf[(5, 0)];
        assert_eq!(cell_a.symbol(), "a");
        assert_eq!(cell_a.fg, red, "styled red fg should win over main_fg");
        assert_eq!(cell_b.symbol(), "b");
        assert_eq!(cell_b.fg, blue);
        assert_eq!(cell_c.symbol(), "c");
        assert_eq!(cell_c.fg, blue);
    }

    #[test]
    fn horizontal_scroll_skips_leading_chars() {
        let theme = theme();
        let lines = vec!["0123456789".to_string()];
        // 1 line → gutter 3 (1 digit + 2 padding); body starts at x=3.
        // Width 9, body width 6; scroll col 4 → body shows '4'..'9'.
        let area = Rect::new(0, 0, 9, 1);
        let mut buf = Buffer::empty(area);
        let w = EditableText::new(&lines, (0, 4), &theme).with_scroll(0, 4);
        (&w).render(area, &mut buf);
        let body: String = (3..area.width).map(|x| buf[(x, 0)].symbol().to_string()).collect();
        assert_eq!(body, "456789");
    }
}
