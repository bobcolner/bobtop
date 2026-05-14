//! Shared PTY-backed terminal pane.
//!
//! Apps own a [`TerminalSession`] in their state, forward key events
//! with [`TerminalSession::send_key`], and render it with
//! [`TerminalPane`]. The component deliberately keeps terminal
//! emulation modest: it strips common ANSI control sequences and keeps
//! a scrollback buffer suitable for embedding shell workflows in an
//! existing TUI pane.

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
            screen: TerminalScreen::new(),
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
    lines: Vec<String>,
    row: String,
    col: usize,
    escape: EscapeState,
}

impl TerminalScreen {
    fn new() -> Self {
        Self {
            lines: Vec::new(),
            row: String::new(),
            col: 0,
            escape: EscapeState::None,
        }
    }

    fn feed(&mut self, bytes: &[u8]) {
        for &b in bytes {
            if self.consume_escape(b) {
                continue;
            }
            match b {
                0x1b => self.escape = EscapeState::Esc,
                b'\r' => self.col = 0,
                b'\n' => {
                    self.lines.push(std::mem::take(&mut self.row));
                    self.col = 0;
                    if self.lines.len() > 5000 {
                        let keep_from = self.lines.len() - 4000;
                        self.lines.drain(0..keep_from);
                    }
                }
                0x08 | 0x7f => {
                    if self.col > 0 {
                        self.col -= 1;
                    }
                    if self.row.len() > self.col {
                        self.row.remove(self.col);
                    }
                }
                b'\t' => {
                    for _ in 0..4 {
                        self.put_char(' ');
                    }
                }
                0x00..=0x1f => {}
                _ => self.put_char(b as char),
            }
        }
    }

    fn consume_escape(&mut self, b: u8) -> bool {
        match self.escape {
            EscapeState::None => false,
            EscapeState::Esc => {
                self.escape = if b == b'[' || b == b']' {
                    EscapeState::Seq
                } else {
                    EscapeState::None
                };
                true
            }
            EscapeState::Seq => {
                if (0x40..=0x7e).contains(&b) || b == 0x07 {
                    self.escape = EscapeState::None;
                }
                true
            }
        }
    }

    fn put_char(&mut self, c: char) {
        while self.row.len() < self.col {
            self.row.push(' ');
        }
        if self.col == self.row.len() {
            self.row.push(c);
        } else {
            self.row.remove(self.col);
            self.row.insert(self.col, c);
        }
        self.col += 1;
    }

    fn lines(&self) -> Vec<Line<'static>> {
        let mut out: Vec<Line<'static>> = self.lines.iter().cloned().map(Line::from).collect();
        let mut row = self.row.clone();
        row.push('▏');
        out.push(Line::from(row));
        out
    }

    fn line_count(&self) -> usize {
        self.lines.len() + 1
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum EscapeState {
    None,
    Esc,
    Seq,
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
