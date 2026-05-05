use bobtop_core::Box as BoxKind;
use crate::options_editor::OptionsEditor;
use bobtop_tui::widgets::{
    panel as boxed_panel, ActionBar, ModalShell, SectionHeader, ToggleRow,
};
use bobtop_tui::{truncate_chars, write_str_at};
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
    let want_w: u16 = 56;
    let want_h: u16 = (OptionsEditor::FIELD_COUNT as u16) + 7;
    let panel = boxed_panel(app.theme.title, app.theme.title, app.corner_style)
        .flat()
        .with_title(presenter::options_overlay_title())
        .with_keybinds(" ↑↓ field   ←→ value   Enter save+apply   Esc cancel ");
    let bg = app.theme.main_bg.unwrap_or(app.theme.meter_bg);
    let Some(body) = ModalShell::new(panel, want_w, want_h)
        .with_fill(Style::default().bg(bg).fg(app.theme.main_fg))
        .render(frame, area) else {
        return;
    };
    let buf = frame.buffer_mut();

    write_str_at(
        buf,
        body.x + 2,
        body.y + 1,
        "(saved to ~/.config/bobtop/bobtop.toml on Enter)",
        Style::default().bg(bg).fg(app.theme.inactive_fg),
    );
    let footer = ActionBar::new(vec![
        ("Esc".into(), "cancel".into()),
        ("Enter".into(), "save+apply".into()),
    ])
    .with_colors(
        app.theme.div_line,
        app.theme.hi_fg,
        app.theme.main_fg,
        app.theme.selected_bg,
    );
    if body.height > 3 {
        frame.render_widget(
            &footer,
            Rect::new(
                body.x + 2,
                body.bottom().saturating_sub(1),
                body.width.saturating_sub(4),
                1,
            ),
        );
    }
    let general = editor.general_form().with_colors(
        app.theme.selected_bg,
        app.theme.selected_fg,
        app.theme.main_fg,
        app.theme.inactive_fg,
        app.theme.main_fg,
    );
    let advanced = editor.behavior_form().with_colors(
        app.theme.selected_bg,
        app.theme.selected_fg,
        app.theme.main_fg,
        app.theme.inactive_fg,
        app.theme.main_fg,
    );
    let header1 = SectionHeader::new("general").with_colors(app.theme.div_line, app.theme.title);
    let header2 = SectionHeader::new("behavior").with_colors(app.theme.div_line, app.theme.title);
    frame.render_widget(&header1, Rect::new(body.x + 2, body.y + 3, body.width.saturating_sub(4), 1));
    frame.render_widget(&general, Rect::new(body.x + 2, body.y + 4, body.width.saturating_sub(4), 4));
    frame.render_widget(&header2, Rect::new(body.x + 2, body.y + 9, body.width.saturating_sub(4), 1));
    frame.render_widget(&advanced, Rect::new(body.x + 2, body.y + 10, body.width.saturating_sub(4), 6));
}

pub(super) fn draw_detail_modal(frame: &mut Frame, area: Rect, app: &App) {
    let Some(d) = &app.ui.detail else { return };
    let want_w = (area.width * 7 / 10).max(50).min(area.width);
    let want_h = (area.height * 7 / 10).max(14).min(area.height);
    let panel = boxed_panel(app.theme.proc_box, app.theme.title, app.corner_style)
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
    let Some(req) = &app.ui.pending_kill else { return };
    let line1 = format!(" Send {} to pid {} ({})?", req.signal.label(), req.pid, req.name);
    let line2 = " [Enter / y]  confirm    [Esc / n]  cancel ";
    let want_w = (line1.chars().count().max(line2.chars().count()) + 4) as u16;
    let want_h: u16 = 6;
    let title = presenter::kill_title(req);
    let panel = boxed_panel(app.theme.proc_box, app.theme.title, app.corner_style)
        .flat()
        .with_title(title);
    let bg = app.theme.main_bg.unwrap_or(app.theme.meter_bg);
    let Some(body) = ModalShell::new(panel, want_w, want_h)
        .with_fill(Style::default().bg(bg).fg(app.theme.main_fg))
        .render(frame, area) else {
        return;
    };
    let buf = frame.buffer_mut();
    write_str_at(buf, body.x + 1, body.y + 1, &line1, Style::default().bg(bg).fg(app.theme.hi_fg));
    write_str_at(buf, body.x + 1, body.y + 3, line2, Style::default().bg(bg).fg(app.theme.inactive_fg));
    if body.height > 4 {
        let footer = ActionBar::new(vec![("Enter / y".into(), "confirm".into()), ("Esc / n".into(), "cancel".into())])
            .with_colors(app.theme.div_line, app.theme.hi_fg, app.theme.main_fg, app.theme.selected_bg);
        frame.render_widget(&footer, Rect::new(body.x + 2, body.bottom().saturating_sub(1), body.width.saturating_sub(4), 1));
    }
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
    ("1 / 2 / 3 / 4", "toggle CPU / Memory / Network / Process panel (btop-style)"),
    ("! / @ / # / $", "apply preset 1-4 (Shift+1-4: full+CPU / full+MEM / full+NET / minimal)"),
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
    let panel = boxed_panel(app.theme.title, app.theme.title, app.corner_style)
        .flat()
        .with_title(presenter::help_title());
    let modal_bg = app.theme.main_bg.unwrap_or(app.theme.meter_bg);
    let Some(body) = ModalShell::new(panel, want_w, want_h)
        .with_fill(Style::default().bg(modal_bg).fg(app.theme.main_fg))
        .render(frame, area) else {
        return;
    };
    let buf = frame.buffer_mut();
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
    if body.height > 2 {
        let footer = ActionBar::new(vec![("Esc".into(), "close".into()), ("q".into(), "quit".into())])
            .with_colors(app.theme.div_line, app.theme.hi_fg, app.theme.main_fg, app.theme.selected_bg);
        frame.render_widget(&footer, Rect::new(body.x + 2, body.bottom().saturating_sub(1), body.width.saturating_sub(4), 1));
    }
}
