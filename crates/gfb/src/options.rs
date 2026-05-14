//! gfb-specific tab impls for `gtui::widgets::OptionsMenu`.
//!
//! Three tabs:
//! - `ThemeTab` — choose a bundled theme. Selection is committed live
//!   on cursor move so the user can see the theme change behind the
//!   menu; on Cancel the App reverts to the theme that was active
//!   when the menu opened.
//! - `BehaviorTab` — startup defaults that aren't fast-toggleable from
//!   a keybind (mouse capture, hidden files, sort order). Applies on
//!   Commit so the user can dial them in without disrupting the live
//!   browser.
//! - `ConnectionsTab` — list existing DB connection URLs and add new
//!   ones. Add fires immediately on Enter so the new connection shows
//!   up in the DB browser without restarting; removes are config-only
//!   and require a restart to take effect (worker indices are baked
//!   into the cache, see the comment on `connections` in `app.rs`).

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use gtui::widgets::{
    OptionsTab, SettingRow, SettingValue, SettingsForm, TabResult,
};
use gtui::Theme;
use ratatui::layout::Rect;
use ratatui::style::Style;
use ratatui::Frame;

use crate::config::ConnectionEntry;
use crate::fs::scan::SortMode;

const SORT_OPTIONS: &[&str] = &["name", "size", "mtime"];

fn sort_index(sort: SortMode) -> usize {
    match sort {
        SortMode::Name => 0,
        SortMode::Size => 1,
        SortMode::Mtime => 2,
    }
}

pub fn sort_from_index(idx: usize) -> SortMode {
    match idx {
        1 => SortMode::Size,
        2 => SortMode::Mtime,
        _ => SortMode::Name,
    }
}

fn form_colors(theme: &Theme) -> (ratatui::style::Color, ratatui::style::Color, ratatui::style::Color, ratatui::style::Color, ratatui::style::Color) {
    (
        theme.selected_bg,
        theme.selected_fg,
        theme.main_fg,
        theme.inactive_fg,
        theme.main_fg,
    )
}

/// Theme picker. Holds the list of known themes and the index of the
/// currently selected one.
pub struct ThemeTab {
    themes: Vec<String>,
    selected: usize,
}

impl ThemeTab {
    pub fn new(current: &str) -> Self {
        let themes: Vec<String> = gtui::builtin_names().map(|s| s.to_string()).collect();
        let selected = themes
            .iter()
            .position(|t| t == current)
            .unwrap_or(0);
        Self { themes, selected }
    }

    pub fn selected_theme(&self) -> &str {
        self.themes
            .get(self.selected)
            .map(|s| s.as_str())
            .unwrap_or(gtui::DEFAULT_THEME_NAME)
    }
}

impl OptionsTab for ThemeTab {
    fn as_any(&self) -> &dyn std::any::Any {
        self
    }
    fn as_any_mut(&mut self) -> &mut dyn std::any::Any {
        self
    }
    fn title(&self) -> &str {
        "theme"
    }

    fn render(&self, frame: &mut Frame, area: Rect, theme: &Theme) {
        let form = SettingsForm::new(vec![SettingRow::choice(
            "theme",
            self.themes.clone(),
            self.selected,
        )]);
        let (sb, sf, nf, inf, vf) = form_colors(theme);
        let form = form.with_colors(sb, sf, nf, inf, vf);
        if area.height >= 1 {
            let row = Rect::new(area.x + 1, area.y + 1, area.width.saturating_sub(2), 1);
            frame.render_widget(&form, row);
        }
        // Hint about live preview — useful since the change happens
        // behind the modal and isn't immediately obvious if the user
        // doesn't move the menu.
        if area.height >= 3 {
            let hint = "(theme applies live — Esc to revert)";
            let style = Style::default()
                .bg(theme.main_bg.unwrap_or(theme.meter_bg))
                .fg(theme.inactive_fg);
            gtui::write_str_clipped(
                frame.buffer_mut(),
                area.x + 1,
                area.y + 3,
                hint,
                area.width.saturating_sub(2),
                style,
            );
        }
    }

    fn handle_key(&mut self, key: KeyEvent) -> TabResult {
        match key.code {
            KeyCode::Left | KeyCode::Char('h') => {
                if !self.themes.is_empty() {
                    let len = self.themes.len();
                    self.selected = (self.selected + len - 1) % len;
                }
                TabResult::Consumed
            }
            KeyCode::Right | KeyCode::Char('l') => {
                if !self.themes.is_empty() {
                    self.selected = (self.selected + 1) % self.themes.len();
                }
                TabResult::Consumed
            }
            _ => TabResult::PassThrough,
        }
    }
}

/// Behavior defaults — toggles + sort choice.
pub struct BehaviorTab {
    form: SettingsForm,
    initial: BehaviorValues,
}

#[derive(Debug, Clone)]
pub struct BehaviorValues {
    pub mouse_capture: bool,
    pub show_hidden: bool,
    pub sort: SortMode,
}

impl BehaviorTab {
    pub fn new(values: BehaviorValues) -> Self {
        let form = SettingsForm::new(vec![
            SettingRow::toggle("mouse_capture", values.mouse_capture),
            SettingRow::toggle("show_hidden", values.show_hidden),
            SettingRow::choice(
                "sort",
                SORT_OPTIONS.iter().map(|s| s.to_string()).collect(),
                sort_index(values.sort),
            ),
        ]);
        Self {
            form,
            initial: values,
        }
    }

    pub fn values(&self) -> BehaviorValues {
        let mut v = self.initial.clone();
        for (i, row) in self.form.rows.iter().enumerate() {
            match (i, &row.value) {
                (0, SettingValue::Toggle(b)) => v.mouse_capture = *b,
                (1, SettingValue::Toggle(b)) => v.show_hidden = *b,
                (2, SettingValue::Choice { selected, .. }) => {
                    v.sort = sort_from_index(*selected);
                }
                _ => {}
            }
        }
        v
    }
}

impl OptionsTab for BehaviorTab {
    fn as_any(&self) -> &dyn std::any::Any {
        self
    }
    fn as_any_mut(&mut self) -> &mut dyn std::any::Any {
        self
    }
    fn title(&self) -> &str {
        "behavior"
    }

    fn render(&self, frame: &mut Frame, area: Rect, theme: &Theme) {
        let (sb, sf, nf, inf, vf) = form_colors(theme);
        let form = self.form.clone().with_colors(sb, sf, nf, inf, vf);
        let inner = Rect::new(
            area.x + 1,
            area.y + 1,
            area.width.saturating_sub(2),
            area.height.saturating_sub(1),
        );
        frame.render_widget(&form, inner);
    }

    fn handle_key(&mut self, key: KeyEvent) -> TabResult {
        match key.code {
            KeyCode::Up | KeyCode::Char('k') => {
                self.form.move_cursor(-1);
                TabResult::Consumed
            }
            KeyCode::Down | KeyCode::Char('j') => {
                self.form.move_cursor(1);
                TabResult::Consumed
            }
            KeyCode::Left | KeyCode::Char('h') => {
                self.form.cycle_current(-1);
                TabResult::Consumed
            }
            KeyCode::Right | KeyCode::Char('l') | KeyCode::Char(' ') => {
                self.form.cycle_current(1);
                TabResult::Consumed
            }
            _ => TabResult::PassThrough,
        }
    }

    fn is_dirty(&self) -> bool {
        let cur = self.values();
        cur.mouse_capture != self.initial.mouse_capture
            || cur.show_hidden != self.initial.show_hidden
            || cur.sort != self.initial.sort
    }
}

/// Connection list + URL input.
///
/// Two regions: the list of existing URLs at the top, an input row at
/// the bottom. `Tab`-level navigation is handled by the shell, so we
/// use `j`/`k`/arrows to move within the list. Pressing `i` focuses
/// the input field; pressing `Enter` while the input has content
/// fires a `pending_add` request that the App drains and turns into a
/// live `ConnectionWorker::spawn`. Pressing `d` on a list entry stages
/// it for removal on Commit (live remove is fragile because the DB
/// cache keys connection workers by index — see options.rs header).
pub struct ConnectionsTab {
    pub existing: Vec<String>,
    /// Entries we want to delete on Commit. Stored as indices into
    /// `existing`. UI shows a strikethrough-style "(remove)" hint
    /// next to staged rows.
    pub staged_removals: std::collections::HashSet<usize>,
    /// Cursor within the existing list, or `existing.len()` when the
    /// cursor is on the input row.
    pub cursor: usize,
    pub input: String,
    pub focus_input: bool,
    /// Set when the user presses Enter on a non-empty input. The App
    /// drains this each tick and spawns a worker; on success the URL
    /// is moved into `existing` and the input clears, on failure the
    /// status bar flashes the error and the input stays.
    pub pending_add: Option<String>,
    /// Whether the tab has any uncommitted state (an unsaved typed URL
    /// that wasn't added, or staged removals).
    last_error: Option<String>,
}

impl ConnectionsTab {
    pub fn new(existing: Vec<String>) -> Self {
        Self {
            existing,
            staged_removals: std::collections::HashSet::new(),
            cursor: 0,
            input: String::new(),
            focus_input: false,
            pending_add: None,
            last_error: None,
        }
    }

    pub fn set_error(&mut self, msg: impl Into<String>) {
        self.last_error = Some(msg.into());
    }

    pub fn finalize_add(&mut self, url: String) {
        self.existing.push(url);
        self.input.clear();
        self.last_error = None;
    }

    fn list_len(&self) -> usize {
        self.existing.len()
    }

    fn on_input_row(&self) -> bool {
        self.focus_input || self.cursor == self.list_len()
    }
}

impl OptionsTab for ConnectionsTab {
    fn as_any(&self) -> &dyn std::any::Any {
        self
    }
    fn as_any_mut(&mut self) -> &mut dyn std::any::Any {
        self
    }
    fn title(&self) -> &str {
        "connections"
    }

    fn render(&self, frame: &mut Frame, area: Rect, theme: &Theme) {
        let bg = theme.main_bg.unwrap_or(theme.meter_bg);
        let buf = frame.buffer_mut();
        let mut y = area.y;
        let right = area.x + area.width;

        // Section header.
        gtui::write_str_clipped(
            buf,
            area.x + 1,
            y,
            "saved connections",
            area.width.saturating_sub(2),
            Style::default().bg(bg).fg(theme.hi_fg),
        );
        y = y.saturating_add(2);

        if self.existing.is_empty() {
            gtui::write_str_clipped(
                buf,
                area.x + 3,
                y,
                "(none yet)",
                area.width.saturating_sub(4),
                Style::default().bg(bg).fg(theme.inactive_fg),
            );
            y = y.saturating_add(1);
        } else {
            for (i, url) in self.existing.iter().enumerate() {
                if y >= area.y + area.height {
                    break;
                }
                let is_cursor = !self.focus_input && i == self.cursor;
                let staged = self.staged_removals.contains(&i);
                let (fg, bg2) = if is_cursor {
                    (theme.selected_fg, theme.selected_bg)
                } else if staged {
                    (theme.inactive_fg, bg)
                } else {
                    (theme.main_fg, bg)
                };
                let prefix = if is_cursor { "▶ " } else { "  " };
                let mut text = format!("{prefix}{url}");
                if staged {
                    text.push_str("  (remove)");
                }
                // Paint background across the whole row for highlight.
                if is_cursor {
                    for xx in area.x..right {
                        buf[(xx, y)].set_style(Style::default().bg(bg2));
                    }
                }
                gtui::write_str_clipped(
                    buf,
                    area.x + 1,
                    y,
                    &text,
                    area.width.saturating_sub(2),
                    Style::default().bg(bg2).fg(fg),
                );
                y = y.saturating_add(1);
            }
        }

        // Input row.
        y = y.saturating_add(1);
        if y >= area.y + area.height {
            return;
        }
        gtui::write_str_clipped(
            buf,
            area.x + 1,
            y,
            "new connection (postgres:// or duckdb://):",
            area.width.saturating_sub(2),
            Style::default().bg(bg).fg(theme.hi_fg),
        );
        y = y.saturating_add(1);
        if y >= area.y + area.height {
            return;
        }
        let focused = self.on_input_row();
        let (fg, bg2) = if focused {
            (theme.selected_fg, theme.selected_bg)
        } else {
            (theme.main_fg, bg)
        };
        let prefix = if focused { "▶ " } else { "  " };
        let mut display = format!("{prefix}{}", self.input);
        if focused {
            display.push('_');
        }
        if focused {
            for xx in area.x..right {
                buf[(xx, y)].set_style(Style::default().bg(bg2));
            }
        }
        gtui::write_str_clipped(
            buf,
            area.x + 1,
            y,
            &display,
            area.width.saturating_sub(2),
            Style::default().bg(bg2).fg(fg),
        );
        // Hint / error.
        y = y.saturating_add(2);
        if y < area.y + area.height {
            let (text, color) = if let Some(err) = &self.last_error {
                (err.clone(), theme.title)
            } else {
                (
                    "i edit URL   Enter add   d stage remove   x unstage".to_string(),
                    theme.inactive_fg,
                )
            };
            gtui::write_str_clipped(
                buf,
                area.x + 1,
                y,
                &text,
                area.width.saturating_sub(2),
                Style::default().bg(bg).fg(color),
            );
        }
    }

    fn handle_key(&mut self, key: KeyEvent) -> TabResult {
        // Input-edit mode: Ctrl-W / Backspace / Esc-out, everything else
        // appends. Esc here only exits edit mode — the shell's Esc
        // intercept still works because we have to return PassThrough
        // when not on the input row.
        if self.focus_input {
            match key.code {
                KeyCode::Esc => {
                    self.focus_input = false;
                    TabResult::Consumed
                }
                KeyCode::Enter => {
                    if !self.input.trim().is_empty() {
                        self.pending_add = Some(self.input.trim().to_string());
                    }
                    TabResult::Consumed
                }
                KeyCode::Backspace => {
                    self.input.pop();
                    TabResult::Consumed
                }
                KeyCode::Char('w') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                    while self.input.ends_with(' ') {
                        self.input.pop();
                    }
                    while let Some(c) = self.input.chars().last() {
                        if c.is_whitespace() {
                            break;
                        }
                        self.input.pop();
                    }
                    TabResult::Consumed
                }
                KeyCode::Char('u') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                    self.input.clear();
                    TabResult::Consumed
                }
                KeyCode::Char(c) => {
                    if !key.modifiers.contains(KeyModifiers::CONTROL) {
                        self.input.push(c);
                    }
                    TabResult::Consumed
                }
                _ => TabResult::Consumed,
            }
        } else {
            match key.code {
                KeyCode::Up | KeyCode::Char('k') => {
                    if self.cursor > 0 {
                        self.cursor -= 1;
                    }
                    TabResult::Consumed
                }
                KeyCode::Down | KeyCode::Char('j') => {
                    let max = self.list_len();
                    if self.cursor < max {
                        self.cursor += 1;
                    }
                    TabResult::Consumed
                }
                KeyCode::Char('i') => {
                    self.focus_input = true;
                    TabResult::Consumed
                }
                KeyCode::Char('d') => {
                    if self.cursor < self.list_len() {
                        self.staged_removals.insert(self.cursor);
                    }
                    TabResult::Consumed
                }
                KeyCode::Char('x') => {
                    if self.cursor < self.list_len() {
                        self.staged_removals.remove(&self.cursor);
                    }
                    TabResult::Consumed
                }
                KeyCode::Enter => {
                    if self.on_input_row() && !self.input.trim().is_empty() {
                        self.pending_add = Some(self.input.trim().to_string());
                        TabResult::Consumed
                    } else {
                        // Let the shell treat Enter as Commit.
                        TabResult::PassThrough
                    }
                }
                _ => TabResult::PassThrough,
            }
        }
    }

    fn is_dirty(&self) -> bool {
        !self.staged_removals.is_empty() || !self.input.trim().is_empty()
    }
}

// ── Adapters used by App to read tab state through `dyn OptionsTab` ─────
//
// `app::handle_options_key` holds the menu as `OptionsMenu` which only
// exposes `&dyn OptionsTab`. Each helper here downcasts back to the
// concrete tab type and extracts what we need. The downcasts can't
// fail in practice (we built the menu in `open_options`) but we treat
// `None` as no-op so a future refactor that reorders tabs degrades
// gracefully instead of panicking.

pub fn infer_theme_selection(tab: &dyn gtui::widgets::OptionsTab) -> Option<String> {
    tab.as_any()
        .downcast_ref::<ThemeTab>()
        .map(|t| t.selected_theme().to_string())
}

pub fn infer_behavior_values(tab: &dyn gtui::widgets::OptionsTab) -> Option<BehaviorValues> {
    tab.as_any()
        .downcast_ref::<BehaviorTab>()
        .map(|t| t.values())
}

pub fn take_pending_add(tab: &mut dyn gtui::widgets::OptionsTab) -> Option<String> {
    tab.as_any_mut()
        .downcast_mut::<ConnectionsTab>()
        .and_then(|t| t.pending_add.take())
}

pub fn finalize_add_on_tab(tab: &mut dyn gtui::widgets::OptionsTab, url: String) {
    if let Some(t) = tab.as_any_mut().downcast_mut::<ConnectionsTab>() {
        t.finalize_add(url);
    }
}

pub fn set_tab_error(tab: &mut dyn gtui::widgets::OptionsTab, msg: String) {
    if let Some(t) = tab.as_any_mut().downcast_mut::<ConnectionsTab>() {
        t.set_error(msg);
    }
}

pub fn finalize_connections_from_tab(
    tab: &dyn gtui::widgets::OptionsTab,
) -> Vec<ConnectionEntry> {
    tab.as_any()
        .downcast_ref::<ConnectionsTab>()
        .map(finalize_connections)
        .unwrap_or_default()
}

/// Build the final list of saved connection entries that should be
/// persisted to config, applying the tab's staged removals.
pub fn finalize_connections(tab: &ConnectionsTab) -> Vec<ConnectionEntry> {
    tab.existing
        .iter()
        .enumerate()
        .filter(|(i, _)| !tab.staged_removals.contains(i))
        .map(|(_, url)| ConnectionEntry {
            url: url.clone(),
            label: None,
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crossterm::event::KeyModifiers;

    fn key(code: KeyCode) -> KeyEvent {
        KeyEvent::new(code, KeyModifiers::NONE)
    }

    #[test]
    fn theme_tab_cycles() {
        let mut t = ThemeTab::new("dracula");
        let before = t.selected_theme().to_string();
        t.handle_key(key(KeyCode::Right));
        let after = t.selected_theme().to_string();
        assert_ne!(before, after, "right arrow should change selection");
    }

    #[test]
    fn behavior_tab_reflects_toggles() {
        let mut t = BehaviorTab::new(BehaviorValues {
            mouse_capture: false,
            show_hidden: false,
            sort: SortMode::Name,
        });
        assert!(!t.is_dirty());
        t.handle_key(key(KeyCode::Right));
        assert!(t.is_dirty());
        assert!(t.values().mouse_capture);
    }

    #[test]
    fn connections_tab_stages_and_adds() {
        let mut t = ConnectionsTab::new(vec!["postgres://a".into()]);
        t.handle_key(key(KeyCode::Char('d')));
        assert!(t.staged_removals.contains(&0));
        let entries = finalize_connections(&t);
        assert!(entries.is_empty(), "staged removal should drop the entry");

        t.handle_key(key(KeyCode::Char('i')));
        for c in "duckdb://test".chars() {
            t.handle_key(key(KeyCode::Char(c)));
        }
        t.handle_key(key(KeyCode::Enter));
        assert_eq!(t.pending_add.as_deref(), Some("duckdb://test"));
    }
}
