use std::cmp::Ordering;

use bobtop_collectors::{classify_interface, NetInterfaceKind};
use bobtop_core::sample::InterfaceSample;
use bobtop_tui::widgets::{BrailleGraph, DualMode, GraphStyle, Trace};
use bobtop_tui::widgets::panel as boxed_panel;
use bobtop_tui::{format_rate, truncate_chars, write_str_at, write_str_clipped};
use ratatui::layout::Rect;
use ratatui::style::{Modifier, Style};
use ratatui::Frame;

use crate::app::{App, NetworkPanelVariant};

use super::presenter;

/// Minimum vertical space (in cells) we'd like to give the graph
/// before we start eating into it for interface rows. Below this the
/// graph stops being a useful trend indicator.
const MIN_GRAPH_H: u16 = 4;

pub(super) fn draw(frame: &mut Frame, area: Rect, app: &App) {
    if app.network_panel == NetworkPanelVariant::Flows {
        draw_flows(frame, area, app);
        return;
    }
    let (rx_now, tx_now) = current_rates(app);
    let (counted, total) = interface_counts(app);
    let scale = app.net_scale_bps();
    let title = presenter::network_title(app, counted, total);
    // Live rates in the top-right slot — peaks are useful but the
    // user almost always wants "what's happening right now". Peak
    // values still drive the in-graph y-axis scale via
    // `app.net_scale_bps()`, so the trend graph stays comparable
    // across spikes.
    let panel = boxed_panel(app.theme.net_box, app.theme.title, app.corner_style)
        .with_title(title)
        .with_controls(format!(
            "↑ {}/s   ↓ {}/s",
            format_rate(tx_now),
            format_rate(rx_now),
        ));
    frame.render_widget(&panel, area);
    let inner = panel.inner(area);
    if inner.width < 12 || inner.height < 4 {
        return;
    }

    // Build the visible interface list once — we use it both for
    // sizing (how many rows to reserve below the graph) and rendering.
    let visible = visible_interfaces(app);
    let want_rows = visible.len() as u16;

    // Layout: graph eats whatever's left after we reserve up to N
    // interface rows. Cap at half the panel so a host with 20 docker
    // bridges (show_virtual_net=true) doesn't squeeze the graph to
    // nothing.
    let max_iface_rows = inner.height.saturating_sub(MIN_GRAPH_H);
    let half = inner.height / 2;
    let iface_rows = want_rows.min(max_iface_rows).min(half.max(1));
    let graph_h = inner.height.saturating_sub(iface_rows);
    let graph_area = Rect::new(inner.x, inner.y, inner.width, graph_h);

    let max_pts = (graph_area.width as usize) * 2;
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
    for (rx, tx) in app.net_normalized_history() {
        graph.push_dual(rx, tx);
    }
    frame.render_widget(&graph, graph_area);

    overlay_center_divider(frame, graph_area, app, rx_now, tx_now);

    if iface_rows > 0 {
        let rows_area = Rect::new(inner.x, inner.y + graph_h, inner.width, iface_rows);
        draw_interface_rows(frame, rows_area, &visible, app);
    }
}

/// Build the sorted list of interfaces the panel should render.
/// Default policy: hide loopback + container bridges; surface
/// external NICs and active tunnels. `show_virtual_net=true` shows
/// every interface the kernel reports. Sorted by total throughput
/// (busiest first) so the row order is meaningful.
fn visible_interfaces(app: &App) -> Vec<InterfaceSample> {
    let Some(s) = &app.latest_network else {
        return Vec::new();
    };
    let mut ifaces: Vec<InterfaceSample> = s
        .interfaces
        .iter()
        .filter(|i| {
            let kind = classify_interface(&i.name);
            app.show_virtual_net || kind.is_external()
        })
        .cloned()
        .collect();
    ifaces.sort_by(|a, b| {
        let ta = a.rx_bytes_per_sec + a.tx_bytes_per_sec;
        let tb = b.rx_bytes_per_sec + b.tx_bytes_per_sec;
        tb.partial_cmp(&ta)
            .unwrap_or(Ordering::Equal)
            .then_with(|| a.name.cmp(&b.name))
    });
    ifaces
}

fn draw_interface_rows(
    frame: &mut Frame,
    area: Rect,
    interfaces: &[InterfaceSample],
    app: &App,
) {
    let buf = frame.buffer_mut();
    for (i, iface) in interfaces.iter().take(area.height as usize).enumerate() {
        draw_interface_row(
            buf,
            Rect::new(area.x, area.y + i as u16, area.width, 1),
            iface,
            app,
        );
    }
}

fn draw_interface_row(
    buf: &mut ratatui::buffer::Buffer,
    area: Rect,
    iface: &InterfaceSample,
    app: &App,
) {
    if area.width < 16 {
        return;
    }
    let theme = &app.theme;
    let kind = classify_interface(&iface.name);
    let (cat_label, cat_color) = match kind {
        NetInterfaceKind::External => ("ext", theme.upload.end),
        NetInterfaceKind::Tunnel => ("tun", theme.cached.end),
        NetInterfaceKind::Container => ("lan", theme.inactive_fg),
        NetInterfaceKind::Loopback => ("lo ", theme.inactive_fg),
    };

    // 1: 3-cell category badge in the kind's accent color.
    write_str_at(
        buf,
        area.x,
        area.y,
        cat_label,
        Style::default().fg(cat_color).add_modifier(Modifier::BOLD),
    );

    // 2: interface name (truncated). Reserve ~14 cells; longer names
    // truncate with the standard ellipsis helper.
    let name_x = area.x + 4;
    let name_max: usize = 14;
    let name = truncate_chars(&iface.name, name_max);
    write_str_at(
        buf,
        name_x,
        area.y,
        &name,
        Style::default().fg(theme.main_fg),
    );

    // 3: right-aligned rates `↑ X/s   ↓ Y/s`. Skip when the row is
    // narrower than the rate block itself.
    let up = format!("↑ {}/s", format_rate(iface.tx_bytes_per_sec));
    let dn = format!("↓ {}/s", format_rate(iface.rx_bytes_per_sec));
    let block = format!("{}  {}", up, dn);
    let block_w = block.chars().count() as u16;
    let name_end = name_x + name_max as u16;
    if name_end + block_w + 2 <= area.right() {
        // Color the arrows by direction; main_fg for everything else
        // so the eye picks up rates first, units second.
        let block_x = area.right().saturating_sub(block_w);
        write_str_at(buf, block_x, area.y, &up, Style::default().fg(theme.upload.end));
        let dn_x = block_x + up.chars().count() as u16 + 2;
        write_str_at(
            buf,
            dn_x,
            area.y,
            &dn,
            Style::default().fg(theme.download.end),
        );
    }
}

/// Aggregate current rate across all visible (filtered) interfaces.
/// Drives the centerline labels above/below the dual-trace graph.
fn current_rates(app: &App) -> (f64, f64) {
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

fn overlay_center_divider(frame: &mut Frame, inner: Rect, app: &App, _rx_now: f64, _tx_now: f64) {
    // Plain `─` rule across the centerline — visually marks the
    // 0 / mirror axis where the upload trace flips into the
    // download trace. Inline rate labels were here too; they
    // were redundant once the live `↑/↓` numbers moved into the
    // panel's top-right slot, so dropping them keeps the data
    // surface from echoing itself.
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
}

/// Per-flow table view of network activity. Shows every (pid, conn)
/// pair the active attributor reported, joined with the per-pid byte
/// rates from the same store. Bytes are pid-aggregate for v1 — true
/// per-flow byte attribution would require extending the eBPF program
/// to key on (pid, 5-tuple).
fn draw_flows(frame: &mut Frame, area: Rect, app: &App) {
    let panel = boxed_panel(app.theme.net_box, app.theme.title, app.corner_style)
        .with_title("net · flows".to_string())
        .with_controls("press N to switch back".to_string());
    frame.render_widget(&panel, area);
    let inner = panel.inner(area);
    if inner.width < 20 || inner.height < 2 {
        return;
    }

    let Some(store) = app.attribution.as_ref() else {
        write_str_clipped(
            frame.buffer_mut(),
            inner.x,
            inner.y,
            "(per-pid attribution unavailable)",
            inner.width,
            Style::default().fg(app.theme.inactive_fg),
        );
        return;
    };
    let mut flows = store.flows();
    if flows.is_empty() {
        write_str_clipped(
            frame.buffer_mut(),
            inner.x,
            inner.y,
            "(no active connections)",
            inner.width,
            Style::default().fg(app.theme.inactive_fg),
        );
        return;
    }

    // Established connections first; within state, by pid for stability.
    flows.sort_by(|a, b| {
        let a_est = a.conn.state == bobtop_pid_attr::SocketState::Established;
        let b_est = b.conn.state == bobtop_pid_attr::SocketState::Established;
        b_est.cmp(&a_est).then_with(|| a.pid.cmp(&b.pid))
    });

    // Header row uses the panel's title style for clarity.
    let header_style = Style::default()
        .fg(app.theme.title)
        .add_modifier(Modifier::BOLD);
    let row_style = Style::default().fg(app.theme.main_fg);
    let buf = frame.buffer_mut();
    // Layout (chars): pid 6 · proc 14 · local 22 · remote 22 · state 6 · ↓/s 11 · ↑/s 11
    // Total 92; we clip to whatever the panel gives us.
    let cols: [(u16, &str); 7] = [
        (6, "PID"),
        (14, "PROC"),
        (22, "LOCAL"),
        (22, "REMOTE"),
        (6, "ST"),
        (11, "↓/s"),
        (11, "↑/s"),
    ];
    write_columns(buf, inner.x, inner.y, inner.width, &cols, header_style);

    let body_top = inner.y.saturating_add(1);
    let body_h = inner.height.saturating_sub(1) as usize;
    for (i, flow) in flows.iter().take(body_h).enumerate() {
        let y = body_top + i as u16;
        let pid = flow.pid.to_string();
        let proc_name = truncate_chars(&flow.name, 14);
        let local = format_endpoint(&flow.conn.local);
        let remote = format_endpoint(&flow.conn.remote);
        let state = state_glyph(flow.conn.state);
        let rate = store.net_for(flow.pid);
        let rx = rate
            .and_then(|n| n.rx_bytes_per_sec)
            .map(|v| format!("{}/s", format_rate(v)))
            .unwrap_or_else(|| "—".into());
        let tx = rate
            .and_then(|n| n.tx_bytes_per_sec)
            .map(|v| format!("{}/s", format_rate(v)))
            .unwrap_or_else(|| "—".into());
        let cells: [(u16, &str); 7] = [
            (6, pid.as_str()),
            (14, &proc_name),
            (22, &local),
            (22, &remote),
            (6, state),
            (11, &rx),
            (11, &tx),
        ];
        write_columns(buf, inner.x, y, inner.width, &cells, row_style);
    }
}

fn write_columns(
    buf: &mut ratatui::buffer::Buffer,
    x: u16,
    y: u16,
    max_w: u16,
    cols: &[(u16, &str)],
    style: Style,
) {
    let mut cx = x;
    let right = x.saturating_add(max_w);
    for (w, text) in cols {
        if cx >= right {
            return;
        }
        let avail = (right - cx) as usize;
        let cell_w = (*w as usize).min(avail);
        write_str_clipped(buf, cx, y, text, cell_w.saturating_sub(1).max(1) as u16, style);
        cx = cx.saturating_add(*w);
    }
}

fn format_endpoint(ep: &bobtop_pid_attr::AddrEndpoint) -> String {
    match ep {
        bobtop_pid_attr::AddrEndpoint::V4 { addr, port } => format!("{addr}:{port}"),
        bobtop_pid_attr::AddrEndpoint::V6 { addr, port } => {
            // Bracket IPv6 so the colon-port is unambiguous; the table
            // column will clip if it overflows.
            format!("[{addr}]:{port}")
        }
    }
}

fn state_glyph(s: bobtop_pid_attr::SocketState) -> &'static str {
    use bobtop_pid_attr::SocketState as S;
    match s {
        S::Established => "EST",
        S::Listen => "LSN",
        S::SynSent => "SYN",
        S::SynRecv => "SYN",
        S::FinWait1 | S::FinWait2 => "FIN",
        S::TimeWait => "TW",
        S::CloseWait => "CW",
        S::LastAck => "ACK",
        S::Closing => "CLG",
        S::Close => "CLS",
        S::NewSynRecv => "SYN",
    }
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
