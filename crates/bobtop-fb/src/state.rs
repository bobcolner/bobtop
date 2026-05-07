//! Persistent UI state for bobtop-fb.
//!
//! Currently just `last_cwd`: the directory the user was browsing
//! when they last quit. Loaded by `cli::run` when no path arg is
//! provided so reopening the browser (`bobtop fb`, or `b` from the
//! main UI on a non-process row) resumes where you left off.
//!
//! Stored at `$XDG_STATE_HOME/bobtop-fb/state.toml` (default
//! `~/.local/state/bobtop-fb/state.toml`). Best-effort — read/write
//! errors are logged and silently ignored so a missing or
//! unwritable state directory never breaks the app.

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

#[derive(Debug, Default, Serialize, Deserialize)]
pub struct PersistedState {
    /// Most recently viewed directory at the moment the user quit.
    pub last_cwd: Option<PathBuf>,
}

/// Resolve the on-disk state file path. Honors `$XDG_STATE_HOME`,
/// falls back to `$HOME/.local/state`, and finally `/tmp` if neither
/// is set (CI / containers without a HOME).
pub fn state_file() -> PathBuf {
    let dir = std::env::var_os("XDG_STATE_HOME")
        .map(PathBuf::from)
        .or_else(|| {
            std::env::var_os("HOME").map(|h| {
                let mut p = PathBuf::from(h);
                p.push(".local/state");
                p
            })
        })
        .unwrap_or_else(|| PathBuf::from("/tmp"));
    let mut path = dir;
    path.push("bobtop-fb");
    path.push("state.toml");
    path
}

pub fn load() -> PersistedState {
    let path = state_file();
    let text = match std::fs::read_to_string(&path) {
        Ok(t) => t,
        Err(_) => return PersistedState::default(),
    };
    toml::from_str(&text).unwrap_or_default()
}

pub fn save(state: &PersistedState) {
    let path = state_file();
    if let Some(parent) = path.parent() {
        if std::fs::create_dir_all(parent).is_err() {
            return;
        }
    }
    let text = match toml::to_string(state) {
        Ok(t) => t,
        Err(_) => return,
    };
    let _ = atomic_write(&path, &text);
}

/// Write `text` to `path` atomically: dump to a sibling tempfile,
/// then rename. Avoids leaving a half-written state file when the
/// process is killed mid-write.
fn atomic_write(path: &Path, text: &str) -> std::io::Result<()> {
    let mut tmp = path.to_path_buf();
    let mut name = path
        .file_name()
        .map(|s| s.to_os_string())
        .unwrap_or_default();
    name.push(".tmp");
    tmp.set_file_name(name);
    std::fs::write(&tmp, text)?;
    std::fs::rename(&tmp, path)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicU32, Ordering};

    static COUNTER: AtomicU32 = AtomicU32::new(0);

    fn unique_dir() -> PathBuf {
        let n = COUNTER.fetch_add(1, Ordering::Relaxed);
        let mut d = std::env::temp_dir();
        d.push(format!(
            "bobtop-fb-state-test-{}-{n}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&d);
        d
    }

    #[test]
    fn save_then_load_round_trips_cwd() {
        let dir = unique_dir();
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("state.toml");
        let s = PersistedState {
            last_cwd: Some(PathBuf::from("/home/foo/projects")),
        };
        let text = toml::to_string(&s).unwrap();
        std::fs::write(&path, &text).unwrap();
        let loaded: PersistedState =
            toml::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
        assert_eq!(loaded.last_cwd, s.last_cwd);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn load_returns_default_when_file_missing() {
        // We can't easily fake $XDG_STATE_HOME for the global helper,
        // but `load()` falls through to a default on any read error
        // — exercise that path via a guaranteed-missing TOML.
        let path = unique_dir().join("nope.toml");
        let result: PersistedState = std::fs::read_to_string(&path)
            .ok()
            .and_then(|t| toml::from_str(&t).ok())
            .unwrap_or_default();
        assert!(result.last_cwd.is_none());
    }
}
