//! Centered confirm / prompt dialog.
//!
//! Both gtop (kill confirm) and gfb (rename / touch /
//! trash / hard-delete prompts) reinvented this — title, body lines,
//! a footer hint, panel chrome. This is the union, parameterized so
//! either app's exact look stays reachable:
//!
//! - `with_hint(s)` puts the hint on the panel's bottom border via
//!   `BoxedPanel::with_controls` (gfb style — quieter).
//! - `with_actions(items)` renders an [`ActionBar`] inside the body
//!   bottom row (gtop style — louder, colored).
//!
//! Sizing is auto: width fits the longest body line within
//! `[min_width, max_width]` (defaults 32–80 cells); height fits the
//! body lines plus chrome and an optional actions row.

use ratatui::layout::Rect;
use ratatui::style::{Color, Style};
use ratatui::text::Line;
use ratatui::Frame;

use crate::widgets::{ActionBar, BoxedPanel, CornerStyle, ModalShell, ScrollableText};
use crate::{display_width, Theme};

/// Footer style — apps pick which one matches their existing look.
#[derive(Debug, Clone, Default)]
pub enum DialogFooter {
    #[default]
    None,
    /// Single-line hint rendered on the panel's bottom border.
    Hint(String),
    /// Colored keybind chips rendered inside the body bottom row.
    Actions(Vec<(String, String)>),
}

pub struct ConfirmDialog<'a> {
    pub title: &'a str,
    pub body: Vec<Line<'a>>,
    pub footer: DialogFooter,
    pub accent: Color,
    pub theme: &'a Theme,
    pub corner: CornerStyle,
    pub min_width: u16,
    pub max_width: u16,
}

impl<'a> ConfirmDialog<'a> {
    pub fn new(theme: &'a Theme, title: &'a str) -> Self {
        Self {
            title,
            body: Vec::new(),
            footer: DialogFooter::None,
            // Default accent: panel slot 3 (formerly `proc_box`) — both
            // existing apps used it. Override via `with_accent` if your
            // app picks a different panel.
            accent: theme.panel_accents[3],
            theme,
            corner: CornerStyle::default(),
            min_width: 32,
            max_width: 80,
        }
    }

    pub fn with_body(mut self, body: Vec<Line<'a>>) -> Self {
        self.body = body;
        self
    }

    pub fn with_hint(mut self, hint: impl Into<String>) -> Self {
        self.footer = DialogFooter::Hint(hint.into());
        self
    }

    pub fn with_actions<I, K, V>(mut self, actions: I) -> Self
    where
        I: IntoIterator<Item = (K, V)>,
        K: Into<String>,
        V: Into<String>,
    {
        self.footer = DialogFooter::Actions(
            actions.into_iter().map(|(k, v)| (k.into(), v.into())).collect(),
        );
        self
    }

    pub fn with_accent(mut self, accent: Color) -> Self {
        self.accent = accent;
        self
    }

    pub fn with_corner(mut self, corner: CornerStyle) -> Self {
        self.corner = corner;
        self
    }

    pub fn with_width_bounds(mut self, min: u16, max: u16) -> Self {
        self.min_width = min;
        self.max_width = max;
        self
    }

    /// Render the dialog centered in `area`. Returns `Some(body_rect)`
    /// for callers that want to overlay extra chrome (e.g. a cursor
    /// indicator on a specific row); returns `None` if the area was
    /// too small for the modal to fit.
    pub fn render(&self, frame: &mut Frame<'_>, area: Rect) -> Option<Rect> {
        let mut panel = BoxedPanel::new(self.accent, self.theme.title)
            .with_title(self.title.to_string())
            .with_corner_style(self.corner)
            .flat();
        if let DialogFooter::Hint(s) = &self.footer {
            panel = panel.with_controls(s.clone());
        }

        let actions_row = matches!(self.footer, DialogFooter::Actions(_));
        let body_h = self.body.len() as u16;
        let modal_h = (body_h + if actions_row { 1 } else { 0 } + 4).max(6);

        let longest = self
            .body
            .iter()
            .map(|line| line_display_width(line))
            .max()
            .unwrap_or(0);
        let title_pill = display_width(self.title) + 4;
        // When a hint is set it rides the top-right of the panel as a
        // second pill — reserve room so it doesn't overlap the title
        // pill, which is left-anchored. The +6 is generous slack for
        // segment joins (` │ ` between `"  "`-delimited parts).
        let hint_pill = match &self.footer {
            DialogFooter::Hint(s) => display_width(s) + 6,
            _ => 0,
        };
        let header_w = if hint_pill > 0 {
            title_pill + hint_pill + 2
        } else {
            title_pill
        };
        let raw_w = longest.max(header_w) as u16 + 4;
        let modal_w = raw_w.clamp(self.min_width, self.max_width);

        let bg = self.theme.main_bg.unwrap_or(self.theme.meter_bg);
        let shell = ModalShell::new(panel, modal_w, modal_h)
            .with_fill(Style::default().bg(bg).fg(self.theme.main_fg));
        let body = shell.render(frame, area)?;
        if body.height == 0 {
            return Some(body);
        }

        let body_rect = if actions_row {
            Rect::new(body.x, body.y, body.width, body.height.saturating_sub(1))
        } else {
            body
        };
        let preview = ScrollableText::new(&self.body, self.theme).with_line_numbers(false);
        frame.render_widget(&preview, body_rect);

        if let DialogFooter::Actions(items) = &self.footer {
            if body.height > 0 {
                let bar_y = body.bottom().saturating_sub(1);
                let bar = ActionBar::new(items.clone()).with_colors(
                    self.theme.div_line,
                    self.theme.hi_fg,
                    self.theme.main_fg,
                    self.theme.selected_bg,
                );
                let bar_rect = Rect::new(
                    body.x.saturating_add(2),
                    bar_y,
                    body.width.saturating_sub(4),
                    1,
                );
                frame.render_widget(&bar, bar_rect);
            }
        }
        Some(body)
    }
}

fn line_display_width(line: &Line<'_>) -> usize {
    line.spans
        .iter()
        .map(|s| display_width(&s.content))
        .sum()
}

#[cfg(test)]
mod tests {
    use super::*;
    use ratatui::backend::TestBackend;
    use ratatui::Terminal;

    fn read_text(buf: &ratatui::buffer::Buffer, y: u16, x_start: u16, len: u16) -> String {
        (x_start..x_start + len)
            .filter_map(|x| buf[(x, y)].symbol().chars().next())
            .collect()
    }

    #[test]
    fn renders_title_and_body_centered() {
        let theme = Theme::fallback();
        // Wide enough that the title (top-left pill) and the hint
        // (top-right pill) don't fight for the same columns.
        let backend = TestBackend::new(120, 12);
        let mut term = Terminal::new(backend).unwrap();
        term.draw(|f| {
            ConfirmDialog::new(&theme, "are you sure?")
                .with_body(vec![Line::from("foo.txt"), Line::from(""), Line::from("This cannot be undone.")])
                .with_hint("y confirm  •  Esc cancel")
                .render(f, f.area());
        })
        .unwrap();
        let buf = term.backend().buffer();
        let mut found_title = false;
        let mut found_body = false;
        for y in 0..12 {
            let row: String = read_text(buf, y, 0, 120);
            if row.contains("are you sure?") {
                found_title = true;
            }
            if row.contains("This cannot be undone.") {
                found_body = true;
            }
        }
        assert!(found_title, "title missing");
        assert!(found_body, "body missing");
    }

    #[test]
    fn returns_none_when_area_too_small() {
        let theme = Theme::fallback();
        let backend = TestBackend::new(8, 4);
        let mut term = Terminal::new(backend).unwrap();
        let result = std::cell::Cell::new(None);
        term.draw(|f| {
            let r = ConfirmDialog::new(&theme, "title")
                .with_body(vec![Line::from("body")])
                .render(f, f.area());
            result.set(r);
        })
        .unwrap();
        assert!(result.get().is_none(), "expected None for tiny area");
    }
}
