//! Key → Action mapping. Hardcoded for v1; config-driven keymaps land
//! later in line with the rest of the suite.

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Action {
    Quit,
    CycleFocus,
    Up,
    Down,
    PageUp,
    PageDown,
    Home,
    End,
    /// Activate the row at the cursor — expand/collapse on tree
    /// nodes, load preview on tables.
    Activate,
    /// On the tree pane: collapse the current node (or jump to its
    /// parent if already collapsed). On the preview pane: ignored.
    CollapseOrParent,
    None,
}

pub fn map(ev: KeyEvent) -> Action {
    if ev.modifiers.contains(KeyModifiers::CONTROL) {
        return match ev.code {
            KeyCode::Char('c') => Action::Quit,
            KeyCode::Char('u') => Action::PageUp,
            KeyCode::Char('d') => Action::PageDown,
            _ => Action::None,
        };
    }
    match ev.code {
        KeyCode::Char('q') | KeyCode::Esc => Action::Quit,
        KeyCode::Tab => Action::CycleFocus,
        KeyCode::Char('j') | KeyCode::Down => Action::Down,
        KeyCode::Char('k') | KeyCode::Up => Action::Up,
        KeyCode::PageDown => Action::PageDown,
        KeyCode::PageUp => Action::PageUp,
        KeyCode::Home | KeyCode::Char('g') => Action::Home,
        KeyCode::End | KeyCode::Char('G') => Action::End,
        KeyCode::Char(' ') | KeyCode::Enter => Action::Activate,
        KeyCode::Char('h') | KeyCode::Left | KeyCode::Backspace => Action::CollapseOrParent,
        KeyCode::Char('l') | KeyCode::Right => Action::Activate,
        _ => Action::None,
    }
}
