//! App state machine and run loop.
//!
//! Holds cwd + entries + nav cursor + preview state. Preview rendering
//! is offloaded to a tokio blocking task so file I/O and syntect
//! highlighting don't stall the run loop. A monotonic generation
//! counter discards stale results when the user has already moved on.

use std::collections::HashMap;
use std::io;
use std::path::{Path, PathBuf};
use std::sync::mpsc;
use std::sync::Arc;
use std::time::Duration;

use anyhow::Result;
use gtui::Theme;
use crossterm::event::{self, Event, KeyCode, KeyEvent, KeyModifiers, MouseEventKind};
use ratatui::backend::Backend;
use ratatui::Terminal;
use tokio::runtime::Runtime;

use crate::fs::entry::FsEntry;
use crate::fs::scan::{scan_dir, SortMode};
use crate::keys::{Action, BaseScope};
use gtui::Nav;
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

/// User-cyclable preview pane width. Hidden collapses the whole
/// pane (the list takes its space); the other steps map to weight
/// values in the miller-column split.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PreviewSize {
    Hidden,
    Small,
    Medium,
    Large,
}

impl PreviewSize {
    /// Miller-column weight for this size. 0 collapses the pane.
    pub fn weight(self) -> u16 {
        match self {
            PreviewSize::Hidden => 0,
            PreviewSize::Small => 2,
            PreviewSize::Medium => 4,
            PreviewSize::Large => 8,
        }
    }

    pub fn wider(self) -> Self {
        match self {
            PreviewSize::Hidden => PreviewSize::Small,
            PreviewSize::Small => PreviewSize::Medium,
            PreviewSize::Medium => PreviewSize::Large,
            PreviewSize::Large => PreviewSize::Large,
        }
    }

    pub fn narrower(self) -> Self {
        match self {
            PreviewSize::Large => PreviewSize::Medium,
            PreviewSize::Medium => PreviewSize::Small,
            PreviewSize::Small => PreviewSize::Hidden,
            PreviewSize::Hidden => PreviewSize::Hidden,
        }
    }
}

/// Suspend the TUI, run `$EDITOR` (default `nano`) on `path`, and
/// restore the TUI when the editor exits. Errors during teardown or
/// restore propagate; spawning the editor itself is best-effort —
/// most editors only fail in pathological setups (PATH unset etc.)
/// and we'd rather come back to a working browser than panic.
fn open_in_editor<B: Backend>(
    terminal: &mut ratatui::Terminal<B>,
    path: &Path,
    mouse_capture: bool,
) -> Result<()>
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
    execute!(backend, EnterAlternateScreen).ok();
    if mouse_capture {
        execute!(backend, EnableMouseCapture).ok();
    }
    // Ratatui's diff renderer assumes the alt-screen contents match
    // its prior buffer; the editor scribbled all over that, so force
    // a full redraw on the next tick.
    terminal.clear()?;
    Ok(())
}

/// Apply a keystroke to a single-line text buffer. Shared between
/// the rename/touch modals and (eventually) any future input.
/// Char keys append; Backspace pops; Ctrl-W deletes the trailing
/// word (matches the filter input's convention); Ctrl-U clears.
/// Unknown keys are ignored so the modal stays open and the user
/// can correct course.
fn edit_buffer(mut buf: String, modifiers: KeyModifiers, code: KeyCode) -> String {
    if modifiers.contains(KeyModifiers::CONTROL) {
        match code {
            KeyCode::Char('u') => buf.clear(),
            KeyCode::Char('w') => {
                while buf.ends_with(' ') {
                    buf.pop();
                }
                while !buf.is_empty() && !buf.ends_with(' ') {
                    buf.pop();
                }
            }
            _ => {}
        }
        return buf;
    }
    match code {
        KeyCode::Backspace => {
            buf.pop();
        }
        KeyCode::Char(c) => buf.push(c),
        _ => {}
    }
    buf
}

/// Detect a true bitmap protocol once at startup. `Sextant` is the
/// safe default — viuer's own block fallback is dimmer than our
/// sextant rasterizer, so we'd rather skip viuer than let it pick
/// that path.
///
/// Detection is two-tiered:
/// 1. viuer's runtime queries (DA1 for sixel, kitty graphics ping,
///    `$TERM_PROGRAM` for iTerm). Most accurate when they work,
///    but they all rely on either a query/response round-trip or
///    a local env var, so they often miss when SSH'd from a
///    capable terminal to a server with a generic `$TERM`.
/// 2. Environment-variable hints. Terminals like Ghostty, kitty,
///    WezTerm, and iTerm leave fingerprints (`KITTY_WINDOW_ID`,
///    `WEZTERM_PANE`, `GHOSTTY_RESOURCES_DIR`, `LC_TERMINAL`,
///    `TERM_PROGRAM`) that survive an SSH session — `LC_*` is
///    forwarded by default, the others if the user adds `SendEnv`
///    to their `~/.ssh/config`. When any of those are set we
///    optimistically pick Native; the `--image-backend sextant`
///    override is the escape hatch if escapes get stripped (tmux
///    on an old version, screen, etc.).
fn detect_image_backend() -> ImageBackend {
    let kitty = matches!(
        viuer::get_kitty_support(),
        viuer::KittySupport::Local | viuer::KittySupport::Remote
    );
    let iterm = viuer::is_iterm_supported();
    if kitty || iterm {
        return ImageBackend::Native;
    }
    if env_hints_native_protocol() {
        return ImageBackend::Native;
    }
    ImageBackend::Sextant
}

/// Inspect environment variables that capable terminals leave on a
/// session — including some that survive SSH. Conservative on
/// recognition (only known values) so we don't claim Native when a
/// random tool happens to set `$TERM_PROGRAM`.
fn env_hints_native_protocol() -> bool {
    fn lower(name: &str) -> Option<String> {
        std::env::var(name).ok().map(|s| s.to_ascii_lowercase())
    }
    // 1. Direct fingerprints — these are present only when the
    //    matching terminal launched our session.
    for var in ["KITTY_WINDOW_ID", "WEZTERM_PANE", "GHOSTTY_RESOURCES_DIR"] {
        if std::env::var_os(var).is_some() {
            return true;
        }
    }
    // 2. iTerm's `LC_TERMINAL` is the gold standard for SSH
    //    forwarding — `LC_*` is in OpenSSH's default `AcceptEnv`
    //    pattern, so the value flows from laptop to server with no
    //    config change.
    if let Some(v) = lower("LC_TERMINAL") {
        if v.contains("iterm") || v.contains("wezterm") {
            return true;
        }
    }
    // 3. `TERM_PROGRAM` is set locally by most modern terminals; it
    //    only survives SSH if the user adds `SendEnv TERM_PROGRAM`
    //    to `~/.ssh/config`, but when present it's authoritative.
    if let Some(v) = lower("TERM_PROGRAM") {
        if matches!(
            v.as_str(),
            "ghostty" | "kitty" | "iterm.app" | "wezterm" | "tabby" | "wayst"
        ) {
            return true;
        }
    }
    // 4. `$TERM` itself — some terminfos identify a kitty-capable
    //    surface (kitty itself sets `xterm-kitty`; some Ghostty
    //    setups use `xterm-ghostty`).
    if let Some(v) = lower("TERM") {
        if v.contains("kitty") || v.contains("ghostty") || v.contains("wezterm") {
            return true;
        }
    }
    false
}

/// Inner rect of a `BoxedPanel` that uses the bubble layout (the
/// default — both side preview pane and modal). Mirrors
/// `BoxedPanel::inner` for tall panels: cap row + top border + floor
/// row eat 3 rows from the top, bottom border eats 1, and side
/// borders each eat 1 column.
fn bubble_panel_inner(rect: ratatui::layout::Rect) -> ratatui::layout::Rect {
    if rect.height < 5 {
        // Falls back to flat layout for short panels — not relevant
        // to our normal pane sizes but keeps the helper safe.
        return ratatui::layout::Rect::new(
            rect.x.saturating_add(1),
            rect.y.saturating_add(1),
            rect.width.saturating_sub(2),
            rect.height.saturating_sub(2),
        );
    }
    ratatui::layout::Rect::new(
        rect.x.saturating_add(1),
        rect.y.saturating_add(3),
        rect.width.saturating_sub(2),
        rect.height.saturating_sub(4),
    )
}

/// Inner rect of a flat-layout `BoxedPanel` (used by the modal).
fn flat_panel_inner(rect: ratatui::layout::Rect) -> ratatui::layout::Rect {
    ratatui::layout::Rect::new(
        rect.x.saturating_add(1),
        rect.y.saturating_add(1),
        rect.width.saturating_sub(2),
        rect.height.saturating_sub(2),
    )
}

/// Compute the rect (in cells) where a native image should be
/// painted this frame. Returns `None` if the current preview isn't
/// a Ready image, the backend isn't Native, or the rect is degenerate.
fn compute_native_image_rect(app: &App, area: ratatui::layout::Rect) -> Option<ratatui::layout::Rect> {
    if app.image_backend() != ImageBackend::Native {
        return None;
    }
    match app.preview_state() {
        crate::preview::PreviewState::Ready { preview, .. } => {
            if !matches!(preview.body, crate::preview::PreviewBody::Image(_)) {
                return None;
            }
        }
        _ => return None,
    }
    let inner = if app.is_full_preview() {
        let modal_w = ((area.width as u32 * 9 / 10) as u16).max(20);
        let modal_h = ((area.height as u32 * 9 / 10) as u16).max(8);
        let modal_x = area.x + (area.width.saturating_sub(modal_w)) / 2;
        let modal_y = area.y + (area.height.saturating_sub(modal_h)) / 2;
        let modal = ratatui::layout::Rect::new(modal_x, modal_y, modal_w, modal_h);
        flat_panel_inner(modal)
    } else {
        let top = ratatui::layout::Rect::new(
            area.x,
            area.y,
            area.width,
            area.height.saturating_sub(1),
        );
        let rects = ui::split_main_columns(top, app.preview_size().weight());
        let pane = rects.get(2).copied()?;
        if pane.width == 0 {
            return None;
        }
        bubble_panel_inner(pane)
    };
    if inner.width == 0 || inner.height == 0 {
        return None;
    }
    Some(inner)
}

/// True when the key is a bare `g` (no modifiers) — the start of a
/// jump chord. The chord is intercepted *before* `map_key` so we can
/// retain `g` as the chord prefix without losing `gg` for top-of-list.
fn is_jump_prefix(key: KeyEvent) -> bool {
    matches!(key.code, KeyCode::Char('g'))
        && !key.modifiers.contains(KeyModifiers::CONTROL)
        && !key.modifiers.contains(KeyModifiers::ALT)
}

/// Recognize Ctrl-O / Ctrl-N as history navigation. Returns -1 for
/// back, +1 for forward, None otherwise. We don't use Tab/Shift-Tab
/// for history because Tab is already focus-toggle.
fn is_history_key(key: KeyEvent) -> Option<i8> {
    if !key.modifiers.contains(KeyModifiers::CONTROL) {
        return None;
    }
    match key.code {
        KeyCode::Char('o') => Some(-1),
        KeyCode::Char('n') => Some(1),
        _ => None,
    }
}

/// Resolve a `g`-chord bookmark character into a target path. The set
/// is intentionally small for v1; user-defined bookmarks via config
/// can layer on top later without breaking the resolver shape.
fn resolve_bookmark(c: char, cwd: &Path) -> Option<PathBuf> {
    match c {
        'h' => std::env::var_os("HOME").map(PathBuf::from),
        '/' => Some(PathBuf::from("/")),
        't' => Some(PathBuf::from("/tmp")),
        'r' => find_repo_root(cwd),
        _ => None,
    }
}

/// Walk up from `start` looking for a `.git` directory. Returns the
/// deepest ancestor that contains one — the conventional "repo root."
fn find_repo_root(start: &Path) -> Option<PathBuf> {
    let mut cur = Some(start);
    while let Some(dir) = cur {
        if dir.join(".git").exists() {
            return Some(dir.to_path_buf());
        }
        cur = dir.parent();
    }
    None
}

/// Run `git diff HEAD -- <path>` and return colored lines for the preview pane.
fn git_diff_lines(path: &Path, repo_root: &Path) -> Vec<ratatui::text::Line<'static>> {
    use ratatui::style::{Color, Modifier, Style};
    use ratatui::text::{Line, Span};

    let out = std::process::Command::new("git")
        .args(["diff", "HEAD", "--", path.to_string_lossy().as_ref()])
        .current_dir(repo_root)
        .output();

    let text = match out {
        Ok(o) => String::from_utf8_lossy(&o.stdout).into_owned(),
        Err(e) => return vec![Line::from(format!("git diff failed: {e}"))],
    };

    if text.is_empty() {
        // File is untracked or added — show the full content as additions.
        let content = std::fs::read_to_string(path).unwrap_or_default();
        return content.lines().map(|l| {
            Line::from(Span::styled(
                format!("+{l}"),
                Style::default().fg(Color::Green),
            ))
        }).collect();
    }

    text.lines().map(|l| {
        let style = if l.starts_with('+') && !l.starts_with("+++") {
            Style::default().fg(Color::Green)
        } else if l.starts_with('-') && !l.starts_with("---") {
            Style::default().fg(Color::Red)
        } else if l.starts_with("@@") {
            Style::default().fg(Color::Cyan).add_modifier(Modifier::DIM)
        } else if l.starts_with("diff ") || l.starts_with("index ") {
            Style::default().add_modifier(Modifier::DIM)
        } else {
            Style::default()
        };
        Line::from(Span::styled(l.to_string(), style))
    }).collect()
}

/// Run `git status --porcelain` and `git rev-parse` in a background thread,
/// returning a `GitScanResult` with per-file status and branch info.
fn run_git_scan(cwd: &Path) -> GitScanResult {
    use std::collections::HashMap;
    // Find repo root.
    let root = {
        let out = std::process::Command::new("git")
            .args(["rev-parse", "--show-toplevel"])
            .current_dir(cwd)
            .output();
        match out {
            Ok(o) if o.status.success() => {
                let s = String::from_utf8_lossy(&o.stdout).trim().to_string();
                Some(PathBuf::from(s))
            }
            _ => return GitScanResult { repo_root: None, branch: None, is_dirty: false, files: HashMap::new() },
        }
    };
    let repo_root = root.unwrap();

    // Branch name.
    let branch = {
        let out = std::process::Command::new("git")
            .args(["rev-parse", "--abbrev-ref", "HEAD"])
            .current_dir(&repo_root)
            .output();
        out.ok()
            .filter(|o| o.status.success())
            .and_then(|o| String::from_utf8(o.stdout).ok())
            .map(|s| s.trim().to_string())
    };

    // File status via `git status --porcelain`.
    let mut files: HashMap<PathBuf, GitFileStatus> = HashMap::new();
    let out = std::process::Command::new("git")
        .args(["status", "--porcelain", "-u"])
        .current_dir(&repo_root)
        .output();
    if let Ok(o) = out {
        let text = String::from_utf8_lossy(&o.stdout);
        for line in text.lines() {
            if line.len() < 4 { continue; }
            let xy = line.get(..2).unwrap_or("  ");
            let x = xy.chars().next().unwrap_or(' '); // staged
            let y = xy.chars().nth(1).unwrap_or(' '); // unstaged
            let path_str = line[3..].trim_matches('"');
            // Handle renames: `old -> new` format
            let path_str = path_str.split(" -> ").last().unwrap_or(path_str);
            let abs = repo_root.join(path_str);
            let status = match (x, y) {
                ('R', _) | (_, 'R') => GitFileStatus::Renamed,
                ('A', _)            => GitFileStatus::Added,
                ('D', _) | (_, 'D') => GitFileStatus::Deleted,
                ('M', ' ') | ('M', '\0') => GitFileStatus::Staged,
                ('M', _) | (_, 'M') => GitFileStatus::Modified,
                ('?', '?')           => GitFileStatus::Untracked,
                _                    => GitFileStatus::Modified,
            };
            files.insert(abs, status);
        }
    }
    let is_dirty = !files.is_empty();
    GitScanResult { repo_root: Some(repo_root), branch, is_dirty, files }
}

/// Recursively walk `path`, counting files, subdirs, and total bytes.
/// Returns `(files, dirs, total_bytes, error)`. Best-effort — read
/// errors are accumulated and returned as a single joined message.
fn dir_scan(path: &std::path::Path) -> (u64, u64, u64, Option<String>) {
    let mut files = 0u64;
    let mut dirs = 0u64;
    let mut total_bytes = 0u64;
    let mut errors: Vec<String> = Vec::new();
    let mut stack = vec![path.to_path_buf()];
    while let Some(dir) = stack.pop() {
        let rd = match std::fs::read_dir(&dir) {
            Ok(r) => r,
            Err(e) => { errors.push(format!("{}: {e}", dir.display())); continue; }
        };
        for entry in rd.flatten() {
            let Ok(meta) = entry.metadata() else { continue };
            if meta.is_dir() {
                dirs += 1;
                stack.push(entry.path());
            } else {
                files += 1;
                total_bytes += meta.len();
            }
        }
    }
    let error = if errors.is_empty() { None } else { Some(errors.join("; ")) };
    (files, dirs, total_bytes, error)
}

/// Quote a SQL identifier for the query editor pre-fill SQL.
fn quote_db_ident(s: &str) -> String {
    let mut out = String::with_capacity(s.len() + 2);
    out.push('"');
    for ch in s.chars() {
        if ch == '"' { out.push('"'); }
        out.push(ch);
    }
    out.push('"');
    out
}

/// Handle a keypress in the SQL editor buffer of the query editor.
fn sql_edit_key(qe: &mut QueryEditorState, key: crossterm::event::KeyEvent) {
    use crossterm::event::{KeyCode, KeyModifiers};
    let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
    // Ensure sql buffer is never empty and cursor is in bounds.
    if qe.sql.is_empty() { qe.sql.push(String::new()); }
    let n_rows = qe.sql.len();
    if qe.cursor.0 >= n_rows { qe.cursor.0 = n_rows - 1; }
    if qe.cursor.1 > qe.sql[qe.cursor.0].chars().count() { qe.cursor.1 = qe.sql[qe.cursor.0].chars().count(); }
    let (row, col) = qe.cursor;

    match key.code {
        KeyCode::Up => {
            if row > 0 {
                let new_row = row - 1;
                let new_col = col.min(qe.sql[new_row].chars().count());
                qe.cursor = (new_row, new_col);
            }
        }
        KeyCode::Down => {
            if row + 1 < n_rows {
                let new_row = row + 1;
                let new_col = col.min(qe.sql[new_row].chars().count());
                qe.cursor = (new_row, new_col);
            }
        }
        KeyCode::Left => {
            if col > 0 {
                qe.cursor = (row, col - 1);
            } else if row > 0 {
                let prev_len = qe.sql[row - 1].chars().count();
                qe.cursor = (row - 1, prev_len);
            }
        }
        KeyCode::Right => {
            let line_len = qe.sql[row].chars().count();
            if col < line_len {
                qe.cursor = (row, col + 1);
            } else if row + 1 < n_rows {
                qe.cursor = (row + 1, 0);
            }
        }
        KeyCode::Home | KeyCode::Char('a') if ctrl => {
            qe.cursor = (row, 0);
        }
        KeyCode::End | KeyCode::Char('e') if ctrl => {
            let line_len = qe.sql[row].chars().count();
            qe.cursor = (row, line_len);
        }
        KeyCode::Char('k') if ctrl => {
            // Kill to end of line.
            let line = &mut qe.sql[row];
            let byte_pos: usize = line.char_indices().nth(col).map(|(b, _)| b).unwrap_or(line.len());
            line.truncate(byte_pos);
        }
        KeyCode::Enter => {
            let line = &qe.sql[row];
            let (before, after) = split_at_char(line, col);
            qe.sql[row] = before;
            qe.sql.insert(row + 1, after);
            qe.cursor = (row + 1, 0);
        }
        KeyCode::Backspace => {
            if col > 0 {
                let line = &mut qe.sql[row];
                let new_col = col - 1;
                remove_char(line, new_col);
                qe.cursor = (row, new_col);
            } else if row > 0 {
                let current = qe.sql.remove(row);
                let prev_len = qe.sql[row - 1].chars().count();
                qe.sql[row - 1].push_str(&current);
                qe.cursor = (row - 1, prev_len);
            }
        }
        KeyCode::Delete => {
            let line_len = qe.sql[row].chars().count();
            if col < line_len {
                remove_char(&mut qe.sql[row], col);
            } else if row + 1 < qe.sql.len() {
                let next = qe.sql.remove(row + 1);
                qe.sql[row].push_str(&next);
            }
        }
        KeyCode::Char(c) if !ctrl => {
            insert_char(&mut qe.sql[row], col, c);
            qe.cursor = (row, col + 1);
        }
        _ => {}
    }
    // Keep cursor visible — clamp both top and bottom.
    let vh = qe.editor_viewport_h.max(1);
    if qe.cursor.0 < qe.scroll_row {
        qe.scroll_row = qe.cursor.0;
    } else if qe.cursor.0 >= qe.scroll_row + vh {
        qe.scroll_row = qe.cursor.0 + 1 - vh;
    }
}

fn split_at_char(s: &str, char_idx: usize) -> (String, String) {
    let byte = s.char_indices().nth(char_idx).map(|(b, _)| b).unwrap_or(s.len());
    (s[..byte].to_string(), s[byte..].to_string())
}

fn remove_char(s: &mut String, char_idx: usize) {
    if let Some((byte, ch)) = s.char_indices().nth(char_idx) {
        s.drain(byte..byte + ch.len_utf8());
    }
}

fn insert_char(s: &mut String, char_idx: usize, c: char) {
    let byte = s.char_indices().nth(char_idx).map(|(b, _)| b).unwrap_or(s.len());
    s.insert(byte, c);
}

/// Push a SetTitle escape to the terminal so window managers / tab
/// strips show "gfb: <basename>". Best-effort — terminals that
/// don't honor the OSC sequence simply ignore it.
fn update_terminal_title(cwd: &Path) {
    let basename = cwd
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_else(|| cwd.display().to_string());
    let title = if basename.is_empty() {
        "gfb".to_string()
    } else {
        format!("gfb: {}", basename)
    };
    let _ = crossterm::execute!(std::io::stdout(), crossterm::terminal::SetTitle(&title));
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
    /// Tracks whether crossterm's mouse capture is currently enabled.
    /// Toggling capture off lets users use their terminal's native
    /// click-and-drag text selection (and thus copy) at the cost of
    /// in-app wheel scrolling.
    mouse_capture: bool,
    /// Set when the user triggers `Action::ToggleMouseCapture`. The
    /// run loop drains this after `handle()` returns so the crossterm
    /// command runs once per dispatch, with the new state matching
    /// `self.mouse_capture`.
    pending_mouse_toggle: bool,
    /// Active options menu (`O`). When `Some`, the modal grabs all
    /// keys until commit or cancel.
    pub(crate) options_menu: Option<gtui::widgets::OptionsMenu>,
    /// Theme name that was active when the options modal opened, so
    /// `Cancel` can revert any live preview the user did on the
    /// theme tab.
    pub(crate) options_original_theme: Option<String>,
    /// `?` toggles a centered help / keybind reference overlay.
    /// When `true`, the modal grabs all keys (any press closes it).
    pub(crate) show_help: bool,
    show_hidden: bool,
    sort: SortMode,
    /// Per-directory cursor memory. Whenever we leave a directory we
    /// stash the selected child's path here, keyed by the directory we
    /// left. On re-entry we look the entry up by path and restore the
    /// cursor to its row — so back/forward and `h`/`l` round-trips keep
    /// you on the same file. Indexed by path (not by row index) so the
    /// position survives sort changes, hidden-toggle, and concurrent
    /// fs changes.
    cursor_memory: HashMap<PathBuf, PathBuf>,
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
    /// User-cyclable size for the side preview pane (`[` / `]`).
    /// Default Medium; Hidden collapses the pane entirely so the
    /// list/parent get more horizontal room.
    preview_size: PreviewSize,
    /// PTY-backed shell hosted in the right preview pane while active.
    terminal: Option<gtui::TerminalSession>,
    terminal_scroll: usize,
    /// Inclusive-exclusive x range of the side preview pane, recorded
    /// by the renderer. Used to dispatch mouse-wheel events to the
    /// pane the cursor is over.
    preview_pane_x_start: u16,
    preview_pane_x_end: u16,

    /// `g` chord state — true when the user has pressed `g` and the
    /// next key resolves a bookmark or `gg`/`gG` movement.
    jump_pending: bool,
    /// Bounded back/forward history. `history_pos` points at the
    /// current cwd's slot — back decrements, forward increments.
    /// `cd()` truncates anything past `history_pos` before pushing,
    /// matching browser-history semantics.
    history: Vec<PathBuf>,
    history_pos: usize,
    /// Filesystem watcher kept alive for the lifetime of the App.
    /// Events arrive on `fs_rx`; the run loop drains it each tick
    /// and triggers `refresh()` if anything fired.
    _fs_watcher: Option<notify::RecommendedWatcher>,
    fs_rx: Option<mpsc::Receiver<()>>,
    /// Last mtime when we processed an FS event. Used to debounce
    /// bursts (cargo build can fire dozens of events in <10 ms).
    fs_dirty: bool,
    /// Backend to use for image previews. Detected once at startup;
    /// `Native` covers kitty / iTerm / sixel, anything else falls
    /// back to our sextant-block rasterizer.
    image_backend: ImageBackend,
    /// Tracks the most recently-painted native image so we only
    /// re-issue the escape codes when the path or rect changes.
    /// Without this, terminals like sixel would flicker on every
    /// frame.
    last_image_paint: Option<(PathBuf, ratatui::layout::Rect)>,
    /// Active input/confirm modal, if any. While Some, key events
    /// route through `handle_input_modal_key` instead of normal
    /// action dispatch.
    input_modal: Option<InputModal>,
    /// Recursive finder state. While Some, the list pane shows
    /// finder results instead of the cwd entries; key events route
    /// through `handle_finder_key` until Esc / Enter closes it.
    finder: Option<FinderState>,
    /// In-place editor state. While Some, the preview pane (or
    /// preview modal, if open) renders `EditableText` against this
    /// buffer and keystrokes route through the editor's handler.
    editor: Option<crate::editor::EditorState>,
    /// Transient status line shown in the action bar — used to flash
    /// op results like "renamed", "trashed", or error messages.
    /// Cleared after a few ticks so it doesn't linger.
    status_message: Option<String>,
    status_message_ticks: u8,
    /// Key dispatch stack. Today the modals (input prompts, editor,
    /// finder, filter) intercept keys via flag-based pre-checks in
    /// `run`; the stack only carries the always-on `BaseScope`.
    /// As each modal is migrated, it'll push a real `Scope` here so
    /// the cascade collapses into a single dispatch site.
    scopes: gtui::ScopeStack<Action>,
    /// Command palette state. `Some` while the `:` modal is open.
    command_palette: Option<CommandPaletteState>,

    /// Active view mode. Default [`ViewMode::Miller`] preserves the
    /// existing yazi-style three-pane layout exactly. Press `T` to
    /// flip into [`ViewMode::Tree`] (single tree pane on the left,
    /// dominant preview on the right). State below the line is only
    /// used in tree mode; toggling between modes preserves it.
    pub(crate) view_mode: ViewMode,
    /// Tree-mode state: the expanded-nodes set + cursor / scroll.
    /// Tree view stacks the cwd's filesystem children with each
    /// `--connect`'d database endpoint as siblings at depth 0; the
    /// expansion set keys on [`crate::sources::AnyNodeId`] so it
    /// persists per-source across re-flattens.
    pub(crate) tree_state: gtui::tree::TreeState<crate::sources::AnyNodeId>,
    /// Sort column for tree mode. Independent of `sort` (which is
    /// the miller-mode sort) so toggling between modes doesn't
    /// disrupt either.
    pub(crate) tree_sort: crate::tree::FbCol,
    pub(crate) tree_sort_descending: bool,
    /// Miller-column state for the dedicated database browser
    /// (`ViewMode::Database`). Each level is a sibling list: connections
    /// at the root, then databases / schemas / tables as the user drills
    /// in with →. The file browser uses the same three-pane Miller
    /// layout, so both modes feel like the same app.
    pub(crate) db_miller: DbMillerState,
    /// Database endpoints attached at startup via `--connect`.
    /// Empty vec means filesystem-only browsing — tree mode behaves
    /// exactly as it did pre-Phase-5.
    pub(crate) connections: Vec<crate::sources::ConnectionWorker>,
    /// Cached preview-row LiveTable for the currently-selected DB
    /// table. Replaced lazily on selection change. `None` while idle
    /// or when the selection is a filesystem entry.
    pub(crate) db_preview: Option<DbPreview>,
    /// Cache of `Connection::{databases,schemas,tables}` results.
    /// Survives across the flattens a single tree-mode keystroke
    /// triggers; cleared on `Refresh`. Cache misses now fire async
    /// requests against the worker thread.
    pub(crate) db_cache: crate::sources::multi::DbCache,
    /// Cache of `scan_dir` results. Same shape as `db_cache` but for
    /// filesystem readdir + stat calls.
    pub(crate) fs_cache: crate::sources::multi::FsCache,
    /// Reply channel: workers post `LoadResult` here when an async
    /// children query completes. The main loop drains the receiver
    /// after each event tick (see `drain_db_loads`).
    pub(crate) db_load_tx: std::sync::mpsc::Sender<crate::sources::LoadResult>,
    pub(crate) db_load_rx: std::sync::mpsc::Receiver<crate::sources::LoadResult>,
    /// SQL query editor state. `Some` while the editor is open.
    pub(crate) query_editor: Option<QueryEditorState>,
    /// Reply channel for async query execution results.
    pub(crate) query_result_tx: std::sync::mpsc::SyncSender<crate::sources::worker::QueryExecResult>,
    pub(crate) query_result_rx: std::sync::mpsc::Receiver<crate::sources::worker::QueryExecResult>,
    /// Info panel for the selected entry. `Some` while `i` is active.
    pub(crate) info_panel: Option<InfoPanel>,
    /// Reply channel for async directory-size scans.
    pub(crate) dir_info_tx: mpsc::Sender<DirInfoResult>,
    pub(crate) dir_info_rx: mpsc::Receiver<DirInfoResult>,
    /// Git integration state — branch, dirty flag, per-file status.
    pub(crate) git: GitState,
    /// Reply channel for async git scans.
    pub(crate) git_scan_tx: mpsc::Sender<GitScanResult>,
    pub(crate) git_scan_rx: mpsc::Receiver<GitScanResult>,
    /// Branch overlay (`B` key). `None` when closed.
    pub(crate) branch_overlay: Option<BranchOverlay>,
}

/// Per-file git status (most-visible status wins when both staged and
/// unstaged changes are present for the same file).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GitFileStatus {
    Modified,
    Added,
    Deleted,
    Renamed,
    Untracked,
    Staged,
}

/// Git state for the current working directory — populated asynchronously
/// by `request_git_scan()` and drained via `drain_git_scan()`.
#[derive(Debug, Clone, Default)]
pub struct GitState {
    pub repo_root: Option<PathBuf>,
    pub branch: Option<String>,
    pub is_dirty: bool,
    pub files: std::collections::HashMap<PathBuf, GitFileStatus>,
}

/// Result returned from the async git scan worker.
#[derive(Debug)]
pub(crate) struct GitScanResult {
    pub repo_root: Option<PathBuf>,
    pub branch: Option<String>,
    pub is_dirty: bool,
    pub files: std::collections::HashMap<PathBuf, GitFileStatus>,
}

/// State for the branch-list overlay (`B` key).
#[derive(Debug, Clone)]
pub struct BranchOverlay {
    pub branches: Vec<String>,
    pub current: Option<String>,
    pub selected: usize,
}

/// Schema-only preview for one DB table — no row data fetched.
#[derive(Debug, Clone)]
pub(crate) struct DbPreview {
    pub path: crate::sources::NodePath,
    pub columns: Vec<crate::sources::ColumnSpec>,
    /// Fast catalog-stats estimate; `None` when not cheaply available.
    pub row_count: Option<u64>,
}

/// One item visible inside a [`DbLevel`]. Mirrors the catalog tiers
/// — endpoint / database / schema / table — and carries enough to
/// reconstruct a [`crate::sources::NodePath`] for child loads,
/// queries, and drops.
#[derive(Debug, Clone)]
pub enum DbItem {
    Connection { root: usize, label: String },
    Database { root: usize, db: String },
    Schema { root: usize, db: String, schema: String },
    Table { root: usize, db: String, schema: String, table: String },
}

impl DbItem {
    pub fn label(&self) -> &str {
        match self {
            DbItem::Connection { label, .. } => label,
            DbItem::Database { db, .. } => db,
            DbItem::Schema { schema, .. } => schema,
            DbItem::Table { table, .. } => table,
        }
    }

    pub fn glyph(&self) -> &'static str {
        match self {
            DbItem::Connection { .. } => "⛁",
            DbItem::Database { .. } => "🗄",
            DbItem::Schema { .. } => "📂",
            DbItem::Table { .. } => "▦",
        }
    }

    pub fn is_expandable(&self) -> bool {
        !matches!(self, DbItem::Table { .. })
    }

    pub fn root(&self) -> usize {
        match self {
            DbItem::Connection { root, .. }
            | DbItem::Database { root, .. }
            | DbItem::Schema { root, .. }
            | DbItem::Table { root, .. } => *root,
        }
    }

    pub fn node_path(&self) -> crate::sources::NodePath {
        use crate::sources::NodePath;
        match self {
            DbItem::Connection { root, .. } => NodePath::endpoint(*root),
            DbItem::Database { root, db } => NodePath::database(*root, db),
            DbItem::Schema { root, db, schema } => NodePath::schema(*root, db, schema),
            DbItem::Table { root, db, schema, table } => {
                NodePath::table(*root, db, schema, table)
            }
        }
    }
}

/// What a [`DbLevel`] is showing. `Connections` is the root level
/// (siblings = every `--connect`'d endpoint); `Children` shows the
/// children of a specific catalog node.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DbLevelSource {
    Connections,
    Children {
        root: usize,
        path: crate::sources::NodePath,
    },
}

/// One Miller column in the DB browser. Items are derived on demand
/// from `connections` / `db_cache`; the level just tracks the cursor
/// plus the label of the item that opened it (used for the breadcrumb
/// without re-walking parents). Scroll is recomputed from cursor at
/// draw time, so it doesn't need to live on the level.
#[derive(Debug, Clone)]
pub struct DbLevel {
    pub source: DbLevelSource,
    pub parent_label: Option<String>,
    pub cursor: usize,
}

/// Miller-column state for the DB browser. Always has at least one
/// level (the connections list). Pushing a level corresponds to
/// drilling into the cursor's item; popping is `←` / `h`.
#[derive(Debug, Clone)]
pub struct DbMillerState {
    pub levels: Vec<DbLevel>,
}

impl DbMillerState {
    pub fn new() -> Self {
        Self {
            levels: vec![DbLevel {
                source: DbLevelSource::Connections,
                parent_label: None,
                cursor: 0,
            }],
        }
    }

    pub fn current(&self) -> &DbLevel {
        self.levels
            .last()
            .expect("DbMillerState always has at least one level")
    }

    pub fn current_mut(&mut self) -> &mut DbLevel {
        self.levels
            .last_mut()
            .expect("DbMillerState always has at least one level")
    }

    pub fn depth(&self) -> usize {
        self.levels.len()
    }
}

impl Default for DbMillerState {
    fn default() -> Self {
        Self::new()
    }
}

/// Project a `(root, NodePath, NodeData)` triple from the catalog
/// into a [`DbItem`] for the Miller browser.
pub(crate) fn db_item_from_path(
    root: usize,
    path: &crate::sources::NodePath,
    data: &crate::sources::NodeData,
) -> DbItem {
    use crate::sources::NodeKind;
    match path.level() {
        NodeKind::Endpoint => DbItem::Connection {
            root,
            label: data.label.clone(),
        },
        NodeKind::Database => DbItem::Database {
            root,
            db: path.database.clone().unwrap_or_default(),
        },
        NodeKind::Schema => DbItem::Schema {
            root,
            db: path.database.clone().unwrap_or_default(),
            schema: path.schema.clone().unwrap_or_default(),
        },
        NodeKind::Table => DbItem::Table {
            root,
            db: path.database.clone().unwrap_or_default(),
            schema: path.schema.clone().unwrap_or_default(),
            table: path.table.clone().unwrap_or_default(),
        },
    }
}

/// Content of a directory info scan — loaded asynchronously.
#[derive(Debug, Clone)]
pub enum DirContent {
    Calculating,
    Ready { files: u64, dirs: u64, total_bytes: u64 },
    Error(String),
}

/// Info panel state for the currently-selected entry.
#[derive(Debug, Clone)]
pub struct InfoPanel {
    pub name: String,
    pub path: std::path::PathBuf,
    pub kind: crate::fs::entry::EntryKind,
    pub size: u64,
    pub mtime: Option<std::time::SystemTime>,
    /// `None` for plain files; `Some` for directories (async scan).
    pub dir_content: Option<DirContent>,
}

/// Token returned from an async dir-info scan.
#[derive(Debug)]
pub(crate) struct DirInfoResult {
    pub path: std::path::PathBuf,
    pub files: u64,
    pub dirs: u64,
    pub total_bytes: u64,
    pub error: Option<String>,
}

/// Layout of the query pane (right side of DB mode).
/// Cycled by Tab: EditorOnly → Split → ResultsOnly → EditorOnly.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum QueryPaneLayout {
    /// Full right pane = SQL editor only.
    EditorOnly,
    /// Right pane split: editor top (~35%), results bottom (~65%).
    Split,
    /// Full right pane = results only.
    ResultsOnly,
}

/// Result state for the query editor pane.
#[derive(Debug, Clone)]
pub enum QueryResultState {
    None,
    Running,
    Ready {
        columns: Vec<String>,
        rows: Vec<crate::sources::db::Row>,
        elapsed_ms: u64,
    },
    Error(String),
}

/// State for the full-screen SQL query editor.
#[derive(Debug, Clone)]
pub struct QueryEditorState {
    /// Which `ConnectionWorker` to run queries against.
    pub conn_root: usize,
    /// SQL buffer lines.
    pub sql: Vec<String>,
    /// Cursor position (row, col in chars).
    pub cursor: (usize, usize),
    pub scroll_row: usize,
    /// Current right-pane layout. Tab cycles through variants.
    pub layout: QueryPaneLayout,
    pub result: QueryResultState,
    pub result_scroll: usize,
    /// Monotonic counter — stale results (from a previous run) are
    /// discarded when their gen doesn't match.
    pub query_gen: u64,
    /// Viewport height of the SQL editor pane from the last draw.
    /// Used to keep the cursor visible during editing.
    pub editor_viewport_h: usize,
    /// Horizontal column offset for the results table.
    pub col_scroll: usize,
}

/// Top-level view mode. Switched with `T` (cycles Miller ↔ Tree).
/// `Table` is auto-activated when the selection is 2-D data (DB table, CSV).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ViewMode {
    /// Three-pane miller layout: parent / current-list / preview.
    Miller,
    /// Three-pane layout with a tree in the center column instead of a
    /// flat list. Left parent column stays visible as the navigation
    /// anchor. `T` toggles between Miller and Tree.
    Tree,
    /// Two/three-pane layout where the center (and optionally right)
    /// pane shows a full table widget. Auto-activates on DB tables,
    /// CSV files, and other 2-D data; `T` exits back to Miller.
    #[allow(dead_code)]
    Table,
    /// Full-screen database browser. Shows only the connected DB
    /// endpoints — no filesystem entries. Toggle with `D`; Esc or
    /// `D` again returns to the previous file-browser mode.
    Database,
}

#[derive(Debug, Clone)]
pub enum InputModal {
    /// Rename `target` to `buffer`. Buffer is pre-filled with the
    /// current name when the modal opens.
    Rename { target: PathBuf, buffer: String },
    /// Create a new empty file at `cwd / buffer`.
    Touch { buffer: String },
    /// Confirm sending `target` to trash. Recoverable, but the user
    /// asked for a safety net so a slip on `d` doesn't reorganize
    /// their filesystem.
    ConfirmTrash { target: PathBuf },
    /// Confirm permanent deletion of `target` — `y` deletes, anything
    /// else cancels.
    ConfirmHardDelete { target: PathBuf },
    /// Confirm dropping a DB table or schema.
    ConfirmDropDbObject {
        root: usize,
        path: crate::sources::NodePath,
        cascade: bool,
    },
}

/// State for the command palette modal (opened with `:`).
#[derive(Debug, Clone)]
pub struct CommandPaletteState {
    pub input: String,
    pub selected: usize,
}

impl Default for CommandPaletteState {
    fn default() -> Self {
        Self { input: String::new(), selected: 0 }
    }
}

/// A single command shown in the palette suggestion list.
pub struct PaletteEntry {
    pub cmd: &'static str,
    pub description: &'static str,
}

/// Which rasterization path the renderer should use for image bodies.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ImageBackend {
    /// Unicode sextant blocks via our internal rasterizer. Always
    /// works; supported by every modern monospace font.
    Sextant,
    /// Native bitmap protocol (kitty, iTerm2, or sixel) via viuer.
    Native,
}

impl ImageBackend {
    pub fn label(self) -> &'static str {
        match self {
            ImageBackend::Sextant => "sextant",
            ImageBackend::Native => "native",
        }
    }
}

/// State for the recursive finder overlay.
#[derive(Debug, Clone)]
pub struct FinderState {
    pub input: String,
    pub results: Vec<crate::find::FindResult>,
    pub cursor: usize,
    pub scroll: usize,
}

impl FinderState {
    pub fn new() -> Self {
        Self {
            input: String::new(),
            results: Vec::new(),
            cursor: 0,
            scroll: 0,
        }
    }
}

impl Default for FinderState {
    fn default() -> Self {
        Self::new()
    }
}

/// User override for image backend selection. `Auto` uses runtime
/// detection; the others force-enable a path so the user can verify
/// what their terminal actually supports.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ImageBackendChoice {
    Auto,
    Sextant,
    Native,
}

impl App {
    /// Convenience constructor that lets the image backend auto-detect.
    /// Currently the binary always specifies a backend explicitly via
    /// `new_with`; this entry point is kept for symmetric API.
    #[allow(dead_code)]
    pub fn new(start: PathBuf, theme: Theme) -> io::Result<Self> {
        Self::new_with(start, theme, ImageBackendChoice::Auto, Vec::new())
    }

    pub fn new_with(
        start: PathBuf,
        theme: Theme,
        backend_choice: ImageBackendChoice,
        connections: Vec<crate::sources::ConnectionWorker>,
    ) -> io::Result<Self> {
        let cwd = start.canonicalize().unwrap_or(start);
        let rt = tokio::runtime::Builder::new_multi_thread()
            .worker_threads(2)
            .enable_all()
            .build()
            .map_err(|e| io::Error::other(format!("tokio: {e}")))?;
        let (preview_tx, preview_rx) = mpsc::channel();
        let mut app = Self {
            cwd: cwd.clone(),
            all_entries: Vec::new(),
            entries: Vec::new(),
            nav: Nav::default(),
            parent_entries: Vec::new(),
            parent_cursor: None,
            filter: None,
            filter_input: None,
            pending_editor: None,
            mouse_capture: false,
            pending_mouse_toggle: false,
            options_menu: None,
            options_original_theme: None,
            show_help: false,
            show_hidden: false,
            sort: SortMode::Name,
            cursor_memory: HashMap::new(),
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
            preview_size: PreviewSize::Medium,
            terminal: None,
            terminal_scroll: 0,
            preview_pane_x_start: 0,
            preview_pane_x_end: 0,
            jump_pending: false,
            history: vec![cwd.clone()],
            history_pos: 0,
            _fs_watcher: None,
            fs_rx: None,
            fs_dirty: false,
            image_backend: match backend_choice {
                ImageBackendChoice::Auto => detect_image_backend(),
                ImageBackendChoice::Sextant => ImageBackend::Sextant,
                ImageBackendChoice::Native => ImageBackend::Native,
            },
            last_image_paint: None,
            input_modal: None,
            finder: None,
            editor: None,
            status_message: None,
            status_message_ticks: 0,
            scopes: gtui::ScopeStack::new(Box::new(BaseScope)),
            command_palette: None,
            view_mode: ViewMode::Miller,
            tree_state: gtui::tree::TreeState::default(),
            tree_sort: crate::tree::FbCol::Name,
            tree_sort_descending: false,
            db_miller: DbMillerState::new(),
            connections,
            db_preview: None,
            db_cache: crate::sources::multi::new_db_cache(),
            fs_cache: crate::sources::multi::new_fs_cache(),
            db_load_tx: {
                let (tx, _) = std::sync::mpsc::channel();
                tx
            },
            db_load_rx: {
                // Replaced below — placeholder so the struct
                // initializer doesn't choke on a forward reference.
                let (_, rx) = std::sync::mpsc::channel();
                rx
            },
            query_editor: None,
            query_result_tx: {
                let (tx, _) = std::sync::mpsc::sync_channel(1);
                tx
            },
            query_result_rx: {
                let (_, rx) = std::sync::mpsc::sync_channel(1);
                rx
            },
            info_panel: None,
            dir_info_tx: { let (tx, _) = mpsc::channel(); tx },
            dir_info_rx: { let (_, rx) = mpsc::channel(); rx },
            git: GitState::default(),
            git_scan_tx: { let (tx, _) = mpsc::channel(); tx },
            git_scan_rx: { let (_, rx) = mpsc::channel(); rx },
            branch_overlay: None,
        };
        // Real channel pairs (the placeholders above get dropped
        // immediately). Keeping the struct-init shape simple — the
        // overhead of dropping unused channels is a couple allocations.
        let (load_tx, load_rx) = std::sync::mpsc::channel();
        app.db_load_tx = load_tx;
        app.db_load_rx = load_rx;
        let (query_tx, query_rx) = std::sync::mpsc::sync_channel(1);
        app.query_result_tx = query_tx;
        app.query_result_rx = query_rx;
        let (dir_info_tx, dir_info_rx) = mpsc::channel();
        app.dir_info_tx = dir_info_tx;
        app.dir_info_rx = dir_info_rx;
        let (git_tx, git_rx) = mpsc::channel();
        app.git_scan_tx = git_tx;
        app.git_scan_rx = git_rx;
        // Auto-expand each DB endpoint so the user immediately sees
        // its database list rather than a collapsed connection row.
        for i in 0..app.connections.len() {
            app.tree_state.expanded.insert(crate::sources::AnyNodeId::Db {
                root: i,
                path: crate::sources::NodePath::endpoint(i),
            });
        }
        app.start_watcher();
        app.refresh()?;
        app.request_git_scan();
        app.request_preview();
        update_terminal_title(&app.cwd);
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

    pub fn cwd(&self) -> &Path {
        &self.cwd
    }

    pub fn show_hidden(&self) -> bool {
        self.show_hidden
    }

    pub fn connections(&self) -> &[crate::sources::ConnectionWorker] {
        &self.connections
    }

    /// Sender for async DB-children loads. Cloned per-flatten and
    /// handed to the catalog so it can post results back when its
    /// worker reply lands.
    pub(crate) fn db_load_tx(&self) -> &std::sync::mpsc::Sender<crate::sources::LoadResult> {
        &self.db_load_tx
    }

    /// Drain any worker-thread `LoadResult`s that landed since the
    /// last tick; populate the cache and return whether a redraw is
    /// warranted (true when at least one result arrived).
    pub(crate) fn drain_db_loads(&mut self) -> bool {
        use crate::sources::multi::ChildrenState;
        let mut dirty = false;
        while let Ok(load) = self.db_load_rx.try_recv() {
            let state = match load.result {
                Ok(rows) => ChildrenState::Ready(rows),
                Err(e) => {
                    self.flash_status(format!("db: {e}"));
                    ChildrenState::Failed(format!("{e:#}"))
                }
            };
            self.db_cache.borrow_mut().insert(load.path, state);
            dirty = true;
        }
        dirty
    }

    pub fn db_preview(&self) -> Option<&DbPreview> {
        self.db_preview.as_ref()
    }

    // ── Query editor ──────────────────────────────────────────────────────

    pub fn is_query_editor_active(&self) -> bool {
        self.query_editor.is_some()
    }

    pub fn query_editor(&self) -> Option<&QueryEditorState> {
        self.query_editor.as_ref()
    }

    /// Open the SQL query editor, pre-filling SQL based on the currently
    /// selected DB node (table → SELECT *, otherwise empty).
    pub(crate) fn open_query_editor(&mut self) {
        let depth = self.db_miller.depth();
        let items = self.db_level_items(depth - 1);
        let cursor = self.db_miller.current().cursor;
        let (conn_root, pre_sql) = if let Some(item) = items.get(cursor) {
            match item {
                DbItem::Table { root, db, schema, table } => {
                    let sql = format!(
                        "SELECT *\nFROM {}.{}.{}\nLIMIT 100",
                        quote_db_ident(db),
                        quote_db_ident(schema),
                        quote_db_ident(table),
                    );
                    (*root, sql)
                }
                DbItem::Schema { root, schema, .. } => {
                    (*root, format!("-- schema: {schema}\n"))
                }
                DbItem::Database { root, .. } | DbItem::Connection { root, .. } => {
                    (*root, String::new())
                }
            }
        } else {
            (0, String::new())
        };
        let sql_lines: Vec<String> = if pre_sql.is_empty() {
            vec![String::new()]
        } else {
            pre_sql.lines().map(|l| l.to_string()).collect()
        };
        self.query_editor = Some(QueryEditorState {
            conn_root,
            cursor: (0, 0),
            scroll_row: 0,
            layout: QueryPaneLayout::EditorOnly,
            result: QueryResultState::None,
            result_scroll: 0,
            query_gen: 0,
            sql: sql_lines,
            editor_viewport_h: 10,
            col_scroll: 0,
        });
    }

    pub(crate) fn close_query_editor(&mut self) {
        self.query_editor = None;
    }

    /// Fire an async query against the current connection. The result
    /// arrives on `query_result_rx` and is drained each tick.
    pub(crate) fn run_query(&mut self) {
        let Some(qe) = self.query_editor.as_mut() else {
            let _ = std::fs::write("/tmp/gfb-debug.log", "run_query: no query editor\n");
            return;
        };
        let sql = qe.sql.join("\n");
        qe.query_gen += 1;
        let gen = qe.query_gen;
        qe.result = QueryResultState::Running;
        qe.result_scroll = 0;
        qe.col_scroll = 0;
        let conn_root = qe.conn_root;
        let _ = std::fs::write(
            "/tmp/gfb-debug.log",
            format!("run_query: conn={conn_root} gen={gen} sql={sql:?}\n"),
        );
        if let Some(conn) = self.connections.get(conn_root) {
            conn.exec_query_async(sql, gen, self.query_result_tx.clone());
        } else {
            let _ = std::fs::write("/tmp/gfb-debug.log", format!("run_query: no connection at index {conn_root}\n"));
        }
    }

    /// Drain any query results that arrived since the last tick.
    pub(crate) fn drain_query_results(&mut self) -> bool {
        let mut dirty = false;
        while let Ok(res) = self.query_result_rx.try_recv() {
            if let Some(qe) = self.query_editor.as_mut() {
                if res.gen == qe.query_gen {
                    qe.result = match res.error {
                        Some(msg) => {
                            let _ = std::fs::write("/tmp/gfb-query-error.log", &msg);
                            QueryResultState::Error(msg)
                        }
                        None => QueryResultState::Ready {
                            columns: res.columns,
                            rows: res.rows,
                            elapsed_ms: res.elapsed_ms,
                        },
                    };
                    // Auto-switch to Split so results are visible.
                    if matches!(qe.layout, QueryPaneLayout::EditorOnly) {
                        qe.layout = QueryPaneLayout::Split;
                    }
                    dirty = true;
                }
            }
        }
        dirty
    }

    // ── Info panel ────────────────────────────────────────────────────────

    pub fn info_panel(&self) -> Option<&InfoPanel> {
        self.info_panel.as_ref()
    }

    pub fn is_info_panel_active(&self) -> bool {
        self.info_panel.is_some()
    }

    /// Open the info panel for the currently selected entry. For
    /// directories, fires an async recursive scan for file count + size.
    pub(crate) fn open_info_panel(&mut self) {
        let Some(entry) = self.selected().cloned() else { return };
        let is_dir = entry.is_dir();
        let panel = InfoPanel {
            name: entry.name.clone(),
            path: entry.path.clone(),
            kind: entry.kind,
            size: entry.size,
            mtime: entry.mtime,
            dir_content: if is_dir { Some(DirContent::Calculating) } else { None },
        };
        self.info_panel = Some(panel);
        if is_dir {
            let tx = self.dir_info_tx.clone();
            let scan_path = entry.path.clone();
            self.rt.spawn_blocking(move || {
                let (files, dirs, total_bytes, error) = dir_scan(&scan_path);
                let _ = tx.send(DirInfoResult { path: scan_path, files, dirs, total_bytes, error });
            });
        }
    }

    pub(crate) fn close_info_panel(&mut self) {
        self.info_panel = None;
    }

    /// Drain any dir-info scan results that landed since the last tick.
    pub(crate) fn drain_dir_info(&mut self) -> bool {
        let mut dirty = false;
        while let Ok(res) = self.dir_info_rx.try_recv() {
            if let Some(panel) = self.info_panel.as_mut() {
                if panel.path == res.path {
                    panel.dir_content = Some(match res.error {
                        Some(e) => DirContent::Error(e),
                        None => DirContent::Ready {
                            files: res.files,
                            dirs: res.dirs,
                            total_bytes: res.total_bytes,
                        },
                    });
                    dirty = true;
                }
            }
        }
        dirty
    }

    // ── Git integration ───────────────────────────────────────────────────

    pub fn git_state(&self) -> &GitState {
        &self.git
    }

    pub fn branch_overlay(&self) -> Option<&BranchOverlay> {
        self.branch_overlay.as_ref()
    }

    pub fn is_branch_overlay_active(&self) -> bool {
        self.branch_overlay.is_some()
    }

    /// Spawn an async git status scan for the current cwd.
    pub(crate) fn request_git_scan(&mut self) {
        let cwd = self.cwd.clone();
        let tx = self.git_scan_tx.clone();
        self.rt.spawn_blocking(move || {
            let result = run_git_scan(&cwd);
            let _ = tx.send(result);
        });
    }

    /// Drain any completed git scan results.
    pub(crate) fn drain_git_scan(&mut self) -> bool {
        let mut dirty = false;
        while let Ok(res) = self.git_scan_rx.try_recv() {
            self.git = GitState {
                repo_root: res.repo_root,
                branch: res.branch,
                is_dirty: res.is_dirty,
                files: res.files,
            };
            dirty = true;
        }
        dirty
    }

    /// Open the branch overlay by running `git branch`.
    pub(crate) fn open_branch_overlay(&mut self) {
        let Some(ref root) = self.git.repo_root.clone() else { return };
        let output = std::process::Command::new("git")
            .args(["branch"])
            .current_dir(root)
            .output();
        let Ok(out) = output else { return };
        let text = String::from_utf8_lossy(&out.stdout);
        let mut current: Option<String> = None;
        let mut branches: Vec<String> = Vec::new();
        for line in text.lines() {
            let is_current = line.starts_with('*');
            let name = line.trim_start_matches(['*', ' ']).to_string();
            if is_current { current = Some(name.clone()); }
            branches.push(name);
        }
        // Put current branch first.
        if let Some(ref cur) = current {
            if let Some(pos) = branches.iter().position(|b| b == cur) {
                branches.remove(pos);
                branches.insert(0, cur.clone());
            }
        }
        self.branch_overlay = Some(BranchOverlay { branches, current, selected: 0 });
    }

    #[allow(dead_code)]
    pub(crate) fn close_branch_overlay(&mut self) {
        self.branch_overlay = None;
    }

    pub(crate) fn handle_branch_overlay_key(&mut self, key: crossterm::event::KeyEvent) {
        use crossterm::event::KeyCode;
        let Some(ov) = self.branch_overlay.as_mut() else { return };
        let n = ov.branches.len();
        match key.code {
            KeyCode::Char('j') | KeyCode::Down  => ov.selected = (ov.selected + 1).min(n.saturating_sub(1)),
            KeyCode::Char('k') | KeyCode::Up    => ov.selected = ov.selected.saturating_sub(1),
            KeyCode::Esc                         => { self.branch_overlay = None; }
            KeyCode::Enter => {
                let branch = {
                    let ov = self.branch_overlay.as_ref().unwrap();
                    ov.branches.get(ov.selected).cloned()
                };
                self.branch_overlay = None;
                if let Some(b) = branch {
                    if let Some(ref root) = self.git.repo_root.clone() {
                        let res = std::process::Command::new("git")
                            .args(["checkout", &b])
                            .current_dir(root)
                            .output();
                        match res {
                            Ok(o) if o.status.success() => {
                                self.flash_status(format!("switched to {b}"));
                                let _ = self.refresh();
                                self.request_git_scan();
                                self.request_preview();
                            }
                            Ok(o) => {
                                let msg = String::from_utf8_lossy(&o.stderr);
                                self.flash_status(format!("checkout failed: {}", msg.lines().next().unwrap_or("?")));
                            }
                            Err(e) => self.flash_status(format!("git: {e}")),
                        }
                    }
                }
            }
            _ => {}
        }
    }

    /// Apply behavior fields from a freshly-loaded `Config`. Called
    /// from `cli::run` after `new_with` so the construction path
    /// stays minimal. Connection URLs aren't applied here — the CLI
    /// layer owns the worker-spawn loop.
    pub(crate) fn apply_initial_config(&mut self, cfg: &crate::config::Config) {
        if let Some(b) = cfg.mouse_capture {
            self.mouse_capture = b;
        }
        if let Some(b) = cfg.show_hidden {
            self.show_hidden = b;
        }
        if let Some(s) = &cfg.sort {
            self.sort = match s.as_str() {
                "size" => SortMode::Size,
                "mtime" | "modified" => SortMode::Mtime,
                _ => SortMode::Name,
            };
        }
    }

    // ── Options menu ──────────────────────────────────────────────────────

    pub fn mouse_capture_enabled(&self) -> bool {
        self.mouse_capture
    }

    pub fn is_help_active(&self) -> bool {
        self.show_help
    }

    pub(crate) fn toggle_help(&mut self) {
        self.show_help = !self.show_help;
    }

    pub(crate) fn close_help(&mut self) {
        self.show_help = false;
    }

    pub fn is_options_active(&self) -> bool {
        self.options_menu.is_some()
    }

    pub fn options_menu(&self) -> Option<&gtui::widgets::OptionsMenu> {
        self.options_menu.as_ref()
    }

    /// Open the modal options menu populated from current App state +
    /// the on-disk config (used for connection list seeding).
    pub(crate) fn open_options(&mut self) {
        use crate::options::{BehaviorTab, BehaviorValues, ConnectionsTab, ThemeTab};
        use gtui::widgets::{OptionsMenu, OptionsTab};

        let theme_name = self.theme.name.clone();
        let theme_tab = ThemeTab::new(&theme_name);
        let behavior_tab = BehaviorTab::new(BehaviorValues {
            mouse_capture: self.mouse_capture,
            show_hidden: self.show_hidden,
            sort: self.sort,
        });
        let existing: Vec<String> = self
            .connections
            .iter()
            .map(|c| c.endpoint_label().to_string())
            .collect();
        let connections_tab = ConnectionsTab::new(existing);

        let tabs: Vec<Box<dyn OptionsTab>> = vec![
            Box::new(theme_tab),
            Box::new(behavior_tab),
            Box::new(connections_tab),
        ];
        let menu = OptionsMenu::new(" options ", tabs).with_size(64, 22);
        self.options_menu = Some(menu);
        self.options_original_theme = Some(theme_name);
    }

    pub(crate) fn close_options(&mut self) {
        self.options_menu = None;
        self.options_original_theme = None;
    }

    /// Dispatch a key to the options menu and react to its outcome.
    /// Returns whether the menu is still open.
    pub(crate) fn handle_options_key(&mut self, key: crossterm::event::KeyEvent) {
        use gtui::widgets::MenuOutcome;
        let Some(menu) = self.options_menu.as_mut() else { return };
        let outcome = menu.handle_key(key);
        // After every key, sync the theme preview from whatever the
        // theme tab currently has selected. Cheap — load_theme is just
        // a hashmap lookup and a few color reads.
        self.sync_theme_preview();
        // Drain any pending connection add staged by the tab. We do
        // it here (not in the event loop) so the work happens before
        // the next render and the new entry shows up in the list.
        self.drain_pending_connection_add();

        match outcome {
            MenuOutcome::Stay => {}
            MenuOutcome::Cancel => {
                self.cancel_options();
            }
            MenuOutcome::Commit => {
                self.commit_options();
            }
        }
    }

    fn sync_theme_preview(&mut self) {
        let Some(menu) = self.options_menu.as_ref() else { return };
        let Some(tab) = menu.active_tab() else { return };
        // Best-effort: only the theme tab exposes a current selection
        // we care about. We can't downcast through dyn OptionsTab, so
        // inspect by title — pluggable trait keeps tab types in this
        // crate so the comparison is stable.
        if tab.title() != "theme" {
            return;
        }
        // Re-walk the tabs to find the typed theme tab without dyn
        // downcasting: we built the menu so we know index 0 is theme.
        let tab_ref = match menu.tabs().first() {
            Some(t) => t,
            None => return,
        };
        // Use the trait method through a downcast-by-known-shape: we
        // also expose the typed theme tab via a tiny adapter. Cheap
        // workaround for not adding `Any` to the trait surface.
        let preview = crate::options::infer_theme_selection(tab_ref.as_ref());
        if let Some(name) = preview {
            if name != self.theme.name {
                self.theme = gtui::load_theme(&name);
            }
        }
    }

    fn drain_pending_connection_add(&mut self) {
        // Pull the URL out under a short borrow so we can then call
        // `self.flash_status` / `self.reload_db_catalog` without
        // overlapping it.
        let url = {
            let Some(menu) = self.options_menu.as_mut() else { return };
            let Some(tab) = menu.tabs_mut().get_mut(2) else { return };
            match crate::options::take_pending_add(tab.as_mut()) {
                Some(u) => u,
                None => return,
            }
        };
        // Spawn synchronously; ConnectionWorker::spawn already blocks
        // on the worker's open result and returns Err on connect
        // failure, which matches the UX we want here.
        match crate::sources::ConnectionWorker::spawn(url.clone(), None, false) {
            Ok(worker) => {
                self.connections.push(worker);
                self.flash_status(format!("connected: {url}"));
                if let Some(menu) = self.options_menu.as_mut() {
                    if let Some(tab) = menu.tabs_mut().get_mut(2) {
                        crate::options::finalize_add_on_tab(tab.as_mut(), url);
                    }
                }
                self.reload_db_catalog();
            }
            Err(e) => {
                let msg = format!("connect failed: {e}");
                self.flash_status(msg.clone());
                if let Some(menu) = self.options_menu.as_mut() {
                    if let Some(tab) = menu.tabs_mut().get_mut(2) {
                        crate::options::set_tab_error(tab.as_mut(), msg);
                    }
                }
            }
        }
    }

    fn cancel_options(&mut self) {
        if let Some(name) = self.options_original_theme.clone() {
            if name != self.theme.name {
                self.theme = gtui::load_theme(&name);
            }
        }
        self.flash_status("options cancelled");
        self.close_options();
    }

    fn commit_options(&mut self) {
        let Some(menu) = self.options_menu.as_ref() else { return };
        // Pull values via the same shape-based adapter used for the
        // theme preview, then close the menu before applying so any
        // status flashes / refreshes operate against the post-menu
        // App state.
        let theme_name = menu
            .tabs()
            .first()
            .and_then(|t| crate::options::infer_theme_selection(t.as_ref()))
            .unwrap_or_else(|| self.theme.name.clone());
        let behavior = menu
            .tabs()
            .get(1)
            .and_then(|t| crate::options::infer_behavior_values(t.as_ref()));
        let connections = menu
            .tabs()
            .get(2)
            .map(|t| crate::options::finalize_connections_from_tab(t.as_ref()))
            .unwrap_or_default();
        self.close_options();

        // Apply behavior changes.
        if let Some(values) = behavior {
            if values.mouse_capture != self.mouse_capture {
                self.mouse_capture = values.mouse_capture;
                self.pending_mouse_toggle = true;
            }
            if values.show_hidden != self.show_hidden {
                self.show_hidden = values.show_hidden;
            }
            if values.sort != self.sort {
                self.sort = values.sort;
            }
            // Re-scan so the new sort / hidden setting is visible.
            let _ = self.refresh();
        }

        // Theme is already live (sync_theme_preview kept it in sync).
        // Just ensure it matches the final commit value.
        if theme_name != self.theme.name {
            self.theme = gtui::load_theme(&theme_name);
        }

        // Persist to disk.
        let config = crate::config::Config {
            theme: Some(theme_name),
            mouse_capture: Some(self.mouse_capture),
            show_hidden: Some(self.show_hidden),
            sort: Some(match self.sort {
                SortMode::Name => "name".into(),
                SortMode::Size => "size".into(),
                SortMode::Mtime => "mtime".into(),
            }),
            connections,
        };
        match crate::config::save(&config) {
            Ok(p) => self.flash_status(format!("saved {}", p.display())),
            Err(e) => self.flash_status(format!("save failed: {e}")),
        }
    }

    /// Handle a keypress while the query editor is active.
    pub(crate) fn handle_query_editor_key(&mut self, key: crossterm::event::KeyEvent) {
        use crossterm::event::{KeyCode, KeyModifiers};
        let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
        // Global shortcuts work in all layout modes.
        match key.code {
            KeyCode::Esc => { self.close_query_editor(); return; }
            KeyCode::Tab => {
                if let Some(qe) = self.query_editor.as_mut() {
                    qe.layout = match qe.layout {
                        QueryPaneLayout::EditorOnly => QueryPaneLayout::Split,
                        QueryPaneLayout::Split => QueryPaneLayout::ResultsOnly,
                        QueryPaneLayout::ResultsOnly => QueryPaneLayout::EditorOnly,
                    };
                }
                return;
            }
            KeyCode::Enter if ctrl => { self.run_query(); return; }
            KeyCode::Char('r') if ctrl => { self.run_query(); return; }
            _ => {}
        }
        let Some(qe) = self.query_editor.as_mut() else { return };
        match qe.layout {
            QueryPaneLayout::ResultsOnly => {
                let max_cols = match &qe.result {
                    QueryResultState::Ready { columns, .. } => columns.len().saturating_sub(1),
                    _ => 0,
                };
                match key.code {
                    KeyCode::Char('j') | KeyCode::Down  => qe.result_scroll = qe.result_scroll.saturating_add(1),
                    KeyCode::Char('k') | KeyCode::Up    => qe.result_scroll = qe.result_scroll.saturating_sub(1),
                    KeyCode::PageDown                   => qe.result_scroll = qe.result_scroll.saturating_add(20),
                    KeyCode::PageUp                     => qe.result_scroll = qe.result_scroll.saturating_sub(20),
                    KeyCode::Right                      => qe.col_scroll = (qe.col_scroll + 1).min(max_cols),
                    KeyCode::Left                       => qe.col_scroll = qe.col_scroll.saturating_sub(1),
                    _ => {}
                }
            }
            QueryPaneLayout::EditorOnly | QueryPaneLayout::Split => {
                sql_edit_key(qe, key);
            }
        }
    }

    pub fn db_cache(&self) -> &crate::sources::multi::DbCache {
        &self.db_cache
    }

    pub fn fs_cache(&self) -> &crate::sources::multi::FsCache {
        &self.fs_cache
    }

    /// Drop the filesystem readdir cache. Called from `refresh()` —
    /// which itself fires on every fs-watcher tick — so the next
    /// flatten re-reads cwd's directory entries from disk.
    ///
    /// Crucially this does NOT touch `db_cache`. A filesystem event
    /// has no bearing on the database catalog, and dropping the DB
    /// cache here would make every save under cwd wipe the DB-mode
    /// panes back to "loading…" until the worker re-fetched every
    /// drilled-in level. Explicit DB refresh (`r` / `F5` in DB mode)
    /// resets `db_cache` directly.
    pub(crate) fn invalidate_tree_caches(&self) {
        self.fs_cache.borrow_mut().clear();
    }

    pub fn selected(&self) -> Option<&FsEntry> {
        self.entries.get(self.nav.cursor)
    }

    #[allow(dead_code)]
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

    pub fn preview_size(&self) -> PreviewSize {
        self.preview_size
    }

    pub fn terminal(&self) -> Option<&gtui::TerminalSession> {
        self.terminal.as_ref()
    }

    pub fn terminal_scroll(&self) -> usize {
        self.terminal_scroll
    }

    pub fn is_terminal_active(&self) -> bool {
        self.terminal.is_some()
    }

    fn toggle_terminal(&mut self) {
        if self.terminal.take().is_some() {
            self.terminal_scroll = 0;
            self.flash_status("terminal closed");
            return;
        }
        let rows = self.preview_viewport_h.max(1) as u16;
        let cols = self
            .preview_pane_x_end
            .saturating_sub(self.preview_pane_x_start)
            .saturating_sub(2)
            .max(1);
        match gtui::TerminalSession::spawn(&self.cwd, rows, cols) {
            Ok(session) => {
                self.terminal = Some(session);
                self.terminal_scroll = 0;
                if matches!(self.preview_size, PreviewSize::Hidden | PreviewSize::Small) {
                    self.preview_size = PreviewSize::Medium;
                }
                self.flash_status("terminal opened");
            }
            Err(e) => self.flash_status(format!("terminal: {e}")),
        }
    }

    fn drain_terminal(&mut self) -> bool {
        let Some(term) = self.terminal.as_mut() else {
            return false;
        };
        let dirty = term.drain();
        if dirty {
            self.terminal_scroll = term.line_count().saturating_sub(self.preview_viewport_h);
        }
        dirty
    }

    fn resize_terminal(&mut self, rows: u16, cols: u16) {
        if let Some(term) = self.terminal.as_mut() {
            term.resize(rows, cols);
        }
    }

    pub(crate) fn handle_terminal_key(&mut self, key: KeyEvent) -> bool {
        if matches!(key.code, KeyCode::Char('t')) && key.modifiers.is_empty() {
            self.toggle_terminal();
            return false;
        }
        if let Some(term) = self.terminal.as_mut() {
            term.send_key(key);
        }
        false
    }

    pub fn image_backend(&self) -> ImageBackend {
        self.image_backend
    }

    pub fn input_modal(&self) -> Option<&InputModal> {
        self.input_modal.as_ref()
    }

    pub fn finder(&self) -> Option<&FinderState> {
        self.finder.as_ref()
    }

    pub fn is_finder_active(&self) -> bool {
        self.finder.is_some()
    }

    pub fn db_miller(&self) -> &DbMillerState {
        &self.db_miller
    }

    /// Items visible in the DB-Miller level at `idx`. Connections come
    /// straight from `self.connections`; child levels read from
    /// `db_cache`. Returns an empty `Vec` while a child load is still
    /// in flight (cache state is `Loading`).
    pub(crate) fn db_level_items(&self, idx: usize) -> Vec<DbItem> {
        use crate::sources::multi::ChildrenState;
        let Some(level) = self.db_miller.levels.get(idx) else {
            return Vec::new();
        };
        match &level.source {
            DbLevelSource::Connections => self
                .connections
                .iter()
                .enumerate()
                .map(|(root, c)| DbItem::Connection {
                    root,
                    label: c.endpoint_label().to_string(),
                })
                .collect(),
            DbLevelSource::Children { root, path } => {
                let cache = self.db_cache.borrow();
                match cache.get(path) {
                    Some(ChildrenState::Ready(rows)) => rows
                        .iter()
                        .map(|(child_path, data)| db_item_from_path(*root, child_path, data))
                        .collect(),
                    _ => Vec::new(),
                }
            }
        }
    }

    /// Ensure the children of `(root, path)` are loaded (or in flight).
    /// Inserts a `Loading` placeholder and dispatches an async request
    /// to the worker if the cache is cold; no-op on hit.
    pub(crate) fn ensure_db_children_loaded(
        &self,
        root: usize,
        path: &crate::sources::NodePath,
    ) {
        use crate::sources::multi::ChildrenState;
        {
            let cache = self.db_cache.borrow();
            if cache.contains_key(path) {
                return;
            }
        }
        self.db_cache
            .borrow_mut()
            .insert(path.clone(), ChildrenState::Loading);
        if let Some(conn) = self.connections.get(root) {
            conn.request_children(path.clone(), self.db_load_tx.clone());
        }
    }

    pub fn is_command_palette_active(&self) -> bool {
        self.command_palette.is_some()
    }

    pub fn command_palette_state(&self) -> Option<&CommandPaletteState> {
        self.command_palette.as_ref()
    }

    pub fn palette_suggestions(&self) -> Vec<PaletteEntry> {
        let all = [
            PaletteEntry { cmd: "sort name", description: "sort by name" },
            PaletteEntry { cmd: "sort size", description: "sort by file size" },
            PaletteEntry { cmd: "sort modified", description: "sort by date modified" },
            PaletteEntry { cmd: "filter", description: "filter entries  e.g. filter *.rs" },
            PaletteEntry { cmd: "hidden", description: "toggle hidden files" },
            PaletteEntry { cmd: "refresh", description: "re-scan current directory" },
        ];
        let input = self.command_palette.as_ref()
            .map(|s| s.input.as_str())
            .unwrap_or("");
        if input.is_empty() {
            return all.into_iter().collect();
        }
        all.into_iter()
            .filter(|e| e.cmd.starts_with(input))
            .collect()
    }

    pub fn editor(&self) -> Option<&crate::editor::EditorState> {
        self.editor.as_ref()
    }

    pub fn is_editor_active(&self) -> bool {
        self.editor.is_some()
    }

    pub fn status_message(&self) -> Option<&str> {
        self.status_message.as_deref()
    }

    fn flash_status(&mut self, msg: impl Into<String>) {
        self.status_message = Some(msg.into());
        // ~1 second at 120 ms tick.
        self.status_message_ticks = 8;
    }

    fn tick_status(&mut self) {
        if self.status_message_ticks > 0 {
            self.status_message_ticks -= 1;
            if self.status_message_ticks == 0 {
                self.status_message = None;
            }
        }
    }

    /// Recorded by the renderer once it knows the preview pane rect.
    /// Mouse-wheel routing reads this back to tell which pane the user
    /// is hovering. Currently unused — mouse routing landed elsewhere.
    #[allow(dead_code)]
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
            if let Some(term) = self.terminal.as_ref() {
                let max_top = term.line_count().saturating_sub(1);
                let next = self.terminal_scroll as isize + delta;
                self.terminal_scroll = next.clamp(0, max_top as isize) as usize;
            } else {
                self.scroll_preview(delta);
            }
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
        self.invalidate_tree_caches();
        self.all_entries = scan_dir(&self.cwd, self.show_hidden, self.sort)?;
        self.apply_filter();
        self.refresh_parent();
        self.request_git_scan();
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
        if target == self.cwd {
            return Ok(());
        }
        // Truncate any forward history past the current position,
        // then push. Matches browser back/forward semantics: a new
        // navigation collapses the redo stack.
        self.history.truncate(self.history_pos + 1);
        self.history.push(target.clone());
        self.history_pos = self.history.len() - 1;
        // Cap to a reasonable depth so this doesn't grow unbounded
        // for users who like to roam.
        if self.history.len() > 64 {
            let drop = self.history.len() - 64;
            self.history.drain(0..drop);
            self.history_pos = self.history_pos.saturating_sub(drop);
        }
        self.cd_internal(target)
    }

    /// `cd` without touching the history stack. Used by back/forward
    /// navigation (which moves the pointer first, then jumps without
    /// disturbing siblings).
    fn cd_internal(&mut self, target: PathBuf) -> io::Result<()> {
        // Stash the cursor so a future return to `old_cwd` lands back
        // on the same entry. Skipped when the cwd has no selection
        // (empty dir or freshly opened) — there's nothing useful to
        // remember in that case.
        if let Some(sel) = self.entries.get(self.nav.cursor) {
            self.cursor_memory
                .insert(self.cwd.clone(), sel.path.clone());
        }
        self.cwd = target;
        self.nav = Nav::default();
        update_terminal_title(&self.cwd);
        self.rewatch();
        self.refresh()?;
        // Restore cursor by looking up the remembered child path in the
        // freshly scanned entries. Falls back to row 0 when the entry
        // disappeared (deleted, renamed, hidden by current filter).
        if let Some(remembered) = self.cursor_memory.get(&self.cwd).cloned() {
            if let Some(idx) = self.entries.iter().position(|e| e.path == remembered) {
                self.nav.cursor = idx;
            }
        }
        Ok(())
    }

    /// (Re-)create the filesystem watcher pointed at the current cwd.
    /// Dropping the previous watcher implicitly stops the old watch.
    fn start_watcher(&mut self) {
        use notify::{recommended_watcher, RecursiveMode, Watcher};
        let (tx, rx) = mpsc::channel();
        let watcher = recommended_watcher(move |res: notify::Result<notify::Event>| {
            // We don't care about the event details — any change in
            // the watched dir warrants a rescan. Errors are dropped
            // so a transient inotify issue doesn't kill the watcher.
            if res.is_ok() {
                let _ = tx.send(());
            }
        });
        match watcher {
            Ok(mut w) => match w.watch(&self.cwd, RecursiveMode::NonRecursive) {
                Ok(()) => {
                    self._fs_watcher = Some(w);
                    self.fs_rx = Some(rx);
                }
                Err(_) => {
                    self._fs_watcher = None;
                    self.fs_rx = None;
                }
            },
            Err(_) => {
                self._fs_watcher = None;
                self.fs_rx = None;
            }
        }
    }

    /// Tear down the old watcher and watch the new cwd. Called from
    /// `cd_internal` so every cd carries a fresh watch.
    fn rewatch(&mut self) {
        self._fs_watcher = None;
        self.fs_rx = None;
        self.start_watcher();
    }

    /// Drain any pending fs events and mark the dir for refresh.
    /// Returns true if a refresh is warranted.
    fn drain_fs_events(&mut self) -> bool {
        let Some(rx) = self.fs_rx.as_ref() else {
            return false;
        };
        let mut got = false;
        while rx.try_recv().is_ok() {
            got = true;
        }
        if got {
            self.fs_dirty = true;
        }
        got
    }

    fn history_back(&mut self) {
        if self.history_pos == 0 {
            return;
        }
        self.history_pos -= 1;
        let target = self.history[self.history_pos].clone();
        let _ = self.cd_internal(target);
    }

    fn history_forward(&mut self) {
        if self.history_pos + 1 >= self.history.len() {
            return;
        }
        self.history_pos += 1;
        let target = self.history[self.history_pos].clone();
        let _ = self.cd_internal(target);
    }

    pub fn jump_pending(&self) -> bool {
        self.jump_pending
    }

    /// Process the second key of a `g`-prefixed chord.
    pub fn handle_jump_key(&mut self, key: KeyEvent) {
        self.jump_pending = false;
        match key.code {
            // `gg` — top of list, vim convention.
            KeyCode::Char('g') => self.nav.home(),
            // `gG` — bottom; mirrors `G` direct.
            KeyCode::Char('G') => self.nav.end(self.entries.len()),
            KeyCode::Char(c) => {
                if let Some(path) = resolve_bookmark(c, &self.cwd) {
                    let _ = self.cd(&path);
                }
            }
            _ => {} // anything else cancels the chord silently.
        }
    }

    /// Compute the preview for the currently selected entry.
    /// - Cache hit → set state synchronously.
    /// - Cache miss → bump `preview_gen`, mark Loading, spawn a blocking
    ///   tokio task whose result lands on `preview_rx`. Stale results
    ///   are discarded by `drain_results()` via the generation counter.
    /// Preview the right thing for whichever view mode is active.
    /// Called from action handlers and the mode-toggle path.
    pub(crate) fn request_preview_for_active_mode(&mut self) {
        match self.view_mode {
            ViewMode::Miller | ViewMode::Table => self.request_preview(),
            ViewMode::Tree => self.request_tree_preview(),
            ViewMode::Database => {
                self.preview_state = PreviewState::None;
                self.preview_scroll = 0;
                self.on_db_cursor_changed();
            }
        }
    }

    /// Tree-mode preview: look up the entry at `tree_nav.cursor` in a
    /// freshly-flattened tree and queue a preview for its path. The
    /// flatten cost is bounded — only `cwd` plus expanded subdirs are
    /// scanned — and only fires when the cursor moves.
    /// Load (or refresh) the preview-rows cache for the DB node at
    /// the cursor. Only fires for `Table`-kind rows; expanding
    /// endpoints / databases / schemas is handled by tree
    /// expansion, not preview. Failures are reported via the status
    /// message and clear any prior preview.
    fn request_db_preview(&mut self, id: crate::sources::AnyNodeId) {
        let crate::sources::AnyNodeId::Db { root, path } = id else {
            self.db_preview = None;
            return;
        };
        if path.level() != crate::sources::NodeKind::Table {
            // Endpoints / databases / schemas don't have a row preview.
            self.db_preview = None;
            return;
        }
        if matches!(&self.db_preview, Some(p) if p.path == path) {
            return;
        }
        let Some(conn) = self.connections.get(root) else {
            self.db_preview = None;
            return;
        };
        let db = path.database.as_deref().unwrap_or_default();
        let sch = path.schema.as_deref().unwrap_or_default();
        let tbl = path.table.as_deref().unwrap_or_default();
        let columns = match conn.columns(db, sch, tbl) {
            Ok(c) => c,
            Err(e) => {
                self.db_preview = None;
                self.flash_status(format!("columns({tbl}) failed: {e}"));
                return;
            }
        };
        let row_count = conn.approximate_row_count(db, sch, tbl);
        self.db_preview = Some(DbPreview { path, columns, row_count });
    }

    fn request_tree_preview(&mut self) {
        let rows = crate::tree::flatten_tree(
            &self.cwd,
            &self.tree_state.expanded,
            self.tree_sort,
            self.tree_sort_descending,
            self.show_hidden,
            self.filter.as_deref(),
            &[],
            &self.db_cache,
            &self.fs_cache,
            Some(&self.db_load_tx),
        );
        let Some(row) = rows.get(self.tree_state.nav.cursor).cloned() else {
            self.preview_state = PreviewState::None;
            self.preview_scroll = 0;
            self.db_preview = None;
            return;
        };
        // DB selections route to the table-preview pipeline; the file
        // preview state stays as it was so flipping selection
        // back to a file doesn't cost a re-render. DB-table preview
        // load happens in `request_db_preview`.
        let entry = match &row.row {
            crate::sources::AnyRow::Fs(e) => e.clone(),
            crate::sources::AnyRow::Db(_) => {
                self.preview_state = PreviewState::None;
                self.preview_scroll = 0;
                self.request_db_preview(row.id.clone());
                return;
            }
        };
        // Filesystem-row path: same flow as before.
        self.db_preview = None;
        let path = entry.path;
        if matches!(&self.preview_state, PreviewState::Ready { path: p, .. } | PreviewState::Loading(p) | PreviewState::Error { path: p, .. } if *p == path)
        {
            return;
        }
        self.preview_scroll = 0;
        if let Some(cached) = self.preview_cache.get(&path) {
            self.preview_state = PreviewState::Ready { path, preview: cached };
            return;
        }
        self.preview_gen = self.preview_gen.wrapping_add(1);
        let gen = self.preview_gen;
        self.preview_state = PreviewState::Loading(path.clone());
        let tx = self.preview_tx.clone();
        let limits = self.preview_limits;
        let sort = self.sort;
        let show_hidden = self.show_hidden;
        let task_path = path.clone();
        self.rt.spawn_blocking(move || {
            let outcome = preview::render_blocking(&task_path, limits, sort, show_hidden);
            let _ = tx.send(PreviewResult { generation: gen, path: task_path, outcome });
        });
    }

    /// Refresh state derived from the DB-Miller cursor. Non-table
    /// selections pre-load their children so the right pane can show
    /// the next level live. Table schema previews are opt-in — fired
    /// from `Action::EnterDir`, not from cursor moves — so just
    /// browsing through a table list doesn't trigger a network round
    /// trip per cursor tick. Whatever was previously displayed is
    /// cleared either way.
    fn on_db_cursor_changed(&mut self) {
        self.db_preview = None;
        let depth = self.db_miller.depth();
        let items = self.db_level_items(depth - 1);
        let cursor = self.db_miller.current().cursor;
        let Some(item) = items.get(cursor).cloned() else {
            return;
        };
        if item.is_expandable() {
            let path = item.node_path();
            self.ensure_db_children_loaded(item.root(), &path);
        }
    }

    /// Synchronously fetch the schema preview for the table item at
    /// the current DB-Miller cursor. No-op for non-table selections.
    /// Triggered explicitly by `Action::EnterDir` so cursor moves
    /// stay free.
    fn request_db_schema_preview(&mut self) {
        let depth = self.db_miller.depth();
        let items = self.db_level_items(depth - 1);
        let cursor = self.db_miller.current().cursor;
        let Some(item) = items.get(cursor).cloned() else {
            return;
        };
        let DbItem::Table { root, db, schema, table } = &item else {
            return;
        };
        let path = crate::sources::NodePath::table(*root, db, schema, table);
        if matches!(&self.db_preview, Some(p) if p.path == path) {
            return;
        }
        let Some(conn) = self.connections.get(*root) else {
            self.db_preview = None;
            return;
        };
        let columns = match conn.columns(db, schema, table) {
            Ok(c) => c,
            Err(e) => {
                self.db_preview = None;
                self.flash_status(format!("columns({table}) failed: {e}"));
                return;
            }
        };
        let row_count = conn.approximate_row_count(db, schema, table);
        self.db_preview = Some(DbPreview { path, columns, row_count });
    }

    fn handle_db_mode(&mut self, action: Action) -> bool {
        let depth = self.db_miller.depth();
        let items = self.db_level_items(depth - 1);
        let total = items.len();
        let prior_cursor = self.db_miller.current().cursor;
        let prior_depth = depth;
        match action {
            Action::Quit => return true,
            Action::Cancel => {
                self.db_preview = None;
                self.view_mode = ViewMode::Miller;
                self.request_preview();
                return false;
            }
            Action::MoveUp => {
                let new = self.db_miller.current().cursor.saturating_sub(1);
                self.db_miller.current_mut().cursor = new;
            }
            Action::MoveDown => {
                if total > 0 {
                    let new = (self.db_miller.current().cursor + 1).min(total - 1);
                    self.db_miller.current_mut().cursor = new;
                }
            }
            Action::PageUp => {
                let new = self.db_miller.current().cursor.saturating_sub(10);
                self.db_miller.current_mut().cursor = new;
            }
            Action::PageDown => {
                if total > 0 {
                    let new = (self.db_miller.current().cursor + 10).min(total - 1);
                    self.db_miller.current_mut().cursor = new;
                }
            }
            Action::Top => {
                self.db_miller.current_mut().cursor = 0;
            }
            Action::Bottom => {
                self.db_miller.current_mut().cursor = total.saturating_sub(1);
            }
            Action::EnterDir => {
                let Some(item) = items.get(prior_cursor).cloned() else {
                    return false;
                };
                if item.is_expandable() {
                    let path = item.node_path();
                    let label = item.label().to_string();
                    self.ensure_db_children_loaded(item.root(), &path);
                    self.db_miller.levels.push(DbLevel {
                        source: DbLevelSource::Children {
                            root: item.root(),
                            path,
                        },
                        parent_label: Some(label),
                        cursor: 0,
                    });
                    self.db_preview = None;
                    self.on_db_cursor_changed();
                } else {
                    // Leaf (table): explicit Enter loads the schema
                    // preview. Cursor moves alone don't trigger this —
                    // it's a deliberate fetch on a key press.
                    self.request_db_schema_preview();
                }
                return false;
            }
            Action::ParentDir => {
                if self.db_miller.levels.len() > 1 {
                    self.db_miller.levels.pop();
                    self.db_preview = None;
                    self.on_db_cursor_changed();
                }
                return false;
            }
            // `Rename` is `r` in the base keymap. File mode uses it to
            // rename an entry; DB mode has no rename semantics, so we
            // hijack the same key to mean "refresh the catalog" — much
            // more useful in this context, and matches what most DB
            // browsers map `r` to.
            Action::Refresh | Action::Rename => {
                self.reload_db_catalog();
                return false;
            }
            Action::Trash => {
                if let Some(item) = items.get(prior_cursor) {
                    match item {
                        DbItem::Table { root, db, schema, table } => {
                            self.input_modal = Some(InputModal::ConfirmDropDbObject {
                                root: *root,
                                path: crate::sources::NodePath::table(*root, db, schema, table),
                                cascade: false,
                            });
                        }
                        DbItem::Schema { root, db, schema } => {
                            self.input_modal = Some(InputModal::ConfirmDropDbObject {
                                root: *root,
                                path: crate::sources::NodePath::schema(*root, db, schema),
                                cascade: false,
                            });
                        }
                        _ => self.flash_status("drop: only tables and schemas can be dropped"),
                    }
                }
            }
            Action::StartEditor => {
                self.open_query_editor();
                return false;
            }
            Action::PreviewNarrower => {
                self.preview_size = self.preview_size.narrower();
                return false;
            }
            Action::PreviewWider => {
                self.preview_size = self.preview_size.wider();
                return false;
            }
            _ => {}
        }
        if self.db_miller.current().cursor != prior_cursor
            || self.db_miller.depth() != prior_depth
        {
            self.on_db_cursor_changed();
        }
        false
    }

    /// Tree-mode action dispatch — minimal v1 covering navigation,
    /// expand/collapse, sort cycling, and the modals that work
    /// without per-mode special-casing (filter, find, editor open).
    /// File ops (rename / trash / delete / touch) currently route
    /// through the miller-mode entry list and are best done from
    /// `Miller` mode for now — extending them to tree mode is a
    /// follow-up.
    fn handle_tree(&mut self, action: Action) -> bool {
        // One flatten covers cursor-clamp + every branch that needs
        // to peek at the row at the cursor. Mutations to
        // tree_state.expanded inside this fn invalidate the slice for
        // any later peek — but the action arms that mutate (EnterDir,
        // ParentDir's collapse path, refresh) don't read rows again
        // afterwards in the same call.
        let rows = crate::tree::flatten_tree(
            &self.cwd,
            &self.tree_state.expanded,
            self.tree_sort,
            self.tree_sort_descending,
            self.show_hidden,
            self.filter.as_deref(),
            &[],
            &self.db_cache,
            &self.fs_cache,
            Some(&self.db_load_tx),
        );
        let total = rows.len();
        let prior_cursor = self.tree_state.nav.cursor;
        match action {
            Action::Quit => return true,
            Action::Cancel => {
                if self.filter.is_some() {
                    self.filter = None;
                    self.apply_filter();
                } else {
                    return true;
                }
            }
            Action::MoveUp => {
                self.tree_state.nav.move_by(-1, total);
            }
            Action::MoveDown => {
                self.tree_state.nav.move_by(1, total);
            }
            Action::PageUp => {
                self.tree_state.nav.move_by(-10, total);
            }
            Action::PageDown => {
                self.tree_state.nav.move_by(10, total);
            }
            Action::Top => {
                self.tree_state.nav.home();
            }
            Action::Bottom => {
                self.tree_state.nav.end(total);
            }
            Action::EnterDir => {
                // On an expandable row (filesystem directory or DB
                // endpoint/database/schema): expand it and step the
                // cursor into the first child. On a leaf (file or DB
                // table): leave the cursor where it is — the preview
                // already shows the leaf's contents.
                let cursor = self.tree_state.nav.cursor;
                if let gtui::tree::ToggleOutcome::Expanded(_) =
                    self.tree_state.toggle_at(&rows, cursor)
                {
                    self.tree_state.nav.move_by(1, total + 1);
                }
            }
            Action::ParentDir => {
                // On an expanded row at the cursor: collapse it.
                // Otherwise jump cursor to the parent row, or
                // `cd ..` when on the filesystem root level.
                match self.tree_state.collapse_or_parent(&rows) {
                    gtui::tree::ParentOutcome::Collapsed(_)
                    | gtui::tree::ParentOutcome::JumpedToParent(_) => {}
                    gtui::tree::ParentOutcome::AtRoot
                    | gtui::tree::ParentOutcome::OutOfRange => {
                        // Depth-0 row (or empty list) — interpret as
                        // "cd to the parent of cwd."
                        if let Some(parent) = self.cwd.parent().map(Path::to_path_buf) {
                            self.tree_state.expanded.clear();
                            self.tree_state.nav = Nav::default();
                            let _ = self.cd(&parent);
                        }
                    }
                }
            }
            Action::ToggleHidden => {
                self.show_hidden = !self.show_hidden;
                let _ = self.refresh();
            }
            Action::Refresh => {
                let _ = self.refresh();
            }
            Action::ToggleFocus | Action::ToggleFullPreview => {
                // Tree mode has no focus split / full-preview modal —
                // the preview already takes ~70% of the screen.
            }
            Action::StartFilter => {
                self.filter_input = Some(self.filter.clone().unwrap_or_default());
            }
            Action::StartFind => {
                self.finder = Some(FinderState::new());
            }
            Action::StartEditor => {
                if let Some(row) = rows.get(self.tree_state.nav.cursor) {
                    if let crate::sources::AnyRow::Fs(ref e) = row.row {
                        if !e.is_dir() {
                            self.pending_editor = Some(e.path.clone());
                        }
                    } else {
                        self.flash_status("edit: only filesystem entries can be edited");
                    }
                }
            }
            Action::PreviewNarrower | Action::PreviewWider => {
                // Could resize the tree-vs-preview split — wire up
                // later. For v1 the split is fixed at 35/65.
            }
            Action::TreeCycleSortNext => {
                self.tree_sort = match self.tree_sort {
                    crate::tree::FbCol::Name => crate::tree::FbCol::Size,
                    crate::tree::FbCol::Size => crate::tree::FbCol::Modified,
                    crate::tree::FbCol::Modified => crate::tree::FbCol::Name,
                };
            }
            Action::TreeCycleSortPrev => {
                self.tree_sort = match self.tree_sort {
                    crate::tree::FbCol::Name => crate::tree::FbCol::Modified,
                    crate::tree::FbCol::Size => crate::tree::FbCol::Name,
                    crate::tree::FbCol::Modified => crate::tree::FbCol::Size,
                };
            }
            Action::TreeReverseSort => {
                self.tree_sort_descending = !self.tree_sort_descending;
            }
            Action::ToggleViewMode => {
                // Already handled in the outer dispatcher. Defensive
                // no-op here.
            }
            Action::ToggleTerminal => {
                // Already handled in the outer dispatcher.
            }
            // File-ops (Trash / HardDelete / Rename / Touch) currently
            // route through the miller-mode entries vector. Supporting
            // them in tree mode requires the modal flow to read from
            // the tree-row cursor — deferred to a follow-up.
            Action::Trash | Action::HardDelete | Action::Rename | Action::Touch => {
                self.status_message =
                    Some("file ops: switch to Miller mode (T) for now".to_string());
                self.status_message_ticks = 24;
            }
            Action::ShowInfo => {
                self.open_info_panel();
                return false;
            }
            Action::ToggleBranchOverlay => {
                self.open_branch_overlay();
                return false;
            }
            Action::Noop
            | Action::StartCommandPalette
            | Action::ToggleDatabaseMode
            | Action::ToggleMouseCapture
            | Action::OpenOptions
            | Action::ToggleHelp => {}
        }
        if self.tree_state.nav.cursor != prior_cursor {
            self.request_tree_preview();
        }
        false
    }

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

        // If the file has a git status, show the diff instead of file content.
        let is_git_changed = !entry.is_dir()
            && self.git.files.contains_key(&path)
            && self.git.repo_root.is_some();
        if is_git_changed {
            let repo_root = self.git.repo_root.clone().unwrap();
            let task_path = path.clone();
            self.rt.spawn_blocking(move || {
                let lines = git_diff_lines(&task_path, &repo_root);
                let outcome = Ok(crate::preview::Preview {
                    kind: crate::preview::PreviewKind::Text,
                    body: crate::preview::PreviewBody::Lines(lines),
                    source_lines: 0,
                    note: Some("git diff".to_string()),
                });
                let _ = tx.send(PreviewResult { generation: gen, path: task_path, outcome });
            });
            return;
        }

        let limits = self.preview_limits;
        let sort = self.sort;
        let show_hidden = self.show_hidden;
        let task_path = path.clone();
        self.rt.spawn_blocking(move || {
            let outcome = preview::render_blocking(&task_path, limits, sort, show_hidden);
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
        // Mode toggle handled before everything else so it works
        // regardless of which mode we're currently in. State for both
        // modes survives — toggling is non-destructive.
        if matches!(action, Action::ToggleViewMode) {
            self.view_mode = match self.view_mode {
                ViewMode::Miller => ViewMode::Tree,
                ViewMode::Tree => ViewMode::Miller,
                ViewMode::Table | ViewMode::Database => ViewMode::Miller,
            };
            self.request_preview_for_active_mode();
            return false;
        }
        if matches!(action, Action::ToggleTerminal) {
            self.toggle_terminal();
            return false;
        }
        if matches!(action, Action::StartCommandPalette) {
            self.command_palette = Some(CommandPaletteState::default());
            return false;
        }
        if matches!(action, Action::OpenOptions) {
            self.open_options();
            return false;
        }
        if matches!(action, Action::ToggleHelp) {
            self.toggle_help();
            return false;
        }
        if matches!(action, Action::ToggleMouseCapture) {
            self.mouse_capture = !self.mouse_capture;
            self.pending_mouse_toggle = true;
            if self.mouse_capture {
                self.flash_status("mouse capture on — pane wheel scroll, copy disabled");
            } else {
                self.flash_status("mouse capture off — drag to select / copy");
            }
            return false;
        }
        if matches!(action, Action::ToggleDatabaseMode) {
            self.view_mode = match self.view_mode {
                ViewMode::Database => {
                    self.db_preview = None;
                    ViewMode::Miller
                }
                _ => ViewMode::Database,
            };
            self.request_preview_for_active_mode();
            return false;
        }
        if matches!(self.view_mode, ViewMode::Database) {
            return self.handle_db_mode(action);
        }
        if matches!(self.view_mode, ViewMode::Tree) {
            return self.handle_tree(action);
        }
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
            Action::Trash => {
                if let Some(entry) = self.selected() {
                    self.input_modal = Some(InputModal::ConfirmTrash {
                        target: entry.path.clone(),
                    });
                }
            }
            Action::HardDelete => {
                if let Some(entry) = self.selected() {
                    self.input_modal = Some(InputModal::ConfirmHardDelete {
                        target: entry.path.clone(),
                    });
                }
            }
            Action::Rename => {
                if let Some(entry) = self.selected() {
                    self.input_modal = Some(InputModal::Rename {
                        target: entry.path.clone(),
                        buffer: entry.name.clone(),
                    });
                }
            }
            Action::Touch => {
                self.input_modal = Some(InputModal::Touch {
                    buffer: String::new(),
                });
            }
            Action::StartFind => {
                self.finder = Some(FinderState::new());
            }
            Action::PreviewWider => {
                let prev = self.preview_size;
                self.preview_size = self.preview_size.wider();
                if self.preview_size != prev {
                    self.flash_status(format!("preview {:?}", self.preview_size));
                }
            }
            Action::PreviewNarrower => {
                let prev = self.preview_size;
                self.preview_size = self.preview_size.narrower();
                if self.preview_size != prev {
                    self.flash_status(format!("preview {:?}", self.preview_size));
                }
            }
            Action::StartEditor => {
                // Only promote text-y previews into editing — opening
                // a 4 GB log or a binary in our buffer is a foot gun.
                let editable = matches!(
                    self.preview_state(),
                    PreviewState::Ready { preview, .. }
                        if matches!(
                            preview.kind,
                            crate::preview::PreviewKind::Text
                                | crate::preview::PreviewKind::Markdown
                                | crate::preview::PreviewKind::Empty
                        )
                );
                if !editable {
                    self.flash_status("edit: preview must be text");
                } else if let Some(entry) = self.selected().cloned() {
                    if entry.is_dir() {
                        self.flash_status("edit: cannot edit a directory");
                    } else {
                        match crate::editor::EditorState::open(&entry.path) {
                            Ok(state) => {
                                self.editor = Some(state);
                                self.focus = Focus::Preview;
                            }
                            Err(e) => self.flash_status(format!("edit: {}", e)),
                        }
                    }
                }
            }
            Action::ParentDir => {
                if let Some(parent) = self.cwd.parent().map(Path::to_path_buf) {
                    let _ = self.cd(&parent);
                }
            }
            Action::ToggleHidden => {
                self.show_hidden = !self.show_hidden;
                // Cached directory previews list a different set of
                // entries depending on this flag — drop them all so
                // the next preview matches the new policy.
                self.preview_cache.clear();
                let _ = self.refresh();
            }
            Action::Refresh => {
                if let Some(p) = self.selected().map(|e| e.path.clone()) {
                    self.preview_cache.invalidate(&p);
                }
                let _ = self.refresh();
            }
            // Tree-mode-specific actions are dispatched in
            // `handle_tree`; reaching them here means we're in
            // miller mode and they should be no-ops.
            Action::ShowInfo => {
                self.open_info_panel();
                return false;
            }
            Action::ToggleBranchOverlay => {
                self.open_branch_overlay();
                return false;
            }
            Action::ToggleViewMode
            | Action::ToggleTerminal
            | Action::TreeCycleSortNext
            | Action::TreeCycleSortPrev
            | Action::TreeReverseSort
            | Action::StartCommandPalette
            | Action::ToggleDatabaseMode
            | Action::ToggleMouseCapture
            | Action::OpenOptions
            | Action::ToggleHelp => {}
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

    pub fn is_input_modal(&self) -> bool {
        self.input_modal.is_some()
    }

    /// Compute the editor's viewport (rows × cols) based on whether
    /// the preview is currently in modal-fullscreen or side-pane.
    /// The widget knows the gutter width itself; we subtract a
    /// generous 6 cells for chrome (borders + gutter) so the
    /// horizontal scroll math still has headroom.
    pub fn editor_viewport(&self) -> (usize, usize) {
        let rows = self.preview_viewport_h.max(1);
        let cols = if self.full_preview {
            let term_w = crossterm::terminal::size().map(|s| s.0).unwrap_or(80);
            ((term_w as u32 * 9 / 10) as usize).saturating_sub(6)
        } else {
            (self
                .preview_pane_x_end
                .saturating_sub(self.preview_pane_x_start) as usize)
                .saturating_sub(6)
        };
        (rows, cols.max(1))
    }

    /// Process a key while the finder overlay is open.
    pub fn handle_finder_key(&mut self, key: KeyEvent) {
        let Some(mut finder) = self.finder.take() else {
            return;
        };
        match key.code {
            KeyCode::Esc => {
                // Drop the overlay; cursor on the underlying list
                // pane is preserved.
                return;
            }
            KeyCode::Enter => {
                // Open: cd to the selected result's parent and put
                // the list cursor on its row. Closes the overlay.
                if let Some(result) = finder.results.get(finder.cursor).cloned() {
                    let abs = self.cwd.join(&result.rel);
                    if result.is_dir {
                        let _ = self.cd(&abs);
                    } else if let Some(parent) = abs.parent() {
                        let _ = self.cd(parent);
                        if let Some(idx) = self.entries.iter().position(|e| e.path == abs) {
                            self.nav.jump_to(idx, self.entries.len());
                            self.request_preview();
                        }
                    }
                }
                return; // overlay closed
            }
            KeyCode::Up => {
                if finder.cursor > 0 {
                    finder.cursor -= 1;
                }
            }
            KeyCode::Down => {
                if finder.cursor + 1 < finder.results.len() {
                    finder.cursor += 1;
                }
            }
            other => {
                let prior = finder.input.clone();
                finder.input = edit_buffer(finder.input.clone(), key.modifiers, other);
                if finder.input != prior {
                    let cwd = self.cwd.clone();
                    let show_hidden = self.show_hidden;
                    finder.results = if finder.input.is_empty() {
                        Vec::new()
                    } else {
                        crate::find::search(
                            &cwd,
                            &finder.input,
                            show_hidden,
                            crate::find::FindLimits::default(),
                        )
                    };
                    finder.cursor = 0;
                    finder.scroll = 0;
                }
            }
        }
        // Auto-scroll so the cursor stays visible — cheap math, no
        // need to thread the viewport height back.
        let viewport_h = self.list_viewport_h.max(1);
        if finder.cursor < finder.scroll {
            finder.scroll = finder.cursor;
        } else if finder.cursor >= finder.scroll + viewport_h {
            finder.scroll = finder.cursor + 1 - viewport_h;
        }
        // Live-preview the highlighted result so the preview pane
        // stays in sync with what the user is browsing.
        let preview_path = finder
            .results
            .get(finder.cursor)
            .map(|r| self.cwd.join(&r.rel));
        self.finder = Some(finder);
        if let Some(path) = preview_path {
            self.request_preview_for(&path);
        }
    }

    /// Same as `request_preview` but targets an arbitrary path
    /// instead of the currently-selected list entry. Used by the
    /// finder to preview a result row without changing cwd.
    fn request_preview_for(&mut self, path: &Path) {
        if matches!(&self.preview_state, PreviewState::Ready { path: p, .. } | PreviewState::Loading(p) | PreviewState::Error { path: p, .. } if *p == *path)
        {
            return;
        }
        self.preview_scroll = 0;
        if let Some(cached) = self.preview_cache.get(&path.to_path_buf()) {
            self.preview_state = PreviewState::Ready {
                path: path.to_path_buf(),
                preview: cached,
            };
            return;
        }
        self.preview_gen = self.preview_gen.wrapping_add(1);
        let gen = self.preview_gen;
        self.preview_state = PreviewState::Loading(path.to_path_buf());
        let tx = self.preview_tx.clone();
        let limits = self.preview_limits;
        let sort = self.sort;
        let show_hidden = self.show_hidden;
        let task_path = path.to_path_buf();
        self.rt.spawn_blocking(move || {
            let outcome = preview::render_blocking(&task_path, limits, sort, show_hidden);
            let _ = tx.send(PreviewResult {
                generation: gen,
                path: task_path,
                outcome,
            });
        });
    }

    /// Process a key while an input/confirm modal is open. Returns
    /// false (never quits) — the run loop only quits via Action::Quit.
    fn handle_command_palette_key(&mut self, key: KeyEvent) {
        match key.code {
            KeyCode::Esc => {
                self.command_palette = None;
            }
            KeyCode::Enter => {
                let suggestions = self.palette_suggestions();
                let cmd = {
                    let state = match self.command_palette.as_ref() {
                        Some(s) => s,
                        None => return,
                    };
                    if !suggestions.is_empty() {
                        let sel = state.selected.min(suggestions.len() - 1);
                        suggestions[sel].cmd.to_string()
                    } else if !state.input.is_empty() {
                        state.input.clone()
                    } else {
                        self.command_palette = None;
                        return;
                    }
                };
                self.command_palette = None;
                self.execute_palette_command(&cmd);
            }
            KeyCode::Up => {
                if let Some(s) = self.command_palette.as_mut() {
                    s.selected = s.selected.saturating_sub(1);
                }
            }
            KeyCode::Down => {
                let max = self.palette_suggestions().len().saturating_sub(1);
                if let Some(s) = self.command_palette.as_mut() {
                    s.selected = (s.selected + 1).min(max);
                }
            }
            _ => {
                let new_input = {
                    let state = match self.command_palette.as_ref() {
                        Some(s) => s,
                        None => return,
                    };
                    edit_buffer(state.input.clone(), key.modifiers, key.code)
                };
                if let Some(s) = self.command_palette.as_mut() {
                    s.input = new_input;
                    s.selected = 0;
                }
            }
        }
    }

    fn execute_palette_command(&mut self, input: &str) {
        let parts: Vec<&str> = input.trim().splitn(2, ' ').collect();
        let cmd = parts.first().copied().unwrap_or("");
        let arg = parts.get(1).copied().unwrap_or("").trim();
        match cmd {
            "sort" => {
                self.sort = match arg {
                    "size" => SortMode::Size,
                    "modified" | "mtime" => SortMode::Mtime,
                    _ => SortMode::Name,
                };
                let _ = self.refresh();
            }
            "filter" => {
                self.filter = if arg.is_empty() { None } else { Some(arg.to_string()) };
                self.apply_filter();
            }
            "hidden" => {
                self.show_hidden = !self.show_hidden;
                let _ = self.refresh();
            }
            "refresh" => {
                let _ = self.refresh();
            }
            _ => {}
        }
    }

    pub fn handle_input_modal_key(&mut self, key: KeyEvent) {
        let Some(modal) = self.input_modal.take() else {
            return;
        };
        // Esc cancels every modal.
        if matches!(key.code, KeyCode::Esc) {
            return;
        }
        match modal {
            InputModal::ConfirmTrash { target } => match key.code {
                KeyCode::Char('y') | KeyCode::Char('Y') | KeyCode::Enter => {
                    self.run_trash(&target);
                }
                _ => {} // anything else cancels.
            },
            InputModal::ConfirmHardDelete { target } => match key.code {
                KeyCode::Char('y') | KeyCode::Char('Y') => {
                    self.run_hard_delete(&target);
                }
                _ => {} // anything else cancels.
            },
            InputModal::Rename { target, buffer } => {
                match key.code {
                    KeyCode::Enter => {
                        self.run_rename(&target, &buffer);
                    }
                    other => {
                        // Edit then re-stash; modal stays open until
                        // Enter or Esc.
                        let new_buf = edit_buffer(buffer, key.modifiers, other);
                        self.input_modal = Some(InputModal::Rename {
                            target,
                            buffer: new_buf,
                        });
                    }
                }
            }
            InputModal::Touch { buffer } => match key.code {
                KeyCode::Enter => {
                    self.run_touch(&buffer);
                }
                other => {
                    let new_buf = edit_buffer(buffer, key.modifiers, other);
                    self.input_modal = Some(InputModal::Touch { buffer: new_buf });
                }
            },
            InputModal::ConfirmDropDbObject { root, path, cascade } => match key.code {
                KeyCode::Char('y') | KeyCode::Char('Y') | KeyCode::Enter => {
                    self.run_drop_db_object(root, &path, cascade);
                }
                _ => {}
            },
        }
    }

    /// Reset the DB catalog cache and re-fire loads for every
    /// Children-level pane in the Miller stack so all visible panes
    /// repopulate. Used both by the explicit refresh action and by
    /// mutations (drop) that invalidate the cached catalog.
    fn reload_db_catalog(&mut self) {
        self.db_cache = crate::sources::multi::new_db_cache();
        self.db_preview = None;
        let sources: Vec<(usize, crate::sources::NodePath)> = self
            .db_miller
            .levels
            .iter()
            .filter_map(|l| match &l.source {
                DbLevelSource::Children { root, path } => Some((*root, path.clone())),
                _ => None,
            })
            .collect();
        for (root, path) in sources {
            self.ensure_db_children_loaded(root, &path);
        }
        self.on_db_cursor_changed();
    }

    fn run_drop_db_object(&mut self, root: usize, path: &crate::sources::NodePath, cascade: bool) {
        use crate::sources::NodeKind;
        let Some(conn) = self.connections.get(root) else {
            self.flash_status("drop: connection not found");
            return;
        };
        let db = path.database.as_deref().unwrap_or_default();
        let schema = path.schema.as_deref().unwrap_or_default();
        let table = path.table.as_deref();
        let label = match path.level() {
            NodeKind::Table => format!("{}.{}", schema, table.unwrap_or("?")),
            NodeKind::Schema => format!("{}", schema),
            _ => return,
        };
        match conn.drop_object(db, schema, table, cascade) {
            Ok(()) => {
                self.reload_db_catalog();
                self.flash_status(format!("dropped {label}"));
            }
            Err(e) => {
                // Sanitize: Postgres error messages include DETAIL/HINT
                // lines with embedded newlines that corrupt the action bar.
                let raw = format!("{e:#}");
                let first_line = raw.lines().next().unwrap_or(&raw).trim().to_string();
                let is_dependency = raw.contains("depend") || raw.contains("constraint");
                if is_dependency && !cascade {
                    // Re-open confirmation with cascade flag set.
                    self.input_modal = Some(InputModal::ConfirmDropDbObject {
                        root,
                        path: path.clone(),
                        cascade: true,
                    });
                } else {
                    self.flash_status(format!("drop failed: {first_line}"));
                }
            }
        }
    }

    fn run_rename(&mut self, target: &Path, new_name: &str) {
        if new_name.is_empty() || new_name.contains('/') {
            self.flash_status("rename: invalid name");
            return;
        }
        let parent = match target.parent() {
            Some(p) => p,
            None => {
                self.flash_status("rename: no parent");
                return;
            }
        };
        let dest = parent.join(new_name);
        if dest == target {
            return; // no-op.
        }
        if dest.exists() {
            self.flash_status("rename: target exists");
            return;
        }
        match std::fs::rename(target, &dest) {
            Ok(()) => {
                self.flash_status(format!("renamed → {}", new_name));
                self.preview_cache.invalidate(&target.to_path_buf());
                let _ = self.refresh();
                // Move cursor onto the renamed entry so it stays
                // selected after the dir re-sorts.
                if let Some(idx) = self.entries.iter().position(|e| e.path == dest) {
                    self.nav.jump_to(idx, self.entries.len());
                    self.request_preview();
                }
            }
            Err(e) => self.flash_status(format!("rename: {}", e)),
        }
    }

    fn run_touch(&mut self, name: &str) {
        if name.is_empty() || name.contains('/') {
            self.flash_status("touch: invalid name");
            return;
        }
        let dest = self.cwd.join(name);
        match std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&dest)
        {
            Ok(_) => {
                self.flash_status(format!("created {}", name));
                let _ = self.refresh();
                if let Some(idx) = self.entries.iter().position(|e| e.path == dest) {
                    self.nav.jump_to(idx, self.entries.len());
                    self.request_preview();
                }
            }
            Err(e) => self.flash_status(format!("touch: {}", e)),
        }
    }

    fn run_trash(&mut self, target: &Path) {
        let name = target
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_else(|| target.display().to_string());
        match trash::delete(target) {
            Ok(()) => {
                self.flash_status(format!("trashed {}", name));
                self.preview_cache.invalidate(&target.to_path_buf());
                let _ = self.refresh();
            }
            Err(e) => self.flash_status(format!("trash: {}", e)),
        }
    }

    fn run_hard_delete(&mut self, target: &Path) {
        let result = if target.is_dir() {
            std::fs::remove_dir_all(target)
        } else {
            std::fs::remove_file(target)
        };
        match result {
            Ok(()) => {
                self.flash_status("deleted");
                self.preview_cache.invalidate(&target.to_path_buf());
                let _ = self.refresh();
            }
            Err(e) => self.flash_status(format!("delete: {}", e)),
        }
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

    /// Issue the native image paint for the current frame if the
    /// (path, rect) pair changed since last frame. Errors are
    /// swallowed — a failed paint shouldn't kill the run loop, and
    /// if the protocol stops working mid-session the user can press
    /// `r` to refresh.
    fn maybe_paint_native_image(&mut self) -> Result<()> {
        let area = ratatui::layout::Rect::new(
            0,
            0,
            crossterm::terminal::size().map(|s| s.0).unwrap_or(80),
            crossterm::terminal::size().map(|s| s.1).unwrap_or(24),
        );
        let want_rect = compute_native_image_rect(self, area);
        let want_path = match (self.preview_state(), want_rect) {
            (crate::preview::PreviewState::Ready { path, .. }, Some(_)) => Some(path.clone()),
            _ => None,
        };
        let want = match (want_path, want_rect) {
            (Some(p), Some(r)) => Some((p, r)),
            _ => None,
        };
        if want == self.last_image_paint {
            return Ok(());
        }
        if let Some((path, rect)) = &want {
            let conf = viuer::Config {
                x: rect.x,
                y: rect.y as i16,
                width: Some(rect.width as u32),
                height: Some(rect.height as u32),
                absolute_offset: true,
                use_kitty: true,
                use_iterm: true,
                ..Default::default()
            };
            // print_from_file errors when the protocol isn't
            // actually supported; we already gated on detect, but
            // be defensive — drop on error rather than panic.
            let _ = viuer::print_from_file(path, &conf);
        }
        self.last_image_paint = want;
        Ok(())
    }

    pub fn run<B: Backend + std::io::Write>(&mut self, terminal: &mut Terminal<B>) -> Result<()> {
        loop {
            self.tick_status();
            if let Some(e) = self.editor.as_mut() {
                e.tick();
            }
            self.drain_results();
            // DB worker replies — populate the children cache for
            // any async load that landed since the last frame. A
            // returned `true` would normally mean "redraw", but the
            // unconditional `terminal.draw` below already happens
            // every tick so we don't need to gate on it.
            let _ = self.drain_db_loads();
            let _ = self.drain_query_results();
            let _ = self.drain_dir_info();
            let _ = self.drain_git_scan();
            let _ = self.drain_terminal();
            if self.drain_fs_events() {
                // The 120 ms event-poll tick acts as natural debounce
                // — a burst of inotify events (cargo build can fire
                // dozens) collapses into one refresh per tick.
                let _ = self.refresh();
                self.fs_dirty = false;
            }
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
                // Update query editor viewport height so scroll clamping works.
                if let Some(qe) = self.query_editor.as_mut() {
                    let content_h = area.height.saturating_sub(2); // minus breadcrumb + action bar
                    qe.editor_viewport_h = match qe.layout {
                        QueryPaneLayout::Split => (content_h * 35 / 100) as usize,
                        _ => content_h as usize,
                    };
                }
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
                let rects = ui::split_main_columns(top, self.preview_size.weight());
                if let Some(p) = rects.get(2) {
                    self.preview_pane_x_start = p.x;
                    self.preview_pane_x_end = p.x.saturating_add(p.width);
                    self.resize_terminal(p.height.saturating_sub(4), p.width.saturating_sub(2));
                }
                ui::draw(self, frame, &self.theme);
            })?;
            // After ratatui's draw flushes, paint the native image
            // on top (kitty / iTerm / sixel only). We only re-issue
            // when the (path, rect) changed since the last frame so
            // sixel doesn't flicker. The image cells in ratatui's
            // buffer stay as inert spaces — its diff renderer won't
            // touch them on the next frame, so the protocol's image
            // overlay persists between repaints.
            self.maybe_paint_native_image()?;
            if event::poll(Duration::from_millis(120))? {
                match event::read()? {
                    Event::Key(key) => {
                        if self.is_input_modal() {
                            // Quit shortcut still works while a modal
                            // is open so the user can always escape
                            // the app.
                            if matches!(key.code, KeyCode::Char('c'))
                                && key.modifiers.contains(KeyModifiers::CONTROL)
                            {
                                return Ok(());
                            }
                            self.handle_input_modal_key(key);
                        } else if self.is_editor_active() {
                            // Ctrl-Q is the universal app-quit while
                            // editor is open (Ctrl-C is intercepted
                            // by the editor for bracket matching etc.
                            // in future, but for now does nothing).
                            if matches!(key.code, KeyCode::Char('q'))
                                && key.modifiers.contains(KeyModifiers::CONTROL)
                            {
                                return Ok(());
                            }
                            let (rows, cols) = self.editor_viewport();
                            let outcome = self
                                .editor
                                .as_mut()
                                .map(|e| e.handle_key(key, rows, cols))
                                .unwrap_or(crate::editor::EditorOutcome::Continue);
                            if outcome == crate::editor::EditorOutcome::Close {
                                let path = self
                                    .editor
                                    .as_ref()
                                    .map(|e| e.path.clone());
                                self.editor = None;
                                if let Some(p) = path {
                                    // The buffer may have been saved
                                    // (or discarded). Drop the cached
                                    // preview so the side pane reads
                                    // from disk again.
                                    self.preview_cache.invalidate(&p);
                                    self.request_preview();
                                }
                            }
                        } else if self.is_finder_active() {
                            if matches!(key.code, KeyCode::Char('c'))
                                && key.modifiers.contains(KeyModifiers::CONTROL)
                            {
                                return Ok(());
                            }
                            self.handle_finder_key(key);
                        } else if self.is_help_active() {
                            if matches!(key.code, KeyCode::Char('c'))
                                && key.modifiers.contains(KeyModifiers::CONTROL)
                            {
                                return Ok(());
                            }
                            // Any key closes the help overlay — matches
                            // gtop's behavior and avoids special-casing.
                            self.close_help();
                        } else if self.is_options_active() {
                            if matches!(key.code, KeyCode::Char('c'))
                                && key.modifiers.contains(KeyModifiers::CONTROL)
                            {
                                return Ok(());
                            }
                            self.handle_options_key(key);
                        } else if self.is_terminal_active() {
                            if self.handle_terminal_key(key) {
                                return Ok(());
                            }
                        } else if self.is_filter_input() {
                            self.handle_filter_key(key);
                        } else if self.is_branch_overlay_active() {
                            if matches!(key.code, KeyCode::Char('c'))
                                && key.modifiers.contains(KeyModifiers::CONTROL)
                            {
                                return Ok(());
                            }
                            self.handle_branch_overlay_key(key);
                        } else if self.is_info_panel_active() {
                            // Any key closes the info panel.
                            if matches!(key.code, KeyCode::Char('c'))
                                && key.modifiers.contains(KeyModifiers::CONTROL)
                            {
                                return Ok(());
                            }
                            self.close_info_panel();
                        } else if self.is_query_editor_active() {
                            if matches!(key.code, KeyCode::Char('c'))
                                && key.modifiers.contains(KeyModifiers::CONTROL)
                            {
                                return Ok(());
                            }
                            self.handle_query_editor_key(key);
                        } else if self.is_full_preview() {
                            // Modal preempts chord/history routing —
                            // jump out of the special interceptors so
                            // navigation can't sneak past the modal.
                            if let Some(action) = self.scopes.dispatch(&key) {
                                if self.handle(action) {
                                    return Ok(());
                                }
                            }
                        } else if self.is_command_palette_active() {
                            if matches!(key.code, KeyCode::Char('c'))
                                && key.modifiers.contains(KeyModifiers::CONTROL)
                            {
                                return Ok(());
                            }
                            self.handle_command_palette_key(key);
                        } else if self.jump_pending() {
                            self.handle_jump_key(key);
                        } else if is_jump_prefix(key) {
                            // Start of a `g`-chord — wait for the
                            // second key. Cancellable by any non-
                            // matching key in `handle_jump_key`.
                            self.jump_pending = true;
                        } else if let Some(direction) = is_history_key(key) {
                            if direction < 0 {
                                self.history_back();
                            } else {
                                self.history_forward();
                            }
                            self.request_preview();
                        } else {
                            if let Some(action) = self.scopes.dispatch(&key) {
                                if self.handle(action) {
                                    return Ok(());
                                }
                            }
                            // The Enter handler may have queued an
                            // editor open — drain it before the next
                            // draw so we don't render between key
                            // and editor handoff.
                            if let Some(path) = self.take_pending_editor() {
                                open_in_editor(terminal, &path, self.mouse_capture)?;
                                self.preview_cache.invalidate(&path);
                                self.request_preview();
                                let _ = self.refresh();
                            }
                            if self.pending_mouse_toggle {
                                self.pending_mouse_toggle = false;
                                use crossterm::event::{
                                    DisableMouseCapture, EnableMouseCapture,
                                };
                                use crossterm::execute;
                                let backend = terminal.backend_mut();
                                if self.mouse_capture {
                                    execute!(backend, EnableMouseCapture).ok();
                                } else {
                                    execute!(backend, DisableMouseCapture).ok();
                                }
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
