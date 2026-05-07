//! Directory scanning — synchronous for v1.
//!
//! Async/streamed scan can come later (large NFS dirs, network mounts);
//! for now `read_dir` is plenty fast on local filesystems and avoids the
//! complexity of partial-list rendering. Sort order is dirs-first then
//! by name, mirroring yazi/ranger defaults.

use std::cmp::Ordering;
use std::io;
use std::path::Path;

use super::entry::{EntryKind, FsEntry};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[allow(dead_code)] // Size / Mtime wired to scan_dir but no UI yet binds the toggle.
pub enum SortMode {
    Name,
    Size,
    Mtime,
}

pub fn scan_dir(path: &Path, show_hidden: bool, sort: SortMode) -> io::Result<Vec<FsEntry>> {
    let mut out = Vec::new();
    for ent in std::fs::read_dir(path)? {
        let ent = match ent {
            Ok(e) => e,
            Err(_) => continue,
        };
        let name = ent.file_name().to_string_lossy().into_owned();
        if !show_hidden && name.starts_with('.') {
            continue;
        }
        let path = ent.path();
        let metadata = match ent.metadata() {
            Ok(m) => m,
            Err(_) => continue,
        };
        let kind = if metadata.is_dir() {
            EntryKind::Dir
        } else if metadata.file_type().is_symlink() {
            EntryKind::Symlink
        } else if metadata.is_file() {
            EntryKind::File
        } else {
            EntryKind::Other
        };
        let size = metadata.len();
        let mtime = metadata.modified().ok();
        out.push(FsEntry {
            path,
            name,
            kind,
            size,
            mtime,
        });
    }
    sort_entries(&mut out, sort);
    Ok(out)
}

fn sort_entries(entries: &mut [FsEntry], sort: SortMode) {
    entries.sort_by(|a, b| {
        // Dirs always before files, regardless of column sort.
        match (a.is_dir(), b.is_dir()) {
            (true, false) => return Ordering::Less,
            (false, true) => return Ordering::Greater,
            _ => {}
        }
        match sort {
            SortMode::Name => natural_cmp(&a.name, &b.name),
            SortMode::Size => b.size.cmp(&a.size).then_with(|| natural_cmp(&a.name, &b.name)),
            SortMode::Mtime => match (a.mtime, b.mtime) {
                (Some(x), Some(y)) => y.cmp(&x).then_with(|| natural_cmp(&a.name, &b.name)),
                (Some(_), None) => Ordering::Less,
                (None, Some(_)) => Ordering::Greater,
                (None, None) => natural_cmp(&a.name, &b.name),
            },
        }
    });
}

/// Case-insensitive byte comparison, falling back to case-sensitive on
/// tie. Cheap stand-in for "natural" sort — good enough for v1; we can
/// upgrade to numeric-aware ordering later if users care.
fn natural_cmp(a: &str, b: &str) -> Ordering {
    let lo = a.to_lowercase().cmp(&b.to_lowercase());
    if lo != Ordering::Equal {
        lo
    } else {
        a.cmp(b)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use std::path::PathBuf;

    fn tmp(name: &str) -> PathBuf {
        let mut d = std::env::temp_dir();
        d.push(format!("bobtop-fb-test-{}-{}", std::process::id(), name));
        let _ = fs::remove_dir_all(&d);
        fs::create_dir_all(&d).unwrap();
        d
    }

    #[test]
    fn dirs_sort_before_files() {
        let dir = tmp("dirs_sort");
        fs::create_dir(dir.join("zzz_dir")).unwrap();
        fs::write(dir.join("aaa_file"), b"x").unwrap();
        let entries = scan_dir(&dir, false, SortMode::Name).unwrap();
        assert_eq!(entries[0].name, "zzz_dir");
        assert_eq!(entries[1].name, "aaa_file");
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn hidden_filter_off_by_default() {
        let dir = tmp("hidden_filter");
        fs::write(dir.join(".hidden"), b"").unwrap();
        fs::write(dir.join("visible"), b"").unwrap();
        let visible = scan_dir(&dir, false, SortMode::Name).unwrap();
        assert_eq!(visible.len(), 1);
        assert_eq!(visible[0].name, "visible");
        let all = scan_dir(&dir, true, SortMode::Name).unwrap();
        assert_eq!(all.len(), 2);
        let _ = fs::remove_dir_all(&dir);
    }
}
