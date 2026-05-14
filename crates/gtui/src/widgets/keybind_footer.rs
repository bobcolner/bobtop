//! Centered keybind chip row — `Key  desc  │  Key  desc  │ ...`.
//!
//! Shared between [`super::OptionsMenu`] and [`super::HelpModal`] so
//! both modals' footers look identical. Renders a `─` divider line
//! followed by the chips on the next row down; the caller picks the
//! starting `divider_y` (top of the footer block).
//!
//! Chips are rendered with key in `theme.hi_fg`, descriptions in
//! `theme.main_fg`, and `│` separators in `theme.div_line`. The
//! whole row is centered horizontally in `[body.x+1, body.right-1)`.

use ratatui::layout::Rect;
use ratatui::style::{Color, Style};
use ratatui::Frame;

use crate::{display_width, Theme};

/// Render a divider line plus the keybind chips below it. `body` is
/// the modal body rect; the divider lands at `divider_y` and the
/// chips at `divider_y + 1`.
///
/// `bg` is the modal fill color — passed in so the divider and chip
/// cells get a consistent background that matches the rest of the
/// modal (otherwise ratatui would emit a terminal bg reset for these
/// cells and tear the modal fill).
pub fn render(
    frame: &mut Frame,
    body: Rect,
    theme: &Theme,
    bg: Color,
    chips: &[(&str, &str)],
    divider_y: u16,
) {
    let buf = frame.buffer_mut();
    // Divider line spans the inner width of the body (1-cell inset
    // on each side so it doesn't kiss the panel border).
    for x in body.x + 1..body.x + body.width.saturating_sub(1) {
        let cell = &mut buf[(x, divider_y)];
        cell.set_char('─');
        cell.set_style(Style::default().bg(bg).fg(theme.div_line));
    }

    let hint_y = divider_y.saturating_add(1);
    if hint_y >= body.y + body.height {
        return;
    }

    // Pre-compute width so we can center.
    let mut width: usize = 0;
    for (i, (k, d)) in chips.iter().enumerate() {
        if i > 0 {
            width += 5; // "   │   " — 2 pad + 1 sep + 2 pad
        }
        width += display_width(k) + 2 + display_width(d);
    }
    let avail = body.width.saturating_sub(2) as usize;
    let start_x = body.x + 1 + (avail.saturating_sub(width) / 2) as u16;
    let right = body.x + body.width.saturating_sub(1);
    let mut x = start_x;
    let key_fg = theme.hi_fg;
    let desc_fg = theme.main_fg;
    let sep_fg = theme.div_line;

    for (i, (k, d)) in chips.iter().enumerate() {
        if i > 0 {
            for (ch, fg) in [
                (' ', desc_fg),
                (' ', desc_fg),
                ('│', sep_fg),
                (' ', desc_fg),
                (' ', desc_fg),
            ] {
                if x >= right {
                    break;
                }
                let cell = &mut buf[(x, hint_y)];
                cell.set_char(ch);
                cell.set_style(Style::default().bg(bg).fg(fg));
                x = x.saturating_add(1);
            }
        }
        for ch in k.chars() {
            if x >= right {
                break;
            }
            let cell = &mut buf[(x, hint_y)];
            cell.set_char(ch);
            cell.set_style(Style::default().bg(bg).fg(key_fg));
            x = x.saturating_add(1);
        }
        for _ in 0..2 {
            if x >= right {
                break;
            }
            let cell = &mut buf[(x, hint_y)];
            cell.set_char(' ');
            cell.set_style(Style::default().bg(bg));
            x = x.saturating_add(1);
        }
        for ch in d.chars() {
            if x >= right {
                break;
            }
            let cell = &mut buf[(x, hint_y)];
            cell.set_char(ch);
            cell.set_style(Style::default().bg(bg).fg(desc_fg));
            x = x.saturating_add(1);
        }
    }
}
