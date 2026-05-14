use crate::core::Box as BoxKind;
use gtui::widgets::{
    panel as boxed_panel, ConfirmDialog, HelpModal, ModalShell, ToggleRow,
};
use gtui::write_str_clipped;
use ratatui::layout::Rect;
use ratatui::style::Style;
use ratatui::text::Line;
use ratatui::Frame;

use crate::app::App;

use super::presenter;

pub(super) fn draw_boxes_overlay(frame: &mut Frame, area: Rect, app: &App) {
    let want_w: u16 = 38;
    let want_h: u16 = (BoxKind::ALL.len() as u16) + 5;
    let panel = boxed_panel(app.theme.title, app.theme.title, app.corner_style)
        .flat()
        .with_title(presenter::boxes_overlay_title())
        .with_controls("space toggle  ↑↓ move  B/Esc close");
    let bg = app.theme.main_bg.unwrap_or(app.theme.meter_bg);
    let Some(body) = ModalShell::new(panel, want_w, want_h)
        .with_fill(Style::default().bg(bg).fg(app.theme.main_fg))
        .render(frame, area) else {
        return;
    };
    let buf = frame.buffer_mut();

    write_str_clipped(
        buf,
        body.x + 2,
        body.y + 1,
        "(panel changes are live — collectors pause too)",
        body.width.saturating_sub(4),
        Style::default().bg(bg).fg(app.theme.inactive_fg),
    );

    for (i, b) in BoxKind::ALL.iter().enumerate() {
        let row_y = body.y + 3 + i as u16;
        if row_y + 1 >= body.y + body.height {
            break;
        }
        let label = box_label(*b);
        let is_cursor = i == app.ui.boxes_overlay_cursor;
        let row = ToggleRow::new(label, app.boxes.is_enabled(*b))
            .with_cursor(is_cursor)
            .with_colors(
                app.theme.selected_bg,
                app.theme.selected_fg,
                app.theme.main_fg,
                app.theme.inactive_fg,
            );
        frame.render_widget(&row, Rect::new(body.x + 1, row_y, body.width.saturating_sub(2), 1));
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
    let Some(editor) = &app.ui.options else { return };
    editor.menu().render(frame, area, &app.theme);
}

pub(super) fn draw_detail_modal(frame: &mut Frame, area: Rect, app: &App) {
    let Some(d) = &app.ui.detail else { return };
    let want_w = (area.width * 7 / 10).max(50).min(area.width);
    let want_h = (area.height * 7 / 10).max(14).min(area.height);
    let panel = boxed_panel(app.theme.proc_box(), app.theme.title, app.corner_style)
        .flat()
        .with_title(presenter::detail_title(d))
        .with_keybinds(" Esc / Enter close ");
    let bg = app.theme.main_bg.unwrap_or(app.theme.meter_bg);
    let Some(body) = ModalShell::new(panel, want_w, want_h)
        .with_fill(Style::default().bg(bg).fg(app.theme.main_fg))
        .render(frame, area) else {
        return;
    };
    let buf = frame.buffer_mut();

    let mut row = body.y;
    let max_row = body.y + body.height;
    let write_section = |buf: &mut ratatui::buffer::Buffer, row: u16, name: &str| {
        write_str_clipped(
            buf,
            body.x + 1,
            row,
            &format!("── {name} "),
            body.width.saturating_sub(2),
            Style::default().bg(bg).fg(app.theme.hi_fg),
        );
    };
    let write_line = |buf: &mut ratatui::buffer::Buffer, row: u16, s: &str| {
        // write_str_clipped trims at modal width; no need to pre-truncate.
        // Long fields just elide their tail visually.
        write_str_clipped(
            buf,
            body.x + 2,
            row,
            s,
            body.width.saturating_sub(4),
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
    let Some(req) = &app.ui.pending_kill else { return };
    let line1 = format!(" Send {} to pid {} ({})?", req.signal.label(), req.pid, req.name);
    let title = presenter::kill_title(req);
    ConfirmDialog::new(&app.theme.base, &title)
        .with_accent(app.theme.proc_box())
        .with_corner(app.corner_style)
        .with_body(vec![Line::from(line1), Line::from("")])
        .with_actions([("Enter / y", "confirm"), ("Esc / n", "cancel")])
        .render(frame, area);
}

pub const HELP_LINES: &[(&str, &str)] = &[
    ("?", "toggle this help"),
    ("q / Ctrl-C", "quit"),
    ("Esc", "close overlay (or quit when none open)"),
    // panels
    ("1 / 2 / 3 / 4 / 5", "cycle CPU / Mem / Net / Proc / Disk size (default → large → off)"),
    ("B", "boxes overlay — quick on/off toggle for each panel"),
    ("! / @ / # / $", "apply preset 1-4 (Shift+1-4: full+CPU / full+MEM / full+NET / minimal)"),
    // process table
    ("↑ / ↓", "select process"),
    ("PgUp / PgDn / Home / End", "jump in process list"),
    ("← / →", "cycle sort column"),
    ("r", "reverse sort direction"),
    ("p / n / m / c", "sort by Pid / Name / Mem / Cpu"),
    ("g", "cycle group mode: flat → exec → cgroup → container → tree"),
    ("Space", "expand/collapse selected group or subtree"),
    ("[ / ]", "step collapse / expand by one tree depth (or all in grouped views)"),
    ("Enter", "detail (process row) | expand (header)"),
    ("f", "filter processes by name/cmdline"),
    ("k / K", "kill SIGTERM / SIGKILL (confirm dialog)"),
    // misc
    ("+ / -", "adjust global tick (sample rate)"),
    ("O", "options — edit config + save to disk"),
];

pub(super) fn draw_help_overlay(frame: &mut Frame, area: Rect, app: &App) {
    // Banner styled by the widget from the current theme so it stays
    // visually identical to gfb's help banner for the same theme.
    HelpModal::new(&app.theme, " gtop ", HELP_LINES)
        .with_banner_text("GTOP")
        .with_corner(app.corner_style)
        .with_actions(vec![
            ("Esc".into(), "close".into()),
            ("q".into(), "quit".into()),
        ])
        .render(frame, area);
}
