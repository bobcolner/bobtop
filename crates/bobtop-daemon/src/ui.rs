//! Pure frame composition. `draw` is the single render function called by
//! the TUI loop on every frame.

use bobtop_core::sample::{CpuSample, FilesystemSample, MemorySample};
use bobtop_core::Box as BoxKind;
use bobtop_tui::widgets::{
    BoxedPanel, BrailleGraph, DualMode, GraphStyle, LegendStyle, Meter, MiniMeter, ProcessTable,
    Sparkline, StackedBar, StackedSegment, TableLayout, Trace,
};
use bobtop_tui::compute_layout;
use ratatui::layout::Rect;
use ratatui::style::Style;
use ratatui::widgets::Widget;
use ratatui::Frame;

use crate::app::App;

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
    // Paint the theme's `main_bg` across the whole frame so panels render
    // ON the theme background instead of the terminal default. When
    // `theme_background = false` (Options overlay), `main_bg` is None and we
    // skip the fill, letting terminal transparency / wallpaper show through.
    // Matches btop: same theme key, same opt-out toggle, same effect.
    if let Some(bg) = app.theme.main_bg {
        let buf = frame.buffer_mut();
        let fill = Style::default().bg(bg);
        for y in area.y..area.bottom() {
            for x in area.x..area.right() {
                let cell = &mut buf[(x, y)];
                cell.set_char(' ');
                cell.set_style(fill);
            }
        }
    }
    let layout = compute_layout(area, app.layout_preset);

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
        .flat()
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
        .flat()
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
        ("theme_background", bool_label(opts.theme_background)),
        ("truecolor", bool_label(opts.truecolor)),
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
        .flat()
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
    let panel = mk_panel(app, app.theme.proc_box, app.theme.title)
        .flat()
        .with_title(title);
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
    ("Enter", "detail (process) | expand (header)"),
    ("g", "cycle group mode: flat → exec → cgroup → tree"),
    ("[ / ]", "cycle network interface in net panel (back / next)"),
    ("Space", "expand/collapse selected group or subtree"),
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
        .flat()
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
    let load = app
        .latest_cpu
        .as_ref()
        .and_then(|s| s.load_average)
        .map(|l| format!("load {:.2} {:.2} {:.2}", l.one, l.five, l.fifteen))
        .unwrap_or_else(|| "load — — —".into());

    // Outer CPU panel title omits freq & model — those moved to the cores
    // subpanel title, where they sit next to the per-core grid that
    // visualizes them. Keeping them on both would just be duplication.
    let title = format!("¹cpu  CPU {:.1}%  Cores={}  {}", cpu_pct, cores, load);
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

/// Nested bordered subpanel inside the CPU panel that holds the per-core
/// list. Title shows the CPU model + current avg frequency (matches btop's
/// "EPYC 9665 X · 3.2 GHz" tag). Reserves the top inner row for an overall
/// CPU% bar so users get a global view alongside the per-core grid.
fn draw_cores_subpanel(
    frame: &mut Frame,
    area: Rect,
    sample: &CpuSample,
    app: &App,
    cpu_pct: f64,
) {
    if area.width < 12 || area.height < 4 {
        // Too small for the nested chrome — fall back to the flat
        // per-core grid like before.
        draw_core_meters(frame, area, sample, app);
        return;
    }
    // Title: "{model}{  freq}". Both halves are optional — empty when /proc
    // didn't surface the value. The 2-space separator splits them into the
    // BoxedPanel's segmented pill rendering for free.
    let freq = avg_frequency_label(sample);
    let model = app.cpu_model.as_deref().unwrap_or("cpu");
    let title = if freq.is_empty() {
        model.to_string()
    } else {
        // `freq` already has a leading "  " separator from
        // `avg_frequency_label`; the panel will split on the double-space.
        format!("{model}{freq}")
    };
    // Use a flat title (no bubble) — this is a sub-panel inside the CPU
    // box and the bubble layout would steal too much vertical space.
    let panel = mk_panel(app, app.theme.div_line, app.theme.title)
        .flat()
        .with_title(title);
    frame.render_widget(&panel, area);
    let inner = panel.inner(area);
    if inner.height == 0 || inner.width == 0 {
        return;
    }

    // Top row: overall CPU% as a single-segment StackedBar — full-width
    // simple horizontal bar. Reuses the chart-library piece so the look
    // matches the memory breakdown bar instead of the per-core MiniMeters.
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
    let overall_rect = Rect::new(inner.x, inner.y, inner.width, 1);
    (&overall).render(overall_rect, frame.buffer_mut());

    // Remaining rows: per-core grid (sparkline or bar based on toggle).
    let cores_rect = Rect::new(inner.x, inner.y + 1, inner.width, inner.height - 1);
    if cores_rect.height > 0 {
        draw_core_meters(frame, cores_rect, sample, app);
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

fn draw_core_meters(frame: &mut Frame, area: Rect, sample: &CpuSample, app: &App) {
    if area.height == 0 || area.width < 8 {
        return;
    }
    let theme = &app.theme;
    let n = sample.cores.len();
    if n == 0 {
        return;
    }
    let rows = area.height as usize;
    // Wrap into N columns when the core count exceeds available rows. btop's
    // signature look on high-core hosts: C0..C15 left, C16..C31 right.
    const MIN_COL_W: u16 = 16;
    let max_cols_by_width = ((area.width / MIN_COL_W).max(1)) as usize;
    let cols_by_count = n.div_ceil(rows.max(1));
    let cols = cols_by_count.min(max_cols_by_width).max(1);
    let col_w = area.width / cols as u16;
    let per_col_rows = n.div_ceil(cols);

    for (i, core) in sample.cores.iter().enumerate() {
        let col = i / per_col_rows;
        let row = i % per_col_rows;
        if col >= cols || row >= rows {
            break;
        }
        let x = area.x + col as u16 * col_w;
        let w = if col + 1 == cols {
            area.width.saturating_sub(col as u16 * col_w)
        } else {
            col_w
        };
        let row_y = area.y + row as u16;

        // Pick the chart for this row from the chart library. Only one
        // chart per row — `Sparkline` (default) or `Bar` (MiniMeter gauge).
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

/// Render one inline-chart row: `[label] [sparkline] [value]`. Used by both
/// per-core CPU rows and per-mount disk rows so the layout chrome stays
/// consistent. `history` is the time-series ring; `None` renders an empty
/// chart slot. Auto-normalizes history against its peak so low-but-varying
/// signals (idle CPU, slow disks) still show movement.
fn draw_track_sparkline_row(
    buf: &mut ratatui::buffer::Buffer,
    area: Rect,
    label: &str,
    value: &str,
    history: Option<&std::collections::VecDeque<f64>>,
    gradient: bobtop_tui::Gradient,
    text_color: ratatui::style::Color,
    tty: bool,
) {
    if area.width == 0 || area.height == 0 {
        return;
    }
    let label_w = (label.chars().count() as u16).min(area.width);
    let value_w = (value.chars().count() as u16).min(area.width.saturating_sub(label_w));
    // 1-cell gutter on each side of the spark when there's room.
    let gutter = if area.width > label_w + value_w + 4 { 1 } else { 0 };
    let spark_x = area.x + label_w + gutter;
    let value_x = area.x + area.width.saturating_sub(value_w);
    let spark_end = value_x.saturating_sub(gutter);
    let spark_w = spark_end.saturating_sub(spark_x);
    let y = area.y;

    write_str_at(buf, area.x, y, label, Style::default().fg(text_color));
    write_str_at(buf, value_x, y, value, Style::default().fg(text_color));

    if spark_w == 0 {
        return;
    }
    let raw: Vec<f64> = history.map(|h| h.iter().copied().collect()).unwrap_or_default();
    if raw.is_empty() {
        return;
    }
    // Auto-scale to the row's own peak so 1% CPU history doesn't render flat.
    // Floor of 5% (0.05) prevents idle noise from getting amplified to fill
    // the row — a series capped under 5% reads as truly quiet.
    let peak = raw.iter().copied().fold(0.05_f64, f64::max);
    let values: Vec<f64> = raw.iter().map(|v| (v / peak).clamp(0.0, 1.0)).collect();
    let spark = Sparkline::new(&values, gradient).with_style(if tty {
        GraphStyle::Blocks
    } else {
        GraphStyle::Braille
    });
    (&spark).render(Rect::new(spark_x, y, spark_w, 1), buf);
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
    // Title carries the size-summary so the breakdown bar can keep its
    // full inner width for the colored segments instead of giving 16 cells
    // to a `64.0 GiB total` value-text.
    let title = match app.latest_mem.as_ref() {
        Some(m) if m.total_bytes > 0 => format!(
            "²mem  {} / {}  {:.1}%",
            format_bytes(m.used_bytes),
            format_bytes(m.total_bytes),
            used_pct
        ),
        _ => "²mem".to_string(),
    };
    let panel = mk_panel(app, app.theme.mem_box, app.theme.title).with_title(title);
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
    draw_memory_breakdown(frame, meters_area, s, app);
}

/// Renders the lower half of the memory panel: a custom `StackedBar`
/// breakdown of Used/Cached/Buffers/Free, then a PSI sparkline (when
/// available), then a Swap meter (when swap is configured).
fn draw_memory_breakdown(frame: &mut Frame, area: Rect, s: &MemorySample, app: &App) {
    let theme = &app.theme;
    let total = s.total_bytes.max(1) as f64;
    // Real "used" = total - available. /proc/meminfo's "MemAvailable" already
    // accounts for reclaimable cache + slab, so this is the truest "what apps
    // are actually consuming" number.
    let used_real = s.used_bytes as f64;
    let cached_b = s.cached_bytes as f64;
    let buffers_b = s.buffers_bytes as f64;
    let free_b = s.free_bytes as f64;
    // Anything not in the four named categories (kernel slab, etc.). Folded
    // into "used" so the bar always sums to 100% — the breakdown is meant
    // to be approximate, not a forensic accounting tool.
    let unaccounted = (total - used_real - cached_b - buffers_b - free_b).max(0.0);
    let used_total = used_real + unaccounted;
    // Buffers is folded into Cached. Buffers on a typical host is <1% of
    // RAM — a separate slice is invisible AND its color (formerly
    // `theme.cached.start`, a near-bg dark) muddied the contrast. Three
    // distinct theme palettes give clearly differentiated hues: warm
    // (used) / cool accent (cached) / green (free).
    let cached_combined = (cached_b + buffers_b) as u64;
    let segments = vec![
        StackedSegment::new("Used", used_total / total, theme.used.end)
            .with_value(format_bytes(used_total as u64)),
        StackedSegment::new("Cached", (cached_b + buffers_b) / total, theme.cached.end)
            .with_value(format_bytes(cached_combined)),
        StackedSegment::new("Free", free_b / total, theme.free.end)
            .with_value(format_bytes(s.free_bytes)),
    ];
    // No inline label / value-text → the bar uses the panel's full inner
    // width. Size-summary moved to the panel title; segment values sit in
    // the aligned legend row directly under their colored slices.
    let bar = StackedBar::new(&segments)
        .with_empty_bg(theme.meter_bg)
        .with_chrome_fg(theme.main_fg)
        .with_legend(area.height >= 3)
        // Aligned legend places each `■ name value` underneath its own
        // colored segment, so the breakdown reads as a real bar chart with
        // axis labels rather than a separate left-to-right legend list.
        .with_legend_style(LegendStyle::Aligned);
    // Bar takes 1 row, legend takes 1 if shown.
    let bar_h: u16 = if area.height >= 3 { 2 } else { 1 };
    let bar_rect = Rect::new(area.x, area.y, area.width, bar_h);
    (&bar).render(bar_rect, frame.buffer_mut());

    let mut next_y = area.y + bar_h;
    let bottom = area.y + area.height;

    // PSI horizontal bar — full-width single-segment StackedBar of
    // `some_avg10`. Drops the inline label / value-text so the bar takes
    // 100% of inner.width; numbers go in a separate dim-text row below.
    // `theme.temp.end` reads as "warning red" against the dark `meter_bg`
    // — same palette btop uses for hot CPU temps.
    if next_y + 1 < bottom && s.pressure.is_some() {
        let p = s.pressure.unwrap_or_default();
        let bar_segs = vec![StackedSegment::new(
            "PSI",
            (p.some_avg10 as f64 / 100.0).clamp(0.0, 1.0),
            theme.temp.end,
        )];
        let pbar = StackedBar::new(&bar_segs)
            .with_empty_bg(theme.meter_bg)
            .with_chrome_fg(theme.main_fg);
        let pbar_rect = Rect::new(area.x, next_y, area.width, 1);
        (&pbar).render(pbar_rect, frame.buffer_mut());
        next_y += 1;

        // Numeric row — labelled, dim, single line. Window labels are short
        // (`10s` / `60s` / `300s`) so all three windows + label fit even on
        // the 40%-width memory panel.
        let nums = format!(
            "PSI  10s {:.1}%  60s {:.1}%  300s {:.1}%",
            p.some_avg10, p.some_avg60, p.some_avg300
        );
        write_str_at(
            frame.buffer_mut(),
            area.x,
            next_y,
            &nums,
            Style::default().fg(theme.inactive_fg),
        );
        next_y += 1;
    }

    // Swap row — only when swap exists. Keep as an existing Meter so users
    // get the bar + bytes treatment matching the old layout.
    if next_y + 2 < bottom && s.swap_total_bytes > 0 {
        let frac = s.swap_used_bytes as f64 / s.swap_total_bytes as f64;
        let m = Meter::new(
            "Swap:",
            format!(
                "{} / {}",
                format_bytes(s.swap_used_bytes),
                format_bytes(s.swap_total_bytes)
            ),
            frac,
        )
        .with_gradient(theme.used)
        .with_meter_bg(theme.meter_bg)
        .with_text_colors(theme.main_fg, theme.title);
        let rest_h = bottom.saturating_sub(next_y);
        m.render(Rect::new(area.x, next_y, area.width, rest_h), frame.buffer_mut());
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

    // Disk panel respects the same chart toggle as per-core: Bar mode
    // renders the existing 3-row Meter gauge, Sparkline mode collapses
    // each disk into a 1-row label + spark + value layout. Only one chart
    // shows at a time; the unused widget never executes.
    match app.track_chart_style {
        crate::app::TrackChartStyle::Bar => {
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
                m.render(Rect::new(inner.x, y, inner.width, row_h), frame.buffer_mut());
            }
        }
        crate::app::TrackChartStyle::Sparkline => {
            // Stacked composition per mount:
            //   row 0: I/O sparkline (label + spark + ▼read ▲write rates)
            //   rows 1..: used/total Meter gauge (kept from Bar mode)
            // Each mount gets ceil(inner.height / n) rows, min 3 — 1 for
            // spark + 2 for Meter (Meter needs 2 lines: bar + value text).
            let n = disk.filesystems.len().max(1);
            let row_h = (inner.height / n as u16).max(3);
            // Use a no-IO variant of the meter — the I/O rates are already
            // displayed in the sparkline row above this gauge, so showing
            // them again here would just be visual duplication.
            let meters: Vec<Meter> = disk
                .filesystems
                .iter()
                .map(|fs| build_disk_meter_used_only(fs, &app.theme))
                .collect();
            for (i, fs) in disk.filesystems.iter().enumerate() {
                let y = inner.y + (i as u16) * row_h;
                if y + 3 > inner.y + inner.height {
                    break;
                }
                // Top row: I/O sparkline. Combined read+write rate, per-
                // mount auto-scale via `draw_track_sparkline_row`.
                let combined: std::collections::VecDeque<f64> = app
                    .disk_history
                    .get(&fs.label)
                    .map(|h| h.iter().map(|(r, w)| r + w).collect())
                    .unwrap_or_default();
                let label = format!("{:<6}", truncate_label(&fs.label, 6));
                let io_value = match (fs.read_bytes_per_sec, fs.write_bytes_per_sec) {
                    (Some(r), Some(w)) if r + w > 0.0 => {
                        format!("▼{}/s ▲{}/s", format_rate(r), format_rate(w))
                    }
                    _ => "idle".to_string(),
                };
                draw_track_sparkline_row(
                    frame.buffer_mut(),
                    Rect::new(inner.x, y, inner.width, 1),
                    &label,
                    &io_value,
                    Some(&combined),
                    app.theme.used,
                    app.theme.main_fg,
                    app.tty_graphs,
                );
                // Remaining rows: existing Meter gauge underneath.
                let gauge_h = row_h.saturating_sub(1);
                if gauge_h >= 2 {
                    let gauge_rect = Rect::new(inner.x, y + 1, inner.width, gauge_h);
                    meters[i].render(gauge_rect, frame.buffer_mut());
                }
            }
        }
    }
}

/// Truncate a mount label to fit a fixed-width column. Long mounts get the
/// trailing component preserved (the leaf, e.g. `home` from `/var/lib/home`)
/// since that's typically the disambiguating part.
fn truncate_label(label: &str, max: usize) -> String {
    if label.chars().count() <= max {
        return label.to_string();
    }
    let chars: Vec<char> = label.chars().collect();
    let start = chars.len() - max;
    chars[start..].iter().collect()
}

/// Used-only variant of `build_disk_meter` — same `used / total` gauge but
/// strips the inline I/O rates that the sparkline row already shows.
fn build_disk_meter_used_only(fs: &FilesystemSample, theme: &bobtop_tui::Theme) -> Meter {
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
    let label = format!("{}:", fs.label);
    Meter::new(label, value, frac)
        .with_gradient(theme.used)
        .with_meter_bg(theme.meter_bg)
        .with_text_colors(theme.main_fg, theme.title)
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

// ---------------------------------------------------------------------------
// Network
// ---------------------------------------------------------------------------

fn draw_network(frame: &mut Frame, area: Rect, app: &App) {
    // When `selected_iface` is set, scope rates + history + scale to that
    // single interface. None = aggregate (sum of all visible interfaces).
    let (rx_now, tx_now) = scoped_current_rates(app);
    let (counted, total) = interface_counts(app);
    let scale = app.net_scale_bps();
    // Title is built from short segments — each pair of double-spaces is a
    // separate pill in the BoxedPanel renderer. Long iface names (docker
    // bridges, k8s veths) get truncated for chrome sanity; full name still
    // available via the cycle.
    let title = if total == 0 {
        "³net  no interfaces".to_string()
    } else {
        let scope = scope_segment(app);
        let mut t = format!("³net  {scope}  {}", app.net_tier.name());
        // Show "X/Y ifaces" only when virtual filtering hides some — quiet
        // hosts with 1 interface don't need to know they have 1/1.
        if !app.show_virtual_net && counted < total {
            t.push_str(&format!("  {counted}/{total}"));
        }
        t
    };
    let panel = mk_panel(app, app.theme.net_box, app.theme.title)
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

/// Title segment for the active iface scope. The bracket-pill chrome of
/// the panel title already wraps each segment, so we just emit the bare
/// name (or "all"). Long names (docker bridges like `br-2880bd517e17…`)
/// truncate to 14 chars + ellipsis so they don't blow out the pill.
fn scope_segment(app: &App) -> String {
    match &app.selected_iface {
        None => "all".to_string(),
        Some(name) => {
            let chars: Vec<char> = name.chars().collect();
            if chars.len() <= 14 {
                name.clone()
            } else {
                let mut s: String = chars[..13].iter().collect();
                s.push('…');
                s
            }
        }
    }
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

/// History pairs already normalized to 0..=1 against `scale`. When scoped
/// to one interface, draws from that iface's ring; otherwise falls back to
/// the aggregate `net_history` via `App::net_normalized_history()`.
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
        "⁴proc  {} procs  group:{}  ←{}{}→  rx/tx: {}{}{}",
        app.processes_sorted.len(),
        app.group_mode.label(),
        app.proc_sort.label(),
        arrow,
        app.net_tier.name(),
        if has_bw { "" } else { " (build w/ --features ebpf or pcap)" },
        filter_tag,
    );
    let panel = mk_panel(app, app.theme.proc_box, app.theme.title)
        .with_title(title)
        .with_keybinds(
            "q quit  ↑↓ select  ←→ sort  r rev  f filter  g group  Space expand  k/K kill  Enter  ?",
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

    // Build the grouped/tree display rows from App state. This is the
    // input the widget actually iterates over — `processes_sorted` is
    // still the underlying source of truth, but the widget never sees
    // it directly anymore.
    let rows = app.display_rows();
    // Pick a column-width preset per group mode:
    //   Flat → Command flexes (full argv visibility)
    //   ByExecutable / ByCgroup → Program flexes (long group keys),
    //                              Command shrinks but stays visible
    //                              for expanded children
    //   ByParent (tree) → Program wider (tree glyphs + indent),
    //                     Command flexes for argv after the tree prefix
    let layout = match app.group_mode {
        crate::group::GroupMode::Flat => TableLayout::Flat,
        crate::group::GroupMode::ByExecutable | crate::group::GroupMode::ByCgroup => {
            TableLayout::Grouped
        }
        crate::group::GroupMode::ByParent => TableLayout::Tree,
    };
    let mut table = ProcessTable::new(&rows, &app.theme)
        .with_selection(Some(app.selected_proc), scroll_offset)
        .with_net_columns(app.net_tier.has_bandwidth())
        .with_direction(app.proc_sort_descending)
        .with_layout(layout);
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

    fn fake_sample(n: u32) -> bobtop_core::sample::CpuSample {
        let cores = (0..n)
            .map(|i| bobtop_core::sample::CoreSample {
                id: i,
                utilization: 0.5,
                frequency_mhz: None,
                temperature_c: None,
            })
            .collect();
        bobtop_core::sample::CpuSample {
            timestamp: std::time::Instant::now(),
            aggregate_utilization: 0.5,
            cores,
            load_average: None,
        }
    }

    /// Wide panel + many cores → meters wrap into multiple columns and every
    /// core has a row to land on, instead of being silently truncated when
    /// `cores > area.height` (the pre-wrap behavior).
    #[test]
    fn core_meters_wrap_into_columns_on_wide_panels() {
        use ratatui::buffer::Buffer;
        use ratatui::layout::Rect;
        use std::sync::{atomic::AtomicU64, Arc};
        let s = fake_sample(32);
        let app = super::super::app::App::new(
            bobtop_tui::Theme::default(),
            bobtop_tui::LayoutPreset::Full,
            Arc::new(AtomicU64::new(500)),
            false,
            false,
        );
        let area = Rect::new(0, 0, 64, 16);
        let mut buf = Buffer::empty(area);
        let backend = ratatui::backend::TestBackend::new(area.width, area.height);
        let mut term = ratatui::Terminal::new(backend).unwrap();
        term.draw(|f| {
            super::draw_core_meters(f, area, &s, &app);
            buf = f.buffer_mut().clone();
        })
        .unwrap();
        // C0 anchored at left column; C16 anchored well to the right.
        let row0: String = (0..6).map(|x| buf[(x, 0)].symbol().to_string()).collect();
        assert!(row0.contains("C 0") || row0.contains("C0"), "left col missing C0: {row0:?}");
        // Find C16 somewhere in row 0 past the midpoint — proves a second
        // column exists.
        let mut found_c16_right = false;
        for x in (area.width / 2)..area.width.saturating_sub(2) {
            let s: String = (x..x + 3).map(|c| buf[(c, 0)].symbol().to_string()).collect();
            if s == "C16" {
                found_c16_right = true;
                break;
            }
        }
        assert!(found_c16_right, "expected C16 to appear in the right column on row 0");
    }

    /// Narrow panel (< MIN_COL_W) falls back to single column.
    #[test]
    fn core_meters_single_column_when_narrow() {
        use ratatui::buffer::Buffer;
        use ratatui::layout::Rect;
        use std::sync::{atomic::AtomicU64, Arc};
        let s = fake_sample(8);
        let app = super::super::app::App::new(
            bobtop_tui::Theme::default(),
            bobtop_tui::LayoutPreset::Full,
            Arc::new(AtomicU64::new(500)),
            false,
            false,
        );
        let area = Rect::new(0, 0, 18, 12);
        let mut buf = Buffer::empty(area);
        let backend = ratatui::backend::TestBackend::new(area.width, area.height);
        let mut term = ratatui::Terminal::new(backend).unwrap();
        term.draw(|f| {
            super::draw_core_meters(f, area, &s, &app);
            buf = f.buffer_mut().clone();
        })
        .unwrap();
        // C7 should be on row 7 (single col); not on row 0.
        let row7: String = (0..3).map(|x| buf[(x, 7)].symbol().to_string()).collect();
        assert_eq!(row7, "C 7");
    }
}
