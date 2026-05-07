//! Cursor / scroll state for the directory list.
//!
//! Keeping the cursor logic out of `App` lets us unit-test movement
//! without spinning up a terminal. `Nav` is index-based; the app maps
//! the index back into its `Vec<FsEntry>` to pull the highlighted entry.

#[derive(Debug, Clone, Default)]
pub struct Nav {
    pub cursor: usize,
    pub scroll: usize,
}

impl Nav {
    pub fn move_by(&mut self, delta: isize, len: usize) {
        if len == 0 {
            self.cursor = 0;
            self.scroll = 0;
            return;
        }
        let last = len - 1;
        self.cursor = ((self.cursor as isize + delta).clamp(0, last as isize)) as usize;
    }

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

    /// Recompute `scroll` so the cursor stays inside a viewport of
    /// `viewport_h` rows. Mirrors how `DataTable` / `Table` callers
    /// handle scroll-on-cursor-move.
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
    fn move_by_clamps_to_bounds() {
        let mut n = Nav::default();
        n.move_by(-5, 10);
        assert_eq!(n.cursor, 0);
        n.move_by(100, 10);
        assert_eq!(n.cursor, 9);
    }

    #[test]
    fn ensure_visible_scrolls_down_when_cursor_off_bottom() {
        let mut n = Nav { cursor: 12, scroll: 0 };
        n.ensure_visible(5);
        assert_eq!(n.scroll, 8);
    }

    #[test]
    fn ensure_visible_scrolls_up_when_cursor_above() {
        let mut n = Nav { cursor: 2, scroll: 10 };
        n.ensure_visible(5);
        assert_eq!(n.scroll, 2);
    }
}
