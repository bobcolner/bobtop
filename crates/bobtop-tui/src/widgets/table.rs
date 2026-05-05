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
        let cw = UnicodeWidthChar::width(ch).unwrap_or(0).max(1) as u16;
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
