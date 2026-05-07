//! In-place text editor — nano-level functionality.
//!
//! `EditorState` owns the buffer (a `Vec<String>` line buffer), the
//! cursor `(row, col)` in chars, and scroll offsets. The widget
//! (`bobtop_tui::EditableText`) renders these; this module only
//! mutates state in response to keystrokes.
//!
//! Ops covered for v1: char insert, Backspace / Delete, newline,
//! Tab (4-space soft tab), arrow keys, Home / End, PgUp / PgDn,
//! Ctrl-Home / Ctrl-End, Ctrl-S save (atomic via sibling tmp +
//! rename), Ctrl-X exit with dirty-confirm. No undo, selection, or
//! search yet — those land in v2 if used.

use std::io;
use std::path::{Path, PathBuf};

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use unicode_width::UnicodeWidthChar;

const SOFT_TAB: &str = "    ";

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
        Ok(Self {
            path: path.to_path_buf(),
            lines,
            cursor: (0, 0),
            scroll_row: 0,
            scroll_col: 0,
            dirty: false,
            message: None,
            message_ttl: 0,
            quit_confirm: false,
        })
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

    /// Atomic save: write to a sibling temp file and rename. Avoids
    /// truncating the original if disk fills up mid-write.
    pub fn save(&mut self) -> io::Result<()> {
        let parent = self.path.parent().unwrap_or_else(|| Path::new("."));
        let mut tmp_name = self
            .path
            .file_name()
            .map(|s| s.to_os_string())
            .unwrap_or_default();
        tmp_name.push(".bobtop-fb.tmp");
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
                    EditorOutcome::Continue
                }
                KeyCode::End => {
                    let last = self.lines.len().saturating_sub(1);
                    let col = self.lines[last].chars().count();
                    self.cursor = (last, col);
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
        self.dirty = true;
    }

    fn insert_newline(&mut self) {
        let (row, col) = self.cursor;
        let line = self.lines[row].clone();
        let byte_idx = char_to_byte(&line, col);
        let (head, tail) = line.split_at(byte_idx);
        self.lines[row] = head.to_string();
        self.lines.insert(row + 1, tail.to_string());
        self.cursor = (row + 1, 0);
        self.dirty = true;
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
            self.dirty = true;
        } else if row > 0 {
            let removed = self.lines.remove(row);
            let prev_chars = self.lines[row - 1].chars().count();
            self.lines[row - 1].push_str(&removed);
            self.cursor = (row - 1, prev_chars);
            self.dirty = true;
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
            self.dirty = true;
        } else if row + 1 < self.lines.len() {
            let next_line = self.lines.remove(row + 1);
            self.lines[row].push_str(&next_line);
            self.dirty = true;
        }
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
    fn save_writes_atomically() {
        let mut p = std::env::temp_dir();
        p.push(format!("bobtop-fb-editor-save-{}.txt", std::process::id()));
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
