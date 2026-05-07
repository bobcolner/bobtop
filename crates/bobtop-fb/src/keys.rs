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
    /// Send the selected entry to the system trash.
    Trash,
    /// Permanently delete the selected entry (after confirmation).
    HardDelete,
    /// Rename the selected entry — opens an input modal pre-filled
    /// with the current name.
    Rename,
    /// Create a new empty file in the current directory.
    Touch,
    /// Open the recursive file finder.
    StartFind,
    /// Promote the currently-previewed file into in-place edit mode.
    StartEditor,
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
        // `q` and `b` both quit. `b` mirrors bobtop's launch key so
        // a single tap toggles between the two apps when bobtop-fb
        // was opened from the main monitor — feels like flipping
        // panes rather than spawning / killing a subprocess.
        KeyCode::Char('q') | KeyCode::Char('b') => Action::Quit,
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
        // `r` is rename now; refresh moves to F5 (the notify watcher
        // makes manual refresh rarely needed anyway).
        KeyCode::Char('r') => Action::Rename,
        KeyCode::F(5) => Action::Refresh,
        KeyCode::Char('d') => Action::Trash,
        KeyCode::Char('D') => Action::HardDelete,
        KeyCode::Char('a') => Action::Touch,
        KeyCode::Char('f') => Action::StartFind,
        KeyCode::Char('e') => Action::StartEditor,
        KeyCode::PageDown => Action::PageDown,
        KeyCode::PageUp => Action::PageUp,
        _ => Action::Noop,
    }
}
