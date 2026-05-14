//! Keymap → semantic action plus a [`Scope`] impl for the always-on
//! base keymap.
//!
//! v1 hardcodes a vim-flavored layout. Config-driven keymaps land later;
//! the `Action` enum is the stable surface — adding new bindings in v2
//! won't break the dispatch site in `App::handle_key`.

use gtui::{Scope, ScopeResult};
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
    /// Toggle the large preview modal (spacebar). No-op in tree mode.
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
    /// Shrink the preview pane one step (Large → Medium → Small → Hidden).
    PreviewNarrower,
    /// Grow the preview pane one step (Hidden → Small → Medium → Large).
    PreviewWider,
    /// Flip between Miller and Tree view modes (`T`). Each mode
    /// keeps its own cursor/sort state so toggling is non-destructive.
    ToggleViewMode,
    /// Toggle the dedicated database browser (`D`). Switches between
    /// the file browser (Miller/Tree) and the DB-only tree view.
    ToggleDatabaseMode,
    /// Tree-mode only: cycle to the next sortable column (`.`).
    TreeCycleSortNext,
    /// Tree-mode only: cycle to the previous sortable column (`,`).
    TreeCycleSortPrev,
    /// Tree-mode only: flip sort direction (`R`).
    TreeReverseSort,
    /// Open the command palette (`:` key).
    StartCommandPalette,
    /// Show file/directory info panel (`i` key).
    ShowInfo,
    /// Open the git branch overlay (`B` key).
    ToggleBranchOverlay,
    /// Suspend / resume crossterm mouse capture (`M` key). With capture
    /// off the terminal handles drag-selection natively so users can
    /// copy text the usual way; the trade-off is that wheel scroll
    /// stops feeding the app until capture is re-enabled.
    ToggleMouseCapture,
    /// Open the modal options menu (`O` key) — tabbed shell hosting
    /// the theme picker, behavior toggles, and DB connection list.
    OpenOptions,
    /// Toggle the help / keybind reference overlay (`?` key).
    ToggleHelp,
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
        // `q` and `b` both quit. `b` mirrors gtop's launch key so
        // a single tap toggles between the two apps when gfb
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
        KeyCode::Char('x') => Action::Trash,
        KeyCode::Char('X') => Action::HardDelete,
        KeyCode::Char('a') => Action::Touch,
        KeyCode::Char('f') => Action::StartFind,
        KeyCode::Char('e') => Action::StartEditor,
        KeyCode::Char('[') => Action::PreviewNarrower,
        KeyCode::Char(']') => Action::PreviewWider,
        KeyCode::PageDown => Action::PageDown,
        KeyCode::PageUp => Action::PageUp,
        // View mode toggle. `T` is unbound in miller mode today and
        // unique enough to not collide with anything muscle memory.
        KeyCode::Char('T') => Action::ToggleViewMode,
        KeyCode::Char('D') => Action::ToggleDatabaseMode,
        // Tree-mode-only sort cycling. `s` / `S` advance / step back
        // through the sortable columns; `R` flips direction. (`.`
        // would have been more mnemonic but it's already bound to
        // ToggleHidden.) These actions are no-ops in miller mode.
        KeyCode::Char('s') => Action::TreeCycleSortNext,
        KeyCode::Char('S') => Action::TreeCycleSortPrev,
        KeyCode::Char('R') => Action::TreeReverseSort,
        KeyCode::Char(':') => Action::StartCommandPalette,
        KeyCode::Char('i') => Action::ShowInfo,
        KeyCode::Char('B') => Action::ToggleBranchOverlay,
        KeyCode::Char('M') => Action::ToggleMouseCapture,
        KeyCode::Char('O') => Action::OpenOptions,
        KeyCode::Char('?') => Action::ToggleHelp,
        _ => Action::Noop,
    }
}

/// Base scope — always-on keymap that sits at the bottom of
/// [`gtui::ScopeStack`]. fb's modals (input prompts, editor,
/// finder, filter input, full preview) are still flag-based today;
/// migrating each into its own pushed Scope is a series of follow-ups.
pub struct BaseScope;

impl Scope<Action> for BaseScope {
    fn name(&self) -> &'static str {
        "base"
    }

    fn handle(&mut self, ev: &KeyEvent) -> ScopeResult<Action> {
        match map(*ev) {
            Action::Noop => ScopeResult::PassThrough,
            a => ScopeResult::Action(a),
        }
    }
}
