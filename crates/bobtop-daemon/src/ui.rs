//! Pure frame composition. `draw` is the single render function called by
//! the TUI loop on every frame.

use bobtop_core::Box as BoxKind;
use bobtop_tui::{compute_layout, format_bytes};
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
    let layout = compute_layout(area, app.layout_preset);

    // Each panel checks its bit in `app.boxes`; a hidden panel renders a
    // small "[hidden — press B to show]" placeholder so the layout shape
    // stays stable and the user has a hint about how to reveal it. The
    // collector for that box is also paused via A3, so the hidden state
    // really is free.
    if app.boxes.is_enabled(BoxKind::Cpu) {
        cpu::draw(frame, layout.cpu, app);
    } else {
        overlays::draw_hidden_panel(frame, layout.cpu, app, "cpu");
    }
    if let Some(mem_area) = layout.memory {
        if app.boxes.is_enabled(BoxKind::Memory) {
            memory::draw(frame, mem_area, app);
        } else {
            overlays::draw_hidden_panel(frame, mem_area, app, "mem");
        }
    }
    if let Some(disks_area) = layout.disks {
        if app.boxes.is_enabled(BoxKind::Disk) {
            disk::draw(frame, disks_area, app);
        } else {
            overlays::draw_hidden_panel(frame, disks_area, app, "disks");
        }
    }
    if let Some(net_area) = layout.network {
        if app.boxes.is_enabled(BoxKind::Network) {
            network::draw(frame, net_area, app);
        } else {
            overlays::draw_hidden_panel(frame, net_area, app, "net");
        }
    }
    if app.boxes.is_enabled(BoxKind::Process) {
        processes::draw(frame, layout.processes, app);
    } else {
        overlays::draw_hidden_panel(frame, layout.processes, app, "proc");
    }

    if app.show_boxes_overlay {
        overlays::draw_boxes_overlay(frame, area, app);
    }
    if app.pending_kill.is_some() {
        overlays::draw_kill_dialog(frame, area, app);
    }
    if app.detail.is_some() {
        overlays::draw_detail_modal(frame, area, app);
    }
    if app.options.is_some() {
        overlays::draw_options_overlay(frame, area, app);
    }
    if app.show_help {
        overlays::draw_help_overlay(frame, area, app);
    }
}

fn draw_core_meters(frame: &mut Frame, area: Rect, sample: &bobtop_core::sample::CpuSample, app: &App) {
    cpu::draw_core_meters(frame, area, sample, app);
}
#[cfg(test)]
mod tests {
    use super::*;

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
            bobtop_tui::Theme::default(),
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
            bobtop_tui::Theme::default(),
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
