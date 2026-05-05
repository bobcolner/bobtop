//! 1-row braille sparkline — reusable mini chart for inline use inside
//! tabular rows (per-core CPU meters, per-mount disk IO, etc.).
//!
//! Design contract for the chart library:
//! - Pure render, no state ownership: `Sparkline` borrows a values slice
//!   and a [`Gradient`]; the caller owns the data history.
//! - Single Rect input; renders inside whatever row height the caller
//!   gives it (1 row optimal — the braille glyph gives 4 vertical levels;
//!   2+ rows also render correctly, scaling proportionally).
//! - Composable: pairs naturally with [`crate::widgets::MiniMeter`] and
//!   [`crate::widgets::Meter`] when the caller wants a "bar + spark" row.
//!
//! Implementation reuses [`crate::widgets::BrailleGraph`] under the hood
//! so we get the same dot-bit math, gradient sampling, and TTY-fallback
//! plumbing for free. The wrapper exists for ergonomics — building a
//! sparkline shouldn't require knowing about `Trace`, `DualMode`, etc.

use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::style::Style;
use ratatui::widgets::Widget;

use crate::color::Gradient;
use crate::widgets::braille_graph::{BrailleGraph, GraphStyle, DEFAULT_DIM_FILL};

/// Inline sparkline chart. Renders as filled-area braille by default;
/// switch to `GraphStyle::Blocks` for TTY/serial-console hosts.
///
/// Values are interpreted as 0.0..=1.0 (caller normalizes — for raw rates
/// like bytes/sec, divide by your peak; for percentages, divide by 100).
/// Out-of-range values clamp at render time.
pub struct Sparkline<'a> {
    values: &'a [f64],
    gradient: Gradient,
    style: GraphStyle,
    show_fill: bool,
    dim_fill_factor: f32,
}

impl<'a> Sparkline<'a> {
    pub fn new(values: &'a [f64], gradient: Gradient) -> Self {
        Self {
            values,
            gradient,
            style: GraphStyle::Braille,
            show_fill: true,
            dim_fill_factor: DEFAULT_DIM_FILL,
        }
    }

    /// TTY fallback (`▁▂▃▄▅▆▇█`). Use when braille glyphs aren't reliably
    /// renderable (linux VT, some serial consoles).
    pub fn with_style(mut self, style: GraphStyle) -> Self {
        self.style = style;
        self
    }

    /// Outline-only mode: no fill below the trace. Default is filled.
    pub fn outline_only(mut self) -> Self {
        self.show_fill = false;
        self
    }

    /// Dim multiplier for fill cells beneath the trace. 1.0 = no dim
    /// (matches btop). Default is the workspace constant.
    pub fn with_dim_fill(mut self, factor: f32) -> Self {
        self.dim_fill_factor = factor;
        self
    }
}

impl<'a> Widget for &Sparkline<'a> {
    fn render(self, area: Rect, buf: &mut Buffer) {
        if area.width == 0 || area.height == 0 {
            return;
        }
        // Reuse BrailleGraph as the underlying primitive. Fill from the
        // values slice — capacity equals the slice length so no eviction
        // happens during this render. The right-aligned "newest on the
        // right" placement is BrailleGraph's default behavior.
        let cap = self.values.len().max(1);
        let mut g = BrailleGraph::new(cap, self.gradient).with_style(self.style);
        g.show_fill = self.show_fill;
        g.dim_fill_factor = self.dim_fill_factor;
        // Suppress overlay chrome — sparklines are inline, no labels or
        // value text. (BrailleGraph's text_style stays None.)
        g.text_style = Some(Style::reset());
        for &v in self.values {
            g.push(v);
        }
        (&g).render(area, buf);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ratatui::style::Color;

    fn grad() -> Gradient {
        Gradient::new(
            Color::Rgb(0, 100, 0),
            Color::Rgb(180, 180, 0),
            Color::Rgb(255, 0, 0),
        )
    }

    #[test]
    fn empty_values_renders_nothing_visible() {
        let s = Sparkline::new(&[], grad());
        let area = Rect::new(0, 0, 8, 1);
        let mut buf = Buffer::empty(area);
        (&s).render(area, &mut buf);
        for x in 0..area.width {
            assert_eq!(buf[(x, 0)].symbol(), " ");
        }
    }

    #[test]
    fn full_values_fill_one_row_braille() {
        let v = vec![1.0; 16];
        let s = Sparkline::new(&v, grad());
        let area = Rect::new(0, 0, 8, 1);
        let mut buf = Buffer::empty(area);
        (&s).render(area, &mut buf);
        // Every cell at value 1.0 in a 1-row braille graph is the full block ⣿.
        for x in 0..area.width {
            assert_eq!(buf[(x, 0)].symbol(), "⣿", "col {x}");
        }
    }

    #[test]
    fn block_style_renders_full_block_at_value_one() {
        let v = vec![1.0; 8];
        let s = Sparkline::new(&v, grad()).with_style(GraphStyle::Blocks);
        let area = Rect::new(0, 0, 8, 1);
        let mut buf = Buffer::empty(area);
        (&s).render(area, &mut buf);
        for x in 0..area.width {
            assert_eq!(buf[(x, 0)].symbol(), "█", "col {x}");
        }
    }
}
