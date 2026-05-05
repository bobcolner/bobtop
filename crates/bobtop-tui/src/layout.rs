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
    /// Initial set of enabled boxes — seeded into `BoxesEnabled` at startup.
    /// After that, the user toggles individual boxes via `1`-`4` or `B`.
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

#[derive(Debug, Clone, Copy, Default)]
pub struct LayoutAreas {
    pub cpu: Option<Rect>,
    pub memory: Option<Rect>,
    pub disks: Option<Rect>,
    pub network: Option<Rect>,
    pub processes: Option<Rect>,
}

/// Compute panel rects from the currently enabled set of boxes.
/// Disabled boxes get `None` and their screen space is reabsorbed by
/// the remaining panels — that's the btop-style "toggle 1-4 to resize"
/// behavior. Empty enabled set returns an all-`None` LayoutAreas.
pub fn compute(area: Rect, enabled: &BoxesEnabled) -> LayoutAreas {
    let cpu_on = enabled.is_enabled(BoxKind::Cpu);
    let mem_on = enabled.is_enabled(BoxKind::Memory);
    let disk_on = enabled.is_enabled(BoxKind::Disk);
    let net_on = enabled.is_enabled(BoxKind::Network);
    let proc_on = enabled.is_enabled(BoxKind::Process);

    let mut out = LayoutAreas::default();
    if !(cpu_on || mem_on || disk_on || net_on || proc_on) {
        return out;
    }

    // Top row: CPU (full width). Takes 30% only when there's bottom
    // content to share with; otherwise CPU claims the whole frame.
    let bottom_region = if cpu_on && (mem_on || disk_on || net_on || proc_on) {
        let parts = Layout::default()
            .direction(Direction::Vertical)
            .constraints([Constraint::Percentage(30), Constraint::Min(1)])
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
    // 40/60 split mirrors btop. Either side collapses to full when the other
    // is empty.
    let left_has = mem_on || disk_on || net_on;
    let (left_region, proc_region) = match (left_has, proc_on) {
        (true, true) => {
            let parts = Layout::default()
                .direction(Direction::Horizontal)
                .constraints([Constraint::Percentage(40), Constraint::Percentage(60)])
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

    // Left stack: mem+disk row on top, net row below — each collapses
    // when the other is empty.
    let top_has = mem_on || disk_on;
    let (top_row, net_row) = match (top_has, net_on) {
        (true, true) => {
            let parts = Layout::default()
                .direction(Direction::Vertical)
                .constraints([Constraint::Percentage(50), Constraint::Percentage(50)])
                .split(left);
            (Some(parts[0]), Some(parts[1]))
        }
        (true, false) => (Some(left), None),
        (false, true) => (None, Some(left)),
        (false, false) => (None, None),
    };
    out.network = net_row;

    if let Some(top) = top_row {
        match (mem_on, disk_on) {
            (true, true) => {
                let parts = Layout::default()
                    .direction(Direction::Horizontal)
                    .constraints([Constraint::Percentage(50), Constraint::Percentage(50)])
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

    fn all_enabled() -> BoxesEnabled {
        BoxesEnabled::with(LayoutPreset::Full.enabled_boxes())
    }

    #[test]
    fn full_layout_areas_dont_overlap_and_sum_to_total() {
        let total = Rect::new(0, 0, 200, 60);
        let l = compute(total, &all_enabled());
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
        let l = compute(Rect::new(0, 0, 200, 60), &all_enabled());
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
        let en = BoxesEnabled::with(LayoutPreset::Minimal.enabled_boxes());
        let l = compute(Rect::new(0, 0, 100, 30), &en);
        assert!(l.memory.is_none());
        assert!(l.disks.is_none());
        assert!(l.network.is_none());
        let cpu = l.cpu.unwrap();
        let proc_ = l.processes.unwrap();
        assert_eq!(cpu.height + proc_.height, 30);
    }

    #[test]
    fn cpu_off_makes_bottom_region_full_height() {
        let mut en = all_enabled();
        en.set(BoxKind::Cpu, false);
        let total = Rect::new(0, 0, 200, 60);
        let l = compute(total, &en);
        assert!(l.cpu.is_none());
        // Memory + disks should now start at y=0 (no CPU row above).
        assert_eq!(l.memory.unwrap().y, 0);
    }

    #[test]
    fn process_off_makes_left_stack_full_width() {
        let mut en = all_enabled();
        en.set(BoxKind::Process, false);
        let total = Rect::new(0, 0, 200, 60);
        let l = compute(total, &en);
        assert!(l.processes.is_none());
        // Mem+Disk row should span full width (no process column to share with).
        assert_eq!(
            l.memory.unwrap().width + l.disks.unwrap().width,
            total.width
        );
    }

    #[test]
    fn only_process_enabled_fills_screen() {
        // BoxesEnabled::default() == all-on; use with(&[]) for an empty set.
        let en = BoxesEnabled::with(&[BoxKind::Process]);
        let total = Rect::new(0, 0, 200, 60);
        let l = compute(total, &en);
        let p = l.processes.unwrap();
        assert_eq!(p.width, total.width);
        assert_eq!(p.height, total.height);
        assert!(l.cpu.is_none());
    }

    #[test]
    fn nothing_enabled_returns_all_none() {
        let en = BoxesEnabled::with(&[]);
        let l = compute(Rect::new(0, 0, 200, 60), &en);
        assert!(l.cpu.is_none());
        assert!(l.memory.is_none());
        assert!(l.disks.is_none());
        assert!(l.network.is_none());
        assert!(l.processes.is_none());
    }

    #[test]
    fn small_areas_dont_panic() {
        let en = all_enabled();
        for (w, h) in [(1, 1), (10, 1), (1, 10), (40, 12), (200, 60)] {
            let _ = compute(Rect::new(0, 0, w, h), &en);
        }
    }
}
