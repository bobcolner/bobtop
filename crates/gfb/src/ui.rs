//! Drawing the file browser frame.
//!
//! Per the workspace rule, `gfb` consumes `gtui` widgets
//! exclusively — `ratatui::Frame` is used only to dispatch those widgets
//! to the terminal. If a primitive isn't covered by a `gtui` widget,
//! the answer is to add one there, not reach into `ratatui::widgets::*`.

use std::time::SystemTime;

use gtui::{
    format_bytes_compact, ActionBar, BoxedPanel, Cell, Column, ConfirmDialog, EditableText,
    HelpModal, MillerColumn, MillerColumns, ModalShell, Row, ScrollableText, Table, Theme,
};
use image::GenericImageView;
use ratatui::layout::Rect;
use ratatui::style::{Color, Style};
use ratatui::text::{Line, Span};
use ratatui::Frame;

use crate::app::{App, DirContent, Focus, ImageBackend, InputModal};
use crate::find::FindResult;

/// Three-column miller layout shared between the renderer and the
/// run loop's mouse-routing math. Returns one rect per column.
/// The preview-pane weight is parameterized so `[`/`]` can shrink
/// or grow it (0 = collapsed entirely; the list pane absorbs).
pub fn split_main_columns(top: Rect, preview_weight: u16) -> Vec<Rect> {
    // Weights and min widths are tuned so the parent collapses first
    // on narrow terminals (weight 1, min 16), then the preview
    // (when present) — the current dir always survives.
    let preview_min = if preview_weight == 0 { 0 } else { 24 };
    let columns = MillerColumns::new(vec![
        MillerColumn::new(1, 16),
        MillerColumn::new(3, 24),
        MillerColumn::new(preview_weight, preview_min),
    ])
    .with_gap(0);
    columns.split(top)
}

/// One draw of the entire file-browser frame.
pub fn draw(app: &App, frame: &mut Frame<'_>, theme: &Theme) {
    let area = frame.area();
    if area.height < 4 || area.width < 16 {
        return;
    }
    // Paint the theme's main_bg across the whole canvas first so
    // panels, text, and gaps between widgets all inherit the
    // intended background. Cells that subsequent widgets don't
    // explicitly restyle (most chrome and gutter cells) keep this
    // bg, which is what makes a Solarized or Dracula theme actually
    // *look* dark/light instead of falling through to the user's
    // terminal default.
    if let Some(bg) = theme.main_bg {
        let buf = frame.buffer_mut();
        for y in area.y..area.y + area.height {
            for x in area.x..area.x + area.width {
                buf[(x, y)].set_bg(bg);
            }
        }
    }
    // Full-screen preview/editor takes over the entire viewport.
    if app.is_full_preview() {
        if app.editor().is_some() {
            draw_editor_fullscreen(app, frame, theme, area);
        } else {
            draw_preview_fullscreen(app, frame, theme, area);
        }
        if let Some(modal) = app.input_modal() {
            draw_input_modal(modal, frame, theme, area);
        }
        return;
    }
    let (breadcrumb_rect, rest) = split_top(area, 1);
    let (top, bottom) = split_top_bottom(rest, 1);
    draw_breadcrumb(app, frame, theme, breadcrumb_rect);
    match app.view_mode {
        crate::app::ViewMode::Database => {
            draw_db_mode(app, frame, theme, top);
            draw_action_bar(app, frame, theme, bottom);
        }
        crate::app::ViewMode::Miller | crate::app::ViewMode::Table => {
            let rects = split_main_columns(top, app.preview_size().weight());
            draw_parent_pane(app, frame, theme, rects[0]);
            draw_list_pane(app, frame, theme, rects[1]);
            draw_preview_pane(app, frame, theme, rects[2]);
            draw_action_bar(app, frame, theme, bottom);
        }
        crate::app::ViewMode::Tree => {
            let rects = split_main_columns(top, app.preview_size().weight());
            draw_parent_pane(app, frame, theme, rects[0]);
            draw_tree_center(app, frame, theme, rects[1]);
            draw_preview_pane(app, frame, theme, rects[2]);
            draw_action_bar(app, frame, theme, bottom);
        }
    }
    if let Some(modal) = app.input_modal() {
        draw_input_modal(modal, frame, theme, area);
    }
    if app.is_command_palette_active() {
        draw_command_palette(app, frame, theme, area);
    }
    if app.is_info_panel_active() {
        draw_info_panel(app, frame, theme, area);
    }
    if app.is_branch_overlay_active() {
        draw_branch_overlay(app, frame, theme, area);
    }
    if let Some(menu) = app.options_menu() {
        menu.render(frame, area, theme);
    }
    if app.is_help_active() {
        draw_help_overlay(frame, area, theme);
    }
}

/// gfb keybind reference. Mirrors gtop's overlay layout via the shared
/// `HelpModal` widget so the two apps' help screens look the same.
const HELP_LINES: &[(&str, &str)] = &[
    ("?", "toggle this help"),
    ("q / b / Ctrl-C", "quit"),
    ("Esc", "close overlay / clear filter / quit if idle"),
    // navigation
    ("↑ ↓ / j k", "move cursor"),
    ("← / h", "parent directory"),
    ("→ / l / Enter", "enter directory or open file"),
    ("g / G  ·  Home / End", "jump to top / bottom"),
    ("PgUp / PgDn  ·  Ctrl-U / Ctrl-D", "page up / down"),
    ("Tab", "toggle focus between list and preview"),
    ("[ / ]", "shrink / grow preview pane"),
    // view modes
    ("T", "toggle Miller / Tree view"),
    ("D", "toggle database browser"),
    ("Space", "toggle full-screen preview"),
    (".", "show / hide dotfiles"),
    // file operations
    ("a", "create empty file"),
    ("r", "rename selection (refresh in DB mode)"),
    ("x / X", "trash / hard-delete with confirm"),
    ("e", "open in $EDITOR"),
    ("f", "recursive file finder"),
    ("/", "filter current directory"),
    // tree-mode sort
    ("s / S", "cycle sort column forward / back (Tree mode)"),
    ("R", "reverse sort direction (Tree mode)"),
    // git
    ("B", "branch overlay"),
    ("i", "info panel"),
    // misc
    (":", "command palette"),
    ("F5", "refresh"),
    ("M", "toggle mouse capture (off = native copy)"),
    ("O", "options — theme / behavior / connections"),
];

fn draw_help_overlay(frame: &mut Frame, area: Rect, theme: &Theme) {
    HelpModal::new(theme, " gfb ", HELP_LINES)
        .with_banner_text("GFB")
        .with_actions(vec![
            ("Esc".into(), "close".into()),
            ("?".into(), "toggle".into()),
            ("q".into(), "quit".into()),
        ])
        .render(frame, area);
}


fn target_name(p: &std::path::Path) -> String {
    p.file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_else(|| p.display().to_string())
}

/// Centered input/confirm modal for rename / touch / soft-delete /
/// hard-delete. Wraps the toolkit [`ConfirmDialog`].
fn draw_input_modal(modal: &InputModal, frame: &mut Frame<'_>, theme: &Theme, area: Rect) {
    let (title, body_lines, controls): (&str, Vec<Line>, &str) = match modal {
        InputModal::Rename { buffer, .. } => (
            "rename",
            vec![Line::from(format!("→ {}▏", buffer))],
            "Enter confirm  •  Esc cancel",
        ),
        InputModal::Touch { buffer } => (
            "new file",
            vec![Line::from(format!("→ {}▏", buffer))],
            "Enter create  •  Esc cancel",
        ),
        InputModal::ConfirmTrash { target } => (
            "send to trash?",
            vec![
                Line::from(target_name(target)),
                Line::from(""),
                Line::from("Recoverable from your system trash."),
            ],
            "y / Enter  trash  •  Esc cancel",
        ),
        InputModal::ConfirmHardDelete { target } => (
            "permanent delete?",
            vec![
                Line::from(target_name(target)),
                Line::from(""),
                Line::from("This cannot be undone."),
            ],
            "y delete  •  any other key cancel",
        ),
        InputModal::ConfirmDropDbObject { path, cascade, .. } => {
            use crate::sources::NodeKind;
            let schema = path.schema.as_deref().unwrap_or("?");
            let tbl = path.table.as_deref().unwrap_or("?");
            let (title, detail, warning) = if *cascade {
                let obj = match path.level() {
                    NodeKind::Table => format!("{schema}.{tbl}"),
                    _ => schema.to_string(),
                };
                (
                    "drop cascade?",
                    format!("{obj}"),
                    "Other objects depend on this — CASCADE will drop them too.",
                )
            } else {
                match path.level() {
                    NodeKind::Table => (
                        "drop table?",
                        format!("{schema}.{tbl}"),
                        "This cannot be undone.",
                    ),
                    NodeKind::Schema => (
                        "drop schema?",
                        format!("{schema}  (all tables inside will be dropped)"),
                        "This cannot be undone.",
                    ),
                    _ => ("drop?", String::new(), ""),
                }
            };
            (
                title,
                vec![
                    Line::from(detail),
                    Line::from(""),
                    Line::from(warning),
                ],
                "y / Enter  confirm  •  any other key cancel",
            )
        }
    };
    ConfirmDialog::new(theme, title)
        .with_body(body_lines)
        .with_hint(controls)
        .render(frame, area);
}

/// Border color for a pane based on focus. Active pane uses its
/// accent color; inactive uses the dim divider color.
fn border_color(theme: &Theme, focused: bool, accent: Color) -> Color {
    if focused {
        accent
    } else {
        theme.div_line
    }
}

fn split_top_bottom(area: Rect, bottom_h: u16) -> (Rect, Rect) {
    let h = area.height;
    if h <= bottom_h {
        return (area, Rect::new(area.x, area.y + h, area.width, 0));
    }
    let top = Rect::new(area.x, area.y, area.width, h - bottom_h);
    let bot = Rect::new(area.x, area.y + h - bottom_h, area.width, bottom_h);
    (top, bot)
}

/// Carve `h` rows off the top of `area`. Returns (top_strip, remainder).
fn split_top(area: Rect, h: u16) -> (Rect, Rect) {
    if area.height <= h {
        return (area, Rect::new(area.x, area.y + area.height, area.width, 0));
    }
    let strip = Rect::new(area.x, area.y, area.width, h);
    let rest = Rect::new(area.x, area.y + h, area.width, area.height - h);
    (strip, rest)
}

fn draw_breadcrumb(app: &App, frame: &mut Frame<'_>, theme: &Theme, area: Rect) {
    use std::path::PathBuf;
    use ratatui::style::Modifier;

    if area.height == 0 || area.width == 0 {
        return;
    }

    // In DB mode show the Miller-level stack as a breadcrumb. Same
    // shape as the file-mode breadcrumb below: leading indent, single
    // leading glyph, components separated by ` / `, last component
    // bold, the rest dimmed.
    if matches!(app.view_mode, crate::app::ViewMode::Database) {
        let sep_style = Style::default().fg(theme.div_line);
        let dim_style = Style::default().fg(theme.main_fg).add_modifier(Modifier::DIM);
        let last_style = Style::default().fg(theme.hi_fg).add_modifier(Modifier::BOLD);

        // Each drilled-in level after the root contributes its parent
        // label; append the currently-selected item in the deepest
        // level so the deepest segment updates live as the user moves.
        let state = app.db_miller();
        let mut segments: Vec<String> = state
            .levels
            .iter()
            .skip(1)
            .filter_map(|l| l.parent_label.clone())
            .collect();
        let depth = state.depth();
        let items = app.db_level_items(depth - 1);
        if let Some(item) = items.get(state.current().cursor) {
            segments.push(item.label().to_string());
        }

        let mut spans: Vec<Span<'static>> = vec![Span::raw("  "), Span::raw("⛁ ")];
        if segments.is_empty() {
            spans.push(Span::styled("(no selection)", dim_style));
        } else {
            let n = segments.len();
            for (i, seg) in segments.into_iter().enumerate() {
                let style = if i + 1 == n { last_style } else { dim_style };
                spans.push(Span::styled(seg, style));
                if i + 1 < n {
                    spans.push(Span::styled(" / ", sep_style));
                }
            }
        }

        let lines = vec![Line::from(spans)];
        let widget = ScrollableText::new(&lines, theme);
        frame.render_widget(&widget, area);
        return;
    }

    let cwd = app.cwd();
    let home = std::env::var("HOME").ok().map(PathBuf::from);

    let display_path = match &home {
        Some(h) if cwd.starts_with(h) => {
            let rel = cwd.strip_prefix(h).unwrap_or(cwd);
            let mut p = PathBuf::from("~");
            p.push(rel);
            p
        }
        _ => cwd.to_path_buf(),
    };

    let components: Vec<String> = display_path
        .components()
        .map(|c| c.as_os_str().to_string_lossy().into_owned())
        .collect();

    let sep_style = Style::default().fg(theme.div_line);
    let dim_style = Style::default().fg(theme.main_fg).add_modifier(Modifier::DIM);
    let last_style = Style::default().fg(theme.hi_fg).add_modifier(Modifier::BOLD);

    let mut spans: Vec<Span<'static>> = vec![Span::raw("  ")];
    let n = components.len();
    for (i, comp) in components.into_iter().enumerate() {
        let style = if i + 1 == n { last_style } else { dim_style };
        spans.push(Span::styled(comp, style));
        if i + 1 < n {
            spans.push(Span::styled(" / ", sep_style));
        }
    }

    // Git branch badge: `  [main ✦]` — ✦ only when dirty.
    let git = app.git_state();
    if let Some(ref branch) = git.branch {
        spans.push(Span::styled("  [", Style::default().fg(theme.div_line)));
        spans.push(Span::styled(
            branch.clone(),
            Style::default().fg(theme.hi_fg).add_modifier(Modifier::BOLD),
        ));
        if git.is_dirty {
            spans.push(Span::styled(" ✦", Style::default().fg(theme.panel_accents[2])));
        }
        spans.push(Span::styled("]", Style::default().fg(theme.div_line)));
    }

    let lines = vec![Line::from(spans)];
    let widget = ScrollableText::new(&lines, theme);
    frame.render_widget(&widget, area);
}

/// Three-pane DB browser using the same Miller-columns layout as the
/// file browser: parent level on the left, current level in the
/// middle, preview (schema for tables, next-level children otherwise,
/// or the SQL editor when active) on the right.
fn draw_db_mode(app: &App, frame: &mut Frame<'_>, theme: &Theme, area: Rect) {
    if app.connections().is_empty() {
        let lines = vec![
            Line::from(""),
            Line::from(Span::styled(
                "  No database connections.",
                Style::default().fg(theme.hi_fg),
            )),
            Line::from(""),
            Line::from(Span::styled(
                "  Set GFB_CONNECT in your environment or use --connect <url>.",
                Style::default().fg(theme.main_fg),
            )),
        ];
        let widget = ScrollableText::new(&lines, theme);
        frame.render_widget(&widget, area);
        return;
    }

    // Use the same preview weight as the file browser so `[`/`]`
    // resize keys feel identical across modes. While the query editor
    // is active, force the preview wide enough to comfortably show
    // SQL + results without the user needing to widen it manually.
    let preview_weight = if app.is_query_editor_active() {
        crate::app::PreviewSize::Large.weight()
    } else {
        app.preview_size().weight()
    };
    let rects = split_main_columns(area, preview_weight);
    let depth = app.db_miller().depth();

    if depth >= 2 {
        draw_db_level_pane(app, frame, theme, rects[0], depth - 2, false);
    } else {
        let panel = BoxedPanel::new(theme.div_line, theme.title)
            .with_title("⌂".to_string());
        frame.render_widget(&panel, rects[0]);
    }

    draw_db_level_pane(app, frame, theme, rects[1], depth - 1, true);

    if app.is_query_editor_active() {
        draw_query_pane(app, frame, theme, rects[2]);
    } else if let Some(preview) = app.db_preview() {
        draw_db_preview(preview, frame, theme, rects[2]);
    } else {
        draw_db_next_level_preview(app, frame, theme, rects[2]);
    }
}

/// Render one Miller column of the DB browser. `focused` controls the
/// border accent (the current level gets the highlighted DB accent).
fn draw_db_level_pane(
    app: &App,
    frame: &mut Frame<'_>,
    theme: &Theme,
    area: Rect,
    level_idx: usize,
    focused: bool,
) {
    use crate::app::{DbLevelSource, DbMillerState};
    let state: &DbMillerState = app.db_miller();
    let Some(level) = state.levels.get(level_idx) else {
        return;
    };
    let items = app.db_level_items(level_idx);
    let (title, glyph) = match &level.source {
        DbLevelSource::Connections => ("connections".to_string(), "⛁"),
        DbLevelSource::Children { .. } => (
            level.parent_label.clone().unwrap_or_else(|| "—".to_string()),
            db_level_glyph(level_idx),
        ),
    };
    // Match the file browser: parent pane uses `div_line`, the focused
    // current pane uses `panel_accents[3]`. Keeping the same palette
    // across modes means the active-pane glow looks identical.
    let accent = if focused {
        theme.panel_accents[3]
    } else {
        theme.div_line
    };
    let panel = BoxedPanel::new(accent, theme.title)
        .with_title(format!("{} {}", glyph, title))
        .with_controls(if items.is_empty() {
            "loading…".to_string()
        } else {
            format!("{} items", items.len())
        });
    frame.render_widget(&panel, area);
    let inner = panel.inner(area);
    if inner.height == 0 || items.is_empty() {
        return;
    }
    let cols = vec![Column::new("", u16::MAX)];
    let rows: Vec<Row<'static>> = items
        .iter()
        .map(|it| Row::data(vec![Cell::new(format!("{} {}", it.glyph(), it.label()))]))
        .collect();
    let body_h = inner.height.saturating_sub(1) as usize;
    let scroll = if body_h > 0 && level.cursor >= body_h {
        level.cursor + 1 - body_h
    } else {
        0
    };
    let table = Table::new(&cols, &rows, theme).with_selection(Some(level.cursor), scroll);
    frame.render_widget(&table, inner);
}

/// Right-pane preview when the current selection is expandable: show
/// the next level's children as a read-only list (drives the "live"
/// look of Miller columns where one column ahead is always visible).
fn draw_db_next_level_preview(app: &App, frame: &mut Frame<'_>, theme: &Theme, area: Rect) {
    use crate::sources::multi::ChildrenState;
    let state = app.db_miller();
    let depth = state.depth();
    let items = app.db_level_items(depth - 1);
    let cursor = state.current().cursor;
    let Some(item) = items.get(cursor) else {
        let panel = BoxedPanel::new(theme.div_line, theme.title).with_title("—".to_string());
        frame.render_widget(&panel, area);
        return;
    };
    if !item.is_expandable() {
        // Table without a loaded preview — show a hint that Enter
        // pulls the schema. Once loaded, this branch is skipped and
        // `draw_db_preview` renders the columns instead.
        use ratatui::style::Modifier;
        let panel = BoxedPanel::new(theme.div_line, theme.title)
            .with_title(format!("{} {}", item.glyph(), item.label()))
            .with_controls("Enter to preview".to_string());
        frame.render_widget(&panel, area);
        let inner = panel.inner(area);
        if inner.height >= 2 {
            let hint = Line::from(vec![
                Span::raw("  "),
                Span::styled(
                    "Press ".to_string(),
                    Style::default().fg(theme.main_fg).add_modifier(Modifier::DIM),
                ),
                Span::styled("Enter", Style::default().fg(theme.hi_fg).add_modifier(Modifier::BOLD)),
                Span::styled(
                    " to load the schema preview.",
                    Style::default().fg(theme.main_fg).add_modifier(Modifier::DIM),
                ),
            ]);
            let lines = vec![Line::from(""), hint];
            let widget = ScrollableText::new(&lines, theme);
            frame.render_widget(&widget, inner);
        }
        return;
    }
    let path = item.node_path();
    let children: Vec<crate::app::DbItem> = {
        let cache = app.db_cache().borrow();
        match cache.get(&path) {
            Some(ChildrenState::Ready(rows)) => rows
                .iter()
                .map(|(p, d)| crate::app::db_item_from_path(item.root(), p, d))
                .collect(),
            _ => Vec::new(),
        }
    };
    let controls = if children.is_empty() {
        "loading…".to_string()
    } else {
        format!("{} items", children.len())
    };
    let panel = BoxedPanel::new(theme.div_line, theme.title)
        .with_title(format!("{} {}", item.glyph(), item.label()))
        .with_controls(controls);
    frame.render_widget(&panel, area);
    let inner = panel.inner(area);
    if inner.height == 0 || children.is_empty() {
        return;
    }
    let cols = vec![Column::new("", u16::MAX)];
    let rows: Vec<Row<'static>> = children
        .iter()
        .map(|it| Row::data(vec![Cell::new(format!("{} {}", it.glyph(), it.label()))]))
        .collect();
    let table = Table::new(&cols, &rows, theme);
    frame.render_widget(&table, inner);
}

/// Glyph for the level at `idx` based on what *kind* of items it
/// holds: 0 → connections, 1 → databases, 2 → schemas, 3 → tables.
fn db_level_glyph(idx: usize) -> &'static str {
    match idx {
        0 => "⛁",
        1 => "🗄",
        2 => "📂",
        _ => "▦",
    }
}

/// Render the tree as a LiveTable with tree glyphs in the given `area`
/// (center column). Unlike the old `draw_tree_view`, this does NOT
/// split `area` into tree + preview — the caller owns the split.
fn draw_tree_center(app: &App, frame: &mut Frame<'_>, theme: &Theme, area: Rect) {
    use crate::tree::{build_rows_for_app, FbCol};
    use gtui::widgets::live_table::{Align, ColumnDef, LiveTable, WidthSpec};

    if area.width == 0 || area.height == 0 {
        return;
    }

    let rows = build_rows_for_app(
        app.cwd(),
        app.tree_sort,
        app.tree_sort_descending,
        app.show_hidden(),
        &app.tree_state.expanded,
        app.filter(),
        &[],
        app.db_cache(),
        app.fs_cache(),
        Some(app.db_load_tx()),
    );

    // Inner width after the BoxedPanel bubble-title border (4 rows eaten,
    // 2 cols per side). Use it to decide which columns to show so the
    // Name column always has enough room for tree glyphs + a filename.
    let inner_w = area.width.saturating_sub(2);
    let mut columns: Vec<ColumnDef<FbCol>> = vec![ColumnDef {
        id: FbCol::Name,
        label: "Name",
        width: WidthSpec::Flex,
        align: Align::Left,
        sortable: true,
    }];
    if inner_w >= 48 {
        columns.push(ColumnDef {
            id: FbCol::Modified,
            label: "Modified",
            width: WidthSpec::Fixed(16),
            align: Align::Left,
            sortable: true,
        });
    }
    if inner_w >= 36 {
        columns.push(ColumnDef {
            id: FbCol::Size,
            label: "Size",
            width: WidthSpec::Fixed(7),
            align: Align::Right,
            sortable: true,
        });
    }

    let panel = BoxedPanel::new(theme.panel_accents[3], theme.title)
        .with_title(format!("tree · {}", app.cwd_display()));
    frame.render_widget(&panel, area);
    let inner = panel.inner(area);
    if inner.height < 1 || inner.width < 4 {
        return;
    }

    let body_h = inner.height.saturating_sub(1) as usize;
    let scroll = gtui::middle_anchor_scroll(app.tree_state.nav.cursor, rows.len(), body_h);
    let table = LiveTable::new(&rows, &columns, theme, FbCol::Name)
        .with_selection(Some(app.tree_state.nav.cursor), scroll)
        .with_tree_glyphs(true)
        .with_fade(false)
        .with_sort(Some(app.tree_sort), app.tree_sort_descending);
    frame.render_widget(&table, inner);
}

fn draw_command_palette(app: &App, frame: &mut Frame<'_>, theme: &Theme, area: Rect) {
    use ratatui::style::Modifier;

    let Some(state) = app.command_palette_state() else {
        return;
    };
    let suggestions = app.palette_suggestions();

    let modal_w = (area.width * 2 / 3).max(52).min(area.width.saturating_sub(4));
    let suggestion_rows = suggestions.len() as u16;
    let modal_h = (suggestion_rows + 4).min(area.height.saturating_sub(4));
    let bg = theme.main_bg.unwrap_or(ratatui::style::Color::Black);

    let panel = BoxedPanel::new(theme.panel_accents[2], theme.title)
        .with_title("command")
        .with_controls("↑↓  Enter  Esc")
        .flat();
    let shell = ModalShell::new(panel, modal_w, modal_h)
        .with_fill(Style::default().bg(bg).fg(theme.main_fg));
    let Some(body) = shell.render(frame, area) else {
        return;
    };

    let input_style = Style::default()
        .fg(theme.hi_fg)
        .add_modifier(Modifier::BOLD);
    let desc_style = Style::default().fg(theme.main_fg).add_modifier(Modifier::DIM);
    let sel_style = Style::default().bg(theme.selected_bg).fg(theme.hi_fg);

    let mut lines: Vec<Line<'static>> = Vec::new();
    lines.push(Line::from(Span::styled(
        format!(": {}▏", state.input),
        input_style,
    )));
    lines.push(Line::from(""));

    for (i, entry) in suggestions.iter().enumerate() {
        let (cmd_style, d_style) = if i == state.selected {
            (sel_style, sel_style)
        } else {
            (Style::default().fg(theme.hi_fg), desc_style)
        };
        lines.push(Line::from(vec![
            Span::styled(format!("  {:<22}", entry.cmd), cmd_style),
            Span::styled(entry.description.to_string(), d_style),
        ]));
    }

    if suggestions.is_empty() {
        lines.push(Line::from(Span::styled(
            "  no matching commands",
            desc_style,
        )));
    }

    let widget = ScrollableText::new(&lines, theme);
    frame.render_widget(&widget, body);
}

fn draw_info_panel(app: &App, frame: &mut Frame<'_>, theme: &Theme, area: Rect) {
    use ratatui::style::Modifier;
    let Some(panel) = app.info_panel() else { return };

    let dim = Style::default().fg(theme.main_fg).add_modifier(Modifier::DIM);
    let val = Style::default().fg(theme.hi_fg);

    let mut lines: Vec<Line<'static>> = Vec::new();

    let push = |lines: &mut Vec<Line<'static>>, label: &'static str, value: String| {
        lines.push(Line::from(vec![
            Span::styled(format!("  {label:<12}"), dim),
            Span::styled(value, val),
        ]));
    };

    push(&mut lines, "name", panel.name.clone());

    let kind_str = match panel.kind {
        crate::fs::entry::EntryKind::Dir => "directory",
        crate::fs::entry::EntryKind::File => "file",
        crate::fs::entry::EntryKind::Symlink => "symlink",
        crate::fs::entry::EntryKind::Other => "other",
    };
    push(&mut lines, "type", kind_str.to_string());

    if let Some(mtime) = panel.mtime {
        use std::time::UNIX_EPOCH;
        let secs = mtime.duration_since(UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0);
        let (y, mo, d, h, mi, s) = epoch_to_ymd_hms(secs);
        push(&mut lines, "modified", format!("{y}-{mo:02}-{d:02}  {h:02}:{mi:02}:{s:02}"));
    }

    match &panel.dir_content {
        None => {
            // Plain file — show size.
            push(&mut lines, "size", format_bytes_compact(panel.size));
        }
        Some(DirContent::Calculating) => {
            push(&mut lines, "files", "calculating…".to_string());
            push(&mut lines, "size", "calculating…".to_string());
        }
        Some(DirContent::Ready { files, dirs, total_bytes }) => {
            push(&mut lines, "files", format!("{files}"));
            push(&mut lines, "dirs", format!("{dirs}"));
            push(&mut lines, "total size", format_bytes_compact(*total_bytes));
        }
        Some(DirContent::Error(e)) => {
            push(&mut lines, "files", format!("error: {e}"));
        }
    }

    lines.push(Line::from(""));
    lines.push(Line::from(Span::styled(
        "  any key to close",
        Style::default().fg(theme.div_line).add_modifier(Modifier::DIM),
    )));

    let modal_w = (area.width / 2).max(42).min(area.width.saturating_sub(4));
    let modal_h = (lines.len() as u16 + 2).min(area.height.saturating_sub(4));
    let bg = theme.main_bg.unwrap_or(Color::Black);
    let panel_widget = BoxedPanel::new(theme.panel_accents[1], theme.title)
        .with_title("info")
        .flat();
    let shell = ModalShell::new(panel_widget, modal_w, modal_h)
        .with_fill(Style::default().bg(bg).fg(theme.main_fg));
    let Some(body) = shell.render(frame, area) else { return };
    let widget = ScrollableText::new(&lines, theme);
    frame.render_widget(&widget, body);
}

fn draw_branch_overlay(app: &App, frame: &mut Frame<'_>, theme: &Theme, area: Rect) {
    use ratatui::style::Modifier;
    let Some(ov) = app.branch_overlay() else { return };

    let modal_w = (area.width * 2 / 3).max(40).min(area.width.saturating_sub(4));
    let modal_h = (ov.branches.len() as u16 + 4).min(area.height.saturating_sub(4));
    let bg = theme.main_bg.unwrap_or(Color::Black);

    let panel = BoxedPanel::new(theme.panel_accents[1], theme.title)
        .with_title("branches")
        .with_controls("↑↓ select  ·  Enter checkout  ·  Esc close")
        .flat();
    let shell = ModalShell::new(panel, modal_w, modal_h)
        .with_fill(Style::default().bg(bg).fg(theme.main_fg));
    let Some(body) = shell.render(frame, area) else { return };

    let sel_style = Style::default().bg(theme.selected_bg).fg(theme.hi_fg);
    let dim_style = Style::default().fg(theme.main_fg).add_modifier(Modifier::DIM);
    let cur_style = Style::default().fg(theme.panel_accents[1]);

    let mut lines: Vec<Line<'static>> = Vec::new();
    for (i, branch) in ov.branches.iter().enumerate() {
        let is_selected = i == ov.selected;
        let is_current = ov.current.as_deref() == Some(branch.as_str());
        let prefix = if is_selected { "▸ " } else { "  " };
        let suffix = if is_current { "  (current)" } else { "" };
        let (base_style, suf_style) = if is_selected {
            (sel_style, sel_style)
        } else if is_current {
            (cur_style, dim_style)
        } else {
            (Style::default().fg(theme.main_fg), dim_style)
        };
        lines.push(Line::from(vec![
            ratatui::text::Span::styled(format!("{prefix}{branch}"), base_style),
            ratatui::text::Span::styled(suffix.to_string(), suf_style),
        ]));
    }
    frame.render_widget(&ScrollableText::new(&lines, theme), body);
}

fn draw_list_pane(app: &App, frame: &mut Frame<'_>, theme: &Theme, area: Rect) {
    if area.width == 0 || area.height == 0 {
        return;
    }
    if let Some(finder) = app.finder() {
        draw_finder_pane(app, finder, frame, theme, area);
        return;
    }
    let title = app.cwd_display();
    let bcol = border_color(theme, app.focus() == Focus::List, theme.panel_accents[3]);
    // Right-side status pill: while typing the filter, echo the input
    // with a trailing cursor mark. Otherwise show the applied filter
    // (if any) followed by the visible item count so the user can
    // tell at a glance how aggressive the filter is.
    let controls = if let Some(input) = app.filter_input() {
        format!("/ {}▏", input)
    } else if let Some(f) = app.filter() {
        format!("/ {}  {} items", f, app.entries().len())
    } else {
        format!("{} items", app.entries().len())
    };
    let panel = BoxedPanel::new(bcol, theme.title)
        .with_title(format!("📁  {}", title))
        .with_controls(controls);
    frame.render_widget(&panel, area);
    let inner = panel.inner(area);
    if inner.height == 0 {
        return;
    }
    let columns = list_columns(inner.width);
    let rows = build_rows(app.entries(), app.git_state());
    let table = Table::new(&columns, &rows, theme)
        .with_selection(Some(app.nav().cursor), app.nav().scroll);
    frame.render_widget(&table, inner);
}

fn draw_finder_pane(
    app: &App,
    finder: &crate::app::FinderState,
    frame: &mut Frame<'_>,
    theme: &Theme,
    area: Rect,
) {
    let bcol = border_color(theme, app.focus() == Focus::List, theme.panel_accents[3]);
    let panel = BoxedPanel::new(bcol, theme.title)
        .with_title(format!("🔍 find: {}▏", finder.input))
        .with_controls(format!("{} results", finder.results.len()));
    frame.render_widget(&panel, area);
    let inner = panel.inner(area);
    if inner.height == 0 {
        return;
    }
    let cols = vec![Column::new("", u16::MAX)];
    let rows = build_find_rows(&finder.results);
    let table = Table::new(&cols, &rows, theme)
        .with_selection(Some(finder.cursor), finder.scroll);
    frame.render_widget(&table, inner);
}

fn build_find_rows(results: &[FindResult]) -> Vec<Row<'static>> {
    results
        .iter()
        .map(|r| {
            let prefix = if r.is_dir { "📁 " } else { "   " };
            Row::data(vec![Cell::new(format!("{}{}", prefix, r.rel.display()))])
        })
        .collect()
}

fn draw_parent_pane(app: &App, frame: &mut Frame<'_>, theme: &Theme, area: Rect) {
    if area.width == 0 || area.height == 0 {
        return;
    }
    // Title shows just the parent's basename — it's already implied
    // by the current pane's full path, no need to repeat it.
    let title = app
        .parent_entries()
        .first()
        .and_then(|e| e.path.parent())
        .and_then(|p| p.file_name())
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_else(|| "/".into());
    let panel = BoxedPanel::new(theme.div_line, theme.title).with_title(format!("⌂ {}", title));
    frame.render_widget(&panel, area);
    let inner = panel.inner(area);
    if inner.height == 0 {
        return;
    }
    if app.parent_entries().is_empty() {
        return;
    }
    // Single name-only column — the parent pane is informational and
    // usually narrow. Adding size/mtime columns wastes space and
    // duplicates info shown in the current pane.
    let cols = vec![Column::new("", u16::MAX)];
    let rows = build_rows_name_only(app.parent_entries());
    // Auto-scroll so the cwd row stays visible, mirroring the list
    // pane's behavior.
    let scroll = match app.parent_cursor() {
        Some(idx) => {
            let h = inner.height.saturating_sub(1) as usize;
            if h > 0 && idx >= h {
                idx + 1 - h
            } else {
                0
            }
        }
        None => 0,
    };
    let table = Table::new(&cols, &rows, theme).with_selection(app.parent_cursor(), scroll);
    frame.render_widget(&table, inner);
}

fn draw_preview_pane(app: &App, frame: &mut Frame<'_>, theme: &Theme, area: Rect) {
    if area.width == 0 || area.height == 0 {
        return;
    }
    // Editor takes over the preview pane when active *and* not in
    // modal-fullscreen — the modal renderer handles that case.
    if app.editor().is_some() && !app.is_full_preview() {
        draw_editor_pane(app, frame, theme, area);
        return;
    }
    // Tree mode + DB-table selection: render preview rows as a
    // LiveTable instead of the file-content pipeline. Endpoints /
    // databases / schemas don't have a row preview, so the panel
    // just shows a hint.
    if let Some(preview) = app.db_preview() {
        draw_db_preview(preview, frame, theme, area);
        return;
    }
    let preview_title = app
        .selected()
        .map(|e| e.name.clone())
        .unwrap_or_else(|| "—".to_string());
    let (controls, lines): (String, Vec<Line<'static>>) = match app.preview_state() {
        crate::preview::PreviewState::None => ("—".into(), vec![Line::from("(no selection)")]),
        crate::preview::PreviewState::Loading(_) => ("loading…".into(), vec![Line::from("…")]),
        crate::preview::PreviewState::Error { message, .. } => (
            "error".into(),
            vec![
                Line::from("preview failed:"),
                Line::from(""),
                Line::from(message.clone()),
            ],
        ),
        crate::preview::PreviewState::Ready { preview, .. } => {
            let mut hint = match preview.kind {
                crate::preview::PreviewKind::Text => format!("{} lines", preview.source_lines),
                crate::preview::PreviewKind::Markdown => "markdown".into(),
                crate::preview::PreviewKind::Image => format!("image · {}", app.image_backend().label()),
                crate::preview::PreviewKind::Empty => "empty".into(),
                crate::preview::PreviewKind::TooLarge => "too large".into(),
                crate::preview::PreviewKind::Binary => "binary".into(),
                crate::preview::PreviewKind::Directory => format!("{} entries", preview.source_lines),
            };
            if let Some(note) = &preview.note {
                hint.push_str("  ");
                hint.push_str(note);
            }
            // Defer body extraction until we know the inner rect (image
            // bodies need it for half-block rasterization). Hand off via
            // a sentinel marker; resolve below.
            let body = match &preview.body {
                crate::preview::PreviewBody::Lines(v) => v.clone(),
                crate::preview::PreviewBody::Image(_) => Vec::new(),
            };
            (hint, body)
        }
    };
    // Match the list pane's focused accent (proc_box) so the two
    // panes read at equal brightness — the preview's prior `hi_fg`
    // was dimmer on most themes and made the focus state ambiguous.
    let bcol = border_color(theme, app.focus() == Focus::Preview, theme.panel_accents[3]);
    let panel = BoxedPanel::new(bcol, theme.title)
        .with_title(preview_title)
        .with_controls(controls);
    frame.render_widget(&panel, area);
    let inner = panel.inner(area);
    if inner.height == 0 {
        return;
    }
    render_preview_body(app, frame, theme, inner, lines, app.preview_scroll());
}

/// Resolve the final preview body into a rendered `Vec<Line>` for the
/// given inner rect — handling image rasterization, scroll clamping,
/// and the line-number gutter — and dispatch to `ScrollableText`.
/// Shared between the regular preview pane and the full-screen modal;
/// callers pass the scroll value from whichever state they own.
fn render_preview_body(
    app: &App,
    frame: &mut Frame<'_>,
    theme: &Theme,
    inner: Rect,
    fallback_lines: Vec<Line<'static>>,
    raw_scroll: usize,
) {
    let final_lines: Vec<Line<'static>> = match app.preview_state() {
        crate::preview::PreviewState::Ready { preview, .. } => match &preview.body {
            crate::preview::PreviewBody::Image(img) => {
                if app.image_backend() == ImageBackend::Native {
                    // Native protocols paint the bitmap as an overlay
                    // *after* terminal.draw() flushes. Leave the body
                    // empty so ratatui's diff renderer doesn't write
                    // changing content into those cells (which would
                    // otherwise erase the protocol's image).
                    Vec::new()
                } else {
                    rasterize_image(img, inner)
                }
            }
            _ => fallback_lines,
        },
        _ => fallback_lines,
    };
    let scroll = if matches!(
        app.preview_state(),
        crate::preview::PreviewState::Ready { preview, .. } if matches!(preview.body, crate::preview::PreviewBody::Image(_))
    ) {
        0
    } else {
        raw_scroll.min(final_lines.len().saturating_sub(1))
    };
    let show_line_numbers = matches!(
        app.preview_state(),
        crate::preview::PreviewState::Ready { preview, .. }
            if preview.kind == crate::preview::PreviewKind::Text
    );
    let preview = ScrollableText::new(&final_lines, theme)
        .with_scroll(scroll)
        .with_line_numbers(show_line_numbers);
    frame.render_widget(&preview, inner);
}

/// Centered full-screen modal that renders the preview at ~90% of the
/// frame area. The underlying panes still draw beneath it so the user
/// keeps spatial context. Body lines are sized via the same path as
/// the normal preview pane — image bodies re-rasterize at the modal's
/// rect for a much higher-resolution view.
fn draw_editor_pane(app: &App, frame: &mut Frame<'_>, theme: &Theme, area: Rect) {
    let Some(editor) = app.editor() else { return };
    let bcol = theme.panel_accents[3]; // editor always has focus implicitly
    let dirty_mark = if editor.dirty { " ●" } else { "" };
    let title = format!("✎ {}{}", editor.name(), dirty_mark);
    let controls = format!(
        "Ln {}  Col {}  ·  ^S save  ^X exit",
        editor.cursor.0 + 1,
        editor.cursor.1 + 1
    );
    let panel = BoxedPanel::new(bcol, theme.title)
        .with_title(title)
        .with_controls(controls);
    frame.render_widget(&panel, area);
    let inner = panel.inner(area);
    if inner.height == 0 {
        return;
    }
    render_editor_body(app, frame, theme, inner);
}

fn render_editor_body(app: &App, frame: &mut Frame<'_>, theme: &Theme, inner: Rect) {
    let Some(editor) = app.editor() else { return };
    let widget = EditableText::new(&editor.lines, editor.cursor, theme)
        .with_scroll(editor.scroll_row, editor.scroll_col)
        .with_line_numbers(true)
        .with_styled(&editor.highlighted);
    if let Some((cx, cy)) = widget.cursor_screen_xy(inner) {
        frame.set_cursor_position((cx, cy));
    }
    frame.render_widget(&widget, inner);
}

fn draw_editor_fullscreen(app: &App, frame: &mut Frame<'_>, theme: &Theme, area: Rect) {
    let Some(editor) = app.editor() else { return };
    let (content, bar_rect) = split_top_bottom(area, 1);
    let dirty_mark = if editor.dirty { " ●" } else { "" };
    let controls = format!(
        "Ln {}  Col {}  ·  ^S save  ^X exit  ·  Space close",
        editor.cursor.0 + 1,
        editor.cursor.1 + 1
    );
    let bar = ActionBar::new(vec![
        (format!("✎ {}{}", editor.name(), dirty_mark), controls),
    ])
    .with_colors(theme.div_line, theme.hi_fg, theme.main_fg, theme.selected_bg);
    frame.render_widget(&bar, bar_rect);
    render_editor_body(app, frame, theme, content);
}

fn draw_preview_fullscreen(app: &App, frame: &mut Frame<'_>, theme: &Theme, area: Rect) {
    let (content, bar_rect) = split_top_bottom(area, 1);
    let name = app
        .selected()
        .map(|e| e.name.clone())
        .unwrap_or_else(|| "preview".to_string());
    let bar = ActionBar::new(vec![
        (name, String::new()),
        ("↑↓".into(), "scroll".into()),
        ("Space/Esc".into(), "close".into()),
    ])
    .with_colors(theme.div_line, theme.hi_fg, theme.main_fg, theme.selected_bg);
    frame.render_widget(&bar, bar_rect);
    let fallback = lines_for_fullscreen(app);
    render_preview_body(app, frame, theme, content, fallback, app.modal_scroll());
}

fn lines_for_fullscreen(app: &App) -> Vec<Line<'static>> {
    match app.preview_state() {
        crate::preview::PreviewState::None => vec![Line::from("(no selection)")],
        crate::preview::PreviewState::Loading(_) => vec![Line::from("…")],
        crate::preview::PreviewState::Error { message, .. } => vec![
            Line::from("preview failed:"),
            Line::from(""),
            Line::from(message.clone()),
        ],
        crate::preview::PreviewState::Ready { preview, .. } => match &preview.body {
            crate::preview::PreviewBody::Lines(v) => v.clone(),
            crate::preview::PreviewBody::Image(_) => Vec::new(),
        },
    }
}

/// Split `area` vertically at `num/den` of its height.
/// Returns (top strip, remainder).
fn split_at_fraction(area: Rect, num: u16, den: u16) -> (Rect, Rect) {
    let top_h = ((area.height as u32 * num as u32) / den as u32) as u16;
    let top_h = top_h.max(1).min(area.height.saturating_sub(1));
    let top = Rect::new(area.x, area.y, area.width, top_h);
    let bot = Rect::new(area.x, area.y + top_h, area.width, area.height.saturating_sub(top_h));
    (top, bot)
}

/// Render the query editor right-panel inside DB mode.
/// The panel layout is controlled by `qe.layout` and cycled by Tab.
fn draw_query_pane(app: &App, frame: &mut Frame<'_>, theme: &Theme, area: Rect) {
    use gtui::widgets::{panel, CornerStyle};
    let Some(qe) = app.query_editor() else { return };
    if area.width == 0 || area.height == 0 { return; }

    let (editor_area, results_area) = match qe.layout {
        crate::app::QueryPaneLayout::EditorOnly =>
            (area, Rect::new(area.x, area.y + area.height, area.width, 0)),
        crate::app::QueryPaneLayout::ResultsOnly =>
            (Rect::new(area.x, area.y, area.width, 0), area),
        crate::app::QueryPaneLayout::Split =>
            split_at_fraction(area, 35, 100),
    };

    let accent = theme.panel_accents[2];

    // ── SQL editor panel ─────────────────────────────────────────────
    if editor_area.height > 0 {
        let layout_hint = match qe.layout {
            crate::app::QueryPaneLayout::EditorOnly => "Tab → split",
            crate::app::QueryPaneLayout::Split => "Tab → results",
            crate::app::QueryPaneLayout::ResultsOnly => "",
        };
        let p = panel(accent, theme.title, CornerStyle::default())
            .with_title("query")
            .with_controls(layout_hint.to_string());
        frame.render_widget(&p, editor_area);
        let inner = p.inner(editor_area);
        if inner.height > 0 && inner.width > 0 {
            let widget = EditableText::new(&qe.sql, qe.cursor, theme)
                .with_scroll(qe.scroll_row, 0)
                .with_line_numbers(true);
            if let Some((cx, cy)) = widget.cursor_screen_xy(inner) {
                frame.set_cursor_position((cx, cy));
            }
            frame.render_widget(&widget, inner);
        }
    }

    // ── Results panel ────────────────────────────────────────────────
    if results_area.height > 0 {
        let (title, controls) = match &qe.result {
            crate::app::QueryResultState::None =>
                ("results".to_string(), "^R to run".to_string()),
            crate::app::QueryResultState::Running =>
                ("results".to_string(), "running…".to_string()),
            crate::app::QueryResultState::Error(_) =>
                ("results".to_string(), "error".to_string()),
            crate::app::QueryResultState::Ready { rows, elapsed_ms, columns } => {
                let n = rows.len();
                let row_label = if n == 1 { "1 row".to_string() } else { format!("{n} rows") };
                let col_info = if columns.len() > 1 {
                    format!("col {}/{}", qe.col_scroll + 1, columns.len())
                } else {
                    format!("{} col", columns.len())
                };
                let scroll_hint = if matches!(qe.layout, crate::app::QueryPaneLayout::ResultsOnly) {
                    "  ·  ←→ cols  ·  Tab → editor"
                } else {
                    "  ·  Tab → editor"
                };
                (
                    col_info,
                    format!("{row_label}  ·  {elapsed_ms}ms{scroll_hint}"),
                )
            }
        };
        let p = panel(accent, theme.title, CornerStyle::default())
            .with_title(title)
            .with_controls(controls);
        frame.render_widget(&p, results_area);
        let inner = p.inner(results_area);
        if inner.height > 0 && inner.width > 0 {
            render_query_results(qe, frame, theme, inner);
        }
    }
}

fn render_query_results(
    qe: &crate::app::QueryEditorState,
    frame: &mut Frame<'_>,
    theme: &Theme,
    area: Rect,
) {
    use ratatui::style::Modifier;
    match &qe.result {
        crate::app::QueryResultState::None => {
            let lines = vec![Line::from(Span::styled(
                "  ^R to run",
                Style::default().fg(theme.div_line).add_modifier(Modifier::DIM),
            ))];
            frame.render_widget(&ScrollableText::new(&lines, theme), area);
        }
        crate::app::QueryResultState::Running => {
            let lines = vec![Line::from(Span::styled(
                "  running…",
                Style::default().fg(theme.div_line).add_modifier(Modifier::DIM),
            ))];
            frame.render_widget(&ScrollableText::new(&lines, theme), area);
        }
        crate::app::QueryResultState::Error(msg) => {
            let mut lines: Vec<Line<'static>> = Vec::new();
            for l in msg.lines() {
                lines.push(Line::from(Span::styled(
                    format!("  {l}"),
                    Style::default().fg(theme.main_fg),
                )));
            }
            frame.render_widget(&ScrollableText::new(&lines, theme), area);
        }
        crate::app::QueryResultState::Ready { columns, rows, .. } => {
            if columns.is_empty() {
                let lines = vec![Line::from(Span::styled(
                    "  (no columns)",
                    Style::default().fg(theme.div_line).add_modifier(Modifier::DIM),
                ))];
                frame.render_widget(&ScrollableText::new(&lines, theme), area);
                return;
            }
            // Pick a comfortable per-column width based on the widest header
            // or cell value, capped so narrow terminals still work.
            let col_w = columns_natural_width(columns, rows, area.width);
            // Determine which columns fit starting from col_scroll.
            let col_start = qe.col_scroll.min(columns.len().saturating_sub(1));
            let mut visible_cols = 0usize;
            let mut used = 0u16;
            for w in columns.iter().skip(col_start).map(|_| col_w) {
                if used + w > area.width { break; }
                used += w;
                visible_cols += 1;
            }
            visible_cols = visible_cols.max(1);
            let col_end = (col_start + visible_cols).min(columns.len());

            let gtui_cols: Vec<gtui::Column> = columns[col_start..col_end]
                .iter()
                .map(|name| gtui::Column::new(name.as_str(), col_w))
                .collect();
            let gtui_rows: Vec<gtui::Row<'static>> = rows
                .iter()
                .skip(qe.result_scroll)
                .map(|r| {
                    let cells: Vec<Cell<'static>> = r.cells
                        .iter()
                        .skip(col_start)
                        .take(col_end - col_start)
                        .map(|c| Cell::new(c.clone()))
                        .collect();
                    gtui::Row::data(cells)
                })
                .collect();
            frame.render_widget(&gtui::Table::new(&gtui_cols, &gtui_rows, theme), area);
        }
    }
}

/// Rasterize `img` into `area` using Unicode sextant blocks — each cell
/// encodes a 2×3 grid of sub-pixels (6 per cell, ~3× the density of
/// half-blocks). Each cell still has only fg + bg, so for every cell we
/// pick the best 2-color partition of its 6 sub-pixels by sorting on
/// luminance and trying each split point (chafa's standard trick).
/// Aspect ratio is preserved: a sub-pixel is ~0.5 cell-cols wide and
/// ~2/3 cell-row tall, and a cell-row is ~2× taller than a cell-col,
/// so the effective sub-pixel aspect (h/w) is 4/3.
fn rasterize_image(img: &image::DynamicImage, area: Rect) -> Vec<Line<'static>> {
    use image::imageops::FilterType;
    if area.width == 0 || area.height == 0 {
        return Vec::new();
    }
    let cells_w = area.width as u32;
    let cells_h = area.height as u32;
    let sub_w = cells_w.saturating_mul(2);
    let sub_h = cells_h.saturating_mul(3);
    let (iw, ih) = img.dimensions();
    if iw == 0 || ih == 0 || sub_w == 0 || sub_h == 0 {
        return Vec::new();
    }
    // Sub-pixel real aspect (height/width) ≈ 4/3 because cell rows are
    // ~2× taller than cell columns and we split rows into 3 vs cols
    // into 2. To make a square image render square, the rendered image
    // in *sub-pixels* must satisfy w/h = native_w/native_h × 4/3.
    let sub_aspect: f32 = 4.0 / 3.0;
    let scale_w = sub_w as f32 / iw as f32;
    let scale_h = (sub_h as f32 / sub_aspect) / ih as f32;
    let scale = scale_w.min(scale_h);
    let mut tw = ((iw as f32 * scale).round() as u32).max(2).min(sub_w);
    let mut th = ((ih as f32 * scale * sub_aspect).round() as u32)
        .max(3)
        .min(sub_h);
    // Clamp to multiples of 2/3 so the sextant sub-pixel grouping is
    // exact — otherwise the bottom row would mismatch.
    tw -= tw % 2;
    th -= th % 3;
    if tw == 0 || th == 0 {
        return Vec::new();
    }
    let resized = img
        .resize_exact(tw, th, FilterType::Lanczos3)
        .to_rgb8();
    let cell_cols = tw / 2;
    let cell_rows = th / 3;
    let pad_left = cells_w.saturating_sub(cell_cols) / 2;

    let mut lines: Vec<Line<'static>> = Vec::with_capacity(cell_rows as usize);
    for cy in 0..cell_rows {
        let mut spans: Vec<Span<'static>> = Vec::with_capacity((pad_left + cell_cols + 1) as usize);
        if pad_left > 0 {
            spans.push(Span::raw(" ".repeat(pad_left as usize)));
        }
        for cx in 0..cell_cols {
            let pxs: [[u8; 3]; 6] = [
                resized.get_pixel(cx * 2, cy * 3).0,
                resized.get_pixel(cx * 2 + 1, cy * 3).0,
                resized.get_pixel(cx * 2, cy * 3 + 1).0,
                resized.get_pixel(cx * 2 + 1, cy * 3 + 1).0,
                resized.get_pixel(cx * 2, cy * 3 + 2).0,
                resized.get_pixel(cx * 2 + 1, cy * 3 + 2).0,
            ];
            let (glyph, fg, bg) = pick_sextant(&pxs);
            spans.push(Span::styled(
                glyph.to_string(),
                Style::default()
                    .fg(Color::Rgb(fg[0], fg[1], fg[2]))
                    .bg(Color::Rgb(bg[0], bg[1], bg[2])),
            ));
        }
        lines.push(Line::from(spans));
    }
    lines
}

/// Pick the sextant glyph + fg/bg colors that best represent these 6
/// sub-pixels. Algorithm:
/// 1. Sort the 6 pixels by luminance.
/// 2. For each split point k ∈ 0..=6, treat the darker `k` as bg and
///    the brighter `6-k` as fg; compute within-cluster SSE.
/// 3. Pick the k with min SSE; build the bit pattern (1 = pixel in
///    bright cluster) and look up the matching glyph.
fn pick_sextant(pxs: &[[u8; 3]; 6]) -> (char, [u8; 3], [u8; 3]) {
    fn lum(p: [u8; 3]) -> f32 {
        0.2126 * p[0] as f32 + 0.7152 * p[1] as f32 + 0.0722 * p[2] as f32
    }
    let lums: [f32; 6] = std::array::from_fn(|i| lum(pxs[i]));
    let mut order: [usize; 6] = [0, 1, 2, 3, 4, 5];
    order.sort_by(|&a, &b| {
        lums[a]
            .partial_cmp(&lums[b])
            .unwrap_or(std::cmp::Ordering::Equal)
    });

    let mut best: Option<(usize, f32, [u8; 3], [u8; 3])> = None;
    for k in 0..=6 {
        let dark = &order[..k];
        let bright = &order[k..];
        let dark_mean = mean_color(pxs, dark);
        let bright_mean = mean_color(pxs, bright);
        let err = sse(pxs, dark, dark_mean) + sse(pxs, bright, bright_mean);
        if best.map_or(true, |(_, e, _, _)| err < e) {
            best = Some((k, err, dark_mean, bright_mean));
        }
    }
    let (k, _, dark_mean, bright_mean) = best.unwrap();
    let mut pattern: u8 = 0;
    for &i in &order[k..] {
        pattern |= 1 << i;
    }
    (sextant_char(pattern), bright_mean, dark_mean)
}

fn mean_color(pxs: &[[u8; 3]; 6], idx: &[usize]) -> [u8; 3] {
    if idx.is_empty() {
        return [0, 0, 0];
    }
    let mut sum = [0u32; 3];
    for &i in idx {
        sum[0] += pxs[i][0] as u32;
        sum[1] += pxs[i][1] as u32;
        sum[2] += pxs[i][2] as u32;
    }
    let n = idx.len() as u32;
    [(sum[0] / n) as u8, (sum[1] / n) as u8, (sum[2] / n) as u8]
}

fn sse(pxs: &[[u8; 3]; 6], idx: &[usize], mean: [u8; 3]) -> f32 {
    let mut total = 0.0f32;
    for &i in idx {
        for c in 0..3 {
            let d = pxs[i][c] as f32 - mean[c] as f32;
            total += d * d;
        }
    }
    total
}

/// Map a 6-bit sextant pattern to a Unicode codepoint.
///
/// Bit assignment: 0=top-left, 1=top-right, 2=mid-left, 3=mid-right,
/// 4=bot-left, 5=bot-right. The Unicode block U+1FB00..=U+1FB3B holds
/// 60 sextant glyphs covering all patterns *except* the four that
/// already had codepoints elsewhere: empty (0=' '), left-half
/// (0b010101=21=▌), right-half (0b101010=42=▐), and full
/// (0b111111=63=█).
fn sextant_char(pattern: u8) -> char {
    match pattern {
        0 => ' ',
        21 => '▌',
        42 => '▐',
        63 => '█',
        p => {
            let mut offset = (p - 1) as u32;
            if p > 21 {
                offset -= 1;
            }
            if p > 42 {
                offset -= 1;
            }
            char::from_u32(0x1FB00 + offset).unwrap_or('?')
        }
    }
}

fn draw_action_bar(app: &App, frame: &mut Frame<'_>, theme: &Theme, area: Rect) {
    if area.height == 0 {
        return;
    }
    if matches!(app.view_mode, crate::app::ViewMode::Database) {
        let bar = if let Some(msg) = app.status_message() {
            ActionBar::new(vec![("·".into(), msg.to_string())])
        } else if app.is_query_editor_active() {
            let qe = app.query_editor().unwrap();
            let layout_label = match qe.layout {
                crate::app::QueryPaneLayout::EditorOnly => "split",
                crate::app::QueryPaneLayout::Split => "results",
                crate::app::QueryPaneLayout::ResultsOnly => "editor",
            };
            let row_hint = match &qe.result {
                crate::app::QueryResultState::Ready { rows, elapsed_ms, .. } =>
                    format!("{}  {}ms", rows.len(), elapsed_ms),
                crate::app::QueryResultState::Running => "running…".into(),
                _ => String::new(),
            };
            let mut chips = vec![
                ("^R".into(), "run".into()),
                ("Tab".into(), layout_label.into()),
            ];
            if !row_hint.is_empty() {
                chips.push(("·".into(), row_hint));
            }
            chips.push(("Esc".into(), "close query".into()));
            chips.push(("q".into(), "quit".into()));
            ActionBar::new(chips)
        } else {
            ActionBar::new(vec![
                ("l".into(), "drill in".into()),
                ("Enter".into(), "preview".into()),
                ("h".into(), "back".into()),
                ("e".into(), "query".into()),
                ("x".into(), "drop".into()),
                ("r".into(), "refresh".into()),
                ("D/Esc".into(), "files".into()),
                ("q".into(), "quit".into()),
            ])
        };
        let bar = bar.with_colors(theme.div_line, theme.hi_fg, theme.main_fg, theme.selected_bg);
        frame.render_widget(&bar, area);
        return;
    }
    if let Some(editor) = app.editor() {
        // Editor-specific bar: nano-style hints. Flash overrides the
        // hints while a status (e.g. "wrote ...") is active.
        let bar = if let Some(msg) = editor.message.as_deref() {
            ActionBar::new(vec![("·".into(), msg.to_string())])
        } else {
            ActionBar::new(vec![
                ("^S".into(), "save".into()),
                ("^X".into(), "exit".into()),
                ("^A/^E".into(), "line".into()),
                ("^T/^G".into(), "top/bot".into()),
                ("^W".into(), "del word".into()),
                ("^K/^U".into(), "kill EOL/BOL".into()),
                ("PgUp/PgDn".into(), "page".into()),
                ("Space".into(), "fullscreen".into()),
            ])
        };
        let bar = bar.with_colors(theme.div_line, theme.hi_fg, theme.main_fg, theme.selected_bg);
        frame.render_widget(&bar, area);
        return;
    }
    // Transient op-result flashes preempt the normal hint chips so
    // the user sees feedback without losing the row to a tooltip.
    if let Some(msg) = app.status_message() {
        let bar = ActionBar::new(vec![("·".into(), msg.to_string())]).with_colors(
            theme.div_line,
            theme.hi_fg,
            theme.main_fg,
            theme.selected_bg,
        );
        frame.render_widget(&bar, area);
        return;
    }
    let _move_label: &str = match app.focus() {
        Focus::List => "move",
        Focus::Preview => "scroll",
    };
    if app.is_finder_active() {
        let bar = ActionBar::new(vec![
            ("type".into(), "filter".into()),
            ("↑↓".into(), "select".into()),
            ("Enter".into(), "open".into()),
            ("Esc".into(), "close".into()),
        ])
        .with_colors(theme.div_line, theme.hi_fg, theme.main_fg, theme.selected_bg);
        frame.render_widget(&bar, area);
        return;
    }
    if matches!(app.view_mode, crate::app::ViewMode::Tree) {
        let actions = vec![
            ("Space".into(), "fullscreen".into()),
            ("i".into(), "info".into()),
            ("/".into(), "filter".into()),
            (":".into(), "cmd".into()),
            ("T".into(), "miller".into()),
            ("D".into(), "db".into()),
            ("s/S".into(), "sort col".into()),
            ("R".into(), "reverse".into()),
            ("e".into(), "edit".into()),
            (".".into(), "hidden".into()),
            ("g…".into(), "jump".into()),
            ("q".into(), "quit".into()),
        ];
        let bar = ActionBar::new(actions)
            .with_colors(theme.div_line, theme.hi_fg, theme.main_fg, theme.selected_bg);
        frame.render_widget(&bar, area);
        return;
    }
    let actions: Vec<(String, String)> = if app.jump_pending() {
        // While the chord is pending the action bar shifts to a hint
        // for which keys are valid second-presses. Mirrors what yazi
        // does to make the chord feel discoverable.
        vec![
            ("g…".into(), "jump:".into()),
            ("g".into(), "top".into()),
            ("G".into(), "bottom".into()),
            ("h".into(), "home".into()),
            ("/".into(), "/".into()),
            ("t".into(), "/tmp".into()),
            ("r".into(), "repo".into()),
            ("Esc".into(), "cancel".into()),
        ]
    } else {
        vec![
            ("Space".into(), "fullscreen".into()),
            ("i".into(), "info".into()),
            ("B".into(), "branches".into()),
            ("/".into(), "filter".into()),
            (":".into(), "cmd".into()),
            ("T".into(), "tree".into()),
            ("D".into(), "db".into()),
            ("x".into(), "trash".into()),
            ("r".into(), "rename".into()),
            ("a".into(), "new".into()),
            ("e".into(), "edit".into()),
            (".".into(), "hidden".into()),
            ("g…".into(), "jump".into()),
            ("q".into(), "quit".into()),
        ]
    };
    let bar = ActionBar::new(actions).with_colors(
        theme.div_line,
        theme.hi_fg,
        theme.main_fg,
        theme.selected_bg,
    );
    frame.render_widget(&bar, area);
}

fn list_columns(width: u16) -> Vec<Column<'static>> {
    // Drop the Modified column when the pane is too narrow for it to
    // be useful — the Name column would otherwise truncate aggressively
    // and the user can still see mtime in the parent pane row.
    if width < 36 {
        vec![
            Column::new("Name", u16::MAX),
            Column::new("Size", 9).right_aligned(true),
        ]
    } else {
        vec![
            Column::new("Name", u16::MAX),
            Column::new("Size", 9).right_aligned(true),
            Column::new("Modified", 12).right_aligned(true),
        ]
    }
}

fn build_rows(
    entries: &[crate::fs::entry::FsEntry],
    git: &crate::app::GitState,
) -> Vec<Row<'static>> {
    entries
        .iter()
        .map(|e| {
            let name = decorated_name(e);
            let size = if e.is_dir() {
                "—".to_string()
            } else {
                format_bytes_compact(e.size)
            };
            let mtime = format_relative_mtime(e.mtime);
            let name_cell = git_name_cell(name, &e.path, git);
            Row::data(vec![
                name_cell,
                Cell::new(size),
                Cell::new(mtime),
            ])
        })
        .collect()
}

fn build_rows_name_only(entries: &[crate::fs::entry::FsEntry]) -> Vec<Row<'static>> {
    entries
        .iter()
        .map(|e| Row::data(vec![Cell::new(decorated_name(e))]))
        .collect()
}

/// Build a Name cell with a colored git-status prefix glyph.
/// Uses `with_style` to color the whole cell since `table::Cell` holds plain text.
fn git_name_cell(
    name: String,
    path: &std::path::Path,
    git: &crate::app::GitState,
) -> Cell<'static> {
    use crate::app::GitFileStatus;
    match git.files.get(path) {
        None => Cell::new(name),
        Some(status) => {
            let (glyph, color) = match status {
                GitFileStatus::Modified  => ("M ", Color::Yellow),
                GitFileStatus::Added     => ("A ", Color::Green),
                GitFileStatus::Deleted   => ("D ", Color::Red),
                GitFileStatus::Renamed   => ("R ", Color::Cyan),
                GitFileStatus::Untracked => ("? ", Color::DarkGray),
                GitFileStatus::Staged    => ("S ", Color::LightGreen),
            };
            Cell::new(format!("{glyph}{name}"))
                .with_style(Style::default().fg(color))
        }
    }
}

fn decorated_name(e: &crate::fs::entry::FsEntry) -> String {
    match e.kind {
        crate::fs::entry::EntryKind::Dir => format!("📁 {}", e.name),
        crate::fs::entry::EntryKind::Symlink => format!("🔗 {}", e.name),
        _ => format!("   {}", e.name),
    }
}

/// Compact relative-time string ("3m", "5h", "12d", "2y"). Good enough
/// for v1; we can switch to a real date crate if users want absolute
/// timestamps in a column.
fn format_relative_mtime(t: Option<SystemTime>) -> String {
    let Some(t) = t else { return "-".to_string() };
    let secs = match SystemTime::now().duration_since(t) {
        Ok(d) => d.as_secs() as i64,
        Err(e) => -(e.duration().as_secs() as i64),
    };
    let s = secs.unsigned_abs();
    let suffix = if secs < 0 { "+" } else { "" };
    if s < 60 {
        format!("{}s{}", s, suffix)
    } else if s < 3600 {
        format!("{}m{}", s / 60, suffix)
    } else if s < 86_400 {
        format!("{}h{}", s / 3600, suffix)
    } else if s < 86_400 * 365 {
        format!("{}d{}", s / 86_400, suffix)
    } else {
        format!("{}y{}", s / (86_400 * 365), suffix)
    }
}

/// Schema preview for a DB table: column names and types, plus an
/// optional catalog-stats row count. No row data is fetched. The
/// table renders inside a nested boxed sub-panel so the column list
/// reads as a tabular sub-component of the outer "table info" pane
/// rather than naked text against the pane chrome.
fn draw_db_preview(
    preview: &crate::app::DbPreview,
    frame: &mut Frame<'_>,
    theme: &Theme,
    area: Rect,
) {
    let title = format!(
        "{}.{}.{}",
        preview.path.database.as_deref().unwrap_or("?"),
        preview.path.schema.as_deref().unwrap_or("?"),
        preview.path.table.as_deref().unwrap_or("?"),
    );
    let controls = match preview.row_count {
        Some(n) => format!("{} cols  ·  ~{} rows", preview.columns.len(), format_count(n)),
        None => format!("{} cols", preview.columns.len()),
    };
    // Right-pane preview accent matches the file browser's unfocused
    // preview pane (`div_line`) so the active-pane glow stays on the
    // CENTER column regardless of mode.
    let outer = BoxedPanel::new(theme.div_line, theme.title)
        .with_title(title)
        .with_controls(controls);
    frame.render_widget(&outer, area);
    let outer_inner = outer.inner(area);
    if outer_inner.width < 10 || outer_inner.height < 3 {
        return;
    }

    // ── Auto-size the inner box ─────────────────────────────────────────
    // Natural column widths come from the longest header label, name,
    // and type string. The nested box then sizes to content so a
    // two-column table doesn't sprawl across an 80-cell preview pane.
    let name_max = preview
        .columns
        .iter()
        .map(|c| c.name.chars().count())
        .max()
        .unwrap_or(0)
        .max("column".len());
    let type_max = preview
        .columns
        .iter()
        .map(|c| c.data_type.chars().count())
        .max()
        .unwrap_or(0)
        .max("type".len());
    let name_w = (name_max as u16).clamp(6, 32);
    let type_w = (type_max as u16).clamp(4, 24);
    // body width = name + space + type; inner-box width adds the two
    // border columns; outer padding adds another two.
    let pad_x: u16 = 1;
    let natural_body_w = name_w + 1 + type_w;
    let natural_box_w = natural_body_w + 2;
    let max_box_w = outer_inner.width.saturating_sub(pad_x * 2);
    let box_w = natural_box_w.min(max_box_w);

    // Inner height = header row + per-column rows + 2 border rows,
    // capped at outer_inner.height so it never overflows.
    let natural_box_h = (preview.columns.len() as u16).saturating_add(1).saturating_add(2);
    let box_h = natural_box_h.min(outer_inner.height);

    let inner_box_area = Rect::new(
        outer_inner.x.saturating_add(pad_x),
        outer_inner.y,
        box_w,
        box_h,
    );
    // `accent_subtle` (btop's proc_misc) is the suite's subdued accent —
    // a child element should read as a child, not compete with the
    // active-pane glow.
    let inner_panel = BoxedPanel::new(theme.accent_subtle, theme.title)
        .with_title("schema".to_string());
    frame.render_widget(&inner_panel, inner_box_area);
    let body = inner_panel.inner(inner_box_area);
    if body.width < 6 || body.height < 1 {
        return;
    }

    // Re-derive column widths from the actually-available body width
    // in case clamping reduced it below the natural sizes.
    let body_type_w = (body.width / 3).min(type_w).max(4);
    let body_name_w = body.width.saturating_sub(body_type_w + 1);
    let cols = vec![
        Column::new("column", body_name_w),
        Column::new("type", body_type_w).right_aligned(true),
    ];
    let rows: Vec<Row<'static>> = preview
        .columns
        .iter()
        .map(|c| {
            Row::data(vec![
                Cell::new(c.name.clone()),
                Cell::new(c.data_type.clone()),
            ])
        })
        .collect();
    let table = Table::new(&cols, &rows, theme);
    frame.render_widget(&table, body);
}

/// Decompose a Unix epoch second into (year, month, day, hour, min, sec).
/// Proleptic Gregorian; good enough for display without pulling in chrono.
/// Choose a column width that fits content without wasting space.
/// Looks at header names and up to 50 rows, caps at 40, floors at 8.
fn columns_natural_width(
    columns: &[String],
    rows: &[crate::sources::db::Row],
    area_width: u16,
) -> u16 {
    let max_header = columns.iter().map(|c| c.len()).max().unwrap_or(8);
    let max_cell = rows.iter().take(50).flat_map(|r| r.cells.iter().map(|c| c.len())).max().unwrap_or(0);
    let natural = max_header.max(max_cell).max(8).min(40) as u16;
    // If all columns would fit at natural width, use it; otherwise keep it so
    // the user can scroll to see them at full fidelity.
    natural.max(8).min(area_width.saturating_sub(2))
}

fn epoch_to_ymd_hms(secs: u64) -> (u32, u32, u32, u32, u32, u32) {
    let s = secs % 60;
    let m = (secs / 60) % 60;
    let h = (secs / 3600) % 24;
    let days = secs / 86400;
    // Days since 1970-01-01 → Gregorian date (Fliegel–Van Flandern style).
    let z = days + 719468;
    let era = z / 146097;
    let doe = z % 146097;
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let mo = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = if mo <= 2 { y + 1 } else { y };
    (y as u32, mo as u32, d as u32, h as u32, m as u32, s as u32)
}

fn format_count(n: u64) -> String {
    if n >= 1_000_000_000 {
        format!("{:.1}B", n as f64 / 1_000_000_000.0)
    } else if n >= 1_000_000 {
        format!("{:.1}M", n as f64 / 1_000_000.0)
    } else if n >= 1_000 {
        format!("{:.1}K", n as f64 / 1_000.0)
    } else {
        n.to_string()
    }
}


#[cfg(test)]
mod tests {
    use super::*;

    /// Reserved patterns map to their established codepoints, not to
    /// the U+1FB00 range.
    #[test]
    fn sextant_reserved_patterns() {
        assert_eq!(sextant_char(0), ' ');
        assert_eq!(sextant_char(21), '▌');
        assert_eq!(sextant_char(42), '▐');
        assert_eq!(sextant_char(63), '█');
    }

    /// Spot-check the U+1FB00 block: pattern 1 (top-left only) is the
    /// first glyph; pattern 22 (the one immediately after the skipped
    /// pattern 21) lands at offset 20.
    #[test]
    fn sextant_block_offsets() {
        assert_eq!(sextant_char(1) as u32, 0x1FB00);
        assert_eq!(sextant_char(20) as u32, 0x1FB13);
        // Pattern 22 sits at offset 20 (skipped 21 = ▌).
        assert_eq!(sextant_char(22) as u32, 0x1FB14);
        // Pattern 43 sits at offset 40 (skipped 21 and 42).
        assert_eq!(sextant_char(43) as u32, 0x1FB28);
        assert_eq!(sextant_char(62) as u32, 0x1FB3B);
    }

    /// Uniform-color cell: any split has zero error, so the optimizer
    /// picks the first one — k=0 (all pixels to "bright"), giving
    /// glyph=█ with fg=color. Either way one of fg/bg must equal the
    /// uniform color and the glyph must be solid (█ or ' ').
    #[test]
    fn pick_sextant_uniform_cell() {
        let pxs = [[100u8, 50, 200]; 6];
        let (glyph, fg, bg) = pick_sextant(&pxs);
        let renders_as = (glyph == '█' && fg == [100, 50, 200])
            || (glyph == ' ' && bg == [100, 50, 200]);
        assert!(renders_as, "uniform cell rendered as {:?} fg={:?} bg={:?}", glyph, fg, bg);
    }

    /// Half-bright / half-dark: 3 white + 3 black sub-pixels split at
    /// k=3 with zero error. fg should be white, bg should be black.
    #[test]
    fn pick_sextant_split_half() {
        // top row white, mid + bottom rows black.
        let w = [255u8, 255, 255];
        let b = [0u8, 0, 0];
        let pxs = [w, w, b, b, b, b];
        let (_glyph, fg, bg) = pick_sextant(&pxs);
        assert_eq!(fg, w);
        assert_eq!(bg, b);
    }
}
