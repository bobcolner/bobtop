//! N-pane horizontal split for Miller-column file browsers (yazi/ranger
//! style: parent | current | preview).
//!
//! Pure layout — does not draw. Callers feed each returned `Rect` to
//! whichever widget they want (typically `BoxedPanel` chrome wrapping a
//! `Table` or `ScrollableText` body). Keeping this as a layout helper
//! rather than a render-owning widget mirrors `gtui::layout`, which
//! also returns named rects.
//!
//! Width allocation is weight-based with `min_width` honored when there
//! is room. If total min widths exceed the area, low-priority columns
//! collapse to zero-width rects (the caller can detect this with
//! `Rect::width == 0` and skip them).

use ratatui::layout::Rect;

#[derive(Debug, Clone, Copy)]
pub struct MillerColumn {
    /// Relative share of the leftover (post-min) space.
    pub weight: u16,
    /// Minimum width; if the area is too narrow, low-weight columns are
    /// collapsed first and high-weight columns absorb the rest.
    pub min_width: u16,
}

impl MillerColumn {
    pub fn new(weight: u16, min_width: u16) -> Self {
        Self { weight, min_width }
    }
}

#[derive(Debug, Clone)]
pub struct MillerColumns {
    pub columns: Vec<MillerColumn>,
    /// Cells of empty space inserted between adjacent columns.
    pub gap: u16,
}

impl MillerColumns {
    pub fn new(columns: Vec<MillerColumn>) -> Self {
        Self { columns, gap: 0 }
    }

    pub fn with_gap(mut self, gap: u16) -> Self {
        self.gap = gap;
        self
    }

    /// Split `area` into one rect per column.
    ///
    /// Algorithm:
    /// 1. Reserve gap cells between columns.
    /// 2. Try to honor every column's `min_width`. If total mins exceed
    ///    available, drop columns from the right (lowest weight first,
    ///    ties broken by index) until they fit.
    /// 3. Distribute leftover width across surviving columns by weight.
    pub fn split(&self, area: Rect) -> Vec<Rect> {
        let n = self.columns.len();
        if n == 0 || area.width == 0 {
            return vec![Rect::new(area.x, area.y, 0, area.height); n];
        }
        let gaps_total = self.gap.saturating_mul(n.saturating_sub(1) as u16);
        let mut budget = area.width.saturating_sub(gaps_total);

        // Decide which columns survive. Start with all on; if the sum of
        // mins is too big, drop the lowest-priority column and retry.
        let mut alive: Vec<bool> = vec![true; n];
        loop {
            let min_sum: u32 = self
                .columns
                .iter()
                .zip(&alive)
                .filter(|(_, on)| **on)
                .map(|(c, _)| c.min_width as u32)
                .sum();
            if min_sum <= budget as u32 {
                break;
            }
            // Drop the alive column with the lowest weight; ties broken
            // by index (rightmost first — preview drops before parent).
            let victim = alive
                .iter()
                .enumerate()
                .filter(|(_, on)| **on)
                .min_by(|(ai, _), (bi, _)| {
                    self.columns[*ai]
                        .weight
                        .cmp(&self.columns[*bi].weight)
                        .then(bi.cmp(ai))
                })
                .map(|(i, _)| i);
            match victim {
                Some(i) => alive[i] = false,
                None => break,
            }
        }
        // Reclaim gaps for dropped columns.
        let live_count = alive.iter().filter(|on| **on).count();
        let live_gaps = self.gap.saturating_mul(live_count.saturating_sub(1) as u16);
        budget = area.width.saturating_sub(live_gaps);

        // Allocate widths: each live column gets at least min_width; the
        // remainder is distributed by weight (largest-remainder method).
        let mut widths = vec![0u16; n];
        let min_total: u16 = self
            .columns
            .iter()
            .zip(&alive)
            .filter(|(_, on)| **on)
            .map(|(c, _)| c.min_width)
            .sum();
        let leftover = budget.saturating_sub(min_total);
        let weight_sum: u32 = self
            .columns
            .iter()
            .zip(&alive)
            .filter(|(_, on)| **on)
            .map(|(c, _)| c.weight as u32)
            .sum();
        // Floor-allocate by weight, track fractional remainders for the
        // largest-remainder rounding pass.
        let mut remainders: Vec<(usize, u32)> = Vec::with_capacity(n);
        for (i, (col, on)) in self.columns.iter().zip(&alive).enumerate() {
            if !*on {
                continue;
            }
            widths[i] = col.min_width;
            if weight_sum == 0 || leftover == 0 {
                continue;
            }
            let scaled = leftover as u32 * col.weight as u32;
            let floor = (scaled / weight_sum) as u16;
            widths[i] = widths[i].saturating_add(floor);
            remainders.push((i, scaled % weight_sum));
        }
        let allocated: u16 = remainders
            .iter()
            .map(|(i, _)| widths[*i].saturating_sub(self.columns[*i].min_width))
            .sum();
        let mut slack = leftover.saturating_sub(allocated);
        // Distribute slack one cell at a time to the largest fractional
        // remainder, breaking ties by lowest index.
        remainders.sort_by(|a, b| b.1.cmp(&a.1).then(a.0.cmp(&b.0)));
        for (i, _) in remainders {
            if slack == 0 {
                break;
            }
            widths[i] = widths[i].saturating_add(1);
            slack -= 1;
        }

        let mut rects = Vec::with_capacity(n);
        let mut x = area.x;
        let mut emitted = 0usize;
        for (i, w) in widths.iter().enumerate() {
            if !alive[i] || *w == 0 {
                rects.push(Rect::new(area.x, area.y, 0, area.height));
                continue;
            }
            if emitted > 0 {
                x = x.saturating_add(self.gap);
            }
            rects.push(Rect::new(x, area.y, *w, area.height));
            x = x.saturating_add(*w);
            emitted += 1;
        }
        rects
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn three_pane_split_honors_weights() {
        // 1:2:3 weights, no gaps, width 60 → 10/20/30 (no min).
        let m = MillerColumns::new(vec![
            MillerColumn::new(1, 0),
            MillerColumn::new(2, 0),
            MillerColumn::new(3, 0),
        ]);
        let rects = m.split(Rect::new(0, 0, 60, 10));
        assert_eq!(rects.len(), 3);
        assert_eq!(rects[0].width, 10);
        assert_eq!(rects[1].width, 20);
        assert_eq!(rects[2].width, 30);
        // Adjacent.
        assert_eq!(rects[0].x + rects[0].width, rects[1].x);
        assert_eq!(rects[1].x + rects[1].width, rects[2].x);
    }

    #[test]
    fn gap_is_subtracted_before_distribution() {
        // Two equal-weight panes with gap=2 in width 10 → each pane = 4.
        let m = MillerColumns::new(vec![
            MillerColumn::new(1, 0),
            MillerColumn::new(1, 0),
        ])
        .with_gap(2);
        let rects = m.split(Rect::new(0, 0, 10, 5));
        assert_eq!(rects[0].width, 4);
        assert_eq!(rects[1].width, 4);
        assert_eq!(rects[0].x + rects[0].width + 2, rects[1].x);
    }

    #[test]
    fn narrow_area_drops_lowest_weight_column() {
        // 3 panes with min widths 6/6/6 in width 14 → must drop one.
        // Weights: 1/3/2 → drop weight=1 (index 0).
        let m = MillerColumns::new(vec![
            MillerColumn::new(1, 6),
            MillerColumn::new(3, 6),
            MillerColumn::new(2, 6),
        ]);
        let rects = m.split(Rect::new(0, 0, 14, 5));
        assert_eq!(rects[0].width, 0, "low-weight col 0 should collapse");
        assert!(rects[1].width >= 6);
        assert!(rects[2].width >= 6);
        assert_eq!(rects[1].width + rects[2].width, 14);
    }

    #[test]
    fn empty_input_returns_empty() {
        let m = MillerColumns::new(vec![]);
        assert!(m.split(Rect::new(0, 0, 100, 10)).is_empty());
    }

    #[test]
    fn zero_width_area_returns_zero_rects() {
        let m = MillerColumns::new(vec![MillerColumn::new(1, 0), MillerColumn::new(1, 0)]);
        let rects = m.split(Rect::new(0, 0, 0, 10));
        assert_eq!(rects.len(), 2);
        for r in rects {
            assert_eq!(r.width, 0);
        }
    }

    #[test]
    fn slack_distribution_is_deterministic() {
        // Width 10, three equal-weight panes → 3 + 3 + 3 with 1 slack cell.
        // Largest-remainder ties break by lowest index → col 0 gets +1.
        let m = MillerColumns::new(vec![
            MillerColumn::new(1, 0),
            MillerColumn::new(1, 0),
            MillerColumn::new(1, 0),
        ]);
        let rects = m.split(Rect::new(0, 0, 10, 5));
        assert_eq!(rects[0].width, 4);
        assert_eq!(rects[1].width, 3);
        assert_eq!(rects[2].width, 3);
    }
}
