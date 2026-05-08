//! Key → Action mapping plus a [`Scope`] impl for the always-on
//! base keymap. Hardcoded for v1; config-driven keymaps land later
//! in line with the rest of the suite.

use bobtop_tui::{Scope, ScopeResult};
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
}

pub fn map(ev: KeyEvent) -> Option<Action> {
    if ev.modifiers.contains(KeyModifiers::CONTROL) {
        return match ev.code {
            KeyCode::Char('c') => Some(Action::Quit),
            KeyCode::Char('u') => Some(Action::PageUp),
            KeyCode::Char('d') => Some(Action::PageDown),
            _ => None,
        };
    }
    match ev.code {
        KeyCode::Char('q') | KeyCode::Esc => Some(Action::Quit),
        KeyCode::Tab => Some(Action::CycleFocus),
        KeyCode::Char('j') | KeyCode::Down => Some(Action::Down),
        KeyCode::Char('k') | KeyCode::Up => Some(Action::Up),
        KeyCode::PageDown => Some(Action::PageDown),
        KeyCode::PageUp => Some(Action::PageUp),
        KeyCode::Home | KeyCode::Char('g') => Some(Action::Home),
        KeyCode::End | KeyCode::Char('G') => Some(Action::End),
        KeyCode::Char(' ') | KeyCode::Enter => Some(Action::Activate),
        KeyCode::Char('h') | KeyCode::Left | KeyCode::Backspace => Some(Action::CollapseOrParent),
        KeyCode::Char('l') | KeyCode::Right => Some(Action::Activate),
        _ => None,
    }
}

/// Base scope — the always-on keymap that sees keys when no modal is
/// open. Sits at the bottom of the [`bobtop_tui::ScopeStack`].
pub struct BaseScope;

impl Scope<Action> for BaseScope {
    fn name(&self) -> &'static str {
        "base"
    }

    fn handle(&mut self, ev: &KeyEvent) -> ScopeResult<Action> {
        match map(*ev) {
            Some(a) => ScopeResult::Action(a),
            None => ScopeResult::PassThrough,
        }
    }
}
