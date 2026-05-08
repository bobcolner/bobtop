use bobtop_tui::middle_anchor_scroll;
use bobtop_tui::widgets::panel as boxed_panel;
use bobtop_tui::write_str_at;
use ratatui::layout::Rect;
use ratatui::style::Style;
use ratatui::Frame;

use crate::app::App;
use crate::widgets::{DataTable, TableLayout};

pub(super) fn draw(frame: &mut Frame, area: Rect, app: &App) {
    let title = super::presenter::process_title(app);
    let panel = boxed_panel(app.theme.proc_box(), app.theme.title, app.corner_style)
        .with_title(title)
        .with_keybinds(
            "q quit  ↑↓ select  ←→ sort  r rev  s sticky  f filter  g group  Space [/] expand  k/K kill  Enter  ?",
        );
    frame.render_widget(&panel, area);
    let inner = panel.inner(area);
    if inner.width < 20 || inner.height < 2 {
        return;
    }

    let (table_area, filter_bar) = if app.ui.filter_active {
        let body_h = inner.height.saturating_sub(1);
        (
            Rect::new(inner.x, inner.y, inner.width, body_h),
            Some(Rect::new(inner.x, inner.y + body_h, inner.width, 1)),
        )
    } else {
        (inner, None)
    };

    let body_h = table_area.height.saturating_sub(1) as usize;
    let rows = app.display_rows();
    let scroll_offset = middle_anchor_scroll(app.selected_proc, rows.len(), body_h);
    let layout = match app.group_mode {
        crate::group::GroupMode::Flat => TableLayout::Flat,
        crate::group::GroupMode::ByExecutable
        | crate::group::GroupMode::ByCgroup
        | crate::group::GroupMode::ByContainer => TableLayout::Grouped,
        crate::group::GroupMode::ByParent => TableLayout::Tree,
    };

    // Drop RX/s and TX/s when the active net tier doesn't expose
    // per-pid bandwidth (proc_inode shows only connections). Cleaner
    // than displaying "-" in every cell, and frees the gutter for
    // wider process names.
    let show_net = app.net_tier.has_bandwidth();

    // Sticky pid → widget-level sticky-selection. When sticky mode is
    // on, the app's tracked `selected_proc_pid` flows down into the
    // widget which finds the matching row and highlights it. This
    // makes visual selection robust against stale `selected_proc`
    // indices in the gap between a re-sort and the next render.
    let sticky_pid = if app.sticky_proc_selection {
        app.selected_proc_pid
    } else {
        None
    };
    let table = DataTable::new(&rows, &app.theme)
        .with_layout(layout)
        .with_net_columns(show_net)
        .with_selection(Some(app.selected_proc), scroll_offset)
        .with_sort(app.proc_sort)
        .with_direction(app.proc_sort_descending)
        .with_sticky_pid(sticky_pid);
    frame.render_widget(&table, table_area);

    if let Some(bar) = filter_bar {
        let buf = frame.buffer_mut();
        let bg = app.theme.meter_bg;
        for x in 0..bar.width {
            let cell = &mut buf[(bar.x + x, bar.y)];
            cell.set_char(' ');
            cell.set_style(Style::default().bg(bg).fg(app.theme.title));
        }
        let label = format!(" filter: {}█  ", app.ui.filter_text);
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

