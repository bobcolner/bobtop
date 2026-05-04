//! Typed settings form for modal config screens.

use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::style::{Color, Style};
use ratatui::widgets::Widget;

use crate::truncate_chars;

#[derive(Debug, Clone)]
pub enum SettingValue {
    Text(String),
    Choice {
        options: Vec<String>,
        selected: usize,
    },
    Number {
        value: u64,
        step: u64,
        min: u64,
        max: u64,
        suffix: String,
    },
    Toggle(bool),
}

#[derive(Debug, Clone)]
pub struct SettingRow {
    pub label: String,
    pub value: SettingValue,
}

impl SettingRow {
    pub fn text(label: impl Into<String>, value: impl Into<String>) -> Self {
        Self {
            label: label.into(),
            value: SettingValue::Text(value.into()),
        }
    }

    pub fn choice(
        label: impl Into<String>,
        options: Vec<String>,
        selected: usize,
    ) -> Self {
        Self {
            label: label.into(),
            value: SettingValue::Choice { options, selected },
        }
    }

    pub fn number(
        label: impl Into<String>,
        value: u64,
        step: u64,
        min: u64,
        max: u64,
        suffix: impl Into<String>,
    ) -> Self {
        Self {
            label: label.into(),
            value: SettingValue::Number {
                value,
                step,
                min,
                max,
                suffix: suffix.into(),
            },
        }
    }

    pub fn toggle(label: impl Into<String>, enabled: bool) -> Self {
        Self {
            label: label.into(),
            value: SettingValue::Toggle(enabled),
        }
    }
}

#[derive(Debug, Clone)]
pub struct SettingsForm {
    pub rows: Vec<SettingRow>,
    pub cursor: usize,
    pub selected_bg: Color,
    pub selected_fg: Color,
    pub normal_fg: Color,
    pub inactive_fg: Color,
    pub value_fg: Color,
}

impl SettingsForm {
    pub fn new(rows: Vec<SettingRow>) -> Self {
        Self {
            rows,
            cursor: 0,
            selected_bg: Color::Reset,
            selected_fg: Color::Reset,
            normal_fg: Color::Reset,
            inactive_fg: Color::Reset,
            value_fg: Color::Reset,
        }
    }

    pub fn with_cursor(mut self, cursor: usize) -> Self {
        self.cursor = cursor;
        self
    }

    pub fn with_colors(
        mut self,
        selected_bg: Color,
        selected_fg: Color,
        normal_fg: Color,
        inactive_fg: Color,
        value_fg: Color,
    ) -> Self {
        self.selected_bg = selected_bg;
        self.selected_fg = selected_fg;
        self.normal_fg = normal_fg;
        self.inactive_fg = inactive_fg;
        self.value_fg = value_fg;
        self
    }

    pub fn move_cursor(&mut self, delta: i32) {
        if self.rows.is_empty() {
            self.cursor = 0;
            return;
        }
        let len = self.rows.len() as i32;
        let cur = self.cursor as i32;
        self.cursor = (((cur + delta) % len + len) % len) as usize;
    }

    pub fn cycle_current(&mut self, delta: i32) -> bool {
        let Some(row) = self.rows.get_mut(self.cursor) else { return false };
        match &mut row.value {
            SettingValue::Choice { options, selected } => {
                if options.is_empty() {
                    return false;
                }
                let len = options.len() as i32;
                let cur = *selected as i32;
                *selected = ((cur + delta) % len + len) as usize % options.len();
                true
            }
            SettingValue::Number {
                value,
                step,
                min,
                max,
                ..
            } => {
                let next = (*value as i64 + (*step as i64 * delta as i64))
                    .clamp(*min as i64, *max as i64) as u64;
                if next != *value {
                    *value = next;
                    true
                } else {
                    false
                }
            }
            SettingValue::Toggle(on) => {
                if delta != 0 {
                    *on = !*on;
                    true
                } else {
                    false
                }
            }
            SettingValue::Text(_) => false,
        }
    }

    pub fn current_is_choice(&self) -> bool {
        matches!(
            self.rows.get(self.cursor).map(|r| &r.value),
            Some(SettingValue::Choice { .. }) | Some(SettingValue::Number { .. }) | Some(SettingValue::Toggle(_))
        )
    }
}

impl Widget for &SettingsForm {
    fn render(self, area: Rect, buf: &mut Buffer) {
        if area.width == 0 || area.height == 0 {
            return;
        }
        let max_rows = area.height as usize;
        let label_width = self
            .rows
            .iter()
            .map(|r| r.label.chars().count())
            .max()
            .unwrap_or(8) as u16;
        for (i, row) in self.rows.iter().take(max_rows).enumerate() {
            let y = area.y + i as u16;
            let is_cursor = i == self.cursor;
            if is_cursor {
                for xx in area.x..area.x + area.width {
                    buf[(xx, y)].set_style(Style::default().bg(self.selected_bg));
                }
            }
            let prefix = if is_cursor { "▶ " } else { "  " };
            match &row.value {
                SettingValue::Text(v) => {
                    let label = truncate_chars(&row.label, label_width as usize);
                    let value = truncate_chars(v, area.width.saturating_sub(label_width + 8) as usize);
                    let style = if is_cursor {
                        Style::default().bg(self.selected_bg).fg(self.selected_fg)
                    } else {
                        Style::default().fg(self.normal_fg)
                    };
                    let line = format!("{prefix}{:<width$}  ◀ {value} ▶", label, width = label_width as usize);
                    write_line(buf, area.x, y, &line, style);
                }
                SettingValue::Choice { options, selected } => {
                    let label = truncate_chars(&row.label, label_width as usize);
                    let value = options.get(*selected).cloned().unwrap_or_default();
                    let value = truncate_chars(&value, area.width.saturating_sub(label_width + 8) as usize);
                    let style = if is_cursor {
                        Style::default().bg(self.selected_bg).fg(self.selected_fg)
                    } else {
                        Style::default().fg(self.normal_fg)
                    };
                    let line = format!("{prefix}{:<width$}  ◀ {value} ▶", label, width = label_width as usize);
                    write_line(buf, area.x, y, &line, style);
                }
                SettingValue::Number {
                    value,
                    suffix,
                    ..
                } => {
                    let label = truncate_chars(&row.label, label_width as usize);
                    let value = format!("{value}{suffix}");
                    let value = truncate_chars(&value, area.width.saturating_sub(label_width + 8) as usize);
                    let style = if is_cursor {
                        Style::default().bg(self.selected_bg).fg(self.selected_fg)
                    } else {
                        Style::default().fg(self.normal_fg)
                    };
                    let line = format!("{prefix}{:<width$}  ◀ {value} ▶", label, width = label_width as usize);
                    write_line(buf, area.x, y, &line, style);
                }
                SettingValue::Toggle(on) => {
                    let label = truncate_chars(&row.label, area.width.saturating_sub(8) as usize);
                    let style = if is_cursor {
                        Style::default().bg(self.selected_bg).fg(self.selected_fg)
                    } else if *on {
                        Style::default().fg(self.normal_fg)
                    } else {
                        Style::default().fg(self.inactive_fg)
                    };
                    let mark = if *on { "[x]" } else { "[ ]" };
                    let line = format!("{prefix}{mark}  {label}");
                    write_line(buf, area.x, y, &line, style);
                }
            }
        }
    }
}

fn write_line(buf: &mut Buffer, x: u16, y: u16, s: &str, style: Style) {
    let mut col = x;
    let right = buf.area.right();
    for ch in s.chars() {
        if col >= right {
            break;
        }
        let cell = &mut buf[(col, y)];
        cell.set_char(ch);
        cell.set_style(style);
        col = col.saturating_add(1);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ratatui::buffer::Buffer;

    #[test]
    fn renders_toggle_and_text_rows() {
        let form = SettingsForm::new(vec![
            SettingRow::text("theme", "dark"),
            SettingRow::toggle("tty", true),
        ])
        .with_cursor(1);
        let area = Rect::new(0, 0, 32, 2);
        let mut buf = Buffer::empty(area);
        (&form).render(area, &mut buf);
        assert_eq!(buf[(0, 1)].symbol(), "▶");
        assert_eq!(buf[(3, 1)].symbol(), "[");
        assert_eq!(buf[(4, 1)].symbol(), "x");
    }

    #[test]
    fn cycles_choice_and_number() {
        let mut form = SettingsForm::new(vec![
            SettingRow::choice("theme", vec!["a".into(), "b".into()], 0),
            SettingRow::number("tick", 100, 50, 0, 200, " ms"),
        ]);
        assert!(form.cycle_current(1));
        if let SettingValue::Choice { selected, .. } = &form.rows[0].value {
            assert_eq!(*selected, 1);
        } else {
            panic!("expected choice");
        }
        form.move_cursor(1);
        assert!(form.cycle_current(1));
        if let SettingValue::Number { value, .. } = &form.rows[1].value {
            assert_eq!(*value, 150);
        } else {
            panic!("expected number");
        }
    }
}
