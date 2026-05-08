//! Daemon scope-stack glue.
//!
//! Most of bobtop's input dispatch still lives in `App::handle_key` —
//! a per-key match that mutates `self` directly. This module hosts the
//! pieces that have been pulled out into the [`bobtop_tui::Scope`]
//! pattern: the always-on [`BaseScope`] (currently a pass-through),
//! the per-modal scopes that *have* been migrated, and the [`Action`]
//! enum that returns from each scope's `handle`.
//!
//! Migration is incremental — each modal gets its own commit. For now
//! the only real scope user is [`HelpScope`], extracted as the proof
//! that the pattern fits the daemon's modal shape.

use bobtop_tui::{Scope, ScopeResult};
use crossterm::event::{KeyCode, KeyEvent};

/// Actions a scope can request the App to run. As more modals are
/// migrated, this grows — see `App::run_scope_action`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Action {
    /// Quit the app. Honoured even with modals up so users always
    /// have an escape.
    Quit,
    /// Close the help overlay. The HelpScope returns this via
    /// `CloseWith` so the scope stack pops itself.
    CloseHelp,
}

/// Bottom of the stack — always present. Currently a no-op so the
/// existing `handle_key` cascade still sees every key. As keys are
/// migrated to the scope-stack pattern, this gains arms that return
/// real actions.
pub struct BaseScope;

impl Scope<Action> for BaseScope {
    fn name(&self) -> &'static str {
        "base"
    }

    fn handle(&mut self, _ev: &KeyEvent) -> ScopeResult<Action> {
        ScopeResult::PassThrough
    }
}

/// Pushed onto the stack when `?` opens the help overlay; popped when
/// closed. Replaces the old flag-based `handle_help_overlay` in the
/// modal cascade. With this scope on the stack:
///
/// - `?` / `Esc` close help (CloseWith → pops + dispatches `CloseHelp`)
/// - `q` quits the app (no pop — the user wants out, not just out of help)
/// - any other key is `Consumed` so the user can't accidentally sort
///   columns or kill a process while reading help
pub struct HelpScope;

impl Scope<Action> for HelpScope {
    fn name(&self) -> &'static str {
        "help"
    }

    fn handle(&mut self, ev: &KeyEvent) -> ScopeResult<Action> {
        match ev.code {
            KeyCode::Char('?') | KeyCode::Esc => ScopeResult::CloseWith(Action::CloseHelp),
            KeyCode::Char('q') => ScopeResult::Action(Action::Quit),
            _ => ScopeResult::Consumed,
        }
    }
}
