//! Composes every widget through the layout engine into a single frame and
//! dumps it to stdout with truecolor ANSI. No real terminal needed.
//!
//! Run with: `cargo run --example frame_smoke -p bobtop-daemon`

use std::time::Instant;

use bobtop_core::sample::{ProcessInfo, ProcessState};
use bobtop_tui::{
    compute_layout,
    widgets::{BrailleGraph, DualMode, Meter, MiniMeter, ProcessTable, Trace},
    BoxedPanel, LayoutPreset,
};
use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::style::Color;
use ratatui::widgets::Widget;

fn main() {
    let theme = bobtop_tui::load_theme("dracula");
    let area = Rect::new(0, 0, 160, 48);
    let mut buf = Buffer::empty(area);

    let layout = compute_layout(area, LayoutPreset::Full);

    // ---- CPU panel ----
    let cpu_panel = BoxedPanel::new(theme.cpu_box, theme.title)
        .with_title("¹cpu  CPU 24.6% Cores=12")
        .with_controls("2000ms");
    (&cpu_panel).render(layout.cpu, &mut buf);
    let cpu_inner = cpu_panel.inner(layout.cpu);
    let (cpu_left, cpu_right) = split_horizontal(cpu_inner, 0.7);
    let mut cpu_graph = BrailleGraph::new(240, theme.cpu)
        .with_value_fn(|v| format!("{:>5.1}%", v * 100.0));
    for i in 0..240 {
        let v = ((i as f64 * 0.07).sin() * 0.4 + 0.5).clamp(0.0, 1.0);
        cpu_graph.push(v);
    }
    (&cpu_graph).render(cpu_left, &mut buf);

    // Per-core mini meters on the right.
    let core_count = cpu_right.height as usize;
    for i in 0..core_count {
        let frac = ((i as f64 * 0.7).sin() * 0.4 + 0.5).clamp(0.05, 0.99);
        let mm = MiniMeter::new(format!("C{:>2}", i), frac, format!("{}%", (frac * 100.0) as u32))
            .with_trailing(format!("{}°C", 40 + (i % 8)))
            .with_gradient(theme.cpu)
            .with_widths(4, 12);
        let row = Rect::new(cpu_right.x, cpu_right.y + i as u16, cpu_right.width, 1);
        (&mm).render(row, &mut buf);
    }

    // ---- Memory panel ----
    let mem_area = layout.memory.unwrap();
    let mem_panel = BoxedPanel::new(theme.mem_box, theme.title).with_title("²mem");
    (&mem_panel).render(mem_area, &mut buf);
    let mem_inner = mem_panel.inner(mem_area);
    let row_h = mem_inner.height / 4;
    let categories = [
        ("Used:", "8.91 GiB", 0.57, theme.used),
        ("Available:", "6.66 GiB", 0.43, theme.available),
        ("Cached:", "4.03 GiB", 0.26, theme.cached),
        ("Free:", "1.54 GiB", 0.10, theme.free),
    ];
    for (i, (label, value, frac, grad)) in categories.iter().enumerate() {
        let r = Rect::new(
            mem_inner.x,
            mem_inner.y + (i as u16) * row_h,
            mem_inner.width,
            row_h.saturating_sub(0).max(3),
        );
        let m = Meter::new(*label, *value, *frac)
            .with_gradient(*grad)
            .with_meter_bg(theme.meter_bg)
            .with_text_colors(theme.main_fg, theme.title);
        (&m).render(r, &mut buf);
    }

    // ---- Network panel: dual-trace mirrored split ----
    let net_area = layout.network.unwrap();
    let net_panel = BoxedPanel::new(theme.net_box, theme.title)
        .with_title("³net  192.168.1.11")
        .with_controls("auto zero <b eth0 n>");
    (&net_panel).render(net_area, &mut buf);
    let net_inner = net_panel.inner(net_area);
    let mut net_graph = BrailleGraph::new(net_inner.width as usize * 2, theme.download)
        .with_y_scale("2M", "13K")
        .with_secondary(
            Trace::new(net_inner.width as usize * 2, theme.upload),
            DualMode::MirroredSplit,
        );
    for i in 0..net_graph.primary.max_points {
        let dl = ((i as f64 * 0.13).sin() * 0.35 + 0.55).clamp(0.0, 1.0);
        let ul = ((i as f64 * 0.21).cos() * 0.30 + 0.40).clamp(0.0, 1.0);
        net_graph.push_dual(dl, ul);
    }
    (&net_graph).render(net_inner, &mut buf);

    // ---- Process table ----
    let proc_panel = BoxedPanel::new(theme.proc_box, theme.title)
        .with_title("⁴proc")
        .with_keybinds("+ select  info  terminate  kill  signals");
    (&proc_panel).render(layout.processes, &mut buf);
    let proc_inner = proc_panel.inner(layout.processes);
    let rows = sample_processes();
    let table = ProcessTable::new(&rows, &theme).with_selection(Some(2), 0);
    (&table).render(proc_inner, &mut buf);

    print_buffer(&buf, area);
}

fn split_horizontal(area: Rect, left_frac: f32) -> (Rect, Rect) {
    let lw = ((area.width as f32) * left_frac) as u16;
    let rw = area.width - lw;
    (
        Rect::new(area.x, area.y, lw, area.height),
        Rect::new(area.x + lw, area.y, rw, area.height),
    )
}

fn sample_processes() -> Vec<ProcessInfo> {
    let names = [
        ("firefox", 0.42, 758),
        ("python3", 0.19, 314),
        ("node", 0.15, 308),
        ("cargo", 0.12, 248),
        ("postgres", 0.08, 196),
        ("Xorg", 0.05, 79),
        ("redis", 0.04, 64),
        ("nvim", 0.02, 42),
    ];
    names
        .iter()
        .enumerate()
        .map(|(i, (name, cpu, mem_mb))| ProcessInfo {
            pid: 1000 + i as u32 * 137,
            parent_pid: None,
            name: (*name).into(),
            cmdline: String::new(),
            user: "gnm".into(),
            state: ProcessState::Running,
            cpu_fraction: *cpu,
            mem_rss_bytes: (*mem_mb as u64) * 1024 * 1024,
            mem_vsz_bytes: (*mem_mb as u64) * 2 * 1024 * 1024,
            threads: 4 + i as u32,
            net_rx_bytes_per_sec: None,
            net_tx_bytes_per_sec: None,
        })
        .collect()
}

fn print_buffer(buf: &Buffer, area: Rect) {
    let _ = Instant::now();
    for y in 0..area.height {
        let mut last_fg = (0u8, 0u8, 0u8);
        let mut last_bg: Option<(u8, u8, u8)> = None;
        let mut wrote_reset = true;
        for x in 0..area.width {
            let cell = &buf[(x, y)];
            let fg = match cell.style().fg.unwrap_or(Color::Reset) {
                Color::Rgb(r, g, b) => (r, g, b),
                _ => (200, 200, 200),
            };
            let bg = match cell.style().bg {
                Some(Color::Rgb(r, g, b)) => Some((r, g, b)),
                _ => None,
            };
            if wrote_reset || fg != last_fg {
                print!("\x1b[38;2;{};{};{}m", fg.0, fg.1, fg.2);
                last_fg = fg;
            }
            if bg != last_bg {
                match bg {
                    Some((r, g, b)) => print!("\x1b[48;2;{r};{g};{b}m"),
                    None => print!("\x1b[49m"),
                }
                last_bg = bg;
            }
            wrote_reset = false;
            print!("{}", cell.symbol());
        }
        println!("\x1b[0m");
    }
}
