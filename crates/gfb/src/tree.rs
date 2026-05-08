//! Tree-view primitives — the data layer behind `ViewMode::Tree`.
//!
//! Pure functions and trait impls; no rendering. The UI layer
//! ([`crate::ui::draw_tree_view`]) takes the flattened row list this
//! module produces and feeds it to a `gtui::LiveTable` in
//! tree-glyph mode.
//!
//! As of the Phase 2 refactor the depth / ancestor-continues / last-
//! sibling computation lives in [`gtui::tree`]. This module supplies
//! the filesystem-specific [`gtui::tree::Catalog`] impl. Phase 3 went
//! further and dropped gfb's wrapper `TreeRow` — the toolkit's
//! [`gtui::tree::TreeRow<PathBuf, FsEntry>`] is the rendering shape
//! directly, with [`TableRowExt`] implemented on it (orphan rule
//! satisfied via the local [`FbCol`] trait parameter).

use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::time::SystemTime;

use gtui::format_bytes_compact;
use gtui::tree::{flatten, Catalog, TreeRow as GtuiTreeRow};
use gtui::widgets::live_table::{Cell, TableEntry, TableRowExt};

use crate::fs::entry::{EntryKind, FsEntry};
use crate::fs::scan::{scan_dir, SortMode};

/// Column id for the tree pane's `LiveTable`. Sortable: Name, Size,
/// Modified. Kind is implicit — directories always cluster first
/// regardless of the active sort.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FbCol {
    Name,
    Size,
    Modified,
}

/// Concrete row type for the tree view: the toolkit's
/// [`gtui::tree::TreeRow`] specialised to filesystem entries.
/// Re-exported as a type alias so callers don't have to spell the
/// generics every time and so the [`TableRowExt`] impl below has a
/// stable, local-feeling identifier.
pub type TreeRow = GtuiTreeRow<PathBuf, FsEntry>;

/// Sort + group: directories first, then alphabetical-or-metric per
/// the active column. Honours `descending` for the comparable
/// columns (Size, Modified). Name flips alphabetical order on
/// `descending = true`.
fn sort_entries(entries: &mut [FsEntry], col: FbCol, descending: bool) {
    entries.sort_by(|a, b| {
        // Dirs first regardless of sort.
        let dir_cmp = b.is_dir().cmp(&a.is_dir());
        if dir_cmp != std::cmp::Ordering::Equal {
            return dir_cmp;
        }
        let primary = match col {
            FbCol::Name => a.name.to_lowercase().cmp(&b.name.to_lowercase()),
            FbCol::Size => a.size.cmp(&b.size),
            FbCol::Modified => a.mtime.cmp(&b.mtime),
        };
        if descending {
            primary.reverse()
        } else {
            primary
        }
    });
}

/// Filesystem-rooted [`Catalog`] impl. Captures all the per-walk
/// knobs (sort, filter, hidden-files, expansion set) so
/// [`Catalog::children`] can sort and filter without arguments.
struct FsCatalog<'a> {
    cwd: &'a Path,
    expanded: &'a HashSet<PathBuf>,
    sort: FbCol,
    descending: bool,
    show_hidden: bool,
    /// Pre-lowercased filter substring. `None` = no filter.
    filter: Option<String>,
}

impl<'a> FsCatalog<'a> {
    fn new(
        cwd: &'a Path,
        expanded: &'a HashSet<PathBuf>,
        sort: FbCol,
        descending: bool,
        show_hidden: bool,
        filter: Option<&str>,
    ) -> Self {
        Self {
            cwd,
            expanded,
            sort,
            descending,
            show_hidden,
            filter: filter.map(|s| s.to_lowercase()),
        }
    }

    /// Read `dir`, sort the results, then drop entries that don't
    /// match the active filter (or that have no matching descendant
    /// in expanded subtrees). Errors are silently swallowed — the
    /// surfaced action bar is the user-visible reporting path.
    fn read_visible(&self, dir: &Path) -> Vec<FsEntry> {
        let Ok(mut entries) = scan_dir(dir, self.show_hidden, SortMode::Name) else {
            return Vec::new();
        };
        sort_entries(&mut entries, self.sort, self.descending);
        let visible = self.compute_visible(&entries);
        entries
            .into_iter()
            .zip(visible)
            .filter_map(|(e, v)| if v { Some(e) } else { None })
            .collect()
    }

    /// Visibility mask. With no filter, every entry is visible.
    /// With a filter, an entry is visible if its name matches OR
    /// (it's a directory the user has expanded and a direct child
    /// matches). We don't recursively scan unexpanded subtrees just
    /// to drive the filter — that'd hammer the FS on big trees.
    fn compute_visible(&self, entries: &[FsEntry]) -> Vec<bool> {
        let Some(q) = self.filter.as_deref() else {
            return vec![true; entries.len()];
        };
        entries
            .iter()
            .map(|e| {
                if e.name.to_lowercase().contains(q) {
                    return true;
                }
                if e.is_dir() && self.expanded.contains(&e.path) {
                    if let Ok(children) = scan_dir(&e.path, self.show_hidden, SortMode::Name) {
                        return children.iter().any(|c| c.name.to_lowercase().contains(q));
                    }
                }
                false
            })
            .collect()
    }
}

impl<'a> Catalog for FsCatalog<'a> {
    type NodeId = PathBuf;
    type Row = FsEntry;

    fn roots(&self) -> Vec<(Self::NodeId, Self::Row)> {
        self.read_visible(self.cwd)
            .into_iter()
            .map(|e| (e.path.clone(), e))
            .collect()
    }

    fn children(&self, node: &Self::NodeId) -> Vec<(Self::NodeId, Self::Row)> {
        self.read_visible(node)
            .into_iter()
            .map(|e| (e.path.clone(), e))
            .collect()
    }

    fn is_expandable(&self, node: &Self::NodeId) -> bool {
        // Path-only test would force a `metadata` syscall per node.
        // The roots/children paths already produced FsEntry with the
        // kind cached, but the trait sees only the NodeId. Re-stat
        // here — cheap on hot caches and only called by the walker
        // when deciding whether to descend.
        node.is_dir()
    }
}

/// Walk `cwd` and any expanded subdirectories under it; return a
/// flattened, sorted list of [`TreeRow`]s ready to feed `LiveTable`.
///
/// `show_hidden`: include dot-files. `filter`: hide entries whose
/// name doesn't substring-match (case-insensitive); ancestors of
/// matching descendants stay visible so context isn't lost.
pub fn flatten_tree(
    cwd: &Path,
    expanded: &HashSet<PathBuf>,
    sort: FbCol,
    descending: bool,
    show_hidden: bool,
    filter: Option<&str>,
) -> Vec<TreeRow> {
    let catalog = FsCatalog::new(cwd, expanded, sort, descending, show_hidden, filter);
    flatten(&catalog, expanded)
}

impl TableRowExt<FbCol> for TreeRow {
    fn cell(&self, col: FbCol) -> Cell {
        match col {
            FbCol::Name => {
                let glyph = match self.row.kind {
                    EntryKind::Dir => "📁",
                    EntryKind::Symlink => "↪",
                    _ => " ",
                };
                Cell::plain(format!("{} {}", glyph, self.row.name))
            }
            FbCol::Size => {
                if self.row.is_dir() {
                    Cell::plain("")
                } else {
                    Cell::plain(format_bytes_compact(self.row.size))
                }
            }
            FbCol::Modified => Cell::plain(format_mtime(self.row.mtime)),
        }
    }

    fn tree_depth(&self) -> u8 {
        self.depth
    }

    fn ancestor_continues(&self) -> &[bool] {
        &self.ancestor_continues
    }

    fn is_last_sibling(&self) -> bool {
        self.is_last_sibling
    }

    fn key(&self) -> Option<u64> {
        // Hash the path so sticky-selection follows the file across
        // re-sort / filter toggles. djb2 — same shape used elsewhere
        // in the suite for stringly-typed keys.
        let mut h: u64 = 5381;
        for b in self.id.as_os_str().as_encoded_bytes() {
            h = h.wrapping_mul(33).wrapping_add(*b as u64);
        }
        Some(h)
    }

    fn matches_filter(&self, q: &str) -> bool {
        self.row.name.to_lowercase().contains(&q.to_lowercase())
    }
}

/// Render an mtime as `YYYY-MM-DD HH:MM`. Defers to the system
/// clock conversion path — fb already paid that cost in `text.rs`
/// for the miller-mode list pane, so we do the same shape here.
fn format_mtime(t: Option<SystemTime>) -> String {
    let Some(t) = t else { return String::new() };
    let secs = t
        .duration_since(SystemTime::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    // Coarse formatting: yyyy-mm-dd hh:mm in UTC. Avoids a chrono
    // dep; precision-to-the-minute matches what the miller list
    // shows. Apps that need locale-aware formatting can layer that
    // on top later.
    let days_since_epoch = (secs / 86_400) as i64;
    let (y, m, d) = days_to_ymd(days_since_epoch);
    let hh = (secs % 86_400) / 3600;
    let mm = (secs % 3600) / 60;
    format!("{y:04}-{m:02}-{d:02} {hh:02}:{mm:02}")
}

/// Convert "days since 1970-01-01" to a (year, month, day) tuple.
/// Civil-from-days algorithm (Howard Hinnant); avoids needing a
/// calendar crate. Days can be negative for dates before 1970, in
/// which case the math still works.
fn days_to_ymd(days: i64) -> (i64, u32, u32) {
    let z = days + 719468;
    let era = if z >= 0 { z / 146097 } else { (z - 146096) / 146097 };
    let doe = (z - era * 146097) as u64; // [0, 146096]
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146096) / 365; // [0, 399]
    let y = (yoe as i64) + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100); // [0, 365]
    let mp = (5 * doy + 2) / 153; // [0, 11]
    let d = (doy - (153 * mp + 2) / 5 + 1) as u32; // [1, 31]
    let m = if mp < 10 { mp + 3 } else { mp - 9 } as u32; // [1, 12]
    let y = if m <= 2 { y + 1 } else { y };
    (y, m, d)
}

/// Convenience wrapper: use the sort/expand state on `App` to build
/// the flattened row list. Defined here (not on `App`) so the data
/// path stays trait-impl-only and easy to test.
pub fn build_rows_for_app(
    cwd: &Path,
    sort: FbCol,
    descending: bool,
    show_hidden: bool,
    expanded: &HashSet<PathBuf>,
    filter: Option<&str>,
) -> Vec<TableEntry<TreeRow, ()>> {
    flatten_tree(cwd, expanded, sort, descending, show_hidden, filter)
        .into_iter()
        .map(TableEntry::Item)
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn entry(name: &str, kind: EntryKind, size: u64) -> FsEntry {
        FsEntry {
            path: PathBuf::from(name),
            name: name.to_string(),
            kind,
            size,
            mtime: None,
        }
    }

    #[test]
    fn sort_keeps_dirs_first() {
        let mut v = vec![
            entry("zebra.txt", EntryKind::File, 100),
            entry("apple_dir", EntryKind::Dir, 0),
            entry("banana.txt", EntryKind::File, 200),
            entry("cherry_dir", EntryKind::Dir, 0),
        ];
        sort_entries(&mut v, FbCol::Name, false);
        // Dirs first (alpha), then files (alpha).
        let names: Vec<&str> = v.iter().map(|e| e.name.as_str()).collect();
        assert_eq!(names, vec!["apple_dir", "cherry_dir", "banana.txt", "zebra.txt"]);
    }

    #[test]
    fn sort_size_descending_lists_biggest_first() {
        let mut v = vec![
            entry("small.txt", EntryKind::File, 10),
            entry("big.txt", EntryKind::File, 1000),
            entry("medium.txt", EntryKind::File, 500),
        ];
        sort_entries(&mut v, FbCol::Size, true);
        let names: Vec<&str> = v.iter().map(|e| e.name.as_str()).collect();
        assert_eq!(names, vec!["big.txt", "medium.txt", "small.txt"]);
    }

    #[test]
    fn days_to_ymd_known_dates() {
        // Spot-check unix epoch + a couple of well-known dates that
        // are easy to verify by hand.
        assert_eq!(days_to_ymd(0), (1970, 1, 1));
        assert_eq!(days_to_ymd(31), (1970, 2, 1));
        assert_eq!(days_to_ymd(365), (1971, 1, 1));
        // 2000-01-01 was day 10957.
        assert_eq!(days_to_ymd(10957), (2000, 1, 1));
    }
}
