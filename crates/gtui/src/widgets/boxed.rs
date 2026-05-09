//! Rounded-corner box with btop-style title slots.
//!
//! Anatomy (matches the screenshots in `Img/`):
//! ```text
//! ╭top_left──────────────────top_right╮
//! │           inner area              │
//! ╰bottom────────────────────────────╯
//! ```
//! - `top_left`: section title (e.g. `¹cpu`), drawn against the top border
//!   right after the corner.
//! - `top_right`: small controls (e.g. `<b br0 n>`, `auto zero`).
//! - `bottom`: keybind hint row (e.g. `+ select  info  terminate`).
//!
//! The widget delegates the actual border drawing to ratatui's `Block` for
//! correctness and adds the inline labels by writing into the buffer after
//! the block has rendered. The inner area returned by `block.inner(area)` is
//! what callers should use to position the inner widget.

use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::style::{Style, Stylize};
use ratatui::widgets::{Block, BorderType, Borders, Widget};

/// Border corner style. `Rounded` is btop's signature look and the gtop
/// default; `Square` is a config-selectable alternative for users on
/// fonts/terminals where rounded glyphs render poorly (some serial-console
/// setups, certain monospace fonts that lack the rounded variants).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum CornerStyle {
    #[default]
    Rounded,
    Square,
}

#[derive(Debug, Clone, Default)]
pub struct BoxedPanel {
    pub border_color: ratatui::style::Color,
    pub title_color: ratatui::style::Color,
    pub corner_style: CornerStyle,
    pub top_left: Option<String>,
    pub center: Option<String>,
    pub top_right: Option<String>,
    pub bottom: Option<String>,
    /// When true, force the original flat-title layout (single chrome row at
    /// top, no bubble cap/floor). Used by modal dialogs whose inner content
    /// is sized against the 2-row chrome budget — adding the bubble would
    /// eat 2 extra inner rows and overflow their hardcoded layouts.
    pub flat_title: bool,
}

impl BoxedPanel {
    pub fn new(border_color: ratatui::style::Color, title_color: ratatui::style::Color) -> Self {
        Self {
            border_color,
            title_color,
            corner_style: CornerStyle::default(),
            top_left: None,
            center: None,
            top_right: None,
            bottom: None,
            flat_title: false,
        }
    }

    /// Opt out of the bubble title layout for this panel. Used by modals.
    pub fn flat(mut self) -> Self {
        self.flat_title = true;
        self
    }

    pub fn with_corner_style(mut self, style: CornerStyle) -> Self {
        self.corner_style = style;
        self
    }

    pub fn with_title(mut self, title: impl Into<String>) -> Self {
        self.top_left = Some(title.into());
        self
    }

    pub fn with_center_title(mut self, title: impl Into<String>) -> Self {
        self.center = Some(title.into());
        self
    }

    pub fn with_controls(mut self, controls: impl Into<String>) -> Self {
        self.top_right = Some(controls.into());
        self
    }

    pub fn with_keybinds(mut self, hints: impl Into<String>) -> Self {
        self.bottom = Some(hints.into());
        self
    }

    /// The block we delegate border rendering to. Use `block.inner(area)` to
    /// get the area available to the inner content.
    pub fn block(&self) -> Block<'static> {
        let bt = match self.corner_style {
            CornerStyle::Rounded => BorderType::Rounded,
            CornerStyle::Square => BorderType::Plain,
        };
        Block::default()
            .borders(Borders::ALL)
            .border_type(bt)
            .border_style(Style::default().fg(self.border_color))
    }

    /// Where the actual rectangular block (corners + side borders) renders
    /// inside `area`. The bubble cap row sits ABOVE this rect; the bubble
    /// floor row eats the first row of the block's inner area. When `area`
    /// is too short for the bubble layout (cap + border + floor + ≥1 inner +
    /// bottom = 5 rows), we fall back to drawing the block at full `area`.
    fn block_area(&self, area: Rect) -> Rect {
        if !self.flat_title && area.height >= 5 {
            Rect::new(area.x, area.y + 1, area.width, area.height - 1)
        } else {
            area
        }
    }

    /// Convenience: returns the inner rect after the border is reserved.
    /// When the bubble layout is active, the floor row also eats one inner
    /// row, so callers see `(H - 4) × (W - 2)` of usable space (vs `H - 2`
    /// in flat mode).
    pub fn inner(&self, area: Rect) -> Rect {
        let block_inner = self.block().inner(self.block_area(area));
        if !self.flat_title && area.height >= 5 {
            // Skip the floor row.
            Rect::new(
                block_inner.x,
                block_inner.y + 1,
                block_inner.width,
                block_inner.height.saturating_sub(1),
            )
        } else {
            block_inner
        }
    }
}

pub fn panel(
    border_color: ratatui::style::Color,
    title_color: ratatui::style::Color,
    corner_style: CornerStyle,
) -> BoxedPanel {
    BoxedPanel::new(border_color, title_color).with_corner_style(corner_style)
}

impl Widget for &BoxedPanel {
    fn render(self, area: Rect, buf: &mut Buffer) {
        if area.width < 2 || area.height < 2 {
            return;
        }
        let block_area = self.block_area(area);
        let bubble_active = block_area.y > area.y;
        // Draw the rounded border first (at block_area, which is shifted down
        // by 1 when bubble layout is active so row `area.y` is free for caps).
        self.block().render(block_area, buf);

        let title_style = Style::default().fg(self.title_color).bold();
        // Bracket glyphs use the border color so the `┤ … ├` reads as part
        // of the frame, with the inner text in title color. Matches btop's
        // `div_left/div_right` look (btop_draw.cpp `├ … ┤`).
        let bracket_style = Style::default().fg(self.border_color);

        let title_y = block_area.top();
        let cap_y = if bubble_active { Some(area.top()) } else { None };
        let floor_y = if bubble_active { Some(block_area.top() + 1) } else { None };

        // Top-left: render starting one cell after the left corner. The
        // helper splits the input on the 2-space delimiter and wraps each
        // segment with `┤ … ├` brackets so the title reads as a row of
        // pill-style sections sitting on the top border.
        if let Some(s) = &self.top_left {
            let segs = write_segments_left(
                buf,
                area.left() + 1,
                title_y,
                s,
                title_style,
                bracket_style,
                area.right().saturating_sub(1),
            );
            decorate_bubble(buf, &segs, cap_y, floor_y, bracket_style);
        }
        // Top-right: same segment treatment, right-aligned.
        if let Some(s) = &self.top_right {
            let total = measured_segments_width(s);
            let right_edge = area.right().saturating_sub(1);
            let usable = right_edge.saturating_sub(area.left() + 1);
            if total <= usable {
                let start_x = right_edge.saturating_sub(total);
                let segs = write_segments_left(
                    buf,
                    start_x,
                    title_y,
                    s,
                    title_style,
                    bracket_style,
                    right_edge,
                );
                decorate_bubble(buf, &segs, cap_y, floor_y, bracket_style);
            }
        }
        if let Some(s) = &self.center {
            let total = measured_segments_width(s);
            if total > 0 && area.width > total + 2 {
                let start_x = area.left() + (area.width.saturating_sub(total)) / 2;
                let left_bound = self
                    .top_left
                    .as_ref()
                    .map(|s| area.left() + 1 + measured_segments_width(s))
                    .unwrap_or(area.left() + 1);
                let right_bound = self
                    .top_right
                    .as_ref()
                    .map(|s| {
                        area.right()
                            .saturating_sub(1)
                            .saturating_sub(measured_segments_width(s))
                    })
                    .unwrap_or(area.right().saturating_sub(1));
                if start_x > left_bound && start_x + total <= right_bound {
                    let segs = write_segments_left(
                        buf,
                        start_x,
                        title_y,
                        s,
                        title_style,
                        bracket_style,
                        right_bound,
                    );
                    decorate_bubble(buf, &segs, cap_y, floor_y, bracket_style);
                }
            }
        }
        // Bottom: render starting one cell after the left corner on the bottom border.
        // Bottom keybind row stays a flat label — no bubble decoration; that
        // would be too busy at the bottom of every panel.
        if let Some(s) = &self.bottom {
            let y = area.bottom().saturating_sub(1);
            write_segments_left(
                buf,
                area.left() + 1,
                y,
                s,
                title_style,
                bracket_style,
                area.right().saturating_sub(1),
            );
        }
    }
}

/// For each `(start_col, end_col)` in `segments`, draw a `╭───╮` cap on
/// `cap_y` (above the title row) and a `╰───╯` floor on `floor_y` (below the
/// title row). When either Y is `None` (e.g. the panel is too short for the
/// bubble layout), the corresponding decoration is skipped — title still
/// reads as a flat segmented row.
fn decorate_bubble(
    buf: &mut Buffer,
    segments: &[(u16, u16)],
    cap_y: Option<u16>,
    floor_y: Option<u16>,
    style: Style,
) {
    for &(start, end) in segments {
        if end <= start + 1 {
            continue;
        }
        if let Some(y) = cap_y {
            put(buf, start, y, '╭', style);
            for x in (start + 1)..end {
                put(buf, x, y, '─', style);
            }
            put(buf, end, y, '╮', style);
        }
        if let Some(y) = floor_y {
            put(buf, start, y, '╰', style);
            for x in (start + 1)..end {
                put(buf, x, y, '─', style);
            }
            put(buf, end, y, '╯', style);
        }
    }
}

/// Width that `write_segments_left` will consume for input `s`. The bracket
/// pair `┤ … ├` is 4 chrome cells; each interior segment join adds 3 cells
/// (` │ ` — space, divider, space).
fn measured_segments_width(s: &str) -> u16 {
    let parts: Vec<&str> = s.split("  ").filter(|p| !p.is_empty()).collect();
    if parts.is_empty() {
        return 0;
    }
    let text: usize = parts.iter().map(|p| p.chars().count()).sum();
    // 4 outer chrome (`┤ ` + ` ├`) + 3 cells per join (` │ `) between segments.
    let joins = parts.len().saturating_sub(1) * 3;
    (text + 4 + joins) as u16
}

/// Render `s` as a single pill spanning all its segments. Segments are split
/// on the `"  "` delimiter and joined with thin `│` dividers — visually
/// matches btop's `┃ cpu ┃ 5% ┃ load ┃` segmented title bar but uses the
/// light-weight glyph that aligns with our existing rounded border.
///
/// Returns the inclusive `(start_col, end_col)` range of the rendered pill
/// (the `┤` and `├` cells) so the caller can draw a single continuous
/// bubble cap and floor around the whole pill.
fn write_segments_left(
    buf: &mut Buffer,
    x: u16,
    y: u16,
    s: &str,
    text_style: Style,
    bracket_style: Style,
    right_limit: u16,
) -> Vec<(u16, u16)> {
    let parts: Vec<&str> = s.split("  ").filter(|p| !p.is_empty()).collect();
    if parts.is_empty() {
        return Vec::new();
    }
    // Need at least 5 cells (`┤ X ├`) for the smallest possible pill.
    if x + 4 >= right_limit {
        return Vec::new();
    }
    let pill_start = x;
    let mut col = x;
    put(buf, col, y, '┤', bracket_style);
    col += 1;
    put(buf, col, y, ' ', text_style);
    col += 1;

    for (i, seg) in parts.iter().enumerate() {
        if i > 0 {
            // Need at least ` │ X ├` ahead — 5 cells.
            if col + 4 >= right_limit {
                break;
            }
            put(buf, col, y, ' ', text_style);
            col += 1;
            put(buf, col, y, '│', bracket_style);
            col += 1;
            put(buf, col, y, ' ', text_style);
            col += 1;
        }
        for ch in seg.chars() {
            if col + 2 >= right_limit {
                break;
            }
            put(buf, col, y, ch, text_style);
            col += 1;
        }
    }

    if col + 2 > right_limit {
        // Out of room for the closing chrome — abort the whole pill so the
        // border stays consistent. The text we already wrote will get
        // overdrawn by the next `Block` render anyway when state advances.
        return Vec::new();
    }
    put(buf, col, y, ' ', text_style);
    col += 1;
    put(buf, col, y, '├', bracket_style);
    vec![(pill_start, col)]
}

fn put(buf: &mut Buffer, x: u16, y: u16, ch: char, style: Style) {
    if x >= buf.area.right() {
        return;
    }
    let cell = &mut buf[(x, y)];
    cell.set_char(ch);
    cell.set_style(style);
}

#[cfg(test)]
mod tests {
    use ratatui::style::Color;

    use super::*;

    /// Tall panels (>= 5 rows) trigger the bubble layout: corners shift down
    /// by one to make room for the cap row above the title.
    #[test]
    fn renders_rounded_corners_in_bubble_layout() {
        let panel = BoxedPanel::new(Color::Reset, Color::Reset);
        let area = Rect::new(0, 0, 10, 6);
        let mut buf = Buffer::empty(area);
        (&panel).render(area, &mut buf);
        // Top corners shifted to row 1 (cap row at row 0 stays empty in
        // panels with no title).
        assert_eq!(buf[(0, 1)].symbol(), "╭");
        assert_eq!(buf[(9, 1)].symbol(), "╮");
        assert_eq!(buf[(0, 5)].symbol(), "╰");
        assert_eq!(buf[(9, 5)].symbol(), "╯");
    }

    /// Short panels (< 5 rows) fall back to flat — no bubble decoration.
    #[test]
    fn flat_layout_for_short_panels() {
        let panel = BoxedPanel::new(Color::Reset, Color::Reset);
        let area = Rect::new(0, 0, 10, 4);
        let mut buf = Buffer::empty(area);
        (&panel).render(area, &mut buf);
        assert_eq!(buf[(0, 0)].symbol(), "╭");
        assert_eq!(buf[(9, 0)].symbol(), "╮");
        assert_eq!(buf[(0, 3)].symbol(), "╰");
        assert_eq!(buf[(9, 3)].symbol(), "╯");
    }

    #[test]
    fn square_corners_when_requested() {
        let panel = BoxedPanel::new(Color::Reset, Color::Reset)
            .with_corner_style(CornerStyle::Square);
        let area = Rect::new(0, 0, 10, 6);
        let mut buf = Buffer::empty(area);
        (&panel).render(area, &mut buf);
        // Bubble layout shifts corners to row 1 / row H-1.
        assert_eq!(buf[(0, 1)].symbol(), "┌");
        assert_eq!(buf[(9, 1)].symbol(), "┐");
        assert_eq!(buf[(0, 5)].symbol(), "└");
        assert_eq!(buf[(9, 5)].symbol(), "┘");
    }

    /// Diagnostic dump — prints all five top rows that the current build
    /// produces for a representative gtop title. Run with `--nocapture`.
    #[test]
    fn dump_real_title_top_rows() {
        let panel = BoxedPanel::new(Color::Reset, Color::Reset)
            .with_title("⁴proc  247 procs  group:flat  rx/tx: ebpf");
        let area = Rect::new(0, 0, 70, 6);
        let mut buf = Buffer::empty(area);
        (&panel).render(area, &mut buf);
        eprintln!();
        for y in 0..area.height {
            let row: String = (0..area.width).map(|x| buf[(x, y)].symbol().to_string()).collect();
            eprintln!("[snapshot row {y}] |{}|", row);
        }
        eprintln!();
    }

    #[test]
    fn single_segment_renders_one_pill_with_bubble() {
        let panel = BoxedPanel::new(Color::Reset, Color::Reset).with_title("cpu");
        let area = Rect::new(0, 0, 20, 6);
        let mut buf = Buffer::empty(area);
        (&panel).render(area, &mut buf);
        // Title row 1: `┤ cpu ├` at cols 1..=7.
        assert_eq!(buf[(1, 1)].symbol(), "┤");
        assert_eq!(buf[(7, 1)].symbol(), "├");
        // Cap row above + floor row below span the same range.
        assert_eq!(buf[(1, 0)].symbol(), "╭");
        assert_eq!(buf[(7, 0)].symbol(), "╮");
        assert_eq!(buf[(1, 2)].symbol(), "╰");
        assert_eq!(buf[(7, 2)].symbol(), "╯");
    }

    #[test]
    fn center_title_renders_in_the_header_middle() {
        let panel = BoxedPanel::new(Color::Reset, Color::Reset)
            .with_title("cpu")
            .with_center_title("2026-05-04 12:34:56 utc")
            .with_controls("- 250ms +");
        let area = Rect::new(0, 0, 60, 6);
        let mut buf = Buffer::empty(area);
        (&panel).render(area, &mut buf);
        let header: String = (0..area.width)
            .map(|x| buf[(x, 1)].symbol().to_string())
            .collect();
        assert!(
            header.contains("┤") && header.contains("2026-05-04 12:34:56 utc") && header.contains("├"),
            "center bubble missing: {header}"
        );
    }

    #[test]
    fn multi_segment_renders_one_continuous_pill() {
        // Two segments joined by ` │ ` divider inside a single pill.
        // `┤ cpu │ 5% ├` is 12 chars wide → cols 1..=12.
        let panel = BoxedPanel::new(Color::Reset, Color::Reset).with_title("cpu  5%");
        let area = Rect::new(0, 0, 20, 6);
        let mut buf = Buffer::empty(area);
        (&panel).render(area, &mut buf);
        assert_eq!(buf[(1, 1)].symbol(), "┤");
        assert_eq!(buf[(2, 1)].symbol(), " ");
        assert_eq!(buf[(3, 1)].symbol(), "c");
        assert_eq!(buf[(6, 1)].symbol(), " ");
        assert_eq!(buf[(7, 1)].symbol(), "│", "single light divider between segments");
        assert_eq!(buf[(8, 1)].symbol(), " ");
        assert_eq!(buf[(9, 1)].symbol(), "5");
        assert_eq!(buf[(12, 1)].symbol(), "├");
        // Bubble cap and floor span the entire pill, not per-segment.
        assert_eq!(buf[(1, 0)].symbol(), "╭");
        assert_eq!(buf[(12, 0)].symbol(), "╮");
        for x in 2..12 {
            assert_eq!(buf[(x, 0)].symbol(), "─", "cap at col {x}");
        }
        assert_eq!(buf[(1, 2)].symbol(), "╰");
        assert_eq!(buf[(12, 2)].symbol(), "╯");
    }

    #[test]
    fn inner_skips_cap_border_and_floor_rows() {
        let panel = BoxedPanel::default();
        // Height 7: cap=0, top border=1, floor=2, inner=3..=5, bottom=6.
        let area = Rect::new(0, 0, 10, 7);
        let inner = panel.inner(area);
        assert_eq!(inner, Rect::new(1, 3, 8, 3));
    }

    #[test]
    fn inner_in_flat_mode_matches_block_inner() {
        let panel = BoxedPanel::default();
        let area = Rect::new(0, 0, 10, 4);
        let inner = panel.inner(area);
        // Flat fallback: just the block's inner (top+bottom border excluded).
        assert_eq!(inner, Rect::new(1, 1, 8, 2));
    }
}
