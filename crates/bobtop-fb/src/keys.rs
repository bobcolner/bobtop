//! Keymap → semantic action.
//!
//! v1 hardcodes a vim-flavored layout. Config-driven keymaps land later;
//! the `Action` enum is the stable surface — adding new bindings in v2
//! won't break the dispatch site in `App::handle_key`.

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Action {
    Quit,
    /// Move cursor / scroll up by 1 (target depends on focus).
    MoveUp,
    MoveDown,
    PageUp,
    PageDown,
    Top,
    Bottom,
    EnterDir,
    ParentDir,
    ToggleHidden,
    Refresh,
    /// Cycle focus between the directory list and the preview pane.
    ToggleFocus,
    /// Toggle the large preview modal (spacebar).
    ToggleFullPreview,
    /// Soft cancel — closes the modal if open, otherwise quits.
    Cancel,
    /// Open the `/` filter input prompt.
    StartFilter,
    Noop,
}

pub fn map(ev: KeyEvent) -> Action {
    if ev.modifiers.contains(KeyModifiers::CONTROL) {
        return match ev.code {
            KeyCode::Char('c') => Action::Quit,
            KeyCode::Char('d') => Action::PageDown,
            KeyCode::Char('u') => Action::PageUp,
            _ => Action::Noop,
        };
    }
    match ev.code {
        KeyCode::Char('q') => Action::Quit,
        KeyCode::Esc => Action::Cancel,
        KeyCode::Char(' ') => Action::ToggleFullPreview,
        KeyCode::Char('/') => Action::StartFilter,
        KeyCode::Tab | KeyCode::BackTab => Action::ToggleFocus,
        KeyCode::Char('j') | KeyCode::Down => Action::MoveDown,
        KeyCode::Char('k') | KeyCode::Up => Action::MoveUp,
        KeyCode::Char('h') | KeyCode::Left => Action::ParentDir,
        KeyCode::Char('l') | KeyCode::Right | KeyCode::Enter => Action::EnterDir,
        KeyCode::Char('g') | KeyCode::Home => Action::Top,
        KeyCode::Char('G') | KeyCode::End => Action::Bottom,
        KeyCode::Char('.') => Action::ToggleHidden,
        KeyCode::Char('r') | KeyCode::F(5) => Action::Refresh,
        KeyCode::PageDown => Action::PageDown,
        KeyCode::PageUp => Action::PageUp,
        _ => Action::Noop,
    }
}
