use crate::app::{KillRequest, OptionsState, ProcessDetail};

#[derive(Debug, Clone)]
pub struct UiState {
    /// `?` toggles the centered help overlay listing keybinds (B2).
    pub show_help: bool,
    /// `B` toggles the boxes overlay — show/hide individual panels (B5).
    pub show_boxes_overlay: bool,
    /// Cursor row inside the boxes overlay (index into `bobtop_core::Box::ALL`).
    pub boxes_overlay_cursor: usize,

    /// `f` opens the process filter input (B3b). While `filter_active`,
    /// keystrokes append to `filter_text`; rebuild_sorted hides processes
    /// whose name and cmdline both miss the substring (case-insensitive).
    /// `filter_text` persists across edit-mode toggles so the user can
    /// briefly switch focus and come back without re-typing.
    pub filter_active: bool,
    pub filter_text: String,

    /// `k` (SIGTERM) / `K` (SIGKILL) opens a confirm dialog targeting the
    /// currently-selected process (B3c). `Some(req)` = modal showing.
    /// Enter sends the signal via libc::kill; Esc cancels.
    pub pending_kill: Option<KillRequest>,
    /// Most recent kill outcome — drives a brief one-line toast in the
    /// status bar so the user gets feedback (success / errno).
    pub last_kill_msg: Option<String>,

    /// `Enter` opens a read-only detail modal for the selected pid (B3d).
    /// Lazy-populated from /proc on open; not refreshed every tick because
    /// most fields (cmdline, environ, fd count) don't churn at human speed.
    pub detail: Option<ProcessDetail>,

    /// `O` opens the options overlay (B11b). When `Some`, the user is
    /// editing a snapshot of the persisted Config; ←/→ cycles the field
    /// at the cursor, ↑/↓ moves cursor, Enter saves to disk + applies
    /// live, Esc closes without saving.
    pub options: Option<OptionsState>,
    /// Status line shown in the header strip after a save attempt
    /// (success path or error). Cleared on next user action.
    pub last_options_msg: Option<String>,

    /// Set by `apply_event` and `handle_input` whenever something
    /// observable to the renderer has changed. Cleared by `take_dirty`
    /// after the render loop calls `terminal.draw()`. Lets us render
    /// on change instead of on a 60Hz heartbeat — see `tui::run`.
    pub dirty: bool,
}

impl Default for UiState {
    fn default() -> Self {
        Self {
            show_help: false,
            show_boxes_overlay: false,
            boxes_overlay_cursor: 0,
            filter_active: false,
            filter_text: String::new(),
            pending_kill: None,
            last_kill_msg: None,
            detail: None,
            options: None,
            last_options_msg: None,
            dirty: true,
        }
    }
}
