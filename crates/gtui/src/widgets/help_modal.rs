//! Centered help / keybind reference modal.
//!
//! Shared between apps so the "?" overlay always looks the same:
//! optional banner at the top, two-column `key  description` lines
//! in the middle, [`keybind_footer`] chip row at the bottom.
//!
//! ```ignore
//! HelpModal::new(&theme, "gtop", &HELP_LINES)
//!     .with_banner(banner)             // optional
//!     .with_actions(vec![
//!         ("Esc".into(), "close".into()),
//!         ("q".into(), "quit".into()),
//!     ])
//!     .render(frame, area);
//! ```
//!
//! Sizing is auto: width fits the longest line (or banner) within
//! `[min_width, area.width-4]`; height fits banner + lines + footer
//! within `[8, area.height-2]`. When a single vertical list would
//! truncate and the modal is wide enough, entries flow into two
//! side-by-side key/description columns.

use ratatui::layout::Rect;
use ratatui::style::Style;
use ratatui::Frame;

use crate::widgets::braille_text::BrailleText;
use crate::widgets::{BoxedPanel, CornerStyle, ModalShell};
use crate::{write_str_clipped, Theme};

pub struct HelpModal<'a> {
    theme: &'a Theme,
    title: String,
    lines: &'a [(&'a str, &'a str)],
    /// When set, the widget builds a [`BrailleText`] banner from this
    /// text using the modal's [`Theme`] so styling is identical across
    /// apps. Prefer this over [`Self::with_banner`] — the explicit
    /// override exists for advanced callers that want a different
    /// gradient or rule character, but using it means the two apps'
    /// banners can drift.
    banner_text: Option<String>,
    banner_override: Option<BrailleText<'a>>,
    /// Chip row across the footer. Defaults to `Esc close` if not set.
    actions: Vec<(String, String)>,
    corner: CornerStyle,
    min_width: u16,
}

impl<'a> HelpModal<'a> {
    pub fn new(
        theme: &'a Theme,
        title: impl Into<String>,
        lines: &'a [(&'a str, &'a str)],
    ) -> Self {
        Self {
            theme,
            title: title.into(),
            lines,
            banner_text: None,
            banner_override: None,
            actions: vec![("Esc".into(), "close".into())],
            corner: CornerStyle::default(),
            min_width: 40,
        }
    }

    /// Recommended: pass the text only and let the widget style it
    /// from the current theme. Guarantees both apps produce visually
    /// identical banners for the same theme.
    pub fn with_banner_text(mut self, text: impl Into<String>) -> Self {
        self.banner_text = Some(text.into());
        self
    }

    /// Escape hatch — fully-styled banner overrides the auto-built one.
    /// Use only when an app needs a non-default look; keeping the
    /// banners shared is the whole point of this widget.
    pub fn with_banner(mut self, banner: BrailleText<'a>) -> Self {
        self.banner_override = Some(banner);
        self
    }

    pub fn with_actions(mut self, actions: Vec<(String, String)>) -> Self {
        self.actions = actions;
        self
    }

    pub fn with_corner(mut self, corner: CornerStyle) -> Self {
        self.corner = corner;
        self
    }

    pub fn with_min_width(mut self, min: u16) -> Self {
        self.min_width = min;
        self
    }

    pub fn render(&self, frame: &mut Frame<'_>, area: Rect) {
        // Build the banner once. Auto-style from theme when only
        // `banner_text` is set — that's how both apps stay visually
        // identical for a given theme. The override path exists but
        // is intentionally awkward to discourage divergence.
        let auto_banner = self.banner_text.as_deref().map(|t| auto_banner(t, self.theme));
        let banner: Option<&BrailleText<'_>> = self
            .banner_override
            .as_ref()
            .or(auto_banner.as_ref());

        // ── Sizing.
        let key_w = self
            .lines
            .iter()
            .map(|(k, _)| k.chars().count())
            .max()
            .unwrap_or(8) as u16;
        let longest_line: u16 = self
            .lines
            .iter()
            .map(|(k, d)| (k.chars().count() + d.chars().count() + 4) as u16)
            .max()
            .unwrap_or(40);
        let banner_intrinsic_w = banner.map(|b| b.intrinsic_width()).unwrap_or(0);
        let two_col_w = longest_line.saturating_mul(2).saturating_add(8);
        let natural_w: u16 = longest_line.max(banner_intrinsic_w + 4).max(two_col_w) + 4;
        let max_w = area.width.saturating_sub(4).max(self.min_width);
        let want_w = natural_w.max(self.min_width).min(max_w);

        let banner_h = banner.map(|b| b.height()).unwrap_or(0);
        // Banner reserves: banner_h + 1 row gap below.
        let banner_block_h: u16 = if banner_h > 0 { banner_h + 1 } else { 0 };
        // Body needs: banner_block + N lines + footer block (3 rows: spacer + divider + chips) + 2 (top/bot border).
        let footer_block_h: u16 = 3;
        let max_h = area.height.saturating_sub(2).max(8);
        let want_h =
            (self.lines.len() as u16 + banner_block_h + footer_block_h + 2).min(max_h);

        // Decide up front whether the banner will fit: the body rect
        // is just `want_w - 2` × `want_h - 2`. We need this *before*
        // building the panel so we can suppress the center-title pill
        // when the banner will render — the banner IS the title in
        // that case, and showing both is redundant noise.
        let prospective_body_h = want_h.saturating_sub(2);
        let prospective_body_w = want_w.saturating_sub(2);
        let banner_will_show = banner.is_some()
            && prospective_body_h >= banner_block_h + 5
            && prospective_body_w >= banner_intrinsic_w + 4;

        // ── Chrome.
        let mut panel = BoxedPanel::new(self.theme.title, self.theme.title)
            .with_corner_style(self.corner)
            .flat();
        if !banner_will_show {
            panel = panel.with_center_title(self.title.clone());
        }
        let bg = self.theme.main_bg.unwrap_or(self.theme.meter_bg);
        let Some(body) = ModalShell::new(panel, want_w, want_h)
            .with_fill(Style::default().bg(bg).fg(self.theme.main_fg))
            .render(frame, area)
        else {
            return;
        };
        if body.height == 0 {
            return;
        }

        // ── Banner.
        let show_banner = banner_will_show
            && body.height >= banner_block_h + 5
            && body.width >= banner_intrinsic_w + 4;
        let key_rows_y = if show_banner {
            let banner = banner.unwrap();
            let banner_x = body.x + 2;
            let banner_w = body.width.saturating_sub(4);
            frame.render_widget(banner, Rect::new(banner_x, body.y, banner_w, banner_h));
            body.y + banner_block_h
        } else {
            body.y + 1
        };

        let lines_avail = body
            .y
            .saturating_add(body.height)
            .saturating_sub(key_rows_y)
            .saturating_sub(footer_block_h) as usize;
        let single_desc_x = body.x + 2 + key_w + 2;
        let single_desc_w = body
            .x
            .saturating_add(body.width)
            .saturating_sub(single_desc_x)
            .saturating_sub(2);
        let buf = frame.buffer_mut();
        let can_two_col = self.lines.len() > lines_avail
            && body.width >= two_col_w.min(area.width.saturating_sub(4));
        if can_two_col {
            let col_gap = 4;
            let col_w = body.width.saturating_sub(4 + col_gap) / 2;
            let rows_per_col = lines_avail.max(1);
            for (i, (key, desc)) in self.lines.iter().take(rows_per_col * 2).enumerate() {
                let col = (i / rows_per_col) as u16;
                let row = (i % rows_per_col) as u16;
                let x = body.x + 2 + col * (col_w + col_gap);
                let row_y = key_rows_y + row;
                let desc_x = x + key_w + 2;
                let desc_w = col_w.saturating_sub(key_w + 2);
                write_str_clipped(
                    buf,
                    x,
                    row_y,
                    key,
                    key_w,
                    Style::default().bg(bg).fg(self.theme.hi_fg),
                );
                write_str_clipped(
                    buf,
                    desc_x,
                    row_y,
                    desc,
                    desc_w,
                    Style::default().bg(bg).fg(self.theme.main_fg),
                );
            }
        } else {
            for (i, (key, desc)) in self.lines.iter().take(lines_avail).enumerate() {
                let row_y = key_rows_y + i as u16;
                write_str_clipped(
                    buf,
                    body.x + 2,
                    row_y,
                    key,
                    key_w,
                    Style::default().bg(bg).fg(self.theme.hi_fg),
                );
                write_str_clipped(
                    buf,
                    single_desc_x,
                    row_y,
                    desc,
                    single_desc_w,
                    Style::default().bg(bg).fg(self.theme.main_fg),
                );
            }
        }

        // ── Footer chips. Mirror OptionsMenu's footer for visual parity.
        if body.height >= footer_block_h + 2 {
            let chip_refs: Vec<(&str, &str)> = self
                .actions
                .iter()
                .map(|(k, d)| (k.as_str(), d.as_str()))
                .collect();
            let divider_y = body.bottom().saturating_sub(footer_block_h - 1);
            crate::widgets::keybind_footer::render(
                frame, body, self.theme, bg, &chip_refs, divider_y,
            );
        }
    }
}

/// Build the auto-styled banner used when callers pass only
/// [`HelpModal::with_banner_text`]. Single source of truth for help
/// banner styling — both apps render the same look from the same
/// theme. Uses the theme's `cpu` gradient so letters sweep across the
/// btop-defined start → mid → end stops, giving the high-contrast
/// look users expect from a system-monitor app's banner.
fn auto_banner<'a>(text: &'a str, theme: &Theme) -> BrailleText<'a> {
    BrailleText::new(text)
        .with_style(Style::default().fg(theme.hi_fg))
        .with_gradient(theme.cpu)
        .with_rule('━', '─')
}
