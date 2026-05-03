//! Pure frame composition. `draw` is the single render function called by
//! the TUI loop on every frame.

use std::collections::HashMap;

use bobtop_core::sample::{CpuSample, MemorySample, ProcessInfo};
use bobtop_tui::widgets::{
    BrailleGraph, DualMode, GraphStyle, Meter, MiniMeter, ProcessTable, Trace,
};
use bobtop_tui::{compute_layout, BoxedPanel, Theme};
use ratatui::layout::Rect;
use ratatui::widgets::Widget;
use ratatui::Frame;

use crate::app::App;

pub fn draw(frame: &mut Frame, app: &App) {
    let area = frame.area();
    let layout = compute_layout(area, app.layout_preset);

    draw_cpu(frame, layout.cpu, app);
    if let Some(mem_area) = layout.memory {
        draw_memory(frame, mem_area, app);
    }
    if let Some(net_area) = layout.network {
        draw_network(frame, net_area, app);
    }
    draw_processes(frame, layout.processes, app);
}

// ---------------------------------------------------------------------------
// CPU
// ---------------------------------------------------------------------------

fn draw_cpu(frame: &mut Frame, area: Rect, app: &App) {
    let cpu_pct = app
        .latest_cpu
        .as_ref()
        .map(|s| s.aggregate_utilization * 100.0)
        .unwrap_or(0.0);
    let cores = app
        .latest_cpu
        .as_ref()
        .map(|s| s.cores.len())
        .unwrap_or(0);
    let load = app
        .latest_cpu
        .as_ref()
        .and_then(|s| s.load_average)
        .map(|l| format!("load {:.2} {:.2} {:.2}", l.one, l.five, l.fifteen))
        .unwrap_or_else(|| "load — — —".into());

    let title = format!("¹cpu  CPU {:.1}%  Cores={}  {}", cpu_pct, cores, load);
    let panel = BoxedPanel::new(app.theme.cpu_box, app.theme.title)
        .with_title(title)
        .with_controls(format!("- {}ms +", app.tick_ms()));
    frame.render_widget(&panel, area);
    let inner = panel.inner(area);
    if inner.width < 4 || inner.height < 2 {
        return;
    }

    // Split: graph on the left ~70%, mini-meters on the right.
    let split = ((inner.width as f32) * 0.70) as u16;
    let graph_area = Rect::new(inner.x, inner.y, split, inner.height);
    let meters_area = Rect::new(inner.x + split, inner.y, inner.width - split, inner.height);

    let mut graph = BrailleGraph::new(
        (graph_area.width as usize) * 2,
        app.theme.cpu,
    );
    graph = graph.with_value_fn(|v| format!("{:>5.1}%", v * 100.0));
    if app.tty_graphs {
        graph = graph.with_style(GraphStyle::Blocks);
    }
    // Replay history.
    for v in app.cpu_history.iter().copied() {
        graph.push(v);
    }
    frame.render_widget(&graph, graph_area);

    if let Some(s) = &app.latest_cpu {
        draw_core_meters(frame, meters_area, s, &app.theme);
    }
}

fn draw_core_meters(frame: &mut Frame, area: Rect, sample: &CpuSample, theme: &Theme) {
    if area.height == 0 || area.width < 8 {
        return;
    }
    let visible = (area.height as usize).min(sample.cores.len());
    for (i, core) in sample.cores.iter().take(visible).enumerate() {
        let temp = core
            .temperature_c
            .map(|t| format!("{:.0}°C", t))
            .unwrap_or_else(|| "—".into());
        let mm = MiniMeter::new(
            format!("C{:>2}", core.id),
            core.utilization as f64,
            format!("{:>3}%", (core.utilization * 100.0) as u32),
        )
        .with_trailing(temp)
        .with_gradient(theme.cpu)
        .with_widths(4, 5);
        let row = Rect::new(area.x, area.y + i as u16, area.width, 1);
        let cell = &mm; // borrow for render
        cell.render(row, frame.buffer_mut());
    }
}

// ---------------------------------------------------------------------------
// Memory
// ---------------------------------------------------------------------------

fn draw_memory(frame: &mut Frame, area: Rect, app: &App) {
    // Top-disk badge in the title when we have data.
    let disk_summary = app.latest_disk.as_ref().and_then(|d| {
        d.devices
            .iter()
            .max_by(|a, b| {
                a.utilization
                    .partial_cmp(&b.utilization)
                    .unwrap_or(std::cmp::Ordering::Equal)
            })
            .map(|d| format!("  {} {:.0}%", d.name, d.utilization * 100.0))
    });
    let title = match disk_summary {
        Some(s) => format!("²mem +disk {}", s),
        None => "²mem".to_string(),
    };
    let panel = BoxedPanel::new(app.theme.mem_box, app.theme.title).with_title(title);
    frame.render_widget(&panel, area);
    let inner = panel.inner(area);

    let Some(s) = &app.latest_mem else {
        return;
    };

    let mut categories = build_memory_categories(s, &app.theme);
    // Append disk meters (top device by utilization, then by write rate) so
    // mem + disk share one panel until we extend the layout to a dedicated box.
    if let Some(disk) = &app.latest_disk {
        let mut top_disks: Vec<_> = disk.devices.iter().collect();
        top_disks.sort_by(|a, b| {
            (b.read_bytes_per_sec + b.write_bytes_per_sec)
                .partial_cmp(&(a.read_bytes_per_sec + a.write_bytes_per_sec))
                .unwrap_or(std::cmp::Ordering::Equal)
        });
        for d in top_disks.iter().take(2) {
            let total = d.read_bytes_per_sec + d.write_bytes_per_sec;
            let value = format!(
                "↓{}/s ↑{}/s",
                format_rate(d.read_bytes_per_sec),
                format_rate(d.write_bytes_per_sec),
            );
            categories.push(
                Meter::new(format!("{}:", d.name), value, d.utilization as f64)
                    .with_gradient(app.theme.cached)
                    .with_meter_bg(app.theme.meter_bg)
                    .with_text_colors(app.theme.main_fg, app.theme.title),
            );
            let _ = total; // total unused, kept in case we want it in label later
        }
    }

    if categories.is_empty() || inner.height < 3 {
        return;
    }
    let row_h = (inner.height / categories.len() as u16).max(3);
    for (i, m) in categories.iter().enumerate() {
        let y = inner.y + (i as u16) * row_h;
        if y + 3 > inner.y + inner.height {
            break;
        }
        let r = Rect::new(inner.x, y, inner.width, row_h);
        m.render(r, frame.buffer_mut());
    }
}

fn build_memory_categories(s: &MemorySample, theme: &Theme) -> Vec<Meter> {
    let total = s.total_bytes.max(1);
    let used_frac = s.used_bytes as f64 / total as f64;
    let avail_frac = s.available_bytes as f64 / total as f64;
    let mut out = Vec::with_capacity(3);
    out.push(
        Meter::new("Used:", format_bytes(s.used_bytes), used_frac)
            .with_gradient(theme.used)
            .with_meter_bg(theme.meter_bg)
            .with_text_colors(theme.main_fg, theme.title),
    );
    out.push(
        Meter::new("Available:", format_bytes(s.available_bytes), avail_frac)
            .with_gradient(theme.available)
            .with_meter_bg(theme.meter_bg)
            .with_text_colors(theme.main_fg, theme.title),
    );
    if s.swap_total_bytes > 0 {
        let swap_frac = s.swap_used_bytes as f64 / s.swap_total_bytes as f64;
        out.push(
            Meter::new(
                "Swap:",
                format!(
                    "{} / {}",
                    format_bytes(s.swap_used_bytes),
                    format_bytes(s.swap_total_bytes)
                ),
                swap_frac,
            )
            .with_gradient(theme.used)
            .with_meter_bg(theme.meter_bg)
            .with_text_colors(theme.main_fg, theme.title),
        );
    }
    out
}

// ---------------------------------------------------------------------------
// Network
// ---------------------------------------------------------------------------

fn draw_network(frame: &mut Frame, area: Rect, app: &App) {
    // Aggregate current rates from the latest NetworkSample (real per-interface
    // counters), excluding loopback / virtual interfaces (unless --show-virtual-net).
    let (rx_now, tx_now) = current_real_rates(app);
    let (counted, total) = interface_counts(app);
    let filter_note = if !app.show_virtual_net && counted < total {
        format!("  [{counted}/{total} ifaces — pass --show-virtual-net to include virtual]")
    } else if total == 0 {
        "  [no interfaces — check /proc/net/dev]".to_string()
    } else {
        format!("  [{counted}/{total} ifaces]")
    };
    let title = format!(
        "³net  ↓{}/s  ↑{}/s  attributor: {}{}{}",
        format_rate(rx_now),
        format_rate(tx_now),
        app.net_tier.name(),
        if app.net_tier.has_bandwidth() {
            ""
        } else {
            " (no per-pid bw)"
        },
        filter_note,
    );
    let panel = BoxedPanel::new(app.theme.net_box, app.theme.title)
        .with_title(title)
        .with_controls(format!("scale {}/s", format_rate(app.net_scale_bps)));
    frame.render_widget(&panel, area);
    let inner = panel.inner(area);
    if inner.width < 4 || inner.height < 2 {
        return;
    }

    let mut graph = BrailleGraph::new((inner.width as usize) * 2, app.theme.download)
        .with_y_scale(format!("↑{}/s", format_rate(app.net_scale_bps)), format!("↓{}/s", format_rate(app.net_scale_bps)))
        .with_secondary(
            Trace::new((inner.width as usize) * 2, app.theme.upload),
            DualMode::MirroredSplit,
        );
    if app.tty_graphs {
        graph = graph.with_style(GraphStyle::Blocks);
    }
    for (rx, tx) in app.net_normalized_history() {
        graph.push_dual(rx, tx);
    }
    frame.render_widget(&graph, inner);
}

fn current_real_rates(app: &App) -> (f64, f64) {
    let Some(s) = &app.latest_network else {
        return (0.0, 0.0);
    };
    let mut rx = 0.0;
    let mut tx = 0.0;
    for iface in &s.interfaces {
        if !app.show_virtual_net && bobtop_collectors::is_virtual_interface(&iface.name) {
            continue;
        }
        rx += iface.rx_bytes_per_sec;
        tx += iface.tx_bytes_per_sec;
    }
    (rx, tx)
}

fn interface_counts(app: &App) -> (usize, usize) {
    let Some(s) = &app.latest_network else {
        return (0, 0);
    };
    let total = s.interfaces.len();
    let counted = if app.show_virtual_net {
        total
    } else {
        s.interfaces
            .iter()
            .filter(|i| !bobtop_collectors::is_virtual_interface(&i.name))
            .count()
    };
    (counted, total)
}

// ---------------------------------------------------------------------------
// Processes
// ---------------------------------------------------------------------------

fn draw_processes(frame: &mut Frame, area: Rect, app: &App) {
    let title = format!("⁴proc  {} processes", app.processes_sorted.len());
    let panel = BoxedPanel::new(app.theme.proc_box, app.theme.title)
        .with_title(title)
        .with_keybinds("q quit  ↑↓ select  1 full  m minimal  +/- tick  PgUp/PgDn jump");
    frame.render_widget(&panel, area);
    let inner = panel.inner(area);
    if inner.width < 20 || inner.height < 2 {
        return;
    }

    // Join per-process net data (when available) into a working list so the
    // table can show RX/TX columns sourced from the active tier.
    let net_index: HashMap<u32, &bobtop_net::ProcessNetSample> = app
        .net_samples
        .iter()
        .map(|s| (s.pid, s))
        .collect();
    let with_net: Vec<ProcessInfo> = app
        .processes_sorted
        .iter()
        .map(|p| {
            let mut q = p.clone();
            if let Some(n) = net_index.get(&p.pid) {
                q.net_rx_bytes_per_sec = n.rx_bytes_per_sec;
                q.net_tx_bytes_per_sec = n.tx_bytes_per_sec;
            }
            q
        })
        .collect();

    let body_h = inner.height.saturating_sub(1) as usize;
    let mut scroll_offset = app.scroll_offset;
    if app.selected_proc >= scroll_offset + body_h && body_h > 0 {
        scroll_offset = app.selected_proc + 1 - body_h;
    }

    let table = ProcessTable::new(&with_net, &app.theme)
        .with_selection(Some(app.selected_proc), scroll_offset)
        .with_net_columns(app.net_tier.has_bandwidth());
    frame.render_widget(&table, inner);
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

fn format_bytes(b: u64) -> String {
    const KIB: u64 = 1024;
    const MIB: u64 = KIB * 1024;
    const GIB: u64 = MIB * 1024;
    const TIB: u64 = GIB * 1024;
    if b >= TIB {
        format!("{:.2} TiB", b as f64 / TIB as f64)
    } else if b >= GIB {
        format!("{:.2} GiB", b as f64 / GIB as f64)
    } else if b >= MIB {
        format!("{:.0} MiB", b as f64 / MIB as f64)
    } else if b >= KIB {
        format!("{:.0} KiB", b as f64 / KIB as f64)
    } else {
        format!("{b} B")
    }
}

fn format_rate(bps: f64) -> String {
    if bps >= 1024.0 * 1024.0 {
        format!("{:.1}M", bps / (1024.0 * 1024.0))
    } else if bps >= 1024.0 {
        format!("{:.0}K", bps / 1024.0)
    } else {
        format!("{:.0}B", bps)
    }
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
}
