//! Process table — the system-monitor adapter on top of the generic
//! [`LiveTable`](gtui::widgets::LiveTable).
//!
//! The widget itself lives in `gtui`; this module owns:
//!
//! 1. Row/header types ([`TableRow`], [`TableRowMeta`], [`TableGroupHeader`])
//!    that the daemon's grouping pipeline produces.
//! 2. The column id ([`ProcCol`]) and layout ([`TableLayout`]) that
//!    parameterize column-set selection per group mode.
//! 3. The closed sort enum ([`TableSort`]) daemon code matches on, plus
//!    the `col()` mapping into `ProcCol` for the live-table indicator.
//! 4. [`MetricScales`] + per-metric color helpers — every gradient choice
//!    the process panel makes (cpu/mem/threads/rates/user_color) lives
//!    here so [`ProcessRowView`] / [`GroupView`] can pre-resolve cell
//!    colors before passing through `TableRowExt`/`GroupAggregate`.
//! 5. [`DataTable`] — the public widget. Builds column defs + view rows
//!    from the inputs the panel render path supplies and renders the
//!    underlying `LiveTable` with `fade=false` to preserve the existing
//!    flat-list visual.

use crate::core::sample::ProcessInfo;
use gtui::color::Gradient;
use gtui::text::{format_bytes_compact, format_rate};
use gtui::widgets::live_table::{
    Align, Cell, ColumnDef, GroupAggregate, LiveTable, TableEntry, TableRowExt, WidthSpec,
};
use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::style::Color;
use ratatui::widgets::Widget;

use crate::monitor_theme::MonitorTheme;

#[derive(Debug, Clone)]
pub enum TableRow {
    Header(TableGroupHeader),
    Item(TableRowMeta),
}

#[derive(Debug, Clone)]
pub struct TableGroupHeader {
    pub key: String,
    pub label: String,
    pub proc_count: usize,
    pub threads_total: u32,
    pub cpu_fraction_total: f32,
    pub mem_rss_total: u64,
    pub net_rx_total: Option<f64>,
    pub net_tx_total: Option<f64>,
    pub disk_read_total: Option<f64>,
    pub disk_write_total: Option<f64>,
    pub dominant_user: String,
    pub expanded: bool,
}

#[derive(Debug, Clone)]
pub struct TableRowMeta {
    pub info: ProcessInfo,
    pub depth: u8,
    pub is_last_sibling: bool,
    pub ancestor_continues: Vec<bool>,
}

/// Column id — every render-time decision the widget makes keys off
/// this. `ProcCol::Program` is the "label column" (chevron + tree
/// glyphs go here).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ProcCol {
    Pid,
    Program,
    Command,
    User,
    Threads,
    Mem,
    Cpu,
    NetRx,
    NetTx,
    DiskRead,
    DiskWrite,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TableSort {
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

impl TableSort {
    /// Cycle order for the `←` / `→` keybinds — left-to-right display
    /// order so selection visually tracks the arrow direction.
    pub fn cycle() -> &'static [TableSort] {
        &[
            TableSort::Pid,
            TableSort::Name,
            TableSort::User,
            TableSort::Threads,
            TableSort::Mem,
            TableSort::Cpu,
            TableSort::NetRx,
            TableSort::NetTx,
            TableSort::DiskRead,
            TableSort::DiskWrite,
        ]
    }

    /// Stable label for the panel-title indicator (e.g. `[cpu↓]`).
    pub fn label(self) -> &'static str {
        match self {
            TableSort::Pid => "pid",
            TableSort::Name => "name",
            TableSort::User => "user",
            TableSort::Threads => "threads",
            TableSort::Mem => "mem",
            TableSort::Cpu => "cpu",
            TableSort::NetRx => "rx",
            TableSort::NetTx => "tx",
            TableSort::DiskRead => "dr",
            TableSort::DiskWrite => "dw",
        }
    }

    fn col(self) -> ProcCol {
        match self {
            TableSort::Pid => ProcCol::Pid,
            TableSort::Name => ProcCol::Program,
            TableSort::User => ProcCol::User,
            TableSort::Threads => ProcCol::Threads,
            TableSort::Mem => ProcCol::Mem,
            TableSort::Cpu => ProcCol::Cpu,
            TableSort::NetRx => ProcCol::NetRx,
            TableSort::NetTx => ProcCol::NetTx,
            TableSort::DiskRead => ProcCol::DiskRead,
            TableSort::DiskWrite => ProcCol::DiskWrite,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum TableLayout {
    #[default]
    Flat,
    Grouped,
    Tree,
}

impl TableLayout {
    pub fn draws_tree_glyphs(self) -> bool {
        matches!(self, TableLayout::Tree)
    }

    pub fn includes_pid(self) -> bool {
        matches!(self, TableLayout::Flat | TableLayout::Tree)
    }

    pub fn includes_command(self) -> bool {
        matches!(self, TableLayout::Flat)
    }

    pub fn includes_user(self) -> bool {
        true
    }
}

/// Per-tick maxima used to scale gradient colors for the rate columns.
/// CPU and Mem have natural ceilings (a single core / a fixed GiB
/// reference) so they aren't tracked here — net and disk rates are
/// open-ended and look uniform unless we normalize against the most
/// active row in this frame.
#[derive(Debug, Default, Clone, Copy)]
pub struct MetricScales {
    pub threads: u32,
    pub net_rx: f64,
    pub net_tx: f64,
    pub disk_r: f64,
    pub disk_w: f64,
}

impl MetricScales {
    pub fn from_rows(rows: &[TableRow]) -> Self {
        let mut s = MetricScales::default();
        for row in rows {
            if let TableRow::Item(meta) = row {
                let p = &meta.info;
                s.threads = s.threads.max(p.threads);
                if let Some(v) = p.net_rx_bytes_per_sec {
                    if v > s.net_rx {
                        s.net_rx = v;
                    }
                }
                if let Some(v) = p.net_tx_bytes_per_sec {
                    if v > s.net_tx {
                        s.net_tx = v;
                    }
                }
                if let Some(v) = p.disk_read_bytes_per_sec {
                    if v > s.disk_r {
                        s.disk_r = v;
                    }
                }
                if let Some(v) = p.disk_write_bytes_per_sec {
                    if v > s.disk_w {
                        s.disk_w = v;
                    }
                }
            }
        }
        s
    }
}

/// Hash-based stable color picker for the User column. Non-root users
/// get a deterministic hue from the theme's process gradient (so multi-
/// user hosts read at a glance — same uid is the same color across
/// sessions). Root highlights with `hi_fg`. `—` (mixed-user group)
/// renders dim.
fn user_color(user: &str, theme: &MonitorTheme) -> Color {
    if user == "—" || user.is_empty() {
        return theme.inactive_fg;
    }
    if user == "root" {
        return theme.hi_fg;
    }
    let mut h: u32 = 5381;
    for b in user.bytes() {
        h = h.wrapping_mul(33).wrapping_add(b as u32);
    }
    let slot = (h % 256) as f32 / 255.0;
    theme.process.sample(slot)
}

fn cpu_color(fraction: f32, theme: &MonitorTheme) -> Color {
    theme.cpu.sample(fraction.clamp(0.0, 1.0))
}

fn mem_color(bytes: u64, theme: &MonitorTheme) -> Color {
    // 32 GiB reference matches btop's heuristic — proxy for "this
    // process is using a meaningful slice of system memory."
    let r = if bytes == 0 {
        0.0
    } else {
        (bytes as f64 / (32.0 * 1024.0 * 1024.0 * 1024.0)).clamp(0.0, 1.0) as f32
    };
    theme.used.sample(r)
}

fn threads_color(n: u32, max: u32, theme: &MonitorTheme) -> Color {
    if max == 0 {
        return theme.inactive_fg;
    }
    let r = (n as f32 / max as f32).clamp(0.0, 1.0);
    if r < 0.10 {
        // Most processes have a handful of threads; don't make them
        // shout. Reserve color for the outliers.
        theme.inactive_fg
    } else {
        theme.process.sample(r)
    }
}

fn rate_color(value: Option<f64>, max: f64, gradient: Gradient, theme: &MonitorTheme) -> Color {
    match value {
        None => theme.inactive_fg,
        Some(v) if v <= 0.0 || max <= 0.0 => theme.inactive_fg,
        Some(v) => {
            // Normalize against the largest value in the column this
            // tick. Floors at 5 % so a single noisy process doesn't
            // wash everything else into the cool end of the gradient.
            let r = (v / max).clamp(0.05, 1.0) as f32;
            gradient.sample(r)
        }
    }
}

fn opt_rate_text(v: Option<f64>) -> String {
    match v {
        Some(r) if r > 0.5 => format_rate(r),
        Some(_) => "0".into(),
        None => "-".into(),
    }
}

pub struct DataTable<'a> {
    pub rows: &'a [TableRow],
    pub selected: Option<usize>,
    pub scroll_offset: usize,
    pub theme: &'a MonitorTheme,
    pub show_net_columns: bool,
    pub sort: TableSort,
    pub sort_descending: bool,
    pub layout: TableLayout,
    /// Sticky-by-pid selection. When `Some(pid)`, the widget locates
    /// the row whose `ProcessRowView::key()` matches and highlights
    /// it — overriding `selected`. The app passes its tracked
    /// `selected_proc_pid` here when sticky mode is on; the row
    /// stays visually selected across re-sorts and re-filters
    /// without the app having to remap indices first.
    pub sticky_pid: Option<u32>,
}

impl<'a> DataTable<'a> {
    pub fn new(rows: &'a [TableRow], theme: &'a MonitorTheme) -> Self {
        Self {
            rows,
            selected: None,
            scroll_offset: 0,
            theme,
            show_net_columns: false,
            sort: TableSort::Cpu,
            sort_descending: true,
            layout: TableLayout::Flat,
            sticky_pid: None,
        }
    }

    pub fn with_layout(mut self, layout: TableLayout) -> Self {
        self.layout = layout;
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

    pub fn with_sort(mut self, sort: TableSort) -> Self {
        self.sort = sort;
        self
    }

    pub fn with_sticky_pid(mut self, pid: Option<u32>) -> Self {
        self.sticky_pid = pid;
        self
    }
}

impl<'a> Widget for &DataTable<'a> {
    fn render(self, area: Rect, buf: &mut Buffer) {
        let columns = build_columns(self.layout, self.show_net_columns);
        let scales = MetricScales::from_rows(self.rows);
        let draws_tree = self.layout.draws_tree_glyphs();

        let entries: Vec<TableEntry<ProcessRowView<'_>, GroupView<'_>>> = self
            .rows
            .iter()
            .map(|row| match row {
                TableRow::Header(h) => TableEntry::Header(GroupView::new(
                    h,
                    self.theme,
                    self.show_net_columns,
                    &scales,
                )),
                TableRow::Item(meta) => TableEntry::Item(ProcessRowView::new(
                    meta,
                    self.theme,
                    self.show_net_columns,
                    &scales,
                    draws_tree,
                )),
            })
            .collect();

        let table = LiveTable::new(&entries, &columns, &self.theme.base, ProcCol::Program)
            .with_selection(self.selected, self.scroll_offset)
            .with_sort(Some(self.sort.col()), self.sort_descending)
            .with_tree_glyphs(draws_tree)
            .with_fade(false)
            .with_sticky_key(self.sticky_pid.map(|p| p as u64));
        (&table).render(area, buf);
    }
}

fn build_columns(layout: TableLayout, show_net: bool) -> Vec<ColumnDef<ProcCol>> {
    // Order: [Pid] · Program · [Command] · User · Th · MEM · CPU% · [RX · TX] · DR · DW
    // Program flexes for Grouped/Tree (group label / tree glyphs);
    // Command flexes for Flat (full argv).
    let mut cols = Vec::with_capacity(11);
    if layout.includes_pid() {
        cols.push(ColumnDef { id: ProcCol::Pid, label: "Pid", width: WidthSpec::Fixed(6), align: Align::Right, sortable: true });
    }
    let program_width = match layout {
        TableLayout::Flat => WidthSpec::Fixed(12),
        TableLayout::Grouped | TableLayout::Tree => WidthSpec::Flex,
    };
    cols.push(ColumnDef { id: ProcCol::Program, label: "Program", width: program_width, align: Align::Left, sortable: true });
    if layout.includes_command() {
        cols.push(ColumnDef { id: ProcCol::Command, label: "Command", width: WidthSpec::Flex, align: Align::Left, sortable: false });
    }
    cols.push(ColumnDef { id: ProcCol::User,    label: "User", width: WidthSpec::Fixed(6), align: Align::Left,  sortable: true });
    cols.push(ColumnDef { id: ProcCol::Threads, label: "Th",   width: WidthSpec::Fixed(3), align: Align::Right, sortable: true });
    cols.push(ColumnDef { id: ProcCol::Mem,     label: "MEM",  width: WidthSpec::Fixed(6), align: Align::Right, sortable: true });
    cols.push(ColumnDef { id: ProcCol::Cpu,     label: "CPU%", width: WidthSpec::Fixed(5), align: Align::Right, sortable: true });
    if show_net {
        cols.push(ColumnDef { id: ProcCol::NetRx, label: "RX/s", width: WidthSpec::Fixed(6), align: Align::Right, sortable: true });
        cols.push(ColumnDef { id: ProcCol::NetTx, label: "TX/s", width: WidthSpec::Fixed(6), align: Align::Right, sortable: true });
    }
    cols.push(ColumnDef { id: ProcCol::DiskRead,  label: "DR/s", width: WidthSpec::Fixed(6), align: Align::Right, sortable: true });
    cols.push(ColumnDef { id: ProcCol::DiskWrite, label: "DW/s", width: WidthSpec::Fixed(6), align: Align::Right, sortable: true });
    cols
}

/// Per-process view that pre-resolves every cell's color so the
/// generic [`LiveTable`] only sees text + final fg per cell.
pub(crate) struct ProcessRowView<'a> {
    meta: &'a TableRowMeta,
    show_net: bool,
    /// Tree-mode flag — controls whether the Program cell's color is
    /// `accent_subtle` (matching the existing render path's tree-mode
    /// look) or unset (default `main_fg`).
    draws_tree_glyphs: bool,
    cpu: Color,
    mem: Color,
    threads: Color,
    user: Color,
    net_rx: Color,
    net_tx: Color,
    disk_r: Color,
    disk_w: Color,
    inactive_fg: Color,
    accent_subtle: Color,
}

impl<'a> ProcessRowView<'a> {
    fn new(
        meta: &'a TableRowMeta,
        theme: &MonitorTheme,
        show_net: bool,
        scales: &MetricScales,
        draws_tree_glyphs: bool,
    ) -> Self {
        let p = &meta.info;
        Self {
            meta,
            show_net,
            draws_tree_glyphs,
            cpu: cpu_color(p.cpu_fraction, theme),
            mem: mem_color(p.mem_rss_bytes, theme),
            threads: threads_color(p.threads, scales.threads, theme),
            user: user_color(&p.user, theme),
            net_rx: rate_color(p.net_rx_bytes_per_sec, scales.net_rx, theme.download, theme),
            net_tx: rate_color(p.net_tx_bytes_per_sec, scales.net_tx, theme.upload, theme),
            disk_r: rate_color(p.disk_read_bytes_per_sec, scales.disk_r, theme.used, theme),
            disk_w: rate_color(p.disk_write_bytes_per_sec, scales.disk_w, theme.used, theme),
            inactive_fg: theme.inactive_fg,
            accent_subtle: theme.accent_subtle,
        }
    }
}

impl<'a> TableRowExt<ProcCol> for ProcessRowView<'a> {
    fn cell(&self, col: ProcCol) -> Cell {
        let p = &self.meta.info;
        match col {
            ProcCol::Pid => Cell::styled(p.pid.to_string(), self.inactive_fg),
            ProcCol::Program => {
                // Tree mode: existing render path colored the entire
                // Program cell (prefix + name) `accent_subtle`. Match
                // that here so the migration is a visual no-op.
                if self.draws_tree_glyphs {
                    Cell::styled(p.name.clone(), self.accent_subtle)
                } else {
                    Cell::plain(p.name.clone())
                }
            }
            ProcCol::Command => {
                let cmd = if p.cmdline.is_empty() {
                    p.name.clone()
                } else {
                    p.cmdline.clone()
                };
                Cell::styled(cmd, self.inactive_fg)
            }
            ProcCol::User => Cell::styled(p.user.clone(), self.user),
            ProcCol::Threads => Cell::styled(p.threads.to_string(), self.threads),
            ProcCol::Mem => Cell::styled(format_bytes_compact(p.mem_rss_bytes), self.mem),
            ProcCol::Cpu => Cell::styled(format!("{:.1}", p.cpu_fraction * 100.0), self.cpu),
            ProcCol::NetRx => Cell::styled(
                opt_rate_text(if self.show_net { p.net_rx_bytes_per_sec } else { None }),
                self.net_rx,
            ),
            ProcCol::NetTx => Cell::styled(
                opt_rate_text(if self.show_net { p.net_tx_bytes_per_sec } else { None }),
                self.net_tx,
            ),
            ProcCol::DiskRead => Cell::styled(opt_rate_text(p.disk_read_bytes_per_sec), self.disk_r),
            ProcCol::DiskWrite => Cell::styled(opt_rate_text(p.disk_write_bytes_per_sec), self.disk_w),
        }
    }

    fn tree_depth(&self) -> u8 {
        self.meta.depth
    }

    fn ancestor_continues(&self) -> &[bool] {
        &self.meta.ancestor_continues
    }

    fn is_last_sibling(&self) -> bool {
        self.meta.is_last_sibling
    }

    fn key(&self) -> Option<u64> {
        // Pids are stable for a process's lifetime — perfect sticky key.
        // u32 fits trivially into u64.
        Some(self.meta.info.pid as u64)
    }

    fn matches_filter(&self, q: &str) -> bool {
        // Match against name and cmdline, case-insensitively. Mirrors
        // the daemon's existing app-level filter; exposing it through
        // the trait lets future apps drive the same filter via
        // `LiveTable::with_filter`.
        let needle = q.to_lowercase();
        let p = &self.meta.info;
        p.name.to_lowercase().contains(&needle) || p.cmdline.to_lowercase().contains(&needle)
    }
}

/// View over a group header that pre-resolves aggregate-cell colors
/// using the same gradients as data rows. The widget overlays the
/// chevron + group label on the `Program` column itself.
pub(crate) struct GroupView<'a> {
    h: &'a TableGroupHeader,
    show_net: bool,
    user: Color,
    threads: Color,
    mem: Color,
    cpu: Color,
    net_rx: Color,
    net_tx: Color,
    disk_r: Color,
    disk_w: Color,
}

impl<'a> GroupView<'a> {
    fn new(h: &'a TableGroupHeader, theme: &MonitorTheme, show_net: bool, scales: &MetricScales) -> Self {
        Self {
            h,
            show_net,
            user: user_color(&h.dominant_user, theme),
            threads: threads_color(h.threads_total, scales.threads, theme),
            mem: mem_color(h.mem_rss_total, theme),
            cpu: cpu_color(h.cpu_fraction_total, theme),
            net_rx: rate_color(h.net_rx_total, scales.net_rx, theme.download, theme),
            net_tx: rate_color(h.net_tx_total, scales.net_tx, theme.upload, theme),
            disk_r: rate_color(h.disk_read_total, scales.disk_r, theme.used, theme),
            disk_w: rate_color(h.disk_write_total, scales.disk_w, theme.used, theme),
        }
    }
}

impl<'a> GroupAggregate<ProcCol> for GroupView<'a> {
    fn label(&self) -> &str {
        &self.h.label
    }

    fn expanded(&self) -> bool {
        self.h.expanded
    }

    fn cell(&self, col: ProcCol) -> Cell {
        match col {
            // Pid / Program / Command stay blank for headers; the widget
            // overlays the chevron + group label in the Program column.
            ProcCol::Pid | ProcCol::Program | ProcCol::Command => Cell::plain(""),
            ProcCol::User => Cell::styled(self.h.dominant_user.clone(), self.user),
            ProcCol::Threads => Cell::styled(self.h.threads_total.to_string(), self.threads),
            ProcCol::Mem => Cell::styled(format_bytes_compact(self.h.mem_rss_total), self.mem),
            ProcCol::Cpu => Cell::styled(format!("{:.1}", self.h.cpu_fraction_total * 100.0), self.cpu),
            ProcCol::NetRx => Cell::styled(
                opt_rate_text(if self.show_net { self.h.net_rx_total } else { None }),
                self.net_rx,
            ),
            ProcCol::NetTx => Cell::styled(
                opt_rate_text(if self.show_net { self.h.net_tx_total } else { None }),
                self.net_tx,
            ),
            ProcCol::DiskRead => Cell::styled(opt_rate_text(self.h.disk_read_total), self.disk_r),
            ProcCol::DiskWrite => Cell::styled(opt_rate_text(self.h.disk_write_total), self.disk_w),
        }
    }
}

#[cfg(test)]
mod tests {
    use std::time::Instant;

    use crate::core::sample::ProcessState;
    use ratatui::buffer::Buffer;
    use ratatui::layout::Rect;
    use ratatui::widgets::Widget;

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
            container: None,
        }
    }

    fn flat_rows(ps: Vec<ProcessInfo>) -> Vec<TableRow> {
        ps.into_iter()
            .map(|info| {
                TableRow::Item(TableRowMeta {
                    info,
                    depth: 0,
                    is_last_sibling: false,
                    ancestor_continues: Vec::new(),
                })
            })
            .collect()
    }

    fn read_text(buf: &Buffer, y: u16, x_start: u16, len: u16) -> String {
        (x_start..x_start + len)
            .filter_map(|x| buf[(x, y)].symbol().chars().next())
            .collect()
    }

    #[test]
    fn header_renders_at_first_row() {
        let theme = MonitorTheme::fallback();
        let rows = flat_rows(vec![proc(1, "init", 0.01, 5)]);
        let table = DataTable::new(&rows, &theme);
        let area = Rect::new(0, 0, 80, 4);
        let mut buf = Buffer::empty(area);
        let _ = Instant::now();
        (&table).render(area, &mut buf);
        let header = read_text(&buf, 0, 0, 80);
        assert!(header.contains("Pid"), "got {header:?}");
        assert!(header.contains("Program"), "got {header:?}");
        assert!(header.contains("CPU%"), "got {header:?}");
    }

    #[test]
    fn data_row_shows_pid_name_cpu() {
        let theme = MonitorTheme::fallback();
        let rows = flat_rows(vec![proc(12345, "firefox", 0.42, 256)]);
        let table = DataTable::new(&rows, &theme);
        let area = Rect::new(0, 0, 80, 4);
        let mut buf = Buffer::empty(area);
        (&table).render(area, &mut buf);
        let row1 = read_text(&buf, 1, 0, 80);
        assert!(row1.contains("12345"), "missing pid: {row1}");
        assert!(row1.contains("firefox"), "missing name: {row1}");
        assert!(row1.contains("42.0"), "missing cpu%: {row1}");
    }

    #[test]
    fn selected_row_gets_full_width_highlight() {
        let theme = MonitorTheme::fallback();
        let rows = flat_rows(vec![proc(1, "init", 0.01, 5)]);
        let table = DataTable::new(&rows, &theme).with_selection(Some(0), 0);
        let area = Rect::new(0, 0, 80, 4);
        let mut buf = Buffer::empty(area);
        (&table).render(area, &mut buf);
        for x in 0..80 {
            let style = buf[(x, 1)].style();
            assert_eq!(style.bg, Some(theme.selected_bg), "col {x}");
        }
    }

    #[test]
    fn group_header_renders_chevron_and_aggregates() {
        let theme = MonitorTheme::fallback();
        let rows = vec![TableRow::Header(TableGroupHeader {
            key: "g1".into(),
            label: "firefox.service".into(),
            proc_count: 47,
            threads_total: 312,
            cpu_fraction_total: 0.18,
            mem_rss_total: 2 * 1024 * 1024 * 1024,
            net_rx_total: None,
            net_tx_total: None,
            disk_read_total: None,
            disk_write_total: None,
            dominant_user: "alice".into(),
            expanded: true,
        })];
        let table = DataTable::new(&rows, &theme).with_layout(TableLayout::Grouped);
        let area = Rect::new(0, 0, 80, 4);
        let mut buf = Buffer::empty(area);
        (&table).render(area, &mut buf);
        let row1 = read_text(&buf, 1, 0, 80);
        assert!(row1.contains('▼'), "missing expanded chevron: {row1:?}");
        assert!(row1.contains("firefox.service"), "missing group label: {row1:?}");
        assert!(row1.contains("312"), "missing threads total: {row1:?}");
        assert!(row1.contains("18.0"), "missing cpu total: {row1:?}");
    }

    #[test]
    fn root_user_uses_hi_fg() {
        let theme = MonitorTheme::fallback();
        assert_eq!(user_color("root", &theme), theme.hi_fg);
        assert_eq!(user_color("—", &theme), theme.inactive_fg);
        assert_eq!(user_color("", &theme), theme.inactive_fg);
        // Same input → same output (deterministic hash)
        assert_eq!(user_color("alice", &theme), user_color("alice", &theme));
    }

    #[test]
    fn metric_scales_pick_max_across_rows() {
        let mut p1 = proc(1, "a", 0.0, 0);
        p1.threads = 5;
        p1.net_rx_bytes_per_sec = Some(100.0);
        let mut p2 = proc(2, "b", 0.0, 0);
        p2.threads = 12;
        p2.net_rx_bytes_per_sec = Some(80.0);
        let rows = flat_rows(vec![p1, p2]);
        let scales = MetricScales::from_rows(&rows);
        assert_eq!(scales.threads, 12);
        assert_eq!(scales.net_rx, 100.0);
    }
}
