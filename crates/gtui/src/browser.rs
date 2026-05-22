//! Two-pane tree + preview composition helper.
//!
//! `gfb` ships a tree-on-the-left + preview-on-the-right shape for
//! both filesystem and database browsing. The tree pane is always a
//! [`crate::widgets::LiveTable`] in tree-glyph mode wrapped in a
//! [`crate::widgets::BoxedPanel`]; the preview pane is domain-specific
//! (file content, DB rows, table rows).
//!
//! [`BrowserShell`] owns the shared chrome: it splits the area, paints
//! the panel, computes the cursor scroll, and renders the tree-side
//! `LiveTable`. The caller fills the preview rect that gets returned.
//!
//! Phase 3 of the gtop refactor (see `docs/gtop-refactor.md`).
//!
//! # Example
//!
//! ```no_run
//! # use gtui::browser::BrowserShell;
//! # use gtui::widgets::live_table::ColumnDef;
//! # use ratatui::layout::Rect;
//! # use ratatui::style::Color;
//! # fn demo<R, C>(
//! #     frame: &mut ratatui::Frame<'_>,
//! #     area: Rect,
//! #     rows: &[gtui::widgets::live_table::TableEntry<R, ()>],
//! #     columns: &[ColumnDef<C>],
//! #     theme: &gtui::Theme,
//! #     label_col: C,
//! # ) where
//! #     R: gtui::widgets::live_table::TableRowExt<C>,
//! #     C: Copy + PartialEq + Eq + std::fmt::Debug,
//! # {
//! let preview_rect = BrowserShell::<C>::new()
//!     .with_title("catalog")
//!     .with_accent(Color::Magenta)
//!     .with_focused(true)
//!     .render(frame, area, rows, columns, theme, label_col, /* cursor */ 0);
//! // Caller fills `preview_rect` with file/table content.
//! # }
//! ```

use ratatui::layout::{Constraint, Direction, Layout, Rect};
use ratatui::style::Color;
use ratatui::Frame;

use crate::util::middle_anchor_scroll;
use crate::widgets::live_table::{
    ColumnDef, LiveTable, TableEntry, TableRowExt,
};
use crate::widgets::{panel as boxed_panel, CornerStyle};
use crate::Theme;

/// Tree-side rendering helper. Builder; nothing fires until
/// [`BrowserShell::render`] runs.
#[derive(Debug, Clone)]
pub struct BrowserShell<C> {
    title: Option<String>,
    controls: Option<String>,
    keybinds: Option<String>,
    accent: Option<Color>,
    focused: bool,
    /// Tree pane percentage of the horizontal split. Default: 35.
    tree_percent: u16,
    corner_style: CornerStyle,
    /// Render the tree pane without a [`BoxedPanel`] frame — useful
    /// when the caller has already wrapped `area` in a panel and just
    /// wants the layout split + LiveTable wiring.
    no_panel: bool,
    /// Active sort column + direction, drawn as a header arrow on the
    /// matching column. None disables the sort indicator.
    sort_state: Option<(C, bool)>,
    /// Sticky-selection key — when set, the LiveTable picks the row
    /// whose [`TableRowExt::key`] returns this value (overriding the
    /// positional cursor). Lets selection follow a node across
    /// re-flattens / re-sorts.
    sticky_key: Option<u64>,
}

impl<C> Default for BrowserShell<C> {
    fn default() -> Self {
        Self {
            title: None,
            controls: None,
            keybinds: None,
            accent: None,
            focused: true,
            tree_percent: 35,
            corner_style: CornerStyle::default(),
            no_panel: false,
            sort_state: None,
            sticky_key: None,
        }
    }
}

impl<C: Copy + PartialEq + Eq + std::fmt::Debug> BrowserShell<C> {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn with_title(mut self, title: impl Into<String>) -> Self {
        self.title = Some(title.into());
        self
    }

    /// Right-side controls hint shown in the panel header.
    pub fn with_controls(mut self, controls: impl Into<String>) -> Self {
        self.controls = Some(controls.into());
        self
    }

    /// Single-line keybind hint shown along the panel's bottom border
    /// (forwards to [`crate::widgets::BoxedPanel::with_keybinds`]).
    pub fn with_keybinds(mut self, keybinds: impl Into<String>) -> Self {
        self.keybinds = Some(keybinds.into());
        self
    }

    /// Border color when focused. When unfocused the panel uses
    /// `theme.div_line`. If unset, the panel uses
    /// `theme.panel_accents[0]`.
    pub fn with_accent(mut self, accent: Color) -> Self {
        self.accent = Some(accent);
        self
    }

    pub fn with_focused(mut self, focused: bool) -> Self {
        self.focused = focused;
        self
    }

    /// Tree pane percentage (1-99) of the horizontal split. Default
    /// 35; 0 or 100+ values get clamped.
    pub fn with_tree_percent(mut self, pct: u16) -> Self {
        self.tree_percent = pct.clamp(1, 99);
        self
    }

    pub fn with_corner_style(mut self, style: CornerStyle) -> Self {
        self.corner_style = style;
        self
    }

    /// Skip rendering the tree-side panel. Caller is responsible for
    /// any framing.
    pub fn without_panel(mut self) -> Self {
        self.no_panel = true;
        self
    }

    /// Active sort column + direction (drawn as a header arrow on the
    /// matching column). Forwards to
    /// [`crate::widgets::LiveTable::with_sort`].
    pub fn with_sort_state(mut self, col: C, descending: bool) -> Self {
        self.sort_state = Some((col, descending));
        self
    }

    /// Sticky-selection key. Forwards to
    /// [`crate::widgets::LiveTable::with_sticky_key`]; lets the
    /// selection follow a node across re-flatten / re-sort. The key
    /// is whatever the row's [`TableRowExt::key`] returns — typically
    /// a hash of the underlying NodeId.
    pub fn with_sticky_key(mut self, key: Option<u64>) -> Self {
        self.sticky_key = key;
        self
    }

    /// Pure layout: split `area` into (tree, preview) using the
    /// configured ratio. Useful for callers that want to do their own
    /// rendering — same split [`BrowserShell::render`] would use.
    pub fn split(&self, area: Rect) -> (Rect, Rect) {
        let chunks = Layout::default()
            .direction(Direction::Horizontal)
            .constraints([
                Constraint::Percentage(self.tree_percent),
                Constraint::Min(0),
            ])
            .split(area);
        (chunks[0], chunks[1])
    }

    /// Render the tree pane and return the preview rect the caller
    /// should fill.
    ///
    /// `label_col` is the column whose cell carries the tree label
    /// (passed through to [`LiveTable::new`]). `cursor` is an index
    /// into `rows`; the shell computes a middle-anchored scroll
    /// offset and forwards both into the underlying [`LiveTable`].
    pub fn render<R>(
        &self,
        frame: &mut Frame<'_>,
        area: Rect,
        rows: &[TableEntry<R, ()>],
        columns: &[ColumnDef<C>],
        theme: &Theme,
        label_col: C,
        cursor: usize,
    ) -> Rect
    where
        R: TableRowExt<C>,
    {
        let (tree_rect, preview_rect) = self.split(area);

        if self.no_panel {
            self.render_table(frame, tree_rect, rows, columns, theme, label_col, cursor);
            return preview_rect;
        }

        let accent = self.accent.unwrap_or(theme.panel_accents[0]);
        let border = if self.focused {
            accent
        } else {
            theme.div_line
        };
        let mut panel = boxed_panel(border, theme.title, self.corner_style);
        if let Some(t) = &self.title {
            panel = panel.with_title(t.clone());
        }
        if let Some(c) = &self.controls {
            panel = panel.with_controls(c.clone());
        }
        if let Some(kb) = &self.keybinds {
            panel = panel.with_keybinds(kb.clone());
        }
        frame.render_widget(&panel, tree_rect);
        let inner = panel.inner(tree_rect);

        if inner.height >= 1 && inner.width >= 4 {
            self.render_table(frame, inner, rows, columns, theme, label_col, cursor);
        }

        preview_rect
    }

    fn render_table<R>(
        &self,
        frame: &mut Frame<'_>,
        inner: Rect,
        rows: &[TableEntry<R, ()>],
        columns: &[ColumnDef<C>],
        theme: &Theme,
        label_col: C,
        cursor: usize,
    ) where
        R: TableRowExt<C>,
    {
        // -1 reserves the header row that LiveTable always paints
        // along the top of `inner`.
        let body_h = inner.height.saturating_sub(1) as usize;
        let scroll = middle_anchor_scroll(cursor, rows.len(), body_h);
        let mut table = LiveTable::new(rows, columns, theme, label_col)
            .with_selection(Some(cursor), scroll)
            .with_tree_glyphs(true)
            .with_fade(false);
        if let Some((col, descending)) = self.sort_state {
            table = table.with_sort(Some(col), descending);
        }
        if self.sticky_key.is_some() {
            table = table.with_sticky_key(self.sticky_key);
        }
        frame.render_widget(&table, inner);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    #[allow(dead_code)]
    enum Col {
        Name,
    }

    #[test]
    fn split_honors_tree_percent() {
        let shell = BrowserShell::<Col>::new().with_tree_percent(40);
        let (tree, preview) = shell.split(Rect::new(0, 0, 100, 24));
        assert_eq!(tree.width + preview.width, 100);
        // 40% of 100 = 40; ratatui's percentage rounding should land
        // exactly on integer boundaries here.
        assert_eq!(tree.width, 40);
    }

    #[test]
    fn extreme_tree_percents_clamp_to_valid_range() {
        // Out-of-range values must not crash — clamp to [1, 99].
        let s0 = BrowserShell::<Col>::new().with_tree_percent(0);
        let (tree0, preview0) = s0.split(Rect::new(0, 0, 100, 24));
        assert!(tree0.width >= 1);
        assert!(preview0.width >= 1);

        let s100 = BrowserShell::<Col>::new().with_tree_percent(120);
        let (tree100, preview100) = s100.split(Rect::new(0, 0, 100, 24));
        assert!(tree100.width <= 99);
        assert!(preview100.width >= 1);
    }
}
