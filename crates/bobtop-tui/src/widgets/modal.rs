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
            for yy in modal.y..modal.y + modal.height {
                for xx in modal.x..modal.x + modal.width {
                    let cell = &mut buf[(xx, yy)];
                    if xx >= body.x && xx < body.x + body.width
                        && yy >= body.y && yy < body.y + body.height
                    {
                        // Body: overwrite with fill (blank space + full fill style).
                        cell.set_char(' ');
                        cell.set_style(style);
                    } else if let Some(bg) = style.bg {
                        // Border / chrome cells: ratatui's Block::render writes
                        // border glyphs with border_style that has no background
                        // (bg = None / Reset).  When such a cell overlaps a row
                        // that previously had a coloured background (e.g. the
                        // selected-row highlight), ratatui emits a terminal
                        // background-reset for that cell, punching a visible
                        // "hole" in the highlight.  Patch the fill bg onto the
                        // border cell *after* the block has set its glyph so the
                        // glyph character and fg are preserved but the bg is
                        // consistent with the modal background.
                        cell.set_style(cell.style().patch(Style::default().bg(bg)));
                    }
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

    /// Border cells must carry the fill background so that ratatui's
    /// diff-based rendering never emits a terminal bg-reset for them.
    /// Without this, a border cell that overlaps a highlighted (selected)
    /// row gets bg=None from Block::render, causing ratatui to emit
    /// ESC[49m and punch a visible "hole" in the selection highlight.
    #[test]
    fn border_cells_carry_fill_bg() {
        let panel = BoxedPanel::new(Color::Green, Color::Green).flat();
        let fill = Style::default().bg(Color::Blue).fg(Color::White);
        let shell = ModalShell::new(panel, 6, 4).with_fill(fill);
        let area = Rect::new(0, 0, 10, 6);
        let backend = ratatui::backend::TestBackend::new(area.width, area.height);
        let mut term = ratatui::Terminal::new(backend).unwrap();
        term.draw(|f| {
            shell.render(f, area);
            let buf = f.buffer_mut();
            // Modal is 6×4, centred → x=2, y=1. Top-left corner at (2,1).
            // All cells inside the modal rect should have bg=Blue.
            for y in 1..5u16 {
                for x in 2..8u16 {
                    assert_eq!(
                        buf[(x, y)].style().bg,
                        Some(Color::Blue),
                        "expected Blue bg at ({x},{y})"
                    );
                }
            }
        })
        .unwrap();
    }
}
