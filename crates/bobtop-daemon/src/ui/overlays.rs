use bobtop_core::Box as BoxKind;
use bobtop_tui::widgets::panel as boxed_panel;
use bobtop_tui::{bool_label, truncate_chars, write_str_at};
use ratatui::layout::Rect;
use ratatui::style::Style;
use ratatui::Frame;

use crate::app::App;

use super::presenter;

pub(super) fn draw_hidden_panel(frame: &mut Frame, area: Rect, app: &App, name: &str) {
    let panel = boxed_panel(app.theme.div_line, app.theme.inactive_fg, app.corner_style)
        .with_title(presenter::hidden_panel_title(name))
        .with_controls("press B to toggle");
    frame.render_widget(&panel, area);
}

pub(super) fn draw_boxes_overlay(frame: &mut Frame, area: Rect, app: &App) {
    let want_w: u16 = 38;
    let want_h: u16 = (BoxKind::ALL.len() as u16) + 5;
    if area.width < want_w || area.height < want_h {
        return;
    }
    let x = area.x + (area.width - want_w) / 2;
    let y = area.y + (area.height.saturating_sub(want_h)) / 2;
    let modal = Rect::new(x, y, want_w, want_h);

    let panel = boxed_panel(app.theme.title, app.theme.title, app.corner_style)
        .flat()
        .with_title(presenter::boxes_overlay_title())
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

pub(super) fn draw_options_overlay(frame: &mut Frame, area: Rect, app: &App) {
    let Some(opts) = &app.options else { return };
    let want_w: u16 = 56;
    let want_h: u16 = (crate::app::OptionsState::FIELD_COUNT as u16) + 6;
    if area.width < want_w || area.height < want_h {
        return;
    }
    let x = area.x + (area.width - want_w) / 2;
    let y = area.y + (area.height - want_h) / 2;
    let modal = Rect::new(x, y, want_w, want_h);

    let panel = boxed_panel(app.theme.title, app.theme.title, app.corner_style)
        .flat()
        .with_title(presenter::options_overlay_title())
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

pub(super) fn draw_detail_modal(frame: &mut Frame, area: Rect, app: &App) {
    let Some(d) = &app.detail else { return };
    let want_w = (area.width * 7 / 10).max(50).min(area.width);
    let want_h = (area.height * 7 / 10).max(14).min(area.height);
    let x = area.x + (area.width - want_w) / 2;
    let y = area.y + (area.height - want_h) / 2;
    let modal = Rect::new(x, y, want_w, want_h);

    let panel = boxed_panel(app.theme.proc_box, app.theme.title, app.corner_style)
        .flat()
        .with_title(presenter::detail_title(d))
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
            format!("{}…", truncate_chars(s, body.width as usize - 5))
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

pub(super) fn draw_kill_dialog(frame: &mut Frame, area: Rect, app: &App) {
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

    let title = presenter::kill_title(req);
    let panel = boxed_panel(app.theme.proc_box, app.theme.title, app.corner_style)
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

pub(super) fn draw_help_overlay(frame: &mut Frame, area: Rect, app: &App) {
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

    let panel = boxed_panel(app.theme.title, app.theme.title, app.corner_style)
        .flat()
        .with_title(presenter::help_title());
    frame.render_widget(&panel, modal);
    let body = panel.inner(modal);
    let buf = frame.buffer_mut();
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
