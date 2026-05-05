use bobtop_core::sample::FilesystemSample;
use bobtop_tui::widgets::Meter;
use bobtop_tui::widgets::panel as boxed_panel;
use bobtop_tui::{format_bytes, format_rate};
use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::widgets::Widget;
use ratatui::Frame;

use crate::app::App;

use super::common;
use super::presenter;

pub(super) fn draw(frame: &mut Frame, area: Rect, app: &App) {
    let title = presenter::disk_panel_title(app);
    let panel = boxed_panel(app.theme.mem_box, app.theme.title, app.corner_style).with_title(title);
    frame.render_widget(&panel, area);
    let inner = panel.inner(area);
    if inner.width < 8 || inner.height < 2 {
        return;
    }

    // Reserve the last 2 rows for the swap meter when swap exists. Swap
    // is conceptually backing-store, not RAM — it lives here so users
    // see disk + swap as one panel and the memory panel has more room
    // for PSI / breakdown.
    let swap_h: u16 = if app
        .latest_mem
        .as_ref()
        .map(|m| m.swap_total_bytes > 0)
        .unwrap_or(false)
        && inner.height >= 5
    {
        2
    } else {
        0
    };
    let mounts_h = inner.height.saturating_sub(swap_h);
    let mounts_area = Rect::new(inner.x, inner.y, inner.width, mounts_h);
    let swap_area = Rect::new(inner.x, inner.y + mounts_h, inner.width, swap_h);

    let Some(disk) = &app.latest_disk else {
        // Even with no disk sample yet, render swap if we have a memory sample.
        if swap_h > 0 {
            draw_swap_row(frame.buffer_mut(), swap_area, app);
        }
        return;
    };
    if !disk.filesystems.is_empty() {
        draw_filesystems(frame, mounts_area, disk, app);
    }
    if swap_h > 0 {
        draw_swap_row(frame.buffer_mut(), swap_area, app);
    }
}

fn draw_filesystems(
    frame: &mut Frame,
    inner: Rect,
    disk: &bobtop_core::sample::DiskSample,
    app: &App,
) {
    match app.track_chart_style {
        crate::app::TrackChartStyle::Bar => {
            let meters: Vec<Meter> = disk
                .filesystems
                .iter()
                .map(|fs| build_disk_meter(fs, &app.theme))
                .collect();
            let row_h = (inner.height / meters.len().max(1) as u16).max(3);
            for (i, m) in meters.iter().enumerate() {
                let y = inner.y + (i as u16) * row_h;
                if y + 3 > inner.y + inner.height {
                    break;
                }
                m.render(Rect::new(inner.x, y, inner.width, row_h), frame.buffer_mut());
            }
        }
        crate::app::TrackChartStyle::Sparkline => {
            let n = disk.filesystems.len().max(1);
            let row_h = (inner.height / n as u16).max(3);
            let meters: Vec<Meter> = disk
                .filesystems
                .iter()
                .map(|fs| build_disk_meter_used_only(fs, &app.theme))
                .collect();
            for (i, fs) in disk.filesystems.iter().enumerate() {
                let y = inner.y + (i as u16) * row_h;
                if y + 3 > inner.y + inner.height {
                    break;
                }
                let combined: std::collections::VecDeque<f64> = app
                    .disk_history
                    .get(&fs.label)
                    .map(|h| h.iter().map(|(r, w)| r + w).collect())
                    .unwrap_or_default();
                let label = presenter::disk_label(&fs.label);
                let io_value = presenter::disk_io_value(fs.read_bytes_per_sec, fs.write_bytes_per_sec);
                common::draw_track_sparkline_row(
                    frame.buffer_mut(),
                    Rect::new(inner.x, y, inner.width, 1),
                    &label,
                    &io_value,
                    Some(&combined),
                    app.theme.used,
                    app.theme.main_fg,
                    app.tty_graphs,
                );
                let gauge_h = row_h.saturating_sub(1);
                if gauge_h >= 2 {
                    let gauge_rect = Rect::new(inner.x, y + 1, inner.width, gauge_h);
                    meters[i].render(gauge_rect, frame.buffer_mut());
                }
            }
        }
    }
}

/// Render the swap meter inside the disk panel. Reads swap fields off
/// `app.latest_mem` (swap is sampled by the memory collector since
/// /proc/meminfo is the source of truth for SwapTotal/SwapFree).
fn draw_swap_row(buf: &mut Buffer, area: Rect, app: &App) {
    if area.height == 0 {
        return;
    }
    let Some(mem) = app.latest_mem.as_ref() else { return };
    if mem.swap_total_bytes == 0 {
        return;
    }
    let theme = &app.theme;
    let frac = mem.swap_used_bytes as f64 / mem.swap_total_bytes as f64;
    let value = format!(
        "{} / {}",
        format_bytes(mem.swap_used_bytes),
        format_bytes(mem.swap_total_bytes)
    );
    let m = Meter::new("Swap:", value, frac)
        .with_gradient(theme.used)
        .with_meter_bg(theme.meter_bg)
        .with_text_colors(theme.main_fg, theme.title);
    m.render(area, buf);
}

fn build_disk_meter_used_only(fs: &FilesystemSample, theme: &bobtop_tui::Theme) -> Meter {
    let frac = if fs.total_bytes > 0 {
        fs.used_bytes as f64 / fs.total_bytes as f64
    } else {
        0.0
    };
    let value = format!(
        "{} / {}",
        format_bytes(fs.used_bytes),
        format_bytes(fs.total_bytes),
    );
    let label = format!("{}:", fs.label);
    Meter::new(label, value, frac)
        .with_gradient(theme.used)
        .with_meter_bg(theme.meter_bg)
        .with_text_colors(theme.main_fg, theme.title)
}

fn build_disk_meter(fs: &FilesystemSample, theme: &bobtop_tui::Theme) -> Meter {
    let frac = if fs.total_bytes > 0 {
        fs.used_bytes as f64 / fs.total_bytes as f64
    } else {
        0.0
    };
    let io_part = match (fs.read_bytes_per_sec, fs.write_bytes_per_sec) {
        (Some(r), Some(w)) if r + w > 0.0 => {
            format!("▼{}/s ▲{}/s  ", format_rate(r), format_rate(w))
        }
        _ => String::new(),
    };
    let value = format!(
        "{}{} / {}",
        io_part,
        format_bytes(fs.used_bytes),
        format_bytes(fs.total_bytes),
    );
    let label = match fs.io_utilization {
        Some(io) if io > 0.05 => format!("{}: io {:.0}%", fs.label, io * 100.0),
        _ => format!("{}:", fs.label),
    };
    Meter::new(label, value, frac)
        .with_gradient(theme.used)
        .with_meter_bg(theme.meter_bg)
        .with_text_colors(theme.main_fg, theme.title)
}
