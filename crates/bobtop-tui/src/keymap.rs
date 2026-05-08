//! Keymap dispatch — a stack of scopes that turn key events into
//! app-defined actions.
//!
//! The shape generalises the cascade pattern every TUI app in the
//! suite was reinventing: a "main keymap" plus N modals, each with
//! its own keybinds, where the topmost modal sees the key first and
//! can either consume it or pass through. The pre-toolkit cascade
//! had to be open-coded as a chain of `Option<ControlFlow>` returns
//! per modal — and a single buggy "is this modal open" flag could
//! silently swallow keys for the rest of the session.
//!
//! `ScopeStack` makes the cascade explicit. Every modal that wants
//! keys must `push` a [`Scope`] onto the stack; closing a modal
//! pops it (or returns [`ScopeResult::Close`] from a `handle` call).
//! [`ScopeStack::stack_names`] lets apps render a debug overlay so
//! "what's eating my keys" is one keystroke away.
//!
//! The stack is generic over the app's Action enum — there's no
//! ratatui or domain coupling. Apps pair it with their own
//! key→action handler.

use crossterm::event::KeyEvent;

/// What a scope decides to do with an incoming key event.
#[derive(Debug)]
pub enum ScopeResult<A> {
    /// Run this app-defined action. Stops dispatch.
    Action(A),
    /// Key handled internally (typed into a text input, toggled a
    /// boolean, advanced a cursor) — don't pass further down the
    /// stack and don't return an action to the app.
    Consumed,
    /// Not for me; let the next-lower scope try.
    PassThrough,
    /// Pop me from the stack. Stops dispatch.
    Close,
    /// Pop me AND return an action (e.g. "Esc closes this modal AND
    /// dispatches `Cancel` so the app can clean up").
    CloseWith(A),
}

/// One layer of the dispatch stack. Apps implement this per modal,
/// plus once for the base keymap that's always present.
pub trait Scope<A> {
    /// Stable identifier shown in [`ScopeStack::stack_names`] —
    /// useful for a debug overlay so a stuck modal becomes visible
    /// at runtime.
    fn name(&self) -> &'static str;

    /// React to a key. `&mut self` so scopes can hold their own
    /// state (text-input cursor, multi-step option editor, etc.).
    fn handle(&mut self, ev: &KeyEvent) -> ScopeResult<A>;
}

/// Stack of scopes. Top-most scope sees keys first; the base scope
/// at index 0 is always present and never popped.
pub struct ScopeStack<A> {
    scopes: Vec<Box<dyn Scope<A>>>,
}

impl<A> ScopeStack<A> {
    /// Create a stack with `base` at the bottom. The base is the
    /// app's main keymap — the always-on layer that sees keys when
    /// no modal is up.
    pub fn new(base: Box<dyn Scope<A>>) -> Self {
        Self { scopes: vec![base] }
    }

    /// Push a transient scope (a modal). It will see keys before
    /// every scope below it until it's popped.
    pub fn push(&mut self, scope: Box<dyn Scope<A>>) {
        self.scopes.push(scope);
    }

    /// Pop the top scope. Returns `None` (and is a no-op) when
    /// only the base remains — the base is guaranteed to stay.
    pub fn pop(&mut self) -> Option<Box<dyn Scope<A>>> {
        if self.scopes.len() > 1 {
            self.scopes.pop()
        } else {
            None
        }
    }

    /// Number of layers currently on the stack (always ≥ 1).
    pub fn depth(&self) -> usize {
        self.scopes.len()
    }

    /// Names of every scope, base first, top-most last. Pair with a
    /// debug-toggle keybind to render `[main, options, kill_confirm]`
    /// somewhere on screen — that's how a stuck modal becomes visible.
    pub fn stack_names(&self) -> Vec<&'static str> {
        self.scopes.iter().map(|s| s.name()).collect()
    }

    /// Mutable access to the topmost scope — needed for a few
    /// patterns where the modal owns enough state that the app
    /// wants to peek at it (e.g. read out filter text after
    /// `Action::ApplyFilter`). Most apps shouldn't need this.
    pub fn top_mut(&mut self) -> Option<&mut (dyn Scope<A> + '_)> {
        self.scopes.last_mut().map(|b| &mut **b as &mut dyn Scope<A>)
    }

    /// Walk the stack top-down, dispatching to the first scope that
    /// returns something other than `PassThrough`. Pops on `Close`.
    pub fn dispatch(&mut self, ev: &KeyEvent) -> Option<A> {
        let mut i = self.scopes.len();
        while i > 0 {
            i -= 1;
            match self.scopes[i].handle(ev) {
                ScopeResult::Action(a) => return Some(a),
                ScopeResult::Consumed => return None,
                ScopeResult::Close => {
                    if i > 0 {
                        self.scopes.remove(i);
                    }
                    return None;
                }
                ScopeResult::CloseWith(a) => {
                    if i > 0 {
                        self.scopes.remove(i);
                    }
                    return Some(a);
                }
                ScopeResult::PassThrough => continue,
            }
        }
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    enum Act {
        Quit,
        Down,
        Confirm,
        Cancel,
    }

    /// Base scope — answers j/k with Down/Up-style actions, q with Quit.
    /// Anything else is PassThrough (so modals stacked above can
    /// see-through to the base, in the rare cases where they want to).
    struct Base;
    impl Scope<Act> for Base {
        fn name(&self) -> &'static str {
            "base"
        }
        fn handle(&mut self, ev: &KeyEvent) -> ScopeResult<Act> {
            match ev.code {
                KeyCode::Char('q') => ScopeResult::Action(Act::Quit),
                KeyCode::Char('j') => ScopeResult::Action(Act::Down),
                _ => ScopeResult::PassThrough,
            }
        }
    }

    /// Confirm modal — y/Enter → Confirm+Close, Esc → Cancel+Close.
    /// Every other key is `Consumed` so the user can't accidentally
    /// quit while the modal is up.
    struct Confirm;
    impl Scope<Act> for Confirm {
        fn name(&self) -> &'static str {
            "confirm"
        }
        fn handle(&mut self, ev: &KeyEvent) -> ScopeResult<Act> {
            match ev.code {
                KeyCode::Enter | KeyCode::Char('y') => ScopeResult::CloseWith(Act::Confirm),
                KeyCode::Esc => ScopeResult::CloseWith(Act::Cancel),
                _ => ScopeResult::Consumed,
            }
        }
    }

    fn key(c: char) -> KeyEvent {
        KeyEvent::new(KeyCode::Char(c), KeyModifiers::NONE)
    }

    #[test]
    fn base_dispatches_actions() {
        let mut stack: ScopeStack<Act> = ScopeStack::new(Box::new(Base));
        assert_eq!(stack.dispatch(&key('q')), Some(Act::Quit));
        assert_eq!(stack.dispatch(&key('j')), Some(Act::Down));
        assert_eq!(stack.dispatch(&key('z')), None);
    }

    #[test]
    fn modal_swallows_unmatched_keys() {
        let mut stack: ScopeStack<Act> = ScopeStack::new(Box::new(Base));
        stack.push(Box::new(Confirm));
        // q would quit at the base — but the modal Consumes it.
        assert_eq!(stack.dispatch(&key('q')), None);
        // j too.
        assert_eq!(stack.dispatch(&key('j')), None);
    }

    #[test]
    fn closewith_pops_and_returns_action() {
        let mut stack: ScopeStack<Act> = ScopeStack::new(Box::new(Base));
        stack.push(Box::new(Confirm));
        assert_eq!(stack.depth(), 2);
        let res = stack.dispatch(&key('y'));
        assert_eq!(res, Some(Act::Confirm));
        assert_eq!(stack.depth(), 1, "modal should have been popped");
        // Now base is back in charge.
        assert_eq!(stack.dispatch(&key('q')), Some(Act::Quit));
    }

    #[test]
    fn esc_closes_modal_with_cancel() {
        let mut stack: ScopeStack<Act> = ScopeStack::new(Box::new(Base));
        stack.push(Box::new(Confirm));
        let res = stack.dispatch(&KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE));
        assert_eq!(res, Some(Act::Cancel));
        assert_eq!(stack.depth(), 1);
    }

    #[test]
    fn passthrough_falls_to_lower_scope() {
        struct Transparent;
        impl Scope<Act> for Transparent {
            fn name(&self) -> &'static str {
                "transparent"
            }
            fn handle(&mut self, _: &KeyEvent) -> ScopeResult<Act> {
                ScopeResult::PassThrough
            }
        }
        let mut stack: ScopeStack<Act> = ScopeStack::new(Box::new(Base));
        stack.push(Box::new(Transparent));
        // Top scope passes through; base sees the key.
        assert_eq!(stack.dispatch(&key('q')), Some(Act::Quit));
        // Transparent stays on the stack — PassThrough doesn't pop.
        assert_eq!(stack.depth(), 2);
    }

    #[test]
    fn base_scope_cannot_be_popped() {
        let mut stack: ScopeStack<Act> = ScopeStack::new(Box::new(Base));
        assert!(stack.pop().is_none());
        assert_eq!(stack.depth(), 1);
    }

    #[test]
    fn stack_names_top_last() {
        let mut stack: ScopeStack<Act> = ScopeStack::new(Box::new(Base));
        stack.push(Box::new(Confirm));
        assert_eq!(stack.stack_names(), vec!["base", "confirm"]);
    }
}
