use crossterm::event::{KeyCode, KeyEvent};
use bobtop_tui::widgets::{SettingRow, SettingValue, SettingsForm};

use crate::{
    app::App,
    cli::{CornerChoice, LayoutChoice, MAX_TICK_MS, MIN_TICK_MS},
};
use bobtop_tui::LayoutPreset;

#[derive(Debug, Clone)]
struct OptionsState {
    cursor: usize,
    theme: String,
    tick_ms: u64,
    layout: LayoutChoice,
    corners: CornerChoice,
    no_ebpf: bool,
    no_pcap: bool,
    tty: bool,
    show_virtual_net: bool,
    theme_background: bool,
    truecolor: bool,
    themes: Vec<String>,
    original_theme: String,
    original_theme_background: bool,
    original_truecolor: bool,
}

impl OptionsState {
    const FIELD_COUNT: usize = 10;
}

#[derive(Debug, Clone)]
pub struct OptionsEditor {
    state: OptionsState,
    form: SettingsForm,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ThemePreview {
    pub theme: String,
    pub theme_background: bool,
    pub truecolor: bool,
}

impl ThemePreview {
    pub fn apply_to(&self, app: &mut App) {
        app.theme = bobtop_tui::load_theme(&self.theme);
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
        app.theme = bobtop_tui::load_theme(&self.theme);
        app.theme_background = self.theme_background;
        app.truecolor = self.truecolor;
        app.apply_color_options();
        app.corner_style = self.corners.into();
        app.tick_ms.store(self.tick_ms, std::sync::atomic::Ordering::Relaxed);
        app.set_layout(match self.layout {
            LayoutChoice::Full => LayoutPreset::Full,
            LayoutChoice::Minimal => LayoutPreset::Minimal,
        });
        app.tty_graphs = self.tty;
        app.show_virtual_net = self.show_virtual_net;
    }
}

#[derive(Debug, Clone)]
pub enum OptionsOutcome {
    KeepOpen(OptionsEditor),
    Preview { editor: OptionsEditor, preview: ThemePreview },
    Cancel { preview: ThemePreview },
    Save { commit: OptionsCommit, message: String },
}

impl OptionsEditor {
    pub const FIELD_COUNT: usize = OptionsState::FIELD_COUNT;
    pub const GENERAL_FIELD_COUNT: usize = 4;
    pub const BEHAVIOR_FIELD_COUNT: usize = OptionsState::FIELD_COUNT - Self::GENERAL_FIELD_COUNT;

    pub fn from_app(app: &App) -> Self {
        let themes: Vec<String> = bobtop_tui::builtin_names().map(|s| s.to_string()).collect();
        let current_theme = app.theme.name.clone();
        let state = OptionsState {
            cursor: 0,
            theme: current_theme.clone(),
            tick_ms: app.tick_ms(),
            layout: match app.layout_preset {
                LayoutPreset::Full => LayoutChoice::Full,
                LayoutPreset::Minimal => LayoutChoice::Minimal,
            },
            corners: match app.corner_style {
                bobtop_tui::widgets::CornerStyle::Rounded => CornerChoice::Rounded,
                bobtop_tui::widgets::CornerStyle::Square => CornerChoice::Square,
            },
            no_ebpf: false,
            no_pcap: false,
            tty: app.tty_graphs,
            show_virtual_net: app.show_virtual_net,
            theme_background: app.theme_background,
            truecolor: app.truecolor,
            themes,
            original_theme: current_theme,
            original_theme_background: app.theme_background,
            original_truecolor: app.truecolor,
        };
        let form = Self::form_from_state(&state);
        Self { state, form }
    }

    pub fn handle_key(mut self, k: KeyEvent) -> OptionsOutcome {
        match k.code {
            KeyCode::Up => {
                self.form.move_cursor(-1);
                self.state.cursor = self.form.cursor;
                OptionsOutcome::KeepOpen(self)
            }
            KeyCode::Down => {
                self.form.move_cursor(1);
                self.state.cursor = self.form.cursor;
                OptionsOutcome::KeepOpen(self)
            }
            KeyCode::Left => self.cycle_current(-1),
            KeyCode::Right | KeyCode::Char(' ') => self.cycle_current(1),
            KeyCode::Esc => OptionsOutcome::Cancel {
                preview: self.cancel_preview(),
            },
            KeyCode::Enter => OptionsOutcome::Save {
                commit: self.commit_from_state(),
                message: match Self::config_from_state(&self.state).save() {
                    Ok(p) => format!("saved {}", p.display()),
                    Err(e) => format!("save failed: {e}"),
                },
            },
            _ => OptionsOutcome::KeepOpen(self),
        }
    }

    pub fn theme_name(&self) -> &str {
        &self.state.theme
    }

    pub fn cursor(&self) -> usize {
        self.state.cursor
    }

    pub fn general_form(&self) -> SettingsForm {
        Self::section_form(
            &self.form,
            0,
            Self::GENERAL_FIELD_COUNT,
            self.form.cursor.min(Self::GENERAL_FIELD_COUNT.saturating_sub(1)),
        )
    }

    pub fn behavior_form(&self) -> SettingsForm {
        let start = Self::GENERAL_FIELD_COUNT;
        let cursor = if self.form.cursor >= start {
            self.form.cursor - start
        } else {
            usize::MAX
        };
        Self::section_form(&self.form, start, Self::BEHAVIOR_FIELD_COUNT, cursor)
    }

    pub fn apply_theme_cycle(&mut self, delta: i32) -> bool {
        let on_theme = self.form.cursor == 0;
        if !self.form.cycle_current(delta) {
            return false;
        }
        self.sync_state_from_form();
        on_theme
    }

    pub fn sync_state_from_form(&mut self) {
        let mut theme = self.state.theme.clone();
        let mut tick_ms = self.state.tick_ms;
        let mut layout = self.state.layout;
        let mut corners = self.state.corners;
        let mut no_ebpf = self.state.no_ebpf;
        let mut no_pcap = self.state.no_pcap;
        let mut tty = self.state.tty;
        let mut show_virtual_net = self.state.show_virtual_net;
        let mut theme_background = self.state.theme_background;
        let mut truecolor = self.state.truecolor;
        for (idx, row) in self.form.rows.iter().enumerate() {
            match idx {
                0 => {
                    if let SettingValue::Choice { options, selected } = &row.value {
                        if let Some(v) = options.get(*selected) {
                            theme = v.clone();
                        }
                    }
                }
                1 => {
                    if let SettingValue::Number { value, .. } = &row.value {
                        tick_ms = *value;
                    }
                }
                2 => {
                    if let SettingValue::Choice { options, selected } = &row.value {
                        layout = match options.get(*selected).map(|s| s.as_str()) {
                            Some("full") => LayoutChoice::Full,
                            Some("minimal") => LayoutChoice::Minimal,
                            _ => layout,
                        };
                    }
                }
                3 => {
                    if let SettingValue::Choice { options, selected } = &row.value {
                        corners = match options.get(*selected).map(|s| s.as_str()) {
                            Some("rounded") => CornerChoice::Rounded,
                            Some("square") => CornerChoice::Square,
                            _ => corners,
                        };
                    }
                }
                4 => if let SettingValue::Toggle(v) = &row.value { no_ebpf = *v; },
                5 => if let SettingValue::Toggle(v) = &row.value { no_pcap = *v; },
                6 => if let SettingValue::Toggle(v) = &row.value { tty = *v; },
                7 => if let SettingValue::Toggle(v) = &row.value { show_virtual_net = *v; },
                8 => if let SettingValue::Toggle(v) = &row.value { theme_background = *v; },
                9 => if let SettingValue::Toggle(v) = &row.value { truecolor = *v; },
                _ => {}
            }
        }
        self.state.theme = theme;
        self.state.tick_ms = tick_ms;
        self.state.layout = layout;
        self.state.corners = corners;
        self.state.no_ebpf = no_ebpf;
        self.state.no_pcap = no_pcap;
        self.state.tty = tty;
        self.state.show_virtual_net = show_virtual_net;
        self.state.theme_background = theme_background;
        self.state.truecolor = truecolor;
        self.state.cursor = self.form.cursor;
    }

    fn cycle_current(mut self, delta: i32) -> OptionsOutcome {
        let on_theme = self.form.cursor == 0;
        if self.form.cycle_current(delta) {
            self.sync_state_from_form();
            if on_theme {
                let preview = self.preview_from_state();
                OptionsOutcome::Preview {
                    editor: self,
                    preview,
                }
            } else {
                OptionsOutcome::KeepOpen(self)
            }
        } else {
            OptionsOutcome::KeepOpen(self)
        }
    }

    fn form_from_state(state: &OptionsState) -> SettingsForm {
        let theme_idx = state.themes.iter().position(|t| t == &state.theme).unwrap_or(0);
        let layout_idx = match state.layout {
            LayoutChoice::Full => 0,
            LayoutChoice::Minimal => 1,
        };
        let corners_idx = match state.corners {
            CornerChoice::Rounded => 0,
            CornerChoice::Square => 1,
        };
        SettingsForm::new(vec![
            SettingRow::choice("theme", state.themes.clone(), theme_idx),
            SettingRow::number("tick_ms", state.tick_ms, 100, MIN_TICK_MS, MAX_TICK_MS, " ms"),
            SettingRow::choice("layout", vec!["full".into(), "minimal".into()], layout_idx),
            SettingRow::choice("corners", vec!["rounded".into(), "square".into()], corners_idx),
            SettingRow::toggle("no_ebpf", state.no_ebpf),
            SettingRow::toggle("no_pcap", state.no_pcap),
            SettingRow::toggle("tty (block graphs)", state.tty),
            SettingRow::toggle("show_virtual_net", state.show_virtual_net),
            SettingRow::toggle("theme_background", state.theme_background),
            SettingRow::toggle("truecolor", state.truecolor),
        ])
        .with_cursor(state.cursor)
    }

    fn config_from_state(state: &OptionsState) -> crate::config::Config {
        crate::config::Config {
            theme: Some(state.theme.clone()),
            tick_ms: Some(state.tick_ms),
            layout: Some(state.layout),
            corners: Some(state.corners),
            no_ebpf: Some(state.no_ebpf),
            no_pcap: Some(state.no_pcap),
            tty: Some(state.tty),
            show_virtual_net: Some(state.show_virtual_net),
            theme_background: Some(state.theme_background),
            truecolor: Some(state.truecolor),
        }
    }

    fn preview_from_state(&self) -> ThemePreview {
        ThemePreview {
            theme: self.state.theme.clone(),
            theme_background: self.state.theme_background,
            truecolor: self.state.truecolor,
        }
    }

    fn cancel_preview(&self) -> ThemePreview {
        ThemePreview {
            theme: self.state.original_theme.clone(),
            theme_background: self.state.original_theme_background,
            truecolor: self.state.original_truecolor,
        }
    }

    fn commit_from_state(&self) -> OptionsCommit {
        OptionsCommit {
            theme: self.state.theme.clone(),
            tick_ms: self.state.tick_ms,
            layout: self.state.layout,
            corners: self.state.corners,
            no_ebpf: self.state.no_ebpf,
            no_pcap: self.state.no_pcap,
            tty: self.state.tty,
            show_virtual_net: self.state.show_virtual_net,
            theme_background: self.state.theme_background,
            truecolor: self.state.truecolor,
        }
    }

    fn section_form(form: &SettingsForm, start: usize, len: usize, cursor: usize) -> SettingsForm {
        let rows = form.rows[start..start + len].to_vec();
        SettingsForm::new(rows)
            .with_cursor(cursor)
            .with_colors(
                form.selected_bg,
                form.selected_fg,
                form.normal_fg,
                form.inactive_fg,
                form.value_fg,
            )
    }
}
