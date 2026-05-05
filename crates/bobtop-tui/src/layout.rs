//! Layout engine — slices a frame into named panel rects based on which
//! boxes are currently enabled.
//!
//! Layout shape (when all 5 boxes are enabled, btop's `normal.png`):
//!
//! ```text
//! ┌────────────────────────────────────────────────┐
//! │                       CPU                      │
//! ├────────────────┬────────────────┬──────────────┤
//! │      MEM       │     DISKS      │              │
//! │                │                │              │
//! ├────────────────┴────────────────┤    PROC      │
//! │                                 │              │
//! │              NET                │              │
//! │                                 │              │
//! └─────────────────────────────────┴──────────────┘
//! ```
//!
//! - CPU spans full width (30 % height) when there's bottom content;
//!   otherwise it claims the full screen.
//! - The bottom region splits 40/60 between the left stack (mem/disks/net)
//!   and PROC. Each side collapses to full width when the other is empty.
//! - The left stack splits 50/50 between the mem+disk row and the net row.
//!   Each row collapses similarly.
//!
//! Toggling any single panel off (via `1`/`2`/`3`/`4` btop-style toggles
//! or the `B` overlay) makes its space reabsorb into the neighbors —
//! that's why this fn takes `&BoxesEnabled` instead of a static enum.
//!
//! `LayoutPreset` lives here only as the *initial* enabled-set selector
//! used at startup (Full = all 5, Minimal = CPU + PROC). After init the
//! daemon's `BoxesEnabled` is the source of truth.

use bobtop_core::{Box as BoxKind, BoxesEnabled};
use ratatui::layout::{Constraint, Direction, Layout, Rect};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LayoutPreset {
    Full,
    Minimal,
}

impl LayoutPreset {
    /// Initial set of enabled boxes — seeded into `PanelSizes` at startup.
    /// After that, the user cycles individual boxes via `1`-`5` or `B`.
    pub fn enabled_boxes(self) -> &'static [BoxKind] {
        match self {
            LayoutPreset::Full => &[
                BoxKind::Cpu,
                BoxKind::Memory,
                BoxKind::Disk,
                BoxKind::Network,
                BoxKind::Process,
            ],
            LayoutPreset::Minimal => &[BoxKind::Cpu, BoxKind::Process],
        }
    }
}

/// Per-panel size variation. Pressing the panel's number key (`1`-`5`)
/// cycles through these states; `Off` collapses the panel and reabsorbs
/// its space into neighbors. `Default` is btop's normal layout; `Large`
/// gives the panel extra room (taller for CPU/Net, wider for Process).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum PanelSize {
    Off,
    #[default]
    Default,
    Large,
}

impl PanelSize {
    /// Cycle order on each keypress: Default → Large → Off → Default.
    /// Putting Off between Large and Default means a "double-tap" from
    /// Default goes large-then-off, and a third tap brings it back —
    /// the cycle takes 3 presses to return to where you started.
    pub fn next(self) -> Self {
        match self {
            PanelSize::Default => PanelSize::Large,
            PanelSize::Large => PanelSize::Off,
            PanelSize::Off => PanelSize::Default,
        }
    }

    pub fn is_on(self) -> bool {
        !matches!(self, PanelSize::Off)
    }

    /// Weight for proportional layout within a shared region. `Default`
    /// gets 2 share-units, `Large` gets 3. So a Large panel paired with
    /// a Default panel claims 3/5 of the row.
    fn weight(self) -> u16 {
        match self {
            PanelSize::Off => 0,
            PanelSize::Default => 2,
            PanelSize::Large => 3,
        }
    }
}

/// Per-box size selection. Drives both layout proportions (this struct)
/// and the BoxesEnabled bitmask the collectors read (derived via
/// [`PanelSizes::enabled`]).
#[derive(Debug, Clone, Copy)]
pub struct PanelSizes {
    pub cpu: PanelSize,
    pub memory: PanelSize,
    pub disk: PanelSize,
    pub network: PanelSize,
    pub process: PanelSize,
}

impl PanelSizes {
    pub fn from_preset(p: LayoutPreset) -> Self {
        let mut out = Self {
            cpu: PanelSize::Off,
            memory: PanelSize::Off,
            disk: PanelSize::Off,
            network: PanelSize::Off,
            process: PanelSize::Off,
        };
        for b in p.enabled_boxes() {
            out.set(*b, PanelSize::Default);
        }
        out
    }

    pub fn get(&self, b: BoxKind) -> PanelSize {
        match b {
            BoxKind::Cpu => self.cpu,
            BoxKind::Memory => self.memory,
            BoxKind::Disk => self.disk,
            BoxKind::Network => self.network,
            BoxKind::Process => self.process,
        }
    }

    pub fn set(&mut self, b: BoxKind, s: PanelSize) {
        match b {
            BoxKind::Cpu => self.cpu = s,
            BoxKind::Memory => self.memory = s,
            BoxKind::Disk => self.disk = s,
            BoxKind::Network => self.network = s,
            BoxKind::Process => self.process = s,
        }
    }

    pub fn cycle(&mut self, b: BoxKind) {
        let next = self.get(b).next();
        self.set(b, next);
    }

    pub fn enabled(&self, b: BoxKind) -> bool {
        self.get(b).is_on()
    }
}

impl Default for PanelSizes {
    fn default() -> Self {
        Self::from_preset(LayoutPreset::Full)
    }
}

#[derive(Debug, Clone, Copy, Default)]
pub struct LayoutAreas {
    pub cpu: Option<Rect>,
    pub memory: Option<Rect>,
    pub disks: Option<Rect>,
    pub network: Option<Rect>,
    pub processes: Option<Rect>,
}

/// Compute panel rects from the configured panel sizes.
///
/// Each panel's [`PanelSize`] drives both its presence and its share of
/// the available space:
/// - `Off` collapses the panel; its rect is `None` and its neighbors
///   reabsorb the freed space.
/// - `Default` gives the panel its conventional share (CPU = 30 % height,
///   process column = 60 % of the bottom width, etc.).
/// - `Large` claims a bigger share at neighbors' expense.
///
/// When two panels share a row/column with weights, the proportion is
/// `self.weight() / sum(weights)` — so a Default+Large pair becomes 40/60.
pub fn compute(area: Rect, sizes: &PanelSizes) -> LayoutAreas {
    let cpu_on = sizes.cpu.is_on();
    let mem_on = sizes.memory.is_on();
    let disk_on = sizes.disk.is_on();
    let net_on = sizes.network.is_on();
    let proc_on = sizes.process.is_on();

    let mut out = LayoutAreas::default();
    if !(cpu_on || mem_on || disk_on || net_on || proc_on) {
        return out;
    }

    // Top row: CPU (full width). Height percentage depends on cpu size
    // when bottom content shares the screen; without bottom content CPU
    // claims the whole frame regardless of size.
    let cpu_pct: u16 = match sizes.cpu {
        PanelSize::Off => 0,
        PanelSize::Default => 30,
        PanelSize::Large => 50,
    };
    let bottom_present = mem_on || disk_on || net_on || proc_on;
    let bottom_region = if cpu_on && bottom_present {
        let parts = Layout::default()
            .direction(Direction::Vertical)
            .constraints([Constraint::Percentage(cpu_pct), Constraint::Min(1)])
            .split(area);
        out.cpu = Some(parts[0]);
        Some(parts[1])
    } else if cpu_on {
        out.cpu = Some(area);
        None
    } else {
        Some(area)
    };

    let Some(bottom) = bottom_region else {
        return out;
    };

    // Bottom region: split into left stack (mem/disk/net) and process column.
    // Process width percentage depends on its size; default 60 % matches btop.
    let proc_pct: u16 = match sizes.process {
        PanelSize::Off => 0,
        PanelSize::Default => 60,
        PanelSize::Large => 75,
    };
    let left_has = mem_on || disk_on || net_on;
    let (left_region, proc_region) = match (left_has, proc_on) {
        (true, true) => {
            let parts = Layout::default()
                .direction(Direction::Horizontal)
                .constraints([
                    Constraint::Percentage(100 - proc_pct),
                    Constraint::Percentage(proc_pct),
                ])
                .split(bottom);
            (Some(parts[0]), Some(parts[1]))
        }
        (true, false) => (Some(bottom), None),
        (false, true) => (None, Some(bottom)),
        (false, false) => (None, None),
    };
    out.processes = proc_region;

    let Some(left) = left_region else {
        return out;
    };

    // Left stack: mem+disk row on top, net row below.
    // Net height percentage depends on its size; default 50 %.
    let net_pct: u16 = match sizes.network {
        PanelSize::Off => 0,
        PanelSize::Default => 50,
        PanelSize::Large => 70,
    };
    let top_has = mem_on || disk_on;
    let (top_row, net_row) = match (top_has, net_on) {
        (true, true) => {
            let parts = Layout::default()
                .direction(Direction::Vertical)
                .constraints([
                    Constraint::Percentage(100 - net_pct),
                    Constraint::Percentage(net_pct),
                ])
                .split(left);
            (Some(parts[0]), Some(parts[1]))
        }
        (true, false) => (Some(left), None),
        (false, true) => (None, Some(left)),
        (false, false) => (None, None),
    };
    out.network = net_row;

    if let Some(top) = top_row {
        // Mem vs Disk: weight-ratio split. Default+Default = 50/50;
        // Default+Large = 40/60; Large+Default = 60/40; Large+Large = 50/50.
        let mw = sizes.memory.weight();
        let dw = sizes.disk.weight();
        match (mem_on, disk_on) {
            (true, true) => {
                let total = (mw + dw).max(1);
                // Use Ratio for fractional accuracy; both numerator and
                // denominator come straight from weights.
                let parts = Layout::default()
                    .direction(Direction::Horizontal)
                    .constraints([
                        Constraint::Ratio(mw as u32, total as u32),
                        Constraint::Ratio(dw as u32, total as u32),
                    ])
                    .split(top);
                out.memory = Some(parts[0]);
                out.disks = Some(parts[1]);
            }
            (true, false) => out.memory = Some(top),
            (false, true) => out.disks = Some(top),
            (false, false) => {}
        }
    }

    out
}

/// Convenience: compute a layout from a [`BoxesEnabled`] bitfield (no
/// size variation — all enabled panels treated as `Default`). Used for
/// the daemon's existing `BoxesEnabled` source-of-truth path while
/// `PanelSizes` is being threaded through.
pub fn compute_from_enabled(area: Rect, enabled: &BoxesEnabled) -> LayoutAreas {
    let mut sizes = PanelSizes {
        cpu: PanelSize::Off,
        memory: PanelSize::Off,
        disk: PanelSize::Off,
        network: PanelSize::Off,
        process: PanelSize::Off,
    };
    for b in [
        BoxKind::Cpu,
        BoxKind::Memory,
        BoxKind::Disk,
        BoxKind::Network,
        BoxKind::Process,
    ] {
        if enabled.is_enabled(b) {
            sizes.set(b, PanelSize::Default);
        }
    }
    compute(area, &sizes)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rect_area(r: Rect) -> u32 {
        r.width as u32 * r.height as u32
    }

    fn rects_overlap(a: Rect, b: Rect) -> bool {
        let ax2 = a.x + a.width;
        let ay2 = a.y + a.height;
        let bx2 = b.x + b.width;
        let by2 = b.y + b.height;
        a.x < bx2 && b.x < ax2 && a.y < by2 && b.y < ay2
    }

    fn all_default() -> PanelSizes {
        PanelSizes::from_preset(LayoutPreset::Full)
    }

    #[test]
    fn full_layout_areas_dont_overlap_and_sum_to_total() {
        let total = Rect::new(0, 0, 200, 60);
        let l = compute(total, &all_default());
        let panels = [
            l.cpu.unwrap(),
            l.memory.unwrap(),
            l.disks.unwrap(),
            l.network.unwrap(),
            l.processes.unwrap(),
        ];
        for (i, a) in panels.iter().enumerate() {
            for (j, b) in panels.iter().enumerate() {
                if i != j {
                    assert!(!rects_overlap(*a, *b), "panels {i} and {j} overlap");
                }
            }
        }
        let total_area = rect_area(total);
        let panels_area: u32 = panels.iter().map(|r| rect_area(*r)).sum();
        assert_eq!(panels_area, total_area);
    }

    #[test]
    fn full_layout_disks_sits_right_of_mem_and_above_net() {
        let l = compute(Rect::new(0, 0, 200, 60), &all_default());
        let mem = l.memory.unwrap();
        let disks = l.disks.unwrap();
        let net = l.network.unwrap();
        assert_eq!(mem.y, disks.y);
        assert_eq!(mem.height, disks.height);
        assert!(disks.x > mem.x, "disks must be to the right of mem");
        assert_eq!(net.y, mem.y + mem.height);
        assert_eq!(net.x, mem.x);
        assert_eq!(net.width, mem.width + disks.width);
    }

    #[test]
    fn minimal_layout_has_no_middle_row() {
        let sizes = PanelSizes::from_preset(LayoutPreset::Minimal);
        let l = compute(Rect::new(0, 0, 100, 30), &sizes);
        assert!(l.memory.is_none());
        assert!(l.disks.is_none());
        assert!(l.network.is_none());
        let cpu = l.cpu.unwrap();
        let proc_ = l.processes.unwrap();
        assert_eq!(cpu.height + proc_.height, 30);
    }

    #[test]
    fn cpu_off_makes_bottom_region_full_height() {
        let mut sizes = all_default();
        sizes.set(BoxKind::Cpu, PanelSize::Off);
        let total = Rect::new(0, 0, 200, 60);
        let l = compute(total, &sizes);
        assert!(l.cpu.is_none());
        assert_eq!(l.memory.unwrap().y, 0);
    }

    #[test]
    fn process_off_makes_left_stack_full_width() {
        let mut sizes = all_default();
        sizes.set(BoxKind::Process, PanelSize::Off);
        let total = Rect::new(0, 0, 200, 60);
        let l = compute(total, &sizes);
        assert!(l.processes.is_none());
        assert_eq!(
            l.memory.unwrap().width + l.disks.unwrap().width,
            total.width
        );
    }

    #[test]
    fn only_process_enabled_fills_screen() {
        let mut sizes = PanelSizes::from_preset(LayoutPreset::Full);
        for b in [BoxKind::Cpu, BoxKind::Memory, BoxKind::Disk, BoxKind::Network] {
            sizes.set(b, PanelSize::Off);
        }
        let total = Rect::new(0, 0, 200, 60);
        let l = compute(total, &sizes);
        let p = l.processes.unwrap();
        assert_eq!(p.width, total.width);
        assert_eq!(p.height, total.height);
        assert!(l.cpu.is_none());
    }

    #[test]
    fn nothing_enabled_returns_all_none() {
        let sizes = PanelSizes {
            cpu: PanelSize::Off,
            memory: PanelSize::Off,
            disk: PanelSize::Off,
            network: PanelSize::Off,
            process: PanelSize::Off,
        };
        let l = compute(Rect::new(0, 0, 200, 60), &sizes);
        assert!(l.cpu.is_none());
        assert!(l.memory.is_none());
        assert!(l.disks.is_none());
        assert!(l.network.is_none());
        assert!(l.processes.is_none());
    }

    #[test]
    fn small_areas_dont_panic() {
        let sizes = all_default();
        for (w, h) in [(1, 1), (10, 1), (1, 10), (40, 12), (200, 60)] {
            let _ = compute(Rect::new(0, 0, w, h), &sizes);
        }
    }

    #[test]
    fn cpu_large_takes_more_vertical_space() {
        let total = Rect::new(0, 0, 200, 60);
        let l_def = compute(total, &all_default());
        let mut sizes = all_default();
        sizes.set(BoxKind::Cpu, PanelSize::Large);
        let l_large = compute(total, &sizes);
        assert!(
            l_large.cpu.unwrap().height > l_def.cpu.unwrap().height,
            "Large CPU should be taller than Default"
        );
    }

    #[test]
    fn process_large_takes_more_horizontal_space() {
        let total = Rect::new(0, 0, 200, 60);
        let l_def = compute(total, &all_default());
        let mut sizes = all_default();
        sizes.set(BoxKind::Process, PanelSize::Large);
        let l_large = compute(total, &sizes);
        assert!(
            l_large.processes.unwrap().width > l_def.processes.unwrap().width,
            "Large Process should be wider than Default"
        );
    }

    #[test]
    fn mem_large_with_disk_default_takes_60_percent_of_row() {
        let total = Rect::new(0, 0, 200, 60);
        let mut sizes = all_default();
        sizes.set(BoxKind::Memory, PanelSize::Large);
        let l = compute(total, &sizes);
        let mem = l.memory.unwrap();
        let disks = l.disks.unwrap();
        // Weights: mem=3, disks=2 → mem gets 3/5 of row width.
        assert!(mem.width > disks.width, "Large mem should be wider than default disks");
        // 60 % of left-stack width — left stack is 40 % of total = 80,
        // so mem ≈ 48, disks ≈ 32. Some rounding tolerance.
        let row_width = mem.width + disks.width;
        assert!(
            (mem.width as i32 - (row_width * 3 / 5) as i32).abs() <= 1,
            "mem={} disks={} row={}",
            mem.width,
            disks.width,
            row_width
        );
    }

    #[test]
    fn panel_size_cycle_returns_to_start_after_three_presses() {
        let s = PanelSize::Default;
        assert_eq!(s.next(), PanelSize::Large);
        assert_eq!(s.next().next(), PanelSize::Off);
        assert_eq!(s.next().next().next(), PanelSize::Default);
    }
}
