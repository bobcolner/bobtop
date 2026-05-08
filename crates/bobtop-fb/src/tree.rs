//! Tree-view primitives — the data layer behind `ViewMode::Tree`.
//!
//! Pure functions and trait impls; no rendering. The UI layer
//! ([`crate::ui::draw_tree_view`]) takes the flattened row list this
//! module produces and feeds it to a `bobtop_tui::LiveTable` in
//! tree-glyph mode.

use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::time::SystemTime;

use bobtop_tui::widgets::live_table::{Cell, TableEntry, TableRowExt};
use bobtop_tui::format_bytes_compact;

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

/// One row the tree view renders. Wraps an [`FsEntry`] with the
/// per-row tree metadata `LiveTable` needs (depth, ancestor
/// continuity, last-sibling marker).
#[derive(Debug, Clone)]
pub struct TreeRow {
    pub entry: FsEntry,
    pub depth: u8,
    pub ancestor_continues: Vec<bool>,
    pub is_last_sibling: bool,
}

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

/// Walk `cwd` and any expanded subdirectories under it; return a
/// flattened, sorted list of [`TreeRow`]s ready to feed `LiveTable`.
///
/// `show_hidden`: include dot-files. `filter`: hide entries whose
/// name doesn't substring-match (case-insensitive); ancestors of
/// matching descendants stay visible so context isn't lost.
///
/// Errors during recursive scans are silently swallowed (an
/// unreadable directory yields no children rather than aborting the
/// tree). The user-visible surface is the action bar; tree-render
/// is best-effort.
pub fn flatten_tree(
    cwd: &Path,
    expanded: &HashSet<PathBuf>,
    sort: FbCol,
    descending: bool,
    show_hidden: bool,
    filter: Option<&str>,
) -> Vec<TreeRow> {
    let mut out = Vec::new();
    walk(
        cwd,
        0,
        &Vec::new(),
        true,
        expanded,
        sort,
        descending,
        show_hidden,
        filter,
        &mut out,
    );
    out
}

#[allow(clippy::too_many_arguments)]
fn walk(
    dir: &Path,
    depth: u8,
    ancestor_continues: &[bool],
    _root_is_last: bool,
    expanded: &HashSet<PathBuf>,
    sort: FbCol,
    descending: bool,
    show_hidden: bool,
    filter: Option<&str>,
    out: &mut Vec<TreeRow>,
) {
    let Ok(mut entries) = scan_dir(dir, show_hidden, SortMode::Name) else { return };
    sort_entries(&mut entries, sort, descending);

    // Filter pass: when a filter is active, keep entries whose name
    // matches OR which are directories that contain a match
    // (transitively). Implementing the second half requires looking
    // ahead — pre-walk to compute a "has-match" set.
    let visible = compute_visible(&entries, expanded, dir, sort, descending, show_hidden, filter);

    for (i, entry) in entries.iter().enumerate() {
        if !visible[i] {
            continue;
        }
        let is_last = visible[(i + 1)..].iter().all(|v| !*v);
        out.push(TreeRow {
            entry: entry.clone(),
            depth,
            ancestor_continues: ancestor_continues.to_vec(),
            is_last_sibling: is_last,
        });
        if entry.is_dir() && expanded.contains(&entry.path) {
            let mut next_ancestors = ancestor_continues.to_vec();
            next_ancestors.push(!is_last);
            walk(
                &entry.path,
                depth + 1,
                &next_ancestors,
                is_last,
                expanded,
                sort,
                descending,
                show_hidden,
                filter,
                out,
            );
        }
    }
}

/// Visibility mask for `entries` under `dir`. With no filter, every
/// entry is visible. With a filter, an entry is visible if its name
/// matches OR (it's a directory and has a matching descendant).
#[allow(clippy::too_many_arguments)]
fn compute_visible(
    entries: &[FsEntry],
    expanded: &HashSet<PathBuf>,
    dir: &Path,
    sort: FbCol,
    descending: bool,
    show_hidden: bool,
    filter: Option<&str>,
) -> Vec<bool> {
    let Some(q) = filter.map(|s| s.to_lowercase()) else {
        return vec![true; entries.len()];
    };
    let _ = (sort, descending, dir);
    entries
        .iter()
        .map(|e| {
            if e.name.to_lowercase().contains(&q) {
                return true;
            }
            // For directories that *don't* match by name, peek inside
            // (only when the user has expanded this branch — we don't
            // recursively scan everything just to drive a filter
            // because that'd hammer the FS on big trees).
            if e.is_dir() && expanded.contains(&e.path) {
                if let Ok(children) = scan_dir(&e.path, show_hidden, SortMode::Name) {
                    return children
                        .iter()
                        .any(|c| c.name.to_lowercase().contains(&q));
                }
            }
            false
        })
        .collect()
}

impl TableRowExt<FbCol> for TreeRow {
    fn cell(&self, col: FbCol) -> Cell {
        match col {
            FbCol::Name => {
                let glyph = match self.entry.kind {
                    EntryKind::Dir => "📁",
                    EntryKind::Symlink => "↪",
                    _ => " ",
                };
                Cell::plain(format!("{} {}", glyph, self.entry.name))
            }
            FbCol::Size => {
                if self.entry.is_dir() {
                    Cell::plain("")
                } else {
                    Cell::plain(format_bytes_compact(self.entry.size))
                }
            }
            FbCol::Modified => Cell::plain(format_mtime(self.entry.mtime)),
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
        for b in self.entry.path.as_os_str().as_encoded_bytes() {
            h = h.wrapping_mul(33).wrapping_add(*b as u64);
        }
        Some(h)
    }

    fn matches_filter(&self, q: &str) -> bool {
        self.entry.name.to_lowercase().contains(&q.to_lowercase())
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
