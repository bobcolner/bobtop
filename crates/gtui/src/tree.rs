//! Tree-walk primitives for two-pane browsers.
//!
//! Defines a small [`Catalog`] trait that lets both filesystem-style and
//! database-style hierarchical sources plug in. The mechanical bits of a
//! tree render — depth, ancestor-continues bitmask, last-sibling marker
//! — are computed once here, in [`flatten`], so each app only has to
//! describe its domain (what's a node, what are its children, can it
//! expand).
//!
//! `gfb`'s filesystem tree-mode and database tree pane share this
//! same flatten machinery — one [`flatten`] walker, two [`Catalog`]
//! impls plugged into one [`MultiRootCatalog`](
//! https://docs.rs/gfb/latest/gfb/sources/struct.MultiRootCatalog.html).
//! See `docs/gtop-refactor.md` §"Phase 2 · `tree` module in `gtui`".
//!
//! # Quick example
//!
//! ```
//! use std::collections::HashSet;
//! use gtui::tree::{flatten, Catalog};
//!
//! struct Folders;
//! impl Catalog for Folders {
//!     type NodeId = &'static str;
//!     type Row = &'static str;
//!     fn roots(&self) -> Vec<(Self::NodeId, Self::Row)> { vec![("/", "/")] }
//!     fn children(&self, id: &Self::NodeId) -> Vec<(Self::NodeId, Self::Row)> {
//!         match *id {
//!             "/" => vec![("/etc", "etc"), ("/home", "home")],
//!             _ => vec![],
//!         }
//!     }
//!     fn is_expandable(&self, _: &Self::NodeId) -> bool { true }
//! }
//!
//! let mut expanded = HashSet::new();
//! expanded.insert("/");
//! let rows = flatten(&Folders, &expanded);
//! assert_eq!(rows.len(), 3);
//! assert_eq!(rows[1].depth, 1);
//! ```

use std::collections::HashSet;
use std::hash::Hash;

use crate::util::Nav;

/// A hierarchical data source the [`flatten`] walker can render.
///
/// Implementations describe their domain — the trait deliberately
/// owns no rendering or sorting policy. Sort children before
/// returning them; filter them too if your app supports it. The
/// walker treats whatever order [`children`](Catalog::children)
/// returns as the display order.
pub trait Catalog {
    /// Stable identity for an expanded-set membership check and for
    /// sticky selection across re-flattens.
    type NodeId: Clone + Eq + Hash;

    /// The per-node payload the caller renders (or wraps for
    /// rendering). The walker is opaque to its shape.
    type Row;

    /// All depth-0 entries. Single-rooted trees (a filesystem cwd)
    /// return one item; multi-rooted trees (a list of DB connections)
    /// return one per root.
    fn roots(&self) -> Vec<(Self::NodeId, Self::Row)>;

    /// In-order children of `node`. Called only for nodes the
    /// caller has marked expanded, so lazy I/O backends can fetch on
    /// demand without prefetching the whole tree. Errors should be
    /// absorbed inside the impl (log + return empty) — the walker
    /// has no side channel for them.
    fn children(&self, node: &Self::NodeId) -> Vec<(Self::NodeId, Self::Row)>;

    /// Whether `node` could have children (controls chevron / expand
    /// glyph). Leaf rows (a regular file, a DB table) return `false`
    /// even though they have no children, so the renderer can show
    /// them differently.
    fn is_expandable(&self, node: &Self::NodeId) -> bool;
}

/// One flattened row from [`flatten`]. Carries the `NodeId` (so
/// callers can route clicks / lookups), the domain `Row`, and the
/// tree-render metadata `LiveTable`'s tree mode needs.
#[derive(Debug, Clone)]
pub struct TreeRow<I, R> {
    pub id: I,
    pub row: R,
    pub depth: u8,
    /// Per-ancestor-depth, "should the vertical line continue past
    /// this row?" bit. `len() == depth`.
    pub ancestor_continues: Vec<bool>,
    /// True for the last visible child of its parent — drives
    /// `└` vs `├` glyph choice.
    pub is_last_sibling: bool,
    /// The catalog's [`Catalog::is_expandable`] verdict, propagated
    /// through so renderers can show the right glyph.
    pub expandable: bool,
    /// Whether the node was in the `expanded` set passed to
    /// [`flatten`].
    pub expanded: bool,
}

/// Bundle of per-tree mutable state: which nodes are expanded, plus a
/// [`Nav`] cursor over the flattened row list. Apps that want to
/// implement custom navigation can manage these fields directly.
#[derive(Debug, Clone)]
pub struct TreeState<I: Eq + Hash> {
    pub expanded: HashSet<I>,
    pub nav: Nav,
}

// Manual `Default` so callers don't need `I: Default`. Useful when
// the NodeId is an enum (no obvious zero value) — gfb's
// multi-source tree hits this.
impl<I: Eq + Hash> Default for TreeState<I> {
    fn default() -> Self {
        Self {
            expanded: HashSet::new(),
            nav: Nav::default(),
        }
    }
}

/// Outcome of [`TreeState::toggle_at`]. Lets callers distinguish a
/// fresh expand (where they may want to step the cursor into the new
/// children) from a collapse or a no-op.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ToggleOutcome<I> {
    /// The node was added to the expanded set.
    Expanded(I),
    /// The node was removed from the expanded set.
    Collapsed(I),
    /// The row at the cursor reports `expandable = false` (a leaf).
    NotExpandable,
    /// The cursor was past the end of `rows`.
    OutOfRange,
}

/// Outcome of [`TreeState::collapse_or_parent`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ParentOutcome<I> {
    /// Cursor row was expanded; we collapsed it and left the cursor
    /// where it was.
    Collapsed(I),
    /// Cursor row was collapsed (or a leaf); we moved the cursor up
    /// to the row's parent. Returns the new cursor index.
    JumpedToParent(usize),
    /// Cursor was on a depth-0 row that's not expanded — the caller
    /// gets to decide what "go up" means (cd up, focus elsewhere,
    /// nothing).
    AtRoot,
    /// The cursor was past the end of `rows`.
    OutOfRange,
}

impl<I: Eq + Hash + Clone> TreeState<I> {
    /// Toggle `id` in the expanded set. Returns the new state (`true`
    /// = now expanded). Use [`toggle_at`](Self::toggle_at) when you
    /// have a row list and want richer outcome reporting.
    pub fn toggle(&mut self, id: &I) -> bool {
        if self.expanded.contains(id) {
            self.expanded.remove(id);
            false
        } else {
            self.expanded.insert(id.clone());
            true
        }
    }

    pub fn is_expanded(&self, id: &I) -> bool {
        self.expanded.contains(id)
    }

    /// Toggle expansion at `idx`. Skips leaf rows (`expandable =
    /// false`). The expanded set is mutated in place; callers
    /// re-flatten and inspect the returned outcome to decide whether
    /// to step the cursor into the new children.
    pub fn toggle_at<R>(&mut self, rows: &[TreeRow<I, R>], idx: usize) -> ToggleOutcome<I> {
        let Some(row) = rows.get(idx) else {
            return ToggleOutcome::OutOfRange;
        };
        if !row.expandable {
            return ToggleOutcome::NotExpandable;
        }
        if self.expanded.contains(&row.id) {
            self.expanded.remove(&row.id);
            ToggleOutcome::Collapsed(row.id.clone())
        } else {
            self.expanded.insert(row.id.clone());
            ToggleOutcome::Expanded(row.id.clone())
        }
    }

    /// Common "left-arrow / h" behaviour for tree views: if the row
    /// at the cursor is expanded, collapse it; otherwise jump the
    /// cursor to the row's parent (the first row above with strictly
    /// lower depth). Returns [`ParentOutcome::AtRoot`] when neither
    /// applies — the caller decides what "go up" means at depth 0.
    pub fn collapse_or_parent<R>(&mut self, rows: &[TreeRow<I, R>]) -> ParentOutcome<I> {
        let cursor = self.nav.cursor;
        let Some(row) = rows.get(cursor) else {
            return ParentOutcome::OutOfRange;
        };
        if row.expandable && row.expanded {
            self.expanded.remove(&row.id);
            return ParentOutcome::Collapsed(row.id.clone());
        }
        if row.depth == 0 {
            return ParentOutcome::AtRoot;
        }
        let target = row.depth - 1;
        for i in (0..cursor).rev() {
            if rows[i].depth == target {
                self.nav.cursor = i;
                return ParentOutcome::JumpedToParent(i);
            }
        }
        ParentOutcome::AtRoot
    }

    /// Locate `id` in `rows` and update [`Nav::cursor`] to its
    /// index. Returns the new index, or `None` when the id isn't in
    /// `rows`. Callers use this to keep selection sticky across a
    /// re-flatten — pop the cursor's id before the change, call
    /// `select_id` with it after.
    pub fn select_id<R>(&mut self, rows: &[TreeRow<I, R>], id: &I) -> Option<usize> {
        let pos = rows.iter().position(|r| &r.id == id)?;
        self.nav.cursor = pos;
        Some(pos)
    }
}

/// Walk `catalog`'s roots, descending into any node whose id is in
/// `expanded`, and emit the visible rows in display order. Tree
/// metadata (`depth`, `ancestor_continues`, `is_last_sibling`) is
/// computed as we go.
pub fn flatten<C: Catalog>(
    catalog: &C,
    expanded: &HashSet<C::NodeId>,
) -> Vec<TreeRow<C::NodeId, C::Row>> {
    let mut out = Vec::new();
    let roots = catalog.roots();
    let total = roots.len();
    for (i, (id, row)) in roots.into_iter().enumerate() {
        let is_last = i + 1 == total;
        push_subtree(catalog, expanded, id, row, 0, &[], is_last, &mut out);
    }
    out
}

fn push_subtree<C: Catalog>(
    catalog: &C,
    expanded: &HashSet<C::NodeId>,
    id: C::NodeId,
    row: C::Row,
    depth: u8,
    ancestor_continues: &[bool],
    is_last_sibling: bool,
    out: &mut Vec<TreeRow<C::NodeId, C::Row>>,
) {
    let expandable = catalog.is_expandable(&id);
    let is_expanded = expanded.contains(&id);
    let recurse = expandable && is_expanded;

    out.push(TreeRow {
        id: id.clone(),
        row,
        depth,
        ancestor_continues: ancestor_continues.to_vec(),
        is_last_sibling,
        expandable,
        expanded: is_expanded,
    });

    if !recurse {
        return;
    }

    let children = catalog.children(&id);
    let total = children.len();
    if total == 0 {
        return;
    }

    let mut next_ancestors = ancestor_continues.to_vec();
    next_ancestors.push(!is_last_sibling);

    for (i, (cid, crow)) in children.into_iter().enumerate() {
        let last = i + 1 == total;
        push_subtree(
            catalog,
            expanded,
            cid,
            crow,
            depth + 1,
            &next_ancestors,
            last,
            out,
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Synthetic two-level catalog: depth-0 endpoints `a`, `b`; each
    /// has children `a/0`, `a/1`, etc. Leaves at depth 2.
    struct Demo;

    impl Catalog for Demo {
        type NodeId = String;
        type Row = String;

        fn roots(&self) -> Vec<(Self::NodeId, Self::Row)> {
            vec![
                ("a".into(), "a".into()),
                ("b".into(), "b".into()),
            ]
        }

        fn children(&self, node: &Self::NodeId) -> Vec<(Self::NodeId, Self::Row)> {
            // Two levels deep.
            if node.contains('/') {
                return vec![];
            }
            (0..2)
                .map(|i| {
                    let id = format!("{node}/{i}");
                    let label = id.clone();
                    (id, label)
                })
                .collect()
        }

        fn is_expandable(&self, node: &Self::NodeId) -> bool {
            !node.contains('/')
        }
    }

    #[test]
    fn nothing_expanded_yields_only_roots() {
        let rows = flatten(&Demo, &HashSet::new());
        let labels: Vec<&str> = rows.iter().map(|r| r.row.as_str()).collect();
        assert_eq!(labels, vec!["a", "b"]);
        assert!(rows.iter().all(|r| r.depth == 0));
    }

    #[test]
    fn expanding_root_inserts_children_at_depth_1() {
        let mut expanded = HashSet::new();
        expanded.insert("a".to_string());
        let rows = flatten(&Demo, &expanded);
        let labels: Vec<&str> = rows.iter().map(|r| r.row.as_str()).collect();
        assert_eq!(labels, vec!["a", "a/0", "a/1", "b"]);
        assert_eq!(rows[1].depth, 1);
        assert_eq!(rows[2].depth, 1);
    }

    #[test]
    fn last_sibling_marker_on_final_child_only() {
        let mut expanded = HashSet::new();
        expanded.insert("a".to_string());
        let rows = flatten(&Demo, &expanded);
        // ["a" (last=false because b follows), "a/0" (false), "a/1" (true), "b" (true)]
        assert_eq!(rows[0].is_last_sibling, false);
        assert_eq!(rows[1].is_last_sibling, false);
        assert_eq!(rows[2].is_last_sibling, true);
        assert_eq!(rows[3].is_last_sibling, true);
    }

    #[test]
    fn ancestor_continues_tracks_parent_chain() {
        let mut expanded = HashSet::new();
        expanded.insert("a".to_string());
        let rows = flatten(&Demo, &expanded);
        // a's children come before "b", so the trunk continues past
        // them: ancestor_continues == [true].
        assert_eq!(rows[1].ancestor_continues, vec![true]);
        assert_eq!(rows[2].ancestor_continues, vec![true]);
    }

    #[test]
    fn second_root_children_have_no_continuing_trunk() {
        let mut expanded = HashSet::new();
        expanded.insert("b".to_string());
        let rows = flatten(&Demo, &expanded);
        // b is the last root; its children's ancestor trunk doesn't
        // continue (no further siblings to draw the line for).
        let b_children: Vec<&TreeRow<_, _>> =
            rows.iter().filter(|r| r.depth == 1).collect();
        assert_eq!(b_children.len(), 2);
        for c in b_children {
            assert_eq!(c.ancestor_continues, vec![false]);
        }
    }

    #[test]
    fn leaves_report_not_expandable() {
        let mut expanded = HashSet::new();
        expanded.insert("a".to_string());
        // Even if a leaf id is in `expanded`, is_expandable=false stops
        // recursion — we only emit the leaf row itself.
        expanded.insert("a/0".to_string());
        let rows = flatten(&Demo, &expanded);
        let leaf = rows.iter().find(|r| r.row == "a/0").unwrap();
        assert_eq!(leaf.expandable, false);
    }

    #[test]
    fn tree_state_toggle_round_trips() {
        let mut state: TreeState<String> = TreeState::default();
        assert!(!state.is_expanded(&"x".into()));
        assert_eq!(state.toggle(&"x".into()), true);
        assert!(state.is_expanded(&"x".into()));
        assert_eq!(state.toggle(&"x".into()), false);
        assert!(!state.is_expanded(&"x".into()));
    }

    #[test]
    fn toggle_at_expands_then_collapses() {
        let rows = flatten(&Demo, &HashSet::new());
        let mut state: TreeState<String> = TreeState::default();
        match state.toggle_at(&rows, 0) {
            ToggleOutcome::Expanded(id) => assert_eq!(id, "a"),
            other => panic!("expected Expanded(\"a\"), got {other:?}"),
        }
        assert!(state.is_expanded(&"a".into()));
        // Re-flatten to see the new shape.
        let rows = flatten(&Demo, &state.expanded);
        match state.toggle_at(&rows, 0) {
            ToggleOutcome::Collapsed(id) => assert_eq!(id, "a"),
            other => panic!("expected Collapsed(\"a\"), got {other:?}"),
        }
    }

    #[test]
    fn toggle_at_skips_leaves() {
        let mut expanded = HashSet::new();
        expanded.insert("a".to_string());
        let rows = flatten(&Demo, &expanded);
        // Find a leaf row.
        let leaf_idx = rows.iter().position(|r| r.row == "a/0").unwrap();
        let mut state: TreeState<String> = TreeState {
            expanded: expanded.clone(),
            nav: Nav::default(),
        };
        assert_eq!(state.toggle_at(&rows, leaf_idx), ToggleOutcome::NotExpandable);
        assert!(!state.is_expanded(&"a/0".into()));
    }

    #[test]
    fn toggle_at_out_of_range_is_safe() {
        let rows = flatten(&Demo, &HashSet::new());
        let mut state: TreeState<String> = TreeState::default();
        assert_eq!(state.toggle_at(&rows, 999), ToggleOutcome::OutOfRange);
    }

    #[test]
    fn collapse_or_parent_collapses_when_expanded() {
        let mut expanded = HashSet::new();
        expanded.insert("a".to_string());
        let rows = flatten(&Demo, &expanded);
        // Cursor on the expanded "a" root.
        let mut state: TreeState<String> = TreeState {
            expanded: expanded.clone(),
            nav: Nav { cursor: 0, scroll: 0 },
        };
        match state.collapse_or_parent(&rows) {
            ParentOutcome::Collapsed(id) => assert_eq!(id, "a"),
            other => panic!("{other:?}"),
        }
        assert!(!state.is_expanded(&"a".into()));
    }

    #[test]
    fn collapse_or_parent_jumps_up_from_child() {
        let mut expanded = HashSet::new();
        expanded.insert("a".to_string());
        let rows = flatten(&Demo, &expanded);
        // rows: ["a", "a/0", "a/1", "b"]. Cursor on "a/1".
        let mut state: TreeState<String> = TreeState {
            expanded,
            nav: Nav { cursor: 2, scroll: 0 },
        };
        match state.collapse_or_parent(&rows) {
            ParentOutcome::JumpedToParent(idx) => {
                assert_eq!(idx, 0);
                assert_eq!(rows[idx].row, "a");
            }
            other => panic!("{other:?}"),
        }
        assert_eq!(state.nav.cursor, 0);
    }

    #[test]
    fn collapse_or_parent_at_root_returns_at_root() {
        let rows = flatten(&Demo, &HashSet::new());
        // Cursor on "a", which is depth-0 and not expanded.
        let mut state: TreeState<String> = TreeState {
            expanded: HashSet::new(),
            nav: Nav { cursor: 0, scroll: 0 },
        };
        assert_eq!(state.collapse_or_parent(&rows), ParentOutcome::AtRoot);
    }

    #[test]
    fn select_id_makes_cursor_sticky_across_reflatten() {
        let mut expanded = HashSet::new();
        expanded.insert("a".to_string());
        let rows = flatten(&Demo, &expanded);
        // Pick "a/1".
        let mut state: TreeState<String> = TreeState {
            expanded,
            nav: Nav { cursor: 2, scroll: 0 },
        };
        let target = rows[state.nav.cursor].id.clone();
        // Now expand "b" too — the row list grows but "a/1" stays.
        state.expanded.insert("b".into());
        let rows2 = flatten(&Demo, &state.expanded);
        let pos = state.select_id(&rows2, &target).unwrap();
        assert_eq!(rows2[pos].row, "a/1");
        assert_eq!(state.nav.cursor, pos);
    }
}
