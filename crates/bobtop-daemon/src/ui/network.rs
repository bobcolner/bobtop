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
        // Reserve one row for a subtle horizontal rule above the iface
        // block when there's space — visually anchors the interface
        // table without nesting another full BoxedPanel inside the
        // existing one. When there's only room for a single row of
        // interfaces, skip the rule rather than steal that row.
        let rule_h: u16 = if iface_rows >= 2 { 1 } else { 0 };
        let actual_iface_rows = iface_rows - rule_h;
        if rule_h == 1 {
            let rule_y = inner.y + graph_h;
            let style = Style::default().fg(app.theme.div_line);
            let buf = frame.buffer_mut();
            for x in inner.x..inner.x.saturating_add(inner.width) {
                let cell = &mut buf[(x, rule_y)];
                cell.set_char('─');
                cell.set_style(style);
            }
        }
        let rows_y = inner.y + graph_h + rule_h;
        let rows_area = Rect::new(inner.x, rows_y, inner.width, actual_iface_rows);
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
    // Heatmap denominator — biggest single-direction rate among visible
    // interfaces. Cells colour themselves at `value / peak`, so the
    // busiest row hits gradient `end` and quieter rows stay muted.
    // Floor at 1 MiB/s so an idle host doesn't drive a full-bright
    // colour for a 200 B/s heartbeat.
    let mut peak = 1024.0_f64 * 1024.0;
    for iface in interfaces {
        let r = iface.rx_bytes_per_sec.max(iface.tx_bytes_per_sec);
        if r > peak {
            peak = r;
        }
    }
    let buf = frame.buffer_mut();
    for (i, iface) in interfaces.iter().take(area.height as usize).enumerate() {
        draw_interface_row(
            buf,
            Rect::new(area.x, area.y + i as u16, area.width, 1),
            iface,
            app,
            peak,
        );
    }
}

/// Below this combined throughput an interface is treated as idle and
/// the whole row is rendered in `inactive_fg`. ~256 B/s catches the
/// background-chatter floor (mDNS pings, ARP, an idle SSH keepalive)
/// without dimming a row that's actually moving data.
const IDLE_BPS_THRESHOLD: f64 = 256.0;

fn draw_interface_row(
    buf: &mut ratatui::buffer::Buffer,
    area: Rect,
    iface: &InterfaceSample,
    app: &App,
    peak_bps: f64,
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
    let active = iface.rx_bytes_per_sec + iface.tx_bytes_per_sec >= IDLE_BPS_THRESHOLD;
    let row_fg = if active { theme.main_fg } else { theme.inactive_fg };

    // Category badge — kept in its accent colour even on idle rows so
    // the kind is always legible at a glance.
    write_str_at(
        buf,
        area.x,
        area.y,
        cat_label,
        Style::default().fg(cat_color).add_modifier(Modifier::BOLD),
    );

    // Interface name. Reserve 12 cells (was 14) — most NICs are
    // ≤6 chars (eth0, wlan0, docker0, br-…); longer names still
    // truncate cleanly.
    let name_x = area.x + 4;
    let name_max: usize = 12;
    let name = truncate_chars(&iface.name, name_max);
    write_str_at(buf, name_x, area.y, &name, Style::default().fg(row_fg));

    // Two fixed-width rate cells. Each cell is `arrow + space + 7-char
    // right-aligned value` = 9 cells, with a 1-cell gap between the
    // two arrow blocks. Worst-case rate text after format_rate() is
    // 6 cells ("999.9G/s") — 7 leaves a comfortable gutter.
    let cell_w: u16 = 9;
    let block_w = cell_w * 2 + 1;
    let name_end = name_x + name_max as u16;
    // Right-justify the rate block to the panel's right edge so the
    // ↑/↓ arrows + rates flush against the border.
    if name_end + block_w + 1 > area.right() {
        return;
    }
    let block_x = area.right().saturating_sub(block_w);
    let up_style = if active {
        rate_style(&theme.upload, iface.tx_bytes_per_sec, peak_bps)
    } else {
        Style::default().fg(theme.inactive_fg)
    };
    let dn_style = if active {
        rate_style(&theme.download, iface.rx_bytes_per_sec, peak_bps)
    } else {
        Style::default().fg(theme.inactive_fg)
    };
    let up_val = format!("{}/s", format_rate(iface.tx_bytes_per_sec));
    let dn_val = format!("{}/s", format_rate(iface.rx_bytes_per_sec));
    write_arrow_rate(buf, block_x, area.y, "↑", &up_val, cell_w, up_style);
    write_arrow_rate(buf, block_x + cell_w + 1, area.y, "↓", &dn_val, cell_w, dn_style);
}

/// Render a single arrow + right-aligned rate value into a fixed-width
/// cell. Layout: `↑` at the cell's left edge, the rate text right-
/// justified inside the remaining `cell_w - 2` cells (the `-2` reserves
/// the arrow itself + a single-cell gutter after it).
fn write_arrow_rate(
    buf: &mut ratatui::buffer::Buffer,
    x: u16,
    y: u16,
    arrow: &str,
    rate: &str,
    cell_w: u16,
    style: Style,
) {
    write_str_at(buf, x, y, arrow, style);
    let value_w = cell_w.saturating_sub(2);
    let rate_w = bobtop_tui::display_width(rate) as u16;
    let pad = value_w.saturating_sub(rate_w);
    write_str_at(buf, x + 2 + pad, y, rate, style);
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

/// Column alignment for one cell in the flow table.
#[derive(Debug, Clone, Copy)]
enum Align {
    Left,
    Right,
}

/// One column slot in the flow table. Width is in cells; alignment
/// only matters for short text (long text gets clipped at the right
/// edge regardless).
#[derive(Debug, Clone, Copy)]
struct Col {
    width: u16,
    align: Align,
}

const FLOW_COLS: [Col; 6] = [
    Col { width: 6, align: Align::Right }, // PID
    Col { width: 14, align: Align::Left },  // PROC
    Col { width: 26, align: Align::Left },  // REMOTE
    Col { width: 8, align: Align::Left },   // STATE
    Col { width: 11, align: Align::Right }, // ↓/s
    Col { width: 11, align: Align::Right }, // ↑/s
];

const FLOW_HEADERS: [&str; 6] = ["PID", "PROC", "REMOTE", "STATE", "↓/s", "↑/s"];

/// Per-flow table view of network activity. Shows every (pid, conn)
/// pair the active attributor reported, joined with the per-pid byte
/// rates from the same store. Bytes are pid-aggregate for v1 — true
/// per-flow byte attribution would require extending the eBPF program
/// to key on (pid, 5-tuple).
fn draw_flows(frame: &mut Frame, area: Rect, app: &App) {
    let Some(store) = app.attribution.as_ref() else {
        let panel = boxed_panel(app.theme.net_box, app.theme.title, app.corner_style)
            .with_title("net · flows".to_string());
        frame.render_widget(&panel, area);
        let inner = panel.inner(area);
        if inner.height >= 1 {
            write_str_clipped(
                frame.buffer_mut(),
                inner.x,
                inner.y,
                "(per-pid attribution unavailable)",
                inner.width,
                Style::default().fg(app.theme.inactive_fg),
            );
        }
        return;
    };

    // Pull and filter: hide pure listening sockets (their `remote`
    // is 0.0.0.0:0) — they don't answer any "where are bytes going"
    // question, and on a busy server they'd dominate the view.
    let mut flows: Vec<_> = store
        .flows()
        .into_iter()
        .filter(|f| !is_unbound_listener(&f.conn))
        .collect();
    let visible_count = flows.len();

    // Aggregate totals across all visible flows for the panel header.
    let (rx_total, tx_total) = aggregate_rates(store, &flows);

    let title = if visible_count == 0 {
        "net · flows".to_string()
    } else {
        format!("net · flows ({})", visible_count)
    };
    let controls = if visible_count == 0 {
        "press N to switch back".to_string()
    } else {
        format!("↑ {}/s   ↓ {}/s", format_rate(tx_total), format_rate(rx_total))
    };
    let panel = boxed_panel(app.theme.net_box, app.theme.title, app.corner_style)
        .with_title(title)
        .with_controls(controls);
    frame.render_widget(&panel, area);
    let inner = panel.inner(area);
    if inner.width < 20 || inner.height < 2 {
        return;
    }

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

    // Sort by busiest first (rx+tx desc) — that's the "what's eating
    // my network" answer the panel exists to give. Pid ascending as a
    // stable tiebreaker so equal-bandwidth rows don't shuffle between
    // ticks (HashMap iteration order in the store is otherwise free
    // to vary).
    let total_bytes = |pid: u32| -> f64 {
        store
            .net_for(pid)
            .map(|n| n.rx_bytes_per_sec.unwrap_or(0.0) + n.tx_bytes_per_sec.unwrap_or(0.0))
            .unwrap_or(0.0)
    };
    flows.sort_by(|a, b| {
        let ta = total_bytes(a.pid);
        let tb = total_bytes(b.pid);
        tb.partial_cmp(&ta)
            .unwrap_or(std::cmp::Ordering::Equal)
            .then_with(|| a.pid.cmp(&b.pid))
    });

    // Render header row with title accent + a dim underline so the
    // body rows visually anchor to it.
    let header_style = Style::default()
        .fg(app.theme.title)
        .add_modifier(Modifier::BOLD);
    let buf = frame.buffer_mut();
    let header_cells: Vec<(Col, String, Style)> = FLOW_COLS
        .iter()
        .zip(FLOW_HEADERS.iter())
        .map(|(c, h)| (*c, h.to_string(), header_style))
        .collect();
    write_row(buf, inner.x, inner.y, inner.width, &header_cells);

    // Subtle horizontal rule — separates header from data without
    // borrowing a row from the body.
    if inner.height >= 3 {
        let underline_y = inner.y + 1;
        let underline_style = Style::default().fg(app.theme.div_line);
        for x in inner.x..inner.x.saturating_add(inner.width) {
            let cell = &mut buf[(x, underline_y)];
            cell.set_char('─');
            cell.set_style(underline_style);
        }
    }

    // Pick the heatmap denominator: the biggest pid-aggregate rate
    // currently visible. Each cell colors itself by `rate / max`.
    // Falls back to a sensible 1 MiB/s scale when nothing is moving
    // so the heatmap doesn't go full-bright on idle hosts.
    let mut peak_rate = 1024.0_f64 * 1024.0;
    for f in &flows {
        if let Some(n) = store.net_for(f.pid) {
            let r = n.rx_bytes_per_sec.unwrap_or(0.0).max(n.tx_bytes_per_sec.unwrap_or(0.0));
            if r > peak_rate {
                peak_rate = r;
            }
        }
    }

    let body_top = inner.y.saturating_add(2);
    let body_h = inner.height.saturating_sub(2) as usize;
    let main_fg_style = Style::default().fg(app.theme.main_fg);
    let dim_style = Style::default().fg(app.theme.inactive_fg);
    for (i, flow) in flows.iter().take(body_h).enumerate() {
        let y = body_top + i as u16;
        let active = flow.conn.state == bobtop_pid_attr::SocketState::Established;
        // Dim non-Established rows so the eye anchors on what's
        // actually moving bytes; LISTEN/TIME_WAIT/etc. recede.
        let row_fg = if active { main_fg_style } else { dim_style };
        let pid = flow.pid.to_string();
        let proc_name = truncate_chars(&flow.name, 14);
        let remote = format_endpoint(&flow.conn.remote);
        let state = state_glyph(flow.conn.state);
        let rate = store.net_for(flow.pid);
        let rx_val = rate.and_then(|n| n.rx_bytes_per_sec);
        let tx_val = rate.and_then(|n| n.tx_bytes_per_sec);
        let rx = rx_val.map(|v| format!("{}/s", format_rate(v))).unwrap_or_else(|| "—".into());
        let tx = tx_val.map(|v| format!("{}/s", format_rate(v))).unwrap_or_else(|| "—".into());
        let rx_style = if active {
            rate_style(&app.theme.download, rx_val.unwrap_or(0.0), peak_rate)
        } else {
            dim_style
        };
        let tx_style = if active {
            rate_style(&app.theme.upload, tx_val.unwrap_or(0.0), peak_rate)
        } else {
            dim_style
        };
        let state_style = state_color(&app.theme, flow.conn.state);
        let row: [(Col, String, Style); 6] = [
            (FLOW_COLS[0], pid, row_fg),
            (FLOW_COLS[1], proc_name, row_fg),
            (FLOW_COLS[2], remote, row_fg),
            (FLOW_COLS[3], state.to_string(), state_style),
            (FLOW_COLS[4], rx, rx_style),
            (FLOW_COLS[5], tx, tx_style),
        ];
        write_row(buf, inner.x, y, inner.width, &row);
    }
}

/// Listening sockets that haven't accepted any peer yet have a remote
/// of 0.0.0.0:0 (or the v6 equivalent). They contribute no useful
/// information to a flow view focused on "where are bytes going."
fn is_unbound_listener(conn: &bobtop_pid_attr::ConnectionInfo) -> bool {
    use bobtop_pid_attr::AddrEndpoint as E;
    matches!(
        conn.remote,
        E::V4 { port: 0, .. } | E::V6 { port: 0, .. }
    )
}

/// Sum unique-per-pid rates across the given flows. Multiple flows
/// share a pid's aggregate rate; we de-duplicate by inserting into a
/// HashMap so the panel header total doesn't double-count.
fn aggregate_rates(
    store: &bobtop_pid_attr::AttributionStore,
    flows: &[bobtop_pid_attr::FlowRow],
) -> (f64, f64) {
    use std::collections::HashSet;
    let mut seen = HashSet::new();
    let mut rx = 0.0_f64;
    let mut tx = 0.0_f64;
    for f in flows {
        if !seen.insert(f.pid) {
            continue;
        }
        if let Some(n) = store.net_for(f.pid) {
            rx += n.rx_bytes_per_sec.unwrap_or(0.0);
            tx += n.tx_bytes_per_sec.unwrap_or(0.0);
        }
    }
    (rx, tx)
}

/// Sample a gradient at the position `value/peak` clamped to [0, 1].
/// Idle rows get the gradient's start color; the busiest row hits
/// `end`. Mirrors the way the classic graphs colour their fill.
fn rate_style(grad: &bobtop_tui::Gradient, value: f64, peak: f64) -> Style {
    let t = if peak > 0.0 {
        (value / peak).clamp(0.0, 1.0) as f32
    } else {
        0.0
    };
    Style::default().fg(grad.sample(t))
}

/// Pick a colour for the STATE cell. ESTABLISHED uses the title accent
/// (it's the "live" state); LISTEN gets `hi_fg` to flag it as a
/// distinct mode; everything else dims into `inactive_fg`.
fn state_color(theme: &bobtop_tui::Theme, state: bobtop_pid_attr::SocketState) -> Style {
    use bobtop_pid_attr::SocketState as S;
    let fg = match state {
        S::Established => theme.title,
        S::Listen => theme.hi_fg,
        _ => theme.inactive_fg,
    };
    Style::default().fg(fg)
}

/// Render one row, applying per-cell alignment + style. Long text is
/// clipped at the column right edge; short text is padded so right-
/// aligned numerics line up under their headers.
fn write_row(
    buf: &mut ratatui::buffer::Buffer,
    x: u16,
    y: u16,
    max_w: u16,
    cells: &[(Col, String, Style)],
) {
    let mut cx = x;
    let right = x.saturating_add(max_w);
    for (col, text, style) in cells {
        if cx >= right {
            return;
        }
        let avail = (right - cx) as u16;
        let cell_w = col.width.min(avail);
        let render_w = cell_w.saturating_sub(1).max(1);
        match col.align {
            Align::Left => {
                write_str_clipped(buf, cx, y, text, render_w, *style);
            }
            Align::Right => {
                let text_w = bobtop_tui::display_width(text) as u16;
                let pad = render_w.saturating_sub(text_w);
                write_str_clipped(buf, cx + pad, y, text, render_w, *style);
            }
        }
        cx = cx.saturating_add(col.width);
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
        S::Established => "ESTAB",
        S::Listen => "LISTEN",
        S::SynSent => "SYN-S",
        S::SynRecv | S::NewSynRecv => "SYN-R",
        S::FinWait1 | S::FinWait2 => "FIN-W",
        S::TimeWait => "TWAIT",
        S::CloseWait => "CWAIT",
        S::LastAck => "LASTAK",
        S::Closing => "CLOSNG",
        S::Close => "CLOSED",
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
