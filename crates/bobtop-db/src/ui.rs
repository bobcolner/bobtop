//! Rendering — two-pane layout (tree on left, preview on right) plus
//! a status row and an action bar. Both panes use [`LiveTable`] from
//! the toolkit; this is the marquee demo of LiveTable's tree mode.

use gtui::browser::BrowserShell;
use gtui::widgets::live_table::{
    Align, Cell, ColumnDef, LiveTable, TableEntry, TableRowExt, WidthSpec,
};
use gtui::widgets::{panel as boxed_panel, ActionBar};
use gtui::write_str_clipped;
use ratatui::layout::{Constraint, Direction, Layout, Rect};
use ratatui::style::{Color, Style};
use ratatui::Frame;

use crate::app::{App, Focus};
use crate::tree::{CatalogNode, NodeKind};

pub fn draw(app: &App, frame: &mut Frame<'_>) {
    let area = frame.area();
    if area.width < 30 || area.height < 6 {
        return;
    }
    // Paint main_bg across the whole canvas first so chrome and
    // gutters carry the theme bg. Same approach as gfb.
    if let Some(bg) = app.theme.main_bg {
        let buf = frame.buffer_mut();
        for y in area.y..area.y + area.height {
            for x in area.x..area.x + area.width {
                buf[(x, y)].set_bg(bg);
            }
        }
    }

    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(1),  // status / endpoint label
            Constraint::Min(0),     // panes
            Constraint::Length(1),  // action bar
        ])
        .split(area);

    draw_status(app, frame, chunks[0]);

    let preview_rect = draw_tree_pane(app, frame, chunks[1]);
    draw_preview_pane(app, frame, preview_rect);

    draw_action_bar(app, frame, chunks[2]);
}

fn draw_status(app: &App, frame: &mut Frame<'_>, area: Rect) {
    if area.height == 0 {
        return;
    }
    let buf = frame.buffer_mut();
    let style = Style::default().fg(app.theme.title);
    let label = match &app.status {
        Some(msg) => format!(" ⚠ {msg}"),
        None => match app.conns.len() {
            0 => " (no connections)".to_string(),
            1 => format!(" {}", app.conns[0].endpoint_label()),
            n => format!(
                " {} connections — {}",
                n,
                app.conns
                    .iter()
                    .map(|c| c.endpoint_label())
                    .collect::<Vec<_>>()
                    .join(", ")
            ),
        },
    };
    write_str_clipped(buf, area.x, area.y, &label, area.width, style);
}

fn draw_tree_pane(app: &App, frame: &mut Frame<'_>, area: Rect) -> Rect {
    let nodes = app.tree.nodes();
    let entries: Vec<TableEntry<TreeRowView<'_>, ()>> = nodes
        .iter()
        .map(|n| TableEntry::Item(TreeRowView { node: n, theme: &app.theme }))
        .collect();
    let columns = vec![ColumnDef {
        id: TreeCol::Name,
        label: "Name",
        width: WidthSpec::Flex,
        align: Align::Left,
        sortable: false,
    }];
    BrowserShell::<TreeCol>::new()
        .with_title("catalog")
        .with_keybinds("Tab focus  •  ↑↓ move  •  Space expand/load  •  q quit")
        .with_accent(app.theme.panel_accents[0])
        .with_focused(matches!(app.focus, Focus::Tree))
        .with_tree_percent(38)
        .render(
            frame,
            area,
            &entries,
            &columns,
            &app.theme,
            TreeCol::Name,
            app.tree_nav.cursor,
        )
}

fn draw_preview_pane(app: &App, frame: &mut Frame<'_>, area: Rect) {
    let focused = matches!(app.focus, Focus::Preview);
    let title = app
        .preview
        .as_ref()
        .map(|p| {
            let path = &p.path;
            format!(
                "{}.{}.{}",
                path.database.as_deref().unwrap_or("?"),
                path.schema.as_deref().unwrap_or("?"),
                path.table.as_deref().unwrap_or("?"),
            )
        })
        .unwrap_or_else(|| "preview".to_string());
    let panel = boxed_panel(border_color(&app.theme, focused, app.theme.panel_accents[1]), app.theme.title, Default::default())
        .with_title(title)
        .with_controls(format!(
            "rows: {}",
            app.preview.as_ref().map(|p| p.rows.len()).unwrap_or(0)
        ));
    frame.render_widget(&panel, area);
    let inner = panel.inner(area);
    if inner.width < 6 || inner.height < 2 {
        return;
    }
    let Some(preview) = &app.preview else {
        let buf = frame.buffer_mut();
        let hint = " select a table on the left, then press Enter ";
        let style = Style::default().fg(app.theme.inactive_fg);
        let y = inner.y + inner.height / 2;
        let x = inner.x + inner.width.saturating_sub(hint.chars().count() as u16) / 2;
        write_str_clipped(buf, x, y, hint, inner.width, style);
        return;
    };

    let columns: Vec<ColumnDef<usize>> = preview
        .columns
        .iter()
        .enumerate()
        .map(|(i, c)| ColumnDef {
            id: i,
            label: leak_column_label(c.name.as_str()),
            width: WidthSpec::Fixed(column_width(c.name.len(), preview.rows.iter().map(|r| r.cells.get(i).map(|s| s.len()).unwrap_or(0)).max().unwrap_or(0))),
            align: align_for(c.data_type.as_str()),
            sortable: false,
        })
        .collect();
    let entries: Vec<TableEntry<PreviewRowView<'_>, ()>> = preview
        .rows
        .iter()
        .map(|r| TableEntry::Item(PreviewRowView { row: r }))
        .collect();
    let body_h = inner.height.saturating_sub(1) as usize;
    let scroll = gtui::middle_anchor_scroll(
        app.preview_nav.cursor,
        preview.rows.len(),
        body_h,
    );
    let table = LiveTable::new(&entries, &columns, &app.theme, 0)
        .with_selection(Some(app.preview_nav.cursor), scroll)
        .with_fade(false);
    frame.render_widget(&table, inner);
}

fn draw_action_bar(app: &App, frame: &mut Frame<'_>, area: Rect) {
    if area.height == 0 {
        return;
    }
    let actions = vec![
        ("Tab".into(), "focus".into()),
        ("↑↓ / jk".into(), "move".into()),
        ("Space / Enter".into(), "expand or load".into()),
        ("h / ←".into(), "collapse".into()),
        ("q / Esc".into(), "quit".into()),
    ];
    let bar = ActionBar::new(actions).with_colors(
        app.theme.div_line,
        app.theme.hi_fg,
        app.theme.main_fg,
        app.theme.selected_bg,
    );
    frame.render_widget(&bar, area);
}

fn border_color(theme: &gtui::Theme, focused: bool, accent: Color) -> Color {
    if focused {
        accent
    } else {
        theme.div_line
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum TreeCol {
    Name,
}

struct TreeRowView<'a> {
    node: &'a CatalogNode,
    theme: &'a gtui::Theme,
}

impl<'a> TableRowExt<TreeCol> for TreeRowView<'a> {
    fn cell(&self, _col: TreeCol) -> Cell {
        let chevron = match (self.node.expandable, self.node.expanded) {
            (true, true) => "▼ ",
            (true, false) => "▶ ",
            (false, _) => "  ",
        };
        let label = format!("{chevron}{}", self.node.label);
        let fg = match self.node.kind {
            NodeKind::Endpoint => self.theme.hi_fg,
            NodeKind::Database => self.theme.title,
            NodeKind::Schema => self.theme.main_fg,
            NodeKind::Table => self.theme.main_fg,
        };
        Cell::styled(label, fg)
    }

    fn tree_depth(&self) -> u8 {
        self.node.depth
    }

    fn ancestor_continues(&self) -> &[bool] {
        &self.node.ancestor_continues
    }

    fn is_last_sibling(&self) -> bool {
        self.node.is_last_sibling
    }
}

struct PreviewRowView<'a> {
    row: &'a crate::conn::Row,
}

impl<'a> TableRowExt<usize> for PreviewRowView<'a> {
    fn cell(&self, col: usize) -> Cell {
        let text = self.row.cells.get(col).cloned().unwrap_or_default();
        Cell::plain(text)
    }
}

fn align_for(data_type: &str) -> Align {
    let lower = data_type.to_ascii_lowercase();
    if lower.contains("int")
        || lower.contains("numeric")
        || lower.contains("float")
        || lower.contains("double")
        || lower.contains("decimal")
        || lower.contains("real")
    {
        Align::Right
    } else {
        Align::Left
    }
}

fn column_width(header_len: usize, max_cell_len: usize) -> u16 {
    // Fit content but cap at 32 cells so wide TEXT/UUID columns don't
    // dominate. Caller can scroll horizontally in a follow-up.
    let raw = header_len.max(max_cell_len);
    raw.clamp(4, 32) as u16
}

/// LiveTable's `ColumnDef::label` is `&'static str`; the preview's
/// column names are owned strings on `PreviewData`. Leak them on the
/// hot path to satisfy the lifetime — the leak is bounded by the
/// number of columns × table loads in a session, well under any
/// practical memory budget.
///
/// A follow-up can lift LiveTable's label to `Cow<'static, str>` to
/// avoid the leak, which matters once column metadata churns (e.g.
/// the user clicks through hundreds of tables in one session).
fn leak_column_label(s: &str) -> &'static str {
    Box::leak(s.to_string().into_boxed_str())
}
