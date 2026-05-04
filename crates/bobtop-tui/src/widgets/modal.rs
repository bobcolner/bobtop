//! Shared centered modal shell.
//!
//! This is a small reusable primitive for TUI dialogs that need:
//! - centered sizing
//! - a `BoxedPanel` frame
//! - optional body clearing/fill
//!
//! App-specific content is still rendered separately into the returned body
//! rect. The shell only handles chrome and placement.

use ratatui::layout::Rect;
use ratatui::style::Style;
use ratatui::Frame;

use super::boxed::BoxedPanel;

#[derive(Debug, Clone)]
pub struct ModalShell {
    panel: BoxedPanel,
    width: u16,
    height: u16,
    fill: Option<Style>,
}

impl ModalShell {
    pub fn new(panel: BoxedPanel, width: u16, height: u16) -> Self {
        Self {
            panel,
            width,
            height,
            fill: None,
        }
    }

    pub fn with_fill(mut self, style: Style) -> Self {
        self.fill = Some(style);
        self
    }

    pub fn panel(&self) -> &BoxedPanel {
        &self.panel
    }

    pub fn render(&self, frame: &mut Frame, area: Rect) -> Option<Rect> {
        if area.width < self.width || area.height < self.height {
            return None;
        }
        let x = area.x + (area.width - self.width) / 2;
        let y = area.y + (area.height - self.height) / 2;
        let modal = Rect::new(x, y, self.width, self.height);
        frame.render_widget(&self.panel, modal);
        let body = self.panel.inner(modal);
        if let Some(style) = self.fill {
            let buf = frame.buffer_mut();
            for yy in body.y..body.y + body.height {
                for xx in body.x..body.x + body.width {
                    let cell = &mut buf[(xx, yy)];
                    cell.set_char(' ');
                    cell.set_style(style);
                }
            }
        }
        Some(body)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ratatui::style::Color;

    #[test]
    fn centers_and_fills_modal() {
        let panel = BoxedPanel::new(Color::Reset, Color::Reset).flat();
        let shell = ModalShell::new(panel, 6, 4).with_fill(Style::default().bg(Color::Blue));
        let area = Rect::new(0, 0, 10, 6);
        let backend = ratatui::backend::TestBackend::new(area.width, area.height);
        let mut term = ratatui::Terminal::new(backend).unwrap();
        term.draw(|f| {
            let body = shell.render(f, area).expect("modal should fit");
            assert_eq!(body.width, 4);
            assert_eq!(body.height, 2);
        })
        .unwrap();
    }
}
