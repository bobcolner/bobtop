//! In-place text editor — nano-level functionality.
//!
//! `EditorState` owns the buffer (a `Vec<String>` line buffer), the
//! cursor `(row, col)` in chars, and scroll offsets. The widget
//! (`gtui::EditableText`) renders these; this module only
//! mutates state in response to keystrokes.
//!
//! Ops covered: char insert, Backspace / Delete, newline, Tab (4-space
//! soft tab), arrow keys, Home / End, PgUp / PgDn, Ctrl-S save (atomic
//! via sibling tmp + rename), Ctrl-X exit with dirty-confirm. No undo,
//! selection, or search yet.
//!
//! Movement bindings are layered to survive Mac terminals that strip
//! Ctrl+arrow / Alt+arrow before delivery:
//!
//! * Readline-style (works everywhere): Ctrl-A / Ctrl-E line start/end,
//!   Ctrl-F / Ctrl-B / Ctrl-N / Ctrl-P char/line motion, Ctrl-T / Ctrl-G
//!   buffer top/bottom, Ctrl-W kill-word-back, Ctrl-K / Ctrl-U kill to
//!   EOL / BOL, Ctrl-D forward delete.
//! * PC-style (works on most Linux/Windows terminals): Ctrl-Home /
//!   Ctrl-End buffer top/bottom, Ctrl-Left / Ctrl-Right word jumps,
//!   Ctrl-Backspace / Ctrl-Delete word delete.
//! * Meta-style (works when "Option as Meta" is enabled in iTerm2 /
//!   Terminal.app): Alt-B / Alt-F / Alt-D word ops, Alt-< / Alt->
//!   buffer top/bottom, Alt-Home / Alt-End viewport top/bottom.
//!
//! Live syntax highlighting: a `Vec<Line<'static>>` cache parallel to
//! `lines` is rebuilt eagerly after every mutation via syntect. Files
//! over `MAX_HIGHLIGHT_LINES` skip the cache and fall through to the
//! widget's plain-text path so very large buffers stay responsive.

use std::io;
use std::path::{Path, PathBuf};

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use ratatui::text::{Line, Span};
use syntect::easy::HighlightLines;
use unicode_width::UnicodeWidthChar;

use crate::preview::highlight::{syntax_for_path, syntaxes, theme, to_ratatui};

const SOFT_TAB: &str = "    ";

/// Cap on file size for live highlighting. Above this we skip the cache
/// to keep keystroke latency bounded — syntect over the full buffer on
/// every mutation is a few ms / 5k lines, which is fine, but a 100k-line
/// log file would feel laggy. Plain rendering kicks in instead.
const MAX_HIGHLIGHT_LINES: usize = 5_000;

#[derive(Debug, Clone)]
pub struct EditorState {
    pub path: PathBuf,
    pub lines: Vec<String>,
    /// `(row, col)` — col is in chars, not bytes or cells.
    pub cursor: (usize, usize),
    pub scroll_row: usize,
    pub scroll_col: usize,
    pub dirty: bool,
    /// Flash status shown in the editor's bottom row.
    pub message: Option<String>,
    pub message_ttl: u8,
    /// Set when the user pressed Ctrl-X / Esc on a dirty buffer —
    /// the next key picks save / discard / cancel.
    pub quit_confirm: bool,
    /// Pre-styled lines parallel to `lines`. Empty when over the line
    /// cap, in which case the renderer falls back to plain text.
    pub highlighted: Vec<Line<'static>>,
}

/// What the run loop should do after `handle_key` returns.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EditorOutcome {
    /// Stay in the editor.
    Continue,
    /// Close the editor and return to the file list.
    Close,
}

impl EditorState {
    pub fn open(path: &Path) -> io::Result<Self> {
        let content = match std::fs::read_to_string(path) {
            Ok(s) => s,
            Err(e) if e.kind() == io::ErrorKind::NotFound => String::new(),
            Err(e) => return Err(e),
        };
        let mut lines: Vec<String> = if content.is_empty() {
            vec![String::new()]
        } else {
            content.split('\n').map(String::from).collect()
        };
        // `split('\n')` on "a\n" yields ["a", ""] which is what we want
        // (an empty trailing line). On "a" yields ["a"]. Both fine.
        if lines.is_empty() {
            lines.push(String::new());
        }
        let mut state = Self {
            path: path.to_path_buf(),
            lines,
            cursor: (0, 0),
            scroll_row: 0,
            scroll_col: 0,
            dirty: false,
            message: None,
            message_ttl: 0,
            quit_confirm: false,
            highlighted: Vec::new(),
        };
        state.rebuild_highlight();
        Ok(state)
    }

    pub fn name(&self) -> String {
        self.path
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_else(|| self.path.display().to_string())
    }

    pub fn flash(&mut self, msg: impl Into<String>) {
        self.message = Some(msg.into());
        self.message_ttl = 16; // ~2 s at 120 ms tick.
    }

    pub fn tick(&mut self) {
        if self.message_ttl > 0 {
            self.message_ttl -= 1;
            if self.message_ttl == 0 {
                self.message = None;
            }
        }
    }

    /// Mark the buffer as modified — bumps the file-dirty flag *and*
    /// triggers a syntax-highlight rebuild. Prefer this over flipping
    /// `self.dirty` directly so the two stay in sync.
    fn mark_modified(&mut self) {
        self.dirty = true;
        self.rebuild_highlight();
    }

    /// Re-run syntect over the entire buffer. Cheap for typical edit
    /// sizes; bypassed for files over `MAX_HIGHLIGHT_LINES`.
    pub fn rebuild_highlight(&mut self) {
        if self.lines.len() > MAX_HIGHLIGHT_LINES {
            self.highlighted.clear();
            return;
        }
        let syntax_set = syntaxes();
        let theme = theme();
        let syn = syntax_for_path(&self.path, syntax_set);
        let mut hl = HighlightLines::new(syn, theme);
        let mut out: Vec<Line<'static>> = Vec::with_capacity(self.lines.len());
        for raw in &self.lines {
            // syntect expects newline-terminated input for its line
            // grammars (multi-line constructs anchor on `\n`). We feed
            // each buffer line plus a synthetic newline.
            let mut buf = String::with_capacity(raw.len() + 1);
            buf.push_str(raw);
            buf.push('\n');
            let regions = match hl.highlight_line(&buf, syntax_set) {
                Ok(r) => r,
                Err(_) => {
                    // On failure (rare; syntect doesn't usually error
                    // mid-stream) fall back to a plain line so the
                    // editor stays usable.
                    out.push(Line::from(raw.clone()));
                    continue;
                }
            };
            let mut spans: Vec<Span<'static>> = Vec::with_capacity(regions.len());
            for (style, text) in regions {
                let stripped = text.trim_end_matches(|c| c == '\n' || c == '\r');
                if stripped.is_empty() {
                    continue;
                }
                spans.push(Span::styled(stripped.to_string(), to_ratatui(style)));
            }
            out.push(Line::from(spans));
        }
        self.highlighted = out;
    }

    /// Atomic save: write to a sibling temp file and rename. Avoids
    /// truncating the original if disk fills up mid-write.
    pub fn save(&mut self) -> io::Result<()> {
        let parent = self.path.parent().unwrap_or_else(|| Path::new("."));
        let mut tmp_name = self
            .path
            .file_name()
            .map(|s| s.to_os_string())
            .unwrap_or_default();
        tmp_name.push(".gfb.tmp");
        let tmp_path = parent.join(tmp_name);
        let content = self.lines.join("\n");
        std::fs::write(&tmp_path, content)?;
        std::fs::rename(&tmp_path, &self.path)?;
        self.dirty = false;
        Ok(())
    }

    /// Apply a key event. Returns whether to stay open or close.
    pub fn handle_key(&mut self, key: KeyEvent, viewport_rows: usize, viewport_cols: usize) -> EditorOutcome {
        // Dirty-quit confirm short-circuits everything else.
        if self.quit_confirm {
            self.quit_confirm = false;
            return match key.code {
                KeyCode::Char('y') | KeyCode::Char('Y') => match self.save() {
                    Ok(()) => EditorOutcome::Close,
                    Err(e) => {
                        self.flash(format!("save failed: {}", e));
                        EditorOutcome::Continue
                    }
                },
                KeyCode::Char('n') | KeyCode::Char('N') => EditorOutcome::Close,
                _ => {
                    self.flash("quit cancelled");
                    EditorOutcome::Continue
                }
            };
        }

        let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
        let alt = key.modifiers.contains(KeyModifiers::ALT);
        if ctrl {
            return match key.code {
                KeyCode::Char('s') => {
                    match self.save() {
                        Ok(()) => self.flash(format!("wrote {}", self.path.display())),
                        Err(e) => self.flash(format!("save failed: {}", e)),
                    }
                    EditorOutcome::Continue
                }
                KeyCode::Char('x') => {
                    if self.dirty {
                        self.quit_confirm = true;
                        self.flash("save changes?  y / n / any other: cancel");
                        EditorOutcome::Continue
                    } else {
                        EditorOutcome::Close
                    }
                }
                KeyCode::Char('c') => {
                    // Ctrl-C from inside the editor cancels any
                    // pending state but doesn't quit the app — the
                    // run loop already special-cases Ctrl-C at the
                    // top to escape the program if truly needed.
                    EditorOutcome::Continue
                }
                KeyCode::Home => {
                    self.cursor = (0, 0);
                    self.ensure_visible(viewport_rows, viewport_cols);
                    EditorOutcome::Continue
                }
                KeyCode::End => {
                    let last = self.lines.len().saturating_sub(1);
                    let col = self.lines[last].chars().count();
                    self.cursor = (last, col);
                    self.ensure_visible(viewport_rows, viewport_cols);
                    EditorOutcome::Continue
                }
                KeyCode::Left => {
                    self.move_word_left();
                    self.ensure_visible(viewport_rows, viewport_cols);
                    EditorOutcome::Continue
                }
                KeyCode::Right => {
                    self.move_word_right();
                    self.ensure_visible(viewport_rows, viewport_cols);
                    EditorOutcome::Continue
                }
                KeyCode::Backspace => {
                    self.delete_word_left();
                    self.ensure_visible(viewport_rows, viewport_cols);
                    EditorOutcome::Continue
                }
                // Readline-style movements — these reach the app
                // reliably even on macOS where Ctrl+arrow / Alt+arrow
                // are stripped by the terminal.
                KeyCode::Char('a') => {
                    self.cursor.1 = 0;
                    self.ensure_visible(viewport_rows, viewport_cols);
                    EditorOutcome::Continue
                }
                KeyCode::Char('e') => {
                    let row = self.cursor.0;
                    self.cursor.1 = self.lines[row].chars().count();
                    self.ensure_visible(viewport_rows, viewport_cols);
                    EditorOutcome::Continue
                }
                KeyCode::Char('f') => {
                    self.move_right();
                    self.ensure_visible(viewport_rows, viewport_cols);
                    EditorOutcome::Continue
                }
                KeyCode::Char('b') => {
                    self.move_left();
                    self.ensure_visible(viewport_rows, viewport_cols);
                    EditorOutcome::Continue
                }
                KeyCode::Char('n') => {
                    self.move_down();
                    self.ensure_visible(viewport_rows, viewport_cols);
                    EditorOutcome::Continue
                }
                KeyCode::Char('p') => {
                    self.move_up();
                    self.ensure_visible(viewport_rows, viewport_cols);
                    EditorOutcome::Continue
                }
                KeyCode::Char('d') => {
                    // Forward delete — same as Delete key.
                    self.delete_forward();
                    self.ensure_visible(viewport_rows, viewport_cols);
                    EditorOutcome::Continue
                }
                KeyCode::Char('w') => {
                    // Kill previous word (readline / emacs / nano).
                    self.delete_word_left();
                    self.ensure_visible(viewport_rows, viewport_cols);
                    EditorOutcome::Continue
                }
                KeyCode::Char('k') => {
                    // Kill from cursor to end of line; if already at
                    // EOL, swallow the line break.
                    let row = self.cursor.0;
                    let len = self.lines[row].chars().count();
                    if self.cursor.1 < len {
                        self.delete_range(self.cursor, (row, len));
                    } else if row + 1 < self.lines.len() {
                        let next = self.lines.remove(row + 1);
                        self.lines[row].push_str(&next);
                        self.mark_modified();
                    }
                    self.ensure_visible(viewport_rows, viewport_cols);
                    EditorOutcome::Continue
                }
                KeyCode::Char('u') => {
                    // Kill from cursor to start of line.
                    let row = self.cursor.0;
                    if self.cursor.1 > 0 {
                        self.delete_range((row, 0), self.cursor);
                    }
                    self.ensure_visible(viewport_rows, viewport_cols);
                    EditorOutcome::Continue
                }
                KeyCode::Char('t') => {
                    // Jump to top of buffer (Mac terminals strip
                    // Ctrl-Home so this gives users a path that
                    // actually arrives at the app).
                    self.cursor = (0, 0);
                    self.ensure_visible(viewport_rows, viewport_cols);
                    EditorOutcome::Continue
                }
                KeyCode::Char('g') => {
                    // Jump to bottom of buffer — symmetric with ^T.
                    let last = self.lines.len().saturating_sub(1);
                    let col = self.lines[last].chars().count();
                    self.cursor = (last, col);
                    self.ensure_visible(viewport_rows, viewport_cols);
                    EditorOutcome::Continue
                }
                KeyCode::Delete => {
                    self.delete_word_right();
                    self.ensure_visible(viewport_rows, viewport_cols);
                    EditorOutcome::Continue
                }
                _ => EditorOutcome::Continue,
            };
        }
        if alt {
            return match key.code {
                KeyCode::Home => {
                    // Jump cursor to the top of the visible viewport.
                    self.cursor.0 = self.scroll_row;
                    self.clamp_cursor();
                    self.ensure_visible(viewport_rows, viewport_cols);
                    EditorOutcome::Continue
                }
                KeyCode::End => {
                    // Bottom of visible viewport. `viewport_rows` is the
                    // body height, so the last visible row index is
                    // `scroll_row + viewport_rows - 1` clamped to EOF.
                    let last = self.lines.len().saturating_sub(1);
                    let target = self
                        .scroll_row
                        .saturating_add(viewport_rows.saturating_sub(1))
                        .min(last);
                    self.cursor.0 = target;
                    self.clamp_cursor();
                    self.ensure_visible(viewport_rows, viewport_cols);
                    EditorOutcome::Continue
                }
                // Alt-Left/Right also map to word jumps as a fallback —
                // not all terminals send Ctrl+arrow cleanly.
                KeyCode::Left => {
                    self.move_word_left();
                    self.ensure_visible(viewport_rows, viewport_cols);
                    EditorOutcome::Continue
                }
                KeyCode::Right => {
                    self.move_word_right();
                    self.ensure_visible(viewport_rows, viewport_cols);
                    EditorOutcome::Continue
                }
                KeyCode::Backspace => {
                    self.delete_word_left();
                    self.ensure_visible(viewport_rows, viewport_cols);
                    EditorOutcome::Continue
                }
                // Readline-style word ops via Meta key — reach the app
                // when iTerm2 / Terminal.app have "Option as Meta key"
                // enabled.
                KeyCode::Char('b') => {
                    self.move_word_left();
                    self.ensure_visible(viewport_rows, viewport_cols);
                    EditorOutcome::Continue
                }
                KeyCode::Char('f') => {
                    self.move_word_right();
                    self.ensure_visible(viewport_rows, viewport_cols);
                    EditorOutcome::Continue
                }
                KeyCode::Char('d') => {
                    self.delete_word_right();
                    self.ensure_visible(viewport_rows, viewport_cols);
                    EditorOutcome::Continue
                }
                // Emacs-style buffer top / bottom — the Mac-friendly
                // equivalent of Ctrl-Home / Ctrl-End. `Alt-<` is
                // typed as Option-Shift-comma.
                KeyCode::Char('<') => {
                    self.cursor = (0, 0);
                    self.ensure_visible(viewport_rows, viewport_cols);
                    EditorOutcome::Continue
                }
                KeyCode::Char('>') => {
                    let last = self.lines.len().saturating_sub(1);
                    let col = self.lines[last].chars().count();
                    self.cursor = (last, col);
                    self.ensure_visible(viewport_rows, viewport_cols);
                    EditorOutcome::Continue
                }
                _ => EditorOutcome::Continue,
            };
        }

        match key.code {
            KeyCode::Esc => {
                if self.dirty {
                    self.quit_confirm = true;
                    self.flash("save changes?  y / n / any other: cancel");
                    EditorOutcome::Continue
                } else {
                    EditorOutcome::Close
                }
            }
            KeyCode::Enter => {
                self.insert_newline();
                self.ensure_visible(viewport_rows, viewport_cols);
                EditorOutcome::Continue
            }
            KeyCode::Tab => {
                for c in SOFT_TAB.chars() {
                    self.insert_char(c);
                }
                self.ensure_visible(viewport_rows, viewport_cols);
                EditorOutcome::Continue
            }
            KeyCode::Backspace => {
                self.backspace();
                self.ensure_visible(viewport_rows, viewport_cols);
                EditorOutcome::Continue
            }
            KeyCode::Delete => {
                self.delete_forward();
                self.ensure_visible(viewport_rows, viewport_cols);
                EditorOutcome::Continue
            }
            KeyCode::Left => {
                self.move_left();
                self.ensure_visible(viewport_rows, viewport_cols);
                EditorOutcome::Continue
            }
            KeyCode::Right => {
                self.move_right();
                self.ensure_visible(viewport_rows, viewport_cols);
                EditorOutcome::Continue
            }
            KeyCode::Up => {
                self.move_up();
                self.ensure_visible(viewport_rows, viewport_cols);
                EditorOutcome::Continue
            }
            KeyCode::Down => {
                self.move_down();
                self.ensure_visible(viewport_rows, viewport_cols);
                EditorOutcome::Continue
            }
            KeyCode::Home => {
                self.cursor.1 = 0;
                self.ensure_visible(viewport_rows, viewport_cols);
                EditorOutcome::Continue
            }
            KeyCode::End => {
                let row = self.cursor.0;
                self.cursor.1 = self.lines[row].chars().count();
                self.ensure_visible(viewport_rows, viewport_cols);
                EditorOutcome::Continue
            }
            KeyCode::PageUp => {
                let step = viewport_rows.max(1);
                self.cursor.0 = self.cursor.0.saturating_sub(step);
                self.clamp_cursor();
                self.ensure_visible(viewport_rows, viewport_cols);
                EditorOutcome::Continue
            }
            KeyCode::PageDown => {
                let step = viewport_rows.max(1);
                let last = self.lines.len().saturating_sub(1);
                self.cursor.0 = (self.cursor.0 + step).min(last);
                self.clamp_cursor();
                self.ensure_visible(viewport_rows, viewport_cols);
                EditorOutcome::Continue
            }
            KeyCode::Char(c) => {
                self.insert_char(c);
                self.ensure_visible(viewport_rows, viewport_cols);
                EditorOutcome::Continue
            }
            _ => EditorOutcome::Continue,
        }
    }

    fn insert_char(&mut self, c: char) {
        let (row, col) = self.cursor;
        let line = &mut self.lines[row];
        let byte_idx = char_to_byte(line, col);
        line.insert(byte_idx, c);
        self.cursor.1 = col + 1;
        self.mark_modified();
    }

    fn insert_newline(&mut self) {
        let (row, col) = self.cursor;
        let line = self.lines[row].clone();
        let byte_idx = char_to_byte(&line, col);
        let (head, tail) = line.split_at(byte_idx);
        self.lines[row] = head.to_string();
        self.lines.insert(row + 1, tail.to_string());
        self.cursor = (row + 1, 0);
        self.mark_modified();
    }

    fn backspace(&mut self) {
        let (row, col) = self.cursor;
        if col > 0 {
            let line = &mut self.lines[row];
            let byte_end = char_to_byte(line, col);
            let prev_byte = line[..byte_end]
                .chars()
                .next_back()
                .map(|c| byte_end - c.len_utf8())
                .unwrap_or(byte_end);
            line.replace_range(prev_byte..byte_end, "");
            self.cursor.1 = col - 1;
            self.mark_modified();
        } else if row > 0 {
            let removed = self.lines.remove(row);
            let prev_chars = self.lines[row - 1].chars().count();
            self.lines[row - 1].push_str(&removed);
            self.cursor = (row - 1, prev_chars);
            self.mark_modified();
        }
    }

    fn delete_forward(&mut self) {
        let (row, col) = self.cursor;
        let line_len = self.lines[row].chars().count();
        if col < line_len {
            let line = &mut self.lines[row];
            let byte_start = char_to_byte(line, col);
            let next_byte = line[byte_start..]
                .chars()
                .next()
                .map(|c| byte_start + c.len_utf8())
                .unwrap_or(byte_start);
            line.replace_range(byte_start..next_byte, "");
            self.mark_modified();
        } else if row + 1 < self.lines.len() {
            let next_line = self.lines.remove(row + 1);
            self.lines[row].push_str(&next_line);
            self.mark_modified();
        }
    }

    /// Move the cursor to the beginning of the previous word group.
    /// Behavior models common editors: from the current position skip
    /// any whitespace backwards, then skip a single contiguous run of
    /// word-characters (or a run of punctuation), landing on the first
    /// character of that run. Crosses line boundaries when at column 0.
    fn move_word_left(&mut self) {
        let (mut row, mut col) = self.cursor;
        if col == 0 {
            if row == 0 {
                return;
            }
            row -= 1;
            col = self.lines[row].chars().count();
            self.cursor = (row, col);
            return;
        }
        let chars: Vec<char> = self.lines[row].chars().collect();
        // Skip whitespace immediately left of cursor.
        while col > 0 && chars[col - 1].is_whitespace() {
            col -= 1;
        }
        if col == 0 {
            self.cursor = (row, col);
            return;
        }
        // Skip a contiguous run of the same category (word vs. punct).
        let kind_word = is_word_char(chars[col - 1]);
        while col > 0
            && !chars[col - 1].is_whitespace()
            && is_word_char(chars[col - 1]) == kind_word
        {
            col -= 1;
        }
        self.cursor = (row, col);
    }

    /// Mirror of `move_word_left` — places the cursor at the start of
    /// the next word: skip the current category run, then skip any
    /// trailing whitespace. Crosses line boundaries at end of line.
    fn move_word_right(&mut self) {
        let (row, mut col) = self.cursor;
        let chars: Vec<char> = self.lines[row].chars().collect();
        let len = chars.len();
        if col >= len {
            if row + 1 < self.lines.len() {
                self.cursor = (row + 1, 0);
            }
            return;
        }
        let kind_word = is_word_char(chars[col]);
        let starting_in_ws = chars[col].is_whitespace();
        if !starting_in_ws {
            // Skip current category run forward.
            while col < len
                && !chars[col].is_whitespace()
                && is_word_char(chars[col]) == kind_word
            {
                col += 1;
            }
        }
        // Skip whitespace after.
        while col < len && chars[col].is_whitespace() {
            col += 1;
        }
        // If we landed at end of line and didn't actually move, hop to
        // the next line so repeated Ctrl-Right always advances.
        if col == self.cursor.1 && col == len && row + 1 < self.lines.len() {
            self.cursor = (row + 1, 0);
            return;
        }
        self.cursor = (row, col);
        let _ = starting_in_ws; // silence warn — kept for readability
    }

    fn delete_word_left(&mut self) {
        let start = self.cursor;
        self.move_word_left();
        let end = start;
        if self.cursor == end {
            return;
        }
        self.delete_range(self.cursor, end);
    }

    fn delete_word_right(&mut self) {
        let start = self.cursor;
        self.move_word_right();
        let end = self.cursor;
        if start == end {
            return;
        }
        // `move_word_right` advanced the cursor; restore it so the
        // delete is forward-from-start.
        self.cursor = start;
        self.delete_range(start, end);
    }

    /// Delete text in `[from, to)` where both are `(row, col)` pairs in
    /// chars. Caller guarantees `from <= to` and both are valid. Cursor
    /// ends at `from`.
    fn delete_range(&mut self, from: (usize, usize), to: (usize, usize)) {
        let (fr, fc) = from;
        let (tr, tc) = to;
        if fr == tr {
            let line = &mut self.lines[fr];
            let bs = char_to_byte(line, fc);
            let be = char_to_byte(line, tc);
            line.replace_range(bs..be, "");
        } else {
            let head_byte = char_to_byte(&self.lines[fr], fc);
            let tail_byte = char_to_byte(&self.lines[tr], tc);
            let tail = self.lines[tr][tail_byte..].to_string();
            self.lines[fr].truncate(head_byte);
            self.lines[fr].push_str(&tail);
            self.lines.drain(fr + 1..=tr);
        }
        self.cursor = from;
        self.mark_modified();
    }

    fn move_left(&mut self) {
        let (row, col) = self.cursor;
        if col > 0 {
            self.cursor.1 = col - 1;
        } else if row > 0 {
            self.cursor.0 = row - 1;
            self.cursor.1 = self.lines[row - 1].chars().count();
        }
    }

    fn move_right(&mut self) {
        let (row, col) = self.cursor;
        let line_len = self.lines[row].chars().count();
        if col < line_len {
            self.cursor.1 = col + 1;
        } else if row + 1 < self.lines.len() {
            self.cursor = (row + 1, 0);
        }
    }

    fn move_up(&mut self) {
        if self.cursor.0 > 0 {
            self.cursor.0 -= 1;
            self.clamp_cursor();
        }
    }

    fn move_down(&mut self) {
        if self.cursor.0 + 1 < self.lines.len() {
            self.cursor.0 += 1;
            self.clamp_cursor();
        }
    }

    fn clamp_cursor(&mut self) {
        let len = self
            .lines
            .get(self.cursor.0)
            .map(|s| s.chars().count())
            .unwrap_or(0);
        if self.cursor.1 > len {
            self.cursor.1 = len;
        }
    }

    /// Adjust scroll so the cursor is visible inside a viewport of
    /// `rows × cols` cells. Mirrors `Nav::ensure_visible` but operates
    /// on the editor's two-axis state.
    pub fn ensure_visible(&mut self, viewport_rows: usize, viewport_cols: usize) {
        let (row, col) = self.cursor;
        if row < self.scroll_row {
            self.scroll_row = row;
        } else if viewport_rows > 0 && row >= self.scroll_row + viewport_rows {
            self.scroll_row = row + 1 - viewport_rows;
        }
        let line = self.lines.get(row).map(|s| s.as_str()).unwrap_or("");
        let cell_col = display_width_until(line, col);
        if cell_col < self.scroll_col {
            self.scroll_col = cell_col;
        } else if viewport_cols > 0 && cell_col >= self.scroll_col + viewport_cols {
            self.scroll_col = cell_col + 1 - viewport_cols;
        }
    }
}

/// Word-character predicate used for word jumps and word delete.
/// Matches what most editors call a "word": alphanumeric runs plus
/// underscore. Punctuation forms its own contiguous run so e.g.
/// `foo()` jumps land between `foo` and `()`.
fn is_word_char(c: char) -> bool {
    c.is_alphanumeric() || c == '_'
}

/// Convert a `(line, char_col)` pair into a byte index suitable for
/// `String::insert`/`replace_range`. Out-of-range cols clamp to end.
fn char_to_byte(s: &str, col: usize) -> usize {
    let mut bytes = 0;
    for (i, ch) in s.chars().enumerate() {
        if i >= col {
            return bytes;
        }
        bytes += ch.len_utf8();
    }
    bytes
}

/// Sum of display widths for the first `col` characters of `line`.
fn display_width_until(line: &str, col: usize) -> usize {
    line.chars()
        .take(col)
        .map(|c| UnicodeWidthChar::width(c).unwrap_or(0))
        .sum()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ed(initial: &str) -> EditorState {
        let lines: Vec<String> = if initial.is_empty() {
            vec![String::new()]
        } else {
            initial.split('\n').map(String::from).collect()
        };
        EditorState {
            path: PathBuf::from("/tmp/test"),
            lines,
            cursor: (0, 0),
            scroll_row: 0,
            scroll_col: 0,
            dirty: false,
            message: None,
            message_ttl: 0,
            quit_confirm: false,
            highlighted: Vec::new(),
        }
    }

    #[test]
    fn insert_at_cursor() {
        let mut e = ed("");
        e.insert_char('h');
        e.insert_char('i');
        assert_eq!(e.lines, vec!["hi".to_string()]);
        assert_eq!(e.cursor, (0, 2));
        assert!(e.dirty);
    }

    #[test]
    fn enter_splits_line() {
        let mut e = ed("hello");
        e.cursor = (0, 3);
        e.insert_newline();
        assert_eq!(e.lines, vec!["hel".to_string(), "lo".to_string()]);
        assert_eq!(e.cursor, (1, 0));
    }

    #[test]
    fn backspace_at_line_start_merges_with_previous() {
        let mut e = ed("foo\nbar");
        e.cursor = (1, 0);
        e.backspace();
        assert_eq!(e.lines, vec!["foobar".to_string()]);
        assert_eq!(e.cursor, (0, 3));
    }

    #[test]
    fn delete_at_line_end_pulls_next_line_up() {
        let mut e = ed("foo\nbar");
        e.cursor = (0, 3);
        e.delete_forward();
        assert_eq!(e.lines, vec!["foobar".to_string()]);
    }

    #[test]
    fn arrow_navigation_clamps_cursor_to_short_lines() {
        let mut e = ed("longer line\nshort");
        e.cursor = (0, 10);
        e.move_down();
        // Short line is 5 chars; cursor should clamp to 5.
        assert_eq!(e.cursor, (1, 5));
    }

    #[test]
    fn unicode_char_insert_preserves_byte_indexing() {
        let mut e = ed("");
        e.insert_char('é');
        e.insert_char('a');
        assert_eq!(e.lines, vec!["éa".to_string()]);
        assert_eq!(e.cursor, (0, 2));
    }

    #[test]
    fn word_jump_right_lands_at_next_word_start() {
        let mut e = ed("foo  bar() baz");
        e.cursor = (0, 0);
        e.move_word_right();
        // Past `foo`, past whitespace → start of `bar`.
        assert_eq!(e.cursor, (0, 5));
        e.move_word_right();
        // From inside `bar`'s start: skip word run `bar`, no whitespace,
        // land at `(`.
        assert_eq!(e.cursor, (0, 8));
        e.move_word_right();
        // Skip `()`, then whitespace → start of `baz`.
        assert_eq!(e.cursor, (0, 11));
    }

    #[test]
    fn word_jump_right_crosses_lines_at_eol() {
        let mut e = ed("end\nnext");
        e.cursor = (0, 3); // end of first line
        e.move_word_right();
        assert_eq!(e.cursor, (1, 0));
    }

    #[test]
    fn word_jump_left_lands_at_word_start() {
        let mut e = ed("foo  bar baz");
        e.cursor = (0, 12); // EOL
        e.move_word_left();
        assert_eq!(e.cursor, (0, 9)); // start of `baz`
        e.move_word_left();
        assert_eq!(e.cursor, (0, 5)); // start of `bar`
    }

    #[test]
    fn word_jump_left_crosses_lines_at_col_zero() {
        let mut e = ed("first\nsecond");
        e.cursor = (1, 0);
        e.move_word_left();
        assert_eq!(e.cursor, (0, 5));
    }

    #[test]
    fn ctrl_backspace_deletes_previous_word() {
        let mut e = ed("hello world");
        e.cursor = (0, 11);
        e.delete_word_left();
        // Deleted `world`; line is `hello ` (trailing space stays — the
        // motion stopped at the space boundary).
        assert_eq!(e.lines, vec!["hello ".to_string()]);
        assert_eq!(e.cursor, (0, 6));
    }

    #[test]
    fn ctrl_delete_removes_next_word() {
        let mut e = ed("hello world");
        e.cursor = (0, 0);
        e.delete_word_right();
        // Deleted `hello ` (word + trailing space jump); line is `world`.
        assert_eq!(e.lines, vec!["world".to_string()]);
        assert_eq!(e.cursor, (0, 0));
    }

    fn ctrl(c: char) -> KeyEvent {
        KeyEvent::new(KeyCode::Char(c), KeyModifiers::CONTROL)
    }

    #[test]
    fn ctrl_a_jumps_to_line_start_ctrl_e_to_end() {
        let mut e = ed("hello world");
        e.cursor = (0, 5);
        e.handle_key(ctrl('a'), 24, 80);
        assert_eq!(e.cursor, (0, 0));
        e.handle_key(ctrl('e'), 24, 80);
        assert_eq!(e.cursor, (0, 11));
    }

    #[test]
    fn ctrl_t_g_jump_buffer_top_and_bottom() {
        let mut e = ed("a\nb\nc");
        e.cursor = (1, 0);
        e.handle_key(ctrl('t'), 24, 80);
        assert_eq!(e.cursor, (0, 0));
        e.handle_key(ctrl('g'), 24, 80);
        assert_eq!(e.cursor, (2, 1));
    }

    #[test]
    fn ctrl_k_kills_to_end_of_line_then_swallows_break() {
        let mut e = ed("hello\nworld");
        e.cursor = (0, 2);
        e.handle_key(ctrl('k'), 24, 80);
        // Killed "llo"; line is now "he\nworld".
        assert_eq!(e.lines, vec!["he".to_string(), "world".to_string()]);
        e.handle_key(ctrl('k'), 24, 80);
        // At EOL → second ^K joins the next line.
        assert_eq!(e.lines, vec!["heworld".to_string()]);
    }

    #[test]
    fn ctrl_u_kills_to_beginning_of_line() {
        let mut e = ed("hello world");
        e.cursor = (0, 6);
        e.handle_key(ctrl('u'), 24, 80);
        assert_eq!(e.lines, vec!["world".to_string()]);
        assert_eq!(e.cursor, (0, 0));
    }

    #[test]
    fn ctrl_w_deletes_previous_word_via_handle_key() {
        let mut e = ed("foo bar");
        e.cursor = (0, 7);
        e.handle_key(ctrl('w'), 24, 80);
        assert_eq!(e.lines, vec!["foo ".to_string()]);
    }

    #[test]
    fn highlight_cache_populates_for_known_extension() {
        let mut p = std::env::temp_dir();
        p.push(format!("gfb-editor-hl-{}.rs", std::process::id()));
        std::fs::write(&p, "fn main() { let x = 1; }\n").unwrap();
        let e = EditorState::open(&p).unwrap();
        // Cache should have one entry per buffer line and the first
        // line should have multiple spans (proves syntect ran).
        assert_eq!(e.highlighted.len(), e.lines.len());
        assert!(e.highlighted[0].spans.len() > 1);
        let _ = std::fs::remove_file(&p);
    }

    #[test]
    fn save_writes_atomically() {
        let mut p = std::env::temp_dir();
        p.push(format!("gfb-editor-save-{}.txt", std::process::id()));
        let _ = std::fs::remove_file(&p);
        let mut e = ed("alpha\nbeta\n");
        e.path = p.clone();
        e.dirty = true;
        e.save().unwrap();
        assert!(!e.dirty);
        let read = std::fs::read_to_string(&p).unwrap();
        assert_eq!(read, "alpha\nbeta\n");
        let _ = std::fs::remove_file(&p);
    }
}
