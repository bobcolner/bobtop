use std::collections::VecDeque;

use bobtop_core::sample::{MemoryPressure, MemorySample};
use bobtop_tui::widgets::{BrailleGraph, GraphStyle, LegendStyle, Sparkline, StackedBar, StackedSegment};
use bobtop_tui::widgets::panel as boxed_panel;
use bobtop_tui::{format_bytes, write_str_at, Gradient};
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

    // Vertical layout — three sections (graph / breakdown / PSI) get
    // breathing room when the panel can spare it. A single blank row
    // between sections is enough to keep them visually separated; we
    // fall through to "no gap" only on cramped panels (< 12 rows of
    // inner height) so things still fit.
    //
    // Section min heights:
    //   graph:        2  (single braille row)
    //   breakdown:    4  (bar + 3-row stacked legend)
    //   psi cluster:  4  (header + 3 source rows)
    //   minimum:     10  rows; gaps inserted above 10
    let total_h = inner.height;
    let mut graph_h = total_h.min(5).saturating_sub(1).max(2);
    let want_breakdown_h: u16 = 4; // bar (1) + 3-row legend
    // Spare height beyond the rigid sections.
    let mut gap_after_graph: u16 = 0;
    let mut gap_after_breakdown: u16 = 0;
    let rigid = graph_h + want_breakdown_h + 4; // 4 = full PSI block
    if total_h >= rigid + 2 {
        // Two gaps fit — one above the breakdown, one above PSI.
        gap_after_graph = 1;
        gap_after_breakdown = 1;
    } else if total_h >= rigid + 1 {
        // Single gap — preferred slot is above the breakdown so the
        // graph doesn't visually run into the bar.
        gap_after_graph = 1;
    }
    // If panel is taller than what graph+breakdown+psi+gaps can use,
    // pad the graph (it scales meaningfully with extra rows).
    let consumed = graph_h + gap_after_graph + want_breakdown_h + gap_after_breakdown + 4;
    if total_h > consumed {
        graph_h = graph_h.saturating_add(total_h - consumed).min(8);
    }

    let graph_area = Rect::new(inner.x, inner.y, inner.width, graph_h);
    let meters_y = inner.y + graph_h + gap_after_graph;
    let meters_h = (inner.y + total_h).saturating_sub(meters_y);
    let meters_area = Rect::new(inner.x, meters_y, inner.width, meters_h);

    let mem_max_pts = graph_area.width as usize * 2;
    let mut mem_graph = BrailleGraph::new(mem_max_pts, app.theme.used)
        .with_value_fn(|v| format!("{:>5.1}%", v * 100.0))
        // Fill-from-bottom Braille — the centered-bloom variant
        // hides the actual usage trend behind a symmetric ribbon,
        // which read as "memory looks fine" even under heavy load.
        // The standard trace is what users expect from a usage
        // graph and matches CPU/Net rendering at high densities.
        .with_style(if app.tty_graphs {
            GraphStyle::Blocks
        } else {
            GraphStyle::Braille
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
    draw_memory_breakdown(frame, meters_area, s, app, gap_after_breakdown);
}

fn draw_memory_breakdown(
    frame: &mut Frame,
    area: Rect,
    s: &MemorySample,
    app: &App,
    psi_gap: u16,
) {
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
    // Three-row stacked legend (one row per segment) — each row gets
    // the full pane width for `■ Used  12.4 GiB  (62%)` so the
    // numbers read cleanly even when the segment slice on the bar
    // is thin. Falls back to one-line `List` when the panel is too
    // short to fit the stacked rows.
    let stacked_h: u16 = 1 + segments.len() as u16; // bar + per-segment legend rows
    let use_stacked = area.height >= stacked_h;
    let legend_style = if use_stacked {
        LegendStyle::Stacked
    } else {
        LegendStyle::List
    };
    let bar = StackedBar::new(&segments)
        .with_empty_bg(theme.meter_bg)
        .with_chrome_fg(theme.main_fg)
        .with_legend(area.height >= 2)
        .with_legend_style(legend_style);
    let bar_h: u16 = if use_stacked {
        stacked_h
    } else if area.height >= 3 {
        2
    } else {
        1
    };
    let bar_rect = Rect::new(area.x, area.y, area.width, bar_h);
    (&bar).render(bar_rect, frame.buffer_mut());

    let mut next_y = area.y + bar_h + psi_gap;
    let bottom = area.y + area.height;

    // PSI cluster — one row per source (cpu / mem / io) with a header
    // legend so the columns are self-documenting. Three rows because
    // the user-visible signal moves between sources: memory PSI is
    // rarely non-zero on healthy hosts (kernel only stalls on memory
    // under genuine reclaim pressure), cpu PSI tracks contention on
    // busy hosts, and io PSI fires during disk-bound work. Showing
    // all three at once removes the "PSI never works" confusion —
    // at least one source typically has signal on any active host.
    let any_psi_available = s.pressure.is_some()
        || s.cpu_pressure.is_some()
        || s.io_pressure.is_some();
    if !any_psi_available && next_y < bottom {
        write_str_at(
            frame.buffer_mut(),
            area.x,
            next_y,
            "PSI  unavailable (kernel CONFIG_PSI=n or no /proc/pressure)",
            Style::default().fg(theme.inactive_fg),
        );
        return;
    }

    // Header legend — explains the three numeric columns and titles
    // the cluster. Renders whenever at least one source row also
    // fits below it (rows_left >= 2). Without this loosening, the
    // header was being skipped on common panel heights where the
    // PSI block had only 3 rows of space available.
    let rows_left = bottom.saturating_sub(next_y);
    if rows_left >= 2 && area.width >= 32 {
        draw_psi_header(frame, Rect::new(area.x, next_y, area.width, 1), theme);
        next_y += 1;
    }

    let rows: [(&str, Option<MemoryPressure>, &VecDeque<f64>, Gradient); 3] = [
        ("cpu", s.cpu_pressure, &app.cpu_pressure_history, theme.cpu),
        ("mem", s.pressure, &app.mem_pressure_history, theme.used),
        ("io ", s.io_pressure, &app.io_pressure_history, theme.upload),
    ];
    for (label, pressure, history, gradient) in rows {
        if next_y >= bottom {
            break;
        }
        draw_psi_row(
            frame,
            Rect::new(area.x, next_y, area.width, 1),
            label,
            pressure,
            history,
            gradient,
            theme,
        );
        next_y += 1;
    }
}

/// Width of the right-aligned numeric block: " 100.0%  100.0%  100.0%"
/// = 7 cells per column × 3 columns = 21 cells, plus 2 leading spaces.
const PSI_NUMS_W: u16 = 23;

fn draw_psi_header(frame: &mut Frame, area: Rect, theme: &bobtop_tui::Theme) {
    if area.width < PSI_NUMS_W + 8 {
        return;
    }
    let buf = frame.buffer_mut();
    let dim = Style::default().fg(theme.inactive_fg);
    let title = Style::default().fg(theme.title);
    write_str_at(buf, area.x, area.y, "PSI · stall % per source", title);
    // Three column headers right-aligned to match the data columns.
    let labels = "    avg10s   avg60s  avg300s";
    let labels_w = labels.chars().count() as u16;
    let labels_x = area.right().saturating_sub(labels_w);
    write_str_at(buf, labels_x, area.y, labels, dim);
}

/// One PSI row: `cpu  ▂▁▃▂   1.9%   4.2%   2.4%`. Sparkline auto-
/// scales against its own history's max (with a 1% floor) so even
/// sub-percent pressure renders visibly — without that scaling the
/// values were 0..0.05 in a 0..=1 sparkline range and the trace
/// looked perpetually flat. The label takes the gradient endpoint
/// color so the row pairs visually with the sparkline.
fn draw_psi_row(
    frame: &mut Frame,
    area: Rect,
    label: &str,
    pressure: Option<MemoryPressure>,
    history: &VecDeque<f64>,
    gradient: Gradient,
    theme: &bobtop_tui::Theme,
) {
    if area.width < 12 || area.height == 0 {
        return;
    }
    let buf = frame.buffer_mut();
    let chrome = Style::default().fg(theme.main_fg);
    let dim = Style::default().fg(theme.inactive_fg);
    let label_style = Style::default().fg(gradient.end);

    // 4-cell colored label slot (`cpu `, `mem `, `io  `) so all three
    // rows align under each other and the colors form a key.
    let prefix = format!("{:<4}", label);
    write_str_at(buf, area.x, area.y, &prefix, label_style);
    let mut col = area.x + 4;

    // Right-aligned numeric block — three 7-cell columns matching the
    // header (`avg10s avg60s avg300s`).
    let nums = match pressure {
        Some(p) => format!(
            " {:>6.1}% {:>6.1}% {:>6.1}%",
            p.some_avg10, p.some_avg60, p.some_avg300
        ),
        None => "                  n/a  ".to_string(),
    };
    let nums_w = nums.chars().count() as u16;
    let nums_x = area.right().saturating_sub(nums_w);

    // Sparkline fills the gap. Auto-scale against history max so
    // small-but-real pressure shows up — flat 0 is unambiguous via
    // the empty space, but anything >0 should be visible.
    if col + 1 < nums_x {
        let spark_x = col + 1; // 1-cell gap so the label doesn't touch the trace
        let spark_w = nums_x.saturating_sub(spark_x);
        if spark_w > 0 {
            // Floor of 1% means a 0.5% blip still draws as half-height,
            // not as the full chart (which would mislead about scale).
            // History values are 0..=1 fractions; multiply by 100 to
            // think in percent for clarity.
            let max_pct = history
                .iter()
                .copied()
                .fold(0.0_f64, f64::max)
                .max(0.01);
            let scaled: Vec<f64> = history
                .iter()
                .map(|v| (v / max_pct).clamp(0.0, 1.0))
                .collect();
            let spark_rect = Rect::new(spark_x, area.y, spark_w, 1);
            let sparkline = Sparkline::new(&scaled, gradient).with_dim_fill(0.6);
            sparkline.render(spark_rect, buf);
        }
        col = nums_x;
    }
    let _ = col;

    // Numbers — chrome color so the live values stand out.
    write_str_at(buf, nums_x, area.y, &nums, chrome);
    let _ = dim;
}
