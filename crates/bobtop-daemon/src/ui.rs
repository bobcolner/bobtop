//! Pure frame composition. `draw` is the single render function called by
//! the TUI loop on every frame.

use bobtop_tui::compute_layout;
use ratatui::layout::Rect;
use ratatui::style::Style;
use ratatui::Frame;

use crate::app::App;

mod common;
mod cpu;
mod disk;
mod memory;
mod presenter;
mod network;
mod processes;
mod overlays;

pub use overlays::HELP_LINES;

pub fn draw(frame: &mut Frame, app: &App) {
    let area = frame.area();
    if area.height == 0 || area.width == 0 {
        return;
    }
    // Paint the theme's `main_bg` across the whole frame so panels render
    // ON the theme background instead of the terminal default. When
    // `theme_background = false` (Options overlay), `main_bg` is None and we
    // skip the fill, letting terminal transparency / wallpaper show through.
    // Matches btop: same theme key, same opt-out toggle, same effect.
    if let Some(bg) = app.theme.main_bg {
        let buf = frame.buffer_mut();
        let fill = Style::default().bg(bg);
        for y in area.y..area.bottom() {
            for x in area.x..area.right() {
                let cell = &mut buf[(x, y)];
                cell.set_char(' ');
                cell.set_style(fill);
            }
        }
    }
    // Layout is computed from `app.panel_sizes` — disabled panels get None
    // (no rect) and their space is reabsorbed; sized panels claim their
    // share of the row/column they live in. Users cycle sizes via 1-5
    // (CPU/Mem/Net/Proc/Disk) or toggle on/off via `B`.
    let layout = compute_layout(area, &app.panel_sizes);

    if let Some(cpu_area) = layout.cpu {
        cpu::draw(frame, cpu_area, app);
    }
    if let Some(mem_area) = layout.memory {
        memory::draw(frame, mem_area, app);
    }
    if let Some(disks_area) = layout.disks {
        disk::draw(frame, disks_area, app);
    }
    if let Some(net_area) = layout.network {
        network::draw(frame, net_area, app);
    }
    if let Some(proc_area) = layout.processes {
        processes::draw(frame, proc_area, app);
    }

    if app.ui.show_boxes_overlay {
        overlays::draw_boxes_overlay(frame, area, app);
    }
    if app.ui.pending_kill.is_some() {
        overlays::draw_kill_dialog(frame, area, app);
    }
    if app.ui.detail.is_some() {
        overlays::draw_detail_modal(frame, area, app);
    }
    if app.ui.options.is_some() {
        overlays::draw_options_overlay(frame, area, app);
    }
    if app.ui.show_help {
        overlays::draw_help_overlay(frame, area, app);
    }
    // Transient error / status banner. Painted *last* so it sits on top
    // of every panel and modal — these are the messages that have been
    // silently black-holed historically (file-browser launch failures
    // were the canonical example).
    if let Some(msg) = &app.ui.last_options_msg {
        draw_toast(frame, area, app, msg);
    }
}

fn draw_toast(frame: &mut Frame, area: Rect, app: &App, msg: &str) {
    if area.height == 0 || area.width == 0 {
        return;
    }
    let buf = frame.buffer_mut();
    let y = area.bottom().saturating_sub(1);
    // Paint the full bottom row with a high-contrast background so the
    // toast is unmissable. Using `selected_bg` because every theme
    // tunes it to be a noisy accent.
    let bg = app.theme.selected_bg;
    let fg = app.theme.selected_fg;
    for x in area.x..area.right() {
        let cell = &mut buf[(x, y)];
        cell.set_char(' ');
        cell.set_style(Style::default().bg(bg).fg(fg));
    }
    let body = format!(" ⚠ {msg}  —  press any key to dismiss ");
    bobtop_tui::write_str_clipped(
        buf,
        area.x,
        y,
        &body,
        area.width,
        Style::default().bg(bg).fg(fg).add_modifier(ratatui::style::Modifier::BOLD),
    );
}

#[cfg(test)]
fn draw_core_meters(frame: &mut Frame, area: Rect, sample: &bobtop_core::sample::CpuSample, app: &App) {
    cpu::draw_core_meters(frame, area, sample, app);
}
#[cfg(test)]
mod tests {
    use bobtop_tui::format_bytes;

    #[test]
    fn format_bytes_picks_right_unit() {
        assert_eq!(format_bytes(0), "0 B");
        assert_eq!(format_bytes(1024), "1 KiB");
        assert_eq!(format_bytes(1024 * 1024), "1 MiB");
        assert!(format_bytes(2_500_000_000).contains("GiB"));
    }

    fn fake_sample(n: u32) -> bobtop_core::sample::CpuSample {
        let cores = (0..n)
            .map(|i| bobtop_core::sample::CoreSample {
                id: i,
                utilization: 0.5,
                frequency_mhz: None,
                temperature_c: None,
            })
            .collect();
        bobtop_core::sample::CpuSample {
            timestamp: std::time::Instant::now(),
            aggregate_utilization: 0.5,
            cores,
            load_average: None,
        }
    }

    /// Wide panel + many cores → meters wrap into multiple columns and every
    /// core has a row to land on, instead of being silently truncated when
    /// `cores > area.height` (the pre-wrap behavior).
    #[test]
    fn core_meters_wrap_into_columns_on_wide_panels() {
        use ratatui::buffer::Buffer;
        use ratatui::layout::Rect;
        use std::sync::{atomic::AtomicU64, Arc};
        let s = fake_sample(32);
        let app = super::super::app::App::new(
            crate::monitor_theme::MonitorTheme::default(),
            bobtop_tui::LayoutPreset::Full,
            Arc::new(AtomicU64::new(500)),
            false,
            false,
        );
        let area = Rect::new(0, 0, 64, 16);
        let mut buf = Buffer::empty(area);
        let backend = ratatui::backend::TestBackend::new(area.width, area.height);
        let mut term = ratatui::Terminal::new(backend).unwrap();
        term.draw(|f| {
            super::draw_core_meters(f, area, &s, &app);
            buf = f.buffer_mut().clone();
        })
        .unwrap();
        // C0 anchored at left column; C16 anchored well to the right.
        let row0: String = (0..6).map(|x| buf[(x, 0)].symbol().to_string()).collect();
        assert!(row0.contains("C 0") || row0.contains("C0"), "left col missing C0: {row0:?}");
        // Find C16 somewhere in row 0 past the midpoint — proves a second
        // column exists.
        let mut found_c16_right = false;
        for x in (area.width / 2)..area.width.saturating_sub(2) {
            let s: String = (x..x + 3).map(|c| buf[(c, 0)].symbol().to_string()).collect();
            if s == "C16" {
                found_c16_right = true;
                break;
            }
        }
        assert!(found_c16_right, "expected C16 to appear in the right column on row 0");
    }

    /// Narrow panel (< MIN_COL_W) falls back to single column.
    #[test]
    fn core_meters_single_column_when_narrow() {
        use ratatui::buffer::Buffer;
        use ratatui::layout::Rect;
        use std::sync::{atomic::AtomicU64, Arc};
        let s = fake_sample(8);
        let app = super::super::app::App::new(
            crate::monitor_theme::MonitorTheme::default(),
            bobtop_tui::LayoutPreset::Full,
            Arc::new(AtomicU64::new(500)),
            false,
            false,
        );
        let area = Rect::new(0, 0, 18, 12);
        let mut buf = Buffer::empty(area);
        let backend = ratatui::backend::TestBackend::new(area.width, area.height);
        let mut term = ratatui::Terminal::new(backend).unwrap();
        term.draw(|f| {
            super::draw_core_meters(f, area, &s, &app);
            buf = f.buffer_mut().clone();
        })
        .unwrap();
        // C7 should be on row 7 (single col); not on row 0.
        let row7: String = (0..3).map(|x| buf[(x, 7)].symbol().to_string()).collect();
        assert_eq!(row7, "C 7");
    }
}
