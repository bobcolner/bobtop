//! BrailleGraph — area-fill graph that drives bobtop's visual identity.
//!
//! Algorithm:
//! 1. Each terminal cell maps to 2 horizontal × 4 vertical "dots" via the
//!    Unicode Braille block (U+2800 + 8-bit dot mask).
//! 2. Data points are right-aligned (newest on the right). For each dot
//!    column, the value `v ∈ [0,1]` lights every dot from the column's
//!    `top_dot_row` down to the bottom.
//! 3. Gradient is sampled by **vertical row position**, not by data value:
//!    top of the graph = `end` color, bottom = `start`. This is what gives
//!    btop its "lit from above" look — peaks read red, baselines green.
//! 4. Cells that contain the trace's leading edge use the full gradient
//!    color; cells entirely below the trace use the same color dimmed by
//!    `dim_fill_factor` (default 0.45). Cells entirely above the trace render
//!    nothing.
//!
//! Two render styles are supported: `Braille` (default — full 8-dot
//! resolution) and `Blocks` (TTY fallback using ▁▂▃▄▅▆▇█ when braille glyphs
//! aren't renderable, e.g. on a Linux VT).

use std::cell::RefCell;
use std::collections::VecDeque;

use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::style::{Color, Style};
use ratatui::widgets::Widget;

use crate::color::{dim, Gradient};

// Per-thread scratch buffers reused across frames so the hot render path
// allocates zero. The renderer is single-threaded today (one TUI thread)
// so a thread-local fits with no synchronization cost. `Widget::render`
// consumes `self` by value, which rules out an `&mut` field on the widget.
thread_local! {
    static SCRATCH_TOPS: RefCell<Vec<usize>> = const { RefCell::new(Vec::new()) };
    static SCRATCH_RANGES: RefCell<Vec<(i32, i32)>> = const { RefCell::new(Vec::new()) };
}

/// Number of dot columns per terminal cell (Unicode braille is 2 wide).
const DOTS_PER_CELL_X: usize = 2;
/// Number of dot rows per terminal cell.
const DOTS_PER_CELL_Y: usize = 4;
/// Default dimming for fill cells beneath the trace. Set high (close to 1.0)
/// so fill cells remain visible when the panel renders on top of the theme's
/// `main_bg` — the previous 0.45 value was tuned for the terminal default
/// (pure black) and washed out against typical theme backgrounds like
/// `#0F1419` (ayu) or `#1d2021` (gruvbox). At 0.85 the trace edge still
/// reads brighter than the fill below it, but the fill keeps enough
/// saturation to draw the gradient.
pub const DEFAULT_DIM_FILL: f32 = 0.85;

/// Block-density characters for the TTY fallback, indexed 0..=8.
const BLOCK_LEVELS: [char; 9] = [' ', '▁', '▂', '▃', '▄', '▅', '▆', '▇', '█'];

/// Bit offsets for the 8 dots inside a braille cell, indexed by
/// `(col_in_cell, row_in_cell)`. Matches the Unicode braille pattern bit
/// assignment in U+2800.
const BRAILLE_BIT: [[u8; DOTS_PER_CELL_Y]; DOTS_PER_CELL_X] = [
    // col 0 (left): rows 0..4
    [0x01, 0x02, 0x04, 0x40],
    // col 1 (right): rows 0..4
    [0x08, 0x10, 0x20, 0x80],
];

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GraphStyle {
    /// Standard fill-from-bottom braille trace.
    Braille,
    /// Block-density characters (▁▂▃▄▅▆▇█) — TTY fallback.
    Blocks,
    /// Trace blooms outward from a horizontal centerline. Value 0 = thin
    /// 1-dot band at center, value 0.5 = fills middle 50%, value 1 = fills
    /// the entire area. Color sampled by distance-from-center: gradient
    /// `start` at the centerline, `end` at the edges.
    CenteredBloom,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DualMode {
    /// Both traces share the area; later push wins on dot collisions, color
    /// goes to the trace with the higher value at that cell.
    Overlay,
    /// Primary fills the bottom half drawing upward; secondary fills the top
    /// half drawing downward (mirrored). This is btop's network panel look.
    MirroredSplit,
}

/// One trace = data buffer + gradient. The widget owns one "primary" and an
/// optional "secondary" used in dual-trace mode.
#[derive(Debug, Clone)]
pub struct Trace {
    pub data: VecDeque<f64>,
    pub max_points: usize,
    pub gradient: Gradient,
}

impl Trace {
    pub fn new(max_points: usize, gradient: Gradient) -> Self {
        Self {
            data: VecDeque::with_capacity(max_points),
            max_points,
            gradient,
        }
    }

    pub fn push(&mut self, value: f64) {
        if self.data.len() >= self.max_points {
            self.data.pop_front();
        }
        self.data.push_back(value);
    }
}

pub struct BrailleGraph {
    pub primary: Trace,
    pub secondary: Option<Trace>,
    pub dual_mode: DualMode,
    pub style: GraphStyle,
    /// When `false`, only the trace pixel is set (no fill below).
    pub show_fill: bool,
    pub dim_fill_factor: f32,
    /// Optional inline label rendered at the top-left of the graph area.
    pub label: Option<String>,
    /// Optional formatter that produces a string for the most recent value
    /// (rendered top-right, e.g. "97.8%" or "1.85 KiB/s").
    pub value_fn: Option<Box<dyn Fn(f64) -> String + Send + Sync>>,
    /// Optional Y-scale labels (top-left and bottom-left of graph area).
    /// Used by the network panel to show auto-scaled max ("2M" / "13K").
    pub y_scale_top: Option<String>,
    pub y_scale_bottom: Option<String>,
    /// Optional style for chrome overlays (label, value readout, y-scale).
    /// When `None`, falls back to `Color::Reset`. Set this from the caller to
    /// theme the chrome via `theme.graph_text`.
    pub text_style: Option<Style>,
}

impl std::fmt::Debug for BrailleGraph {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("BrailleGraph")
            .field("primary_len", &self.primary.data.len())
            .field("secondary_len", &self.secondary.as_ref().map(|s| s.data.len()))
            .field("dual_mode", &self.dual_mode)
            .field("style", &self.style)
            .field("show_fill", &self.show_fill)
            .field("label", &self.label)
            .finish()
    }
}

impl BrailleGraph {
    pub fn new(max_points: usize, gradient: Gradient) -> Self {
        Self {
            primary: Trace::new(max_points, gradient),
            secondary: None,
            dual_mode: DualMode::Overlay,
            style: GraphStyle::Braille,
            show_fill: true,
            dim_fill_factor: DEFAULT_DIM_FILL,
            label: None,
            value_fn: None,
            y_scale_top: None,
            y_scale_bottom: None,
            text_style: None,
        }
    }

    pub fn with_text_style(mut self, style: Style) -> Self {
        self.text_style = Some(style);
        self
    }

    /// Convert this into a dual-trace graph by attaching a secondary.
    pub fn with_secondary(mut self, secondary: Trace, mode: DualMode) -> Self {
        self.secondary = Some(secondary);
        self.dual_mode = mode;
        self
    }

    pub fn with_style(mut self, style: GraphStyle) -> Self {
        self.style = style;
        self
    }

    pub fn with_label(mut self, label: impl Into<String>) -> Self {
        self.label = Some(label.into());
        self
    }

    pub fn with_value_fn<F>(mut self, f: F) -> Self
    where
        F: Fn(f64) -> String + Send + Sync + 'static,
    {
        self.value_fn = Some(Box::new(f));
        self
    }

    pub fn with_y_scale(mut self, top: impl Into<String>, bottom: impl Into<String>) -> Self {
        self.y_scale_top = Some(top.into());
        self.y_scale_bottom = Some(bottom.into());
        self
    }

    pub fn push(&mut self, value: f64) {
        self.primary.push(value);
    }

    pub fn push_dual(&mut self, primary: f64, secondary: f64) {
        self.primary.push(primary);
        if let Some(s) = self.secondary.as_mut() {
            s.push(secondary);
        }
    }

    fn current_value(&self) -> Option<f64> {
        self.primary.data.back().copied()
    }
}

impl Widget for &BrailleGraph {
    fn render(self, area: Rect, buf: &mut Buffer) {
        if area.width == 0 || area.height == 0 {
            return;
        }
        match (self.style, &self.secondary, self.dual_mode) {
            (GraphStyle::Braille, Some(sec), DualMode::MirroredSplit) => {
                let (top, bot) = split_vertical(area);
                render_braille_mirrored_top(buf, top, sec);
                render_braille(buf, bot, &self.primary, self.show_fill, self.dim_fill_factor);
            }
            (GraphStyle::Braille, Some(sec), DualMode::Overlay) => {
                render_braille(buf, area, &self.primary, self.show_fill, self.dim_fill_factor);
                render_braille_overlay(buf, area, sec, self.show_fill, self.dim_fill_factor);
            }
            (GraphStyle::Braille, None, _) => {
                render_braille(buf, area, &self.primary, self.show_fill, self.dim_fill_factor);
            }
            (GraphStyle::Blocks, _, _) => {
                render_blocks(buf, area, &self.primary);
                if let Some(sec) = &self.secondary {
                    // Block mode just overlays the second trace; no mirror.
                    render_blocks(buf, area, sec);
                }
            }
            (GraphStyle::CenteredBloom, Some(sec), _) => {
                // Dual-trace: split area at center. Top half = secondary
                // (e.g. upload) blooming UP from center toward top edge;
                // bottom half = primary (download) blooming DOWN from
                // center toward bottom edge. Empty middle band when both
                // are quiet.
                let top_h = area.height / 2;
                let bot_h = area.height - top_h;
                let top = Rect { x: area.x, y: area.y, width: area.width, height: top_h };
                let bot = Rect { x: area.x, y: area.y + top_h, width: area.width, height: bot_h };
                render_braille_centered_half(buf, top, sec, /*grow_upward*/ true);
                render_braille_centered_half(buf, bot, &self.primary, false);
            }
            (GraphStyle::CenteredBloom, None, _) => {
                render_braille_centered(buf, area, &self.primary);
            }
        }
        render_overlays(self, area, buf);
    }
}

fn render_overlays(graph: &BrailleGraph, area: Rect, buf: &mut Buffer) {
    // Optional caller-provided text style for graph chrome (label, value
    // readout, y-scale ticks). When unset we fall back to plain reset to
    // match historical behavior; bobtop's UI now passes `theme.graph_text`.
    let style = graph
        .text_style
        .unwrap_or_else(|| Style::default().fg(Color::Reset));
    if let Some(label) = &graph.label {
        write_str(buf, area.left(), area.top(), label, area.width as usize, style);
    }
    if let (Some(f), Some(v)) = (&graph.value_fn, graph.current_value()) {
        let s = f(v);
        let len = s.chars().count() as u16;
        if len <= area.width {
            let x = area.right().saturating_sub(len);
            write_str(buf, x, area.top(), &s, area.width as usize, style);
        }
    }
    if let Some(top_label) = &graph.y_scale_top {
        write_str(buf, area.left(), area.top(), top_label, area.width as usize, style);
    }
    if let Some(bot_label) = &graph.y_scale_bottom {
        let y = area.bottom().saturating_sub(1);
        write_str(buf, area.left(), y, bot_label, area.width as usize, style);
    }
}

fn write_str(buf: &mut Buffer, x: u16, y: u16, s: &str, max_cols: usize, style: Style) {
    let mut col = x;
    let right = x.saturating_add(max_cols as u16).min(buf.area.right());
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

fn split_vertical(area: Rect) -> (Rect, Rect) {
    let top_h = area.height / 2;
    let bot_h = area.height - top_h;
    let top = Rect { x: area.x, y: area.y, width: area.width, height: top_h };
    let bot = Rect { x: area.x, y: area.y + top_h, width: area.width, height: bot_h };
    (top, bot)
}

// ---------------------------------------------------------------------------
// Braille rendering — primary and overlay
// ---------------------------------------------------------------------------

/// Render a single trace into `area` using braille dots.
fn render_braille(buf: &mut Buffer, area: Rect, trace: &Trace, show_fill: bool, dim_factor: f32) {
    let cells_w = area.width as usize;
    let cells_h = area.height as usize;
    let dot_cols = cells_w * DOTS_PER_CELL_X;
    let dot_rows = cells_h * DOTS_PER_CELL_Y;

    // Per-dot-column "topmost set dot row" — `dot_rows` means "no dots set".
    // Borrowed from a thread-local scratch so we don't alloc per frame.
    SCRATCH_TOPS.with(|c| {
        let mut tops = c.borrow_mut();
        trace_top_dots(trace, dot_cols, dot_rows, &mut tops);

        for cell_x in 0..cells_w {
            let dx_l = cell_x * DOTS_PER_CELL_X;
            let dx_r = dx_l + 1;
            let top_l = tops[dx_l];
            let top_r = tops[dx_r];
            let cell_top_row = |cell_y: usize| cell_y * DOTS_PER_CELL_Y;

            for cell_y in 0..cells_h {
                let row_top = cell_top_row(cell_y);
                let row_bot = row_top + DOTS_PER_CELL_Y - 1;

                let mut bits: u8 = 0;
                for ci in 0..DOTS_PER_CELL_X {
                    let top = if ci == 0 { top_l } else { top_r };
                    for ri in 0..DOTS_PER_CELL_Y {
                        let dot_row = row_top + ri;
                        let dot_set = if show_fill {
                            dot_row >= top
                        } else {
                            dot_row == top
                        };
                        if dot_set {
                            bits |= BRAILLE_BIT[ci][ri];
                        }
                    }
                }
                if bits == 0 {
                    continue;
                }

                // Determine if this cell contains the trace's leading edge in
                // either column. If so it's "trace"; if entirely below it's "fill".
                let trace_in_cell = (top_l >= row_top && top_l <= row_bot)
                    || (top_r >= row_top && top_r <= row_bot);

                // Color is sampled at the topmost set dot in the cell. row 0 = top
                // → t=1.0; row dot_rows-1 = bottom → t=0.0.
                let topmost = top_l.min(top_r).max(row_top);
                let t = if dot_rows > 1 {
                    1.0 - (topmost as f32) / ((dot_rows - 1) as f32)
                } else {
                    1.0
                };
                let mut color = trace.gradient.sample(t);
                if !trace_in_cell {
                    color = dim(color, dim_factor);
                }

                let glyph = char::from_u32(0x2800 + bits as u32).unwrap_or('⠀');
                let cell = &mut buf[(area.x + cell_x as u16, area.y + cell_y as u16)];
                cell.set_char(glyph);
                cell.set_style(Style::default().fg(color));
            }
        }
    });
}

/// Overlay a trace on top of an already-rendered area: OR in the dot bits and
/// recolor the cell when the secondary contributes the higher trace.
fn render_braille_overlay(
    buf: &mut Buffer,
    area: Rect,
    trace: &Trace,
    show_fill: bool,
    dim_factor: f32,
) {
    let cells_w = area.width as usize;
    let cells_h = area.height as usize;
    let dot_cols = cells_w * DOTS_PER_CELL_X;
    let dot_rows = cells_h * DOTS_PER_CELL_Y;
    SCRATCH_TOPS.with(|c| {
        let mut tops = c.borrow_mut();
        trace_top_dots(trace, dot_cols, dot_rows, &mut tops);

        for cell_x in 0..cells_w {
            let dx_l = cell_x * DOTS_PER_CELL_X;
            let dx_r = dx_l + 1;
            let top_l = tops[dx_l];
            let top_r = tops[dx_r];

            for cell_y in 0..cells_h {
                let row_top = cell_y * DOTS_PER_CELL_Y;
                let row_bot = row_top + DOTS_PER_CELL_Y - 1;

                let mut new_bits: u8 = 0;
                for ci in 0..DOTS_PER_CELL_X {
                    let top = if ci == 0 { top_l } else { top_r };
                    for ri in 0..DOTS_PER_CELL_Y {
                        let dot_row = row_top + ri;
                        let set = if show_fill {
                            dot_row >= top
                        } else {
                            dot_row == top
                        };
                        if set {
                            new_bits |= BRAILLE_BIT[ci][ri];
                        }
                    }
                }
                if new_bits == 0 {
                    continue;
                }

                let cell = &mut buf[(area.x + cell_x as u16, area.y + cell_y as u16)];
                let existing_glyph = cell.symbol().chars().next().unwrap_or(' ');
                let existing_bits = if (0x2800..=0x28FF).contains(&(existing_glyph as u32)) {
                    (existing_glyph as u32 - 0x2800) as u8
                } else {
                    0
                };
                let merged = existing_bits | new_bits;
                let glyph = char::from_u32(0x2800 + merged as u32).unwrap_or('⠀');
                cell.set_char(glyph);

                // Color this cell with the secondary if it has a "higher"
                // trace (smaller top dot row) in this cell, else keep prior.
                let trace_in_cell = (top_l >= row_top && top_l <= row_bot)
                    || (top_r >= row_top && top_r <= row_bot);
                if trace_in_cell {
                    let topmost = top_l.min(top_r).max(row_top);
                    let t = if dot_rows > 1 {
                        1.0 - (topmost as f32) / ((dot_rows - 1) as f32)
                    } else {
                        1.0
                    };
                    cell.set_style(Style::default().fg(trace.gradient.sample(t)));
                } else if existing_bits == 0 {
                    // Pure fill cell: dim secondary at this row.
                    let topmost = row_top;
                    let t = if dot_rows > 1 {
                        1.0 - (topmost as f32) / ((dot_rows - 1) as f32)
                    } else {
                        1.0
                    };
                    cell.set_style(
                        Style::default().fg(dim(trace.gradient.sample(t), dim_factor)),
                    );
                }
            }
        }
    });
}

/// Mirrored render: draw the trace from the TOP downward instead of from the
/// bottom up. Used for the secondary trace in [`DualMode::MirroredSplit`].
fn render_braille_mirrored_top(buf: &mut Buffer, area: Rect, trace: &Trace) {
    let cells_w = area.width as usize;
    let cells_h = area.height as usize;
    let dot_cols = cells_w * DOTS_PER_CELL_X;
    let dot_rows = cells_h * DOTS_PER_CELL_Y;

    // Bottoms (mirror of tops): for each dot column, value v fills rows
    // 0..floor(v * dot_rows). row 0 = top. Reuses SCRATCH_TOPS — same
    // Vec<usize> shape (length == dot_cols), and the four braille renderers
    // are called sequentially within a single draw, never nested.
    SCRATCH_TOPS.with(|c| {
        let mut bottoms = c.borrow_mut();
        bottoms.clear();
        bottoms.resize(dot_cols, 0);
        let n = trace.data.len();
        let start_offset = if n >= dot_cols { n - dot_cols } else { 0 };
        let pad = dot_cols.saturating_sub(n);
        for c in 0..dot_cols {
            let v = if c < pad {
                0.0
            } else {
                trace.data.get(start_offset + (c - pad)).copied().unwrap_or(0.0)
            };
            let v = v.clamp(0.0, 1.0);
            bottoms[c] = ((v * dot_rows as f64).round() as usize).min(dot_rows);
        }

        for cell_x in 0..cells_w {
            let dx_l = cell_x * DOTS_PER_CELL_X;
            let dx_r = dx_l + 1;
            let bot_l = bottoms[dx_l];
            let bot_r = bottoms[dx_r];

            for cell_y in 0..cells_h {
                let row_top = cell_y * DOTS_PER_CELL_Y;
                let row_bot = row_top + DOTS_PER_CELL_Y - 1;

                let mut bits: u8 = 0;
                for ci in 0..DOTS_PER_CELL_X {
                    let bottom = if ci == 0 { bot_l } else { bot_r };
                    for ri in 0..DOTS_PER_CELL_Y {
                        let dot_row = row_top + ri;
                        if dot_row < bottom {
                            bits |= BRAILLE_BIT[ci][ri];
                        }
                    }
                }
                if bits == 0 {
                    continue;
                }

                let trace_in_cell = (bot_l > row_top && bot_l <= row_bot + 1)
                    || (bot_r > row_top && bot_r <= row_bot + 1);
                let bottommost = bot_l.max(bot_r).min(row_bot + 1).saturating_sub(1);
                let t = if dot_rows > 1 {
                    1.0 - (bottommost as f32) / ((dot_rows - 1) as f32)
                } else {
                    1.0
                };
                let mut color = trace.gradient.sample(t);
                if !trace_in_cell {
                    color = dim(color, DEFAULT_DIM_FILL);
                }
                let glyph = char::from_u32(0x2800 + bits as u32).unwrap_or('⠀');
                let cell = &mut buf[(area.x + cell_x as u16, area.y + cell_y as u16)];
                cell.set_char(glyph);
                cell.set_style(Style::default().fg(color));
            }
        }
    });
}

/// Fill `out` with the index of the topmost set dot row for each dot column.
/// A value equal to `dot_rows` means "no dots in this column." Reuses the
/// caller-provided buffer (length is reset, capacity retained) so the hot
/// render path doesn't allocate per frame.
fn trace_top_dots(trace: &Trace, dot_cols: usize, dot_rows: usize, out: &mut Vec<usize>) {
    out.clear();
    out.resize(dot_cols, dot_rows);
    let n = trace.data.len();
    let start_offset = if n >= dot_cols { n - dot_cols } else { 0 };
    let pad = dot_cols.saturating_sub(n);
    for c in 0..dot_cols {
        let v = if c < pad {
            0.0
        } else {
            trace.data.get(start_offset + (c - pad)).copied().unwrap_or(0.0)
        };
        let v = v.clamp(0.0, 1.0);
        if v <= 0.0 {
            continue;
        }
        let height = (v * dot_rows as f64).round() as usize;
        if height == 0 {
            continue;
        }
        let top = dot_rows - height;
        out[c] = top;
    }
}

// ---------------------------------------------------------------------------
// Centered-bloom braille
// ---------------------------------------------------------------------------

/// Render a trace as a "centered bloom" — for each dot column, the value
/// determines how many dot rows are filled, anchored at the *center*
/// horizontal line and growing symmetrically outward. Color is sampled by
/// distance from center: `start` at the centerline, `end` at the edges.
fn render_braille_centered(buf: &mut Buffer, area: Rect, trace: &Trace) {
    let cells_w = area.width as usize;
    let cells_h = area.height as usize;
    let dot_cols = cells_w * DOTS_PER_CELL_X;
    let dot_rows = cells_h * DOTS_PER_CELL_Y;
    if dot_rows == 0 {
        return;
    }
    let center = dot_rows as i32 / 2;

    // Right-align data: newest sample on the right.
    let n = trace.data.len();
    let start_offset = if n >= dot_cols { n - dot_cols } else { 0 };
    let pad = dot_cols.saturating_sub(n);

    // Per-column fill range (lo..hi over dot rows). Reuses thread-local
    // scratch — capacity grows monotonically so steady-state allocs == 0.
    SCRATCH_RANGES.with(|c| {
        let mut ranges = c.borrow_mut();
        ranges.clear();
        ranges.reserve(dot_cols);
        for c in 0..dot_cols {
            let v = if c < pad {
                0.0
            } else {
                trace.data.get(start_offset + (c - pad)).copied().unwrap_or(0.0)
            };
            let v = v.clamp(0.0, 1.0);
            // Total filled dots = round(v * dot_rows). Always at least 1 so a
            // value of 0 still draws a thin centerline reference.
            let total = ((v * dot_rows as f64).round() as i32).max(1);
            let half = total / 2;
            let extra = total - 2 * half; // 1 when total is odd, 0 when even
            let lo = (center - half).max(0);
            let hi = (center + half + extra).min(dot_rows as i32);
            ranges.push((lo, hi));
        }

        for cell_x in 0..cells_w {
            for cell_y in 0..cells_h {
                let row_top = (cell_y * DOTS_PER_CELL_Y) as i32;

                let mut bits: u8 = 0;
                for ci in 0..DOTS_PER_CELL_X {
                    let dx = cell_x * DOTS_PER_CELL_X + ci;
                    let (lo, hi) = ranges[dx];
                    for ri in 0..DOTS_PER_CELL_Y {
                        let dot_row = row_top + ri as i32;
                        if dot_row >= lo && dot_row < hi {
                            bits |= BRAILLE_BIT[ci][ri];
                        }
                    }
                }
                if bits == 0 {
                    continue;
                }
                // Color by cell's vertical distance from the centerline.
                let cell_center_dot = row_top + (DOTS_PER_CELL_Y as i32) / 2;
                let dist = (cell_center_dot - center).abs() as f32;
                let max_dist = ((dot_rows as i32) / 2).max(1) as f32;
                let t = (dist / max_dist).clamp(0.0, 1.0);
                let color = trace.gradient.sample(t);

                let glyph = char::from_u32(0x2800 + bits as u32).unwrap_or('⠀');
                let cell = &mut buf[(area.x + cell_x as u16, area.y + cell_y as u16)];
                cell.set_char(glyph);
                cell.set_style(Style::default().fg(color));
            }
        }
    });
}

/// Render a single trace into one half of the centered-split layout.
/// `grow_upward = true`: the trace anchors at the BOTTOM edge of `area` and
/// fills upward as value increases (used for the TOP half of a centered
/// split — visually, the trace blooms upward away from the centerline).
/// `grow_upward = false`: anchors at the TOP edge, fills downward.
fn render_braille_centered_half(buf: &mut Buffer, area: Rect, trace: &Trace, grow_upward: bool) {
    let cells_w = area.width as usize;
    let cells_h = area.height as usize;
    let dot_cols = cells_w * DOTS_PER_CELL_X;
    let dot_rows = cells_h * DOTS_PER_CELL_Y;
    if dot_rows == 0 {
        return;
    }
    let n = trace.data.len();
    let start_offset = if n >= dot_cols { n - dot_cols } else { 0 };
    let pad = dot_cols.saturating_sub(n);

    let mut spans: Vec<(i32, i32)> = Vec::with_capacity(dot_cols);
    for c in 0..dot_cols {
        let v = if c < pad {
            0.0
        } else {
            trace.data.get(start_offset + (c - pad)).copied().unwrap_or(0.0)
        };
        let v = v.clamp(0.0, 1.0);
        let filled = ((v * dot_rows as f64).round() as i32).min(dot_rows as i32);
        if grow_upward {
            // Anchor at bottom of this half (= centerline of the whole panel),
            // grow upward.
            let lo = (dot_rows as i32 - filled).max(0);
            let hi = dot_rows as i32;
            spans.push((lo, hi));
        } else {
            // Anchor at top of this half (= centerline), grow downward.
            spans.push((0, filled));
        }
    }

    for cell_x in 0..cells_w {
        for cell_y in 0..cells_h {
            let row_top = (cell_y * DOTS_PER_CELL_Y) as i32;
            let mut bits: u8 = 0;
            for ci in 0..DOTS_PER_CELL_X {
                let dx = cell_x * DOTS_PER_CELL_X + ci;
                let (lo, hi) = spans[dx];
                for ri in 0..DOTS_PER_CELL_Y {
                    let dot_row = row_top + ri as i32;
                    if dot_row >= lo && dot_row < hi {
                        bits |= BRAILLE_BIT[ci][ri];
                    }
                }
            }
            if bits == 0 {
                continue;
            }
            // Color sampled by row's distance from the centerline (i.e. the
            // edge of the half that's adjacent to the other half).
            let cell_center_dot = row_top + (DOTS_PER_CELL_Y as i32) / 2;
            let centerline_dot = if grow_upward { dot_rows as i32 } else { 0 };
            let dist = (cell_center_dot - centerline_dot).abs() as f32;
            let max_dist = (dot_rows as i32).max(1) as f32;
            let t = (dist / max_dist).clamp(0.0, 1.0);
            let color = trace.gradient.sample(t);
            let glyph = char::from_u32(0x2800 + bits as u32).unwrap_or('⠀');
            let cell = &mut buf[(area.x + cell_x as u16, area.y + cell_y as u16)];
            cell.set_char(glyph);
            cell.set_style(Style::default().fg(color));
        }
    }
}

// ---------------------------------------------------------------------------
// Block-density TTY fallback
// ---------------------------------------------------------------------------

fn render_blocks(buf: &mut Buffer, area: Rect, trace: &Trace) {
    let cells_w = area.width as usize;
    let cells_h = area.height as usize;
    let n = trace.data.len();
    let start_offset = if n >= cells_w { n - cells_w } else { 0 };
    let pad = cells_w.saturating_sub(n);

    for cell_x in 0..cells_w {
        let v = if cell_x < pad {
            0.0
        } else {
            trace.data.get(start_offset + (cell_x - pad)).copied().unwrap_or(0.0)
        };
        let v = v.clamp(0.0, 1.0);

        // value v occupies the bottom v * cells_h rows. Within each cell,
        // we may render a fractional block at the boundary.
        let total_eighths = (v * (cells_h as f64) * 8.0).round() as usize;

        for cell_y in (0..cells_h).rev() {
            let row_eighths_from_bottom = (cells_h - 1 - cell_y) * 8;
            let cell_fill = total_eighths.saturating_sub(row_eighths_from_bottom).min(8);
            let glyph = BLOCK_LEVELS[cell_fill];
            if glyph == ' ' {
                break;
            }

            let t = if cells_h > 1 {
                1.0 - (cell_y as f32) / ((cells_h - 1) as f32)
            } else {
                1.0
            };
            let color = trace.gradient.sample(t);
            let cell = &mut buf[(area.x + cell_x as u16, area.y + cell_y as u16)];
            cell.set_char(glyph);
            cell.set_style(Style::default().fg(color));
        }
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use ratatui::style::Color;

    fn test_gradient() -> Gradient {
        Gradient::new(Color::Rgb(0, 100, 0), Color::Rgb(180, 180, 0), Color::Rgb(255, 0, 0))
    }

    #[test]
    fn empty_data_renders_nothing() {
        let g = BrailleGraph::new(64, test_gradient());
        let area = Rect::new(0, 0, 10, 4);
        let mut buf = Buffer::empty(area);
        (&g).render(area, &mut buf);
        for x in 0..area.width {
            for y in 0..area.height {
                assert_eq!(buf[(x, y)].symbol(), " ");
            }
        }
    }

    #[test]
    fn full_data_fills_every_cell() {
        let mut g = BrailleGraph::new(64, test_gradient());
        for _ in 0..64 {
            g.push(1.0);
        }
        let area = Rect::new(0, 0, 10, 4);
        let mut buf = Buffer::empty(area);
        (&g).render(area, &mut buf);
        for x in 0..area.width {
            for y in 0..area.height {
                let s = buf[(x, y)].symbol();
                assert_eq!(s, "⣿", "cell ({x},{y}) should be fully set, got {s:?}");
            }
        }
    }

    #[test]
    fn trace_top_dots_right_aligns_data() {
        let mut t = Trace::new(8, test_gradient());
        // With 4 points (data: 1,1,1,1), dot_cols=8 → first 4 cols empty,
        // last 4 cols filled.
        for _ in 0..4 {
            t.push(1.0);
        }
        let mut tops = Vec::new();
        trace_top_dots(&t, 8, 4, &mut tops);
        assert_eq!(tops, vec![4, 4, 4, 4, 0, 0, 0, 0]);
    }

    #[test]
    fn half_value_fills_bottom_half() {
        let mut g = BrailleGraph::new(64, test_gradient());
        for _ in 0..64 {
            g.push(0.5);
        }
        let area = Rect::new(0, 0, 8, 4);
        let mut buf = Buffer::empty(area);
        (&g).render(area, &mut buf);
        // Top two rows (cells y=0,1) entirely above trace → empty.
        for x in 0..area.width {
            for y in 0..2 {
                assert_eq!(buf[(x, y)].symbol(), " ");
            }
        }
        // Bottom two rows (cells y=2,3) below trace → fully set.
        for x in 0..area.width {
            for y in 2..4 {
                assert_eq!(buf[(x, y)].symbol(), "⣿");
            }
        }
    }

    #[test]
    fn dim_disabled_when_show_fill_is_false() {
        let mut g = BrailleGraph::new(64, test_gradient());
        g.show_fill = false;
        for _ in 0..64 {
            g.push(0.5);
        }
        let area = Rect::new(0, 0, 8, 4);
        let mut buf = Buffer::empty(area);
        (&g).render(area, &mut buf);
        // No cells in the bottom half should be the "full" glyph; only the
        // trace line itself is set.
        for x in 0..area.width {
            for y in 3..4 {
                assert_ne!(buf[(x, y)].symbol(), "⣿");
            }
        }
    }

    #[test]
    fn block_style_renders_full_block_at_value_one() {
        let mut g = BrailleGraph::new(64, test_gradient()).with_style(GraphStyle::Blocks);
        for _ in 0..16 {
            g.push(1.0);
        }
        let area = Rect::new(0, 0, 16, 4);
        let mut buf = Buffer::empty(area);
        (&g).render(area, &mut buf);
        for x in 0..area.width {
            for y in 0..area.height {
                assert_eq!(buf[(x, y)].symbol(), "█", "cell ({x},{y})");
            }
        }
    }

    #[test]
    fn label_appears_top_left() {
        let g = BrailleGraph::new(64, test_gradient()).with_label("cpu");
        let area = Rect::new(0, 0, 20, 4);
        let mut buf = Buffer::empty(area);
        (&g).render(area, &mut buf);
        assert_eq!(buf[(0, 0)].symbol(), "c");
        assert_eq!(buf[(1, 0)].symbol(), "p");
        assert_eq!(buf[(2, 0)].symbol(), "u");
    }

    #[test]
    fn value_fn_appears_top_right() {
        let mut g = BrailleGraph::new(64, test_gradient())
            .with_value_fn(|v| format!("{:.0}%", v * 100.0));
        g.push(0.97);
        let area = Rect::new(0, 0, 20, 4);
        let mut buf = Buffer::empty(area);
        (&g).render(area, &mut buf);
        // "97%" should be at the right edge of row 0.
        assert_eq!(buf[(17, 0)].symbol(), "9");
        assert_eq!(buf[(18, 0)].symbol(), "7");
        assert_eq!(buf[(19, 0)].symbol(), "%");
    }

    #[test]
    fn dual_mirrored_split_doesnt_panic_and_renders() {
        let mut g = BrailleGraph::new(64, test_gradient()).with_secondary(
            Trace::new(64, test_gradient()),
            DualMode::MirroredSplit,
        );
        for _ in 0..64 {
            g.push_dual(0.6, 0.4);
        }
        let area = Rect::new(0, 0, 16, 6);
        let mut buf = Buffer::empty(area);
        (&g).render(area, &mut buf);
        // At minimum the area shouldn't be all blank.
        let mut any_nonblank = false;
        for x in 0..area.width {
            for y in 0..area.height {
                if buf[(x, y)].symbol() != " " {
                    any_nonblank = true;
                }
            }
        }
        assert!(any_nonblank, "mirrored split rendered nothing");
    }
}
