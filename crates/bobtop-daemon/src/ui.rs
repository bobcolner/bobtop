//! Pure frame composition. `draw` is the single render function called by
//! the TUI loop on every frame.

use bobtop_core::sample::{CpuSample, FilesystemSample, MemorySample};
use bobtop_core::Box as BoxKind;
use bobtop_tui::widgets::{
    BoxedPanel, BrailleGraph, DualMode, GraphStyle, Meter, MiniMeter, ProcessTable, Trace,
};
use bobtop_tui::{compute_layout, Theme};
use ratatui::layout::Rect;
use ratatui::style::Style;
use ratatui::widgets::Widget;
use ratatui::Frame;

use crate::app::App;

/// Header height in rows. Reserved at the top of the frame for the status
/// bar (B1) — the rest of the layout flows beneath.
const HEADER_H: u16 = 1;

/// Construct a `BoxedPanel` with the user's configured corner style
/// (rounded vs square — B12). Centralized so the corner choice doesn't
/// have to be threaded through every `draw_*` function. Named with a
/// `mk_` prefix so it doesn't shadow the local `let panel = ...` idiom
/// the draw functions use.
fn mk_panel(app: &App, border: ratatui::style::Color, title: ratatui::style::Color) -> BoxedPanel {
    BoxedPanel::new(border, title).with_corner_style(app.corner_style)
}

pub fn draw(frame: &mut Frame, app: &App) {
    let area = frame.area();
    if area.height == 0 || area.width == 0 {
        return;
    }
    let header = Rect::new(area.x, area.y, area.width, HEADER_H.min(area.height));
    let body_y = area.y + header.height;
    let body_h = area.height.saturating_sub(header.height);
    let body = Rect::new(area.x, body_y, area.width, body_h);

    draw_header(frame, header, app);

    if body.height == 0 {
        return;
    }
    let layout = compute_layout(body, app.layout_preset);

    // Each panel checks its bit in `app.boxes`; a hidden panel renders a
    // small "[hidden — press B to show]" placeholder so the layout shape
    // stays stable and the user has a hint about how to reveal it. The
    // collector for that box is also paused via A3, so the hidden state
    // really is free.
    if app.boxes.is_enabled(BoxKind::Cpu) {
        draw_cpu(frame, layout.cpu, app);
    } else {
        draw_hidden_panel(frame, layout.cpu, app, "cpu");
    }
    if let Some(mem_area) = layout.memory {
        if app.boxes.is_enabled(BoxKind::Memory) {
            draw_memory(frame, mem_area, app);
        } else {
            draw_hidden_panel(frame, mem_area, app, "mem");
        }
    }
    if let Some(disks_area) = layout.disks {
        if app.boxes.is_enabled(BoxKind::Disk) {
            draw_disks(frame, disks_area, app);
        } else {
            draw_hidden_panel(frame, disks_area, app, "disks");
        }
    }
    if let Some(net_area) = layout.network {
        if app.boxes.is_enabled(BoxKind::Network) {
            draw_network(frame, net_area, app);
        } else {
            draw_hidden_panel(frame, net_area, app, "net");
        }
    }
    if app.boxes.is_enabled(BoxKind::Process) {
        draw_processes(frame, layout.processes, app);
    } else {
        draw_hidden_panel(frame, layout.processes, app, "proc");
    }

    if app.show_boxes_overlay {
        draw_boxes_overlay(frame, area, app);
    }
    if app.pending_kill.is_some() {
        draw_kill_dialog(frame, area, app);
    }
    if app.detail.is_some() {
        draw_detail_modal(frame, area, app);
    }
    if app.options.is_some() {
        draw_options_overlay(frame, area, app);
    }
    if app.show_help {
        draw_help_overlay(frame, area, app);
    }
}

fn draw_hidden_panel(frame: &mut Frame, area: Rect, app: &App, name: &str) {
    let panel = mk_panel(app, app.theme.div_line, app.theme.inactive_fg)
        .with_title(format!("{name} — hidden"))
        .with_controls("press B to toggle");
    frame.render_widget(&panel, area);
}

// ---------------------------------------------------------------------------
// Header status bar
// ---------------------------------------------------------------------------

fn draw_header(frame: &mut Frame, area: Rect, app: &App) {
    if area.width == 0 || area.height == 0 {
        return;
    }
    let buf = frame.buffer_mut();
    // Background fill in the title color so the strip reads as a separate
    // surface from the panels below.
    let bg = app.theme.meter_bg;
    let fg = app.theme.title;
    let dim = app.theme.inactive_fg;
    for x in 0..area.width {
        let cell = &mut buf[(area.x + x, area.y)];
        cell.set_char(' ');
        cell.set_style(Style::default().bg(bg).fg(fg));
    }

    let version = env!("CARGO_PKG_VERSION");
    let uptime = format_uptime(app.started_at.elapsed());
    // last_options_msg wins over last_kill_msg if both are set, since the
    // options-save toast is rarer and the user just took action.
    let toast = app.last_options_msg.as_ref().or(app.last_kill_msg.as_ref());
    let left = match toast {
        Some(msg) => format!(" {msg} "),
        None => format!(
            " bobtop v{version}  │  tier: {}  │  theme: {}",
            app.net_tier.name(),
            app.theme.name,
        ),
    };
    let right = format!("up {uptime} ");

    let left_fg = if toast.is_some() { app.theme.hi_fg } else { fg };
    write_str_at(buf, area.x, area.y, &left, Style::default().bg(bg).fg(left_fg));
    let right_len = right.chars().count() as u16;
    if right_len + 1 < area.width {
        write_str_at(
            buf,
            area.x + area.width - right_len,
            area.y,
            &right,
            Style::default().bg(bg).fg(dim),
        );
    }
}

fn format_uptime(d: std::time::Duration) -> String {
    let secs = d.as_secs();
    let h = secs / 3600;
    let m = (secs % 3600) / 60;
    let s = secs % 60;
    format!("{h:02}:{m:02}:{s:02}")
}

// ---------------------------------------------------------------------------
// Boxes overlay (B5) — show/hide individual panels live
// ---------------------------------------------------------------------------

fn draw_boxes_overlay(frame: &mut Frame, area: Rect, app: &App) {
    // Compact modal — one row per box plus 2 lines of header/footer.
    let want_w: u16 = 38;
    let want_h: u16 = (BoxKind::ALL.len() as u16) + 5;
    if area.width < want_w || area.height < want_h {
        return;
    }
    let x = area.x + (area.width - want_w) / 2;
    let y = area.y + (area.height.saturating_sub(want_h)) / 2;
    let modal = Rect::new(x, y, want_w, want_h);

    let panel = mk_panel(app, app.theme.title, app.theme.title)
        .with_title(" boxes — show/hide panels ".to_string())
        .with_controls("space toggle  ↑↓ move  B/Esc close");
    frame.render_widget(&panel, modal);
    let body = panel.inner(modal);
    let buf = frame.buffer_mut();
    let bg = app.theme.main_bg.unwrap_or(app.theme.meter_bg);
    for yy in body.y..body.y + body.height {
        for xx in body.x..body.x + body.width {
            let cell = &mut buf[(xx, yy)];
            cell.set_char(' ');
            cell.set_style(Style::default().bg(bg).fg(app.theme.main_fg));
        }
    }

    write_str_at(
        buf,
        body.x + 2,
        body.y + 1,
        "(panel changes are live — collectors pause too)",
        Style::default().bg(bg).fg(app.theme.inactive_fg),
    );

    for (i, b) in BoxKind::ALL.iter().enumerate() {
        let row_y = body.y + 3 + i as u16;
        if row_y + 1 >= body.y + body.height {
            break;
        }
        let enabled = app.boxes.is_enabled(*b);
        let mark = if enabled { "[x]" } else { "[ ]" };
        let label = box_label(*b);
        let is_cursor = i == app.boxes_overlay_cursor;
        let prefix = if is_cursor { "▶ " } else { "  " };
        let line = format!("{prefix}{mark}  {label}");
        let row_style = if is_cursor {
            Style::default().bg(app.theme.selected_bg).fg(app.theme.selected_fg)
        } else if enabled {
            Style::default().bg(bg).fg(app.theme.main_fg)
        } else {
            Style::default().bg(bg).fg(app.theme.inactive_fg)
        };
        // Selected row gets a full-width fill; non-selected rows use modal bg.
        if is_cursor {
            for xx in body.x + 1..body.x + body.width - 1 {
                buf[(xx, row_y)].set_style(Style::default().bg(app.theme.selected_bg));
            }
        }
        write_str_at(buf, body.x + 2, row_y, &line, row_style);
    }
}

fn box_label(b: BoxKind) -> &'static str {
    match b {
        BoxKind::Cpu => "CPU",
        BoxKind::Memory => "MEM",
        BoxKind::Disk => "DISKS",
        BoxKind::Network => "NET",
        BoxKind::Process => "PROC",
    }
}

// ---------------------------------------------------------------------------
// Options overlay (B11b)
// ---------------------------------------------------------------------------

fn draw_options_overlay(frame: &mut Frame, area: Rect, app: &App) {
    let Some(opts) = &app.options else { return };
    let want_w: u16 = 56;
    let want_h: u16 = (crate::app::OptionsState::FIELD_COUNT as u16) + 6;
    if area.width < want_w || area.height < want_h {
        return;
    }
    let x = area.x + (area.width - want_w) / 2;
    let y = area.y + (area.height - want_h) / 2;
    let modal = Rect::new(x, y, want_w, want_h);

    let panel = mk_panel(app, app.theme.title, app.theme.title)
        .with_title(" options ".to_string())
        .with_keybinds(" ↑↓ field   ←→ value   Enter save+apply   Esc cancel ");
    frame.render_widget(&panel, modal);
    let body = panel.inner(modal);
    let buf = frame.buffer_mut();
    let bg = app.theme.main_bg.unwrap_or(app.theme.meter_bg);
    for yy in body.y..body.y + body.height {
        for xx in body.x..body.x + body.width {
            let cell = &mut buf[(xx, yy)];
            cell.set_char(' ');
            cell.set_style(Style::default().bg(bg).fg(app.theme.main_fg));
        }
    }

    let rows: [(&str, String); crate::app::OptionsState::FIELD_COUNT] = [
        ("theme", opts.theme.clone()),
        ("tick_ms", format!("{} ms", opts.tick_ms)),
        ("layout", format!("{:?}", opts.layout).to_lowercase()),
        ("corners", format!("{:?}", opts.corners).to_lowercase()),
        ("no_ebpf", bool_label(opts.no_ebpf)),
        ("no_pcap", bool_label(opts.no_pcap)),
        ("tty (block graphs)", bool_label(opts.tty)),
        ("show_virtual_net", bool_label(opts.show_virtual_net)),
    ];
    let label_w = rows.iter().map(|(k, _)| k.chars().count()).max().unwrap_or(8) as u16;
    write_str_at(
        buf,
        body.x + 2,
        body.y + 1,
        "(saved to ~/.config/bobtop/bobtop.toml on Enter)",
        Style::default().bg(bg).fg(app.theme.inactive_fg),
    );
    for (i, (label, value)) in rows.iter().enumerate() {
        let row_y = body.y + 3 + i as u16;
        if row_y >= body.y + body.height {
            break;
        }
        let is_cursor = i == opts.cursor;
        if is_cursor {
            for xx in body.x + 1..body.x + body.width - 1 {
                buf[(xx, row_y)].set_style(Style::default().bg(app.theme.selected_bg));
            }
        }
        let prefix = if is_cursor { "▶ " } else { "  " };
        let row_bg = if is_cursor { app.theme.selected_bg } else { bg };
        let row_fg = if is_cursor { app.theme.selected_fg } else { app.theme.main_fg };
        let line = format!(
            "{prefix}{:<width$}  ◀ {value} ▶",
            label,
            width = label_w as usize,
        );
        write_str_at(buf, body.x + 2, row_y, &line, Style::default().bg(row_bg).fg(row_fg));
    }
}

fn bool_label(b: bool) -> String {
    if b { "yes".into() } else { "no".into() }
}

// ---------------------------------------------------------------------------
// Process detail modal (B3d)
// ---------------------------------------------------------------------------

fn draw_detail_modal(frame: &mut Frame, area: Rect, app: &App) {
    let Some(d) = &app.detail else { return };
    // Take ~70% width / ~70% height, centered.
    let want_w = (area.width * 7 / 10).max(50).min(area.width);
    let want_h = (area.height * 7 / 10).max(14).min(area.height);
    let x = area.x + (area.width - want_w) / 2;
    let y = area.y + (area.height - want_h) / 2;
    let modal = Rect::new(x, y, want_w, want_h);

    let panel = mk_panel(app, app.theme.proc_box, app.theme.title)
        .with_title(format!(" {} (pid {}) ", d.name, d.pid))
        .with_keybinds(" Esc / Enter close ");
    frame.render_widget(&panel, modal);
    let body = panel.inner(modal);
    let buf = frame.buffer_mut();
    let bg = app.theme.main_bg.unwrap_or(app.theme.meter_bg);
    for yy in body.y..body.y + body.height {
        for xx in body.x..body.x + body.width {
            let cell = &mut buf[(xx, yy)];
            cell.set_char(' ');
            cell.set_style(Style::default().bg(bg).fg(app.theme.main_fg));
        }
    }

    // Section header writer: highlight color, then a divider in dim.
    let mut row = body.y;
    let max_row = body.y + body.height;
    let write_section = |buf: &mut ratatui::buffer::Buffer, row: u16, name: &str| {
        write_str_at(
            buf,
            body.x + 1,
            row,
            &format!("── {name} "),
            Style::default().bg(bg).fg(app.theme.hi_fg),
        );
    };
    let write_line = |buf: &mut ratatui::buffer::Buffer, row: u16, s: &str| {
        let s = if s.chars().count() > body.width as usize - 4 {
            let mut t: String = s.chars().take(body.width as usize - 5).collect();
            t.push('…');
            t
        } else {
            s.to_string()
        };
        write_str_at(
            buf,
            body.x + 2,
            row,
            &s,
            Style::default().bg(bg).fg(app.theme.main_fg),
        );
    };

    write_section(buf, row, "cmdline");
    row += 1;
    if row < max_row {
        let cmd = if d.cmdline.is_empty() { "(no cmdline)" } else { &d.cmdline };
        write_line(buf, row, cmd);
        row += 2;
    }

    if row < max_row {
        write_section(buf, row, "status");
        row += 1;
        for line in &d.status_lines {
            if row >= max_row {
                break;
            }
            write_line(buf, row, line);
            row += 1;
        }
        row += 1;
    }

    if row < max_row {
        write_section(buf, row, "fd");
        row += 1;
        let fd_text = match &d.fd_count {
            Ok(n) => format!("open file descriptors: {n}"),
            Err(e) => format!("(unavailable: {e})"),
        };
        write_line(buf, row, &fd_text);
        row += 2;
    }

    if row < max_row {
        write_section(buf, row, "io");
        row += 1;
        for line in &d.io_lines {
            if row >= max_row {
                break;
            }
            write_line(buf, row, line);
            row += 1;
        }
    }
}

// ---------------------------------------------------------------------------
// Kill confirm dialog (B3c)
// ---------------------------------------------------------------------------

fn draw_kill_dialog(frame: &mut Frame, area: Rect, app: &App) {
    let Some(req) = &app.pending_kill else { return };
    let line1 = format!(" Send {} to pid {} ({})?", req.signal.label(), req.pid, req.name);
    let line2 = " [Enter / y]  confirm    [Esc / n]  cancel ";
    let want_w = (line1.chars().count().max(line2.chars().count()) + 4) as u16;
    let want_h: u16 = 6;
    if area.width < want_w || area.height < want_h {
        return;
    }
    let x = area.x + (area.width - want_w) / 2;
    let y = area.y + (area.height.saturating_sub(want_h)) / 2;
    let modal = Rect::new(x, y, want_w, want_h);

    // Reuse the proc panel's accent color for the title — danger reads as
    // a clearly differentiated overlay.
    let title = match req.signal {
        crate::app::KillSignal::Term => " confirm — SIGTERM ".to_string(),
        crate::app::KillSignal::Kill => " ⚠ confirm — SIGKILL ".to_string(),
    };
    let panel = mk_panel(app, app.theme.proc_box, app.theme.title).with_title(title);
    frame.render_widget(&panel, modal);
    let body = panel.inner(modal);
    let buf = frame.buffer_mut();
    let bg = app.theme.main_bg.unwrap_or(app.theme.meter_bg);
    for yy in body.y..body.y + body.height {
        for xx in body.x..body.x + body.width {
            let cell = &mut buf[(xx, yy)];
            cell.set_char(' ');
            cell.set_style(Style::default().bg(bg).fg(app.theme.main_fg));
        }
    }
    write_str_at(buf, body.x + 1, body.y + 1, &line1, Style::default().bg(bg).fg(app.theme.hi_fg));
    write_str_at(buf, body.x + 1, body.y + 3, line2, Style::default().bg(bg).fg(app.theme.inactive_fg));
}

// ---------------------------------------------------------------------------
// Help overlay (B2)
// ---------------------------------------------------------------------------

/// Authoritative keybind list — used both by the in-app `?` overlay and
/// the `--help-keys` CLI flag (see `main.rs::print_help_keys`). One source
/// of truth so the flag and the overlay can never drift apart.
pub const HELP_LINES: &[(&str, &str)] = &[
    ("?", "toggle this help"),
    ("q / Ctrl-C", "quit"),
    ("Esc", "close overlay (or quit when none open)"),
    ("↑ / ↓", "select process"),
    ("PgUp / PgDn / Home / End", "jump in process list"),
    ("← / →", "cycle sort column"),
    ("r", "reverse sort direction"),
    ("+ / -", "adjust global tick"),
    ("1", "preset 1 — full, sort by CPU"),
    ("2", "preset 2 — full, sort by MEM"),
    ("3", "preset 3 — full, sort by NET RX"),
    ("4", "preset 4 — minimal (CPU + processes only)"),
    ("p / n / m / c", "sort by Pid / Name / Mem / Cpu"),
    ("B", "boxes — show/hide individual panels"),
    ("f", "filter processes by name/cmdline"),
    ("k / K", "kill (SIGTERM / SIGKILL) — confirm dialog"),
    ("Enter", "process detail (read-only)"),
    ("O", "options — edit config + save to disk"),
];

fn draw_help_overlay(frame: &mut Frame, area: Rect, app: &App) {
    // Sized to fit the longest line + padding; centered in the frame.
    let inner_w: u16 = HELP_LINES
        .iter()
        .map(|(k, d)| (k.chars().count() + d.chars().count() + 4) as u16)
        .max()
        .unwrap_or(40);
    let want_w = inner_w + 4;
    let want_h = (HELP_LINES.len() as u16) + 4;
    if area.width < want_w || area.height < want_h {
        return;
    }
    let x = area.x + (area.width - want_w) / 2;
    let y = area.y + (area.height.saturating_sub(want_h)) / 2;
    let modal = Rect::new(x, y, want_w, want_h);

    let panel = mk_panel(app, app.theme.title, app.theme.title)
        .with_title(" help — keybinds ".to_string());
    frame.render_widget(&panel, modal);
    let body = panel.inner(modal);
    let buf = frame.buffer_mut();
    // Clear the body cells so panels behind don't bleed through. `main_bg`
    // is themed-optional (terminals with translucent backgrounds prefer
    // None); fall back to `meter_bg` so the modal still has a solid panel.
    let modal_bg = app.theme.main_bg.unwrap_or(app.theme.meter_bg);
    for yy in body.y..body.y + body.height {
        for xx in body.x..body.x + body.width {
            let cell = &mut buf[(xx, yy)];
            cell.set_char(' ');
            cell.set_style(Style::default().bg(modal_bg).fg(app.theme.main_fg));
        }
    }
    let key_w = HELP_LINES
        .iter()
        .map(|(k, _)| k.chars().count())
        .max()
        .unwrap_or(8) as u16;
    for (i, (key, desc)) in HELP_LINES.iter().enumerate() {
        let row_y = body.y + 1 + i as u16;
        if row_y >= body.y + body.height {
            break;
        }
        write_str_at(
            buf,
            body.x + 2,
            row_y,
            key,
            Style::default().bg(modal_bg).fg(app.theme.hi_fg),
        );
        write_str_at(
            buf,
            body.x + 2 + key_w + 2,
            row_y,
            desc,
            Style::default().bg(modal_bg).fg(app.theme.main_fg),
        );
    }
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
    let freq = app
        .latest_cpu
        .as_ref()
        .map(|s| avg_frequency_label(s))
        .unwrap_or_default();
    let load = app
        .latest_cpu
        .as_ref()
        .and_then(|s| s.load_average)
        .map(|l| format!("load {:.2} {:.2} {:.2}", l.one, l.five, l.fifteen))
        .unwrap_or_else(|| "load — — —".into());

    let title = format!("¹cpu  CPU {:.1}%{}  Cores={}  {}", cpu_pct, freq, cores, load);
    let panel = mk_panel(app, app.theme.cpu_box, app.theme.title)
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

    // CenteredBloom: trace blooms outward from a horizontal centerline
    // — value 0 = thin line at center, value 1 = full fill. Color sampled
    // by distance-from-center: gradient.start at the line, .end at edges.
    let max_pts = (graph_area.width as usize) * 2;
    let mut graph = BrailleGraph::new(max_pts, app.theme.cpu)
        .with_value_fn(|v| format!("{:>5.1}%", v * 100.0))
        .with_style(if app.tty_graphs {
            GraphStyle::Blocks
        } else {
            GraphStyle::CenteredBloom
        });
    for v in app.cpu_history.iter().copied() {
        graph.push(v);
    }
    frame.render_widget(&graph, graph_area);

    if let Some(s) = &app.latest_cpu {
        draw_core_meters(frame, meters_area, s, &app.theme);
    }
}

/// Average per-core frequency formatted for the CPU panel title.
/// Returns `"  3.2 GHz"` (leading 2-space separator) when at least one core
/// reports a frequency, or empty when no core reports it (some VMs / WSL
/// expose 0 for `frequency_mhz`). Empty avoids ugly `(0 MHz)` artifacts.
fn avg_frequency_label(s: &bobtop_core::sample::CpuSample) -> String {
    let mut sum_mhz: u64 = 0;
    let mut n: u64 = 0;
    for c in &s.cores {
        if let Some(f) = c.frequency_mhz {
            if f > 0 {
                sum_mhz += f as u64;
                n += 1;
            }
        }
    }
    if n == 0 {
        return String::new();
    }
    let avg = (sum_mhz / n) as f64;
    if avg >= 1000.0 {
        format!("  {:.1} GHz", avg / 1000.0)
    } else {
        format!("  {:.0} MHz", avg)
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
    let used_pct = app
        .latest_mem
        .as_ref()
        .filter(|m| m.total_bytes > 0)
        .map(|m| (m.used_bytes as f64 / m.total_bytes as f64) * 100.0)
        .unwrap_or(0.0);
    let panel = mk_panel(app, app.theme.mem_box, app.theme.title)
        .with_title(format!("²mem  {:.1}% used", used_pct));
    frame.render_widget(&panel, area);
    let inner = panel.inner(area);
    if inner.height < 3 {
        return;
    }

    // Top strip: small braille time-series of memory used %, ~3 rows tall.
    let graph_h = inner.height.min(4).saturating_sub(1).max(2);
    let graph_area = Rect::new(inner.x, inner.y, inner.width, graph_h);
    let meters_y = inner.y + graph_h;
    let meters_area = Rect::new(
        inner.x,
        meters_y,
        inner.width,
        inner.y + inner.height - meters_y,
    );

    // CenteredBloom: trace blooms outward from horizontal centerline.
    let mem_max_pts = graph_area.width as usize * 2;
    let mut mem_graph = BrailleGraph::new(mem_max_pts, app.theme.used)
        .with_value_fn(|v| format!("{:>5.1}%", v * 100.0))
        .with_style(if app.tty_graphs {
            GraphStyle::Blocks
        } else {
            GraphStyle::CenteredBloom
        });
    for v in app.mem_history.iter().copied() {
        mem_graph.push(v);
    }
    frame.render_widget(&mem_graph, graph_area);

    let Some(s) = &app.latest_mem else { return };
    let categories = build_memory_categories(s, &app.theme);
    if categories.is_empty() || meters_area.height < 3 {
        return;
    }
    let row_h = (meters_area.height / categories.len() as u16).max(3);
    for (i, m) in categories.iter().enumerate() {
        let y = meters_area.y + (i as u16) * row_h;
        if y + 3 > meters_area.y + meters_area.height {
            break;
        }
        let r = Rect::new(meters_area.x, y, meters_area.width, row_h);
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
    let panel = mk_panel(app, app.theme.mem_box, app.theme.title).with_title(title);
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
    // Bake live read/write rates into the right-aligned value text so the
    // disk panel actually shows IO at a glance (matches btop's "▼6526" style).
    let io_part = match (fs.read_bytes_per_sec, fs.write_bytes_per_sec) {
        (Some(r), Some(w)) if r + w > 0.0 => {
            format!("▼{}/s ▲{}/s  ", format_rate(r), format_rate(w))
        }
        _ => String::new(),
    };
    let value = format!(
        "{}{} / {}",
        io_part,
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
    let scale = app.net_scale_bps();
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
    let panel = mk_panel(app, app.theme.net_box, app.theme.title)
        .with_title(format!("{title}{bandwidth_note}"))
        .with_controls(format!(
            "↑peak {}/s  ↓peak {}/s",
            format_rate(app.net_peak_tx()),
            format_rate(app.net_peak_rx()),
        ));
    frame.render_widget(&panel, area);
    let inner = panel.inner(area);
    if inner.width < 12 || inner.height < 4 {
        return;
    }

    // CenteredBloom split: upload (secondary) blooms UP from the center
    // line; download (primary) blooms DOWN from the center line. Idle
    // network = thin band at center; busy = traces reach toward edges.
    // Y-axis labels show the actual scale ceiling so users can read
    // "what does the top edge mean" without parsing the title — matches
    // btop's auto-scale label placement.
    let max_pts = (inner.width as usize) * 2;
    let scale_label = format!("{}/s", format_rate(scale));
    let mut graph = BrailleGraph::new(max_pts, app.theme.download)
        .with_secondary(Trace::new(max_pts, app.theme.upload), DualMode::MirroredSplit)
        .with_style(if app.tty_graphs {
            GraphStyle::Blocks
        } else {
            GraphStyle::CenteredBloom
        })
        .with_y_scale(scale_label.clone(), scale_label);
    for (rx, tx) in app.net_normalized_history() {
        graph.push_dual(rx, tx);
    }
    frame.render_widget(&graph, inner);

    // Center divider with embedded current-rate labels (peak labels stay in
    // the panel title's right-side controls slot — no graph cells obscured).
    overlay_center_divider(frame, inner, app, rx_now, tx_now);
}

/// Draw the center divider AND embed the live rate labels in it. Putting
/// labels here (rather than at the top/bottom corners) keeps the actual
/// graph rows free of text overlay — graph data was being clobbered before.
fn overlay_center_divider(frame: &mut Frame, inner: Rect, app: &App, rx_now: f64, tx_now: f64) {
    let div_y = inner.y + inner.height / 2;
    if div_y >= inner.y + inner.height {
        return;
    }
    use ratatui::style::Style;
    let style = Style::default().fg(app.theme.div_line);
    let buf = frame.buffer_mut();
    for x in 0..inner.width {
        let cell = &mut buf[(inner.x + x, div_y)];
        cell.set_char('─');
        cell.set_style(style);
    }
    // Embedded labels: "↑ N/s" on the left (upload, top half), "N/s ↓" on
    // the right (download, bottom half) — direction matches which side
    // of the divider that flow occupies.
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

fn write_str_at(
    buf: &mut ratatui::buffer::Buffer,
    x: u16,
    y: u16,
    s: &str,
    style: ratatui::style::Style,
) {
    let mut col = x;
    let right = buf.area.right();
    for ch in s.chars() {
        if col >= right {
            break;
        }
        let cell = &mut buf[(col, y)];
        cell.set_char(ch);
        cell.set_style(style);
        col = col.saturating_add(1);
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
    let has_bw = app.net_tier.has_bandwidth();
    let filter_tag = if !app.filter_text.is_empty() {
        format!("  filter:\"{}\"", app.filter_text)
    } else {
        String::new()
    };
    let title = format!(
        "⁴proc  {} procs  ←{}{}→  rx/tx: {}{}{}",
        app.processes_sorted.len(),
        app.proc_sort.label(),
        arrow,
        app.net_tier.name(),
        if has_bw { "" } else { " (build w/ --features ebpf or pcap)" },
        filter_tag,
    );
    let panel = mk_panel(app, app.theme.proc_box, app.theme.title)
        .with_title(title)
        .with_keybinds(
            "q quit  ↑↓ select  ←→ sort col  r reverse  f filter  k/K kill  Enter detail  ?",
        );
    frame.render_widget(&panel, area);
    let inner = panel.inner(area);
    if inner.width < 20 || inner.height < 2 {
        return;
    }

    // Reserve a footer row for the filter input bar when active. Otherwise
    // the table claims the full inner area.
    let (table_area, filter_bar) = if app.filter_active {
        let body_h = inner.height.saturating_sub(1);
        (
            Rect::new(inner.x, inner.y, inner.width, body_h),
            Some(Rect::new(inner.x, inner.y + body_h, inner.width, 1)),
        )
    } else {
        (inner, None)
    };

    // App.processes_sorted already has the net join + sort applied at apply
    // time. Render uses it directly — no per-frame re-cloning.
    let body_h = table_area.height.saturating_sub(1) as usize;
    let mut scroll_offset = app.scroll_offset;
    if app.selected_proc >= scroll_offset + body_h && body_h > 0 {
        scroll_offset = app.selected_proc + 1 - body_h;
    }

    let mut table = ProcessTable::new(&app.processes_sorted, &app.theme)
        .with_selection(Some(app.selected_proc), scroll_offset)
        .with_net_columns(app.net_tier.has_bandwidth())
        .with_direction(app.proc_sort_descending);
    table.sort = app.proc_sort;
    frame.render_widget(&table, table_area);

    if let Some(bar) = filter_bar {
        let buf = frame.buffer_mut();
        let bg = app.theme.meter_bg;
        for x in 0..bar.width {
            let cell = &mut buf[(bar.x + x, bar.y)];
            cell.set_char(' ');
            cell.set_style(Style::default().bg(bg).fg(app.theme.title));
        }
        let label = format!(" filter: {}█  ", app.filter_text);
        write_str_at(buf, bar.x, bar.y, &label, Style::default().bg(bg).fg(app.theme.hi_fg));
        let hint = " Enter=apply  Esc=clear ";
        let len = hint.chars().count() as u16;
        if len + 2 < bar.width {
            write_str_at(
                buf,
                bar.x + bar.width.saturating_sub(len + 1),
                bar.y,
                hint,
                Style::default().bg(bg).fg(app.theme.inactive_fg),
            );
        }
    }
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
