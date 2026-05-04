use std::collections::VecDeque;

use bobtop_tui::{Gradient, GraphStyle, Sparkline, write_str_at};
use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::style::Style;
use ratatui::widgets::Widget;

/// Render one inline-chart row: `[label] [sparkline] [value]`.
///
/// Shared by CPU core rows and disk I/O rows so the inline chart chrome stays
/// consistent across panels.
pub(super) fn draw_track_sparkline_row(
    buf: &mut Buffer,
    area: Rect,
    label: &str,
    value: &str,
    history: Option<&VecDeque<f64>>,
    gradient: Gradient,
    text_color: ratatui::style::Color,
    tty: bool,
) {
    if area.width == 0 || area.height == 0 {
        return;
    }
    let label_w = (label.chars().count() as u16).min(area.width);
    let value_w = (value.chars().count() as u16).min(area.width.saturating_sub(label_w));
    let gutter = if area.width > label_w + value_w + 4 { 1 } else { 0 };
    let spark_x = area.x + label_w + gutter;
    let value_x = area.x + area.width.saturating_sub(value_w);
    let spark_end = value_x.saturating_sub(gutter);
    let spark_w = spark_end.saturating_sub(spark_x);
    let y = area.y;

    write_str_at(buf, area.x, y, label, Style::default().fg(text_color));
    write_str_at(buf, value_x, y, value, Style::default().fg(text_color));

    if spark_w == 0 {
        return;
    }
    let raw: Vec<f64> = history.map(|h| h.iter().copied().collect()).unwrap_or_default();
    if raw.is_empty() {
        return;
    }
    let peak = raw.iter().copied().fold(0.05_f64, f64::max);
    let values: Vec<f64> = raw.iter().map(|v| (v / peak).clamp(0.0, 1.0)).collect();
    let spark = Sparkline::new(&values, gradient).with_style(if tty {
        GraphStyle::Blocks
    } else {
        GraphStyle::Braille
    });
    (&spark).render(Rect::new(spark_x, y, spark_w, 1), buf);
}
