//! App state machine and run loop.
//!
//! Holds cwd + entries + nav cursor + preview state. Preview rendering
//! is offloaded to a tokio blocking task so file I/O and syntect
//! highlighting don't stall the run loop. A monotonic generation
//! counter discards stale results when the user has already moved on.

use std::io;
use std::path::{Path, PathBuf};
use std::sync::mpsc;
use std::sync::Arc;
use std::time::Duration;

use anyhow::Result;
use bobtop_tui::Theme;
use crossterm::event::{self, Event, KeyCode, KeyEvent, KeyModifiers, MouseEventKind};
use ratatui::backend::Backend;
use ratatui::Terminal;
use tokio::runtime::Runtime;

use crate::fs::entry::FsEntry;
use crate::fs::scan::{scan_dir, SortMode};
use crate::keys::{map as map_key, Action};
use crate::nav::Nav;
use crate::preview::{
    self,
    cache::PreviewCache,
    PreviewBody, PreviewLimits, PreviewResult, PreviewState,
};
use crate::ui;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Focus {
    List,
    Preview,
}

/// Suspend the TUI, run `$EDITOR` (default `nano`) on `path`, and
/// restore the TUI when the editor exits. Errors during teardown or
/// restore propagate; spawning the editor itself is best-effort —
/// most editors only fail in pathological setups (PATH unset etc.)
/// and we'd rather come back to a working browser than panic.
fn open_in_editor<B: Backend>(terminal: &mut ratatui::Terminal<B>, path: &Path) -> Result<()>
where
    B: ratatui::backend::Backend + std::io::Write,
{
    use crossterm::event::{DisableMouseCapture, EnableMouseCapture};
    use crossterm::execute;
    use crossterm::terminal::{
        disable_raw_mode, enable_raw_mode, EnterAlternateScreen, LeaveAlternateScreen,
    };
    let editor = std::env::var("VISUAL")
        .or_else(|_| std::env::var("EDITOR"))
        .unwrap_or_else(|_| "nano".into());

    disable_raw_mode().ok();
    let backend = terminal.backend_mut();
    execute!(backend, LeaveAlternateScreen, DisableMouseCapture).ok();
    terminal.show_cursor().ok();

    let _ = std::process::Command::new(&editor).arg(path).status();

    enable_raw_mode().ok();
    let backend = terminal.backend_mut();
    execute!(backend, EnterAlternateScreen, EnableMouseCapture).ok();
    // Ratatui's diff renderer assumes the alt-screen contents match
    // its prior buffer; the editor scribbled all over that, so force
    // a full redraw on the next tick.
    terminal.clear()?;
    Ok(())
}

/// Build a case-insensitive glob matcher for the filter input. Plain
/// strings (no `*`, `?`, `[`) are wrapped as `*input*` so substring
/// matching is the default. Returns None for malformed patterns —
/// the caller falls back to substring search.
fn build_filter_matcher(raw: &str) -> Option<globset::GlobMatcher> {
    let has_meta = raw.contains(['*', '?', '[']);
    let pattern = if has_meta {
        raw.to_string()
    } else {
        format!("*{}*", raw)
    };
    globset::GlobBuilder::new(&pattern)
        .case_insensitive(true)
        .literal_separator(false)
        .build()
        .ok()
        .map(|g| g.compile_matcher())
}

pub struct App {
    cwd: PathBuf,
    /// Disk scan result for `cwd`. Stable until `refresh()`.
    all_entries: Vec<FsEntry>,
    /// Currently visible subset of `all_entries` after `filter` is
    /// applied. The cursor and scroll math operate on this slice;
    /// callers see only this via `entries()`.
    entries: Vec<FsEntry>,
    nav: Nav,
    /// Snapshot of the parent directory, used by the leftmost miller
    /// column. Empty when `cwd` has no parent (root).
    parent_entries: Vec<FsEntry>,
    /// Index in `parent_entries` whose path equals `cwd` — the row to
    /// highlight in the parent column.
    parent_cursor: Option<usize>,
    /// Applied filter string. None = no filter; Some("") collapses to
    /// None at apply time (no need to track an empty filter).
    filter: Option<String>,
    /// While the user is editing the filter (`/`-prompt is open) this
    /// holds the in-progress text. Always reflects what's on screen
    /// since we apply live.
    filter_input: Option<String>,
    /// Set by `Action::EnterDir` when the selection is a regular file.
    /// The run loop consumes this after `handle()` returns to suspend
    /// the TUI and spawn `$EDITOR`.
    pending_editor: Option<PathBuf>,
    show_hidden: bool,
    sort: SortMode,
    list_viewport_h: usize,
    theme: Theme,

    rt: Runtime,
    preview_state: PreviewState,
    preview_gen: u64,
    preview_cache: PreviewCache,
    preview_tx: mpsc::Sender<PreviewResult>,
    preview_rx: mpsc::Receiver<PreviewResult>,
    preview_limits: PreviewLimits,
    /// Top line index shown in the side preview pane. Reset to 0 when
    /// the visible preview path changes; clamped against the rendered
    /// line count at draw time.
    preview_scroll: usize,
    /// Independent scroll state for the full-screen modal — keeps the
    /// side pane's position untouched while the user pages through
    /// the same file in the larger modal view. Reset to 0 each time
    /// the modal opens.
    modal_scroll: usize,
    /// Last preview pane height (in cells), recorded by the renderer
    /// so PgUp/PgDn step by a real screen-full and we can clamp scroll
    /// to keep at least one row of content visible.
    preview_viewport_h: usize,
    focus: Focus,
    /// Whether the full-screen preview modal is open. While true, the
    /// background panes still draw underneath but key routing prefers
    /// the modal: vertical movement scrolls the preview, Esc/Space
    /// closes, other actions are ignored.
    full_preview: bool,
    /// Inclusive-exclusive x range of the side preview pane, recorded
    /// by the renderer. Used to dispatch mouse-wheel events to the
    /// pane the cursor is over.
    preview_pane_x_start: u16,
    preview_pane_x_end: u16,
}

impl App {
    pub fn new(start: PathBuf, theme: Theme) -> io::Result<Self> {
        let cwd = start.canonicalize().unwrap_or(start);
        let rt = tokio::runtime::Builder::new_multi_thread()
            .worker_threads(2)
            .enable_all()
            .build()
            .map_err(|e| io::Error::other(format!("tokio: {e}")))?;
        let (preview_tx, preview_rx) = mpsc::channel();
        let mut app = Self {
            cwd,
            all_entries: Vec::new(),
            entries: Vec::new(),
            nav: Nav::default(),
            parent_entries: Vec::new(),
            parent_cursor: None,
            filter: None,
            filter_input: None,
            pending_editor: None,
            show_hidden: false,
            sort: SortMode::Name,
            list_viewport_h: 0,
            theme,
            rt,
            preview_state: PreviewState::None,
            preview_gen: 0,
            preview_cache: PreviewCache::default(),
            preview_tx,
            preview_rx,
            preview_limits: PreviewLimits::default(),
            preview_scroll: 0,
            modal_scroll: 0,
            preview_viewport_h: 0,
            focus: Focus::List,
            full_preview: false,
            preview_pane_x_start: 0,
            preview_pane_x_end: 0,
        };
        app.refresh()?;
        app.request_preview();
        Ok(app)
    }

    pub fn entries(&self) -> &[FsEntry] {
        &self.entries
    }

    pub fn parent_entries(&self) -> &[FsEntry] {
        &self.parent_entries
    }

    pub fn parent_cursor(&self) -> Option<usize> {
        self.parent_cursor
    }

    pub fn nav(&self) -> &Nav {
        &self.nav
    }

    pub fn cwd_display(&self) -> String {
        self.cwd.display().to_string()
    }

    pub fn selected(&self) -> Option<&FsEntry> {
        self.entries.get(self.nav.cursor)
    }

    pub fn theme(&self) -> &Theme {
        &self.theme
    }

    pub fn preview_state(&self) -> &PreviewState {
        &self.preview_state
    }

    pub fn preview_scroll(&self) -> usize {
        self.preview_scroll
    }

    pub fn modal_scroll(&self) -> usize {
        self.modal_scroll
    }

    pub fn focus(&self) -> Focus {
        self.focus
    }

    pub fn is_full_preview(&self) -> bool {
        self.full_preview
    }

    /// Recorded by the renderer once it knows the preview pane rect.
    /// Mouse-wheel routing reads this back to tell which pane the user
    /// is hovering.
    pub fn record_preview_pane_x(&mut self, start: u16, end: u16) {
        self.preview_pane_x_start = start;
        self.preview_pane_x_end = end;
    }

    /// Direct list cursor movement — bypasses the focus-based routing
    /// in `handle()`. Used for mouse-wheel events on the list pane,
    /// which should always move the cursor regardless of which pane
    /// owns keyboard focus.
    fn move_list_cursor(&mut self, delta: isize) {
        let prior = self.nav.cursor;
        self.nav.move_by(delta, self.entries.len());
        if self.nav.cursor != prior {
            self.focus = Focus::List;
            self.request_preview();
        }
    }

    /// Dispatch a mouse-wheel scroll to the right pane: modal swallows
    /// everything; otherwise we route by the column the cursor is in.
    /// Returns false (never quits) — the signature mirrors `handle()`.
    pub fn handle_mouse_scroll(&mut self, column: u16, delta: isize) {
        if self.full_preview {
            self.scroll_preview(delta);
            return;
        }
        let in_preview = column >= self.preview_pane_x_start
            && column < self.preview_pane_x_end
            && self.preview_pane_x_end > self.preview_pane_x_start;
        if in_preview {
            self.scroll_preview(delta);
        } else {
            self.move_list_cursor(delta);
        }
    }

    /// Number of lines in the current `Ready` preview if it's a Lines
    /// body. Image bodies don't scroll (rendered to fit), so they
    /// report 0 — handle.handle_focus_aware_move uses that to no-op.
    pub fn preview_total_lines(&self) -> usize {
        match &self.preview_state {
            PreviewState::Ready { preview, .. } => match &preview.body {
                PreviewBody::Lines(v) => v.len(),
                PreviewBody::Image(_) => 0,
            },
            _ => 0,
        }
    }

    pub fn refresh(&mut self) -> io::Result<()> {
        self.all_entries = scan_dir(&self.cwd, self.show_hidden, self.sort)?;
        self.apply_filter();
        self.refresh_parent();
        Ok(())
    }

    /// Recompute `entries` from `all_entries` + current filter and
    /// clamp the cursor so it doesn't dangle past the filtered length.
    ///
    /// Filter input is interpreted as a glob (case-insensitive). If
    /// the user typed something without any glob meta-chars, it's
    /// auto-wrapped as `*input*` so plain substring searches still
    /// behave like substring searches; power users can type real
    /// patterns like `*.rs` or `test_*.py`. Malformed globs fall
    /// back to a substring match so the user isn't locked out by a
    /// stray `[`.
    fn apply_filter(&mut self) {
        let pattern = self.filter.as_deref().filter(|s| !s.is_empty());
        let matcher = pattern.and_then(|raw| build_filter_matcher(raw));
        self.entries = match (pattern, matcher) {
            (None, _) => self.all_entries.clone(),
            (Some(_), Some(m)) => self
                .all_entries
                .iter()
                .filter(|e| m.is_match(&e.name))
                .cloned()
                .collect(),
            (Some(raw), None) => {
                // Glob compile failed — fall back to a case-insensitive
                // substring match on the raw text.
                let needle = raw.to_lowercase();
                self.all_entries
                    .iter()
                    .filter(|e| e.name.to_lowercase().contains(&needle))
                    .cloned()
                    .collect()
            }
        };
        if self.nav.cursor >= self.entries.len() {
            self.nav.end(self.entries.len());
        }
    }

    pub fn filter(&self) -> Option<&str> {
        self.filter.as_deref()
    }

    pub fn filter_input(&self) -> Option<&str> {
        self.filter_input.as_deref()
    }

    pub fn is_filter_input(&self) -> bool {
        self.filter_input.is_some()
    }

    pub fn take_pending_editor(&mut self) -> Option<PathBuf> {
        self.pending_editor.take()
    }

    /// Rescan the parent directory and locate the cursor row that
    /// matches `cwd`. Failures (no parent, EACCES, missing) silently
    /// produce an empty parent column — the rest of the UI keeps
    /// working.
    fn refresh_parent(&mut self) {
        let Some(parent) = self.cwd.parent() else {
            self.parent_entries.clear();
            self.parent_cursor = None;
            return;
        };
        match scan_dir(parent, self.show_hidden, self.sort) {
            Ok(list) => {
                let cursor = list.iter().position(|e| e.path == self.cwd);
                self.parent_entries = list;
                self.parent_cursor = cursor;
            }
            Err(_) => {
                self.parent_entries.clear();
                self.parent_cursor = None;
            }
        }
    }

    pub fn cd(&mut self, path: &Path) -> io::Result<()> {
        let target = path.canonicalize().unwrap_or_else(|_| path.to_path_buf());
        self.cwd = target;
        self.nav = Nav::default();
        self.refresh()
    }

    /// Compute the preview for the currently selected entry.
    /// - Cache hit → set state synchronously.
    /// - Cache miss → bump `preview_gen`, mark Loading, spawn a blocking
    ///   tokio task whose result lands on `preview_rx`. Stale results
    ///   are discarded by `drain_results()` via the generation counter.
    fn request_preview(&mut self) {
        let Some(entry) = self.selected().cloned() else {
            self.preview_state = PreviewState::None;
            self.preview_scroll = 0;
            return;
        };
        let path = entry.path.clone();
        // No-op: the visible preview is already for this path.
        if matches!(&self.preview_state, PreviewState::Ready { path: p, .. } | PreviewState::Loading(p) | PreviewState::Error { path: p, .. } if *p == path)
        {
            return;
        }
        // Path changed → reset preview scroll. Don't reset for in-pane
        // refresh of the same path (handled by the early-return above).
        self.preview_scroll = 0;
        if let Some(cached) = self.preview_cache.get(&path) {
            self.preview_state = PreviewState::Ready {
                path,
                preview: cached,
            };
            return;
        }
        self.preview_gen = self.preview_gen.wrapping_add(1);
        let gen = self.preview_gen;
        self.preview_state = PreviewState::Loading(path.clone());
        let tx = self.preview_tx.clone();
        let limits = self.preview_limits;
        let task_path = path.clone();
        self.rt.spawn_blocking(move || {
            let outcome = preview::render_blocking(&task_path, limits);
            // If the receiver is gone, the App was dropped — that's
            // fine, we just drop the result.
            let _ = tx.send(PreviewResult {
                generation: gen,
                path: task_path,
                outcome,
            });
        });
    }

    /// Drain any preview results that arrived since the last tick. Stale
    /// results (older generation) are dropped without touching state.
    fn drain_results(&mut self) {
        loop {
            match self.preview_rx.try_recv() {
                Ok(result) => self.apply_result(result),
                Err(mpsc::TryRecvError::Empty) => break,
                Err(mpsc::TryRecvError::Disconnected) => break,
            }
        }
    }

    fn apply_result(&mut self, result: PreviewResult) {
        if result.generation != self.preview_gen {
            return;
        }
        match result.outcome {
            Ok(preview) => {
                let arc = Arc::new(preview);
                self.preview_cache.put(result.path.clone(), arc.clone());
                self.preview_state = PreviewState::Ready {
                    path: result.path,
                    preview: arc,
                };
            }
            Err(message) => {
                self.preview_state = PreviewState::Error {
                    path: result.path,
                    message,
                };
            }
        }
    }

    fn handle(&mut self, action: Action) -> bool {
        let prior_idx = self.nav.cursor;
        // Modal short-circuit: only quit, close, and scroll work
        // while the full-preview modal is open. Everything else is a
        // no-op so users can't accidentally `cd` away from underneath.
        if self.full_preview {
            match action {
                Action::Quit => return true,
                Action::Cancel | Action::ToggleFullPreview => self.full_preview = false,
                Action::MoveUp => self.scroll_preview(-1),
                Action::MoveDown => self.scroll_preview(1),
                Action::PageUp => {
                    let step = self.preview_viewport_h.max(1) as isize;
                    self.scroll_preview(-step);
                }
                Action::PageDown => {
                    let step = self.preview_viewport_h.max(1) as isize;
                    self.scroll_preview(step);
                }
                Action::Top => self.modal_scroll = 0,
                Action::Bottom => self.scroll_preview(isize::MAX),
                _ => {}
            }
            return false;
        }
        // Route vertical movement based on focus. Everything else
        // (Enter, ParentDir, Refresh, ToggleHidden, Top/Bottom) targets
        // the list pane regardless of focus — those have no obvious
        // meaning in the preview pane.
        let focused_preview = self.focus == Focus::Preview;
        match action {
            Action::Quit => return true,
            Action::Cancel => {
                // Cancel hierarchy: clear an applied filter first;
                // only quit if there's nothing to cancel. The modal
                // case is handled in the early-return above.
                if self.filter.is_some() {
                    self.filter = None;
                    self.apply_filter();
                } else {
                    return true;
                }
            }
            Action::ToggleFullPreview => {
                // Only enter the modal if there's something to look at.
                if matches!(self.preview_state, PreviewState::Ready { .. }) {
                    self.full_preview = true;
                    // Modal starts at the top of the file independent
                    // of where the side pane is scrolled — feels more
                    // like "expand to read" than "continue reading".
                    self.modal_scroll = 0;
                }
            }
            Action::ToggleFocus => {
                // Don't focus the preview if there's nothing to scroll.
                let can_focus_preview = self.preview_total_lines() > 0;
                self.focus = match self.focus {
                    Focus::List if can_focus_preview => Focus::Preview,
                    _ => Focus::List,
                };
            }
            Action::MoveUp if focused_preview => self.scroll_preview(-1),
            Action::MoveDown if focused_preview => self.scroll_preview(1),
            Action::PageUp if focused_preview => {
                let step = self.preview_viewport_h.max(1) as isize;
                self.scroll_preview(-step);
            }
            Action::PageDown if focused_preview => {
                let step = self.preview_viewport_h.max(1) as isize;
                self.scroll_preview(step);
            }
            Action::Top if focused_preview => self.preview_scroll = 0,
            Action::Bottom if focused_preview => self.scroll_preview(isize::MAX),
            Action::MoveUp => self.nav.move_by(-1, self.entries.len()),
            Action::MoveDown => self.nav.move_by(1, self.entries.len()),
            Action::PageUp => {
                let step = self.list_viewport_h.max(1) as isize;
                self.nav.move_by(-step, self.entries.len());
            }
            Action::PageDown => {
                let step = self.list_viewport_h.max(1) as isize;
                self.nav.move_by(step, self.entries.len());
            }
            Action::Top => self.nav.home(),
            Action::Bottom => self.nav.end(self.entries.len()),
            Action::EnterDir => {
                if let Some(entry) = self.selected() {
                    if entry.is_dir() {
                        let target = entry.path.clone();
                        let _ = self.cd(&target);
                    } else {
                        // Defer the editor handoff to the run loop —
                        // it owns the Terminal and can suspend it.
                        self.pending_editor = Some(entry.path.clone());
                    }
                }
            }
            Action::StartFilter => {
                // Re-entering filter mode preloads the input with the
                // current filter so the user can edit it.
                self.filter_input = Some(self.filter.clone().unwrap_or_default());
            }
            Action::ParentDir => {
                if let Some(parent) = self.cwd.parent().map(Path::to_path_buf) {
                    let _ = self.cd(&parent);
                }
            }
            Action::ToggleHidden => {
                self.show_hidden = !self.show_hidden;
                let _ = self.refresh();
            }
            Action::Refresh => {
                if let Some(p) = self.selected().map(|e| e.path.clone()) {
                    self.preview_cache.invalidate(&p);
                }
                let _ = self.refresh();
            }
            Action::Noop => {}
        }
        // Any movement / refresh / cd may have changed the selected
        // path. Re-request the preview unconditionally — request_preview
        // short-circuits if the path is unchanged.
        if self.nav.cursor != prior_idx
            || matches!(action, Action::EnterDir | Action::ParentDir | Action::Refresh)
        {
            // Cd and movement implicitly snap focus back to the list:
            // the preview the user was scrolling is gone.
            self.focus = Focus::List;
            self.request_preview();
        }
        false
    }

    /// Process a key while the `/` filter input is open. Always
    /// updates `filter` live so the visible list tracks each keystroke.
    pub fn handle_filter_key(&mut self, key: KeyEvent) -> bool {
        let Some(ref mut buf) = self.filter_input else {
            return false;
        };
        if key.modifiers.contains(KeyModifiers::CONTROL) {
            match key.code {
                KeyCode::Char('c') => {
                    self.filter_input = None;
                    self.filter = None;
                    self.apply_filter();
                }
                KeyCode::Char('u') => {
                    buf.clear();
                    self.filter = None;
                    self.apply_filter();
                }
                KeyCode::Char('w') => {
                    // Pop a "word" — last non-space run + the gap.
                    while buf.ends_with(' ') {
                        buf.pop();
                    }
                    while !buf.is_empty() && !buf.ends_with(' ') {
                        buf.pop();
                    }
                    let new = buf.clone();
                    self.filter = if new.is_empty() { None } else { Some(new) };
                    self.apply_filter();
                }
                _ => {}
            }
            return false;
        }
        match key.code {
            KeyCode::Esc => {
                // Cancel the filter entirely — both the input and any
                // applied filter are discarded.
                self.filter_input = None;
                self.filter = None;
                self.apply_filter();
            }
            KeyCode::Enter => {
                // Confirm: leave the filter applied, close the input.
                self.filter_input = None;
            }
            KeyCode::Backspace => {
                buf.pop();
                let new = buf.clone();
                self.filter = if new.is_empty() { None } else { Some(new) };
                self.apply_filter();
            }
            KeyCode::Char(c) => {
                buf.push(c);
                self.filter = Some(buf.clone());
                self.apply_filter();
            }
            _ => {}
        }
        false
    }

    fn scroll_preview(&mut self, delta: isize) {
        let total = self.preview_total_lines();
        // Route to whichever scroll state owns the active view. Side
        // pane and modal each track their own offset so scrolling one
        // doesn't move the other underneath.
        let target = if self.full_preview {
            &mut self.modal_scroll
        } else {
            &mut self.preview_scroll
        };
        if total == 0 {
            *target = 0;
            return;
        }
        // Keep at least one row of content on screen — clamp the top so
        // it can't pass `total - 1`. PgDn should land on "bottom of
        // file" rather than scrolling past it.
        let max_top = total.saturating_sub(1);
        let next = *target as isize + delta;
        *target = next.clamp(0, max_top as isize) as usize;
    }

    pub fn run<B: Backend + std::io::Write>(&mut self, terminal: &mut Terminal<B>) -> Result<()> {
        loop {
            self.drain_results();
            terminal.draw(|frame| {
                let area = frame.area();
                // Both panes share the same vertical chrome budget
                // (1 row action bar + 4 rows panel chrome: cap, top
                // border, floor, bottom border) — record the same
                // value for both viewports. While the modal is open,
                // override the preview viewport with the modal body
                // height so PgUp/PgDn step by the modal's screen-full.
                let pane_h = area.height.saturating_sub(1).saturating_sub(4) as usize;
                self.list_viewport_h = pane_h;
                self.preview_viewport_h = if self.full_preview {
                    let modal_h = (area.height as u32 * 9 / 10) as u16;
                    (modal_h.saturating_sub(4) as usize).max(1)
                } else {
                    pane_h
                };
                self.nav.ensure_visible(pane_h.max(1));
                // Re-derive the preview pane's x range so mouse
                // events arriving after this frame route correctly.
                let top = ratatui::layout::Rect::new(
                    area.x,
                    area.y,
                    area.width,
                    area.height.saturating_sub(1),
                );
                let rects = ui::split_main_columns(top);
                if let Some(p) = rects.get(2) {
                    self.preview_pane_x_start = p.x;
                    self.preview_pane_x_end = p.x.saturating_add(p.width);
                }
                ui::draw(self, frame, &self.theme);
            })?;
            if event::poll(Duration::from_millis(120))? {
                match event::read()? {
                    Event::Key(key) => {
                        if self.is_filter_input() {
                            self.handle_filter_key(key);
                        } else {
                            let action = map_key(key);
                            if self.handle(action) {
                                return Ok(());
                            }
                            // The Enter handler may have queued an
                            // editor open — drain it before the next
                            // draw so we don't render between key
                            // and editor handoff.
                            if let Some(path) = self.take_pending_editor() {
                                open_in_editor(terminal, &path)?;
                                self.preview_cache.invalidate(&path);
                                self.request_preview();
                                let _ = self.refresh();
                            }
                        }
                    }
                    Event::Mouse(m) => {
                        // 3 lines per wheel tick — matches how most
                        // terminal emulators report scroll velocity
                        // and feels natural for both list and prose.
                        let delta = match m.kind {
                            MouseEventKind::ScrollUp => -3,
                            MouseEventKind::ScrollDown => 3,
                            _ => 0,
                        };
                        if delta != 0 {
                            self.handle_mouse_scroll(m.column, delta);
                        }
                    }
                    _ => {}
                }
            }
        }
    }
}
