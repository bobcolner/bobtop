//! Generic tabbed options modal.
//!
//! A reusable shell for the apps' settings UI. The shell owns the
//! modal chrome (panel, tab bar, footer hints) and routes keys; each
//! tab is a [`OptionsTab`] impl that draws into a body rect and
//! decides what to do with the keys it receives.
//!
//! The trait is intentionally tiny so heterogeneous tabs (a static
//! `SettingsForm`, a CRUD list of DB connections, a free-form text
//! editor) can all coexist behind `Box<dyn OptionsTab>`. The caller
//! owns the concrete tab structs and reads their final state when
//! [`MenuOutcome::Commit`] fires — there's no value-marshalling
//! contract here on purpose, since each tab knows its own shape.
//!
//! Universal keybinds (always intercepted before the tab sees them):
//!
//! - `Tab` / `Shift+Tab`: cycle active tab.
//! - `Esc`: emit [`MenuOutcome::Cancel`].
//! - `Ctrl+S`: emit [`MenuOutcome::Commit`].
//!
//! Everything else is routed to the active tab's [`OptionsTab::handle_key`].
//! Tabs return [`TabResult::Consumed`] to stop further processing, or
//! [`TabResult::PassThrough`] to let the shell try a fallback — used
//! today only for `Enter`, which the shell treats as a secondary
//! commit binding for SettingsForm-style tabs that don't otherwise
//! consume it.

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use ratatui::layout::Rect;
use ratatui::style::{Color, Style};
use ratatui::Frame;

use crate::widgets::{BoxedPanel, CornerStyle, ModalShell};
use crate::{display_width, write_str_clipped, Theme};

/// What a tab returns from `handle_key`. Tabs that want exclusive
/// control of a key return `Consumed`; tabs that don't care return
/// `PassThrough` so the shell can apply its fallback rules.
#[derive(Debug, Clone, Copy)]
pub enum TabResult {
    Consumed,
    PassThrough,
}

/// One pane of the tabbed options modal.
///
/// Implementors decide their own internal state and rendering; the
/// shell only knows the title, whether the tab has unsaved edits, and
/// how to forward keys. The body `Rect` passed to `render` is the
/// space below the tab bar and above the footer — the tab does not
/// need to draw any chrome.
pub trait OptionsTab {
    fn title(&self) -> &str;

    fn render(&self, frame: &mut Frame, area: Rect, theme: &Theme);

    fn handle_key(&mut self, key: KeyEvent) -> TabResult;

    /// Adds a `*` after the title in the tab bar when `true`. Default
    /// is `false` since not every tab tracks dirty state.
    fn is_dirty(&self) -> bool {
        false
    }

    /// Escape hatches so callers can downcast back to a concrete tab
    /// type when reading final state on Commit. Implementations are
    /// uniformly `{ self }` — the indirection is needed because Rust
    /// trait objects are not directly convertible to `&dyn Any` even
    /// when the trait extends `Any` (different vtable shape).
    fn as_any(&self) -> &dyn std::any::Any;
    fn as_any_mut(&mut self) -> &mut dyn std::any::Any;
}

/// What the shell decided after dispatching a key.
#[derive(Debug, Clone, Copy)]
pub enum MenuOutcome {
    /// Keep the menu open.
    Stay,
    /// User pressed Esc — close without applying.
    Cancel,
    /// User pressed Ctrl+S (or Enter that no tab consumed) — caller
    /// should walk `tabs()` and apply each tab's state.
    Commit,
}

/// Tabbed options modal shell. Build once per open, drop on close.
pub struct OptionsMenu {
    title: String,
    tabs: Vec<Box<dyn OptionsTab>>,
    active: usize,
    width: u16,
    height: u16,
    corner: CornerStyle,
}

impl OptionsMenu {
    pub fn new(title: impl Into<String>, tabs: Vec<Box<dyn OptionsTab>>) -> Self {
        Self {
            title: title.into(),
            tabs,
            active: 0,
            width: 64,
            height: 22,
            corner: CornerStyle::default(),
        }
    }

    pub fn with_size(mut self, width: u16, height: u16) -> Self {
        self.width = width;
        self.height = height;
        self
    }

    pub fn with_corner(mut self, corner: CornerStyle) -> Self {
        self.corner = corner;
        self
    }

    pub fn with_active(mut self, idx: usize) -> Self {
        if idx < self.tabs.len() {
            self.active = idx;
        }
        self
    }

    pub fn active_index(&self) -> usize {
        self.active
    }

    pub fn tabs(&self) -> &[Box<dyn OptionsTab>] {
        &self.tabs
    }

    /// Mutable accessor for callers that need to inject state changes
    /// (e.g. preview-on-cursor-move where the app wants to read the
    /// current selection before deciding to fire a side effect).
    pub fn tabs_mut(&mut self) -> &mut [Box<dyn OptionsTab>] {
        &mut self.tabs
    }

    pub fn active_tab(&self) -> Option<&dyn OptionsTab> {
        self.tabs.get(self.active).map(|t| t.as_ref())
    }

    pub fn active_tab_mut(&mut self) -> Option<&mut Box<dyn OptionsTab>> {
        self.tabs.get_mut(self.active)
    }

    pub fn handle_key(&mut self, key: KeyEvent) -> MenuOutcome {
        // Universal intercepts — apply before tabs see them so tab
        // implementors can't accidentally swallow the global keys.
        if key.modifiers.contains(KeyModifiers::CONTROL) {
            if matches!(key.code, KeyCode::Char('s') | KeyCode::Char('S')) {
                return MenuOutcome::Commit;
            }
        }
        match key.code {
            KeyCode::Esc => return MenuOutcome::Cancel,
            KeyCode::Tab => {
                self.cycle_tab(1);
                return MenuOutcome::Stay;
            }
            KeyCode::BackTab => {
                self.cycle_tab(-1);
                return MenuOutcome::Stay;
            }
            _ => {}
        }
        let Some(tab) = self.tabs.get_mut(self.active) else {
            return MenuOutcome::Stay;
        };
        match tab.handle_key(key) {
            TabResult::Consumed => MenuOutcome::Stay,
            TabResult::PassThrough => {
                // Enter as a secondary save binding — matches gtop's
                // OptionsEditor where Enter == save+apply. Tabs that
                // need Enter for their own purposes (e.g. confirming
                // an input field) should return Consumed for it.
                if matches!(key.code, KeyCode::Enter) {
                    MenuOutcome::Commit
                } else {
                    MenuOutcome::Stay
                }
            }
        }
    }

    fn cycle_tab(&mut self, delta: i32) {
        if self.tabs.is_empty() {
            return;
        }
        let len = self.tabs.len() as i32;
        let cur = self.active as i32;
        self.active = (((cur + delta) % len + len) % len) as usize;
    }

    pub fn render(&self, frame: &mut Frame, area: Rect, theme: &Theme) {
        // No `with_keybinds` here — the in-body footer below renders
        // the same hints with proper spacing. The two-line duplicate
        // (pill on the border + chips in the body) was redundant.
        // Title is the centered `┤ … ├` bubble so the modal reads
        // like a dialog, not a corner-tagged panel.
        let panel = BoxedPanel::new(theme.title, theme.title)
            .with_center_title(self.title.clone())
            .with_corner_style(self.corner)
            .flat();
        let bg = theme.main_bg.unwrap_or(theme.meter_bg);
        let shell = ModalShell::new(panel, self.width, self.height)
            .with_fill(Style::default().bg(bg).fg(theme.main_fg));
        let Some(body) = shell.render(frame, area) else {
            return;
        };
        if body.height == 0 {
            return;
        }

        // Tab bar on the first body row, separator on the second.
        self.render_tab_bar(frame, Rect::new(body.x, body.y, body.width, 1), theme, bg);
        if body.height > 1 {
            let sep_y = body.y + 1;
            let buf = frame.buffer_mut();
            for x in body.x..body.x + body.width {
                let cell = &mut buf[(x, sep_y)];
                cell.set_char('─');
                cell.set_style(Style::default().bg(bg).fg(theme.div_line));
            }
        }

        // Reserve up to 3 rows for the footer: a blank spacer, a thin
        // `─` divider, and the keybind hint. The spacer + divider
        // prevent the hint from butting up against the body content
        // above. Footer is dropped entirely on very short modals.
        let footer_rows: u16 = if body.height >= 8 {
            3
        } else if body.height >= 6 {
            2
        } else {
            0
        };
        let body_top = body.y + 2;
        let body_h = body.height.saturating_sub(2 + footer_rows);
        let body_rect = Rect::new(body.x, body_top, body.width, body_h);
        if let Some(tab) = self.tabs.get(self.active) {
            tab.render(frame, body_rect, theme);
        }
        if footer_rows >= 2 {
            self.render_footer(frame, body, theme, bg, footer_rows);
        }
    }

    /// Render the bottom keybind hint with a divider line above it and
    /// generous spacing between groups so each `key  desc` chip is
    /// distinct. Delegates to [`crate::widgets::keybind_footer`] so
    /// the help modal can share the exact same look.
    fn render_footer(
        &self,
        frame: &mut Frame,
        body: Rect,
        theme: &Theme,
        bg: Color,
        footer_rows: u16,
    ) {
        let divider_y = body.bottom().saturating_sub(footer_rows.saturating_sub(1));
        let chips: &[(&str, &str)] = &[
            ("Tab", "switch"),
            ("Ctrl+S", "save"),
            ("Esc", "cancel"),
        ];
        crate::widgets::keybind_footer::render(frame, body, theme, bg, chips, divider_y);
    }

    fn render_tab_bar(&self, frame: &mut Frame, area: Rect, theme: &Theme, bg: Color) {
        let buf = frame.buffer_mut();
        // Clear the row first so a previous frame's tab bar doesn't
        // bleed through (ModalShell::with_fill already paints `bg`,
        // but be explicit so we don't depend on draw order).
        for x in area.x..area.x + area.width {
            buf[(x, area.y)].set_style(Style::default().bg(bg).fg(theme.main_fg));
        }
        let mut x = area.x + 2;
        let right = area.x + area.width;
        for (i, tab) in self.tabs.iter().enumerate() {
            if x >= right {
                break;
            }
            let is_active = i == self.active;
            let mut label = format!(" {} ", tab.title());
            if tab.is_dirty() {
                // Insert dirty marker before the trailing space.
                label.pop();
                label.push('*');
                label.push(' ');
            }
            let style = if is_active {
                Style::default().bg(theme.selected_bg).fg(theme.selected_fg)
            } else {
                Style::default().bg(bg).fg(theme.inactive_fg)
            };
            let width = display_width(&label) as u16;
            let avail = right.saturating_sub(x);
            let draw_w = width.min(avail);
            write_str_clipped(buf, x, area.y, &label, draw_w, style);
            x = x.saturating_add(draw_w + 1);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crossterm::event::KeyCode;
    use ratatui::backend::TestBackend;
    use ratatui::Terminal;

    struct StubTab {
        name: &'static str,
        last_key: Option<KeyCode>,
        dirty: bool,
        consume: bool,
    }

    impl OptionsTab for StubTab {
        fn title(&self) -> &str {
            self.name
        }
        fn render(&self, _f: &mut Frame, _a: Rect, _t: &Theme) {}
        fn handle_key(&mut self, key: KeyEvent) -> TabResult {
            self.last_key = Some(key.code);
            if self.consume {
                TabResult::Consumed
            } else {
                TabResult::PassThrough
            }
        }
        fn is_dirty(&self) -> bool {
            self.dirty
        }
        fn as_any(&self) -> &dyn std::any::Any {
            self
        }
        fn as_any_mut(&mut self) -> &mut dyn std::any::Any {
            self
        }
    }

    fn key(code: KeyCode) -> KeyEvent {
        KeyEvent::new(code, KeyModifiers::NONE)
    }

    fn ctrl(code: KeyCode) -> KeyEvent {
        KeyEvent::new(code, KeyModifiers::CONTROL)
    }

    #[test]
    fn esc_cancels_ctrl_s_commits() {
        let tabs: Vec<Box<dyn OptionsTab>> = vec![Box::new(StubTab {
            name: "a",
            last_key: None,
            dirty: false,
            consume: false,
        })];
        let mut menu = OptionsMenu::new("opts", tabs);
        assert!(matches!(menu.handle_key(key(KeyCode::Esc)), MenuOutcome::Cancel));
        assert!(matches!(
            menu.handle_key(ctrl(KeyCode::Char('s'))),
            MenuOutcome::Commit
        ));
    }

    #[test]
    fn tab_cycles_active() {
        let tabs: Vec<Box<dyn OptionsTab>> = vec![
            Box::new(StubTab {
                name: "a",
                last_key: None,
                dirty: false,
                consume: false,
            }),
            Box::new(StubTab {
                name: "b",
                last_key: None,
                dirty: false,
                consume: false,
            }),
            Box::new(StubTab {
                name: "c",
                last_key: None,
                dirty: false,
                consume: false,
            }),
        ];
        let mut menu = OptionsMenu::new("opts", tabs);
        assert_eq!(menu.active_index(), 0);
        menu.handle_key(key(KeyCode::Tab));
        assert_eq!(menu.active_index(), 1);
        menu.handle_key(key(KeyCode::BackTab));
        assert_eq!(menu.active_index(), 0);
        menu.handle_key(key(KeyCode::BackTab));
        assert_eq!(menu.active_index(), 2);
    }

    #[test]
    fn enter_passthrough_commits_consumed_stays() {
        let tabs: Vec<Box<dyn OptionsTab>> = vec![Box::new(StubTab {
            name: "a",
            last_key: None,
            dirty: false,
            consume: false,
        })];
        let mut menu = OptionsMenu::new("opts", tabs);
        assert!(matches!(
            menu.handle_key(key(KeyCode::Enter)),
            MenuOutcome::Commit
        ));

        let tabs: Vec<Box<dyn OptionsTab>> = vec![Box::new(StubTab {
            name: "a",
            last_key: None,
            dirty: false,
            consume: true,
        })];
        let mut menu = OptionsMenu::new("opts", tabs);
        assert!(matches!(
            menu.handle_key(key(KeyCode::Enter)),
            MenuOutcome::Stay
        ));
    }

    #[test]
    fn renders_tab_bar_with_dirty_marker() {
        let tabs: Vec<Box<dyn OptionsTab>> = vec![
            Box::new(StubTab {
                name: "alpha",
                last_key: None,
                dirty: true,
                consume: false,
            }),
            Box::new(StubTab {
                name: "beta",
                last_key: None,
                dirty: false,
                consume: false,
            }),
        ];
        let menu = OptionsMenu::new("opts", tabs).with_size(40, 12);
        let theme = Theme::fallback();
        let backend = TestBackend::new(60, 16);
        let mut term = Terminal::new(backend).unwrap();
        term.draw(|f| menu.render(f, f.area(), &theme)).unwrap();
        let buf = term.backend().buffer();
        let mut joined = String::new();
        for y in 0..16 {
            for x in 0..60 {
                joined.push(buf[(x, y)].symbol().chars().next().unwrap_or(' '));
            }
            joined.push('\n');
        }
        assert!(joined.contains("alpha*"), "dirty marker missing in:\n{joined}");
        assert!(joined.contains("beta"), "inactive tab missing in:\n{joined}");
    }
}
