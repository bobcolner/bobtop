//! Recursive file finder backing the `f` overlay.
//!
//! Walks `cwd` breadth-first, matching every entry against a glob
//! (auto-wrapped as `*input*` when the user typed plain text). Caps
//! results, depth, and walltime so a `f` in `~/` doesn't try to
//! enumerate the whole disk. Reuses the same `globset` engine as
//! the `/` filter, so power users get globs for free here too.

use std::collections::VecDeque;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use globset::GlobMatcher;

#[derive(Debug, Clone)]
pub struct FindResult {
    /// Path relative to the search root. Display this in the list,
    /// resolve back to absolute via `root.join(rel)` for actions.
    pub rel: PathBuf,
    pub is_dir: bool,
}

#[derive(Debug, Clone, Copy)]
pub struct FindLimits {
    pub max_results: usize,
    pub max_depth: usize,
    pub timeout: Duration,
}

impl Default for FindLimits {
    fn default() -> Self {
        Self {
            max_results: 5_000,
            max_depth: 8,
            timeout: Duration::from_millis(200),
        }
    }
}

/// Directories we skip during a recursive walk by default. This is
/// the default; users will be able to override later. Mirrors the
/// list `fd` and `ripgrep` use as their hardcoded fallback.
const PRUNE_DIRS: &[&str] = &[
    ".git",
    ".hg",
    ".svn",
    "node_modules",
    "target",
    ".cache",
    ".cargo",
    "dist",
    "build",
    ".next",
    ".venv",
    "venv",
    "__pycache__",
];

pub fn search(
    root: &Path,
    pattern: &str,
    show_hidden: bool,
    limits: FindLimits,
) -> Vec<FindResult> {
    let pattern = pattern.trim();
    if pattern.is_empty() {
        return Vec::new();
    }
    let matcher = build_matcher(pattern);
    let needle = pattern.to_lowercase();
    let started = Instant::now();
    let mut results: Vec<FindResult> = Vec::new();
    let mut queue: VecDeque<(PathBuf, usize)> = VecDeque::new();
    queue.push_back((root.to_path_buf(), 0));

    while let Some((dir, depth)) = queue.pop_front() {
        if started.elapsed() > limits.timeout || results.len() >= limits.max_results {
            break;
        }
        let Ok(read) = std::fs::read_dir(&dir) else {
            continue;
        };
        for entry in read.flatten() {
            if results.len() >= limits.max_results {
                break;
            }
            let name = entry.file_name().to_string_lossy().into_owned();
            if !show_hidden && name.starts_with('.') {
                continue;
            }
            let path = entry.path();
            let file_type = match entry.file_type() {
                Ok(t) => t,
                Err(_) => continue,
            };
            let is_dir = file_type.is_dir();
            // Match. Glob pattern ran against the full relative
            // path so users can find e.g. "src/lib.rs"; substring
            // fallback matches against name + relative path.
            let rel = match path.strip_prefix(root) {
                Ok(r) => r.to_path_buf(),
                Err(_) => path.clone(),
            };
            let rel_display = rel.to_string_lossy();
            let matched = match &matcher {
                Some(m) => m.is_match(&name) || m.is_match(rel_display.as_ref()),
                None => {
                    let lname = name.to_lowercase();
                    let lrel = rel_display.to_lowercase();
                    lname.contains(&needle) || lrel.contains(&needle)
                }
            };
            if matched {
                results.push(FindResult { rel, is_dir });
            }
            if is_dir && depth + 1 < limits.max_depth {
                if PRUNE_DIRS.iter().any(|p| *p == name.as_str()) {
                    continue;
                }
                queue.push_back((path, depth + 1));
            }
        }
    }
    results
}

fn build_matcher(raw: &str) -> Option<GlobMatcher> {
    let has_meta = raw.contains(['*', '?', '[']);
    let pattern = if has_meta {
        raw.to_string()
    } else {
        format!("*{}*", raw)
    };
    globset::GlobBuilder::new(&pattern)
        .case_insensitive(true)
        .literal_separator(false)
        .build()
        .ok()
        .map(|g| g.compile_matcher())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    fn tmp(name: &str) -> PathBuf {
        let mut d = std::env::temp_dir();
        d.push(format!("gfb-find-{}-{}", std::process::id(), name));
        let _ = fs::remove_dir_all(&d);
        fs::create_dir_all(&d).unwrap();
        d
    }

    #[test]
    fn substring_matches_basename() {
        let dir = tmp("substr");
        fs::create_dir(dir.join("subdir")).unwrap();
        fs::write(dir.join("subdir/main.rs"), b"").unwrap();
        fs::write(dir.join("readme.md"), b"").unwrap();
        let results = search(&dir, "main", false, FindLimits::default());
        assert!(results.iter().any(|r| r.rel == PathBuf::from("subdir/main.rs")));
        assert!(!results.iter().any(|r| r.rel == PathBuf::from("readme.md")));
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn glob_pattern_works() {
        let dir = tmp("glob");
        fs::write(dir.join("a.rs"), b"").unwrap();
        fs::write(dir.join("b.toml"), b"").unwrap();
        let results = search(&dir, "*.rs", false, FindLimits::default());
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].rel, PathBuf::from("a.rs"));
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn pruned_dirs_are_skipped() {
        let dir = tmp("prune");
        fs::create_dir(dir.join("node_modules")).unwrap();
        fs::write(dir.join("node_modules/x.js"), b"").unwrap();
        fs::write(dir.join("y.js"), b"").unwrap();
        let results = search(&dir, "*.js", false, FindLimits::default());
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].rel, PathBuf::from("y.js"));
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn hidden_filter_off_by_default() {
        let dir = tmp("hidden");
        fs::write(dir.join(".secret"), b"").unwrap();
        fs::write(dir.join("public"), b"").unwrap();
        let r1 = search(&dir, "secret", false, FindLimits::default());
        assert_eq!(r1.len(), 0);
        let r2 = search(&dir, "secret", true, FindLimits::default());
        assert_eq!(r2.len(), 1);
        let _ = fs::remove_dir_all(&dir);
    }
}
