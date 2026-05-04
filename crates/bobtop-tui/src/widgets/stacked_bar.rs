//! Multi-segment horizontal stacked bar — composable chart-library piece
//! for "this whole quantity is split N ways" data.
//!
//! Use cases:
//! - Memory breakdown: Used / Cached / Buffers / Free against total.
//! - Disk pool view: per-mount used proportions of a backing pool.
//! - Network share: bytes-by-protocol or by-interface.
//!
//! Layout (single-row form):
//! ```text
//! Memory  ████████░░░▒▒▒▒▒▒▒▒▒▒▒▒░░░░░  31.2 / 64.0 GiB
//! ```
//!
//! Layout (with legend, 2-row form when `with_legend(true)`):
//! ```text
//! Memory  ████████░░░▒▒▒▒▒▒▒▒▒▒▒▒░░░░░  31.2 / 64.0 GiB
//!         ■ used 12.4G   ■ cached 24.0G   ■ free 27.6G
//! ```
//!
//! Each segment is a `(label, fraction, color)`; fractions should sum to
//! `<= 1.0` (anything past 1.0 is clipped at the right edge — defensive,
//! since the kernel can briefly report inconsistent /proc/meminfo lines).
//!
//! Renders pure block/half-block glyphs (`█▌`) — works in any terminal
//! with basic Unicode, no braille requirement. Color is the entire signal.

use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::style::{Color, Style};
use ratatui::widgets::Widget;

/// One slice of the stacked bar.
#[derive(Debug, Clone)]
pub struct StackedSegment {
    pub label: String,
    /// 0.0..=1.0. The widget clips at 1.0 cumulative across all segments.
    pub fraction: f64,
    /// Color for the segment block. Typically a theme gradient endpoint
    /// (`theme.used.end`, `theme.cached.end`, etc.) so segments read as
    /// "this is the used color" at a glance.
    pub color: Color,
    /// Optional value text shown next to the segment label in the legend
    /// row (e.g. `12.4 GiB`). Skipped when the bar has no legend.
    pub value: Option<String>,
}

impl StackedSegment {
    pub fn new(label: impl Into<String>, fraction: f64, color: Color) -> Self {
        Self {
            label: label.into(),
            fraction: fraction.clamp(0.0, 1.0),
            color,
            value: None,
        }
    }

    pub fn with_value(mut self, v: impl Into<String>) -> Self {
        self.value = Some(v.into());
        self
    }
}

/// `StackedBar` widget. Single row by default; opt into a legend row below
/// with `with_legend(true)`. Keep it simple — no value labels inline on the
/// bar itself (those go in the legend or the right-side value text), so the
/// bar stays clean even at small widths.
pub struct StackedBar<'a> {
    pub segments: &'a [StackedSegment],
    /// Label rendered to the LEFT of the bar (e.g. `"Memory"`). Empty by
    /// default.
    pub label: Option<String>,
    /// Free-form text rendered to the RIGHT of the bar (e.g.
    /// `"31.2 / 64.0 GiB"`). Empty by default.
    pub value_text: Option<String>,
    /// Background color for empty cells. Typically `theme.meter_bg`.
    pub empty_bg: Color,
    /// Foreground color for chrome (label + value text). Typically
    /// `theme.main_fg`.
    pub chrome_fg: Color,
    /// Render a legend row below the bar listing each segment's color
    /// square + label + value.
    pub show_legend: bool,
    /// Style of the legend row when `show_legend` is true. `List` packs
    /// `■ label value` left-to-right (the original behavior). `Aligned`
    /// positions each segment's label UNDER its colored block on the bar
    /// — gives a true "bar chart with axis labels" feel.
    pub legend_style: LegendStyle,
}

/// How `StackedBar` lays out its legend row.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum LegendStyle {
    /// Compact left-to-right list of `■ name value`. Default.
    #[default]
    List,
    /// Each label sits in the column range of its segment. Labels truncate
    /// or drop entirely when their segment is too narrow to fit them.
    Aligned,
}

impl<'a> StackedBar<'a> {
    pub fn new(segments: &'a [StackedSegment]) -> Self {
        Self {
            segments,
            label: None,
            value_text: None,
            empty_bg: Color::Rgb(40, 42, 54),
            chrome_fg: Color::Rgb(248, 248, 242),
            show_legend: false,
            legend_style: LegendStyle::default(),
        }
    }

    pub fn with_legend_style(mut self, s: LegendStyle) -> Self {
        self.legend_style = s;
        self
    }

    pub fn with_label(mut self, s: impl Into<String>) -> Self {
        self.label = Some(s.into());
        self
    }

    pub fn with_value_text(mut self, s: impl Into<String>) -> Self {
        self.value_text = Some(s.into());
        self
    }

    pub fn with_empty_bg(mut self, c: Color) -> Self {
        self.empty_bg = c;
        self
    }

    pub fn with_chrome_fg(mut self, c: Color) -> Self {
        self.chrome_fg = c;
        self
    }

    pub fn with_legend(mut self, on: bool) -> Self {
        self.show_legend = on;
        self
    }
}

impl<'a> Widget for &StackedBar<'a> {
    fn render(self, area: Rect, buf: &mut Buffer) {
        if area.width == 0 || area.height == 0 {
            return;
        }
        let chrome = Style::default().fg(self.chrome_fg);

        // Reserve label + value chrome on the bar row. Label gets its full
        // length + 1-cell gutter; value gets its full length + 1-cell gutter
        // before the right edge.
        let label_w = self
            .label
            .as_deref()
            .map(|s| s.chars().count() as u16 + 2)
            .unwrap_or(0);
        let value_w = self
            .value_text
            .as_deref()
            .map(|s| s.chars().count() as u16 + 2)
            .unwrap_or(0);
        let bar_x = area.x + label_w;
        let bar_w = area.width.saturating_sub(label_w).saturating_sub(value_w);
        let bar_y = area.y;

        if let Some(s) = &self.label {
            write_str(buf, area.x, bar_y, s, chrome);
        }
        if let Some(s) = &self.value_text {
            let len = s.chars().count() as u16;
            let x = area.right().saturating_sub(len);
            write_str(buf, x, bar_y, s, chrome);
        }

        // Build per-segment cell counts. Use eighth-of-cell precision so
        // small slices still render visibly — converted to (full_cells,
        // remainder_eighths) per segment.
        if bar_w > 0 {
            render_bar_segments(buf, bar_x, bar_y, bar_w, self.segments, self.empty_bg);
        }

        if self.show_legend && area.height >= 2 {
            match self.legend_style {
                LegendStyle::List => {
                    render_legend(buf, area.x, bar_y + 1, area.width, self.segments, chrome);
                }
                LegendStyle::Aligned => {
                    render_legend_aligned(
                        buf,
                        bar_x,
                        bar_y + 1,
                        bar_w,
                        self.segments,
                        chrome,
                    );
                }
            }
        }
    }
}

/// Place each segment's `name value` label inside that segment's column
/// range (left-aligned within the slot). Skips segments whose slot can't
/// fit even a 4-char label — better to drop than to render garbled stubs.
fn render_legend_aligned(
    buf: &mut Buffer,
    bar_x: u16,
    y: u16,
    bar_w: u16,
    segments: &[StackedSegment],
    chrome: Style,
) {
    let total_eighths = (bar_w as usize) * 8;
    let mut consumed: usize = 0;
    for seg in segments {
        let want = (seg.fraction * total_eighths as f64).round() as usize;
        let slot_eighths = want.min(total_eighths.saturating_sub(consumed));
        if slot_eighths == 0 {
            continue;
        }
        let start_cell = (consumed + 4) / 8; // round to nearest cell start
        let end_cell = ((consumed + slot_eighths) / 8).min(bar_w as usize);
        let slot_w = end_cell.saturating_sub(start_cell);
        consumed += slot_eighths;
        if slot_w < 4 {
            continue;
        }
        // `■ Name value` — color square inherits segment color, text is
        // chrome. Truncate to slot width with ellipsis when value is long.
        let value = seg.value.as_deref().unwrap_or("");
        let want_text = if value.is_empty() {
            seg.label.clone()
        } else {
            format!("{} {}", seg.label, value)
        };
        let mut col = bar_x + start_cell as u16;
        // Color marker.
        let cell = &mut buf[(col, y)];
        cell.set_char('■');
        cell.set_style(Style::default().fg(seg.color));
        col = col.saturating_add(1);
        let cell = &mut buf[(col, y)];
        cell.set_char(' ');
        cell.set_style(chrome);
        col = col.saturating_add(1);
        // Text — truncate with `…` when over budget.
        let avail = (slot_w as usize).saturating_sub(2);
        let text = truncate_to(&want_text, avail);
        write_str_n(buf, col, y, &text, avail, chrome);
    }
}

fn truncate_to(s: &str, max: usize) -> String {
    let count = s.chars().count();
    if count <= max {
        s.to_string()
    } else if max == 0 {
        String::new()
    } else {
        let mut out: String = s.chars().take(max - 1).collect();
        out.push('…');
        out
    }
}

/// Eighth-block ramp for the trailing partial cell of each segment so we
/// don't snap to whole cells (which makes <2% slices invisible).
const LEFT_EIGHTHS: [char; 9] = [' ', '▏', '▎', '▍', '▌', '▋', '▊', '▉', '█'];

fn render_bar_segments(
    buf: &mut Buffer,
    x: u16,
    y: u16,
    bar_w: u16,
    segments: &[StackedSegment],
    empty_bg: Color,
) {
    // First fill the whole bar with empty-bg blocks so any unfilled tail
    // (sum < 1.0) reads as "free space".
    for col in 0..bar_w {
        let cell = &mut buf[(x + col, y)];
        cell.set_char('█');
        cell.set_style(Style::default().fg(empty_bg));
    }
    // Walk segments left-to-right. `consumed_eighths` is our accumulator
    // in 1/8-cell units — converts to (cell, sub) for any boundary.
    let total_eighths = (bar_w as usize) * 8;
    let mut consumed: usize = 0;
    for seg in segments {
        if consumed >= total_eighths {
            break;
        }
        let want = (seg.fraction * total_eighths as f64).round() as usize;
        let take = want.min(total_eighths - consumed);
        if take == 0 {
            continue;
        }
        let start_cell = consumed / 8;
        let start_sub = consumed % 8;
        let end_cell = (consumed + take) / 8;
        let end_sub = (consumed + take) % 8;

        // Full blocks in the interior.
        for cell_x in start_cell..end_cell {
            if cell_x as u16 >= bar_w {
                break;
            }
            let cell = &mut buf[(x + cell_x as u16, y)];
            cell.set_char('█');
            cell.set_style(Style::default().fg(seg.color));
        }
        // Trailing partial cell at end_cell (if end_sub > 0) — half-block
        // glyph painted with seg.color over an empty_bg background so the
        // remainder shows as the next-segment-or-empty color.
        if end_sub > 0 && (end_cell as u16) < bar_w {
            let cell = &mut buf[(x + end_cell as u16, y)];
            cell.set_char(LEFT_EIGHTHS[end_sub]);
            cell.set_style(Style::default().fg(seg.color).bg(empty_bg));
        }
        // Leading partial of this segment (when the previous segment ended
        // mid-cell) — set the BG of that boundary cell to this segment's
        // color so the half-block from prev seg sits on top of THIS color.
        // Skipped on the first segment since there's no prev.
        if start_sub > 0 && start_cell == (consumed.saturating_sub(start_sub) / 8) {
            // Already painted by the previous segment; we just need to
            // restyle the BG. The glyph stays as the previous seg's
            // partial fill, fg=prev color, now bg=this color.
            if (start_cell as u16) < bar_w {
                let cell = &mut buf[(x + start_cell as u16, y)];
                let style = cell.style().bg(seg.color);
                cell.set_style(style);
            }
        }
        consumed += take;
    }
}

fn render_legend(
    buf: &mut Buffer,
    x: u16,
    y: u16,
    width: u16,
    segments: &[StackedSegment],
    chrome: Style,
) {
    let mut col = x;
    let right = x + width;
    for seg in segments {
        // "■ label valueoptional   " per segment.
        if col >= right {
            break;
        }
        // Color square.
        let cell = &mut buf[(col, y)];
        cell.set_char('■');
        cell.set_style(Style::default().fg(seg.color));
        col = col.saturating_add(1);
        if col >= right {
            break;
        }
        // Space + label.
        let mut text = format!(" {}", seg.label);
        if let Some(v) = &seg.value {
            text.push(' ');
            text.push_str(v);
        }
        text.push_str("   ");
        let len = text.chars().count() as u16;
        let cap = right.saturating_sub(col).min(len);
        write_str_n(buf, col, y, &text, cap as usize, chrome);
        col = col.saturating_add(cap);
    }
}

fn write_str(buf: &mut Buffer, x: u16, y: u16, s: &str, style: Style) {
    write_str_n(buf, x, y, s, s.chars().count(), style);
}

fn write_str_n(buf: &mut Buffer, x: u16, y: u16, s: &str, max: usize, style: Style) {
    let mut col = x;
    let right = buf.area.right();
    for ch in s.chars().take(max) {
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

    fn rgb(r: u8, g: u8, b: u8) -> Color {
        Color::Rgb(r, g, b)
    }

    #[test]
    fn empty_segments_render_only_empty_bg() {
        let bar = StackedBar::new(&[]).with_empty_bg(rgb(10, 10, 10));
        let area = Rect::new(0, 0, 8, 1);
        let mut buf = Buffer::empty(area);
        (&bar).render(area, &mut buf);
        for x in 0..area.width {
            assert_eq!(buf[(x, 0)].symbol(), "█");
        }
    }

    #[test]
    fn full_single_segment_fills_entire_bar() {
        let segs = vec![StackedSegment::new("used", 1.0, rgb(255, 0, 0))];
        let bar = StackedBar::new(&segs);
        let area = Rect::new(0, 0, 10, 1);
        let mut buf = Buffer::empty(area);
        (&bar).render(area, &mut buf);
        for x in 0..area.width {
            assert_eq!(buf[(x, 0)].symbol(), "█");
        }
    }

    #[test]
    fn two_segments_split_bar_proportionally() {
        // 40% used + 30% cached, 30% free leftover (no segment).
        let segs = vec![
            StackedSegment::new("used", 0.4, rgb(255, 0, 0)),
            StackedSegment::new("cached", 0.3, rgb(0, 255, 0)),
        ];
        let bar = StackedBar::new(&segs);
        let area = Rect::new(0, 0, 10, 1);
        let mut buf = Buffer::empty(area);
        (&bar).render(area, &mut buf);
        // Cells 0..3 should have used color (red), 4..6 cached (green),
        // 7..9 are free space (empty_bg).
        // At least the deep interior cells should have unambiguous colors.
        let used = buf[(1, 0)].fg;
        let cached = buf[(5, 0)].fg;
        assert_eq!(used, rgb(255, 0, 0));
        assert_eq!(cached, rgb(0, 255, 0));
    }

    #[test]
    fn legend_renders_when_requested() {
        let segs = vec![StackedSegment::new("used", 0.5, rgb(255, 0, 0))];
        let bar = StackedBar::new(&segs).with_legend(true);
        let area = Rect::new(0, 0, 20, 2);
        let mut buf = Buffer::empty(area);
        (&bar).render(area, &mut buf);
        // Legend on row 1: ■ used   ...
        assert_eq!(buf[(0, 1)].symbol(), "■");
        assert_eq!(buf[(2, 1)].symbol(), "u");
    }
}
