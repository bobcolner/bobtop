//! gtop options modal — now a thin wrapper around
//! [`gtui::widgets::OptionsMenu`] so it shares chrome (tab bar,
//! footer, corner style) with gfb's options menu. The two apps look
//! the same when you press `O`.
//!
//! Public surface kept stable:
//! - `OptionsEditor::from_app` builds the modal from the live App.
//! - `handle_key` returns `OptionsOutcome::{KeepOpen,Preview,Cancel,Save}`.
//! - `cursor()` / `theme_name()` / `FIELD_COUNT` are still callable —
//!   useful for tests and the overlay's old sizing logic.

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use gtui::widgets::{
    MenuOutcome, OptionsMenu, OptionsTab, SettingRow, SettingValue, SettingsForm, TabResult,
};
use gtui::Theme;
use ratatui::layout::Rect;
use ratatui::style::Style;
use ratatui::Frame;

use crate::{
    app::App,
    cli::{CornerChoice, LayoutChoice, MAX_TICK_MS, MIN_TICK_MS},
};
use gtui::LayoutPreset;

/// Snapshot of theme-affecting settings — applied live on cursor
/// move and on Cancel to revert preview edits.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ThemePreview {
    pub theme: String,
    pub theme_background: bool,
    pub truecolor: bool,
}

impl ThemePreview {
    pub fn apply_to(&self, app: &mut App) {
        app.theme = crate::monitor_theme::load(&self.theme);
        app.theme_background = self.theme_background;
        app.truecolor = self.truecolor;
        app.apply_color_options();
    }
}

#[derive(Debug, Clone)]
pub struct OptionsCommit {
    pub theme: String,
    pub tick_ms: u64,
    pub layout: LayoutChoice,
    pub corners: CornerChoice,
    pub no_ebpf: bool,
    pub no_pcap: bool,
    pub tty: bool,
    pub show_virtual_net: bool,
    pub theme_background: bool,
    pub truecolor: bool,
}

impl OptionsCommit {
    pub fn apply_to(&self, app: &mut App) {
        app.theme = crate::monitor_theme::load(&self.theme);
        app.theme_background = self.theme_background;
        app.truecolor = self.truecolor;
        app.apply_color_options();
        app.corner_style = self.corners.into();
        app.tick_ms
            .store(self.tick_ms, std::sync::atomic::Ordering::Relaxed);
        app.set_layout(match self.layout {
            LayoutChoice::Full => LayoutPreset::Full,
            LayoutChoice::Minimal => LayoutPreset::Minimal,
        });
        app.tty_graphs = self.tty;
        app.show_virtual_net = self.show_virtual_net;
    }
}

#[derive(Debug)]
pub enum OptionsOutcome {
    KeepOpen(OptionsEditor),
    Preview {
        editor: OptionsEditor,
        preview: ThemePreview,
    },
    Cancel {
        preview: ThemePreview,
    },
    Save {
        commit: OptionsCommit,
        message: String,
    },
}

const LAYOUT_OPTIONS: &[&str] = &["full", "minimal"];
const CORNER_OPTIONS: &[&str] = &["rounded", "square"];

pub struct OptionsEditor {
    menu: OptionsMenu,
    original_theme: String,
    original_theme_background: bool,
    original_truecolor: bool,
}

impl std::fmt::Debug for OptionsEditor {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("OptionsEditor")
            .field("original_theme", &self.original_theme)
            .finish_non_exhaustive()
    }
}

impl OptionsEditor {
    /// Total visible field count across both tabs. Used by the
    /// overlay for default sizing; preserved on the public surface
    /// for backward compatibility.
    pub const FIELD_COUNT: usize = GeneralTab::FIELD_COUNT + BehaviorTab::FIELD_COUNT;
    pub const GENERAL_FIELD_COUNT: usize = GeneralTab::FIELD_COUNT;
    pub const BEHAVIOR_FIELD_COUNT: usize = BehaviorTab::FIELD_COUNT;

    pub fn from_app(app: &App) -> Self {
        let themes: Vec<String> = gtui::builtin_names().map(|s| s.to_string()).collect();
        let general = GeneralTab::from_app(app, &themes);
        let behavior = BehaviorTab::from_app(app);
        let tabs: Vec<Box<dyn OptionsTab>> = vec![Box::new(general), Box::new(behavior)];
        let menu = OptionsMenu::new(" options ", tabs)
            .with_corner(app.corner_style)
            .with_size(56, 18);
        Self {
            menu,
            original_theme: app.theme.name.clone(),
            original_theme_background: app.theme_background,
            original_truecolor: app.truecolor,
        }
    }

    /// Cursor index for tests / legacy callers. Returns the active
    /// tab's row cursor plus a section offset so the meaning matches
    /// the old single-form model (0..=3 in general, 4..=9 in behavior).
    pub fn cursor(&self) -> usize {
        match self.menu.active_index() {
            0 => self
                .general_tab()
                .map(|t| t.form.cursor)
                .unwrap_or(0),
            _ => {
                Self::GENERAL_FIELD_COUNT
                    + self.behavior_tab().map(|t| t.form.cursor).unwrap_or(0)
            }
        }
    }

    pub fn theme_name(&self) -> &str {
        self.general_tab()
            .map(GeneralTab::selected_theme)
            .unwrap_or(gtui::DEFAULT_THEME_NAME)
    }

    pub fn menu(&self) -> &OptionsMenu {
        &self.menu
    }

    pub fn handle_key(mut self, k: KeyEvent) -> OptionsOutcome {
        // Whether the keystroke might change the live theme preview.
        // Cycling on the theme row (General row 0) is the only path
        // that does — track it before dispatching so we can build the
        // outcome without re-reading the menu after.
        let was_on_theme_row = matches!(self.menu.active_index(), 0)
            && self.general_tab().map(|t| t.form.cursor).unwrap_or(99) == 0;
        let cycles_value = matches!(
            k.code,
            KeyCode::Left
                | KeyCode::Right
                | KeyCode::Char('h')
                | KeyCode::Char('l')
                | KeyCode::Char(' ')
        );
        let outcome = self.menu.handle_key(k);
        match outcome {
            MenuOutcome::Stay => {
                if was_on_theme_row && cycles_value {
                    let preview = self.preview();
                    OptionsOutcome::Preview {
                        editor: self,
                        preview,
                    }
                } else {
                    OptionsOutcome::KeepOpen(self)
                }
            }
            MenuOutcome::Cancel => OptionsOutcome::Cancel {
                preview: self.cancel_preview(),
            },
            MenuOutcome::Commit => {
                let commit = self.commit();
                let message = match Self::config_from_commit(&commit).save() {
                    Ok(p) => format!("saved {}", p.display()),
                    Err(e) => format!("save failed: {e}"),
                };
                OptionsOutcome::Save { commit, message }
            }
        }
    }

    fn general_tab(&self) -> Option<&GeneralTab> {
        self.menu
            .tabs()
            .first()
            .and_then(|t| t.as_any().downcast_ref::<GeneralTab>())
    }

    fn behavior_tab(&self) -> Option<&BehaviorTab> {
        self.menu
            .tabs()
            .get(1)
            .and_then(|t| t.as_any().downcast_ref::<BehaviorTab>())
    }

    fn preview(&self) -> ThemePreview {
        let g = self.general_tab();
        let b = self.behavior_tab();
        ThemePreview {
            theme: g.map(|t| t.selected_theme().to_string()).unwrap_or_default(),
            theme_background: b.map(|t| t.theme_background()).unwrap_or(true),
            truecolor: b.map(|t| t.truecolor()).unwrap_or(true),
        }
    }

    fn cancel_preview(&self) -> ThemePreview {
        ThemePreview {
            theme: self.original_theme.clone(),
            theme_background: self.original_theme_background,
            truecolor: self.original_truecolor,
        }
    }

    fn commit(&self) -> OptionsCommit {
        let g = self.general_tab();
        let b = self.behavior_tab();
        OptionsCommit {
            theme: g
                .map(|t| t.selected_theme().to_string())
                .unwrap_or_else(|| self.original_theme.clone()),
            tick_ms: g.map(GeneralTab::tick_ms).unwrap_or(500),
            layout: g.map(GeneralTab::layout).unwrap_or(LayoutChoice::Full),
            corners: g
                .map(GeneralTab::corners)
                .unwrap_or(CornerChoice::Rounded),
            no_ebpf: b.map(BehaviorTab::no_ebpf).unwrap_or(false),
            no_pcap: b.map(BehaviorTab::no_pcap).unwrap_or(false),
            tty: b.map(BehaviorTab::tty).unwrap_or(false),
            show_virtual_net: b.map(BehaviorTab::show_virtual_net).unwrap_or(false),
            theme_background: b.map(BehaviorTab::theme_background).unwrap_or(true),
            truecolor: b.map(BehaviorTab::truecolor).unwrap_or(true),
        }
    }

    fn config_from_commit(commit: &OptionsCommit) -> crate::config::Config {
        crate::config::Config {
            theme: Some(commit.theme.clone()),
            tick_ms: Some(commit.tick_ms),
            layout: Some(commit.layout),
            corners: Some(commit.corners),
            no_ebpf: Some(commit.no_ebpf),
            no_pcap: Some(commit.no_pcap),
            tty: Some(commit.tty),
            show_virtual_net: Some(commit.show_virtual_net),
            theme_background: Some(commit.theme_background),
            truecolor: Some(commit.truecolor),
        }
    }
}

/// ── General tab ───────────────────────────────────────────────────
///
/// Theme + render-pace + chrome choices. Themes load live; everything
/// else applies on Save.
pub struct GeneralTab {
    form: SettingsForm,
}

impl GeneralTab {
    pub const FIELD_COUNT: usize = 4;

    fn from_app(app: &App, themes: &[String]) -> Self {
        let theme_idx = themes
            .iter()
            .position(|t| t == &app.theme.name)
            .unwrap_or(0);
        let layout_idx = match app.layout_preset {
            LayoutPreset::Full => 0,
            LayoutPreset::Minimal => 1,
        };
        let corners_idx = match app.corner_style {
            gtui::widgets::CornerStyle::Rounded => 0,
            gtui::widgets::CornerStyle::Square => 1,
        };
        let form = SettingsForm::new(vec![
            SettingRow::choice("theme", themes.to_vec(), theme_idx),
            SettingRow::number(
                "tick_ms",
                app.tick_ms(),
                100,
                MIN_TICK_MS,
                MAX_TICK_MS,
                " ms",
            ),
            SettingRow::choice(
                "layout",
                LAYOUT_OPTIONS.iter().map(|s| s.to_string()).collect(),
                layout_idx,
            ),
            SettingRow::choice(
                "corners",
                CORNER_OPTIONS.iter().map(|s| s.to_string()).collect(),
                corners_idx,
            ),
        ]);
        Self { form }
    }

    pub fn selected_theme(&self) -> &str {
        match &self.form.rows[0].value {
            SettingValue::Choice { options, selected } => options
                .get(*selected)
                .map(String::as_str)
                .unwrap_or(gtui::DEFAULT_THEME_NAME),
            _ => gtui::DEFAULT_THEME_NAME,
        }
    }

    pub fn tick_ms(&self) -> u64 {
        match &self.form.rows[1].value {
            SettingValue::Number { value, .. } => *value,
            _ => 500,
        }
    }

    pub fn layout(&self) -> LayoutChoice {
        match &self.form.rows[2].value {
            SettingValue::Choice { options, selected } => {
                match options.get(*selected).map(String::as_str) {
                    Some("minimal") => LayoutChoice::Minimal,
                    _ => LayoutChoice::Full,
                }
            }
            _ => LayoutChoice::Full,
        }
    }

    pub fn corners(&self) -> CornerChoice {
        match &self.form.rows[3].value {
            SettingValue::Choice { options, selected } => {
                match options.get(*selected).map(String::as_str) {
                    Some("square") => CornerChoice::Square,
                    _ => CornerChoice::Rounded,
                }
            }
            _ => CornerChoice::Rounded,
        }
    }
}

impl OptionsTab for GeneralTab {
    fn as_any(&self) -> &dyn std::any::Any {
        self
    }
    fn as_any_mut(&mut self) -> &mut dyn std::any::Any {
        self
    }
    fn title(&self) -> &str {
        "general"
    }
    fn render(&self, frame: &mut Frame, area: Rect, theme: &Theme) {
        render_form(frame, area, theme, &self.form);
    }
    fn handle_key(&mut self, key: KeyEvent) -> TabResult {
        form_dispatch(&mut self.form, key)
    }
}

/// ── Behavior tab ──────────────────────────────────────────────────
///
/// All toggles. Apply on Save (except `theme_background` / `truecolor`,
/// which are part of the theme preview).
pub struct BehaviorTab {
    form: SettingsForm,
}

impl BehaviorTab {
    pub const FIELD_COUNT: usize = 6;

    fn from_app(app: &App) -> Self {
        let form = SettingsForm::new(vec![
            SettingRow::toggle("no_ebpf", false),
            SettingRow::toggle("no_pcap", false),
            SettingRow::toggle("tty (block graphs)", app.tty_graphs),
            SettingRow::toggle("show_virtual_net", app.show_virtual_net),
            SettingRow::toggle("theme_background", app.theme_background),
            SettingRow::toggle("truecolor", app.truecolor),
        ]);
        Self { form }
    }

    pub fn no_ebpf(&self) -> bool {
        toggle_at(&self.form, 0)
    }
    pub fn no_pcap(&self) -> bool {
        toggle_at(&self.form, 1)
    }
    pub fn tty(&self) -> bool {
        toggle_at(&self.form, 2)
    }
    pub fn show_virtual_net(&self) -> bool {
        toggle_at(&self.form, 3)
    }
    pub fn theme_background(&self) -> bool {
        toggle_at(&self.form, 4)
    }
    pub fn truecolor(&self) -> bool {
        toggle_at(&self.form, 5)
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
        render_form(frame, area, theme, &self.form);
    }
    fn handle_key(&mut self, key: KeyEvent) -> TabResult {
        form_dispatch(&mut self.form, key)
    }
}

fn toggle_at(form: &SettingsForm, idx: usize) -> bool {
    matches!(form.rows.get(idx).map(|r| &r.value), Some(SettingValue::Toggle(true)))
}

fn render_form(frame: &mut Frame, area: Rect, theme: &Theme, form: &SettingsForm) {
    let colored = form.clone().with_colors(
        theme.selected_bg,
        theme.selected_fg,
        theme.main_fg,
        theme.inactive_fg,
        theme.main_fg,
    );
    if area.height == 0 || area.width == 0 {
        return;
    }
    let inner = Rect::new(
        area.x + 1,
        area.y + 1,
        area.width.saturating_sub(2),
        area.height.saturating_sub(1),
    );
    frame.render_widget(&colored, inner);
    // Tiny hint below the form telling users which keys do what — the
    // top-of-modal pill is easy to miss.
    if area.height >= 4 {
        let hint = "↑↓ field   ←→ value   space toggle";
        let style = Style::default()
            .bg(theme.main_bg.unwrap_or(theme.meter_bg))
            .fg(theme.inactive_fg);
        let y = area.y + area.height.saturating_sub(1);
        gtui::write_str_clipped(
            frame.buffer_mut(),
            area.x + 1,
            y,
            hint,
            area.width.saturating_sub(2),
            style,
        );
    }
}

fn form_dispatch(form: &mut SettingsForm, key: KeyEvent) -> TabResult {
    let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
    if ctrl {
        // Let the shell handle Ctrl+S / Ctrl+anything.
        return TabResult::PassThrough;
    }
    match key.code {
        KeyCode::Up | KeyCode::Char('k') => {
            form.move_cursor(-1);
            TabResult::Consumed
        }
        KeyCode::Down | KeyCode::Char('j') => {
            form.move_cursor(1);
            TabResult::Consumed
        }
        KeyCode::Left | KeyCode::Char('h') => {
            form.cycle_current(-1);
            TabResult::Consumed
        }
        KeyCode::Right | KeyCode::Char('l') | KeyCode::Char(' ') => {
            form.cycle_current(1);
            TabResult::Consumed
        }
        _ => TabResult::PassThrough,
    }
}
