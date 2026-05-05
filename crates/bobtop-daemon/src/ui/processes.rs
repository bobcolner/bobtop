use bobtop_tui::widgets::panel as boxed_panel;
use bobtop_tui::widgets::{Cell as GridCell, Column as GridColumn, Row as GridRow, Table};
use bobtop_tui::widgets::ProcessTableSort as TableSort;
use bobtop_tui::write_str_at;
use ratatui::layout::Rect;
use ratatui::style::Style;
use ratatui::Frame;

use crate::app::App;

use super::presenter;

pub(super) fn draw(frame: &mut Frame, area: Rect, app: &App) {
    let title = presenter::process_title(app);
        let panel = boxed_panel(app.theme.proc_box, app.theme.title, app.corner_style)
        .with_title(title)
        .with_keybinds(
            "q quit  ↑↓ select  ←→ sort  r rev  s sticky  f filter  g group  Space expand  k/K kill  Enter  ?",
        );
    frame.render_widget(&panel, area);
    let inner = panel.inner(area);
    if inner.width < 20 || inner.height < 2 {
        return;
    }

    let (table_area, filter_bar) = if app.ui.filter_active {
        let body_h = inner.height.saturating_sub(1);
        (
            Rect::new(inner.x, inner.y, inner.width, body_h),
            Some(Rect::new(inner.x, inner.y + body_h, inner.width, 1)),
        )
    } else {
        (inner, None)
    };

    let body_h = table_area.height.saturating_sub(1) as usize;
    let rows = app.display_rows();
    let scroll_offset = selection_scroll_offset(app.selected_proc, rows.len(), body_h);
    let layout = match app.group_mode {
        crate::group::GroupMode::Flat => bobtop_tui::widgets::TableLayout::Flat,
        crate::group::GroupMode::ByExecutable | crate::group::GroupMode::ByCgroup => {
            bobtop_tui::widgets::TableLayout::Grouped
        }
        crate::group::GroupMode::ByParent => bobtop_tui::widgets::TableLayout::Tree,
    };
    let (columns, grid_rows, sort_col) = build_table_model(app, &rows, layout);
    let table = Table::new(&columns, &grid_rows, &app.theme)
        .with_selection(Some(app.selected_proc), scroll_offset)
        .with_sort(sort_col, app.proc_sort_descending);
    frame.render_widget(&table, table_area);

    if let Some(bar) = filter_bar {
        let buf = frame.buffer_mut();
        let bg = app.theme.meter_bg;
        for x in 0..bar.width {
            let cell = &mut buf[(bar.x + x, bar.y)];
            cell.set_char(' ');
            cell.set_style(Style::default().bg(bg).fg(app.theme.title));
        }
        let label = format!(" filter: {}█  ", app.ui.filter_text);
        write_str_at(buf, bar.x, bar.y, &label, Style::default().bg(bg).fg(app.theme.hi_fg));
        let hint = " Enter=apply  Esc=clear ";
        let len = hint.chars().count() as u16;
        if len + 2 < bar.width {
            write_str_at(
                buf,
                bar.x + bar.width.saturating_sub(len + 1),
                bar.y,
                hint,
                Style::default().bg(bg).fg(app.theme.inactive_fg),
            );
        }
    }
}

fn build_table_model(
    app: &App,
    rows: &[crate::group::TableRow],
    layout: bobtop_tui::widgets::TableLayout,
) -> (Vec<GridColumn<'static>>, Vec<GridRow<'static>>, Option<usize>) {
    let columns = build_columns(layout);
    let sort_col = sort_column_index(app, layout, &columns);
    let grid_rows = rows
        .iter()
        .map(|row| match row {
            crate::group::TableRow::Header(h) => build_header_row(app, h, layout, &columns),
            crate::group::TableRow::Item(p) => build_process_row(app, p, layout, &columns),
        })
        .collect();
    (columns, grid_rows, sort_col)
}

fn build_columns(layout: bobtop_tui::widgets::TableLayout) -> Vec<GridColumn<'static>> {
    let mut cols = Vec::with_capacity(11);
    if layout.includes_pid() {
        cols.push(GridColumn::new("Pid", 6).right_aligned(true));
    }
    cols.push(GridColumn::new("Program", layout.program_width()).right_aligned(false));
    if layout.includes_command() {
        cols.push(GridColumn::new("Command", layout.command_width()).right_aligned(false));
    }
    if layout.includes_user() {
        cols.push(GridColumn::new("User", 6).right_aligned(false));
    }
    cols.push(GridColumn::new("Th", 3).right_aligned(true));
    cols.push(GridColumn::new("MEM", 6).right_aligned(true));
    cols.push(GridColumn::new("CPU%", 5).right_aligned(true));
    cols.push(GridColumn::new("RX/s", 6).right_aligned(true));
    cols.push(GridColumn::new("TX/s", 6).right_aligned(true));
    cols.push(GridColumn::new("DR/s", 6).right_aligned(true));
    cols.push(GridColumn::new("DW/s", 6).right_aligned(true));
    cols
}

fn sort_column_index(
    app: &App,
    layout: bobtop_tui::widgets::TableLayout,
    columns: &[GridColumn<'static>],
) -> Option<usize> {
    let title = match app.proc_sort {
        TableSort::Pid => "Pid",
        TableSort::Name => "Program",
        TableSort::User => "User",
        TableSort::Threads => "Th",
        TableSort::Mem => "MEM",
        TableSort::Cpu => "CPU%",
        TableSort::NetRx => "RX/s",
        TableSort::NetTx => "TX/s",
        TableSort::DiskRead => "DR/s",
        TableSort::DiskWrite => "DW/s",
    };
    let _ = layout;
    columns.iter().position(|c| c.title.as_ref() == title)
}

fn build_header_row(
    app: &App,
    h: &crate::group::TableGroupHeader,
    layout: bobtop_tui::widgets::TableLayout,
    columns: &[GridColumn<'static>],
) -> GridRow<'static> {
    let mut cells = blank_cells(columns.len());
    let label_col = program_col(columns);
    let mut idx = 0usize;
    if layout.includes_pid() {
        idx += 1;
    }
    idx += 1; // program
    if layout.includes_command() {
        idx += 1;
    }
    if layout.includes_user() {
        idx += 1;
    }
    cells[idx] = GridCell::new(h.threads_total.to_string());
    cells[idx + 1] = GridCell::bytes(h.mem_rss_total);
    cells[idx + 2] = GridCell::new(format!("{:.1}", h.cpu_fraction_total * 100.0));
    cells[idx + 3] = GridCell::rate(h.net_rx_total);
    cells[idx + 4] = GridCell::rate(h.net_tx_total);
    cells[idx + 5] = GridCell::rate(h.disk_read_total);
    cells[idx + 6] = GridCell::rate(h.disk_write_total);

    let row = GridRow::header(format!("▼ {}", h.label), label_col, cells);
    let style = if h.expanded {
        Style::default().fg(app.theme.hi_fg)
    } else {
        Style::default().fg(app.theme.title)
    };
    row.with_style(style)
}

fn build_process_row(
    app: &App,
    meta: &crate::group::TableRowMeta,
    layout: bobtop_tui::widgets::TableLayout,
    columns: &[GridColumn<'static>],
) -> GridRow<'static> {
    let p = &meta.info;
    let row_fg = bobtop_tui::color::lerp_color(app.theme.main_fg, app.theme.inactive_fg, 0.0);
    let fade_fg = bobtop_tui::color::lerp_color(app.theme.main_fg, app.theme.inactive_fg, 0.25);
    let base_style = Style::default().fg(row_fg);
    let prefix = if layout.draws_tree_glyphs() {
        tree_prefix(meta)
    } else {
        "  ".repeat(meta.depth as usize)
    };
    let mut cells = blank_cells(columns.len());
    let mut col = 0usize;
    if layout.includes_pid() {
        cells[col] = GridCell::new(p.pid.to_string()).with_style(Style::default().fg(fade_fg));
        col += 1;
    }
    let prog_style = if layout.draws_tree_glyphs() {
        Style::default().fg(bobtop_tui::color::lerp_color(
            app.theme.proc_misc,
            app.theme.inactive_fg,
            0.25,
        ))
    } else {
        base_style
    };
    cells[col] = GridCell::new(format!("{prefix}{}", p.name)).with_style(prog_style);
    col += 1;
    if layout.includes_command() {
        let cmd = if p.cmdline.is_empty() {
            p.name.clone()
        } else {
            p.cmdline.clone()
        };
        cells[col] = GridCell::new(cmd).with_style(Style::default().fg(fade_fg));
        col += 1;
    }
    if layout.includes_user() {
        cells[col] = GridCell::new(p.user.clone()).with_style(Style::default().fg(fade_fg));
        col += 1;
    }
    cells[col] = GridCell::new(p.threads.to_string()).with_style(Style::default().fg(fade_fg));

    let mem_ratio = if p.mem_rss_bytes == 0 {
        0.0
    } else {
        (p.mem_rss_bytes as f64 / (32.0 * 1024.0 * 1024.0 * 1024.0)).min(1.0)
    } as f32;
    let mem_style = Style::default().fg(bobtop_tui::color::lerp_color(
        app.theme.used.sample(mem_ratio),
        app.theme.inactive_fg,
        0.25,
    ));
    cells[col + 1] = GridCell::bytes(p.mem_rss_bytes).with_style(mem_style);

    let cpu_style = Style::default().fg(bobtop_tui::color::lerp_color(
        app.theme.cpu.sample(p.cpu_fraction.clamp(0.0, 1.0)),
        app.theme.inactive_fg,
        0.25,
    ));
    cells[col + 2] =
        GridCell::new(format!("{:.1}", p.cpu_fraction * 100.0)).with_style(cpu_style);

    let net_style = Style::default().fg(fade_fg);
    cells[col + 3] = GridCell::rate(p.net_rx_bytes_per_sec).with_style(net_style);
    cells[col + 4] = GridCell::rate(p.net_tx_bytes_per_sec).with_style(net_style);
    cells[col + 5] = GridCell::rate(p.disk_read_bytes_per_sec).with_style(net_style);
    cells[col + 6] = GridCell::rate(p.disk_write_bytes_per_sec).with_style(net_style);

    let row_style = if meta.depth == 0 {
        base_style
    } else {
        base_style.fg(bobtop_tui::color::lerp_color(
            app.theme.main_fg,
            app.theme.inactive_fg,
            0.25,
        ))
    };

    GridRow::data(cells).with_style(row_style)
}

fn selection_scroll_offset(selected: usize, total_rows: usize, body_h: usize) -> usize {
    if body_h == 0 || total_rows == 0 {
        return 0;
    }
    let max_scroll = total_rows.saturating_sub(body_h);
    let anchor = body_h / 2;
    selected.saturating_sub(anchor).min(max_scroll)
}

fn blank_cells(len: usize) -> Vec<GridCell<'static>> {
    (0..len).map(|_| GridCell::new("")).collect()
}

fn program_col(columns: &[GridColumn<'static>]) -> usize {
    columns
        .iter()
        .position(|c| c.title.as_ref() == "Program")
        .unwrap_or(0)
}

fn tree_prefix(meta: &crate::group::TableRowMeta) -> String {
    if meta.depth == 0 {
        return String::new();
    }
    let mut out = String::new();
    for &cont in &meta.ancestor_continues {
        out.push_str(if cont { "│  " } else { "   " });
    }
    out.push_str(if meta.is_last_sibling { "└─ " } else { "├─ " });
    out
}

#[cfg(test)]
mod tests {
    use super::selection_scroll_offset;

    #[test]
    fn selection_keeps_one_row_buffer_at_bottom() {
        assert_eq!(selection_scroll_offset(9, 10, 5), 5);
        assert_eq!(selection_scroll_offset(8, 10, 5), 5);
        assert_eq!(selection_scroll_offset(3, 10, 5), 1);
    }

    #[test]
    fn tiny_viewports_do_not_over_scroll() {
        assert_eq!(selection_scroll_offset(4, 10, 1), 4);
        // body_h=2: anchor=1, offset=3 shows rows [3,4] — selection visible at bottom.
        // offset=4 would over-scroll (scroll further than needed to show selection).
        assert_eq!(selection_scroll_offset(4, 10, 2), 3);
    }
}
