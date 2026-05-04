use bobtop_tui::widgets::{ProcessTable, TableLayout};
use bobtop_tui::widgets::panel as boxed_panel;
use bobtop_tui::write_str_at;
use ratatui::layout::Rect;
use ratatui::style::Style;
use ratatui::Frame;

use crate::app::App;
use super::presenter;
pub(super) fn draw(frame: &mut Frame, area: Rect, app: &App) {
    let title = presenter::process_title(app);
    let panel = boxed_panel(app.theme.proc_box, app.theme.title, app.corner_style)
        .with_title(title)
        .with_keybinds(
            "q quit  ↑↓ select  ←→ sort  r rev  f filter  g group  Space expand  k/K kill  Enter  ?",
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
    let mut scroll_offset = app.scroll_offset;
    if app.selected_proc >= scroll_offset + body_h && body_h > 0 {
        scroll_offset = app.selected_proc + 1 - body_h;
    }

    let rows = app.display_rows();
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
