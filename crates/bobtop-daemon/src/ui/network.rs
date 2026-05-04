use bobtop_tui::widgets::{BrailleGraph, DualMode, GraphStyle, Trace};
use bobtop_tui::widgets::panel as boxed_panel;
use bobtop_tui::{format_rate, write_str_at};
use ratatui::layout::Rect;
use ratatui::style::Style;
use ratatui::Frame;

use crate::app::App;

use super::presenter;

pub(super) fn draw(frame: &mut Frame, area: Rect, app: &App) {
    let (rx_now, tx_now) = scoped_current_rates(app);
    let (counted, total) = interface_counts(app);
    let scale = app.net_scale_bps();
    let title = presenter::network_title(app, counted, total);
    let panel = boxed_panel(app.theme.net_box, app.theme.title, app.corner_style)
        .with_title(title)
        .with_controls(format!(
            "pk ↑{} ↓{}",
            format_rate(app.net_peak_tx()),
            format_rate(app.net_peak_rx()),
        ));
    frame.render_widget(&panel, area);
    let inner = panel.inner(area);
    if inner.width < 12 || inner.height < 4 {
        return;
    }

    let max_pts = (inner.width as usize) * 2;
    let scale_label = format!("{}/s", format_rate(scale));
    let mut graph = BrailleGraph::new(max_pts, app.theme.download)
        .with_secondary(Trace::new(max_pts, app.theme.upload), DualMode::MirroredSplit)
        .with_style(if app.tty_graphs {
            GraphStyle::Blocks
        } else {
            GraphStyle::CenteredBloom
        })
        .with_y_scale(scale_label.clone(), scale_label)
        .with_text_style(Style::default().fg(app.theme.graph_text));
    for (rx, tx) in scoped_normalized_history(app, scale) {
        graph.push_dual(rx, tx);
    }
    frame.render_widget(&graph, inner);

    overlay_center_divider(frame, inner, app, rx_now, tx_now);
}

fn scoped_current_rates(app: &App) -> (f64, f64) {
    if let Some(name) = &app.selected_iface {
        if let Some(s) = &app.latest_network {
            if let Some(iface) = s.interfaces.iter().find(|i| &i.name == name) {
                return (iface.rx_bytes_per_sec, iface.tx_bytes_per_sec);
            }
        }
        return (0.0, 0.0);
    }
    current_real_rates(app)
}

fn scoped_normalized_history(app: &App, scale: f64) -> Vec<(f64, f64)> {
    if let Some(name) = &app.selected_iface {
        if let Some(h) = app.iface_history.get(name) {
            let s = scale.max(1.0);
            return h
                .iter()
                .map(|(rx, tx)| ((rx / s).clamp(0.0, 1.0), (tx / s).clamp(0.0, 1.0)))
                .collect();
        }
        return Vec::new();
    }
    app.net_normalized_history()
}

fn overlay_center_divider(frame: &mut Frame, inner: Rect, app: &App, rx_now: f64, tx_now: f64) {
    let div_y = inner.y + inner.height / 2;
    if div_y >= inner.y + inner.height {
        return;
    }
    let style = Style::default().fg(app.theme.div_line);
    let buf = frame.buffer_mut();
    for x in 0..inner.width {
        let cell = &mut buf[(inner.x + x, div_y)];
        cell.set_char('─');
        cell.set_style(style);
    }
    let up_label = format!(" ↑ {}/s ", format_rate(tx_now));
    let dn_label = format!(" {}/s ↓ ", format_rate(rx_now));
    write_str_at(
        buf,
        inner.x + 1,
        div_y,
        &up_label,
        Style::default().fg(app.theme.upload.end),
    );
    let dn_len = dn_label.chars().count() as u16;
    if dn_len + 2 < inner.width {
        write_str_at(
            buf,
            inner.right().saturating_sub(dn_len + 1),
            div_y,
            &dn_label,
            Style::default().fg(app.theme.download.end),
        );
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
