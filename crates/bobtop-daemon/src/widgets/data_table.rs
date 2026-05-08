//! Process table — the system-monitor adapter on top of the generic
//! [`LiveTable`](bobtop_tui::widgets::LiveTable).
//!
//! The widget itself is in `bobtop-tui`; this module provides:
//!
//! 1. The column-id enum ([`ProcCol`]) and column defs for the three
//!    layouts (Flat / Grouped / Tree) the monitor uses.
//! 2. [`TableRowExt`] / [`GroupAggregate`] impls on internal view types
//!    that pre-sample CPU and memory gradients before passing through.
//! 3. [`TableSort`] — the closed sort enum daemon code (`group.rs`,
//!    `presets.rs`, `proc_sort.rs`) matches on. Wraps `ProcCol`.
//! 4. [`DataTable`] — a thin builder that materializes the view rows and
//!    column defs, then renders the wrapped `LiveTable`.

use bobtop_core::sample::ProcessInfo;
use bobtop_tui::text::{format_bytes_compact, format_rate};
use bobtop_tui::widgets::live_table::{
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

    /// Width preset for the `Program` column. Used by the `ui::processes`
    /// render path that still drives the simpler `bobtop_tui::Table`
    /// widget directly (not yet migrated to [`DataTable`] / [`LiveTable`]).
    pub fn program_width(self) -> u16 {
        match self {
            TableLayout::Flat => 12,
            TableLayout::Grouped | TableLayout::Tree => u16::MAX,
        }
    }

    /// Width preset for the `Command` column. `0` is a sentinel — only
    /// the `Flat` layout renders Command at all.
    pub fn command_width(self) -> u16 {
        match self {
            TableLayout::Flat => u16::MAX,
            _ => 0,
        }
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
}

impl<'a> Widget for &DataTable<'a> {
    fn render(self, area: Rect, buf: &mut Buffer) {
        let columns = build_columns(self.layout, self.show_net_columns);

        let entries: Vec<TableEntry<ProcessRowView<'_>, GroupView<'_>>> = self
            .rows
            .iter()
            .map(|row| match row {
                TableRow::Header(h) => TableEntry::Header(GroupView { h, show_net: self.show_net_columns }),
                TableRow::Item(meta) => TableEntry::Item(ProcessRowView::new(meta, self.theme, self.show_net_columns)),
            })
            .collect();

        let table = LiveTable::new(&entries, &columns, &self.theme.base, ProcCol::Program)
            .with_selection(self.selected, self.scroll_offset)
            .with_sort(Some(self.sort.col()), self.sort_descending)
            .with_tree_glyphs(self.layout.draws_tree_glyphs());
        (&table).render(area, buf);
    }
}

fn build_columns(layout: TableLayout, show_net: bool) -> Vec<ColumnDef<ProcCol>> {
    // Order: [Pid] · Program · [Command] · User · Th · MEM · CPU% · [RX · TX] · DR · DW
    // Program flexes for Grouped/Tree (group label / tree glyphs); Command flexes for Flat (full argv).
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

/// View of a process row that pre-samples gradient colors at construction
/// so `cell()` is cheap and self-contained.
struct ProcessRowView<'a> {
    meta: &'a TableRowMeta,
    show_net: bool,
    cpu_color: Color,
    mem_color: Color,
    inactive_fg: Color,
}

impl<'a> ProcessRowView<'a> {
    fn new(meta: &'a TableRowMeta, theme: &MonitorTheme, show_net: bool) -> Self {
        let p = &meta.info;
        let cpu_color = theme.cpu.sample(p.cpu_fraction.clamp(0.0, 1.0));
        let mem_color = theme.used.sample(
            (p.mem_rss_bytes as f64 / (32.0 * 1024.0 * 1024.0 * 1024.0)).min(1.0) as f32,
        );
        Self { meta, show_net, cpu_color, mem_color, inactive_fg: theme.inactive_fg }
    }
}

impl<'a> TableRowExt<ProcCol> for ProcessRowView<'a> {
    fn cell(&self, col: ProcCol) -> Cell {
        let p = &self.meta.info;
        match col {
            ProcCol::Pid => Cell::styled(p.pid.to_string(), self.inactive_fg),
            // Program is the "label column" — LiveTable overlays the
            // tree prefix here. Just supply the program name; the
            // widget handles the prefix overlay and width truncation.
            ProcCol::Program => Cell::plain(p.name.clone()),
            ProcCol::Command => {
                let cmd = if p.cmdline.is_empty() {
                    p.name.clone()
                } else {
                    p.cmdline.clone()
                };
                Cell::plain(cmd)
            }
            ProcCol::User => Cell::plain(p.user.clone()),
            ProcCol::Threads => Cell::plain(p.threads.to_string()),
            ProcCol::Mem => Cell::styled(format_bytes_compact(p.mem_rss_bytes), self.mem_color),
            ProcCol::Cpu => Cell::styled(format!("{:.1}", p.cpu_fraction * 100.0), self.cpu_color),
            ProcCol::NetRx => Cell::plain(opt_rate(if self.show_net { p.net_rx_bytes_per_sec } else { None })),
            ProcCol::NetTx => Cell::plain(opt_rate(if self.show_net { p.net_tx_bytes_per_sec } else { None })),
            ProcCol::DiskRead => Cell::plain(opt_rate(p.disk_read_bytes_per_sec)),
            ProcCol::DiskWrite => Cell::plain(opt_rate(p.disk_write_bytes_per_sec)),
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
}

/// View over a group header that emits aggregate cells for the metric
/// columns. The `Program` column is left blank — LiveTable overlays the
/// chevron + group label there.
struct GroupView<'a> {
    h: &'a TableGroupHeader,
    show_net: bool,
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
            ProcCol::Pid | ProcCol::Program | ProcCol::Command => Cell::plain(""),
            ProcCol::User => Cell::plain(self.h.dominant_user.clone()),
            ProcCol::Threads => Cell::plain(self.h.threads_total.to_string()),
            ProcCol::Mem => Cell::plain(format_bytes_compact(self.h.mem_rss_total)),
            ProcCol::Cpu => Cell::plain(format!("{:.1}", self.h.cpu_fraction_total * 100.0)),
            ProcCol::NetRx => Cell::plain(opt_rate(if self.show_net { self.h.net_rx_total } else { None })),
            ProcCol::NetTx => Cell::plain(opt_rate(if self.show_net { self.h.net_tx_total } else { None })),
            ProcCol::DiskRead => Cell::plain(opt_rate(self.h.disk_read_total)),
            ProcCol::DiskWrite => Cell::plain(opt_rate(self.h.disk_write_total)),
        }
    }
}

fn opt_rate(v: Option<f64>) -> String {
    match v {
        Some(r) if r > 0.5 => format_rate(r),
        Some(_) => "0".into(),
        None => "-".into(),
    }
}

#[cfg(test)]
mod tests {
    use std::time::Instant;

    use bobtop_core::sample::ProcessState;
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
}
