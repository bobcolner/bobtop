use bobtop_core::sample::MemorySample;
use bobtop_tui::widgets::{BrailleGraph, GraphStyle, LegendStyle, StackedBar, StackedSegment};
use bobtop_tui::widgets::panel as boxed_panel;
use bobtop_tui::{format_bytes, write_str_at};
use ratatui::layout::Rect;
use ratatui::style::Style;
use ratatui::widgets::Widget;
use ratatui::Frame;

use crate::app::App;
use super::presenter;

pub(super) fn draw(frame: &mut Frame, area: Rect, app: &App) {
    let title = presenter::memory_panel_title(app);
    let panel = boxed_panel(app.theme.mem_box, app.theme.title, app.corner_style).with_title(title);
    frame.render_widget(&panel, area);
    let inner = panel.inner(area);
    if inner.height < 3 {
        return;
    }

    let graph_h = inner.height.min(4).saturating_sub(1).max(2);
    let graph_area = Rect::new(inner.x, inner.y, inner.width, graph_h);
    let meters_y = inner.y + graph_h;
    let meters_area = Rect::new(
        inner.x,
        meters_y,
        inner.width,
        inner.y + inner.height - meters_y,
    );

    let mem_max_pts = graph_area.width as usize * 2;
    let mut mem_graph = BrailleGraph::new(mem_max_pts, app.theme.used)
        .with_value_fn(|v| format!("{:>5.1}%", v * 100.0))
        .with_style(if app.tty_graphs {
            GraphStyle::Blocks
        } else {
            GraphStyle::CenteredBloom
        })
        .with_text_style(Style::default().fg(app.theme.graph_text));
    for v in app.mem_history.iter().copied() {
        mem_graph.push(v);
    }
    frame.render_widget(&mem_graph, graph_area);

    let Some(s) = &app.latest_mem else { return };
    if meters_area.height < 2 {
        return;
    }
    draw_memory_breakdown(frame, meters_area, s, app);
}

fn draw_memory_breakdown(frame: &mut Frame, area: Rect, s: &MemorySample, app: &App) {
    let theme = &app.theme;
    let total = s.total_bytes.max(1) as f64;
    let used_real = s.used_bytes as f64;
    let cached_b = s.cached_bytes as f64;
    let buffers_b = s.buffers_bytes as f64;
    let free_b = s.free_bytes as f64;
    let unaccounted = (total - used_real - cached_b - buffers_b - free_b).max(0.0);
    let used_total = used_real + unaccounted;
    let cached_combined = (cached_b + buffers_b) as u64;
    let segments = vec![
        StackedSegment::new("Used", used_total / total, theme.used.end)
            .with_value(format_bytes(used_total as u64)),
        StackedSegment::new("Cached", (cached_b + buffers_b) / total, theme.cached.end)
            .with_value(format_bytes(cached_combined)),
        StackedSegment::new("Free", free_b / total, theme.free.end)
            .with_value(format_bytes(s.free_bytes)),
    ];
    let bar = StackedBar::new(&segments)
        .with_empty_bg(theme.meter_bg)
        .with_chrome_fg(theme.main_fg)
        .with_legend(area.height >= 3)
        .with_legend_style(LegendStyle::Aligned);
    let bar_h: u16 = if area.height >= 3 { 2 } else { 1 };
    let bar_rect = Rect::new(area.x, area.y, area.width, bar_h);
    (&bar).render(bar_rect, frame.buffer_mut());

    let mut next_y = area.y + bar_h;
    let bottom = area.y + area.height;

    // Pressure-Stall Information (PSI). Two rows when shown: a colored
    // bar gauging current 10s avg + a numeric line for 10s/60s/300s.
    // Falls back to a single line ("PSI: unavailable") when the kernel
    // doesn't expose /proc/pressure/memory — better than silently empty
    // space, which the user can't distinguish from "not yet sampled".
    if next_y < bottom {
        match s.pressure {
            Some(p) => {
                if next_y + 1 < bottom {
                    let bar_segs = vec![StackedSegment::new(
                        "PSI",
                        (p.some_avg10 as f64 / 100.0).clamp(0.0, 1.0),
                        theme.temp.end,
                    )];
                    let pbar = StackedBar::new(&bar_segs)
                        .with_empty_bg(theme.meter_bg)
                        .with_chrome_fg(theme.main_fg);
                    let pbar_rect = Rect::new(area.x, next_y, area.width, 1);
                    (&pbar).render(pbar_rect, frame.buffer_mut());
                    next_y += 1;

                    let nums = format!(
                        "PSI  10s {:.1}%  60s {:.1}%  300s {:.1}%",
                        p.some_avg10, p.some_avg60, p.some_avg300
                    );
                    write_str_at(
                        frame.buffer_mut(),
                        area.x,
                        next_y,
                        &nums,
                        Style::default().fg(theme.inactive_fg),
                    );
                }
            }
            None => {
                write_str_at(
                    frame.buffer_mut(),
                    area.x,
                    next_y,
                    "PSI  unavailable (kernel CONFIG_PSI=n or no /proc/pressure)",
                    Style::default().fg(theme.inactive_fg),
                );
            }
        }
    }
    // Swap moved to the disk panel — see ui/disk.rs::draw_swap_row.
    let _ = bottom; // suppress unused-warning when only PSI block reads it
}
