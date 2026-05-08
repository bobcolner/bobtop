//! Generic, sortable table widget — the marquee component of the bobtop
//! TUI toolkit. Powers the system-monitor process table today and is the
//! intended substrate for DB query results, portfolio holdings, k8s pod
//! lists, log viewers, and any other "dense, structured, live-updating
//! tabular data" use case in the suite.
//!
//! Apps describe their table by:
//!
//! 1. Defining a column-id type (`Cid`) — usually an enum.
//! 2. Building a `Vec<ColumnDef<Cid>>` describing layout and sortability.
//! 3. Implementing [`TableRowExt`] on the row payload so `cell(col)`
//!    yields a [`Cell`] (text + optional logical color before fade).
//! 4. Implementing [`GroupAggregate`] on the header payload if the table
//!    needs collapsible groups; otherwise passing `()` and never emitting
//!    `TableEntry::Header`.
//!
//! The widget owns: row fading by visible position, selection highlight,
//! sort indicator on the active column, group chevron + tree branch
//! glyphs in the configured `label_column`. State (selection, scroll,
//! sort) stays with the caller — the widget is render-only.

use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::style::{Color, Modifier, Style};
use ratatui::widgets::Widget;
use unicode_width::UnicodeWidthChar;

use crate::text;
use crate::Theme;

/// Vertical-position fade — bottom-of-screen rows lerp toward
/// `inactive_fg` by this fraction. btop applies a similar darkening
/// pass for visual depth.
const FADE_END: f32 = 0.55;

/// One rendered cell. `fg` is the *logical* foreground before fade —
/// the widget will lerp it toward `theme.inactive_fg` based on the
/// row's visible position. Use `Cell::plain` to inherit the table's
/// default foreground (`theme.main_fg`); use `Cell::styled` when the
/// app already knows the cell's color (e.g. a metric sampled through
/// a gradient).
#[derive(Debug, Clone)]
pub struct Cell {
    pub text: String,
    pub fg: Option<Color>,
}

impl Cell {
    pub fn plain(text: impl Into<String>) -> Self {
        Self { text: text.into(), fg: None }
    }

    pub fn styled(text: impl Into<String>, fg: Color) -> Self {
        Self { text: text.into(), fg: Some(fg) }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Align {
    Left,
    Right,
}

#[derive(Debug, Clone, Copy)]
pub enum WidthSpec {
    Fixed(u16),
    /// Soak up leftover horizontal space after fixed columns + gutters.
    /// At most one `Flex` column is expected per layout; if multiple are
    /// declared the first gets the slack and others render at width 0.
    Flex,
}

#[derive(Debug, Clone)]
pub struct ColumnDef<Cid> {
    pub id: Cid,
    pub label: &'static str,
    pub width: WidthSpec,
    pub align: Align,
    /// Whether this column participates in sort cycling. The widget only
    /// reads this for the header indicator — cycling order itself is the
    /// caller's responsibility.
    pub sortable: bool,
}

/// Row payload contract. The widget calls [`cell`](Self::cell) once per
/// visible column to get text + optional logical color, plus the tree
/// hooks when [`LiveTable::draws_tree_glyphs`] is set.
pub trait TableRowExt<Cid> {
    fn cell(&self, col: Cid) -> Cell;

    /// 0 = top-level. 1+ = nested under a group header or tree parent.
    fn tree_depth(&self) -> u8 {
        0
    }

    /// Per-ancestor-depth, true if that ancestor has a continuing sibling
    /// (drives `│` vs space at column d). Length should equal `tree_depth`.
    fn ancestor_continues(&self) -> &[bool] {
        &[]
    }

    /// True when this row is the last sibling at its depth (drives
    /// `└` vs `├`).
    fn is_last_sibling(&self) -> bool {
        false
    }
}

/// Group-header contract. Provides a label (rendered with a chevron in
/// the configured `label_column`), cell aggregates for non-label
/// columns, and the expanded state.
pub trait GroupAggregate<Cid> {
    fn label(&self) -> &str;
    fn cell(&self, col: Cid) -> Cell;
    fn expanded(&self) -> bool;
}

/// Either a group header (with aggregate cells) or a regular item row.
#[derive(Debug, Clone)]
pub enum TableEntry<R, G> {
    Header(G),
    Item(R),
}

/// A `()` impl so apps that never emit headers can pass `TableEntry<R, ()>`.
impl<Cid> GroupAggregate<Cid> for () {
    fn label(&self) -> &str {
        ""
    }
    fn cell(&self, _col: Cid) -> Cell {
        Cell::plain("")
    }
    fn expanded(&self) -> bool {
        true
    }
}

/// Stateless render widget. State (selection, scroll, sort) is owned by
/// the caller and passed in each frame.
pub struct LiveTable<'a, R, G, Cid: Copy + PartialEq> {
    pub rows: &'a [TableEntry<R, G>],
    pub columns: &'a [ColumnDef<Cid>],
    pub theme: &'a Theme,
    pub selected: Option<usize>,
    pub scroll_offset: usize,
    pub sort: Option<Cid>,
    pub sort_descending: bool,
    /// Which column hosts the group chevron (`▼ name`) and the tree
    /// branch glyphs (`├ │ └`). Usually the leftmost text column.
    pub label_column: Cid,
    pub draws_tree_glyphs: bool,
}

impl<'a, R, G, Cid> LiveTable<'a, R, G, Cid>
where
    R: TableRowExt<Cid>,
    G: GroupAggregate<Cid>,
    Cid: Copy + PartialEq,
{
    pub fn new(rows: &'a [TableEntry<R, G>], columns: &'a [ColumnDef<Cid>], theme: &'a Theme, label_column: Cid) -> Self {
        Self {
            rows,
            columns,
            theme,
            selected: None,
            scroll_offset: 0,
            sort: None,
            sort_descending: true,
            label_column,
            draws_tree_glyphs: false,
        }
    }

    pub fn with_selection(mut self, selected: Option<usize>, scroll_offset: usize) -> Self {
        self.selected = selected;
        self.scroll_offset = scroll_offset;
        self
    }

    pub fn with_sort(mut self, sort: Option<Cid>, descending: bool) -> Self {
        self.sort = sort;
        self.sort_descending = descending;
        self
    }

    pub fn with_tree_glyphs(mut self, on: bool) -> Self {
        self.draws_tree_glyphs = on;
        self
    }
}

impl<'a, R, G, Cid> Widget for &LiveTable<'a, R, G, Cid>
where
    R: TableRowExt<Cid>,
    G: GroupAggregate<Cid>,
    Cid: Copy + PartialEq,
{
    fn render(self, area: Rect, buf: &mut Buffer) {
        if area.width == 0 || area.height == 0 || self.columns.is_empty() {
            return;
        }

        let widths = resolve_widths(self.columns, area.width);

        let header_style = Style::default().fg(self.theme.title).add_modifier(Modifier::BOLD);
        let active_style = Style::default()
            .fg(self.theme.hi_fg)
            .add_modifier(Modifier::BOLD | Modifier::REVERSED);
        let arrow = if self.sort_descending { '↓' } else { '↑' };

        render_row(buf, area.x, area.y, area.width, self.columns, &widths, |idx| {
            let c = &self.columns[idx];
            let is_active = c.sortable && self.sort == Some(c.id);
            let tag = if is_active {
                format!("{}{}", c.label, arrow)
            } else {
                c.label.to_string()
            };
            let style = if is_active { active_style } else { header_style };
            (tag, style, c.align)
        });

        let body_top = area.y + 1;
        let body_h = area.height.saturating_sub(1) as usize;
        let max_visible = body_h.max(1);

        let label_idx = self
            .columns
            .iter()
            .position(|c| c.id == self.label_column)
            .expect("label_column must reference a column in columns");

        for (i, (row_idx, entry)) in self
            .rows
            .iter()
            .enumerate()
            .skip(self.scroll_offset)
            .take(body_h)
            .enumerate()
        {
            let y = body_top + i as u16;
            let is_selected = self.selected == Some(row_idx);
            let fade_t = (i as f32 / max_visible as f32) * FADE_END;

            if is_selected {
                for x in area.x..area.x + area.width {
                    buf[(x, y)].set_style(Style::default().bg(self.theme.selected_bg));
                }
            }

            match entry {
                TableEntry::Header(h) => {
                    self.render_header_entry(buf, area, y, h, is_selected, &widths, label_idx);
                }
                TableEntry::Item(r) => {
                    self.render_item_entry(buf, area, y, r, is_selected, fade_t, &widths, label_idx);
                }
            }
        }
    }
}

impl<'a, R, G, Cid> LiveTable<'a, R, G, Cid>
where
    R: TableRowExt<Cid>,
    G: GroupAggregate<Cid>,
    Cid: Copy + PartialEq,
{
    fn render_header_entry(
        &self,
        buf: &mut Buffer,
        area: Rect,
        y: u16,
        h: &G,
        is_selected: bool,
        widths: &[u16],
        label_idx: usize,
    ) {
        let glyph = if h.expanded() { '▼' } else { '▶' };
        let label_text = format!("{glyph} {}", h.label());

        let fg = if is_selected { self.theme.selected_fg } else { self.theme.hi_fg };
        let style = if is_selected {
            Style::default().bg(self.theme.selected_bg).fg(fg).add_modifier(Modifier::BOLD)
        } else {
            Style::default().fg(fg).add_modifier(Modifier::BOLD)
        };

        render_row(buf, area.x, y, area.width, self.columns, widths, |idx| {
            let cell = if idx == label_idx {
                Cell::plain("")
            } else {
                h.cell(self.columns[idx].id)
            };
            (cell.text, style, self.columns[idx].align)
        });

        let label_x = column_x(widths, area.x, label_idx);
        let label_w = widths[label_idx];
        write_str(buf, label_x, y, &label_text, label_w as usize, style);
    }

    fn render_item_entry(
        &self,
        buf: &mut Buffer,
        area: Rect,
        y: u16,
        r: &R,
        is_selected: bool,
        fade_t: f32,
        widths: &[u16],
        label_idx: usize,
    ) {
        let row_fg = lerp_color(self.theme.main_fg, self.theme.inactive_fg, fade_t);

        let prefix = if self.draws_tree_glyphs {
            tree_prefix(r)
        } else {
            "  ".repeat(r.tree_depth() as usize)
        };

        render_row(buf, area.x, y, area.width, self.columns, widths, |idx| {
            let col = &self.columns[idx];
            let mut cell = r.cell(col.id);

            if idx == label_idx {
                let avail = (widths[idx] as usize).saturating_sub(prefix.chars().count());
                cell.text = format!("{prefix}{}", truncate(&cell.text, avail.max(1)));
            }

            let style = if is_selected {
                Style::default().bg(self.theme.selected_bg).fg(self.theme.selected_fg)
            } else {
                let logical = cell.fg.unwrap_or(row_fg);
                Style::default().fg(lerp_color(logical, self.theme.inactive_fg, fade_t))
            };
            (cell.text, style, col.align)
        });

        if !is_selected && self.draws_tree_glyphs && !prefix.is_empty() {
            let glyph_fg = lerp_color(self.theme.accent_subtle, self.theme.inactive_fg, fade_t);
            let mut x = column_x(widths, area.x, label_idx);
            for _ in prefix.chars() {
                if x >= area.x + area.width {
                    break;
                }
                buf[(x, y)].set_style(Style::default().fg(glyph_fg));
                x = x.saturating_add(1);
            }
        }
    }
}

fn tree_prefix<R, Cid>(r: &R) -> String
where
    R: TableRowExt<Cid>,
{
    if r.tree_depth() == 0 {
        return String::new();
    }
    let mut out = String::new();
    for &cont in r.ancestor_continues() {
        out.push_str(if cont { "│  " } else { "   " });
    }
    out.push_str(if r.is_last_sibling() { "└─ " } else { "├─ " });
    out
}

/// Resolve each column's actual rendered width given `total_width`.
/// Flex columns soak up leftover space after fixed columns + gutters.
fn resolve_widths<Cid>(columns: &[ColumnDef<Cid>], total_width: u16) -> Vec<u16> {
    let n = columns.len() as u16;
    let fixed_total: u16 = columns
        .iter()
        .filter_map(|c| match c.width {
            WidthSpec::Fixed(w) => Some(w),
            WidthSpec::Flex => None,
        })
        .sum::<u16>()
        .saturating_add(n.saturating_sub(1));
    let mut flex_remaining = total_width.saturating_sub(fixed_total);

    columns
        .iter()
        .map(|c| match c.width {
            WidthSpec::Fixed(w) => w,
            WidthSpec::Flex => {
                let w = flex_remaining;
                flex_remaining = 0;
                w
            }
        })
        .collect()
}

/// Sum the widths (+ gutters) of the columns preceding `idx`.
fn column_x(widths: &[u16], x0: u16, idx: usize) -> u16 {
    let offset: u16 = widths
        .iter()
        .take(idx)
        .map(|w| w.saturating_add(1))
        .sum();
    x0.saturating_add(offset)
}

fn render_row<F, Cid>(
    buf: &mut Buffer,
    x0: u16,
    y: u16,
    total_width: u16,
    columns: &[ColumnDef<Cid>],
    widths: &[u16],
    mut cell_fn: F,
) where
    F: FnMut(usize) -> (String, Style, Align),
{
    if total_width == 0 || columns.is_empty() {
        return;
    }
    let right_limit = x0.saturating_add(total_width);
    let mut cursor = x0;
    for (i, col_w) in widths.iter().enumerate() {
        if cursor >= right_limit {
            break;
        }
        let avail = right_limit.saturating_sub(cursor).min(*col_w);
        if avail == 0 {
            cursor = cursor.saturating_add(*col_w).saturating_add(1);
            continue;
        }
        let (text, style, align) = cell_fn(i);
        let len = text::display_width(&text) as u16;
        let (text_x, text) = match (align, len < avail, len > avail) {
            (Align::Right, true, _) => (cursor + (avail - len), text),
            (_, _, true) => (cursor, truncate(&text, avail as usize)),
            _ => (cursor, text),
        };
        write_str(buf, text_x, y, &text, avail as usize, style);
        cursor = cursor.saturating_add(*col_w).saturating_add(1);
    }
}

fn write_str(buf: &mut Buffer, x: u16, y: u16, s: &str, max_cols: usize, style: Style) {
    let mut col = x;
    let right = x.saturating_add(max_cols as u16).min(buf.area.right());
    for ch in s.chars() {
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

fn lerp_color(a: Color, b: Color, t: f32) -> Color {
    let t = t.clamp(0.0, 1.0);
    match (a, b) {
        (Color::Rgb(ar, ag, ab), Color::Rgb(br, bg, bb)) => {
            let r = (ar as f32 + (br as f32 - ar as f32) * t).round() as u8;
            let g = (ag as f32 + (bg as f32 - ag as f32) * t).round() as u8;
            let b = (ab as f32 + (bb as f32 - ab as f32) * t).round() as u8;
            Color::Rgb(r, g, b)
        }
        _ => a,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    enum TestCol {
        A,
        B,
        C,
    }

    fn test_columns() -> Vec<ColumnDef<TestCol>> {
        vec![
            ColumnDef { id: TestCol::A, label: "A", width: WidthSpec::Fixed(4), align: Align::Left, sortable: true },
            ColumnDef { id: TestCol::B, label: "B", width: WidthSpec::Flex, align: Align::Left, sortable: true },
            ColumnDef { id: TestCol::C, label: "C", width: WidthSpec::Fixed(4), align: Align::Right, sortable: false },
        ]
    }

    struct TestRow {
        a: &'static str,
        b: &'static str,
        c: &'static str,
        depth: u8,
    }

    impl TableRowExt<TestCol> for TestRow {
        fn cell(&self, col: TestCol) -> Cell {
            match col {
                TestCol::A => Cell::plain(self.a),
                TestCol::B => Cell::plain(self.b),
                TestCol::C => Cell::plain(self.c),
            }
        }
        fn tree_depth(&self) -> u8 {
            self.depth
        }
        fn ancestor_continues(&self) -> &[bool] {
            &[]
        }
        fn is_last_sibling(&self) -> bool {
            true
        }
    }

    fn read_text(buf: &Buffer, y: u16, x_start: u16, len: u16) -> String {
        (x_start..x_start + len)
            .filter_map(|x| buf[(x, y)].symbol().chars().next())
            .collect()
    }

    #[test]
    fn resolve_widths_distributes_flex() {
        let cols = test_columns();
        let widths = resolve_widths(&cols, 20);
        // 4 (A) + flex (B) + 4 (C) + 2 gutters = 20  →  flex = 10
        assert_eq!(widths, vec![4, 10, 4]);
    }

    #[test]
    fn header_row_renders_labels_with_sort_arrow() {
        let theme = Theme::fallback();
        let cols = test_columns();
        let rows: Vec<TableEntry<TestRow, ()>> = vec![];
        let table = LiveTable::new(&rows, &cols, &theme, TestCol::B)
            .with_sort(Some(TestCol::A), true);
        let area = Rect::new(0, 0, 20, 1);
        let mut buf = Buffer::empty(area);
        (&table).render(area, &mut buf);
        // Column A is sortable, sort=A descending → "A↓".
        let header = read_text(&buf, 0, 0, 4);
        assert!(header.contains('A'), "got {header:?}");
        assert!(header.contains('↓'), "got {header:?}");
    }

    #[test]
    fn item_renders_into_cells() {
        let theme = Theme::fallback();
        let cols = test_columns();
        let rows: Vec<TableEntry<TestRow, ()>> = vec![TableEntry::Item(TestRow {
            a: "PID",
            b: "name",
            c: "9",
            depth: 0,
        })];
        let table = LiveTable::new(&rows, &cols, &theme, TestCol::B);
        let area = Rect::new(0, 0, 20, 2);
        let mut buf = Buffer::empty(area);
        (&table).render(area, &mut buf);
        let row1 = read_text(&buf, 1, 0, 20);
        assert!(row1.contains("PID"), "got {row1:?}");
        assert!(row1.contains("name"), "got {row1:?}");
        assert!(row1.contains('9'), "got {row1:?}");
    }

    #[test]
    fn selected_row_gets_full_width_highlight() {
        let theme = Theme::fallback();
        let cols = test_columns();
        let rows: Vec<TableEntry<TestRow, ()>> = vec![TableEntry::Item(TestRow {
            a: "x",
            b: "y",
            c: "z",
            depth: 0,
        })];
        let table = LiveTable::new(&rows, &cols, &theme, TestCol::B)
            .with_selection(Some(0), 0);
        let area = Rect::new(0, 0, 20, 2);
        let mut buf = Buffer::empty(area);
        (&table).render(area, &mut buf);
        for x in 0..20 {
            assert_eq!(buf[(x, 1)].style().bg, Some(theme.selected_bg), "col {x}");
        }
    }

    #[test]
    fn group_header_chevron_rendered_in_label_column() {
        struct G;
        impl GroupAggregate<TestCol> for G {
            fn label(&self) -> &str {
                "group-1"
            }
            fn cell(&self, _: TestCol) -> Cell {
                Cell::plain("")
            }
            fn expanded(&self) -> bool {
                true
            }
        }
        let theme = Theme::fallback();
        let cols = test_columns();
        let rows: Vec<TableEntry<TestRow, G>> = vec![TableEntry::Header(G)];
        let table = LiveTable::new(&rows, &cols, &theme, TestCol::B);
        let area = Rect::new(0, 0, 20, 2);
        let mut buf = Buffer::empty(area);
        (&table).render(area, &mut buf);
        let row1 = read_text(&buf, 1, 0, 20);
        assert!(row1.contains('▼'), "got {row1:?}");
        assert!(row1.contains("group-1"), "got {row1:?}");
    }
}
