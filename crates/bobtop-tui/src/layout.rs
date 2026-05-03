//! Layout engine — slices a frame into named panels.
//!
//! Two presets are supported, matching the original spec:
//! - [`LayoutPreset::Full`] — CPU on top, Memory + Network side-by-side in
//!   the middle, Process table fills the rest.
//! - [`LayoutPreset::Minimal`] — CPU on top, Process table below.
//!
//! [`compute`] is pure and side-effect free; the daemon owns the state and
//! calls it once per render.

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
    pub network: Option<Rect>,
    pub processes: Rect,
}

/// Slice `area` into panel rects for the chosen preset.
///
/// Heights are tuned to match btop's defaults: CPU ≈ 30% of vertical space,
/// the middle row (when present) ≈ 35%, processes get the remainder.
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
        network: None,
        processes: parts[1],
    }
}

fn compute_full(area: Rect) -> LayoutAreas {
    let rows = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Percentage(30),
            Constraint::Percentage(35),
            Constraint::Min(1),
        ])
        .split(area);

    let mid = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([Constraint::Percentage(50), Constraint::Percentage(50)])
        .split(rows[1]);

    LayoutAreas {
        cpu: rows[0],
        memory: Some(mid[0]),
        network: Some(mid[1]),
        processes: rows[2],
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
        let net = l.network.unwrap();
        assert!(!rects_overlap(l.cpu, mem));
        assert!(!rects_overlap(l.cpu, net));
        assert!(!rects_overlap(l.cpu, l.processes));
        assert!(!rects_overlap(mem, net));
        assert!(!rects_overlap(mem, l.processes));
        assert!(!rects_overlap(net, l.processes));
        let total_area = rect_area(total);
        let panels_area = rect_area(l.cpu) + rect_area(mem) + rect_area(net) + rect_area(l.processes);
        assert_eq!(panels_area, total_area);
    }

    #[test]
    fn minimal_layout_has_no_middle_row() {
        let l = compute(Rect::new(0, 0, 100, 30), LayoutPreset::Minimal);
        assert!(l.memory.is_none());
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
