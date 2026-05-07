use std::borrow::Cow;

use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::style::{Modifier, Style};
use ratatui::widgets::Widget;

use crate::text::display_width;
use crate::{format_bytes_compact, format_rate, Theme};
use unicode_width::UnicodeWidthChar;

#[derive(Debug, Clone)]
pub struct Column<'a> {
    pub title: Cow<'a, str>,
    pub width: u16,
    pub right_align: bool,
}

impl<'a> Column<'a> {
    pub fn new(title: impl Into<Cow<'a, str>>, width: u16) -> Self {
        Self {
            title: title.into(),
            width,
            right_align: false,
        }
    }

    pub fn right_aligned(mut self, right_align: bool) -> Self {
        self.right_align = right_align;
        self
    }
}

#[derive(Debug, Clone)]
pub struct Cell<'a> {
    pub text: Cow<'a, str>,
    pub style: Style,
}

impl<'a> Cell<'a> {
    pub fn new(text: impl Into<Cow<'a, str>>) -> Self {
        Self {
            text: text.into(),
            style: Style::default(),
        }
    }

    pub fn with_style(mut self, style: Style) -> Self {
        self.style = style;
        self
    }
}

#[derive(Debug, Clone)]
pub enum RowKind<'a> {
    Data,
    Header {
        label: Cow<'a, str>,
        label_col: usize,
    },
}

#[derive(Debug, Clone)]
pub struct Row<'a> {
    pub cells: Vec<Cell<'a>>,
    pub kind: RowKind<'a>,
    pub style: Style,
}

impl<'a> Row<'a> {
    pub fn data(cells: Vec<Cell<'a>>) -> Self {
        Self {
            cells,
            kind: RowKind::Data,
            style: Style::default(),
        }
    }

    pub fn header(label: impl Into<Cow<'a, str>>, label_col: usize, cells: Vec<Cell<'a>>) -> Self {
        Self {
            cells,
            kind: RowKind::Header {
                label: label.into(),
                label_col,
            },
            style: Style::default(),
        }
    }

    pub fn with_style(mut self, style: Style) -> Self {
        self.style = style;
        self
    }
}

#[derive(Debug, Clone)]
pub struct Table<'a> {
    pub columns: &'a [Column<'a>],
    pub rows: &'a [Row<'a>],
    pub selected: Option<usize>,
    pub scroll_offset: usize,
    pub sort_column: Option<usize>,
    pub sort_descending: bool,
    pub theme: &'a Theme,
}

impl<'a> Table<'a> {
    pub fn new(columns: &'a [Column<'a>], rows: &'a [Row<'a>], theme: &'a Theme) -> Self {
        Self {
            columns,
            rows,
            selected: None,
            scroll_offset: 0,
            sort_column: None,
            sort_descending: true,
            theme,
        }
    }

    pub fn with_selection(mut self, selected: Option<usize>, scroll_offset: usize) -> Self {
        self.selected = selected;
        self.scroll_offset = scroll_offset;
        self
    }

    pub fn with_sort(mut self, column: Option<usize>, descending: bool) -> Self {
        self.sort_column = column;
        self.sort_descending = descending;
        self
    }
}

impl<'a> Widget for &Table<'a> {
    fn render(self, area: Rect, buf: &mut Buffer) {
        if area.width == 0 || area.height == 0 || self.columns.is_empty() {
            return;
        }

        let header_style = Style::default().fg(self.theme.title).add_modifier(Modifier::BOLD);
        let active_style = Style::default()
            .fg(self.theme.hi_fg)
            .add_modifier(Modifier::BOLD | Modifier::REVERSED);
        let arrow = if self.sort_descending { '↓' } else { '↑' };
        // Pre-build the decorated title for the active sort column (if any).
        // Saves a per-row format!() inside the cell_fn callback below.
        let active_title: Option<String> = self.sort_column.and_then(|idx| {
            self.columns
                .get(idx)
                .map(|c| format!("{}{}", c.title, arrow))
        });
        render_row(buf, area.x, area.y, area.width, self.columns, |idx| {
            let c = &self.columns[idx];
            let is_active = self.sort_column == Some(idx);
            let text: &str = if is_active {
                active_title.as_deref().unwrap_or(&c.title)
            } else {
                &c.title
            };
            let style = if is_active { active_style } else { header_style };
            (text, style, c.right_align)
        });

        let body_top = area.y + 1;
        let body_h = area.height.saturating_sub(1) as usize;
        let visible = self.rows.iter().enumerate().skip(self.scroll_offset).take(body_h);
        for (i, (row_idx, row)) in visible.enumerate() {
            let y = body_top + i as u16;
            let is_selected = self.selected == Some(row_idx);
            if is_selected {
                for x in area.x..area.x + area.width {
                    buf[(x, y)].set_style(Style::default().bg(self.theme.selected_bg));
                }
            }
            let style = if is_selected {
                Style::default()
                    .bg(self.theme.selected_bg)
                    .fg(self.theme.selected_fg)
            } else {
                row.style
            };
            // Header rows override one column's text with a label string.
            // Pull it out once instead of building a Vec<String> shadow.
            let header_label: Option<(usize, &str)> = match &row.kind {
                RowKind::Header { label, label_col } => Some((*label_col, label.as_ref())),
                RowKind::Data => None,
            };

            render_row(buf, area.x, y, area.width, self.columns, |idx| {
                let text: &str = header_label
                    .filter(|(col, _)| *col == idx)
                    .map(|(_, s)| s)
                    .or_else(|| row.cells.get(idx).map(|c| c.text.as_ref()))
                    .unwrap_or("");
                let cell_style = if is_selected {
                    style
                } else {
                    row.cells
                        .get(idx)
                        .map(|c| style.patch(c.style))
                        .unwrap_or(style)
                };
                (text, cell_style, self.columns[idx].right_align)
            });
        }
    }
}

fn render_row<'a, F>(
    buf: &mut Buffer,
    x0: u16,
    y: u16,
    total_width: u16,
    cols: &[Column<'a>],
    mut cell_fn: F,
) where
    F: FnMut(usize) -> (&'a str, Style, bool),
{
    if total_width == 0 || cols.is_empty() {
        return;
    }
    let right_limit = x0.saturating_add(total_width);
    let n = cols.len() as u16;
    let fixed_total: u16 = cols
        .iter()
        .filter(|c| c.width != u16::MAX)
        .map(|c| c.width)
        .sum::<u16>()
        .saturating_add(n.saturating_sub(1));
    let flex_remaining = total_width.saturating_sub(fixed_total);

    let mut cursor = x0;
    for (i, col) in cols.iter().enumerate() {
        if cursor >= right_limit {
            break;
        }
        let col_width = if col.width == u16::MAX { flex_remaining } else { col.width };
        let avail = right_limit.saturating_sub(cursor).min(col_width);
        if avail == 0 {
            break;
        }
        let (text, style, right_align) = cell_fn(i);
        // Layout math is in *terminal cells*, not chars: CJK/emoji glyphs
        // claim two cells. Counting chars instead made cells overrun their
        // gutter for any user with non-Latin process names.
        let len = display_width(text) as u16;
        let text_x = if right_align && len < avail {
            cursor + (avail - len)
        } else {
            cursor
        };
        let drawn: std::borrow::Cow<'_, str> = if len > avail {
            std::borrow::Cow::Owned(truncate(text, avail as usize))
        } else {
            std::borrow::Cow::Borrowed(text)
        };
        write_str(buf, text_x, y, &drawn, avail as usize, style);
        cursor = cursor.saturating_add(col_width).saturating_add(1);
    }
}

fn write_str(buf: &mut Buffer, x: u16, y: u16, s: &str, max_cols: usize, style: Style) {
    let mut col = x;
    let right = x.saturating_add(max_cols as u16).min(buf.area.right());
    for ch in s.chars() {
        // Control chars (tab, newline, ESC, …) and combining marks have display
        // width 0.  Skip them entirely so the renderer stays consistent with
        // display_width() used for column layout — writing a tab via set_char
        // produces a terminal cursor jump that overflows the column gutter.
        let cw = UnicodeWidthChar::width(ch).unwrap_or(0) as u16;
        if cw == 0 {
            continue;
        }
        if col.saturating_add(cw) > right {
            break;
        }
        let c = &mut buf[(col, y)];
        c.set_char(ch);
        c.set_style(c.style().patch(style));
        col = col.saturating_add(cw);
    }
}

/// Truncate `s` to at most `max_cells` *terminal cells*, appending `…` when
/// shortened. Reserves 1 cell for the ellipsis. Caller has already verified
/// `display_width(s) > max_cells`, so this always allocates.
fn truncate(s: &str, max_cells: usize) -> String {
    if max_cells == 0 {
        return String::new();
    }
    // Reserve one cell for the ellipsis (it's a 1-cell glyph in every font
    // we care about; UnicodeWidthChar agrees).
    let budget = max_cells.saturating_sub(1);
    let mut out = String::with_capacity(s.len());
    let mut w = 0usize;
    for ch in s.chars() {
        let cw = UnicodeWidthChar::width(ch).unwrap_or(0);
        if w + cw > budget {
            break;
        }
        out.push(ch);
        w += cw;
    }
    out.push('…');
    out
}

impl<'a> Cell<'a> {
    pub fn bytes(b: u64) -> Self {
        Self::new(format_bytes_compact(b))
    }

    pub fn rate(v: Option<f64>) -> Self {
        Self::new(match v {
            Some(r) if r > 0.5 => format_rate(r),
            Some(_) => "0".into(),
            None => "-".into(),
        })
    }
}

#[cfg(test)]
mod tests {
    use ratatui::buffer::Buffer;
    use ratatui::layout::Rect;
    use ratatui::style::Color;
    use ratatui::widgets::Widget;

    use crate::Theme;

    use super::*;

    fn make_row(label: &'static str) -> Row<'static> {
        Row::data(vec![Cell::new(label)]).with_style(Style::default().fg(Color::White))
    }

    fn two_col_theme() -> Theme {
        Theme::fallback()
    }

    /// The selected row — and only the selected row — must carry
    /// `selected_bg` across its full width after rendering.
    #[test]
    fn selected_row_gets_full_width_highlight() {
        let theme = two_col_theme();
        let cols = vec![Column::new("Name", u16::MAX)];
        let rows = vec![make_row("alpha"), make_row("beta"), make_row("gamma")];
        let area = Rect::new(0, 0, 40, 4);
        let mut buf = Buffer::empty(area);
        let table = Table::new(&cols, &rows, &theme).with_selection(Some(1), 0);
        (&table).render(area, &mut buf);
        // Row 0 of area = header; row 1 = rows[0]; row 2 = rows[1] (SELECTED); row 3 = rows[2].
        for x in 0..area.width {
            assert_eq!(buf[(x, 2)].style().bg, Some(theme.selected_bg), "selected row col {x}");
            assert_ne!(buf[(x, 1)].style().bg, Some(theme.selected_bg), "row above must not be highlighted col {x}");
            assert_ne!(buf[(x, 3)].style().bg, Some(theme.selected_bg), "row below must not be highlighted col {x}");
        }
    }

    /// When the selection index is scrolled off-screen, no row should carry
    /// `selected_bg` (the selected row is outside the visible window).
    #[test]
    fn offscreen_selection_leaves_no_highlight() {
        let theme = two_col_theme();
        let cols = vec![Column::new("Name", u16::MAX)];
        let rows = vec![make_row("a"), make_row("b"), make_row("c"), make_row("d"), make_row("e")];
        // body_h = 2 (area.height=3, minus 1 header).  scroll_offset=2 shows rows[2..4].
        // selected=0 is above the viewport.
        let area = Rect::new(0, 0, 20, 3);
        let mut buf = Buffer::empty(area);
        let table = Table::new(&cols, &rows, &theme).with_selection(Some(0), 2);
        (&table).render(area, &mut buf);
        for y in 1..area.height {
            for x in 0..area.width {
                assert_ne!(buf[(x, y)].style().bg, Some(theme.selected_bg), "y={y} x={x} should not be highlighted");
            }
        }
    }

    /// Control characters in cell text must not be written to the buffer
    /// (they'd cause terminal cursor jumps that break column alignment).
    #[test]
    fn control_chars_skipped_in_write_str() {
        let theme = two_col_theme();
        let cols = vec![Column::new("Cmd", u16::MAX)];
        let rows = vec![Row::data(vec![Cell::new("ab\tc")])
            .with_style(Style::default().fg(Color::White))];
        let area = Rect::new(0, 0, 20, 2);
        let mut buf = Buffer::empty(area);
        let table = Table::new(&cols, &rows, &theme);
        (&table).render(area, &mut buf);
        // Only 'a', 'b', 'c' should appear; the tab must be silently skipped.
        let row: String = (0..area.width).map(|x| buf[(x, 1)].symbol().to_string()).collect();
        assert!(row.contains('a') && row.contains('b') && row.contains('c'),
            "printable chars missing from row: {row:?}");
        assert!(!row.contains('\t'), "tab written to buffer: {row:?}");
    }
}
