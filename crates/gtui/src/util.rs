//! Generic state helpers — cursor / scroll math every list-with-cursor
//! TUI app reinvents. Pure logic, no rendering.

/// Anchor `selected` roughly in the middle of a viewport of height
/// `viewport_h` over `total` rows. Clamped at the top (never returns a
/// negative offset) and at the bottom (never scrolls past the last row
/// that fits). Returns `0` for empty lists or zero-height viewports.
///
/// Use this for sortable tables, log viewers, query results — anywhere
/// you want the cursor to stay near the middle of the screen as it
/// moves so the user always sees context above and below.
pub fn middle_anchor_scroll(selected: usize, total: usize, viewport_h: usize) -> usize {
    if viewport_h == 0 || total == 0 {
        return 0;
    }
    let max_scroll = total.saturating_sub(viewport_h);
    let anchor = viewport_h / 2;
    selected.saturating_sub(anchor).min(max_scroll)
}

/// Cursor + scroll-offset state for a list / table viewport.
///
/// Index-based and rendering-agnostic: callers map `cursor` back into
/// whatever vector they're displaying. `scroll` is the index of the
/// first visible row.
#[derive(Debug, Clone, Default)]
pub struct Nav {
    pub cursor: usize,
    pub scroll: usize,
}

impl Nav {
    /// Move the cursor by `delta` rows (negative = up). Clamps to
    /// `[0, len-1]`. No-op when `len == 0`.
    pub fn move_by(&mut self, delta: isize, len: usize) {
        if len == 0 {
            self.cursor = 0;
            self.scroll = 0;
            return;
        }
        let last = len - 1;
        self.cursor = ((self.cursor as isize + delta).clamp(0, last as isize)) as usize;
    }

    /// Jump straight to `idx`, clamped to `[0, len-1]`. Doesn't touch
    /// scroll — call [`Nav::ensure_visible`] after if needed.
    pub fn jump_to(&mut self, idx: usize, len: usize) {
        if len == 0 {
            self.cursor = 0;
            return;
        }
        self.cursor = idx.min(len - 1);
    }

    pub fn home(&mut self) {
        self.cursor = 0;
    }

    pub fn end(&mut self, len: usize) {
        self.cursor = len.saturating_sub(1);
    }

    /// Adjust `scroll` so the cursor stays inside a viewport of
    /// `viewport_h` rows. Edge-anchored (cursor at top → scroll up;
    /// cursor at bottom → scroll down by exactly enough). Use
    /// [`middle_anchor_scroll`] instead when you want the cursor to
    /// stay near the middle of the viewport.
    pub fn ensure_visible(&mut self, viewport_h: usize) {
        if viewport_h == 0 {
            self.scroll = 0;
            return;
        }
        if self.cursor < self.scroll {
            self.scroll = self.cursor;
        } else if self.cursor >= self.scroll + viewport_h {
            self.scroll = self.cursor + 1 - viewport_h;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn middle_anchor_keeps_one_row_buffer_at_bottom() {
        assert_eq!(middle_anchor_scroll(9, 10, 5), 5);
        assert_eq!(middle_anchor_scroll(8, 10, 5), 5);
        assert_eq!(middle_anchor_scroll(3, 10, 5), 1);
    }

    #[test]
    fn middle_anchor_does_not_over_scroll_in_tiny_viewports() {
        assert_eq!(middle_anchor_scroll(4, 10, 1), 4);
        assert_eq!(middle_anchor_scroll(4, 10, 2), 3);
    }

    #[test]
    fn middle_anchor_handles_empty_inputs() {
        assert_eq!(middle_anchor_scroll(0, 0, 5), 0);
        assert_eq!(middle_anchor_scroll(5, 10, 0), 0);
    }

    #[test]
    fn nav_move_by_clamps_to_bounds() {
        let mut n = Nav::default();
        n.move_by(-5, 10);
        assert_eq!(n.cursor, 0);
        n.move_by(100, 10);
        assert_eq!(n.cursor, 9);
    }

    #[test]
    fn nav_ensure_visible_scrolls_down_when_cursor_off_bottom() {
        let mut n = Nav { cursor: 12, scroll: 0 };
        n.ensure_visible(5);
        assert_eq!(n.scroll, 8);
    }

    #[test]
    fn nav_ensure_visible_scrolls_up_when_cursor_above() {
        let mut n = Nav { cursor: 2, scroll: 10 };
        n.ensure_visible(5);
        assert_eq!(n.scroll, 2);
    }
}
