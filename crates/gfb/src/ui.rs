//! Drawing the file browser frame.
//!
//! Per the workspace rule, `gfb` consumes `gtui` widgets
//! exclusively — `ratatui::Frame` is used only to dispatch those widgets
//! to the terminal. If a primitive isn't covered by a `gtui` widget,
//! the answer is to add one there, not reach into `ratatui::widgets::*`.

use std::time::SystemTime;

use gtui::{
    format_bytes_compact, ActionBar, BoxedPanel, Cell, Column, ConfirmDialog, EditableText,
    MillerColumn, MillerColumns, ModalShell, Row, ScrollableText, Table, Theme,
};
use image::GenericImageView;
use ratatui::layout::Rect;
use ratatui::style::{Color, Style};
use ratatui::text::{Line, Span};
use ratatui::Frame;

use crate::app::{App, Focus, ImageBackend, InputModal};
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
    let (top, bottom) = split_top_bottom(area, 1);
    match app.view_mode {
        crate::app::ViewMode::Miller => {
            let rects = split_main_columns(top, app.preview_size().weight());
            draw_parent_pane(app, frame, theme, rects[0]);
            draw_list_pane(app, frame, theme, rects[1]);
            draw_preview_pane(app, frame, theme, rects[2]);
            draw_action_bar(app, frame, theme, bottom);
            if app.is_full_preview() {
                if app.editor().is_some() {
                    draw_editor_modal(app, frame, theme, area);
                } else {
                    draw_preview_modal(app, frame, theme, area);
                }
            }
        }
        crate::app::ViewMode::Tree => {
            draw_tree_view(app, frame, theme, top);
            draw_action_bar(app, frame, theme, bottom);
        }
    }
    if let Some(modal) = app.input_modal() {
        draw_input_modal(modal, frame, theme, area);
    }
}

/// Two-pane layout for `ViewMode::Tree`: tree (left ~30%) +
/// preview (right ~70%). Tree pane uses [`gtui::LiveTable`]
/// in tree-glyph mode; preview pane reuses the same renderer the
/// miller-mode side preview uses, just with a much bigger area.
fn draw_tree_view(app: &App, frame: &mut Frame<'_>, theme: &Theme, area: Rect) {
    use crate::tree::{build_rows_for_app, FbCol};
    use gtui::browser::BrowserShell;
    use gtui::widgets::live_table::{Align, ColumnDef, WidthSpec};

    let rows = build_rows_for_app(
        app.cwd(),
        app.tree_sort,
        app.tree_sort_descending,
        app.show_hidden(),
        &app.tree_state.expanded,
        app.filter(),
        app.connections(),
        app.db_cache(),
        app.fs_cache(),
    );

    let columns: Vec<ColumnDef<FbCol>> = vec![
        ColumnDef {
            id: FbCol::Name,
            label: "Name",
            width: WidthSpec::Flex,
            align: Align::Left,
            sortable: true,
        },
        ColumnDef {
            id: FbCol::Size,
            label: "Size",
            width: WidthSpec::Fixed(7),
            align: Align::Right,
            sortable: true,
        },
        ColumnDef {
            id: FbCol::Modified,
            label: "Modified",
            width: WidthSpec::Fixed(16),
            align: Align::Left,
            sortable: true,
        },
    ];

    let preview_rect = BrowserShell::<FbCol>::new()
        .with_title(format!("tree · {}", app.cwd_display()))
        .with_accent(theme.panel_accents[3])
        .with_sort_state(app.tree_sort, app.tree_sort_descending)
        .render(
            frame,
            area,
            &rows,
            &columns,
            theme,
            FbCol::Name,
            app.tree_state.nav.cursor,
        );

    // Preview pane — reuse the miller-mode preview renderer against
    // the bigger right-hand rect.
    draw_preview_pane(app, frame, theme, preview_rect);
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
    let rows = build_rows(app.entries());
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

fn draw_editor_modal(app: &App, frame: &mut Frame<'_>, theme: &Theme, area: Rect) {
    let Some(editor) = app.editor() else { return };
    let dirty_mark = if editor.dirty { " ●" } else { "" };
    let title = format!("✎ {}{}", editor.name(), dirty_mark);
    let controls = format!(
        "Ln {}  Col {}  ·  ^S save  ^X exit",
        editor.cursor.0 + 1,
        editor.cursor.1 + 1
    );
    let panel = BoxedPanel::new(theme.panel_accents[3], theme.title)
        .with_title(title)
        .with_controls(controls)
        .flat();
    let modal_w = ((area.width as u32 * 9 / 10) as u16).max(20);
    let modal_h = ((area.height as u32 * 9 / 10) as u16).max(8);
    let bg = theme.main_bg.unwrap_or(Color::Black);
    let shell = ModalShell::new(panel, modal_w, modal_h)
        .with_fill(Style::default().bg(bg).fg(theme.main_fg));
    let Some(body) = shell.render(frame, area) else {
        return;
    };
    if body.height == 0 {
        return;
    }
    render_editor_body(app, frame, theme, body);
}

fn draw_preview_modal(app: &App, frame: &mut Frame<'_>, theme: &Theme, area: Rect) {
    let title = app
        .selected()
        .map(|e| e.name.clone())
        .unwrap_or_else(|| "preview".to_string());
    let panel = BoxedPanel::new(theme.panel_accents[3], theme.title)
        .with_title(title)
        .with_controls("j/k scroll  •  Space/Esc close")
        .flat();
    let modal_w = ((area.width as u32 * 9 / 10) as u16).max(20);
    let modal_h = ((area.height as u32 * 9 / 10) as u16).max(8);
    // ModalShell only patches the fill bg onto border/chrome cells if
    // the fill style has a Some(bg). Use the theme's main_bg when set;
    // otherwise fall back to Black so the modal reads as a solid layer
    // and the panes behind don't bleed through the borders or any
    // gaps that ratatui's diff renderer leaves.
    let modal_bg = theme.main_bg.unwrap_or(Color::Black);
    let shell = ModalShell::new(panel, modal_w, modal_h)
        .with_fill(Style::default().bg(modal_bg).fg(theme.main_fg));
    let Some(body) = shell.render(frame, area) else {
        return;
    };
    if body.height == 0 {
        return;
    }
    // Re-derive the lines for this rect — we can't borrow the closure
    // result from draw_preview_pane.
    let fallback = lines_for_modal(app);
    render_preview_body(app, frame, theme, body, fallback, app.modal_scroll());
}

fn lines_for_modal(app: &App) -> Vec<Line<'static>> {
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
    let move_label: &str = match app.focus() {
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
    let actions: Vec<(String, String)> = if app.jump_pending() {
        // While the chord is pending the action bar shifts to a hint
        // for which keys are valid second-presses. Mirrors what yazi
        // does to make the chord feel discoverable.
        vec![
            ("g…".into(), "jump:".into()),
            ("h".into(), "home".into()),
            ("/".into(), "/".into()),
            ("t".into(), "/tmp".into()),
            ("r".into(), "repo".into()),
            ("g".into(), "top".into()),
            ("Esc".into(), "cancel".into()),
        ]
    } else {
        vec![
            ("Tab".into(), "focus".into()),
            ("Space".into(), "full".into()),
            ("[/]".into(), "preview size".into()),
            ("/".into(), "filter".into()),
            ("g_".into(), "jump".into()),
            ("h/l".into(), "parent/open".into()),
            ("j/k".into(), move_label.into()),
            ("f".into(), "find".into()),
            ("e".into(), "edit".into()),
            ("a".into(), "new".into()),
            ("r".into(), "rename".into()),
            ("d/D".into(), "trash/del".into()),
            ("q/b".into(), "quit".into()),
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

fn build_rows(entries: &[crate::fs::entry::FsEntry]) -> Vec<Row<'static>> {
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
            Row::data(vec![
                Cell::new(name).with_style(Style::default()),
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

/// Render the DB-table preview as a `LiveTable` of result rows. Used
/// in tree mode when the cursor lands on a `NodeKind::Table` row.
fn draw_db_preview(
    preview: &crate::app::DbPreview,
    frame: &mut Frame<'_>,
    theme: &Theme,
    area: Rect,
) {
    use gtui::widgets::live_table::{
        Cell, ColumnDef, LiveTable, TableEntry, TableRowExt, WidthSpec,
    };
    use gtui::widgets::{panel, CornerStyle};
    let title = format!(
        "{}.{}.{}",
        preview.path.database.as_deref().unwrap_or("?"),
        preview.path.schema.as_deref().unwrap_or("?"),
        preview.path.table.as_deref().unwrap_or("?"),
    );
    let p = panel(
        theme.panel_accents[1],
        theme.title,
        CornerStyle::default(),
    )
    .with_title(title)
    .with_controls(format!("{} rows", preview.rows.len()));
    frame.render_widget(&p, area);
    let inner = p.inner(area);
    if inner.width < 6 || inner.height < 2 {
        return;
    }

    // Build columns. Labels need 'static — leak each on the hot
    // path. The alternative (Cow-typed labels in LiveTable) is a
    // follow-up; matters once a session churns through hundreds of
    // tables.
    let columns: Vec<ColumnDef<usize>> = preview
        .columns
        .iter()
        .enumerate()
        .map(|(i, c)| ColumnDef {
            id: i,
            label: leak_column_label(c.name.as_str()),
            width: WidthSpec::Fixed(column_width(
                c.name.len(),
                preview
                    .rows
                    .iter()
                    .map(|r| r.cells.get(i).map(|s| s.len()).unwrap_or(0))
                    .max()
                    .unwrap_or(0),
            )),
            align: align_for(c.data_type.as_str()),
            sortable: false,
        })
        .collect();

    struct PreviewRowView<'a> {
        row: &'a crate::sources::Row,
    }
    impl<'a> TableRowExt<usize> for PreviewRowView<'a> {
        fn cell(&self, col: usize) -> Cell {
            Cell::plain(self.row.cells.get(col).cloned().unwrap_or_default())
        }
    }

    let entries: Vec<TableEntry<PreviewRowView<'_>, ()>> = preview
        .rows
        .iter()
        .map(|r| TableEntry::Item(PreviewRowView { row: r }))
        .collect();

    let body_h = inner.height.saturating_sub(1) as usize;
    let scroll = gtui::middle_anchor_scroll(0, preview.rows.len(), body_h);
    let table = LiveTable::new(&entries, &columns, theme, 0)
        .with_selection(Some(0), scroll)
        .with_fade(false);
    frame.render_widget(&table, inner);
}

fn align_for(data_type: &str) -> gtui::widgets::live_table::Align {
    use gtui::widgets::live_table::Align;
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
    // Fit content but cap at 32 cells so wide TEXT/UUID columns
    // don't dominate. Caller can scroll horizontally in a follow-up.
    header_len.max(max_cell_len).clamp(4, 32) as u16
}

fn leak_column_label(s: &str) -> &'static str {
    Box::leak(s.to_string().into_boxed_str())
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
