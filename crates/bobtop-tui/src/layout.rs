//! Layout engine — slices a frame into named panels.
//!
//! ## `LayoutPreset::Full` (matches btop's normal.png)
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
//! - CPU spans full width, 30 % of height.
//! - The remaining 70 % is split horizontally 50/50.
//! - The left half subdivides: top row = MEM | DISKS (50/50), bottom row = NET (full width).
//! - PROC fills the right half (full bottom-row height).
//!
//! ## `LayoutPreset::Minimal`
//!
//! Just CPU on top + PROC below — useful at narrow terminal widths.

use ratatui::layout::{Constraint, Direction, Layout, Rect};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LayoutPreset {
    Full,
    Minimal,
}

#[derive(Debug, Clone, Copy)]
pub struct LayoutAreas {
    pub cpu: Rect,
    pub memory: Option<Rect>,
    pub disks: Option<Rect>,
    pub network: Option<Rect>,
    pub processes: Rect,
}

pub fn compute(area: Rect, preset: LayoutPreset) -> LayoutAreas {
    match preset {
        LayoutPreset::Minimal => compute_minimal(area),
        LayoutPreset::Full => compute_full(area),
    }
}

fn compute_minimal(area: Rect) -> LayoutAreas {
    let parts = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Percentage(30), Constraint::Min(1)])
        .split(area);
    LayoutAreas {
        cpu: parts[0],
        memory: None,
        disks: None,
        network: None,
        processes: parts[1],
    }
}

fn compute_full(area: Rect) -> LayoutAreas {
    let rows = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Percentage(30), Constraint::Min(1)])
        .split(area);

    let bottom_cols = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([Constraint::Percentage(50), Constraint::Percentage(50)])
        .split(rows[1]);

    // Left column subdivides vertically into the mem/disks row + the net row.
    let left_rows = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Percentage(50), Constraint::Percentage(50)])
        .split(bottom_cols[0]);

    let mem_disks = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([Constraint::Percentage(50), Constraint::Percentage(50)])
        .split(left_rows[0]);

    LayoutAreas {
        cpu: rows[0],
        memory: Some(mem_disks[0]),
        disks: Some(mem_disks[1]),
        network: Some(left_rows[1]),
        processes: bottom_cols[1],
    }
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

    #[test]
    fn full_layout_areas_dont_overlap_and_sum_to_total() {
        let total = Rect::new(0, 0, 200, 60);
        let l = compute(total, LayoutPreset::Full);
        let mem = l.memory.unwrap();
        let disks = l.disks.unwrap();
        let net = l.network.unwrap();
        let panels = [l.cpu, mem, disks, net, l.processes];
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
        let l = compute(Rect::new(0, 0, 200, 60), LayoutPreset::Full);
        let mem = l.memory.unwrap();
        let disks = l.disks.unwrap();
        let net = l.network.unwrap();
        // mem and disks share the same y-row.
        assert_eq!(mem.y, disks.y);
        assert_eq!(mem.height, disks.height);
        assert!(disks.x > mem.x, "disks must be to the right of mem");
        // net sits below them.
        assert_eq!(net.y, mem.y + mem.height);
        // net spans full left column width.
        assert_eq!(net.x, mem.x);
        assert_eq!(net.width, mem.width + disks.width);
    }

    #[test]
    fn minimal_layout_has_no_middle_row() {
        let l = compute(Rect::new(0, 0, 100, 30), LayoutPreset::Minimal);
        assert!(l.memory.is_none());
        assert!(l.disks.is_none());
        assert!(l.network.is_none());
        assert!(l.cpu.height + l.processes.height == 30);
    }

    #[test]
    fn small_areas_dont_panic() {
        for (w, h) in [(1, 1), (10, 1), (1, 10), (40, 12), (200, 60)] {
            let _ = compute(Rect::new(0, 0, w, h), LayoutPreset::Full);
            let _ = compute(Rect::new(0, 0, w, h), LayoutPreset::Minimal);
        }
    }
}
