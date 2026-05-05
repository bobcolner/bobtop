use bobtop_core::sample::CpuSample;
use bobtop_tui::widgets::{BrailleGraph, GraphStyle, MiniMeter, StackedBar, StackedSegment};
use bobtop_tui::widgets::panel as boxed_panel;
use bobtop_tui::{display_width, write_str_at};
use ratatui::layout::Rect;
use ratatui::style::Style;
use ratatui::widgets::Widget;
use ratatui::Frame;

use crate::app::App;

use super::common::draw_track_sparkline_row;
use super::presenter;

pub(super) fn draw(frame: &mut Frame, area: Rect, app: &App) {
    let cpu_pct = app
        .latest_cpu
        .as_ref()
        .map(|s| s.aggregate_utilization * 100.0)
        .unwrap_or(0.0);
    let title = presenter::cpu_panel_title(app);
    let panel = boxed_panel(app.theme.cpu_box, app.theme.title, app.corner_style)
        .with_title(title)
        .with_center_title(presenter::cpu_clock_label(app.tick_ms()))
        .with_controls(format!("- {}ms +", app.tick_ms()));
    frame.render_widget(&panel, area);
    let inner = panel.inner(area);
    if inner.width < 4 || inner.height < 2 {
        return;
    }

    let split = ((inner.width as f32) * 0.70) as u16;
    let graph_area = Rect::new(inner.x, inner.y, split, inner.height);
    let meters_area = Rect::new(inner.x + split, inner.y, inner.width - split, inner.height);

    let max_pts = (graph_area.width as usize) * 2;
    let mut graph = BrailleGraph::new(max_pts, app.theme.cpu)
        .with_style(if app.tty_graphs {
            GraphStyle::Blocks
        } else {
            GraphStyle::CenteredBloom
        })
        .with_text_style(Style::default().fg(app.theme.graph_text));
    for v in app.cpu_history.iter().copied() {
        graph.push(v);
    }
    frame.render_widget(&graph, graph_area);

    if let Some(s) = &app.latest_cpu {
        draw_cores_subpanel(frame, meters_area, s, app, cpu_pct as f64);
    }
}

fn draw_cores_subpanel(
    frame: &mut Frame,
    area: Rect,
    sample: &CpuSample,
    app: &App,
    cpu_pct: f64,
) {
    if area.width < 14 || area.height < 4 {
        draw_core_meters(frame, area, sample, app);
        return;
    }
    let freq = presenter::cpu_cores_frequency(sample);
    let model = presenter::cpu_cores_title(app);
    let panel = boxed_panel(app.theme.div_line, app.theme.title, app.corner_style)
        .flat()
        .with_title(model.to_string());
    let panel = if freq.is_empty() {
        panel
    } else {
        panel.with_controls(freq)
    };
    frame.render_widget(&panel, area);
    let inner = panel.inner(area);
    if inner.height == 0 || inner.width == 0 {
        return;
    }

    // 1-col horizontal breathing room inside the subpanel borders.
    let pad_x: u16 = if inner.width >= 6 { 1 } else { 0 };
    let content_x = inner.x + pad_x;
    let content_w = inner.width.saturating_sub(pad_x * 2);

    let overall_segs = vec![StackedSegment::new(
        "CPU",
        (cpu_pct / 100.0).clamp(0.0, 1.0),
        app.theme.cpu.end,
    )];
    let overall = StackedBar::new(&overall_segs)
        .with_label("CPU")
        .with_value_text(format!("{:>5.1}%", cpu_pct))
        .with_empty_bg(app.theme.meter_bg)
        .with_chrome_fg(app.theme.main_fg);
    let overall_rect = Rect::new(content_x, inner.y, content_w, 1);
    (&overall).render(overall_rect, frame.buffer_mut());

    let load_label = sample
        .load_average
        .map(|l| format!("load  {:.2} · {:.2} · {:.2}  (1m / 5m / 15m)", l.one, l.five, l.fifteen))
        .unwrap_or_else(|| "load  — · — · —".to_string());
    let load_h: u16 = if inner.height >= 4 { 1 } else { 0 };
    // Optional blank row between overall bar and core meters when tall enough.
    let gap_h: u16 = if inner.height >= 6 { 1 } else { 0 };

    let cores_h = inner
        .height
        .saturating_sub(1)
        .saturating_sub(gap_h)
        .saturating_sub(load_h);
    if cores_h > 0 {
        let cores_rect = Rect::new(content_x, inner.y + 1 + gap_h, content_w, cores_h);
        draw_core_meters(frame, cores_rect, sample, app);
    }

    if load_h > 0 {
        let y = inner.y + inner.height - 1;
        let label_w = display_width(&load_label) as u16;
        let x = if label_w >= content_w {
            content_x
        } else {
            content_x + (content_w - label_w) / 2
        };
        write_str_at(
            frame.buffer_mut(),
            x,
            y,
            &load_label,
            Style::default().fg(app.theme.inactive_fg),
        );
    }
}

pub(super) fn draw_core_meters(frame: &mut Frame, area: Rect, sample: &CpuSample, app: &App) {
    if area.height == 0 || area.width < 8 {
        return;
    }
    let theme = &app.theme;
    let n = sample.cores.len();
    if n == 0 {
        return;
    }
    let rows = area.height as usize;
    const MIN_COL_W: u16 = 16;
    // 1-cell gap between columns, filled with a faint vertical rule.
    const GAP_W: u16 = 1;
    let max_cols_by_width = {
        // each column needs MIN_COL_W, with GAP_W between adjacent columns:
        // cols * MIN_COL_W + (cols - 1) * GAP_W <= area.width
        let w = area.width;
        let mut c = 1usize;
        while ((c as u16 + 1) * MIN_COL_W + (c as u16) * GAP_W) <= w {
            c += 1;
        }
        c
    };
    let cols_by_count = n.div_ceil(rows.max(1));
    let cols = cols_by_count.min(max_cols_by_width).max(1);
    let total_gap = GAP_W * (cols.saturating_sub(1) as u16);
    let usable_w = area.width.saturating_sub(total_gap);
    let col_w = usable_w / cols as u16;
    let stride = col_w + GAP_W;
    let per_col_rows = n.div_ceil(cols);

    if cols > 1 {
        let rule_style = Style::default().fg(app.theme.div_line);
        for col in 0..cols.saturating_sub(1) {
            let x = area.x + (col as u16) * stride + col_w;
            for r in 0..(area.height as usize).min(per_col_rows) {
                write_str_at(
                    frame.buffer_mut(),
                    x,
                    area.y + r as u16,
                    "│",
                    rule_style,
                );
            }
        }
    }

    for (i, core) in sample.cores.iter().enumerate() {
        let col = i / per_col_rows;
        let row = i % per_col_rows;
        if col >= cols || row >= rows {
            break;
        }
        let x = area.x + col as u16 * stride;
        let w = if col + 1 == cols {
            area.width.saturating_sub(col as u16 * stride)
        } else {
            col_w
        };
        let row_y = area.y + row as u16;
        match app.track_chart_style {
            crate::app::TrackChartStyle::Bar => {
                let mm = MiniMeter::new(
                    format!("C{:>2}", core.id),
                    core.utilization as f64,
                    format!("{:>3}%", (core.utilization * 100.0) as u32),
                )
                .with_gradient(theme.cpu)
                .with_widths(4, 5);
                (&mm).render(Rect::new(x, row_y, w, 1), frame.buffer_mut());
            }
            crate::app::TrackChartStyle::Sparkline => {
                draw_track_sparkline_row(
                    frame.buffer_mut(),
                    Rect::new(x, row_y, w, 1),
                    &format!("C{:>2}", core.id),
                    &format!("{:>3}%", (core.utilization * 100.0) as u32),
                    app.core_history.get(&core.id),
                    theme.cpu,
                    app.theme.main_fg,
                    app.tty_graphs,
                );
            }
        }
    }
}
