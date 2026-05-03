//! Visual smoke test for the BrailleGraph widget.
//!
//! Renders a few graphs into a `Buffer` (no real terminal needed) and dumps
//! them to stdout with ANSI truecolor escapes so you can eyeball the gradient
//! and braille glyphs.
//!
//! Run with: `cargo run --example braille_smoke -p bobtop-daemon`

use std::time::Duration;

use bobtop_collectors::CpuCollector;
use bobtop_core::Collector;
use bobtop_tui::widgets::{BrailleGraph, DualMode, GraphStyle, Trace};
use bobtop_tui::{load_theme, BoxedPanel};
use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::style::Color;
use ratatui::widgets::Widget;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let theme = load_theme("dracula");
    println!("theme: {}\n", theme.name);

    // Synthetic data: a sine + noise fill so the gradient shows clearly.
    let mut graph = BrailleGraph::new(120, theme.cpu)
        .with_label("¹cpu")
        .with_value_fn(|v| format!("{:>5.1}%", v * 100.0));
    for i in 0..120 {
        let t = i as f64 / 12.0;
        let v = (t.sin() * 0.4 + 0.5).clamp(0.05, 0.99);
        graph.push(v);
    }

    let net = BrailleGraph::new(120, theme.download)
        .with_y_scale("2M", "13K")
        .with_secondary(Trace::new(120, theme.upload), DualMode::MirroredSplit);
    let mut net = net;
    for i in 0..120 {
        let dl = ((i as f64 * 0.13).sin() * 0.35 + 0.6).clamp(0.0, 1.0);
        let ul = ((i as f64 * 0.21).cos() * 0.30 + 0.4).clamp(0.0, 1.0);
        net.push_dual(dl, ul);
    }

    let blocks = BrailleGraph::new(80, theme.used)
        .with_label("tty")
        .with_style(GraphStyle::Blocks);
    let mut blocks = blocks;
    for i in 0..80 {
        let v = ((i as f64 * 0.18).sin() * 0.4 + 0.5).clamp(0.0, 1.0);
        blocks.push(v);
    }

    let mut live_cpu = BrailleGraph::new(80, theme.cpu).with_label("live cpu");
    let cpu_collector = CpuCollector::new();
    let _ = cpu_collector.collect().await?;
    for _ in 0..40 {
        tokio::time::sleep(Duration::from_millis(40)).await;
        let s = cpu_collector.collect().await?;
        live_cpu.push(s.aggregate_utilization as f64);
    }

    print_panel("synthetic cpu (braille + fill + gradient)", &graph, &theme.cpu_box, &theme.title, 60, 10);
    println!();
    print_panel("synthetic net (mirrored split: dl top, ul bottom)", &net, &theme.net_box, &theme.title, 60, 12);
    println!();
    print_panel("tty fallback (block density)", &blocks, &theme.cpu_box, &theme.title, 60, 6);
    println!();
    print_panel("live cpu sampled at 25hz for 1.6s", &live_cpu, &theme.cpu_box, &theme.title, 60, 8);

    Ok(())
}

fn print_panel(
    title: &str,
    graph: &BrailleGraph,
    border: &Color,
    title_color: &Color,
    width: u16,
    height: u16,
) {
    let panel = BoxedPanel::new(*border, *title_color)
        .with_title(title)
        .with_controls("60fps");
    let area = Rect::new(0, 0, width, height);
    let mut buf = Buffer::empty(area);
    (&panel).render(area, &mut buf);
    let inner = panel.inner(area);
    graph.render(inner, &mut buf);
    print_buffer(&buf, area);
}

fn print_buffer(buf: &Buffer, area: Rect) {
    for y in 0..area.height {
        for x in 0..area.width {
            let cell = &buf[(x, y)];
            let fg = cell.style().fg.unwrap_or(Color::Reset);
            let (r, g, b) = match fg {
                Color::Rgb(r, g, b) => (r, g, b),
                _ => (200, 200, 200),
            };
            print!("\x1b[38;2;{r};{g};{b}m{}", cell.symbol());
        }
        println!("\x1b[0m");
    }
}
