use std::borrow::Cow;

use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::style::{Modifier, Style};
use ratatui::widgets::Widget;

use crate::{format_bytes, format_rate, Theme};

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

#[derive(Debug, Clone)]
struct ColSpec {
    title: String,
    width: u16,
    right_align: bool,
}

impl<'a> Widget for &Table<'a> {
    fn render(self, area: Rect, buf: &mut Buffer) {
        if area.width == 0 || area.height == 0 || self.columns.is_empty() {
            return;
        }

        let cols: Vec<ColSpec> = self
            .columns
            .iter()
            .map(|c| ColSpec {
                title: c.title.to_string(),
                width: c.width,
                right_align: c.right_align,
            })
            .collect();

        let header_style = Style::default().fg(self.theme.title).add_modifier(Modifier::BOLD);
        let active_style = Style::default()
            .fg(self.theme.hi_fg)
            .add_modifier(Modifier::BOLD | Modifier::REVERSED);
        let arrow = if self.sort_descending { '↓' } else { '↑' };
        render_row(buf, area.x, area.y, area.width, &cols, |idx| {
            let c = &cols[idx];
            let is_active = self.sort_column == Some(idx);
            let title = if is_active {
                format!("{}{}", c.title, arrow)
            } else {
                c.title.clone()
            };
            let style = if is_active { active_style } else { header_style };
            (title, style, c.right_align)
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
            let mut cells: Vec<String> = row.cells.iter().map(|c| c.text.to_string()).collect();
            let cell_styles: Vec<Style> = if is_selected {
                vec![style; row.cells.len()]
            } else {
                row.cells.iter().map(|c| style.patch(c.style)).collect()
            };

            if let RowKind::Header { label, label_col } = &row.kind {
                if *label_col < cells.len() {
                    cells[*label_col] = label.to_string();
                }
            }

            render_row(buf, area.x, y, area.width, &cols, |idx| {
                let s = cells.get(idx).cloned().unwrap_or_default();
                let st = cell_styles.get(idx).cloned().unwrap_or(style);
                (s, st, cols[idx].right_align)
            });
        }
    }
}

fn render_row<F>(buf: &mut Buffer, x0: u16, y: u16, total_width: u16, cols: &[ColSpec], mut cell_fn: F)
where
    F: FnMut(usize) -> (String, Style, bool),
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
        let len = text.chars().count() as u16;
        let (text_x, text) = if right_align && len < avail {
            (cursor + (avail - len), text)
        } else if len > avail {
            (cursor, truncate(&text, avail as usize))
        } else {
            (cursor, text)
        };
        write_str(buf, text_x, y, &text, avail as usize, style);
        cursor = cursor.saturating_add(col_width).saturating_add(1);
    }
}

fn write_str(buf: &mut Buffer, x: u16, y: u16, s: &str, max_cols: usize, style: Style) {
    let mut col = x;
    let right = x.saturating_add(max_cols as u16).min(buf.area.right());
    for ch in s.chars() {
        if col >= right {
            break;
        }
        let c = &mut buf[(col, y)];
        c.set_char(ch);
        c.set_style(c.style().patch(style));
        col = col.saturating_add(1);
    }
}

fn truncate(s: &str, max_chars: usize) -> String {
    if s.chars().count() <= max_chars {
        s.to_string()
    } else if max_chars == 0 {
        String::new()
    } else {
        let mut out: String = s.chars().take(max_chars.saturating_sub(1)).collect();
        out.push('…');
        out
    }
}

impl<'a> Cell<'a> {
    pub fn bytes(b: u64) -> Self {
        Self::new(format_bytes(b))
    }

    pub fn rate(v: Option<f64>) -> Self {
        Self::new(match v {
            Some(r) if r > 0.5 => format_rate(r),
            Some(_) => "0".into(),
            None => "-".into(),
        })
    }
}
