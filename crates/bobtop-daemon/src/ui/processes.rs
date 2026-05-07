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
            "q quit  ↑↓ select  ←→ sort  r rev  s sticky  f filter  g group  Space [/] expand  k/K kill  Enter  ?",
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
        crate::group::GroupMode::ByExecutable
        | crate::group::GroupMode::ByCgroup
        | crate::group::GroupMode::ByContainer => bobtop_tui::widgets::TableLayout::Grouped,
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
    // Drop RX/s and TX/s when the active net tier doesn't expose per-pid
    // bandwidth (proc_inode shows only connections). Cleaner than
    // displaying "-" in every cell, and frees the gutter for wider
    // process names. eBPF and pcap tiers both populate, so they show.
    let show_net = app.net_tier.has_bandwidth();
    let columns = build_columns(layout, show_net);
    let sort_col = sort_column_index(app, layout, &columns);
    let scales = compute_metric_scales(rows);
    let grid_rows = rows
        .iter()
        .map(|row| match row {
            crate::group::TableRow::Header(h) => {
                build_header_row(app, h, &columns, &scales)
            }
            crate::group::TableRow::Item(p) => {
                build_process_row(app, p, layout, &columns, &scales)
            }
        })
        .collect();
    (columns, grid_rows, sort_col)
}

/// Per-tick maxima used to scale gradient colors for the rate columns.
/// CPU and Mem have natural ceilings (a single core / a fixed GiB
/// reference) so we don't track them here — net and disk rates are
/// open-ended and look uniform unless we normalize against the most
/// active row in this frame.
#[derive(Debug, Default, Clone, Copy)]
struct MetricScales {
    threads: u32,
    net_rx: f64,
    net_tx: f64,
    disk_r: f64,
    disk_w: f64,
}

fn compute_metric_scales(rows: &[crate::group::TableRow]) -> MetricScales {
    let mut s = MetricScales::default();
    for row in rows {
        match row {
            crate::group::TableRow::Item(meta) => {
                let p = &meta.info;
                s.threads = s.threads.max(p.threads);
                if let Some(v) = p.net_rx_bytes_per_sec {
                    if v > s.net_rx {
                        s.net_rx = v;
                    }
                }
                if let Some(v) = p.net_tx_bytes_per_sec {
                    if v > s.net_tx {
                        s.net_tx = v;
                    }
                }
                if let Some(v) = p.disk_read_bytes_per_sec {
                    if v > s.disk_r {
                        s.disk_r = v;
                    }
                }
                if let Some(v) = p.disk_write_bytes_per_sec {
                    if v > s.disk_w {
                        s.disk_w = v;
                    }
                }
            }
            crate::group::TableRow::Header(_) => {}
        }
    }
    s
}

fn build_columns(
    layout: bobtop_tui::widgets::TableLayout,
    show_net: bool,
) -> Vec<GridColumn<'static>> {
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
    if show_net {
        cols.push(GridColumn::new("RX/s", 6).right_aligned(true));
        cols.push(GridColumn::new("TX/s", 6).right_aligned(true));
    }
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
    columns: &[GridColumn<'static>],
    scales: &MetricScales,
) -> GridRow<'static> {
    let mut cells = blank_cells(columns.len());
    let label_col = program_col(columns);
    let pos = |title: &str| columns.iter().position(|c| c.title.as_ref() == title);
    let user_col = Style::default().fg(user_color(&h.dominant_user, &app.theme));
    if let Some(i) = pos("User") {
        cells[i] = GridCell::new(h.dominant_user.clone()).with_style(user_col);
    }
    if let Some(i) = pos("Th") {
        let style = threads_style(h.threads_total, scales.threads, &app.theme);
        cells[i] = GridCell::new(h.threads_total.to_string()).with_style(style);
    }
    if let Some(i) = pos("MEM") {
        let style = mem_style(h.mem_rss_total, &app.theme);
        cells[i] = GridCell::bytes(h.mem_rss_total).with_style(style);
    }
    if let Some(i) = pos("CPU%") {
        let style = cpu_style(h.cpu_fraction_total, &app.theme);
        cells[i] =
            GridCell::new(format!("{:.1}", h.cpu_fraction_total * 100.0)).with_style(style);
    }
    if let Some(i) = pos("RX/s") {
        let style = rate_style(h.net_rx_total, scales.net_rx, app.theme.download, &app.theme);
        cells[i] = GridCell::rate(h.net_rx_total).with_style(style);
    }
    if let Some(i) = pos("TX/s") {
        let style = rate_style(h.net_tx_total, scales.net_tx, app.theme.upload, &app.theme);
        cells[i] = GridCell::rate(h.net_tx_total).with_style(style);
    }
    if let Some(i) = pos("DR/s") {
        let style = rate_style(h.disk_read_total, scales.disk_r, app.theme.used, &app.theme);
        cells[i] = GridCell::rate(h.disk_read_total).with_style(style);
    }
    if let Some(i) = pos("DW/s") {
        let style = rate_style(h.disk_write_total, scales.disk_w, app.theme.used, &app.theme);
        cells[i] = GridCell::rate(h.disk_write_total).with_style(style);
    }

    let glyph = if h.expanded { '▼' } else { '▶' };
    let row = GridRow::header(format!("{} {}", glyph, h.label), label_col, cells);
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
    scales: &MetricScales,
) -> GridRow<'static> {
    let p = &meta.info;
    let row_fg = app.theme.main_fg;
    let dim_fg = app.theme.inactive_fg;
    let base_style = Style::default().fg(row_fg);
    let dim_style = Style::default().fg(dim_fg);
    let prefix = if layout.draws_tree_glyphs() {
        tree_prefix(meta)
    } else {
        "  ".repeat(meta.depth as usize)
    };
    let mut cells = blank_cells(columns.len());
    let pos = |title: &str| columns.iter().position(|c| c.title.as_ref() == title);

    if let Some(i) = pos("Pid") {
        cells[i] = GridCell::new(p.pid.to_string()).with_style(dim_style);
    }
    if let Some(i) = pos("Program") {
        let prog_style = if layout.draws_tree_glyphs() {
            Style::default().fg(app.theme.proc_misc)
        } else {
            base_style
        };
        cells[i] =
            GridCell::new(format!("{prefix}{}", p.name)).with_style(prog_style);
    }
    if let Some(i) = pos("Command") {
        let cmd = if p.cmdline.is_empty() {
            p.name.clone()
        } else {
            p.cmdline.clone()
        };
        cells[i] = GridCell::new(cmd).with_style(dim_style);
    }
    if let Some(i) = pos("User") {
        cells[i] =
            GridCell::new(p.user.clone()).with_style(Style::default().fg(user_color(&p.user, &app.theme)));
    }
    if let Some(i) = pos("Th") {
        let style = threads_style(p.threads, scales.threads, &app.theme);
        cells[i] = GridCell::new(p.threads.to_string()).with_style(style);
    }
    if let Some(i) = pos("MEM") {
        let style = mem_style(p.mem_rss_bytes, &app.theme);
        cells[i] = GridCell::bytes(p.mem_rss_bytes).with_style(style);
    }
    if let Some(i) = pos("CPU%") {
        let style = cpu_style(p.cpu_fraction, &app.theme);
        cells[i] = GridCell::new(format!("{:.1}", p.cpu_fraction * 100.0)).with_style(style);
    }
    if let Some(i) = pos("RX/s") {
        let style = rate_style(
            p.net_rx_bytes_per_sec,
            scales.net_rx,
            app.theme.download,
            &app.theme,
        );
        cells[i] = GridCell::rate(p.net_rx_bytes_per_sec).with_style(style);
    }
    if let Some(i) = pos("TX/s") {
        let style = rate_style(
            p.net_tx_bytes_per_sec,
            scales.net_tx,
            app.theme.upload,
            &app.theme,
        );
        cells[i] = GridCell::rate(p.net_tx_bytes_per_sec).with_style(style);
    }
    if let Some(i) = pos("DR/s") {
        let style = rate_style(
            p.disk_read_bytes_per_sec,
            scales.disk_r,
            app.theme.used,
            &app.theme,
        );
        cells[i] = GridCell::rate(p.disk_read_bytes_per_sec).with_style(style);
    }
    if let Some(i) = pos("DW/s") {
        let style = rate_style(
            p.disk_write_bytes_per_sec,
            scales.disk_w,
            app.theme.used,
            &app.theme,
        );
        cells[i] = GridCell::rate(p.disk_write_bytes_per_sec).with_style(style);
    }

    GridRow::data(cells).with_style(base_style)
}

/// Color picker for the User column. Non-root users get a stable
/// hash-derived hue (so multi-user hosts read at a glance — same
/// uid is the same color across sessions); root is highlighted
/// with the theme's `hi_fg` so it stands out as a privileged
/// owner. `—` (mixed-user group) renders dim.
fn user_color(user: &str, theme: &bobtop_tui::Theme) -> ratatui::style::Color {
    if user == "—" || user.is_empty() {
        return theme.inactive_fg;
    }
    if user == "root" {
        return theme.hi_fg;
    }
    // Cheap deterministic hash → 0..1 slot in the theme's process
    // gradient. Doesn't have to be cryptographic — we just want
    // stability so `myuser` keeps the same color across ticks.
    let mut h: u32 = 5381;
    for b in user.bytes() {
        h = h.wrapping_mul(33).wrapping_add(b as u32);
    }
    let slot = (h % 256) as f32 / 255.0;
    theme.process.sample(slot)
}

fn cpu_style(fraction: f32, theme: &bobtop_tui::Theme) -> Style {
    Style::default().fg(theme.cpu.sample(fraction.clamp(0.0, 1.0)))
}

fn mem_style(bytes: u64, theme: &bobtop_tui::Theme) -> Style {
    // 32 GiB reference matches the original code — proxy for "this
    // process is using a meaningful slice of system memory."
    let r = if bytes == 0 {
        0.0
    } else {
        (bytes as f64 / (32.0 * 1024.0 * 1024.0 * 1024.0))
            .clamp(0.0, 1.0) as f32
    };
    Style::default().fg(theme.used.sample(r))
}

fn threads_style(n: u32, max: u32, theme: &bobtop_tui::Theme) -> Style {
    if max == 0 {
        return Style::default().fg(theme.inactive_fg);
    }
    let r = (n as f32 / max as f32).clamp(0.0, 1.0);
    if r < 0.10 {
        // Most processes have a handful of threads; don't make them
        // shout. Reserve color for the outliers.
        Style::default().fg(theme.inactive_fg)
    } else {
        Style::default().fg(theme.process.sample(r))
    }
}

fn rate_style(
    value: Option<f64>,
    max: f64,
    gradient: bobtop_tui::color::Gradient,
    theme: &bobtop_tui::Theme,
) -> Style {
    match value {
        None => Style::default().fg(theme.inactive_fg),
        Some(v) if v <= 0.0 || max <= 0.0 => Style::default().fg(theme.inactive_fg),
        Some(v) => {
            // Normalize against the largest value in the column this
            // tick. Floors at 5 % so a single noisy process doesn't
            // wash everything else into the cool end of the gradient.
            let r = (v / max).clamp(0.05, 1.0) as f32;
            Style::default().fg(gradient.sample(r))
        }
    }
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
