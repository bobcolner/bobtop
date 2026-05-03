//! Pure frame composition. `draw` is the single render function called by
//! the TUI loop on every frame.

use std::collections::HashMap;

use bobtop_core::sample::{CpuSample, FilesystemSample, MemorySample, ProcessInfo};
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
    if let Some(disks_area) = layout.disks {
        draw_disks(frame, disks_area, app);
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
    let panel = BoxedPanel::new(app.theme.mem_box, app.theme.title).with_title("²mem");
    frame.render_widget(&panel, area);
    let inner = panel.inner(area);

    let Some(s) = &app.latest_mem else {
        return;
    };

    let categories = build_memory_categories(s, &app.theme);
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

// ---------------------------------------------------------------------------
// Disks
// ---------------------------------------------------------------------------

fn draw_disks(frame: &mut Frame, area: Rect, app: &App) {
    let title = match app.latest_disk.as_ref().map(|d| d.filesystems.len()) {
        Some(n) if n > 0 => format!("²disks  {} mounts", n),
        _ => "²disks".to_string(),
    };
    let panel = BoxedPanel::new(app.theme.mem_box, app.theme.title).with_title(title);
    frame.render_widget(&panel, area);
    let inner = panel.inner(area);
    if inner.width < 8 || inner.height < 2 {
        return;
    }

    let Some(disk) = &app.latest_disk else { return };
    if disk.filesystems.is_empty() {
        return;
    }

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
        let r = Rect::new(inner.x, y, inner.width, row_h);
        m.render(r, frame.buffer_mut());
    }
}

fn build_disk_meter(fs: &FilesystemSample, theme: &bobtop_tui::Theme) -> Meter {
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
    let label = match fs.io_utilization {
        Some(io) if io > 0.05 => format!("{}: io {:.0}%", fs.label, io * 100.0),
        _ => format!("{}:", fs.label),
    };
    Meter::new(label, value, frac)
        .with_gradient(theme.used)
        .with_meter_bg(theme.meter_bg)
        .with_text_colors(theme.main_fg, theme.title)
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
    let (rx_now, tx_now) = current_real_rates(app);
    let (counted, total) = interface_counts(app);
    let title = if total == 0 {
        "³net  [no interfaces — check /proc/net/dev]".to_string()
    } else if !app.show_virtual_net && counted < total {
        format!("³net  {counted}/{total} ifaces  attributor: {}", app.net_tier.name())
    } else {
        format!("³net  {counted} ifaces  attributor: {}", app.net_tier.name())
    };
    let bandwidth_note = if app.net_tier.has_bandwidth() {
        ""
    } else {
        "  (per-pid bw unavailable)"
    };
    let panel = BoxedPanel::new(app.theme.net_box, app.theme.title)
        .with_title(format!("{title}{bandwidth_note}"))
        .with_controls(format!("peak {}/s", format_rate(app.net_scale_bps)));
    frame.render_widget(&panel, area);
    let inner = panel.inner(area);
    if inner.width < 12 || inner.height < 4 {
        return;
    }

    // Split the inner area: graph on the left, summary panel on the right.
    let side_w = (inner.width / 4).clamp(18, 32);
    let graph_w = inner.width.saturating_sub(side_w + 1);
    let graph_area = Rect::new(inner.x, inner.y, graph_w, inner.height);
    let side_area = Rect::new(inner.x + graph_w + 1, inner.y, side_w, inner.height);

    // Build graph with mirrored split: download top, upload bottom (mirrored).
    let mut graph = BrailleGraph::new((graph_area.width as usize) * 2, app.theme.download)
        .with_y_scale(
            format!("↑ {}/s", format_rate(app.net_scale_bps)),
            format!("↓ {}/s", format_rate(app.net_scale_bps)),
        )
        .with_secondary(
            Trace::new((graph_area.width as usize) * 2, app.theme.upload),
            DualMode::MirroredSplit,
        );
    if app.tty_graphs {
        graph = graph.with_style(GraphStyle::Blocks);
    }
    for (rx, tx) in app.net_normalized_history() {
        graph.push_dual(rx, tx);
    }
    frame.render_widget(&graph, graph_area);

    // Side panel: clear current rates with bps + total.
    draw_net_side(frame, side_area, app, rx_now, tx_now);
}

fn draw_net_side(frame: &mut Frame, area: Rect, app: &App, rx_now: f64, tx_now: f64) {
    let buf = frame.buffer_mut();
    let dim = ratatui::style::Style::default().fg(app.theme.inactive_fg);
    let dl = ratatui::style::Style::default().fg(app.theme.download.end);
    let ul = ratatui::style::Style::default().fg(app.theme.upload.end);
    let txt = ratatui::style::Style::default().fg(app.theme.main_fg);

    let lines: Vec<(ratatui::style::Style, String)> = vec![
        (dim, "▼ download".to_string()),
        (dl, format!("  {}/s", format_rate(rx_now))),
        (txt, format!("  ({} bps)", format_bps(rx_now))),
        (ratatui::style::Style::default(), String::new()),
        (dim, "▲ upload".to_string()),
        (ul, format!("  {}/s", format_rate(tx_now))),
        (txt, format!("  ({} bps)", format_bps(tx_now))),
    ];
    for (i, (style, s)) in lines.iter().enumerate() {
        if i as u16 >= area.height {
            break;
        }
        let y = area.y + i as u16;
        let mut x = area.x;
        for ch in s.chars().take(area.width as usize) {
            let cell = &mut buf[(x, y)];
            cell.set_char(ch);
            cell.set_style(*style);
            x = x.saturating_add(1);
        }
    }
}

fn format_bps(bytes_per_sec: f64) -> String {
    let bps = bytes_per_sec * 8.0;
    if bps >= 1_000_000.0 {
        format!("{:.1}M", bps / 1_000_000.0)
    } else if bps >= 1_000.0 {
        format!("{:.1}K", bps / 1_000.0)
    } else {
        format!("{:.0}", bps)
    }
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
    let arrow = if app.proc_sort_descending { '↓' } else { '↑' };
    let title = format!(
        "⁴proc  {} processes  sort: ←{}{}→",
        app.processes_sorted.len(),
        app.proc_sort.label(),
        arrow,
    );
    let panel = BoxedPanel::new(app.theme.proc_box, app.theme.title)
        .with_title(title)
        .with_keybinds("q quit  ↑↓ select  ←→ sort col  r reverse  +/- tick  1 full  m minimal");
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

    let mut table = ProcessTable::new(&with_net, &app.theme)
        .with_selection(Some(app.selected_proc), scroll_offset)
        .with_net_columns(app.net_tier.has_bandwidth())
        .with_direction(app.proc_sort_descending);
    table.sort = app.proc_sort;
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
