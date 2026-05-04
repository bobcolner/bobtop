use bobtop_core::sample::FilesystemSample;
use bobtop_tui::widgets::Meter;
use bobtop_tui::widgets::panel as boxed_panel;
use bobtop_tui::{format_bytes, format_rate};
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

    let Some(disk) = &app.latest_disk else { return };
    if disk.filesystems.is_empty() {
        return;
    }

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
