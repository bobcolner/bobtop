//! Process table — sortable row list matching btop's proc panel.
//!
//! Columns: PID, Program, User, Threads, MEM (RSS), CPU%. Optional NET RX /
//! NET TX columns appear when the active [`bobtop_net::AttributorTier`]
//! provides per-process bandwidth (Tiers 2 and 3).
//!
//! Selection state is owned by the caller — pass `selected: Some(idx)` and
//! `scroll_offset` so this widget stays render-only.

use bobtop_core::sample::ProcessInfo;
use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::style::{Color, Modifier, Style};
use ratatui::widgets::Widget;

use crate::Theme;

/// One row the renderer paints. Headers carry aggregates (group total
/// CPU/MEM/etc); processes carry their own info plus depth/branch info
/// for tree-mode indentation. Built by `bobtop_daemon::group::build_display`.
#[derive(Debug, Clone)]
pub enum DisplayRow {
    Header(GroupHeader),
    Process(ProcessRowMeta),
}

#[derive(Debug, Clone)]
pub struct GroupHeader {
    pub key: String,
    pub label: String,
    pub proc_count: usize,
    /// Sum of `ProcessInfo.threads` across the group. Surfaced in the
    /// "Th" column of the header row so users can see "firefox.service:
    /// 47 procs / 312 threads" at a glance.
    pub threads_total: u32,
    pub cpu_fraction_total: f32,
    pub mem_rss_total: u64,
    pub net_rx_total: Option<f64>,
    pub net_tx_total: Option<f64>,
    pub disk_read_total: Option<f64>,
    pub disk_write_total: Option<f64>,
    pub expanded: bool,
}

#[derive(Debug, Clone)]
pub struct ProcessRowMeta {
    pub info: ProcessInfo,
    /// 0 = top-level. 1+ = nested under a group header or a tree parent.
    pub depth: u8,
    /// True when this row is the last sibling at its depth (drives └ vs ├).
    pub is_last_sibling: bool,
    /// Per-ancestor-depth, true if that ancestor has a continuing sibling
    /// (drives │ vs space at column d).
    pub ancestor_continues: Vec<bool>,
}

/// How aggressively rows fade toward `inactive_fg` as their visible index
/// grows. `0.0` = no fade (matches old behavior); `1.0` = bottom row fully
/// at `inactive_fg`. btop uses a similar darkening pass for visual depth —
/// keeping it modest so contrast on long lists doesn't get unreadable.
const FADE_END: f32 = 0.55;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProcessSort {
    Pid,
    Name,
    User,
    Threads,
    Mem,
    Cpu,
    NetRx,
    NetTx,
    DiskRead,
    DiskWrite,
}

impl ProcessSort {
    /// Cycle order for the `←` / `→` keybinds — left-to-right display order
    /// so the selection visually tracks the arrow direction. Always includes
    /// every sortable column so users can reach net/disk sort even at Tier 1
    /// (rows just compare on `0.0` then).
    pub fn cycle() -> &'static [ProcessSort] {
        &[
            ProcessSort::Pid,
            ProcessSort::Name,
            ProcessSort::User,
            ProcessSort::Threads,
            ProcessSort::Mem,
            ProcessSort::Cpu,
            ProcessSort::NetRx,
            ProcessSort::NetTx,
            ProcessSort::DiskRead,
            ProcessSort::DiskWrite,
        ]
    }

    /// Stable label for the panel-title indicator (e.g. `[cpu↓]`).
    pub fn label(self) -> &'static str {
        match self {
            ProcessSort::Pid => "pid",
            ProcessSort::Name => "name",
            ProcessSort::User => "user",
            ProcessSort::Threads => "threads",
            ProcessSort::Mem => "mem",
            ProcessSort::Cpu => "cpu",
            ProcessSort::NetRx => "rx",
            ProcessSort::NetTx => "tx",
            ProcessSort::DiskRead => "dr",
            ProcessSort::DiskWrite => "dw",
        }
    }
}

pub struct ProcessTable<'a> {
    pub rows: &'a [DisplayRow],
    pub selected: Option<usize>,
    pub scroll_offset: usize,
    pub theme: &'a Theme,
    pub show_net_columns: bool,
    pub sort: ProcessSort,
    pub sort_descending: bool,
    /// True when the active group mode is `ByParent` (tree). The widget
    /// uses this to decide whether to draw branch glyphs (├ │ └) in the
    /// Program column. In flat / executable / cgroup modes children are
    /// indented with a simple two-space prefix instead.
    pub tree_mode: bool,
    /// Hide the User and Command columns entirely (set in ByExecutable
    /// and ByCgroup modes where headers don't have a meaningful single
    /// user / single command and leaving them blank looked broken).
    /// Reclaims width for the useful aggregate columns.
    pub compact_columns: bool,
}

impl<'a> ProcessTable<'a> {
    pub fn new(rows: &'a [DisplayRow], theme: &'a Theme) -> Self {
        Self {
            rows,
            selected: None,
            scroll_offset: 0,
            theme,
            show_net_columns: false,
            sort: ProcessSort::Cpu,
            sort_descending: true,
            tree_mode: false,
            compact_columns: false,
        }
    }

    pub fn with_tree_mode(mut self, on: bool) -> Self {
        self.tree_mode = on;
        self
    }

    pub fn with_compact_columns(mut self, on: bool) -> Self {
        self.compact_columns = on;
        self
    }

    pub fn with_direction(mut self, descending: bool) -> Self {
        self.sort_descending = descending;
        self
    }

    pub fn with_selection(mut self, selected: Option<usize>, scroll_offset: usize) -> Self {
        self.selected = selected;
        self.scroll_offset = scroll_offset;
        self
    }

    pub fn with_net_columns(mut self, show: bool) -> Self {
        self.show_net_columns = show;
        self
    }

    pub fn with_sort(mut self, sort: ProcessSort) -> Self {
        self.sort = sort;
        self
    }
}

#[derive(Debug, Clone, Copy)]
struct ColSpec {
    title: &'static str,
    sort: Option<ProcessSort>,
    width: u16,
    right_align: bool,
}

impl<'a> Widget for &ProcessTable<'a> {
    fn render(self, area: Rect, buf: &mut Buffer) {
        if area.width == 0 || area.height == 0 {
            return;
        }

        // 11-column layout (or 9 when compact, dropping User + Command).
        // Cells render `-` when data is unavailable so columns are
        // discoverable even at Tier 1. Render-row truncates from the
        // right when terminal is narrow, so on tight panels Command/Disk
        // drop first, Net second. Whichever cell builder we use below
        // MUST emit values in the same order as `cols`.
        let mut cols: Vec<ColSpec> = Vec::with_capacity(11);
        cols.push(ColSpec { title: "Pid",     sort: Some(ProcessSort::Pid),       width: 6,  right_align: true });
        cols.push(ColSpec { title: "Program", sort: Some(ProcessSort::Name),      width: 12, right_align: false });
        if !self.compact_columns {
            cols.push(ColSpec { title: "User", sort: Some(ProcessSort::User),     width: 6,  right_align: false });
        }
        cols.push(ColSpec { title: "Th",      sort: Some(ProcessSort::Threads),   width: 3,  right_align: true });
        cols.push(ColSpec { title: "MEM",     sort: Some(ProcessSort::Mem),       width: 6,  right_align: true });
        cols.push(ColSpec { title: "CPU%",    sort: Some(ProcessSort::Cpu),       width: 5,  right_align: true });
        cols.push(ColSpec { title: "RX/s",    sort: Some(ProcessSort::NetRx),     width: 6,  right_align: true });
        cols.push(ColSpec { title: "TX/s",    sort: Some(ProcessSort::NetTx),     width: 6,  right_align: true });
        cols.push(ColSpec { title: "DR/s",    sort: Some(ProcessSort::DiskRead),  width: 6,  right_align: true });
        cols.push(ColSpec { title: "DW/s",    sort: Some(ProcessSort::DiskWrite), width: 6,  right_align: true });
        if !self.compact_columns {
            cols.push(ColSpec { title: "Command", sort: None,                     width: u16::MAX, right_align: false });
        }
        let _ = self.show_net_columns; // kept for API back-compat; columns now always shown

        // Header row. Active sort column gets bracketed + arrow indicator.
        let header_style = Style::default().fg(self.theme.title).add_modifier(Modifier::BOLD);
        let active_style = Style::default().fg(self.theme.hi_fg).add_modifier(Modifier::BOLD | Modifier::REVERSED);
        let arrow = if self.sort_descending { '↓' } else { '↑' };
        render_row(
            buf,
            area.x,
            area.y,
            area.width,
            &cols,
            |idx| {
                let c = &cols[idx];
                let is_active = c.sort.map(|s| s == self.sort).unwrap_or(false);
                let tag = if is_active {
                    format!("{}{}", c.title, arrow)
                } else {
                    c.title.to_string()
                };
                let style = if is_active { active_style } else { header_style };
                (tag, style, c.right_align)
            },
        );

        // Data rows.
        let body_top = area.y + 1;
        let body_h = area.height.saturating_sub(1) as usize;
        let visible = self
            .rows
            .iter()
            .enumerate()
            .skip(self.scroll_offset)
            .take(body_h);

        let max_visible = body_h.max(1);
        for (i, (row_idx, row)) in visible.enumerate() {
            let y = body_top + i as u16;
            let is_selected = self.selected == Some(row_idx);
            let fade_t = (i as f32 / max_visible as f32) * FADE_END;

            // Selection background fill applies to either kind.
            if is_selected {
                for x in area.x..area.x + area.width {
                    buf[(x, y)].set_style(Style::default().bg(self.theme.selected_bg));
                }
            }

            match row {
                DisplayRow::Header(h) => {
                    self.render_header_row(buf, area, y, h, is_selected, &cols);
                }
                DisplayRow::Process(p) => {
                    self.render_process_row(
                        buf,
                        area,
                        y,
                        p,
                        is_selected,
                        fade_t,
                        &cols,
                    );
                }
            }
        }
    }
}

impl<'a> ProcessTable<'a> {
    fn render_header_row(
        &self,
        buf: &mut Buffer,
        area: Rect,
        y: u16,
        h: &GroupHeader,
        is_selected: bool,
        cols: &[ColSpec],
    ) {
        let glyph = if h.expanded { '▼' } else { '▶' };
        let label = format!("{glyph} {}", h.label);
        // Right side: aggregate metrics formatted like the data row's
        // CPU%/MEM/RX/TX/DR/DW columns. We reuse the same column widths
        // so the values line up under their headers.
        // Cells built to match the conditional `cols` layout above.
        // Order: Pid, Program, [User], Th, MEM, CPU%, RX, TX, DR, DW, [Command].
        let mut cells: Vec<String> = Vec::with_capacity(cols.len());
        cells.push(String::new());                           // Pid (blank)
        cells.push(String::new());                           // Program — overlaid below
        if !self.compact_columns {
            cells.push(String::new());                       // User
        }
        cells.push(h.threads_total.to_string());             // Th — sum across group
        cells.push(format_bytes(h.mem_rss_total));           // MEM
        cells.push(format!("{:.1}", h.cpu_fraction_total * 100.0)); // CPU%
        cells.push(opt_rate(h.net_rx_total));
        cells.push(opt_rate(h.net_tx_total));
        cells.push(opt_rate(h.disk_read_total));
        cells.push(opt_rate(h.disk_write_total));
        if !self.compact_columns {
            cells.push(String::new());                       // Command
        }
        let bg = if is_selected { Some(self.theme.selected_bg) } else { None };
        let fg = if is_selected { self.theme.selected_fg } else { self.theme.hi_fg };
        let style = match bg {
            Some(b) => Style::default().bg(b).fg(fg).add_modifier(Modifier::BOLD),
            None => Style::default().fg(fg).add_modifier(Modifier::BOLD),
        };
        render_row(buf, area.x, y, area.width, cols, |idx| {
            let s = cells[idx].clone();
            (s, style, cols[idx].right_align)
        });
        // Overwrite the Program column with the chevron + label. We do
        // this after the table render so it spans the column properly.
        write_str(
            buf,
            area.x + cols[0].width + 1,
            y,
            &label,
            cols[1].width as usize,
            style,
        );
    }

    fn render_process_row(
        &self,
        buf: &mut Buffer,
        area: Rect,
        y: u16,
        meta: &ProcessRowMeta,
        is_selected: bool,
        fade_t: f32,
        cols: &[ColSpec],
    ) {
        let p = &meta.info;
        let row_fg = lerp_color(self.theme.main_fg, self.theme.inactive_fg, fade_t);
        let base_style = if is_selected {
            Style::default()
                .bg(self.theme.selected_bg)
                .fg(self.theme.selected_fg)
        } else {
            Style::default().fg(row_fg)
        };

        let cpu_color = self.theme.cpu.sample(p.cpu_fraction.clamp(0.0, 1.0));
        let mem_color = self.theme.used.sample(
            (p.mem_rss_bytes as f64 / (32.0 * 1024.0 * 1024.0 * 1024.0)).min(1.0) as f32,
        );

        // Build the indent prefix. Tree mode draws branch glyphs; grouped
        // modes use a flat 2-space-per-depth indent.
        let prefix = if self.tree_mode {
            tree_prefix(meta)
        } else {
            "  ".repeat(meta.depth as usize)
        };

        let mut cells = build_row_cells(p, self.compact_columns);
        // Replace the Program cell with prefix + name (truncated to fit
        // the column allowance after the prefix). Program is always at
        // index 1 (immediately after Pid) regardless of compact mode.
        let prog_avail = (cols[1].width as usize).saturating_sub(prefix.chars().count());
        cells[1] = format!("{prefix}{}", truncate(&p.name, prog_avail.max(1)));

        render_row(buf, area.x, y, area.width, cols, |idx| {
            let s = cells[idx].clone();
            let mut style = base_style;
            if !is_selected {
                if cols[idx].title == "CPU%" {
                    style = style.fg(lerp_color(cpu_color, self.theme.inactive_fg, fade_t));
                } else if cols[idx].title == "MEM" {
                    style = style.fg(lerp_color(mem_color, self.theme.inactive_fg, fade_t));
                } else if cols[idx].title == "Pid" {
                    style = style.fg(self.theme.inactive_fg);
                }
            }
            (s, style, cols[idx].right_align)
        });
    }
}

/// Build a tree branch prefix for a process row using its
/// ancestor-continues bitmap and last-sibling flag. Examples:
///   depth=0:           ""
///   depth=1, last:     "└─ "
///   depth=1, mid:      "├─ "
///   depth=2, last,
///     parent had more: "│  └─ "
fn tree_prefix(meta: &ProcessRowMeta) -> String {
    if meta.depth == 0 {
        return String::new();
    }
    let mut out = String::new();
    for &cont in &meta.ancestor_continues {
        out.push_str(if cont { "│  " } else { "   " });
    }
    out.push_str(if meta.is_last_sibling { "└─ " } else { "├─ " });
    out
}

/// Linear interpolate between two terminal colors at `t ∈ [0,1]`.
/// Operates on RGB; falls back to `a` for non-RGB enum variants so
/// 256-color / named themes degrade gracefully (no fade, but no panic).
fn lerp_color(a: Color, b: Color, t: f32) -> Color {
    let t = t.clamp(0.0, 1.0);
    match (a, b) {
        (Color::Rgb(ar, ag, ab), Color::Rgb(br, bg, bb)) => {
            let r = (ar as f32 + (br as f32 - ar as f32) * t).round() as u8;
            let g = (ag as f32 + (bg as f32 - ag as f32) * t).round() as u8;
            let b = (ab as f32 + (bb as f32 - ab as f32) * t).round() as u8;
            Color::Rgb(r, g, b)
        }
        _ => a,
    }
}

fn build_row_cells(p: &ProcessInfo, compact: bool) -> Vec<String> {
    // Order MUST match the conditional `cols` layout in render():
    // Pid, Program, [User], Th, MEM, CPU%, RX, TX, DR, DW, [Command].
    let mut out = Vec::with_capacity(11);
    out.push(p.pid.to_string());
    out.push(truncate(&p.name, 12));
    if !compact {
        out.push(truncate(&p.user, 6));
    }
    out.push(p.threads.to_string());
    out.push(format_bytes(p.mem_rss_bytes));
    out.push(format!("{:.1}", p.cpu_fraction * 100.0));
    out.push(opt_rate(p.net_rx_bytes_per_sec));
    out.push(opt_rate(p.net_tx_bytes_per_sec));
    out.push(opt_rate(p.disk_read_bytes_per_sec));
    out.push(opt_rate(p.disk_write_bytes_per_sec));
    if !compact {
        let cmd = if p.cmdline.is_empty() {
            p.name.clone()
        } else {
            p.cmdline.clone()
        };
        out.push(cmd);
    }
    out
}

fn opt_rate(v: Option<f64>) -> String {
    match v {
        Some(r) if r > 0.5 => format_rate(r),
        Some(_) => "0".into(),
        None => "-".into(),
    }
}

fn render_row<F>(buf: &mut Buffer, x0: u16, y: u16, total_width: u16, cols: &[ColSpec], mut cell_fn: F)
where
    F: FnMut(usize) -> (String, Style, bool),
{
    if total_width == 0 || cols.is_empty() {
        return;
    }
    let right_limit = x0.saturating_add(total_width);
    // Sum the *fixed* column widths (Command uses u16::MAX as a sentinel that
    // means "soak up the remainder"). One-cell gutter between columns.
    let n = cols.len() as u16;
    let fixed_total: u16 = cols
        .iter()
        .filter(|c| c.width != u16::MAX)
        .map(|c| c.width)
        .sum::<u16>()
        .saturating_add(n.saturating_sub(1));
    let flex_remaining = total_width.saturating_sub(fixed_total);

    let mut cursor = x0;
    for (i, col) in cols.iter().enumerate() {
        if cursor >= right_limit {
            break;
        }
        let col_width = if col.width == u16::MAX {
            flex_remaining
        } else {
            col.width
        };
        let avail = right_limit.saturating_sub(cursor).min(col_width);
        if avail == 0 {
            break;
        }
        let (text, style, right_align) = cell_fn(i);
        let len = text.chars().count() as u16;
        let (text_x, text) = if right_align && len < avail {
            (cursor + (avail - len), text)
        } else if len > avail {
            (cursor, truncate(&text, avail as usize))
        } else {
            (cursor, text)
        };
        write_str(buf, text_x, y, &text, avail as usize, style);
        cursor = cursor.saturating_add(col_width).saturating_add(1);
    }
}

fn write_str(buf: &mut Buffer, x: u16, y: u16, s: &str, max_cols: usize, style: Style) {
    let mut col = x;
    let right = x.saturating_add(max_cols as u16).min(buf.area.right());
    for ch in s.chars() {
        if col >= right {
            break;
        }
        let c = &mut buf[(col, y)];
        c.set_char(ch);
        c.set_style(c.style().patch(style));
        col = col.saturating_add(1);
    }
}

fn truncate(s: &str, max_chars: usize) -> String {
    if s.chars().count() <= max_chars {
        s.to_string()
    } else if max_chars == 0 {
        String::new()
    } else {
        let mut out: String = s.chars().take(max_chars.saturating_sub(1)).collect();
        out.push('…');
        out
    }
}

fn format_bytes(b: u64) -> String {
    const KIB: u64 = 1024;
    const MIB: u64 = KIB * 1024;
    const GIB: u64 = MIB * 1024;
    if b >= GIB {
        format!("{:.1}G", b as f64 / GIB as f64)
    } else if b >= MIB {
        format!("{:.0}M", b as f64 / MIB as f64)
    } else if b >= KIB {
        format!("{:.0}K", b as f64 / KIB as f64)
    } else {
        format!("{b}B")
    }
}

fn format_rate(bps: f64) -> String {
    if bps >= 1024.0 * 1024.0 {
        format!("{:.1}M", bps / (1024.0 * 1024.0))
    } else if bps >= 1024.0 {
        format!("{:.1}K", bps / 1024.0)
    } else {
        format!("{:.0}B", bps)
    }
}

#[cfg(test)]
mod tests {
    use std::time::Instant;

    use bobtop_core::sample::ProcessState;

    use super::*;

    fn proc(pid: u32, name: &str, cpu: f32, mem_mb: u64) -> ProcessInfo {
        ProcessInfo {
            pid,
            parent_pid: None,
            name: name.into(),
            cmdline: String::new(),
            user: "root".into(),
            state: ProcessState::Running,
            cpu_fraction: cpu,
            mem_rss_bytes: mem_mb * 1024 * 1024,
            mem_vsz_bytes: mem_mb * 2 * 1024 * 1024,
            threads: 4,
            net_rx_bytes_per_sec: None,
            net_tx_bytes_per_sec: None,
            disk_read_bytes_per_sec: None,
            disk_write_bytes_per_sec: None,
            cgroup: None,
        }
    }

    fn flat_rows(ps: Vec<ProcessInfo>) -> Vec<DisplayRow> {
        ps.into_iter()
            .map(|info| {
                DisplayRow::Process(ProcessRowMeta {
                    info,
                    depth: 0,
                    is_last_sibling: false,
                    ancestor_continues: Vec::new(),
                })
            })
            .collect()
    }

    #[test]
    fn header_renders_at_first_row() {
        let theme = Theme::fallback();
        let rows = flat_rows(vec![proc(1, "init", 0.01, 5)]);
        let table = ProcessTable::new(&rows, &theme);
        let area = Rect::new(0, 0, 80, 4);
        let mut buf = Buffer::empty(area);
        let _ = Instant::now();
        (&table).render(area, &mut buf);
        // First column header is "Pid" right-aligned in 8-cell column.
        // (After sort indicator, on the active "Cpu" column.)
        // We assert "P" appears somewhere on row 0.
        let row0: String = (0..area.width).map(|x| buf[(x, 0)].symbol()).collect::<Vec<_>>().join("");
        assert!(row0.contains("Pid"), "header missing Pid: {row0}");
        assert!(row0.contains("CPU"), "header missing CPU: {row0}");
    }

    #[test]
    fn data_row_shows_pid_name_cpu() {
        let theme = Theme::fallback();
        let rows = flat_rows(vec![proc(12345, "cargo", 0.42, 256)]);
        let table = ProcessTable::new(&rows, &theme);
        let area = Rect::new(0, 0, 80, 3);
        let mut buf = Buffer::empty(area);
        (&table).render(area, &mut buf);
        let row1: String = (0..area.width).map(|x| buf[(x, 1)].symbol()).collect::<Vec<_>>().join("");
        assert!(row1.contains("12345"), "missing pid: {row1}");
        assert!(row1.contains("cargo"), "missing name: {row1}");
        assert!(row1.contains("42.0"), "missing cpu%: {row1}");
    }

    #[test]
    fn selected_row_gets_background_fill() {
        let theme = Theme::fallback();
        let rows = flat_rows(vec![proc(1, "a", 0.1, 1), proc(2, "b", 0.1, 1)]);
        let table = ProcessTable::new(&rows, &theme).with_selection(Some(1), 0);
        let area = Rect::new(0, 0, 80, 4);
        let mut buf = Buffer::empty(area);
        (&table).render(area, &mut buf);
        // Row at y=2 corresponds to rows[1]. Every cell should have the
        // selected_bg as its background color.
        for x in 0..area.width {
            let style = buf[(x, 2)].style();
            assert_eq!(style.bg, Some(theme.selected_bg), "col {x}");
        }
    }

    #[test]
    fn net_columns_appear_when_enabled() {
        let theme = Theme::fallback();
        let mut p = proc(1, "x", 0.0, 0);
        p.net_rx_bytes_per_sec = Some(2048.0);
        p.net_tx_bytes_per_sec = Some(512.0);
        let rows = flat_rows(vec![p]);
        let table = ProcessTable::new(&rows, &theme).with_net_columns(true);
        let area = Rect::new(0, 0, 100, 3);
        let mut buf = Buffer::empty(area);
        (&table).render(area, &mut buf);
        let header: String = (0..area.width).map(|x| buf[(x, 0)].symbol()).collect::<Vec<_>>().join("");
        assert!(header.contains("RX/s"));
        assert!(header.contains("TX/s"));
        let row1: String = (0..area.width).map(|x| buf[(x, 1)].symbol()).collect::<Vec<_>>().join("");
        assert!(row1.contains("2.0K"), "rx missing: {row1}");
    }

    #[test]
    fn compact_columns_drops_user_and_command_headers() {
        let theme = Theme::fallback();
        let rows = flat_rows(vec![proc(1, "init", 0.01, 5)]);
        let table = ProcessTable::new(&rows, &theme).with_compact_columns(true);
        let area = Rect::new(0, 0, 80, 4);
        let mut buf = Buffer::empty(area);
        (&table).render(area, &mut buf);
        let row0: String =
            (0..area.width).map(|x| buf[(x, 0)].symbol()).collect::<Vec<_>>().join("");
        assert!(!row0.contains("User"), "User col should be hidden when compact: {row0}");
        assert!(!row0.contains("Command"), "Command col should be hidden when compact: {row0}");
        assert!(row0.contains("Pid"), "Pid still present");
        assert!(row0.contains("MEM"), "MEM still present");
    }

    #[test]
    fn truncate_respects_char_count() {
        assert_eq!(truncate("hello", 10), "hello");
        assert_eq!(truncate("hello world", 5), "hell…");
        assert_eq!(truncate("anything", 0), "");
    }
}
