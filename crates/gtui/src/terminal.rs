//! Shared PTY-backed terminal pane.
//!
//! Apps own a [`TerminalSession`] in their state, forward key events
//! with [`TerminalSession::send_key`], and render it with
//! [`TerminalPane`]. The component deliberately keeps terminal
//! emulation modest: it handles the common cursor-addressed ANSI
//! controls used by shells and full-screen CLIs, ignores unsafe
//! terminal-control payloads, and keeps bounded scrollback suitable for
//! embedding shell workflows in an existing TUI pane.

use std::io::{Read, Write};
use std::path::Path;
use std::sync::mpsc;
use std::thread;

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use portable_pty::{native_pty_system, Child, CommandBuilder, MasterPty, PtySize};
use ratatui::layout::Rect;
use ratatui::text::Line;
use ratatui::widgets::Widget;

use crate::widgets::{BoxedPanel, ScrollableText};
use crate::Theme;

const MAX_SCROLLBACK_LINES: usize = 5000;
const TRIM_SCROLLBACK_TO: usize = 4000;
const MAX_COLS: usize = 500;

/// Running shell attached to a PTY.
pub struct TerminalSession {
    master: Box<dyn MasterPty + Send>,
    writer: Box<dyn Write + Send>,
    child: Box<dyn Child + Send + Sync>,
    rx: mpsc::Receiver<Vec<u8>>,
    screen: TerminalScreen,
    rows: u16,
    cols: u16,
}

impl TerminalSession {
    /// Spawn the user's `$SHELL` in `cwd` with the given cell size.
    pub fn spawn(cwd: &Path, rows: u16, cols: u16) -> std::io::Result<Self> {
        let pty_system = native_pty_system();
        let pair = pty_system
            .openpty(PtySize {
                rows: rows.max(1),
                cols: cols.max(1),
                pixel_width: 0,
                pixel_height: 0,
            })
            .map_err(std::io::Error::other)?;
        let shell = std::env::var("SHELL").unwrap_or_else(|_| "/bin/bash".to_string());
        let mut cmd = CommandBuilder::new(shell);
        cmd.cwd(cwd);
        configure_embedded_terminal_env(&mut cmd);
        let child = pair
            .slave
            .spawn_command(cmd)
            .map_err(std::io::Error::other)?;
        let mut reader = pair
            .master
            .try_clone_reader()
            .map_err(std::io::Error::other)?;
        let writer = pair.master.take_writer().map_err(std::io::Error::other)?;
        drop(pair.slave);

        let (tx, rx) = mpsc::channel();
        thread::spawn(move || {
            let mut buf = [0_u8; 8192];
            loop {
                match reader.read(&mut buf) {
                    Ok(0) => break,
                    Ok(n) => {
                        if tx.send(buf[..n].to_vec()).is_err() {
                            break;
                        }
                    }
                    Err(_) => break,
                }
            }
        });

        Ok(Self {
            master: pair.master,
            writer,
            child,
            rx,
            screen: TerminalScreen::new(rows.max(1), cols.max(1)),
            rows: rows.max(1),
            cols: cols.max(1),
        })
    }

    /// Drain PTY output into the scrollback. Returns true if anything changed.
    pub fn drain(&mut self) -> bool {
        let mut dirty = false;
        while let Ok(bytes) = self.rx.try_recv() {
            self.screen.feed(&bytes);
            dirty = true;
        }
        dirty
    }

    /// Resize the underlying PTY when the pane changes size.
    pub fn resize(&mut self, rows: u16, cols: u16) {
        let rows = rows.max(1);
        let cols = cols.max(1);
        if rows == self.rows && cols == self.cols {
            return;
        }
        self.rows = rows;
        self.cols = cols;
        self.screen.resize(rows, cols);
        let _ = self.master.resize(PtySize {
            rows,
            cols,
            pixel_width: 0,
            pixel_height: 0,
        });
    }

    /// Forward a crossterm key event to the PTY.
    pub fn send_key(&mut self, key: KeyEvent) {
        if let Some(bytes) = key_to_bytes(key) {
            let _ = self.writer.write_all(&bytes);
            let _ = self.writer.flush();
        }
    }

    pub fn lines(&self) -> Vec<Line<'static>> {
        self.screen.lines()
    }

    pub fn line_count(&self) -> usize {
        self.screen.line_count()
    }
}

impl Drop for TerminalSession {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

/// Render a [`TerminalSession`] inside a themed boxed pane.
pub struct TerminalPane<'a> {
    session: &'a TerminalSession,
    theme: &'a Theme,
    title: String,
    controls: String,
    accent: ratatui::style::Color,
    scroll: usize,
}

impl<'a> TerminalPane<'a> {
    pub fn new(session: &'a TerminalSession, theme: &'a Theme) -> Self {
        Self {
            session,
            theme,
            title: "terminal".to_string(),
            controls: "T close".to_string(),
            accent: theme.panel_accents[3],
            scroll: 0,
        }
    }

    pub fn with_title(mut self, title: impl Into<String>) -> Self {
        self.title = title.into();
        self
    }

    pub fn with_controls(mut self, controls: impl Into<String>) -> Self {
        self.controls = controls.into();
        self
    }

    pub fn with_accent(mut self, accent: ratatui::style::Color) -> Self {
        self.accent = accent;
        self
    }

    pub fn with_scroll(mut self, scroll: usize) -> Self {
        self.scroll = scroll;
        self
    }
}

impl Widget for TerminalPane<'_> {
    fn render(self, area: Rect, buf: &mut ratatui::buffer::Buffer) {
        if area.width == 0 || area.height == 0 {
            return;
        }
        let panel = BoxedPanel::new(self.accent, self.theme.title)
            .with_title(self.title)
            .with_controls(self.controls);
        let inner = panel.inner(area);
        panel.render(area, buf);
        if inner.height == 0 {
            return;
        }
        let lines = self.session.lines();
        let widget = ScrollableText::new(&lines, self.theme).with_scroll(self.scroll);
        widget.render(inner, buf);
    }
}

#[derive(Debug, Clone)]
struct TerminalScreen {
    scrollback: Vec<String>,
    screen: Vec<String>,
    row: usize,
    col: usize,
    rows: usize,
    cols: usize,
    escape: EscapeState,
    saved_cursor: Option<(usize, usize)>,
    alternate_screen: bool,
    primary_screen: Option<(Vec<String>, usize, usize)>,
}

impl TerminalScreen {
    fn new(rows: u16, cols: u16) -> Self {
        let rows = rows.max(1) as usize;
        let cols = (cols.max(1) as usize).min(MAX_COLS);
        Self {
            scrollback: Vec::new(),
            screen: vec![String::new(); rows],
            row: 0,
            col: 0,
            rows,
            cols,
            escape: EscapeState::None,
            saved_cursor: None,
            alternate_screen: false,
            primary_screen: None,
        }
    }

    fn feed(&mut self, bytes: &[u8]) {
        for ch in String::from_utf8_lossy(bytes).chars() {
            if self.consume_escape(ch) {
                continue;
            }
            match ch {
                '\x1b' => self.escape = EscapeState::Esc,
                '\r' => self.col = 0,
                '\n' => self.line_feed(),
                '\x08' | '\x7f' => {
                    self.backspace();
                }
                '\t' => {
                    let next_tab = ((self.col / 4) + 1) * 4;
                    while self.col < next_tab {
                        self.put_char(' ');
                    }
                }
                c if c.is_control() => {}
                c => self.put_char(c),
            }
        }
    }

    fn resize(&mut self, rows: u16, cols: u16) {
        self.rows = rows.max(1) as usize;
        self.cols = (cols.max(1) as usize).min(MAX_COLS);
        if self.screen.len() > self.rows {
            let overflow = self.screen.len() - self.rows;
            if self.alternate_screen {
                self.screen.drain(0..overflow);
            } else {
                let drained: Vec<String> = self.screen.drain(0..overflow).collect();
                self.scrollback.extend(drained);
                self.trim_scrollback();
            }
            self.row = self.row.saturating_sub(overflow);
        } else {
            self.screen.resize_with(self.rows, String::new);
        }
        self.row = self.row.min(self.rows.saturating_sub(1));
        self.col = self.col.min(self.cols.saturating_sub(1));
        self.truncate_rows();
    }

    fn consume_escape(&mut self, ch: char) -> bool {
        let escape = std::mem::replace(&mut self.escape, EscapeState::None);
        match escape {
            EscapeState::None => false,
            EscapeState::Esc => {
                self.escape = match ch {
                    '[' => EscapeState::Csi(String::new()),
                    ']' | 'P' | '^' | '_' | 'X' => EscapeState::String,
                    '7' => {
                        self.saved_cursor = Some((self.row, self.col));
                        EscapeState::None
                    }
                    '8' => {
                        if let Some((row, col)) = self.saved_cursor {
                            self.row = row.min(self.rows.saturating_sub(1));
                            self.col = col.min(self.cols.saturating_sub(1));
                        }
                        EscapeState::None
                    }
                    'c' => {
                        self.clear_screen();
                        EscapeState::None
                    }
                    _ => EscapeState::None,
                };
                true
            }
            EscapeState::Csi(mut seq) => {
                if ('@'..='~').contains(&ch) {
                    self.apply_csi(&seq, ch);
                    self.escape = EscapeState::None;
                } else if seq.len() < 64 {
                    seq.push(ch);
                    self.escape = EscapeState::Csi(seq);
                } else {
                    self.escape = EscapeState::None;
                }
                true
            }
            EscapeState::String => {
                if ch == '\x07' {
                    self.escape = EscapeState::None;
                } else if ch == '\x1b' {
                    self.escape = EscapeState::StringEsc;
                } else {
                    self.escape = EscapeState::String;
                }
                true
            }
            EscapeState::StringEsc => {
                self.escape = if ch == '\\' {
                    EscapeState::None
                } else if ch == '\x1b' {
                    EscapeState::StringEsc
                } else {
                    EscapeState::String
                };
                true
            }
        }
    }

    fn put_char(&mut self, c: char) {
        if self.col >= self.cols {
            self.line_feed();
        }
        while char_len(&self.screen[self.row]) < self.col {
            self.screen[self.row].push(' ');
        }
        if char_len(&self.screen[self.row]) == self.col {
            self.screen[self.row].push(c);
        } else {
            replace_char(&mut self.screen[self.row], self.col, c);
        }
        self.col += 1;
    }

    fn backspace(&mut self) {
        if self.col > 0 {
            self.col -= 1;
        }
        remove_char(&mut self.screen[self.row], self.col);
    }

    fn line_feed(&mut self) {
        self.col = 0;
        if self.row + 1 < self.rows {
            self.row += 1;
            return;
        }
        if !self.screen.is_empty() {
            let scrolled = self.screen.remove(0);
            if !self.alternate_screen {
                self.scrollback.push(scrolled);
            }
            self.screen.push(String::new());
            self.trim_scrollback();
        }
    }

    fn apply_csi(&mut self, seq: &str, final_ch: char) {
        let private = seq.starts_with('?');
        let params = parse_csi_params(seq);
        match final_ch {
            '@' => self.insert_blank_chars(param_or(&params, 0, 1)),
            'A' => self.row = self.row.saturating_sub(param_or(&params, 0, 1)),
            'B' => self.row = (self.row + param_or(&params, 0, 1)).min(self.rows - 1),
            'C' => self.col = (self.col + param_or(&params, 0, 1)).min(self.cols - 1),
            'D' => self.col = self.col.saturating_sub(param_or(&params, 0, 1)),
            'E' => {
                self.row = (self.row + param_or(&params, 0, 1)).min(self.rows - 1);
                self.col = 0;
            }
            'F' => {
                self.row = self.row.saturating_sub(param_or(&params, 0, 1));
                self.col = 0;
            }
            'G' => self.col = param_or(&params, 0, 1).saturating_sub(1).min(self.cols - 1),
            'H' | 'f' => {
                self.row = param_or(&params, 0, 1).saturating_sub(1).min(self.rows - 1);
                self.col = param_or(&params, 1, 1).saturating_sub(1).min(self.cols - 1);
            }
            'J' => match param_or(&params, 0, 0) {
                2 | 3 => {
                    if param_or(&params, 0, 0) == 3 {
                        self.scrollback.clear();
                    }
                    self.clear_screen();
                }
                0 => self.clear_from_cursor(),
                1 => self.clear_to_cursor(),
                _ => {}
            },
            'K' => match param_or(&params, 0, 0) {
                0 => truncate_chars_in_place(&mut self.screen[self.row], self.col),
                1 => clear_line_prefix(&mut self.screen[self.row], self.col),
                2 => self.screen[self.row].clear(),
                _ => {}
            },
            'L' => self.insert_blank_lines(param_or(&params, 0, 1)),
            'M' => self.delete_lines(param_or(&params, 0, 1)),
            'P' => self.delete_chars(param_or(&params, 0, 1)),
            'S' => {
                for _ in 0..param_or(&params, 0, 1) {
                    let scrolled = self.screen.remove(0);
                    if !self.alternate_screen {
                        self.scrollback.push(scrolled);
                    }
                    self.screen.push(String::new());
                }
                self.trim_scrollback();
            }
            'T' => {
                for _ in 0..param_or(&params, 0, 1) {
                    self.screen.pop();
                    self.screen.insert(0, String::new());
                }
            }
            'X' => self.erase_chars(param_or(&params, 0, 1)),
            'h' | 'l' if private && switches_alt_screen(seq) => {
                if final_ch == 'h' {
                    if !self.alternate_screen {
                        self.primary_screen = Some((self.screen.clone(), self.row, self.col));
                    }
                    self.alternate_screen = true;
                    self.clear_screen();
                } else {
                    self.alternate_screen = false;
                    if let Some((screen, row, col)) = self.primary_screen.take() {
                        self.screen = screen;
                        self.row = row.min(self.rows - 1);
                        self.col = col.min(self.cols.saturating_sub(1));
                    } else {
                        self.clear_screen();
                    }
                }
            }
            's' => self.saved_cursor = Some((self.row, self.col)),
            'u' => {
                if let Some((row, col)) = self.saved_cursor {
                    self.row = row.min(self.rows - 1);
                    self.col = col.min(self.cols - 1);
                }
            }
            _ => {}
        }
    }

    fn insert_blank_chars(&mut self, n: usize) {
        let mut chars: Vec<char> = self.screen[self.row].chars().collect();
        let at = self.col.min(chars.len());
        for _ in 0..n.min(self.cols) {
            chars.insert(at, ' ');
        }
        chars.truncate(self.cols);
        self.screen[self.row] = chars.into_iter().collect();
    }

    fn delete_chars(&mut self, n: usize) {
        let mut chars: Vec<char> = self.screen[self.row].chars().collect();
        let at = self.col.min(chars.len());
        for _ in 0..n.min(self.cols) {
            if at < chars.len() {
                chars.remove(at);
            }
        }
        self.screen[self.row] = chars.into_iter().collect();
    }

    fn erase_chars(&mut self, n: usize) {
        let mut chars: Vec<char> = self.screen[self.row].chars().collect();
        let end = (self.col + n).min(self.cols);
        if chars.len() < end {
            chars.resize(end, ' ');
        }
        for ch in chars.iter_mut().take(end).skip(self.col) {
            *ch = ' ';
        }
        self.screen[self.row] = chars.into_iter().collect();
    }

    fn insert_blank_lines(&mut self, n: usize) {
        for _ in 0..n.min(self.rows) {
            self.screen.insert(self.row, String::new());
            self.screen.pop();
        }
    }

    fn delete_lines(&mut self, n: usize) {
        for _ in 0..n.min(self.rows) {
            if self.row < self.screen.len() {
                self.screen.remove(self.row);
                self.screen.push(String::new());
            }
        }
    }

    fn clear_screen(&mut self) {
        for row in &mut self.screen {
            row.clear();
        }
        self.row = 0;
        self.col = 0;
    }

    fn clear_from_cursor(&mut self) {
        truncate_chars_in_place(&mut self.screen[self.row], self.col);
        for row in self.screen.iter_mut().skip(self.row + 1) {
            row.clear();
        }
    }

    fn clear_to_cursor(&mut self) {
        for row in self.screen.iter_mut().take(self.row) {
            row.clear();
        }
        clear_line_prefix(&mut self.screen[self.row], self.col);
    }

    fn trim_scrollback(&mut self) {
        if self.scrollback.len() > MAX_SCROLLBACK_LINES {
            let keep_from = self.scrollback.len() - TRIM_SCROLLBACK_TO;
            self.scrollback.drain(0..keep_from);
        }
    }

    fn truncate_rows(&mut self) {
        for row in &mut self.screen {
            truncate_chars_in_place(row, self.cols);
        }
        for row in &mut self.scrollback {
            truncate_chars_in_place(row, self.cols);
        }
    }

    fn lines(&self) -> Vec<Line<'static>> {
        let mut out: Vec<Line<'static>> = if self.alternate_screen {
            Vec::new()
        } else {
            self.scrollback.iter().cloned().map(Line::from).collect()
        };
        for (idx, screen_row) in self.screen.iter().enumerate() {
            let mut row = screen_row.clone();
            if idx == self.row {
                let cursor_at = self.col.min(self.cols.saturating_sub(1));
                while char_len(&row) < cursor_at {
                    row.push(' ');
                }
                insert_char(&mut row, cursor_at, '▏');
            }
            out.push(Line::from(row));
        }
        out
    }

    fn line_count(&self) -> usize {
        if self.alternate_screen {
            self.screen.len()
        } else {
            self.scrollback.len() + self.screen.len()
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum EscapeState {
    None,
    Esc,
    Csi(String),
    String,
    StringEsc,
}

fn parse_csi_params(seq: &str) -> Vec<usize> {
    seq.trim_start_matches('?')
        .split(';')
        .map(|part| {
            part.chars()
                .filter(|c| c.is_ascii_digit())
                .collect::<String>()
                .parse()
                .unwrap_or(0)
        })
        .collect()
}

fn param_or(params: &[usize], idx: usize, default: usize) -> usize {
    params
        .get(idx)
        .copied()
        .filter(|v| *v > 0)
        .unwrap_or(default)
}

fn char_len(s: &str) -> usize {
    s.chars().count()
}

fn byte_index(s: &str, char_idx: usize) -> usize {
    s.char_indices()
        .nth(char_idx)
        .map(|(idx, _)| idx)
        .unwrap_or(s.len())
}

fn insert_char(s: &mut String, idx: usize, ch: char) {
    let byte = byte_index(s, idx);
    s.insert(byte, ch);
}

fn replace_char(s: &mut String, idx: usize, ch: char) {
    let start = byte_index(s, idx);
    let end = byte_index(s, idx + 1);
    s.replace_range(start..end, &ch.to_string());
}

fn remove_char(s: &mut String, idx: usize) {
    if idx >= char_len(s) {
        return;
    }
    let start = byte_index(s, idx);
    let end = byte_index(s, idx + 1);
    s.replace_range(start..end, "");
}

fn truncate_chars_in_place(s: &mut String, len: usize) {
    let byte = byte_index(s, len);
    s.truncate(byte);
}

fn clear_line_prefix(s: &mut String, through_col: usize) {
    let mut chars: Vec<char> = s.chars().collect();
    let end = through_col.min(chars.len().saturating_sub(1));
    for ch in chars.iter_mut().take(end + 1) {
        *ch = ' ';
    }
    *s = chars.into_iter().collect();
}

fn switches_alt_screen(seq: &str) -> bool {
    seq.trim_start_matches('?')
        .split(';')
        .any(|param| matches!(param, "47" | "1047" | "1049"))
}

fn configure_embedded_terminal_env(cmd: &mut CommandBuilder) {
    cmd.env("TERM", "xterm-256color");
    cmd.env_remove("TERM_PROGRAM");
    cmd.env_remove("TERM_PROGRAM_VERSION");
    cmd.env_remove("KITTY_WINDOW_ID");
    cmd.env_remove("KITTY_PID");
    cmd.env_remove("WEZTERM_EXECUTABLE");
    cmd.env_remove("WEZTERM_PANE");
    cmd.env_remove("WT_SESSION");
    cmd.env_remove("VTE_VERSION");
}

fn key_to_bytes(key: KeyEvent) -> Option<Vec<u8>> {
    let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
    let alt = key.modifiers.contains(KeyModifiers::ALT);
    let mut out = Vec::new();
    if alt {
        out.push(0x1b);
    }
    match key.code {
        KeyCode::Char(c) if ctrl => {
            let lower = c.to_ascii_lowercase();
            if lower == ' ' {
                out.push(0);
            } else if lower.is_ascii_lowercase() {
                out.push((lower as u8) - b'a' + 1);
            } else {
                return None;
            }
        }
        KeyCode::Char(c) => {
            let mut buf = [0_u8; 4];
            out.extend_from_slice(c.encode_utf8(&mut buf).as_bytes());
        }
        KeyCode::Enter => out.push(b'\r'),
        KeyCode::Tab => out.push(b'\t'),
        KeyCode::Backspace => out.push(0x7f),
        KeyCode::Esc => out.push(0x1b),
        KeyCode::Up => out.extend_from_slice(b"\x1b[A"),
        KeyCode::Down => out.extend_from_slice(b"\x1b[B"),
        KeyCode::Right => out.extend_from_slice(b"\x1b[C"),
        KeyCode::Left => out.extend_from_slice(b"\x1b[D"),
        KeyCode::Home => out.extend_from_slice(b"\x1b[H"),
        KeyCode::End => out.extend_from_slice(b"\x1b[F"),
        KeyCode::PageUp => out.extend_from_slice(b"\x1b[5~"),
        KeyCode::PageDown => out.extend_from_slice(b"\x1b[6~"),
        KeyCode::Delete => out.extend_from_slice(b"\x1b[3~"),
        _ => return None,
    }
    Some(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rendered(screen: &TerminalScreen) -> Vec<String> {
        screen
            .lines()
            .into_iter()
            .map(|line| line.spans.into_iter().map(|span| span.content).collect())
            .collect()
    }

    #[test]
    fn cursor_addressed_output_stays_on_virtual_screen() {
        let mut screen = TerminalScreen::new(4, 12);
        screen.feed(b"one\ntwo\nthree");
        screen.feed(b"\x1b[2;5HXX");

        let lines = rendered(&screen);
        assert_eq!(lines[0], "one");
        assert_eq!(lines[1], "two XX\u{258f}");
        assert_eq!(lines[2], "three");
    }

    #[test]
    fn clear_screen_for_fullscreen_tui_does_not_leave_old_rows() {
        let mut screen = TerminalScreen::new(3, 12);
        screen.feed(b"old prompt\nold output");
        screen.feed(b"\x1b[2J\x1b[Hcodex");

        let lines = rendered(&screen);
        assert_eq!(lines[0], "codex\u{258f}");
        assert_eq!(lines[1], "");
        assert_eq!(lines[2], "");
    }

    #[test]
    fn osc_and_dcs_payloads_are_not_rendered() {
        let mut screen = TerminalScreen::new(2, 20);
        screen.feed(b"pre\x1b]0;title\x07mid\x1bP1;payload\x1b\\post");

        let lines = rendered(&screen);
        assert_eq!(lines[0], "premidpost\u{258f}");
    }

    #[test]
    fn long_output_wraps_instead_of_creating_unbounded_rows() {
        let mut screen = TerminalScreen::new(2, 5);
        screen.feed(b"abcdef");

        let lines = rendered(&screen);
        assert_eq!(lines[0], "abcde");
        assert_eq!(lines[1], "f\u{258f}");
    }

    #[test]
    fn insert_delete_and_erase_controls_update_row_in_place() {
        let mut screen = TerminalScreen::new(2, 12);
        screen.feed(b"abcdef");
        screen.feed(b"\x1b[1;3H\x1b[2@");
        screen.feed(b"\x1b[1;5HXY");
        screen.feed(b"\x1b[1;4H\x1b[P");
        screen.feed(b"\x1b[1;6H\x1b[2X");

        let lines = rendered(&screen);
        assert_eq!(lines[0], "ab XY\u{258f}  ");
    }

    #[test]
    fn next_and_previous_line_controls_reset_column() {
        let mut screen = TerminalScreen::new(3, 12);
        screen.feed(b"abc\x1b[Edef\x1b[FZ");

        let lines = rendered(&screen);
        assert_eq!(lines[0], "Z\u{258f}bc");
        assert_eq!(lines[1], "def");
    }

    #[test]
    fn alternate_screen_hides_scrollback_and_restores_normal_history() {
        let mut screen = TerminalScreen::new(2, 12);
        screen.feed(b"shell\nhistory");
        screen.feed(b"\x1b[?1049happ");

        let lines = rendered(&screen);
        assert_eq!(lines, vec!["app\u{258f}".to_string(), String::new()]);
        assert_eq!(screen.line_count(), 2);

        screen.feed(b"\x1b[?1049l\r\x1b[Kprompt");
        let lines = rendered(&screen);
        assert_eq!(lines[0], "shell");
        assert_eq!(lines[1], "prompt\u{258f}");
    }
}
